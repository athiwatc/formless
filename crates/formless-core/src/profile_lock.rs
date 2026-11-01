//! Kernel ownership for a profile, separate from its diagnostic PID marker.
//! Lock files are persistent and outside profile directories: unlinking a held
//! lock would let another process lock a different inode at the same name.

use crate::Paths;
use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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
    configured_path(&paths.data_dir)
}

fn configured_path(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
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
    canonical_path(&paths.data_dir)
}

fn canonical_path(path: &Path) -> Result<PathBuf> {
    let raw = path
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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum LockMode {
    Shared,
    Exclusive,
}

fn root_identities(paths: &Paths) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for path in [&paths.data_dir, &paths.config_dir] {
        roots.push(configured_path(path)?);
        roots.push(canonical_path(path)?);
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

/// Every ancestor participates, so an independently configured default profile
/// inside another profile's subtree cannot bypass its parent's maintenance.
#[cfg(test)]
fn hierarchy_plan(paths: &Paths, exclusive: bool) -> Result<Vec<(PathBuf, LockMode)>> {
    hierarchy_roots(&root_identities(paths)?, exclusive)
}

fn hierarchy_roots(roots: &[PathBuf], exclusive: bool) -> Result<Vec<(PathBuf, LockMode)>> {
    let dir = directory(true)?;
    let mut plan = BTreeMap::new();
    for root in roots {
        for ancestor in root.ancestors() {
            let name = identity_lock_path(&dir, ancestor, false).with_extension("family");
            let mode = if exclusive && ancestor == root.as_path() {
                LockMode::Exclusive
            } else {
                LockMode::Shared
            };
            plan.entry(name)
                .and_modify(|existing| {
                    if mode > *existing {
                        *existing = mode;
                    }
                })
                .or_insert(mode);
        }
    }
    Ok(plan.into_iter().collect())
}

fn try_plan(plan: Vec<(PathBuf, LockMode)>) -> Result<Option<ProfileLock>> {
    let mut files = Vec::new();
    for (path, mode) in plan {
        let file = open_lock(&path, true)?.expect("create opens a lock file");
        let locked = match mode {
            LockMode::Shared => file.try_lock_shared(),
            LockMode::Exclusive => file.try_lock(),
        };
        match locked {
            Ok(()) => files.push(file),
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => {
                return Err(error).context("claim profile kernel locks")
            }
        }
    }
    Ok(Some(ProfileLock { _files: files }))
}

/// Preparation is shared with ordinary operations. Freeze releases that shared
/// lease before claiming the same namespace exclusively; its persistent launch
/// lock stays held throughout, and failed upgrades never authorize deletion.
pub struct MaintenanceGate {
    paths: Paths,
    shared: ProfileLock,
    launch: ProfileLock,
    roots: Vec<PathBuf>,
}

pub struct MaintenanceGuard {
    _owner: ProfileLock,
    _exclusive: ProfileLock,
    _launch: ProfileLock,
    roots: Vec<PathBuf>,
}

impl MaintenanceGate {
    pub fn freeze(self) -> Result<MaintenanceGuard> {
        let Self {
            paths,
            shared,
            launch,
            roots,
        } = self;
        drop(shared);
        if root_identities(&paths)? != roots {
            anyhow::bail!(
                "profile namespace changed during preparation; destructive cleanup was refused"
            );
        }
        let exclusive = try_plan(hierarchy_roots(&roots, true)?)?.ok_or_else(|| anyhow!("profile family is in use; close TUIs and stop named profiles before destructive cleanup"))?;
        let owner = try_plan(
            lock_paths(&paths, false)?
                .into_iter()
                .map(|p| (p, LockMode::Exclusive))
                .collect(),
        )?
        .ok_or_else(|| anyhow!("profile is still owned; destructive cleanup was refused"))?;
        if root_identities(&paths)? != roots {
            anyhow::bail!(
                "profile namespace changed while freezing; destructive cleanup was refused"
            );
        }
        Ok(MaintenanceGuard {
            _owner: owner,
            _exclusive: exclusive,
            _launch: launch,
            roots,
        })
    }
}

impl MaintenanceGuard {
    /// All raw and canonical paths must remain within the frozen namespace.
    /// Retargeting a child symlink outside the protected tree is refused.
    pub fn covers(&self, paths: &Paths) -> Result<bool> {
        Ok(root_identities(paths)?
            .iter()
            .all(|path| self.roots.iter().any(|root| path.starts_with(root))))
    }
}

impl ProfileLock {
    pub fn try_owner(paths: &Paths) -> Result<Option<Self>> {
        Self::try_kind(paths, false)
    }

    fn try_kind(paths: &Paths, launcher: bool) -> Result<Option<Self>> {
        let roots = root_identities(paths)?;
        let mut plan = hierarchy_roots(&roots, false)?;
        plan.extend(
            lock_paths(paths, launcher)?
                .into_iter()
                .map(|p| (p, LockMode::Exclusive)),
        );
        plan.sort_by(|a, b| a.0.cmp(&b.0));
        let claimed = try_plan(plan)?;
        if root_identities(paths)? != roots {
            anyhow::bail!("profile namespace changed while claiming ownership");
        }
        Ok(claimed)
    }

    /// Ordinary operations retain a shared lease through their last write.
    pub fn hierarchy_shared(paths: &Paths) -> Result<Self> {
        Self::shared_hierarchy(paths).map(|(guard, _)| guard)
    }

    fn shared_hierarchy(paths: &Paths) -> Result<(Self, Vec<PathBuf>)> {
        let roots = root_identities(paths)?;
        let shared = try_plan(hierarchy_roots(&roots, false)?)?.ok_or_else(|| {
            anyhow!("profile family is under maintenance; wait for cleanup to finish and retry")
        })?;
        if root_identities(paths)? != roots {
            anyhow::bail!("profile namespace changed while claiming operation lease");
        }
        Ok((shared, roots))
    }

    pub fn maintenance(paths: &Paths, timeout: Duration) -> Result<MaintenanceGate> {
        let deadline = Instant::now() + timeout;
        let launch = loop {
            if let Some(lock) = try_plan(
                lock_paths(paths, true)?
                    .into_iter()
                    .map(|p| (p, LockMode::Exclusive))
                    .collect(),
            )? {
                break lock;
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("profile launch/maintenance gate did not release"));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let (shared, roots) = Self::shared_hierarchy(paths)?;
        Ok(MaintenanceGate {
            paths: paths.clone(),
            shared,
            launch,
            roots,
        })
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

    // A parallel sibling can fork while a short-lived lock/probe is held.
    // CLOEXEC closes the inherited descriptor at exec, not at fork, so an
    // unrelated child can temporarily retain it after our drop. Run exact
    // immediate-release assertions in a single-test child after exec. Its only
    // process creation is the controlled ownership child, fully waited below.
    #[derive(Clone, Copy)]
    enum ImmediateReleaseCase {
        ParentCleanup,
        ParentFreeze,
        #[cfg(unix)]
        Retargeting,
        #[cfg(unix)]
        AliasedParent,
        PartialFreeze,
        #[cfg(unix)]
        ConfiguredAlias,
        #[cfg(unix)]
        DistinctAliases,
        #[cfg(unix)]
        UnknownCanonical,
        Marker,
        CrossProcess,
    }

    impl ImmediateReleaseCase {
        fn name(self) -> &'static str {
            match self {
                Self::ParentCleanup => "ParentCleanup",
                Self::ParentFreeze => "ParentFreeze",
                #[cfg(unix)]
                Self::Retargeting => "Retargeting",
                #[cfg(unix)]
                Self::AliasedParent => "AliasedParent",
                Self::PartialFreeze => "PartialFreeze",
                #[cfg(unix)]
                Self::ConfiguredAlias => "ConfiguredAlias",
                #[cfg(unix)]
                Self::DistinctAliases => "DistinctAliases",
                #[cfg(unix)]
                Self::UnknownCanonical => "UnknownCanonical",
                Self::Marker => "Marker",
                Self::CrossProcess => "CrossProcess",
            }
        }
    }

    fn isolated_immediate_release(case: ImmediateReleaseCase) {
        let mut child = Process(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "profile_lock::tests::immediate_release_child_fixture",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("FORMLESS_TEST_IMMEDIATE_RELEASE_MODE", case.name())
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        // This is the existing private-fixture containment deadline, not a
        // retry/timeout for any lock acquisition or post-drop assertion.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "isolated {} fixture failed: {status}",
                    case.name()
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "isolated {} fixture did not finish within ten seconds",
                case.name()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "private subprocess fixture for exact immediate-release assertions"]
    fn immediate_release_child_fixture() {
        let mode = std::env::var("FORMLESS_TEST_IMMEDIATE_RELEASE_MODE")
            .expect("invoke only through an isolated immediate-release parent");
        match mode.as_str() {
            "ParentCleanup" => parent_cleanup_assertions(),
            "ParentFreeze" => parent_freeze_assertions(),
            #[cfg(unix)]
            "Retargeting" => retargeting_assertions(),
            #[cfg(unix)]
            "AliasedParent" => aliased_parent_assertions(),
            "PartialFreeze" => partial_freeze_assertions(),
            #[cfg(unix)]
            "ConfiguredAlias" => configured_alias_assertions(),
            #[cfg(unix)]
            "DistinctAliases" => distinct_aliases_assertions(),
            #[cfg(unix)]
            "UnknownCanonical" => unknown_canonical_assertions(),
            "Marker" => marker_assertions(),
            "CrossProcess" => cross_process_assertions(),
            _ => panic!("invalid private immediate-release fixture mode: {mode}"),
        }
    }

    #[test]
    fn parent_cleanup_refuses_named_owner_and_named_profiles_without_touching_sentinels() {
        isolated_immediate_release(ImmediateReleaseCase::ParentCleanup);
    }

    fn parent_cleanup_assertions() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let (_fixture, base) = Fixture::new();
        let named = base.named("child");
        let child = Paths::from_dirs(named.data_dir.clone(), named.config_dir.clone()).unwrap();
        std::fs::write(base.db_file(), "default sentinel").unwrap();
        std::fs::write(child.db_file(), "child data sentinel").unwrap();
        std::fs::write(child.config_file(), "child config sentinel").unwrap();
        let owner = ProfileLock::owner(&named).unwrap();
        let error = crate::config::reset_all(&base).unwrap_err();
        assert!(error.to_string().contains("family is in use"));
        assert_eq!(
            std::fs::read_to_string(base.db_file()).unwrap(),
            "default sentinel"
        );
        assert_eq!(
            std::fs::read_to_string(child.db_file()).unwrap(),
            "child data sentinel"
        );
        drop(owner);
        let error = crate::config::reset_all(&base).unwrap_err();
        assert!(error.to_string().contains("profiles rm NAME"));
        assert_eq!(
            std::fs::read_to_string(child.config_file()).unwrap(),
            "child config sentinel"
        );
        assert!(
            ProfileLock::owner(&named).is_ok(),
            "failed parent cleanup rolls back all locks"
        );
    }

    #[test]
    fn parent_freeze_blocks_new_profile_creation_operation_and_constructor_until_release() {
        isolated_immediate_release(ImmediateReleaseCase::ParentFreeze);
    }

    fn parent_freeze_assertions() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let (_fixture, base) = Fixture::new();
        let frozen = ProfileLock::maintenance(&base, Duration::ZERO)
            .unwrap()
            .freeze()
            .unwrap();
        let named = base.named("new-child");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let child_paths = named.clone();
        let child_barrier = barrier.clone();
        let child = std::thread::spawn(move || {
            child_barrier.wait();
            assert!(
                Paths::from_dirs(child_paths.data_dir.clone(), child_paths.config_dir.clone())
                    .is_err()
            );
            assert!(ProfileLock::hierarchy_shared(&child_paths).is_err());
            assert!(ProfileLock::try_owner(&child_paths).unwrap().is_none());
            assert!(crate::daemon::Daemon::new(child_paths.clone()).is_err());
            assert!(!child_paths.data_dir.exists() && !child_paths.config_dir.exists());
        });
        barrier.wait();
        child.join().unwrap();
        drop(frozen);
        assert!(Paths::from_dirs(named.data_dir, named.config_dir).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn retargeting_during_preparation_or_after_freeze_cannot_authorize_an_uncovered_tree() {
        isolated_immediate_release(ImmediateReleaseCase::Retargeting);
    }

    #[cfg(unix)]
    fn retargeting_assertions() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let (fixture, base) = Fixture::new();
        let alias_path = fixture.0.join("alias");
        std::os::unix::fs::symlink(&base.data_dir, &alias_path).unwrap();
        let alias = Paths {
            data_dir: alias_path.clone(),
            ..base.clone()
        };
        let gate = ProfileLock::maintenance(&alias, Duration::ZERO).unwrap();
        let outside = fixture.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("formless.db"), "outside sentinel").unwrap();
        std::fs::remove_file(&alias_path).unwrap();
        std::os::unix::fs::symlink(&outside, &alias_path).unwrap();
        assert!(gate.freeze().is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("formless.db")).unwrap(),
            "outside sentinel"
        );
        let frozen = ProfileLock::maintenance(&base, Duration::ZERO)
            .unwrap()
            .freeze()
            .unwrap();
        let child = base.named("child");
        std::os::unix::fs::symlink(&outside, &child.data_dir).unwrap();
        assert!(!frozen.covers(&child).unwrap());
        assert!(crate::config::reset_all_guarded(&child, &frozen).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("formless.db")).unwrap(),
            "outside sentinel"
        );
    }

    #[cfg(unix)]
    #[test]
    fn aliased_parent_tree_cannot_freeze_a_descendant_owned_through_real_paths() {
        isolated_immediate_release(ImmediateReleaseCase::AliasedParent);
    }

    #[cfg(unix)]
    fn aliased_parent_assertions() {
        let (fixture, base) = Fixture::new();
        let child = base.named("child");
        Paths::from_dirs(child.data_dir.clone(), child.config_dir.clone()).unwrap();
        let owner = ProfileLock::owner(&child).unwrap();
        let data_alias = fixture.0.join("data-alias");
        let config_alias = fixture.0.join("config-alias");
        std::os::unix::fs::symlink(&base.data_dir, &data_alias).unwrap();
        std::os::unix::fs::symlink(&base.config_dir, &config_alias).unwrap();
        let alias = Paths {
            data_dir: data_alias,
            config_dir: config_alias,
            profile: "default".into(),
        };
        assert!(ProfileLock::maintenance(&alias, Duration::ZERO)
            .unwrap()
            .freeze()
            .is_err());
        drop(owner);
        let frozen = ProfileLock::maintenance(&alias, Duration::ZERO)
            .unwrap()
            .freeze()
            .unwrap();
        assert!(ProfileLock::try_owner(&child).unwrap().is_none());
        assert!(frozen.covers(&alias.named("child")).unwrap());
    }

    #[test]
    fn failed_partial_freeze_releases_locks_and_overlapping_roots_do_not_self_deadlock() {
        isolated_immediate_release(ImmediateReleaseCase::PartialFreeze);
    }

    fn partial_freeze_assertions() {
        let (_fixture, base) = Fixture::new();
        let plan = hierarchy_plan(&base, true).unwrap();
        let blocked_path = &plan
            .iter()
            .rev()
            .find(|(_, mode)| *mode == LockMode::Exclusive)
            .unwrap()
            .0;
        let blocked = open_lock(blocked_path, true).unwrap().unwrap();
        blocked.try_lock_shared().unwrap();
        assert!(ProfileLock::maintenance(&base, Duration::ZERO)
            .unwrap()
            .freeze()
            .is_err());
        drop(blocked);
        let frozen = ProfileLock::maintenance(&base, Duration::ZERO)
            .unwrap()
            .freeze()
            .unwrap();
        assert!(frozen.covers(&base).unwrap());
        drop(frozen);
        let nested = Paths {
            config_dir: base.data_dir.join("config"),
            ..base.clone()
        };
        let frozen = ProfileLock::maintenance(&nested, Duration::ZERO)
            .unwrap()
            .freeze()
            .unwrap();
        assert!(frozen.covers(&nested).unwrap());
        let identical = Paths {
            config_dir: base.data_dir.clone(),
            ..base
        };
        drop(frozen);
        assert!(ProfileLock::maintenance(&identical, Duration::ZERO)
            .unwrap()
            .freeze()
            .is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn configured_alias_stays_owned_after_unlink_and_retarget_without_constructor_effects() {
        isolated_immediate_release(ImmediateReleaseCase::ConfiguredAlias);
    }

    #[cfg(unix)]
    fn configured_alias_assertions() {
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
        isolated_immediate_release(ImmediateReleaseCase::DistinctAliases);
    }

    #[cfg(unix)]
    fn distinct_aliases_assertions() {
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
        isolated_immediate_release(ImmediateReleaseCase::UnknownCanonical);
    }

    #[cfg(unix)]
    fn unknown_canonical_assertions() {
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
        isolated_immediate_release(ImmediateReleaseCase::Marker);
    }

    fn marker_assertions() {
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
        isolated_immediate_release(ImmediateReleaseCase::CrossProcess);
    }

    fn cross_process_assertions() {
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
