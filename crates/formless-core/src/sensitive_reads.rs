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
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
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
