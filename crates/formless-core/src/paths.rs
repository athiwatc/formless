use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;

/// Longest unix socket path, NUL included (`sun_path`: 104 bytes on
/// macOS and the BSDs, 108 on Linux).
#[cfg(all(unix, target_os = "linux"))]
const SUN_PATH_MAX: usize = 108;
#[cfg(all(unix, not(target_os = "linux")))]
const SUN_PATH_MAX: usize = 104;

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
        let profile = profile.unwrap_or("default").to_string();
        let (mut data, mut config) = match (
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
        if profile != "default" {
            data = data.join(&profile);
            config = config.join(&profile);
        }
        Ok(Self {
            data_dir: data,
            config_dir: config,
            profile,
        })
    }

    /// Every profile that exists: `default` first, then each named one
    /// (a sub-directory with a database or config), sorted. Named
    /// profiles live inside the default profile's directories.
    pub fn list_profiles(&self) -> Vec<Paths> {
        let base = self.base();
        let mut names = std::collections::BTreeSet::new();
        for dir in [&base.data_dir, &base.config_dir] {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let p = base.named(&name);
                if entry.path().is_dir() && (p.db_file().exists() || p.config_file().exists()) {
                    names.insert(name);
                }
            }
        }
        std::iter::once(base.clone())
            .chain(names.into_iter().map(|n| base.named(&n)))
            .collect()
    }

    /// The default profile's paths.
    fn base(&self) -> Paths {
        if self.profile == "default" {
            return self.clone();
        }
        let up = |p: &PathBuf| p.parent().map(PathBuf::from).unwrap_or_else(|| p.clone());
        Paths {
            data_dir: up(&self.data_dir),
            config_dir: up(&self.config_dir),
            profile: "default".into(),
        }
    }

    /// Another profile's paths, without creating anything.
    pub fn named(&self, profile: &str) -> Paths {
        let base = self.base();
        if profile == "default" {
            return base;
        }
        Paths {
            data_dir: base.data_dir.join(profile),
            config_dir: base.config_dir.join(profile),
            profile: profile.to_string(),
        }
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
