//! Path-shaped credential protection. This is an access gate, not an OS sandbox
//! or a claim that canonicalization and a later open form an atomic operation.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// User additions are typed, append-only and cannot override protected roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SensitivePathRule {
    Subtree(String),
    BasenamePrefix(String),
    BasenameSuffix(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathDecision {
    Ordinary,
    Sensitive(String),
    Forbidden,
}

#[derive(Debug, Clone, Default)]
pub struct PathPolicy {
    protected: Vec<PathBuf>,
    frozen: Vec<PathBuf>,
    rules: Vec<SensitivePathRule>,
}

impl PathPolicy {
    pub fn new(protected: Vec<PathBuf>, rules: Vec<SensitivePathRule>) -> Result<Self, String> {
        for root in &protected {
            if !root.is_absolute() {
                return Err("protected profile roots must be absolute".into());
            }
        }
        for rule in &rules {
            match rule {
                SensitivePathRule::Subtree(path) if Path::new(path).is_absolute() => {}
                SensitivePathRule::Subtree(_) => return Err("sensitive subtree must be an absolute path (expand ~ in the host configuration)".into()),
                SensitivePathRule::BasenamePrefix(value) | SensitivePathRule::BasenameSuffix(value)
                    if !value.is_empty() && !value.contains(['/', '\\', '*', '?', '\0']) => {}
                _ => return Err("sensitive basename prefix/suffix must be nonempty and contain no separators or wildcards".into()),
            }
        }
        let frozen = protected
            .iter()
            .map(|root| {
                crate::caps::canonical_path(root.to_str().ok_or("protected root is not UTF-8")?)
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            protected,
            frozen,
            rules,
        })
    }

    /// Compare the caller's configured namespace as well as its resolved
    /// destination. Re-resolve roots on each check so retargeting a configured
    /// profile symlink cannot silently move the protected boundary.
    /// Destructive directory operations cannot move/remove an ancestor that
    /// contains a protected profile, even if the ancestor itself is ordinary.
    pub fn contains_profile(&self, directory: &Path) -> Result<bool, String> {
        for root in self.protected.iter().chain(&self.frozen) {
            let resolved =
                crate::caps::canonical_path(root.to_str().ok_or("protected root is not UTF-8")?)?;
            if contains(directory, &resolved) || contains(directory, root) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn classify(&self, raw: &Path, canonical: &Path) -> Result<PathDecision, String> {
        for root in self.protected.iter().chain(&self.frozen) {
            let resolved =
                crate::caps::canonical_path(root.to_str().ok_or("protected root is not UTF-8")?)?;
            if contains(root, raw) || contains(&resolved, canonical) {
                return Ok(PathDecision::Forbidden);
            }
        }
        for path in [raw, canonical] {
            if let Some(reason) = builtin(path) {
                return Ok(PathDecision::Sensitive(reason.into()));
            }
            for rule in &self.rules {
                let matches = match rule {
                    SensitivePathRule::Subtree(root) => {
                        let configured = Path::new(root);
                        let resolved = crate::caps::canonical_path(root)?;
                        contains(configured, path) || contains(&resolved, canonical)
                    }
                    SensitivePathRule::BasenamePrefix(prefix) => path
                        .file_name()
                        .is_some_and(|name| name.as_encoded_bytes().starts_with(prefix.as_bytes())),
                    SensitivePathRule::BasenameSuffix(suffix) => path
                        .file_name()
                        .is_some_and(|name| name.as_encoded_bytes().ends_with(suffix.as_bytes())),
                };
                if matches {
                    return Ok(PathDecision::Sensitive("configured sensitive path".into()));
                }
            }
        }
        Ok(PathDecision::Ordinary)
    }
}

#[cfg(not(windows))]
fn contains(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(windows)]
fn contains(root: &Path, path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let identity = |component: Component<'_>| {
        let normal = matches!(component, Component::Normal(_));
        component
            .as_os_str()
            .encode_wide()
            .take_while(|&unit| !normal || unit != b':' as u16)
            .map(|unit| {
                if (b'A' as u16..=b'Z' as u16).contains(&unit) {
                    unit + 32
                } else {
                    unit
                }
            })
            .collect::<Vec<_>>()
    };
    let mut parts = path.components();
    root.components().all(|root| {
        parts
            .next()
            .is_some_and(|part| identity(root) == identity(part))
    })
}

fn credential_name(name: &std::ffi::OsStr) -> String {
    let name = name.to_string_lossy().to_ascii_lowercase();
    #[cfg(windows)]
    let name = name.split(':').next().unwrap_or_default().to_string();
    name
}

fn builtin(path: &Path) -> Option<&'static str> {
    let parts: Vec<_> = path
        .components()
        .filter_map(|part| {
            if let Component::Normal(name) = part {
                Some(credential_name(name))
            } else {
                None
            }
        })
        .collect();
    for name in &parts {
        if matches!(name.as_str(), ".ssh" | ".aws" | ".gnupg" | ".pki") {
            return Some("credential directory");
        }
        if name.starts_with(".env") || name.ends_with(".pem") || name.ends_with(".key") {
            return Some("credential-shaped filename");
        }
    }
    for sequence in [
        &["library", "keychains"][..],
        &[".local", "share", "keyrings"][..],
        &["microsoft", "credentials"][..],
        &["microsoft", "vault"][..],
        &["etc", "ssl", "private"][..],
    ] {
        if parts.windows(sequence.len()).any(|parts| {
            parts
                .iter()
                .map(String::as_str)
                .eq(sequence.iter().copied())
        }) {
            return Some("native credential store");
        }
    }
    let name = parts.last()?;
    if matches!(
        name.as_str(),
        ".bash_history"
            | ".zsh_history"
            | ".python_history"
            | ".node_repl_history"
            | "fish_history"
            | "consolehost_history.txt"
    ) {
        return Some("shell history");
    }
    if matches!(
        name.as_str(),
        "login data"
            | "login data for account"
            | "logins.json"
            | "key4.db"
            | "key3.db"
            | "cookies"
            | "cookies.sqlite"
    ) || name.ends_with(".binarycookies")
    {
        return Some("browser credential store");
    }
    if matches!(
        name.as_str(),
        "api_key" | "oauth_tokens.json" | "relay_secret" | ".netrc" | "_netrc" | ".git-credentials"
    ) || name.ends_with(".keychain")
        || name.ends_with(".keychain-db")
    {
        return Some("credential store file");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_paths_stay_quiet_and_all_required_shapes_are_sensitive() {
        let root = std::env::temp_dir();
        let policy = PathPolicy::default();
        let project = root.join("project").join("notes.txt");
        assert_eq!(
            policy.classify(&project, &project).unwrap(),
            PathDecision::Ordinary
        );
        for relative in [
            ".ssh/config",
            ".aws/credentials",
            ".gnupg/private",
            "Library/Keychains/login.keychain-db",
            ".local/share/keyrings/login.keyring",
            "Microsoft/Credentials/store",
            ".env",
            ".env.local",
            "server.pem",
            "server.key",
            ".bash_history",
            "fish_history",
            "Profiles/default/logins.json",
            "Chrome/Profile/Login Data",
        ] {
            let path = root.join(relative);
            assert!(
                matches!(
                    policy.classify(&path, &path).unwrap(),
                    PathDecision::Sensitive(_)
                ),
                "{relative}"
            );
        }
    }
    #[test]
    fn typed_extensions_do_not_remove_builtins_and_invalid_grammar_is_refused() {
        let policy = PathPolicy::new(
            vec![],
            vec![SensitivePathRule::BasenamePrefix("private-".into())],
        )
        .unwrap();
        let path = std::env::temp_dir().join("private-report");
        assert!(matches!(
            policy.classify(&path, &path).unwrap(),
            PathDecision::Sensitive(_)
        ));
        assert!(PathPolicy::new(
            vec![],
            vec![SensitivePathRule::BasenameSuffix("*.p12".into())]
        )
        .is_err());
        assert!(
            PathPolicy::new(vec![], vec![SensitivePathRule::Subtree("relative".into())]).is_err()
        );
    }
    #[test]
    fn protected_namespace_and_alias_destination_win_before_sensitive_rules() {
        let root = std::env::temp_dir().join(format!("fl-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let policy = PathPolicy::new(vec![root.clone()], vec![]).unwrap();
        let file = root.join("missing-child");
        assert_eq!(
            policy.classify(&file, &file).unwrap(),
            PathDecision::Forbidden
        );
        assert_eq!(
            policy
                .classify(&std::env::temp_dir().join("alias"), &file)
                .unwrap(),
            PathDecision::Forbidden
        );
        std::fs::remove_dir(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn unix_backslash_names_are_not_windows_directory_components() {
        let path = std::env::temp_dir().join(r".ssh\ordinary.txt");
        assert_eq!(
            PathPolicy::default().classify(&path, &path).unwrap(),
            PathDecision::Ordinary
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_and_alternate_stream_identity_cannot_escape_profile_or_key_rules() {
        assert!(contains(
            Path::new(r"C:\Profile\Data"),
            Path::new(r"c:\profile\DATA:stream")
        ));
        assert!(contains(
            Path::new(r"\\server\share\Data"),
            Path::new(r"\\SERVER\SHARE\data\file:stream")
        ));
        for path in [r"C:\work\private.KEY:stream", r"C:\work\.SSH\config:stream"] {
            let path = Path::new(path);
            assert!(matches!(
                PathPolicy::default().classify(path, path).unwrap(),
                PathDecision::Sensitive(_)
            ));
        }
    }
}
