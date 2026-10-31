//! Kernel ownership for a profile, separate from its diagnostic PID marker.
//! Lock files are persistent and outside profile directories: unlinking a held
//! lock would let another process lock a different inode at the same name.

use crate::Paths;
use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct ProfileLock {
    _files: Vec<File>,
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

/// Lexical absolute identity survives unlinking or retargeting a configured
/// symlink. Do not canonicalize even its parent: that namespace can change too.
fn configured_identity(paths: &Paths) -> Result<PathBuf> {
    let absolute = std::path::absolute(&paths.data_dir)?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => (),
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

fn canonical_identity(paths: &Paths) -> Result<PathBuf> {
    let raw = paths
        .data_dir
        .to_str()
        .context("canonical profile ownership requires a UTF-8 data path")?;
    formless_runtime::caps::canonical_path(raw).map_err(anyhow::Error::msg)
}

fn identity_lock_path(dir: &Path, identity: &Path, launcher: bool) -> PathBuf {
    let digest = Sha256::digest(identity.as_os_str().as_encoded_bytes());
    let key: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    dir.join(format!(
        "{key}.{}",
        if launcher { "launch" } else { "owner" }
    ))
}

#[cfg(test)]
fn lock_path(paths: &Paths, launcher: bool, create: bool) -> Result<PathBuf> {
    Ok(identity_lock_path(
        &directory(create)?,
        &configured_identity(paths)?,
        launcher,
    ))
}

fn lock_paths(paths: &Paths, launcher: bool) -> Result<Vec<PathBuf>> {
    let dir = directory(true)?;
    let mut names = vec![
        identity_lock_path(&dir, &configured_identity(paths)?, launcher),
        identity_lock_path(&dir, &canonical_identity(paths)?, launcher),
    ];
    // Partial acquisition is released on any failure. A global ordering keeps
    // aliases from acquiring their shared locks in opposite orders.
    names.sort();
    names.dedup();
    Ok(names)
}

fn open_lock(path: &Path, create: bool) -> Result<Option<File>> {
    match std::fs::symlink_metadata(path) {
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
        .open(path)
        .with_context(|| format!("open profile lock {}", path.display()))?;
    // Recheck after opening as well. The parent is private; a same-user rename
    // must not make us knowingly accept a symlink or special file.
    let named = std::fs::symlink_metadata(path)?;
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
        let mut files = Vec::new();
        for path in lock_paths(paths, launcher)? {
            let file = open_lock(&path, true)?.expect("create opens a lock file");
            match file.try_lock() {
                Ok(()) => files.push(file),
                Err(TryLockError::WouldBlock) => return Ok(None),
                Err(TryLockError::Error(error)) => {
                    return Err(error).context("claim profile kernel locks")
                }
            }
        }
        Ok(Some(Self { _files: files }))
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
    let dir = directory(false)?;
    let mut unknown = None;
    for identity in [configured_identity(paths), canonical_identity(paths)] {
        let probe = identity.and_then(|identity| {
            let path = identity_lock_path(&dir, &identity, false);
            let Some(file) = open_lock(&path, false)? else {
                return Ok(false);
            };
            match file.try_lock() {
                Ok(()) => Ok(false),
                Err(TryLockError::WouldBlock) => Ok(true),
                Err(TryLockError::Error(error)) => {
                    Err(error).context("inspect profile kernel ownership")
                }
            }
        });
        match probe {
            Ok(true) => return Ok(true),
            Ok(false) => (),
            Err(error) => unknown = Some(error),
        }
    }
    match unknown {
        Some(error) => Err(error),
        None => Ok(false),
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

    #[cfg(unix)]
    #[test]
    fn configured_alias_stays_owned_after_unlink_and_retarget_without_constructor_effects() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let (fixture, mut paths) = Fixture::new();
        let real = paths.data_dir.clone();
        let alias = fixture.0.join("alias");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        paths.data_dir = alias.clone();
        let owner = ProfileLock::owner(&paths).unwrap();
        std::fs::remove_dir_all(&alias).unwrap();
        assert!(
            owner_held(&paths).unwrap(),
            "stable configured key is held after unlink"
        );
        assert!(ProfileLock::try_owner(&paths).unwrap().is_none());
        let replacement = fixture.0.join("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(
            replacement.join("formless.db"),
            "untouched replacement database",
        )
        .unwrap();
        std::os::unix::fs::symlink(&replacement, &alias).unwrap();
        assert!(owner_held(&paths).unwrap());
        assert!(
            ProfileLock::try_owner(&paths).unwrap().is_none(),
            "retargeting cannot evade the configured identity"
        );
        let error = crate::daemon::Daemon::new(paths.clone()).err().unwrap();
        assert!(error.to_string().contains("owned"));
        assert_eq!(
            std::fs::read_to_string(replacement.join("formless.db")).unwrap(),
            "untouched replacement database"
        );
        assert!(!paths.config_file().exists());
        drop(owner);
        assert!(ProfileLock::try_owner(&paths).unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn distinct_existing_aliases_share_canonical_ownership_and_launch_gate() {
        let (fixture, paths) = Fixture::new();
        let mut first = paths.clone();
        first.data_dir = fixture.0.join("first");
        let mut second = paths.clone();
        second.data_dir = fixture.0.join("second");
        std::os::unix::fs::symlink(&paths.data_dir, &first.data_dir).unwrap();
        std::os::unix::fs::symlink(&paths.data_dir, &second.data_dir).unwrap();
        let owner = ProfileLock::owner(&first).unwrap();
        assert!(owner_held(&second).unwrap());
        assert!(ProfileLock::try_owner(&second).unwrap().is_none());
        assert!(ProfileLock::try_owner(&paths).unwrap().is_none());
        let launch = ProfileLock::launcher(&first, Duration::ZERO).unwrap();
        assert!(ProfileLock::launcher(&second, Duration::ZERO).is_err());
        drop(launch);
        drop(owner);
        assert!(ProfileLock::try_owner(&second).unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn known_configured_owner_wins_over_unknown_canonical_resolution() {
        let (fixture, paths) = Fixture::new();
        let mut alias = paths.clone();
        alias.data_dir = fixture.0.join("alias");
        std::os::unix::fs::symlink(&paths.data_dir, &alias.data_dir).unwrap();
        let owner = ProfileLock::owner(&alias).unwrap();
        std::fs::remove_file(&alias.data_dir).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("missing-target"), &alias.data_dir).unwrap();
        assert!(
            owner_held(&alias).unwrap(),
            "known raw ownership remains authoritative even when canonical resolution fails"
        );
        assert!(
            ProfileLock::try_owner(&alias).is_err(),
            "canonical uncertainty cannot authorize a partial claim"
        );
        drop(owner);
        assert!(
            owner_held(&alias).is_err(),
            "unknown canonical state must not become idle"
        );
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
