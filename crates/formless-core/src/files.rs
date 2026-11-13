//! File search for `@` mentions (SPEC §5.6): a walk of a root that
//! respects `.gitignore`, skips hidden and build directories, and stops
//! at `MAX_WALK` entries so huge trees stay responsive.

use crate::fuzzy::fuzzy_match;
use serde::{Deserialize, Serialize};
use std::path::Path;

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
/// level; a query ending in `/` lists that directory's children, so
/// `@dir/` completes step by step. Otherwise entries are ranked by fuzzy
/// match on the path, with a bonus when the file name itself matches.
pub fn search<'a>(entries: &'a [FileEntry], query: &str, limit: usize) -> Vec<&'a FileEntry> {
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
        let mut kids: Vec<&FileEntry> = entries
            .iter()
            .filter(|e| match e.path.strip_prefix(query) {
                Some(rest) => !rest.is_empty() && !rest.contains('/'),
                None => false,
            })
            .collect();
        kids.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.path.cmp(&b.path)));
        kids.truncate(limit);
        return kids;
    }
    let base_query = query.rsplit('/').next().unwrap_or(query);
    let mut scored: Vec<(i64, &FileEntry)> = entries
        .iter()
        .filter_map(|e| {
            let (mut score, _) = fuzzy_match(query, &e.path)?;
            let name = e.path.rsplit('/').next().unwrap_or(&e.path);
            if let Some((s, _)) = fuzzy_match(base_query, name) {
                score += 10 + s;
            }
            Some((score, e))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.path.len().cmp(&b.1.path.len()))
            .then(a.1.path.cmp(&b.1.path))
    });
    scored.into_iter().take(limit).map(|(_, e)| e).collect()
}

#[path = "file_mentions.rs"]
mod file_mentions;
pub use file_mentions::{
    encode_mention, mention_completion, mention_ranges, mentions, MentionCompletion,
    MentionInsertion,
};

#[cfg(test)]
mod tests {
    use super::*;

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
