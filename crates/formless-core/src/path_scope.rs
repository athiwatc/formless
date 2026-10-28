//! Wider path grants offered on approval prompts (SPEC §10.3): the file
//! asked for, its folder, or the repository (or project) it lies in.

use formless_protocol::PathChoice;
use std::path::{Path, PathBuf};

/// Version-control markers, strongest first: a folder holding one is a
/// repository root (`.git` may be a file, in worktrees and submodules).
const VCS: &[(&str, &str)] = &[
    (".git", "git"),
    (".hg", "Mercurial"),
    (".jj", "Jujutsu"),
    (".svn", "Subversion"),
    (".pijul", "Pijul"),
    ("_darcs", "Darcs"),
    (".fossil", "Fossil"),
    ("_FOSSIL_", "Fossil"),
];

/// Language project files: a weaker hint, used only outside any repository.
const PROJECT: &[&str] = &["Cargo.toml", "package.json", "pyproject.toml", "go.mod"];

/// The nearest folder at or above `start` that is a repository root, with
/// the kind of repository; else the nearest project root, labelled by its
/// marker file.
pub fn enclosing_root(start: &Path) -> Option<(PathBuf, String)> {
    let mut project = None;
    for dir in start.ancestors() {
        if let Some((_, kind)) = VCS.iter().find(|(m, _)| dir.join(m).exists()) {
            return Some((dir.to_path_buf(), kind.to_string()));
        }
        if project.is_none() {
            if let Some(m) = PROJECT.iter().find(|m| dir.join(m).is_file()) {
                project = Some((dir.to_path_buf(), m.to_string()));
            }
        }
    }
    project
}

/// Folders too broad to grant in one click: `/` and the home folder, and
/// anything above home.
fn too_broad(dir: &Path) -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    dir.parent().is_none() || home.is_some_and(|h| h.starts_with(dir))
}

/// The scopes to offer for a path Capability's path, narrowest first:
/// the path itself, its folder, then the repository or project root above
/// it. Never `/` or the home folder.
pub fn choices(path: &str) -> Vec<PathChoice> {
    let p = Path::new(path);
    let mut out = vec![PathChoice {
        path: path.to_string(),
        kind: if p.is_dir() { "folder" } else { "file" }.into(),
        label: String::new(),
    }];
    let folder = if p.is_dir() { Some(p) } else { p.parent() };
    if let Some(dir) = folder.filter(|d| *d != p && !too_broad(d)) {
        out.push(PathChoice {
            path: dir.to_string_lossy().into_owned(),
            kind: "folder".into(),
            label: String::new(),
        });
    }
    if let Some((root, label)) = folder.and_then(enclosing_root) {
        let known = out.iter().any(|c| Path::new(&c.path) == root);
        if !known && !too_broad(&root) {
            out.push(PathChoice {
                path: root.to_string_lossy().into_owned(),
                kind: if VCS.iter().any(|(_, k)| *k == label) {
                    "repo"
                } else {
                    "project"
                }
                .into(),
                label,
            });
        }
    }
    out
}

/// Accept a chosen scope for `requested` only when it is the path itself
/// or a folder above it (whole components, so `/proj` never covers
/// `/proj-secrets`) and not too broad. Returns the canonical folder.
pub fn validate(requested: &str, chosen: &str) -> Result<String, String> {
    let chosen_p = formless_runtime::caps::canonical_path(chosen)?;
    let requested_p = Path::new(requested);
    if !requested_p.starts_with(&chosen_p) {
        return Err(format!("{chosen} does not contain {requested}"));
    }
    if chosen_p != requested_p && too_broad(&chosen_p) {
        return Err(format!(
            "{chosen} is too broad to grant from an approval prompt"
        ));
    }
    Ok(chosen_p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "fl-scope-{tag}-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        ));
        std::fs::create_dir_all(d.join("src")).unwrap();
        d
    }

    #[test]
    fn repositories_of_each_kind_are_found() {
        for (marker, kind, is_file) in [
            (".git", "git", false),
            (".git", "git", true),
            (".hg", "Mercurial", false),
            (".jj", "Jujutsu", false),
        ] {
            let root = tmp(kind);
            if is_file {
                std::fs::write(root.join(marker), "gitdir: elsewhere").unwrap();
            } else {
                std::fs::create_dir(root.join(marker)).unwrap();
            }
            let file = root.join("src/foo.rs");
            std::fs::write(&file, "").unwrap();
            let c = choices(file.to_str().unwrap());
            let kinds: Vec<&str> = c.iter().map(|c| c.kind.as_str()).collect();
            assert_eq!(kinds, ["file", "folder", "repo"], "{marker}");
            assert_eq!(Path::new(&c[2].path), root);
            assert_eq!(c[2].label, kind);
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn a_project_file_is_a_weaker_hint() {
        let root = tmp("proj");
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        let file = root.join("src/main.rs");
        let c = choices(file.to_str().unwrap());
        assert_eq!(c.last().unwrap().kind, "project");
        assert_eq!(c.last().unwrap().label, "Cargo.toml");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn chosen_scopes_must_contain_the_path_and_stay_narrow() {
        let root = tmp("val");
        let file = root.join("src/foo.rs");
        let f = file.to_str().unwrap();
        assert!(validate(f, root.to_str().unwrap()).is_ok());
        assert!(validate(f, f).is_ok());
        let sibling = format!("{}-secrets", root.display());
        assert!(validate(&format!("{sibling}/x"), root.to_str().unwrap()).is_err());
        assert!(validate(f, root.join("other").to_str().unwrap()).is_err());
        assert!(validate(f, "/").is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
