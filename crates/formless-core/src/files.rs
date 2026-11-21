//! File search for `@` mentions (SPEC §5.6): a walk of a root that
//! respects `.gitignore`, skips hidden and build directories, and stops
//! at `MAX_WALK` entries so huge trees stay responsive.

use crate::fuzzy::{fold, PreparedQuery};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::Path;
use std::{cmp::Ordering, collections::BinaryHeap};

use unicode_normalization::UnicodeNormalization;

/// Most entries one walk collects.
pub const MAX_WALK: usize = 200_000;

/// Directories skipped even when not ignored by git.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    "venv",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    /// Relative to the root, with `/` separators.
    pub path: String,
    pub dir: bool,
    pub bytes: u64,
}

/// Every file and directory under `root`, and whether the walk stopped at
/// `MAX_WALK`.
pub fn walk(root: &Path) -> (Vec<FileEntry>, bool) {
    let Ok(canonical_root) = root.canonicalize() else {
        return (Vec::new(), false);
    };
    let walker = ignore::WalkBuilder::new(root)
        .follow_links(true)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            if e.file_type().is_some_and(|t| t.is_dir())
                && SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
            {
                return false;
            }
            // Check each entry too: an alias may have been replaced since
            // its parent was admitted, so descendants must remain contained.
            let Ok(target) = e.path().canonicalize() else {
                return false;
            };
            let Ok(relative) = target.strip_prefix(&canonical_root) else {
                return false;
            };
            // A visible alias must not bypass the hidden/build exclusions.
            let directories = if e.file_type().is_some_and(|t| t.is_dir()) {
                relative
            } else {
                relative.parent().unwrap_or_else(|| Path::new(""))
            };
            !relative
                .components()
                .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
                && !directories
                    .components()
                    .any(|part| SKIP_DIRS.contains(&part.as_os_str().to_string_lossy().as_ref()))
        })
        .build();
    let mut out = Vec::new();
    for entry in walker.flatten() {
        if entry.depth() == 0 {
            continue;
        }
        if out.len() >= MAX_WALK {
            return (out, true);
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let dir = entry.file_type().is_some_and(|t| t.is_dir());
        let bytes = if dir {
            0
        } else {
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        };
        out.push(FileEntry {
            path: rel.to_string_lossy().replace('\\', "/"),
            dir,
            bytes,
        });
    }
    (out, false)
}

/// The best `limit` entries for `query`. An empty query lists the top
/// level. Either path separator is accepted, and a trailing separator lists
/// that directory's children. Otherwise entries are ranked by fuzzy
/// match on the path, with a bonus when the file name itself matches.
/// Directory spelling is exact first, then uniquely canonical-equivalent.
/// Ambiguous canonical spellings require an exact spelling; children are
/// never merged. Directory fallback remains case/accent-sensitive.
pub fn search<'a>(entries: &'a [FileEntry], query: &str, limit: usize) -> Vec<&'a FileEntry> {
    // FileEntry paths are indexed with forward slashes. Normalize only
    // the search text, never the path or filesystem access.
    let query = if query.contains('\\') {
        Cow::Owned(query.replace('\\', "/"))
    } else {
        Cow::Borrowed(query)
    };
    let query = match query.as_ref() {
        "/" => "",
        query => query,
    };
    // A delimited mention can supply an indexed directory whose first
    // component starts with whitespace. Prefer that exact spelling before
    // applying the existing hand-typed-query trimming fallback.
    let exact_directory = query.ends_with('/')
        && entries.iter().any(|entry| {
            entry.path.starts_with(query)
                || (entry.dir && Some(entry.path.as_str()) == query.strip_suffix('/'))
        });
    let query = if exact_directory { query } else { query.trim() };
    if query.is_empty() || query.ends_with('/') {
        let Some(prefix) = directory_prefix(entries, query) else {
            return Vec::new();
        };
        let mut kids: Vec<&FileEntry> = entries
            .iter()
            .filter(|e| match e.path.strip_prefix(prefix.as_ref()) {
                Some(rest) => !rest.is_empty() && !rest.contains('/'),
                None => false,
            })
            .collect();
        kids.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.path.cmp(&b.path)));
        kids.truncate(limit);
        return kids;
    }
    if limit == 0 {
        return Vec::new();
    }
    let full_query = PreparedQuery::new(query);
    let base_query = PreparedQuery::new(query.rsplit('/').next().unwrap_or(query));
    let mut selected = BinaryHeap::new();
    let keep = limit.min(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let folded = fold(&entry.path);
        let Some(mut score) = full_query.score_folded(&folded) else {
            continue;
        };
        // Fold is per source scalar. Original indices let a basename slice
        // preserve dropped marks/expansions while scoring from its own start.
        let name_start = entry
            .path
            .rsplit_once('/')
            .map_or(0, |(prefix, _)| prefix.chars().count() + 1);
        let folded_start = folded.partition_point(|(original, _)| *original < name_start);
        if let Some(name_score) = base_query.score_folded(&folded[folded_start..]) {
            score += 10 + name_score;
        }
        let ranked = Ranked {
            score,
            index,
            entry,
        };
        if selected.len() < keep {
            selected.push(ranked);
        } else if selected.peek().is_some_and(|worst| ranked < *worst) {
            *selected.peek_mut().expect("nonempty bounded selection") = ranked;
        }
    }
    selected
        .into_sorted_vec()
        .into_iter()
        .map(|ranked| ranked.entry)
        .collect()
}

/// Greater means worse, so the heap exposes the entry to evict. Input index
/// reproduces stable sorting even when duplicate paths carry different data.
struct Ranked<'a> {
    score: i64,
    index: usize,
    entry: &'a FileEntry,
}
impl Ord for Ranked<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .cmp(&self.score)
            .then(self.entry.path.len().cmp(&other.entry.path.len()))
            .then(self.entry.path.cmp(&other.entry.path))
            .then(self.index.cmp(&other.index))
    }
}
impl PartialOrd for Ranked<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Ranked<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Ranked<'_> {}

fn directory_prefix<'a>(entries: &[FileEntry], query: &'a str) -> Option<Cow<'a, str>> {
    let directory = query.strip_suffix('/').unwrap_or(query);
    if query.is_empty()
        || entries
            .iter()
            .any(|e| (e.dir && e.path == directory) || e.path.starts_with(query))
    {
        return Some(Cow::Borrowed(query));
    }
    let normalized: String = directory.nfc().collect();
    let depth = query.bytes().filter(|b| *b == b'/').count();
    let mut found: Option<&str> = None;
    for entry in entries {
        // Resolve an original directory boundary before slicing children.
        // Ancestors are considered even if a partial index has no dir row.
        let mut separators = entry.path.match_indices('/');
        let candidate = match separators.nth(depth.saturating_sub(1)) {
            Some((byte, _)) => &entry.path[..byte],
            None if entry.dir && entry.path.bytes().filter(|b| *b == b'/').count() + 1 == depth => {
                &entry.path
            }
            None => continue,
        };
        if candidate.nfc().eq(normalized.chars()) {
            if found.is_some_and(|previous| previous != candidate) {
                return None;
            }
            found = Some(candidate);
        }
    }
    found.map(|original| Cow::Owned(format!("{original}/")))
}

#[path = "file_mentions.rs"]
mod file_mentions;
pub use file_mentions::{
    encode_mention, mention_completion, mention_ranges, mentions, MentionCompletion,
    MentionInsertion,
};

/// Parse a numeric line or line/column suffix, preserving colons in the path.
pub fn path_and_line(word: &str) -> (&str, Option<u32>) {
    let Some((head, last)) = word.rsplit_once(':') else {
        return (word, None);
    };
    let Ok(number) = last.parse::<u32>() else {
        return (word, None);
    };
    if let Some((path, line)) = head.rsplit_once(':') {
        if let Ok(line) = line.parse::<u32>() {
            return (path, Some(line));
        }
    }
    (head, Some(number))
}

/// Return the requested path and its canonical destination. A literal file
/// takes precedence over a line citation. Interpret only the final path
/// component, so directory names and Windows prefixes stay intact.
pub fn resolve_file_mention(
    base: &Path,
    path: std::path::PathBuf,
) -> std::io::Result<(std::path::PathBuf, std::path::PathBuf)> {
    use std::io::ErrorKind;

    let error = match base.join(&path).canonicalize() {
        Ok(full) => return Ok((path, full)),
        Err(error) => error,
    };
    if error.kind() != ErrorKind::NotFound
        && !(cfg!(windows) && error.kind() == ErrorKind::InvalidInput)
    {
        return Err(error);
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(error);
    };
    let (name, line) = path_and_line(name);
    if line.is_none() || name.is_empty() {
        return Err(error);
    }
    let cited_path = path.with_file_name(name);
    let full = base.join(&cited_path).canonicalize()?;
    if !full.is_file() {
        return Err(error);
    }
    Ok((cited_path, full))
}

/// The suffix of a path that explicitly names the native home directory.
pub fn home_path_suffix(path: &str) -> Option<&str> {
    if path == "~" {
        return Some("");
    }
    #[cfg(windows)]
    if let Some(suffix) = path.strip_prefix("~\\") {
        return Some(suffix);
    }
    path.strip_prefix("~/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_path_prefixes_follow_native_path_separators() {
        assert_eq!(home_path_suffix("~"), Some(""));
        assert_eq!(
            home_path_suffix("~/Documents/notes.txt"),
            Some("Documents/notes.txt")
        );
        assert_eq!(home_path_suffix("~other/file"), None);
        let windows_path = r"~\Documents\notes.txt";
        assert_eq!(mentions(&format!("@{windows_path}")), [windows_path]);
        assert_eq!(
            home_path_suffix(windows_path),
            cfg!(windows).then_some(r"Documents\notes.txt")
        );
    }

    fn tree() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fl-files-{}", uuid::Uuid::new_v4().simple()));
        for f in [
            "src/main.rs",
            "src/lib.rs",
            "src/ui/view.rs",
            "README.md",
            "target/debug/huge.bin",
            ".hidden/secret.txt",
            "ignored/x.txt",
        ] {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        std::fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
        dir
    }

    #[test]
    fn walk_skips_ignored_hidden_and_build_dirs() {
        let root = tree();
        let (entries, truncated) = walk(&root);
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(!truncated);
        assert!(paths.contains(&"src/main.rs") && paths.contains(&"src"));
        assert!(!paths.iter().any(|p| p.starts_with("target")
            || p.starts_with(".hidden")
            || p.starts_with("ignored")));
        let main = entries.iter().find(|e| e.path == "src/main.rs").unwrap();
        assert_eq!((main.dir, main.bytes), (false, 1));
    }

    #[test]
    #[cfg(unix)]
    fn in_root_directory_links_are_searchable_without_escaping_or_looping() {
        use std::os::unix::fs::symlink;
        let root = tree();
        let outside =
            std::env::temp_dir().join(format!("fl-files-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("outside-secret.txt"), "outside").unwrap();
        symlink(root.join("src"), root.join("linked-src")).unwrap();
        symlink(root.join("src/main.rs"), root.join("linked-file.rs")).unwrap();
        symlink(&root, root.join("src/loop")).unwrap();
        symlink(&outside, root.join("outside-dir")).unwrap();
        symlink(
            outside.join("outside-secret.txt"),
            root.join("outside-file"),
        )
        .unwrap();
        symlink(root.join(".hidden"), root.join("visible-hidden-alias")).unwrap();
        symlink(root.join("target"), root.join("visible-build-alias")).unwrap();
        symlink(root.join("missing"), root.join("dangling")).unwrap();

        let (entries, truncated) = walk(&root);
        assert!(!truncated);
        assert!(
            entries.len() < 40,
            "looping aliases cannot duplicate indefinitely"
        );
        let directory = entries
            .iter()
            .find(|entry| entry.path == "linked-src")
            .unwrap();
        assert_eq!((directory.dir, directory.bytes), (true, 0));
        let file = entries
            .iter()
            .find(|entry| entry.path == "linked-file.rs")
            .unwrap();
        assert_eq!(
            (file.dir, file.bytes),
            (false, 1),
            "size comes from the target file"
        );
        assert!(entries
            .iter()
            .any(|entry| entry.path == "linked-src/ui/view.rs"));
        let children: Vec<&str> = search(&entries, "linked-src/", 10)
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(children.contains(&"linked-src/main.rs") && children.contains(&"linked-src/ui"));
        assert_eq!(
            search(&entries, "linked-src/view", 1)[0].path,
            "linked-src/ui/view.rs"
        );
        for excluded in [
            "outside-dir",
            "outside-file",
            "visible-hidden-alias",
            "visible-build-alias",
            "dangling",
        ] {
            assert!(
                !entries.iter().any(|entry| entry.path == excluded
                    || entry.path.starts_with(&format!("{excluded}/"))),
                "{excluded}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn windows_separators_match_the_indexed_path_spelling() {
        let entry = |path: &str, dir| FileEntry {
            path: path.into(),
            dir,
            bytes: 0,
        };
        let entries = [
            entry("src", true),
            entry("src/ui", true),
            entry("src/ui/view.rs", false),
            entry("src/main.rs", false),
            entry("src/lib.rs", false),
            entry("README.md", false),
            entry(" docs", true),
            entry(" docs/index.md", false),
        ];
        for (typed, indexed) in [
            (r"\", ""),
            (r"src\", "src/"),
            (r"src\main", "src/main"),
            (r"src\ui/", "src/ui/"),
            (r" docs\", " docs/"),
            (r"missing\", "missing/"),
        ] {
            for limit in [0, 1, 10] {
                assert_eq!(
                    search(&entries, typed, limit),
                    search(&entries, indexed, limit)
                );
            }
        }
        assert_eq!(search(&entries, r"src\main", 1)[0].path, "src/main.rs");
        assert_eq!(search(&entries, r"src\ui\", 1)[0].path, "src/ui/view.rs");
        assert_eq!(search(&entries, "/", 10), search(&entries, "", 10));
    }

    #[test]
    fn directory_completion_prefers_exact_then_unique_canonical_spelling_without_merging() {
        let e = |path: &str, dir| FileEntry {
            path: path.into(),
            dir,
            bytes: if dir { 0 } else { 7 },
        };
        let paths = |entries: &[FileEntry], query: &str, limit| {
            search(entries, query, limit)
                .into_iter()
                .map(|e| e.path.clone())
                .collect::<Vec<_>>()
        };
        let nfd = "cafe\u{301}";
        let entries = vec![
            e(nfd, true),
            e(&format!("{nfd}/menu.txt"), false),
            e(&format!("{nfd}/sub"), true),
            e(&format!("{nfd}/sub/deep.txt"), false),
            e("cafe2/menu.txt", false),
            e("other", false),
        ];
        let expected = vec![format!("{nfd}/sub"), format!("{nfd}/menu.txt")];
        assert_eq!(paths(&entries, "café/", 20), expected);
        assert_eq!(paths(&entries, &format!("{nfd}/"), 20), expected);
        assert_eq!(paths(&entries, "café/", 1), expected[..1]);
        assert!(paths(&entries, "café/", 0).is_empty());
        assert_eq!(
            paths(&entries, "", 20),
            vec![nfd.to_string(), "other".into()]
        );
        let nfc = vec![
            e("nested/café/menu.txt", false),
            e("nested/café2/other.txt", false),
            e("nested/café/sub/deep.txt", false),
        ];
        assert_eq!(
            paths(&nfc, "nested/cafe\u{301}/", 20),
            vec!["nested/café/menu.txt"]
        );
        let mut collision = entries.clone();
        collision.push(e("café", true));
        assert!(
            paths(&collision, "café/", 20).is_empty(),
            "exact empty dir wins over canonical populated dir"
        );
        collision.push(e("café/exact.txt", false));
        assert_eq!(paths(&collision, "café/", 20), vec!["café/exact.txt"]);
        assert_eq!(paths(&collision, &format!("{nfd}/"), 20), expected);
        let collision = vec![
            e("Å", true),
            e("Å/one", false),
            e("A\u{30a}", true),
            e("A\u{30a}/two", false),
        ];
        assert!(
            paths(&collision, "Å/", 20).is_empty(),
            "ambiguous canonical lookup must require exact spelling"
        );
        assert_eq!(paths(&collision, "Å/", 20), vec!["Å/one"]);
        assert_eq!(paths(&collision, "A\u{30a}/", 20), vec!["A\u{30a}/two"]);
        let different = vec![
            e("cafe", true),
            e("cafe/plain", false),
            e("Café", true),
            e("Café/case", false),
            e("café", true),
            e("café/accent", false),
        ];
        assert_eq!(paths(&different, "Cafe\u{301}/", 20), vec!["Café/case"]);
        assert!(paths(&different, "CAFÉ/", 20).is_empty());
        assert!(paths(&different, "CAFE/", 20).is_empty());
        assert_eq!(paths(&different, "cafe/", 20), vec!["cafe/plain"]);
        assert_eq!(entries[1].bytes, 7);
        assert_eq!(entries[1].path, format!("{nfd}/menu.txt"));
    }

    #[test]
    fn search_ranks_and_completes_directories() {
        let (entries, _) = walk(&tree());
        let top: Vec<&str> = search(&entries, "", 10)
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(top, vec!["src", "README.md"]);
        let kids: Vec<&str> = search(&entries, "src/", 10)
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(kids, vec!["src/ui", "src/lib.rs", "src/main.rs"]);
        assert_eq!(search(&entries, "main", 1)[0].path, "src/main.rs");
        assert_eq!(search(&entries, "srcview", 1)[0].path, "src/ui/view.rs");
        assert!(search(&entries, "zzz", 5).is_empty());
    }

    #[test]
    fn search_stays_fast_on_a_large_index() {
        let entries: Vec<FileEntry> = (0..60_000)
            .map(|i| FileEntry {
                path: format!("pkg{}/mod{}/file{i}.rs", i % 50, i % 300),
                dir: false,
                bytes: 10,
            })
            .collect();
        let t = std::time::Instant::now();
        let hits = search(&entries, "file59999", 10);
        assert_eq!(hits[0].path, "pkg49/mod299/file59999.rs");
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn finds_mentions() {
        assert_eq!(
            mentions("explain @src/main.rs, and @README.md. mail a@b.c @"),
            vec!["src/main.rs", "README.md"]
        );
        assert_eq!(mentions("@a @a"), vec!["a"]);
    }

    /// macOS lists names in NFD; a query typed in NFC finds them, and the
    /// other way round (#789).
    #[test]
    fn search_ignores_unicode_normal_form() {
        let e = |p: &str| FileEntry {
            path: p.into(),
            dir: false,
            bytes: 1,
        };
        let nfd = [e("src/cafe\u{301}.rs"), e("src/other.rs")];
        let hit = search(&nfd, "café.rs", 5);
        assert_eq!(
            hit.first().map(|f| f.path.as_str()),
            Some("src/cafe\u{301}.rs")
        );
        let nfc = [e("src/café.rs")];
        assert_eq!(search(&nfc, "cafe\u{301}.rs", 5).len(), 1);
    }
}
