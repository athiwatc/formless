//! Apple's default `unix` VFS auto-selects locking. Never open the source with
//! that finder: a proxy/dotlock selection can create sidecars before xLock.
//! On proven local APFS/HFS/UFS, use its explicit POSIX sibling instead.
use anyhow::{bail, Result};
use std::ffi::OsStr;

fn check_proxy_policy(preferred: bool, forced: Option<&OsStr>) -> Result<()> {
    if preferred || forced.is_some_and(|value| value != OsStr::new("0")) {
        bail!("forecast cannot prove write-free locking with preferred or forced proxy locking");
    }
    Ok(())
}
fn check_filesystem(name: &[u8], local: bool, read_only: bool) -> Result<()> {
    if !local || read_only || !matches!(name, b"apfs" | b"hfs" | b"ufs") {
        bail!("forecast requires a writable local APFS/HFS/UFS filesystem for macOS POSIX locking");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) unsafe fn source_vfs(
    default: *mut rusqlite::ffi::sqlite3_vfs,
    source: &std::ffi::CStr,
) -> Result<*mut rusqlite::ffi::sqlite3_vfs> {
    use anyhow::{anyhow, Context};
    use rusqlite::ffi;
    // SAFETY: caller initialized SQLite and validated its live default VFS.
    // These queries do not open any source file or change a global option.
    let preferred =
        unsafe { ffi::sqlite3_compileoption_used(c"PREFER_PROXY_LOCKING".as_ptr()) != 0 };
    check_proxy_policy(
        preferred,
        std::env::var_os("SQLITE_FORCE_PROXY_LOCKING").as_deref(),
    )?;
    // Use libc's Darwin ABI rather than duplicating statfs layout, and never
    // open a separate main-DB descriptor: closing one can release fcntl locks
    // belonging to another SQLite connection in this same process.
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: source is NUL-terminated and statfs receives writable storage of
    // the exact Darwin type. Failed calls are never assumed initialized.
    if unsafe { libc::statfs(source.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("inspect macOS forecast filesystem");
    }
    let filesystem = unsafe { filesystem.assume_init() };
    // Bounded decoding: do not assume a filesystem supplied a terminator.
    let name: Vec<u8> = filesystem
        .f_fstypename
        .iter()
        .map(|c| *c as u8)
        .take_while(|c| *c != 0)
        .collect();
    check_filesystem(
        &name,
        filesystem.f_flags & libc::MNT_LOCAL as u32 != 0,
        filesystem.f_flags & libc::MNT_RDONLY as u32 != 0,
    )?;
    // SAFETY: this built-in VFS remains registered for SQLite's lifetime. Its
    // unixOpen is shared with the default VFS, but its finder is posixIoFinder.
    // PREFER_PROXY was rejected above, and no AUTOPROXY open flag is passed.
    let posix = unsafe { ffi::sqlite3_vfs_find(c"unix-posix".as_ptr()) };
    if posix.is_null() {
        return Err(anyhow!("forecast has no explicit macOS POSIX VFS"));
    }
    unsafe {
        if (*posix).zName.is_null()
            || std::ffi::CStr::from_ptr((*posix).zName).to_bytes() != b"unix-posix"
            || (*posix).xOpen.map(|open| open as usize)
                != (*default).xOpen.map(|open| open as usize)
        {
            bail!("forecast refuses an unproven macOS POSIX VFS");
        }
    }
    Ok(posix)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_filesystem_policy_rejects_network_unknown_and_read_only_mounts() {
        for name in [b"apfs".as_slice(), b"hfs", b"ufs"] {
            check_filesystem(name, true, false).unwrap();
            assert!(check_filesystem(name, false, false).is_err());
            assert!(check_filesystem(name, true, true).is_err());
        }
        for name in [
            b"nfs".as_slice(),
            b"smbfs",
            b"afpfs",
            b"webdav",
            b"msdos",
            b"exfat",
            b"unknown",
            b"apfs\0untrusted",
        ] {
            assert!(check_filesystem(name, true, false).is_err());
        }
    }
    #[test]
    fn proxy_preferences_are_rejected_without_mutating_any_environment_or_vfs() {
        check_proxy_policy(false, None).unwrap();
        check_proxy_policy(false, Some(OsStr::new("0"))).unwrap();
        assert!(check_proxy_policy(true, None).is_err());
        assert!(check_proxy_policy(true, Some(OsStr::new("0"))).is_err());
        for forced in ["1", "2", "true", "", "unrecognized"] {
            assert!(check_proxy_policy(false, Some(OsStr::new(forced))).is_err());
        }
    }
}
