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

/// BaseDirs uses the native user-profile API on Windows and a user database
/// fallback on Unix without HOME. A relative or unresolvable result cannot
/// safely establish which wider folders should be offered.
fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().and_then(|dirs| canonical_home(dirs.home_dir()))
}

fn canonical_home(home: &Path) -> Option<PathBuf> {
    if !home.is_absolute() {
        return None;
    }
    home.canonicalize().ok().filter(|home| home.is_dir())
}

fn canonical_scope(path: &Path) -> Result<PathBuf, String> {
    formless_runtime::caps::canonical_path(path.to_str().ok_or("path is not valid UTF-8")?)
}

/// Folders too broad to grant in one click: a filesystem root, home or an
/// ancestor of home. Fail closed for widening if either side is unresolved.
fn too_broad(
    dir: &Path,
    home: Option<&Path>,
    canonicalize: &impl Fn(&Path) -> Result<PathBuf, String>,
) -> bool {
    let Ok(dir) = canonicalize(dir) else {
        return true;
    };
    let Some(home) = home.and_then(|home| canonicalize(home).ok()) else {
        return true;
    };
    dir.parent().is_none() || home.starts_with(dir)
}

/// The scopes to offer for a path Capability's path, narrowest first:
/// the path itself, its folder, then the repository or project root above
/// it. Never `/` or the home folder.
pub fn choices(path: &str) -> Vec<PathChoice> {
    choices_with_home(path, home_dir().as_deref())
}

fn choices_with_home(path: &str, home: Option<&Path>) -> Vec<PathChoice> {
    choices_with(path, home, canonical_scope)
}

fn choices_with(
    path: &str,
    home: Option<&Path>,
    canonicalize: impl Fn(&Path) -> Result<PathBuf, String>,
) -> Vec<PathChoice> {
    let p = Path::new(path);
    let mut out = vec![PathChoice {
        path: path.to_string(),
        kind: if p.is_dir() { "folder" } else { "file" }.into(),
        label: String::new(),
    }];
    let folder = if p.is_dir() { Some(p) } else { p.parent() };
    if let Some(dir) = folder.filter(|d| *d != p && !too_broad(d, home, &canonicalize)) {
        out.push(PathChoice {
            path: dir.to_string_lossy().into_owned(),
            kind: "folder".into(),
            label: String::new(),
        });
    }
    if let Some((root, label)) = folder.and_then(enclosing_root) {
        let known = out.iter().any(|c| Path::new(&c.path) == root);
        if !known && !too_broad(&root, home, &canonicalize) {
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
    validate_with_home(requested, chosen, home_dir().as_deref())
}

fn validate_with_home(
    requested: &str,
    chosen: &str,
    home: Option<&Path>,
) -> Result<String, String> {
    validate_with(requested, chosen, home, canonical_scope)
}

fn validate_with(
    requested: &str,
    chosen: &str,
    home: Option<&Path>,
    canonicalize: impl Fn(&Path) -> Result<PathBuf, String>,
) -> Result<String, String> {
    let chosen_p = canonicalize(Path::new(chosen))?;
    // Both sides resolved, so a symlink on the way (macOS /tmp, a linked
    // home or repo) doesn't make a real parent look foreign (#772).
    let requested_p = canonicalize(Path::new(requested))?;
    if !requested_p.starts_with(&chosen_p) {
        return Err(format!("{chosen} does not contain {requested}"));
    }
    if chosen_p != requested_p && too_broad(&chosen_p, home, &canonicalize) {
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

    /// A repo reached through a symlink can still be granted whole, for a
    /// file not written yet too (#772).
    #[cfg(unix)]
    #[test]
    fn symlinked_paths_widen_to_their_real_folder() {
        let real = tmp("real");
        let link = real.parent().unwrap().join(format!(
            "fl-scope-link-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        ));
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let via_link = link.join("src/new.rs");
        let granted = validate(&via_link.to_string_lossy(), &real.to_string_lossy()).unwrap();
        assert_eq!(Path::new(&granted), real.as_path());
        // and the grant covers a sibling file
        assert!(real.join("src/other.rs").starts_with(&granted));
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&real);
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
    fn injected_windows_home_blocks_widening_without_home_environment() {
        // Forward-slash Windows paths keep component boundaries on every
        // host. Inject their already-canonical forms too, so this test never
        // depends on a real C: drive, filesystem profile or process HOME.
        let canonical = |path: &Path| Ok(path.to_path_buf());
        let home = Path::new("C:/Users/Alice");
        let requested = "C:/Users/Alice/project/src/main.rs";
        for chosen in ["C:/", "C:/Users", "C:/Users/Alice"] {
            let error = validate_with(requested, chosen, Some(home), canonical).unwrap_err();
            assert!(error.contains("too broad"), "{chosen}: {error}");
        }
        assert!(validate_with(requested, "C:/Users/Alice/project", Some(home), canonical).is_ok());
        // Parent comparison must be by component, never a string prefix.
        assert!(validate_with(
            "C:/Users/Alice-secrets/file",
            "C:/Users/Alice",
            Some(home),
            canonical
        )
        .is_err());
        let choices = choices_with("C:/Users/Alice/new.txt", Some(home), canonical);
        assert_eq!(choices.len(), 1, "home is not a wider folder choice");
        let choices = choices_with("C:/Users/new.txt", Some(home), canonical);
        assert_eq!(choices.len(), 1, "an ancestor of home is not a choice");
        let choices = choices_with(requested, Some(home), canonical);
        assert!(choices
            .iter()
            .any(|choice| choice.path == "C:/Users/Alice/project/src"));
    }

    #[test]
    fn an_unknown_home_disables_widening_but_keeps_the_exact_request() {
        assert!(canonical_home(Path::new("relative/home")).is_none());
        let root = tmp("unknown-home");
        let requested = root.join("src/new.rs");
        let requested = requested.to_str().unwrap();
        assert_eq!(choices_with_home(requested, None).len(), 1);
        assert!(validate_with_home(requested, root.to_str().unwrap(), None)
            .unwrap_err()
            .contains("too broad"));
        assert!(validate_with_home(requested, requested, None).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn home_and_chosen_scope_are_both_canonicalized() {
        let root = tmp("linked-home");
        let home = root.join("Users/Alice");
        std::fs::create_dir_all(home.join("project")).unwrap();
        let alias = root.join("home-link");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        assert_eq!(canonical_home(&alias), Some(home.canonicalize().unwrap()));
        let requested = home.join("project/new.rs");
        for chosen in [&home, &alias, &root.join("Users")] {
            assert!(validate_with_home(
                requested.to_str().unwrap(),
                chosen.to_str().unwrap(),
                Some(&alias)
            )
            .unwrap_err()
            .contains("too broad"));
        }
        assert_eq!(
            choices_with_home(alias.join("new.rs").to_str().unwrap(), Some(&home)).len(),
            1
        );
        assert!(validate_with_home(
            requested.to_str().unwrap(),
            home.join("project").to_str().unwrap(),
            Some(&alias)
        )
        .is_ok());
        std::fs::remove_dir_all(root).unwrap();
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
