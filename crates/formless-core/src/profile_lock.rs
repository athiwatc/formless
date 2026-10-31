//! Kernel ownership for a profile, separate from its diagnostic PID marker.
//! Lock files are persistent and outside profile directories: unlinking a held
//! lock would let another process lock a different inode at the same name.

use crate::Paths;
use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct ProfileLock {
    _file: File,
}

fn directory(create: bool) -> Result<PathBuf> {
    #[cfg(unix)]
    let path = {
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        // SAFETY: getuid takes no arguments and has no memory preconditions.
        let uid = unsafe { getuid() };
        PathBuf::from(format!("/tmp/formless-ownership-{uid}"))
    };
    #[cfg(not(unix))]
    let path = directories::BaseDirs::new()
        .context("resolve the user's ownership directory")?
        .cache_dir()
        .join("formless/ownership");
    if create {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
    }
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(anyhow!(
                "ownership directory is not a real directory: {}",
                path.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            unsafe extern "C" {
                fn getuid() -> u32;
            }
            // SAFETY: getuid has no arguments or memory preconditions.
            if meta.uid() != unsafe { getuid() } || meta.permissions().mode() & 0o077 != 0 {
                return Err(anyhow!(
                    "ownership directory must be private and owned by this user: {}",
                    path.display()
                ));
            }
        }
    }
    Ok(path)
}

fn lock_path(paths: &Paths, launcher: bool, create: bool) -> Result<PathBuf> {
    let raw = paths
        .data_dir
        .to_str()
        .context("profile data path must be UTF-8")?;
    let canonical = formless_runtime::caps::canonical_path(raw).map_err(anyhow::Error::msg)?;
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let key: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(directory(create)?.join(format!(
        "{key}.{}",
        if launcher { "launch" } else { "owner" }
    )))
}

fn open_lock(paths: &Paths, launcher: bool, create: bool) -> Result<Option<File>> {
    let path = lock_path(paths, launcher, create)?;
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(anyhow!(
                "ownership lock is not a regular file: {}",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("open profile lock {}", path.display()))?;
    // Recheck after opening as well. The parent is private; a same-user rename
    // must not make us knowingly accept a symlink or special file.
    let named = std::fs::symlink_metadata(&path)?;
    if !named.is_file() || named.file_type().is_symlink() || !file.metadata()?.is_file() {
        return Err(anyhow!(
            "profile lock changed while opening {}",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
            return Err(anyhow!(
                "profile lock changed while opening {}",
                path.display()
            ));
        }
    }
    Ok(Some(file))
}

impl ProfileLock {
    pub fn try_owner(paths: &Paths) -> Result<Option<Self>> {
        Self::try_kind(paths, false)
    }

    fn try_kind(paths: &Paths, launcher: bool) -> Result<Option<Self>> {
        let file = open_lock(paths, launcher, true)?.expect("create opens a lock file");
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error).context("claim profile kernel lock"),
        }
    }

    pub fn owner(paths: &Paths) -> Result<Self> {
        Self::try_owner(paths)?.ok_or_else(|| {
            anyhow!("profile is owned by a daemon or maintenance operation; stop it and try again")
        })
    }

    pub fn launcher(paths: &Paths, timeout: Duration) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(lock) = Self::try_kind(paths, true)? {
                return Ok(lock);
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "profile launch/maintenance gate did not release within {}s",
                    timeout.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Errors remain errors: unknown ownership must never be treated as idle.
pub fn owner_held(paths: &Paths) -> Result<bool> {
    let Some(file) = open_lock(paths, false, false)? else {
        return Ok(false);
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error).context("inspect profile kernel ownership"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read, Write};
    use std::process::{Child, Command, Stdio};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> (Self, Paths) {
            let root = std::env::temp_dir().join(format!("fl-owner-{}", uuid::Uuid::new_v4()));
            let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
            (Self(root), paths)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn ownership_is_not_a_pid_marker_and_survives_profile_removal() {
        let (_fixture, paths) = Fixture::new();
        std::fs::write(paths.lock_file(), std::process::id().to_string()).unwrap();
        assert!(!owner_held(&paths).unwrap());
        let owner = ProfileLock::owner(&paths).unwrap();
        let persistent = lock_path(&paths, false, false).unwrap();
        assert!(owner_held(&paths).unwrap());
        assert!(ProfileLock::try_owner(&paths).unwrap().is_none());
        std::fs::remove_dir_all(&paths.data_dir).unwrap();
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        assert!(
            ProfileLock::try_owner(&paths).unwrap().is_none(),
            "recreating the profile cannot split ownership into two lock files"
        );
        drop(owner);
        assert!(persistent.is_file(), "lock inodes are never unlinked");
        assert!(!owner_held(&paths).unwrap());
        assert!(ProfileLock::try_owner(&paths).unwrap().is_some());
    }

    #[test]
    fn ownership_is_exclusive_across_processes_and_released_on_exit() {
        let (_fixture, paths) = Fixture::new();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "profile_lock::tests::ownership_child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("FORMLESS_TEST_OWNERSHIP_DATA", &paths.data_dir)
            .env("FORMLESS_TEST_OWNERSHIP_CONFIG", &paths.config_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut child = Process(child);
        let mut output = std::io::BufReader::new(child.0.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "fixture exited before ownership barrier"
            );
            if line.trim() == "OWNER_READY" {
                break;
            }
        }
        assert!(owner_held(&paths).unwrap());
        assert!(ProfileLock::try_owner(&paths).unwrap().is_none());
        // Closing stdin is a barrier, without timing guesses or unrelated signals.
        drop(child.0.stdin.take());
        assert!(child.0.wait().unwrap().success());
        assert!(!owner_held(&paths).unwrap());
        assert!(ProfileLock::try_owner(&paths).unwrap().is_some());
    }

    #[test]
    #[ignore = "private child-process fixture, invoked explicitly by ownership test"]
    fn ownership_child_fixture() {
        let paths = Paths {
            data_dir: std::env::var_os("FORMLESS_TEST_OWNERSHIP_DATA")
                .expect("private fixture data")
                .into(),
            config_dir: std::env::var_os("FORMLESS_TEST_OWNERSHIP_CONFIG")
                .expect("private fixture config")
                .into(),
            profile: "default".into(),
        };
        let _owner = ProfileLock::owner(&paths).unwrap();
        println!("OWNER_READY");
        std::io::stdout().flush().unwrap();
        let mut release = Vec::new();
        std::io::stdin().read_to_end(&mut release).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_lock_fails_closed_without_touching_target() {
        let (_fixture, paths) = Fixture::new();
        let target = paths.config_dir.join("unrelated");
        std::fs::write(&target, "unchanged").unwrap();
        let lock = lock_path(&paths, false, true).unwrap();
        std::os::unix::fs::symlink(&target, &lock).unwrap();
        assert!(ProfileLock::owner(&paths).is_err());
        assert!(owner_held(&paths).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
        std::fs::remove_file(lock).unwrap(); // This test created a symlink, never a held lock inode.
    }
}
