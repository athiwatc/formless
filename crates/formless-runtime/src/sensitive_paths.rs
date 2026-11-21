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

    /// A read consent must not turn a configured private namespace into
    /// ordinary files through a subsequent rename. Ancestors cannot move the
    /// namespace either; callers can read it with consent or edit the owner's
    /// configuration explicitly instead.
    pub fn relocation_allowed(
        &self,
        raw_from: &Path,
        from: &Path,
        to: &Path,
    ) -> Result<bool, String> {
        if from.is_dir()
            && (self.classify(raw_from, from)? != PathDecision::Ordinary
                || directory_namespace_prefix(raw_from)
                || directory_namespace_prefix(from))
        {
            // A directory consent covers only that directory's listing or
            // metadata. It cannot authorize declassifying its descendants.
            // Reject cross-namespace moves conservatively as well.
            return Ok(false);
        }
        for rule in &self.rules {
            if let SensitivePathRule::Subtree(configured) = rule {
                let root = crate::caps::canonical_path(configured)?;
                if contains(from, &root) {
                    return Ok(false);
                }
                if contains(&root, from) && !contains(&root, to) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
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
                        .is_some_and(|name| basename_matches(name, prefix, true)),
                    SensitivePathRule::BasenameSuffix(suffix) => path
                        .file_name()
                        .is_some_and(|name| basename_matches(name, suffix, false)),
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
fn basename_matches(name: &std::ffi::OsStr, affix: &str, prefix: bool) -> bool {
    if prefix {
        name.as_encoded_bytes().starts_with(affix.as_bytes())
    } else {
        name.as_encoded_bytes().ends_with(affix.as_bytes())
    }
}

#[cfg(windows)]
fn basename_matches(name: &std::ffi::OsStr, affix: &str, prefix: bool) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let fold = |unit: u16| {
        if (b'A' as u16..=b'Z' as u16).contains(&unit) {
            unit + 32
        } else {
            unit
        }
    };
    let name: Vec<_> = name
        .encode_wide()
        .take_while(|&unit| unit != b':' as u16)
        .map(fold)
        .collect();
    let affix: Vec<_> = affix.encode_utf16().map(fold).collect();
    if prefix {
        name.starts_with(&affix)
    } else {
        name.ends_with(&affix)
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

const DIRECTORY_SEQUENCES: &[&[&str]] = &[
    &["library", "keychains"],
    &[".local", "share", "keyrings"],
    &["microsoft", "credentials"],
    &["microsoft", "vault"],
    &["etc", "ssl", "private"],
];

/// Removing a prefix of a recognized sequence also removes the descendants'
/// classification: Library -> Ordinary would wash Library/Keychains/files.
/// This deliberately rejects such directory names even if currently empty;
/// no recursive, racy traversal is used to prove absence of secrets.
fn directory_namespace_prefix(path: &Path) -> bool {
    let parts: Vec<_> = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(credential_name(name)),
            _ => None,
        })
        .collect();
    DIRECTORY_SEQUENCES.iter().any(|sequence| {
        (1..sequence.len()).any(|len| {
            parts.len() >= len
                && parts[parts.len() - len..]
                    .iter()
                    .map(String::as_str)
                    .eq(sequence[..len].iter().copied())
        })
    })
}

/// Conventional env-template leaf names. This is only a builtin filename
/// exception, not a content check or an override of protected/custom rules.
/// Directories and symlinks retain their namespace protection, including
/// during relocation. Missing leaves remain eligible for ordinary creation.
pub fn is_env_template_file(path: &Path) -> bool {
    let known_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                ".env.example" | ".env.sample" | ".env.template" | ".env.dist"
            )
        });
    if !known_name {
        return false;
    }
    match path.symlink_metadata() {
        Ok(meta) => meta.file_type().is_file(),
        // New template files can be created normally; existing symlinks,
        // directories, special files and metadata errors stay conservative.
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

fn builtin(path: &Path) -> Option<&'static str> {
    if formless_protocol::is_composer_auth_path(path) {
        return Some("Composer credentials");
    }
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
    for (index, name) in parts.iter().enumerate() {
        if matches!(
            name.as_str(),
            ".ssh" | ".aws" | ".gnupg" | ".pki" | ".kube" | ".docker" | ".password-store"
        ) {
            return Some("credential directory");
        }
        if (name.starts_with(".env") && !(index + 1 == parts.len() && is_env_template_file(path)))
            || name.ends_with(".pem")
            || name.ends_with(".ppk")
            || name.ends_with(".key")
        {
            return Some("credential-shaped filename");
        }
    }
    for sequence in DIRECTORY_SEQUENCES {
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
        "api_key"
            | "oauth_tokens.json"
            | "relay_secret"
            | ".netrc"
            | "_netrc"
            | ".htpasswd"
            | ".git-credentials"
            | "credentials.tfrc.json"
            | ".terraformrc"
            | "terraform.rc"
    ) || name == "credentials.toml"
        || name.ends_with(".keychain")
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
    fn cargo_credential_files_are_sensitive_without_classifying_the_cache() {
        let home = std::env::temp_dir().join("formless-home");
        let policy = PathPolicy::default();
        for relative in [".cargo/credentials.toml", "custom-cargo/CREDENTIALS.TOML"] {
            let path = home.join(relative);
            assert!(matches!(
                policy.classify(&path, &path).unwrap(),
                PathDecision::Sensitive(_)
            ));
        }
        for relative in [
            ".cargo/registry/cache.txt",
            ".cargo/credentials.toml.example",
            "notes.txt",
        ] {
            let path = home.join(relative);
            assert_eq!(
                policy.classify(&path, &path).unwrap(),
                PathDecision::Ordinary
            );
        }
    }

    #[test]
    fn ordinary_paths_stay_quiet_and_all_required_shapes_are_sensitive() {
        let root = std::env::temp_dir();
        let policy = PathPolicy::default();
        let project = root.join("project").join("notes.txt");
        assert_eq!(
            policy.classify(&project, &project).unwrap(),
            PathDecision::Ordinary
        );
        let note = root.join("project/ppk-notes.txt");
        assert_eq!(
            policy.classify(&note, &note).unwrap(),
            PathDecision::Ordinary
        );
        for relative in [
            ".ssh/config",
            ".aws/credentials",
            ".gnupg/private",
            ".kube/config",
            ".docker/config.json",
            ".password-store/account.gpg",
            "Library/Keychains/login.keychain-db",
            ".local/share/keyrings/login.keyring",
            "Microsoft/Credentials/store",
            ".env",
            ".env.local",
            "server.pem",
            "server.ppk",
            "identity.PPK",
            "server.key",
            ".bash_history",
            "www/.htpasswd",
            "site/.HTPASSWD",
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
    fn apache_password_file_checks_both_path_spellings_and_requires_exact_name() {
        let root = std::env::temp_dir().join("project");
        let policy = PathPolicy::default();
        let secret = root.join(".HTPASSWD");
        let alias = root.join("alias.txt");
        for (requested, resolved) in [(&alias, &secret), (&secret, &alias)] {
            assert!(matches!(
                policy.classify(requested, resolved).unwrap(),
                PathDecision::Sensitive(_)
            ));
        }
        for name in ["htpasswd", "htpasswd.txt", ".htpasswd.bak"] {
            let path = root.join(name);
            assert_eq!(
                policy.classify(&path, &path).unwrap(),
                PathDecision::Ordinary
            );
        }
    }
    #[test]
    fn composer_auth_file_is_classified_before_ordinary_config_files() {
        let root = std::env::temp_dir().join("formless-project");
        let policy = PathPolicy::default();
        for relative in [".composer/auth.json", ".config/composer/auth.json"] {
            let path = root.join(relative);
            let alias = root.join("alias.txt");
            assert!(matches!(
                policy.classify(&alias, &path).unwrap(),
                PathDecision::Sensitive(_)
            ));
        }
        for relative in [
            "auth.json",
            ".composer/cache/auth.json",
            ".config/composer/config.json",
        ] {
            let path = root.join(relative);
            assert_eq!(
                policy.classify(&path, &path).unwrap(),
                PathDecision::Ordinary
            );
        }
    }

    #[test]
    fn protects_terraform_config_during_tool_reads() {
        let root = std::env::temp_dir();
        let policy = PathPolicy::default();
        for relative in [
            ".terraform.d/credentials.tfrc.json",
            ".terraformrc",
            "AppData/Roaming/TERRAFORM.RC",
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
        for relative in [
            ".terraform/notes.txt",
            "credentials.tfrc.json.example",
            ".terraformrc.sample",
        ] {
            let path = root.join(relative);
            assert_eq!(
                policy.classify(&path, &path).unwrap(),
                PathDecision::Ordinary,
                "{relative}"
            );
        }
    }

    #[test]
    fn env_template_files_are_ordinary_without_declassifying_directories_or_rules() {
        struct Owned(PathBuf);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = std::env::temp_dir().join(format!("fl-env-template-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _owned = Owned(root.clone());
        let policy = PathPolicy::default();
        for name in [".env.example", ".env.sample", ".env.template", ".env.dist"] {
            let path = root.join(name);
            std::fs::write(&path, "OPTION=placeholder").unwrap();
            assert_eq!(
                policy.classify(&path, &path).unwrap(),
                PathDecision::Ordinary,
                "{name}"
            );
            let custom = PathPolicy::new(
                vec![],
                vec![SensitivePathRule::BasenamePrefix(".env".into())],
            )
            .unwrap();
            assert!(
                matches!(
                    custom.classify(&path, &path).unwrap(),
                    PathDecision::Sensitive(_)
                ),
                "custom rule: {name}"
            );
            let protected = PathPolicy::new(vec![root.clone()], vec![]).unwrap();
            assert_eq!(
                protected.classify(&path, &path).unwrap(),
                PathDecision::Forbidden
            );
        }
        let upper = root.join(".ENV.EXAMPLE");
        std::fs::write(&upper, "OPTION=placeholder").unwrap();
        assert_eq!(
            policy.classify(&upper, &upper).unwrap(),
            PathDecision::Ordinary
        );
        for name in [
            ".env",
            ".env.local",
            ".env.production",
            ".env.unknown",
            ".env.local.example",
            ".env.example.bak",
            ".env.example:stream",
            ".environment",
            ".ssh/.env.example",
            ".env.template/notes.txt",
        ] {
            let path = root.join(name);
            assert!(
                matches!(
                    policy.classify(&path, &path).unwrap(),
                    PathDecision::Sensitive(_)
                ),
                "{name}"
            );
        }
        let directory = root.join("nested/.env.example");
        std::fs::create_dir_all(&directory).unwrap();
        assert!(matches!(
            policy.classify(&directory, &directory).unwrap(),
            PathDecision::Sensitive(_)
        ));
        assert!(!policy
            .relocation_allowed(&directory, &directory, &root.join("ordinary"))
            .unwrap());
        #[cfg(unix)]
        {
            let link = root.join("links/.env.sample");
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            let target = root.join("notes.txt");
            std::fs::write(&target, "ordinary fixture").unwrap();
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(matches!(
                policy.classify(&link, &target).unwrap(),
                PathDecision::Sensitive(_)
            ));
        }
        let template = root.join(".env.example");
        let actual_secret = root.join(".env.local");
        assert!(
            matches!(
                policy.classify(&template, &actual_secret).unwrap(),
                PathDecision::Sensitive(_)
            ),
            "canonical secret still wins"
        );
        assert!(
            matches!(
                policy.classify(&actual_secret, &template).unwrap(),
                PathDecision::Sensitive(_)
            ),
            "raw secret still wins"
        );
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
        // The second argument is the resolved destination, including when
        // Darwin's temporary-directory spelling traverses /var -> /private/var.
        let canonical = crate::caps::canonical_path(file.to_str().unwrap()).unwrap();
        assert_eq!(
            policy.classify(&file, &canonical).unwrap(),
            PathDecision::Forbidden
        );
        assert_eq!(
            policy
                .classify(&std::env::temp_dir().join("alias"), &canonical)
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
        let custom = PathPolicy::new(
            vec![],
            vec![SensitivePathRule::BasenameSuffix(".p12".into())],
        )
        .unwrap();
        let custom_path = Path::new(r"C:\work\private.P12:stream");
        assert!(matches!(
            custom.classify(custom_path, custom_path).unwrap(),
            PathDecision::Sensitive(_)
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
