use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;

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
        std::fs::create_dir_all(&data)?;
        std::fs::create_dir_all(&config)?;
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
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&config_dir)?;
        Ok(Self {
            data_dir,
            config_dir,
            profile: "default".into(),
        })
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

    #[cfg(unix)]
    pub fn socket_path(&self) -> PathBuf {
        self.data_dir.join("daemon.sock")
    }
    #[cfg(windows)]
    pub fn pipe_name(&self) -> String {
        format!(r"\\.\pipe\formless-{}", self.profile)
    }
}
