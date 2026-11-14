//! Compile user-only additive rules and the current daemon's hard boundary.
use crate::{config::SecurityCfg, Paths};
use formless_runtime::sensitive_paths::{PathPolicy, SensitivePathRule};

pub(crate) fn policy(
    paths: &Paths,
    security: &SecurityCfg,
    anchors: &[std::path::PathBuf],
) -> Result<PathPolicy, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut protected = [&paths.data_dir, &paths.config_dir]
        .into_iter()
        .map(|root| formless_runtime::caps::lexical_absolute(&cwd, root))
        .collect::<Result<Vec<_>, _>>()?;
    protected.extend(credential_file_paths()?);
    protected.extend_from_slice(anchors);
    let rules = security
        .sensitive_paths
        .iter()
        .map(|rule| {
            Ok(match rule {
                SensitivePathRule::Subtree(path)
                    if path == "~"
                        || path.starts_with("~/")
                        || (cfg!(windows) && path.starts_with("~\\")) =>
                {
                    let home = directories::BaseDirs::new()
                        .ok_or("cannot establish native home for sensitive path")?;
                    let tail = path.get(2..).unwrap_or_default();
                    let root = formless_runtime::caps::lexical_absolute(
                        home.home_dir(),
                        std::path::Path::new(tail),
                    )?;
                    SensitivePathRule::Subtree(
                        root.into_os_string()
                            .into_string()
                            .map_err(|_| "sensitive path cannot be represented as UTF-8")?,
                    )
                }
                other => other.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    PathPolicy::new(protected, rules)
}

/// Inspect only the path-valued variables understood by load_api_key and
/// provider_key. Include any normalized named-provider identifier, even if
/// that provider is created later. Never call a credential reader here.
pub(crate) fn credential_file_paths() -> Result<Vec<std::path::PathBuf>, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    std::env::vars_os()
        .filter(|(name, path)| {
            !path.is_empty()
                && name.to_str().is_some_and(|name| {
                    #[cfg(windows)]
                    let name = name.to_ascii_uppercase();
                    name == "FORMLESS_API_KEY_FILE"
                        || name
                            .strip_prefix("FORMLESS_API_KEY_")
                            .and_then(|name| name.strip_suffix("_FILE"))
                            .is_some_and(|name| {
                                name.bytes()
                                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                            })
                })
        })
        .map(|(_, path)| {
            formless_runtime::caps::lexical_absolute(&cwd, std::path::Path::new(&path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_credential_file_overrides_are_unset_without_weakening_nonempty_paths() {
        let env = crate::credential_env::EnvGuard::isolated();
        for name in ["FORMLESS_API_KEY_FILE", "FORMLESS_API_KEY_FIXTURE_FILE"] {
            env.set(name, "");
        }
        assert!(credential_file_paths().unwrap().is_empty());
        for name in ["FORMLESS_API_KEY_FILE", "FORMLESS_API_KEY_FIXTURE_FILE"] {
            assert!(crate::config::key_from_file_env(name).is_none());
        }
        struct Owned(std::path::PathBuf);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let root = std::env::temp_dir().join(format!("fl-empty-key-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let _owned = Owned(root.clone());
        let key = root.join("synthetic-key");
        std::fs::write(&key, "synthetic-fixture-key").unwrap();
        env.set("FORMLESS_API_KEY_FILE", &key);
        assert!(credential_file_paths().unwrap().contains(&key));
        assert_eq!(
            crate::config::key_from_file_env("FORMLESS_API_KEY_FILE")
                .unwrap()
                .unwrap(),
            "synthetic-fixture-key"
        );
        let missing = root.join("missing");
        env.set("FORMLESS_API_KEY_FILE", &missing);
        assert!(credential_file_paths().unwrap().contains(&missing));
        assert!(crate::config::key_from_file_env("FORMLESS_API_KEY_FILE")
            .unwrap()
            .is_err());
    }
}
