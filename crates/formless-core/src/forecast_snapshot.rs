//! Forecast snapshots never open a SQLite connection to the source.
//!
//! A Formless maintenance freeze excludes its cooperating writers. In addition,
//! the native default SQLite VFS must grant the main-file EXCLUSIVE lock, which
//! excludes ordinary external SQLite WAL writers, checkpoints and recovery. No
//! source pager or shared-memory mapping is opened. Unknown/custom/nolock VFSes
//! are refused. Raw writers or programs deliberately bypassing SQLite's locks
//! are unsupported; hashes/quick_check cannot prove coherence against them.
use crate::{profile_lock::ProfileLock, Paths, Store};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::ffi;
use std::{
    ffi::{CStr, CString},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(any(target_os = "macos", test))]
#[path = "forecast_snapshot_macos.rs"]
mod macos;

pub(super) struct SnapshotDir(PathBuf, Option<crate::profile_lock::MaintenanceGuard>);
impl SnapshotDir {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("formless-forecast-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path, None))
    }
}
impl Drop for SnapshotDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Owns SQLite-allocated/aligned VFS storage and its filename until xClose.
/// No Connection, pager, xShmMap, xWrite, xTruncate or xSync is used on source.
struct NativeLock {
    file: *mut ffi::sqlite3_file,
    name: ffi::sqlite3_filename,
    locked: bool,
}
impl NativeLock {
    fn acquire(source: &Path) -> Result<Self> {
        let path = CString::new(
            source
                .to_str()
                .context("forecast database path must be UTF-8")?,
        )?;
        let journal = CString::new(format!("{}-journal", source.display()))?;
        let wal = CString::new(format!("{}-wal", source.display()))?;
        // SAFETY: SQLite initialization/VFS lookup have no source-file access.
        // The default VFS is process-global and remains registered by SQLite.
        unsafe {
            check(ffi::sqlite3_initialize(), "initialize SQLite VFS")?;
            let vfs = ffi::sqlite3_vfs_find(std::ptr::null());
            if vfs.is_null() || (*vfs).zName.is_null() {
                bail!("forecast has no native SQLite VFS");
            }
            let name = CStr::from_ptr((*vfs).zName).to_bytes();
            if !matches!(name, b"unix" | b"win32") {
                bail!("forecast refuses unsupported/custom SQLite VFS");
            }
            #[cfg(target_os = "macos")]
            let (vfs, probe_posix) = {
                if name != b"unix" {
                    bail!("forecast refuses a non-native macOS VFS");
                }
                (macos::source_vfs(vfs, path.as_c_str())?, true)
            };
            #[cfg(not(target_os = "macos"))]
            let (vfs, probe_posix) = {
                if name == b"unix"
                    && ffi::sqlite3_compileoption_used(c"ENABLE_LOCKING_STYLE=1".as_ptr()) != 0
                {
                    bail!("forecast cannot prove write-free locking with automatic Unix locking styles");
                }
                (vfs, false)
            };
            if (*vfs).szOsFile < std::mem::size_of::<ffi::sqlite3_file>() as i32 {
                bail!("invalid native SQLite VFS file size");
            }
            let x_open = (*vfs).xOpen.context("native SQLite VFS has no xOpen")?;
            let filename = ffi::sqlite3_create_filename(
                path.as_ptr(),
                journal.as_ptr(),
                wal.as_ptr(),
                0,
                std::ptr::null_mut(),
            );
            if filename.is_null() {
                bail!("allocate SQLite VFS filename");
            }
            let allocation =
                ffi::sqlite3_malloc64((*vfs).szOsFile as u64) as *mut ffi::sqlite3_file;
            if allocation.is_null() {
                ffi::sqlite3_free_filename(filename);
                bail!("allocate SQLite VFS file");
            }
            // sqlite3_malloc64 guarantees SQLite's required alignment; zeroed
            // pMethods makes cleanup safe even when xOpen only partially fails.
            std::ptr::write_bytes(allocation.cast::<u8>(), 0, (*vfs).szOsFile as usize);
            let mut guard = Self {
                file: allocation,
                name: filename,
                locked: false,
            };
            let mut flags = 0;
            check(
                x_open(
                    vfs,
                    filename,
                    allocation,
                    ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_MAIN_DB,
                    &mut flags,
                ),
                "open forecast native lock (no CREATE)",
            )?;
            if flags & ffi::SQLITE_OPEN_READWRITE == 0 {
                bail!("forecast cannot lock a read-only database without changing permissions");
            }
            let methods = guard.methods()?;
            if probe_posix {
                // Before taking SHARED, posix unixCheckReservedLock performs
                // F_GETLK when no in-process reservation is cached. A writer
                // reservation is refused. This is the same syscall support
                // Apple's APFS auto-finder uses to choose posixIoMethods;
                // HFS/UFS select POSIX directly. No pager/SHM is involved.
                let probe = methods
                    .xCheckReservedLock
                    .context("POSIX VFS has no reserved-lock probe")?;
                let mut reserved = 0;
                check(
                    probe(allocation, &mut reserved),
                    "probe macOS source POSIX F_GETLK",
                )?;
                if reserved != 0 {
                    bail!("forecast source is in use; cannot exclude SQLite writers");
                }
            }
            let lock = methods.xLock.context("native SQLite VFS has no locking")?;
            check(
                lock(allocation, ffi::SQLITE_LOCK_SHARED),
                "claim forecast source SHARED lock",
            )?;
            guard.locked = true;
            check(
                lock(allocation, ffi::SQLITE_LOCK_EXCLUSIVE),
                "forecast source is in use; cannot exclude SQLite writers",
            )?;
            Ok(guard)
        }
    }
    unsafe fn methods(&self) -> Result<&ffi::sqlite3_io_methods> {
        // SAFETY: file is our live SQLite-allocated VFS object; xOpen sets
        // pMethods on success, and it remains valid until our xClose.
        unsafe {
            (*self.file)
                .pMethods
                .as_ref()
                .context("native SQLite VFS file is not open")
        }
    }
    fn copy_main(&self, destination: &Path) -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(destination)?;
        // Do not std::fs::open/read the main source here. On POSIX, closing
        // *any* same-process descriptor for its inode releases fcntl locks.
        // Read through the still-locked native VFS handle instead.
        unsafe {
            let methods = self.methods()?;
            let size = methods
                .xFileSize
                .context("native SQLite VFS has no file size")?;
            let read = methods.xRead.context("native SQLite VFS has no read")?;
            let mut bytes = 0i64;
            check(size(self.file, &mut bytes), "read forecast source size")?;
            if bytes < 0 {
                bail!("invalid forecast database size");
            }
            let mut offset = 0i64;
            let mut buffer = [0u8; 65536];
            while offset < bytes {
                let count = (bytes - offset).min(buffer.len() as i64) as usize;
                check(
                    read(self.file, buffer.as_mut_ptr().cast(), count as i32, offset),
                    "read locked forecast database",
                )?;
                output.write_all(&buffer[..count])?;
                offset += count as i64;
            }
        }
        output.sync_all()?;
        Ok(())
    }
}
impl Drop for NativeLock {
    fn drop(&mut self) {
        // SAFETY: only this owner accesses/closes/frees these pointers. xOpen
        // failure may still supply pMethods, which requires xClose. Never call
        // any source method after xClose; free filename last as required by VFS.
        unsafe {
            if let Some(methods) = (*self.file).pMethods.as_ref() {
                if self.locked {
                    if let Some(unlock) = methods.xUnlock {
                        let _ = unlock(self.file, ffi::SQLITE_LOCK_NONE);
                    }
                }
                if let Some(close) = methods.xClose {
                    let _ = close(self.file);
                }
            }
            ffi::sqlite3_free(self.file.cast());
            ffi::sqlite3_free_filename(self.name);
        }
    }
}
fn check(code: i32, action: &str) -> Result<()> {
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(anyhow!("{action}: SQLite VFS error {code}"))
    }
}

pub(super) fn open(paths: &Paths) -> Result<Option<Store>> {
    open_file(paths, &paths.db_file())
}

pub(super) fn open_file(paths: &Paths, file: &Path) -> Result<Option<Store>> {
    let source = file;
    match std::fs::metadata(&source) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect forecast database"),
        Ok(metadata) if !metadata.is_file() => bail!("forecast database is not a regular file"),
        Ok(_) => (),
    }
    let frozen = ProfileLock::maintenance(paths, Duration::ZERO)?.freeze()?;
    if !frozen.covers(paths)? {
        bail!("forecast source namespace changed");
    }
    let source = std::fs::canonicalize(source)?;
    let native = NativeLock::acquire(&source)?;
    // A raw lock deliberately performs no pager recovery. A possibly hot
    // rollback journal could describe uncommitted changes in the main DB.
    // Refuse even a cold nonempty PERSIST journal rather than bless a copy
    // without knowing its recovery state; never replay it against the source.
    match std::fs::metadata(PathBuf::from(format!("{}-journal",source.display()))) {
        Ok(metadata) if metadata.is_file() && metadata.len()==0=>(),
        Ok(_)=>bail!("forecast source has a rollback journal; normal SQLite recovery is required before an offline snapshot"),
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>(),
        Err(error)=>return Err(error).context("inspect forecast rollback journal"),
    }
    let mut temporary = SnapshotDir::new()?;
    let copy = temporary.0.join("formless.db");
    native.copy_main(&copy)?;
    let wal = PathBuf::from(format!("{}-wal", source.display()));
    match std::fs::metadata(&wal) {
        Ok(metadata) if metadata.is_file() => {
            let mut input = std::fs::File::open(&wal)?;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut output = options.open(temporary.0.join("formless.db-wal"))?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
        }
        Ok(_) => bail!("forecast source WAL is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error).context("read forecast source WAL"),
    }
    // Only the private pager may rebuild SHM, replay WAL, migrate or optimize.
    // The source is still exclusively locked until its coherent copy is open.
    let mut store = Store::open(&copy)?;
    store.check_integrity(&copy)?;
    store.conn.execute_batch("PRAGMA query_only=ON;")?;
    // Keep configuration and data namespaces frozen until the caller finishes
    // reading Config and quoting this coherent private database.
    temporary.1 = Some(frozen);
    store._forecast_snapshot = Some(temporary);
    drop(native);
    Ok(Some(store))
}

#[cfg(test)]
#[path = "forecast_snapshot_tests.rs"]
mod tests;
