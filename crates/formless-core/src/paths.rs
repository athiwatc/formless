use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::{Path, PathBuf};

/// Longest unix socket path, NUL included (`sun_path`: 104 bytes on
/// macOS and the BSDs, 108 on Linux).
#[cfg(all(unix, target_os = "linux"))]
const SUN_PATH_MAX: usize = 108;
#[cfg(all(unix, not(target_os = "linux")))]
const SUN_PATH_MAX: usize = 104;

/// Default-profile entries share the parent namespace with named profiles.
/// Keep this list beside profile resolution whenever a new owned root is added.
pub const RESERVED_PROFILE_NAMES: &[&str] = &[
    "api_key",
    "artifacts",
    "backups",
    "commands",
    "exports",
    "last_run_session",
    "mcp",
    "models",
    "profiles",
    "relay_secret",
    "review-objects",
    "review-receipts",
    "skills",
];

/// Validate before joining paths or touching locks, services or credentials.
pub fn validate_profile_name(name: &str) -> Result<()> {
    let charset = "use 1–64 ASCII letters, digits, '-' or '_', starting with a letter or digit";
    if name.is_empty()
        || name.len() > 64
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        anyhow::bail!("invalid profile name {name:?}: {charset}");
    }
    let upper = name.to_ascii_uppercase();
    let device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.as_bytes()[3], b'1'..=b'9'));
    if device
        || (name != "default" && name.eq_ignore_ascii_case("default"))
        || RESERVED_PROFILE_NAMES
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
    {
        anyhow::bail!("reserved profile name {name:?}: choose another name ({charset}); existing directories are preserved, not migrated or removed");
    }
    Ok(())
}

#[derive(Debug)]
pub struct ProfileInventory {
    pub profiles: Vec<Paths>,
    /// Ambiguous legacy entries are preserved and must block parent cleanup.
    pub diagnostics: Vec<String>,
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn profile_marker_with(
    directory: &Path,
    mut inspect: impl FnMut(&Path) -> std::io::Result<std::fs::Metadata>,
) -> Result<bool> {
    for marker in ["formless.db", "config.toml"] {
        let path = directory.join(marker);
        match inspect(&path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot inspect profile marker {path:?}"))
            }
        }
    }
    Ok(false)
}

fn absolute_identity(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in std::path::absolute(path)?.components() {
        match component {
            std::path::Component::CurDir => (),
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    #[cfg(windows)]
    {
        normalized = PathBuf::from(normalized.to_string_lossy().to_lowercase());
    }
    Ok(normalized)
}
fn canonical_identity(path: &Path) -> Result<PathBuf> {
    let path = path
        .to_str()
        .context("profile namespace requires UTF-8 paths")?;
    absolute_identity(&formless_runtime::caps::canonical_path(path).map_err(anyhow::Error::msg)?)
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub profile: String,
}

impl Paths {
    /// Resolve OS dirs. Env overrides (for tests): FORMLESS_DATA_DIR,
    /// FORMLESS_CONFIG_DIR. Profile appends a suffix dir (SPEC §9.4).
    pub fn resolve(profile: Option<&str>) -> Result<Self> {
        let paths = Self::resolve_read_only(profile)?;
        let _lease = crate::profile_lock::ProfileLock::hierarchy_shared(&paths)?;
        std::fs::create_dir_all(&paths.data_dir)?;
        std::fs::create_dir_all(&paths.config_dir)?;
        Ok(paths)
    }

    /// Resolve the same profile paths without creating directories or
    /// opening profile state, for previews that must not change anything.
    pub fn resolve_read_only(profile: Option<&str>) -> Result<Self> {
        let profile = profile.unwrap_or("default");
        validate_profile_name(profile)?;
        let (data, config) = match (
            std::env::var("FORMLESS_DATA_DIR"),
            std::env::var("FORMLESS_CONFIG_DIR"),
        ) {
            (Ok(d), Ok(c)) => (PathBuf::from(d), PathBuf::from(c)),
            (Ok(d), _) => (PathBuf::from(d.clone()), PathBuf::from(d)),
            (_, Ok(c)) => (PathBuf::from(c.clone()), PathBuf::from(c)),
            _ => {
                let dirs = ProjectDirs::from("", "", "formless")
                    .context("could not resolve OS config/data directories")?;
                (
                    dirs.data_dir().to_path_buf(),
                    dirs.config_dir().to_path_buf(),
                )
            }
        };
        Self {
            data_dir: data,
            config_dir: config,
            profile: "default".into(),
        }
        .named(profile)
    }

    /// Existing valid profiles only. Use `profile_inventory` when displaying
    /// diagnostics or preparing cleanup; skipped legacy entries are not disposable.
    pub fn list_profiles(&self) -> Vec<Paths> {
        self.profile_inventory().profiles
    }

    pub fn profile_inventory(&self) -> ProfileInventory {
        let base = self.base();
        let identities = match [&base.data_dir, &base.config_dir]
            .into_iter().map(|root| absolute_identity(root)).collect::<Result<Vec<_>>>() {
            Ok(identities)=>identities,
            Err(error)=>return ProfileInventory {profiles:vec![base],diagnostics:vec![format!("Cannot identify profile roots: {error:#}; files are preserved; restore access before cleanup.")]},
        };
        let mut names = std::collections::BTreeSet::new();
        let mut diagnostics = std::collections::BTreeSet::new();
        for dir in [&base.data_dir, &base.config_dir] {
            let entries = match std::fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    diagnostics.insert(format!("Cannot inspect profile directory {dir:?}: {error}; files are preserved. Restore access and retry `formless profiles list` before cleanup."));
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        diagnostics.insert(format!("Cannot inspect an entry in {dir:?}: {error}; restore access before cleanup."));
                        continue;
                    }
                };
                let metadata = match std::fs::symlink_metadata(entry.path()) {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        diagnostics.insert(format!("Cannot inspect {:?}: {error}; files are preserved; restore access before cleanup.",entry.path()));
                        continue;
                    }
                };
                if linked(&metadata) {
                    diagnostics.insert(format!("Skipped linked profile entry {:?}; its files are preserved. Back up and inspect this link manually before cleanup.",entry.path()));
                    continue;
                }
                if !metadata.is_dir() {
                    continue;
                }
                // A configured nested default root is not a named profile.
                // The named resolver still rejects any attempt to select it.
                let entry_identity = match absolute_identity(&entry.path()) {
                    Ok(identity) => identity,
                    Err(error) => {
                        diagnostics.insert(format!("Cannot identify {:?}: {error:#}; files are preserved; inspect this directory before cleanup.",entry.path()));
                        continue;
                    }
                };
                if identities.contains(&entry_identity) {
                    continue;
                }
                // Probe markers only inside a real entry, never through a link.
                let marked = match profile_marker_with(&entry.path(), |path| {
                    path.symlink_metadata()
                }) {
                    Ok(marked) => marked,
                    Err(error) => {
                        diagnostics.insert(format!("Skipped directory {:?}: {error:#}; files are preserved. Restore access and retry `formless profiles list` before cleanup.",entry.path()));
                        continue;
                    }
                };
                if !marked {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    diagnostics.insert(format!("Skipped non-UTF-8 profile directory {:?}; back it up and recover it manually before cleanup.",entry.path()));
                    continue;
                };
                if name == "default" {
                    diagnostics.insert(format!("Skipped legacy profile directory {:?}: default is the parent profile; back up and recover this directory manually before cleanup.",entry.path()));
                    continue;
                }
                match base.named(&name) {
                    Ok(_) => {
                        names.insert(name);
                    }
                    Err(error) => {
                        diagnostics.insert(format!("Skipped profile directory {:?}: {error}. Its files are preserved. Back up this directory outside the Formless roots and recover it manually before cleanup; no automatic migration is performed.",entry.path()));
                    }
                }
            }
        }
        let mut profiles = vec![base.clone()];
        for name in names {
            match base.named(&name) {
                Ok(profile) => profiles.push(profile),
                Err(error) => {
                    diagnostics.insert(format!("Profile {name:?} changed during discovery: {error}; files are preserved; inspect it before cleanup."));
                }
            }
        }
        ProfileInventory {
            profiles,
            diagnostics: diagnostics.into_iter().collect(),
        }
    }

    /// The default profile's configured parent paths.
    fn base(&self) -> Paths {
        if self.profile == "default" {
            return self.clone();
        }
        let up = |path: &PathBuf| {
            path.parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| path.clone())
        };
        Paths {
            data_dir: up(&self.data_dir),
            config_dir: up(&self.config_dir),
            profile: "default".into(),
        }
    }

    /// Another profile's paths, without creating anything or accepting aliases.
    pub fn named(&self, profile: &str) -> Result<Paths> {
        validate_profile_name(profile)?;
        let base = self.base();
        if profile == "default" {
            return Ok(base);
        }
        let paths = Paths {
            data_dir: base.data_dir.join(profile),
            config_dir: base.config_dir.join(profile),
            profile: profile.to_owned(),
        };
        paths.validate_profile_namespace()?;
        Ok(paths)
    }

    /// Independent validation for public/constructed Paths at mutation boundaries.
    pub fn validate_profile_namespace(&self) -> Result<()> {
        validate_profile_name(&self.profile)?;
        if self.profile == "default" {
            return Ok(());
        }
        let base = self.base();
        for path in [&self.data_dir, &self.config_dir] {
            if path.file_name() != Some(std::ffi::OsStr::new(&self.profile)) {
                anyhow::bail!("profile {:?} does not match its configured directory {path:?}; files are preserved",self.profile);
            }
            match path.symlink_metadata() {
                Ok(metadata) if linked(&metadata) || !metadata.is_dir()=>anyhow::bail!("profile path {path:?} is linked or is not a real directory; files are preserved"),
                Err(error) if error.kind()!=std::io::ErrorKind::NotFound=>return Err(error).context("inspect profile namespace before mutation"),
                _=>(),
            }
        }
        for root in [&base.data_dir, &base.config_dir] {
            match std::fs::read_dir(root) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry?;
                        if let Some(name) = entry.file_name().to_str() {
                            if name != self.profile && name.eq_ignore_ascii_case(&self.profile) {
                                anyhow::bail!("profile {:?} collides case-insensitively with {name:?} in {root:?}; existing directories are preserved; choose a distinct name",self.profile);
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => {
                    return Err(error).context("inspect profile name collisions before mutation")
                }
            }
        }
        let roots = [
            absolute_identity(&base.data_dir)?,
            absolute_identity(&base.config_dir)?,
            canonical_identity(&base.data_dir)?,
            canonical_identity(&base.config_dir)?,
        ];
        for path in [&self.data_dir, &self.config_dir] {
            for candidate in [absolute_identity(path)?, canonical_identity(path)?] {
                if roots.iter().any(|root| root.starts_with(&candidate)) {
                    anyhow::bail!("profile {:?} path {path:?} overlaps a default-profile root; choose a different name or nonoverlapping roots; files are preserved",self.profile);
                }
            }
            let expected =
                canonical_identity(path.parent().context("profile directory has no parent")?)?
                    .join(&self.profile);
            if canonical_identity(path)? != absolute_identity(&expected)? {
                anyhow::bail!(
                    "profile path {path:?} aliases another namespace; files are preserved"
                );
            }
        }
        Ok(())
    }

    /// Preflight before any service/secret preparation, then recheck under freeze.
    pub fn validate_cleanup_namespace(&self) -> Result<()> {
        self.validate_profile_namespace()?;
        if self.profile == "default" {
            let diagnostics = self.profile_inventory().diagnostics;
            if !diagnostics.is_empty() {
                anyhow::bail!("profile cleanup refused: {}", diagnostics.join("; "));
            }
        }
        Ok(())
    }

    pub fn from_dirs(data_dir: PathBuf, config_dir: PathBuf) -> Result<Self> {
        let paths = Self {
            data_dir,
            config_dir,
            profile: "default".into(),
        };
        let _lease = crate::profile_lock::ProfileLock::hierarchy_shared(&paths)?;
        std::fs::create_dir_all(&paths.data_dir)?;
        std::fs::create_dir_all(&paths.config_dir)?;
        Ok(paths)
    }

    /// The Session the last `formless run` used (`run --continue`).
    pub fn last_run_session_file(&self) -> PathBuf {
        self.data_dir.join("last_run_session")
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("formless.db")
    }
    pub fn key_fallback_file(&self) -> PathBuf {
        self.config_dir.join("api_key")
    }
    pub fn relay_secret_file(&self) -> PathBuf {
        self.config_dir.join("relay_secret")
    }
    pub fn oauth_tokens_file(&self) -> PathBuf {
        self.config_dir.join("oauth_tokens.json")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.data_dir.join("daemon.lock")
    }
    pub fn log_file(&self) -> PathBuf {
        self.data_dir.join("daemon.log")
    }

    /// `<data_dir>/daemon.sock`, unless that is too long for a unix
    /// socket address (104 bytes on macOS, 108 on Linux, counting the
    /// final NUL): then a short path every client computes the same way,
    /// `<runtime dir>/<hash of data_dir>.sock` (see [`Self::socket_dir`]).
    #[cfg(unix)]
    pub fn socket_path(&self) -> PathBuf {
        let local = self.data_dir.join("daemon.sock");
        if local.as_os_str().len() < SUN_PATH_MAX {
            return local;
        }
        use sha2::Digest;
        let digest = sha2::Sha256::digest(self.data_dir.as_os_str().as_encoded_bytes());
        let hash: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        Self::socket_dir().join(format!("{hash}.sock"))
    }

    /// Where long-path sockets live: `$XDG_RUNTIME_DIR/formless` when that
    /// is short enough, else `/tmp/formless-<user>`.
    #[cfg(unix)]
    pub fn socket_dir() -> PathBuf {
        if let Some(run) = std::env::var_os("XDG_RUNTIME_DIR").filter(|r| !r.is_empty()) {
            let dir = PathBuf::from(run).join("formless");
            // Room for "/<16 hex>.sock".
            if dir.as_os_str().len() + 22 < SUN_PATH_MAX {
                return dir;
            }
        }
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        let user: String = user
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(32)
            .collect();
        PathBuf::from(format!("/tmp/formless-{user}"))
    }

    /// Make sure the socket's directory exists, belongs to this user and
    /// is private (mode 0700): a shared `/tmp` folder someone else made
    /// first must not receive the daemon's socket.
    #[cfg(unix)]
    pub fn prepare_socket_dir(&self) -> Result<()> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let sock = self.socket_path();
        let Some(dir) = sock.parent() else {
            return Ok(());
        };
        if dir == self.data_dir {
            return Ok(());
        }
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("create {}", dir.display()))?;
        }
        let meta = std::fs::symlink_metadata(dir)?;
        let mine = std::fs::metadata(&self.data_dir)
            .map(|m| m.uid())
            .unwrap_or_else(|_| meta.uid());
        if !meta.is_dir() || meta.uid() != mine {
            anyhow::bail!(
                "{} is not a folder owned by you; the data dir path is too long for a unix \
                 socket ({} bytes), so the socket goes there. Remove it, or use a shorter \
                 FORMLESS_DATA_DIR",
                dir.display(),
                self.data_dir.join("daemon.sock").as_os_str().len()
            );
        }
        if meta.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    pub fn pipe_name(&self) -> String {
        self.pipe_name_for()
    }

    /// The daemon's named pipe on Windows: the profile plus a hash of the
    /// data dir, so two installs (or test harnesses) with the same profile
    /// but different data dirs never share a daemon, like the Unix socket
    /// that lives in the data dir (#830).
    pub fn pipe_name_for(&self) -> String {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(self.data_dir.as_os_str().as_encoded_bytes());
        let hash: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
        format!(r"\\.\pipe\formless-{}-{hash}", self.profile)
    }
}

#[cfg(test)]
mod pipe_tests {
    use super::*;

    #[test]
    fn pipe_names_follow_the_data_dir() {
        let p = |data: &str| Paths {
            data_dir: data.into(),
            config_dir: "/c".into(),
            profile: "default".into(),
        };
        assert_eq!(p("/a/data").pipe_name_for(), p("/a/data").pipe_name_for());
        assert_ne!(p("/a/data").pipe_name_for(), p("/b/data").pipe_name_for());
        assert!(p("/a/data")
            .pipe_name_for()
            .starts_with(r"\\.\pipe\formless-default-"));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_long_data_dir_gets_a_short_socket_path() {
        let short = Paths::from_dirs("/tmp/fl-s/d".into(), "/tmp/fl-s/c".into()).unwrap();
        assert_eq!(
            short.socket_path(),
            PathBuf::from("/tmp/fl-s/d/daemon.sock")
        );
        let deep = PathBuf::from(format!("/tmp/fl-long-{}/d", "x".repeat(120)));
        let long = Paths {
            data_dir: deep.clone(),
            config_dir: deep.clone(),
            profile: "default".into(),
        };
        let sock = long.socket_path();
        assert!(sock.as_os_str().len() < SUN_PATH_MAX, "{}", sock.display());
        assert_eq!(sock, long.clone().socket_path(), "every client agrees");
        let other = Paths {
            data_dir: deep.join("other"),
            ..long
        };
        assert_ne!(sock, other.socket_path(), "one socket per data dir");
    }
}

#[cfg(test)]
#[path = "profile_namespace_tests.rs"]
mod profile_namespace_tests;
