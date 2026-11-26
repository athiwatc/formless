//! Daemon log rotation (SPEC §9.4): `daemon.log` is rotated to
//! `daemon.log.1` … `daemon.log.<keep>` once it exceeds `log_max_bytes`.

use crate::Paths;
use std::path::PathBuf;

/// Set (to any value) in a detached daemon's environment by
/// `spawn_daemon`: its stdout/stderr are `daemon.log`, so it may rotate and
/// reopen them. An in-process daemon (tests, `daemon serve` in a terminal)
/// never touches its own stdio.
pub const DAEMON_LOG_ENV: &str = "FORMLESS_DAEMON_LOG";

pub(crate) fn open_daemon_log(paths: &Paths) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    options.open(paths.log_file())
}

fn rotated(paths: &Paths, n: usize) -> PathBuf {
    let mut name = paths.log_file().into_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

pub const MAX_LOG_KEEP: usize = 64;

#[path = "log_read.rs"]
mod read;
pub use read::{
    tail_lines, visit_appended_lines, visit_lines, LogReadSummary, LogTail, MAX_LOG_LINE_BYTES,
};

/// Positive numeric rotations, newest first. Only ordinary files are log history;
/// lookalikes, directories, symlinks and special files are not opened.
pub fn rotated_logs(paths: &Paths) -> std::io::Result<Vec<(u64, PathBuf)>> {
    let live = paths.log_file();
    let parent = live.parent().unwrap_or_else(|| std::path::Path::new("."));
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(std::io::Error::new(
                e.kind(),
                format!("list log history in {}: {e}", parent.display()),
            ))
        }
    };
    let prefix = format!("{}.", live.file_name().unwrap().to_string_lossy());
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|s| s.strip_prefix(&prefix)) else {
            continue;
        };
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        // Oversized decimal suffixes are still old history, not invisible
        // files merely because their index does not fit a machine integer.
        let index = suffix.parse::<u64>().unwrap_or(u64::MAX);
        if index > 0 && entry.file_type()?.is_file() {
            files.push((index, entry.path()));
        }
    }
    fn digits(path: &std::path::Path) -> &str {
        path.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .rsplit('.')
            .next()
            .unwrap()
            .trim_start_matches('0')
    }
    files.sort_by(|a, b| {
        let a_digits = digits(&a.1);
        let b_digits = digits(&b.1);
        a_digits
            .len()
            .cmp(&b_digits.len())
            .then_with(|| a_digits.cmp(b_digits))
            .then_with(|| a.1.as_os_str().len().cmp(&b.1.as_os_str().len()))
            .then_with(|| a.1.cmp(&b.1))
    });
    Ok(files)
}

/// The live file followed by numeric rotations, newest first.
pub fn log_files(paths: &Paths) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let live = paths.log_file();
    match std::fs::symlink_metadata(&live) {
        Ok(m) if m.is_file() => files.push(live),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    files.extend(rotated_logs(paths)?.into_iter().map(|(_, p)| p));
    Ok(files)
}

fn remove_if_present(path: PathBuf) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Add the canonical name without replacing any destination created since
/// discovery. Keep both names if identity verification fails; never guess
/// which path is safe to remove after concurrent changes.
fn promote_alias(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    let fail = |message: &str| {
        std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "normalize log history {} to {}: {message}",
                source.display(),
                destination.display()
            ),
        )
    };
    if !std::fs::symlink_metadata(source)?.is_file() {
        return Err(fail("source is no longer a regular file"));
    }
    let original = same_file::Handle::from_path(source)?;
    std::fs::hard_link(source, destination).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "normalize log history {} to {}: {e}",
                source.display(),
                destination.display()
            ),
        )
    })?;
    if !std::fs::symlink_metadata(source)?.is_file()
        || !std::fs::symlink_metadata(destination)?.is_file()
        || same_file::Handle::from_path(source)? != original
        || same_file::Handle::from_path(destination)? != original
    {
        return Err(fail("file identity changed; both names preserved"));
    }
    std::fs::remove_file(source)
}

#[cfg(windows)]
const PENDING_LOG_NAME: &str = "daemon.log.discard-pending";

#[cfg(windows)]
fn discard_live_log(path: &std::path::Path) -> std::io::Result<()> {
    discard_live_log_with(path, delete_log_link_posix)
}

#[cfg(windows)]
fn discard_live_log_with(
    path: &std::path::Path,
    delete_posix: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::{
        ERROR_CALL_NOT_IMPLEMENTED, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let log = std::fs::OpenOptions::new()
        .access_mode(DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    match delete_posix(&log) {
        Ok(()) => Ok(()),
        Err(error)
            if error.raw_os_error().is_some_and(|code| {
                matches!(
                    code as u32,
                    ERROR_INVALID_FUNCTION
                        | ERROR_NOT_SUPPORTED
                        | ERROR_INVALID_PARAMETER
                        | ERROR_CALL_NOT_IMPLEMENTED
                )
            }) =>
        {
            let parent = path.parent().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "log has no parent directory",
                )
            })?;
            move_and_delete_log_link(&log, parent)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn delete_log_link_posix(log: &std::fs::File) -> std::io::Result<()> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfoEx, SetFileInformationByHandle, FILE_DISPOSITION_FLAG_DELETE,
        FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX,
    };

    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: the handle and complete fixed-size input are valid throughout
    // the synchronous call. Close removes this name without touching others.
    if unsafe {
        SetFileInformationByHandle(
            log.as_raw_handle(),
            FileDispositionInfoEx,
            (&disposition as *const FILE_DISPOSITION_INFO_EX).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn move_and_delete_log_link(log: &std::fs::File, parent: &std::path::Path) -> std::io::Result<()> {
    move_and_delete_log_link_with(log, parent, delete_log_link_on_release)
}

#[cfg(windows)]
fn move_and_delete_log_link_with(
    log: &std::fs::File,
    parent: &std::path::Path,
    mark_for_deletion: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::mem::{offset_of, size_of};
    use std::os::windows::{ffi::OsStrExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    // Use a full Win32 name with a null RootDirectory. A handle-relative
    // target was rejected by SetFileInformationByHandle with error 87.
    let pending = std::fs::canonicalize(parent)?.join(PENDING_LOG_NAME);
    match std::fs::symlink_metadata(&pending) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "pending daemon log slot is occupied or inaccessible (see Windows install guide): {}",
                    pending.display()
                ),
            ));
        }
    }
    let name: Vec<u16> = pending.as_os_str().encode_wide().collect();
    // Match the existing Windows namespace rename buffer alignment and include
    // a terminal NUL for compatibility (the declared byte count excludes it).
    let bytes = size_of::<FILE_RENAME_INFO>()
        .max(offset_of!(FILE_RENAME_INFO, FileName) + (name.len() + 1) * 2);
    let mut buffer = vec![0u64; bytes.div_ceil(size_of::<u64>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: the aligned, zeroed, correctly sized allocation and borrowed
    // live-file handle remain valid until the synchronous operation returns.
    if unsafe {
        (*info).Anonymous.ReplaceIfExists = 0;
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
        SetFileInformationByHandle(
            log.as_raw_handle(),
            FileRenameInfo,
            info.cast(),
            (buffer.len() * size_of::<u64>()) as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }

    if let Err(error) = mark_for_deletion(log) {
        // The live name is already free: report rotation so stdio switches to
        // a fresh file. Do not delete this pathname later without proof of
        // ownership, including if the process stops between rename and mark.
        tracing::warn!(
            path = %pending.display(),
            %error,
            "could not mark pending daemon log for deletion; preserved for inspection"
        );
    }
    Ok(())
}

#[cfg(windows)]
fn delete_log_link_on_release(log: &std::fs::File) -> std::io::Result<()> {
    use std::mem::size_of;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: 1 };
    // SAFETY: same opened link as the rename. Traditional disposition removes
    // only that moved name once its old writers close.
    if unsafe {
        SetFileInformationByHandle(
            log.as_raw_handle(),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(windows, test))]
mod windows_rotation_fallback {
    use super::*;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Write};
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use windows_sys::Win32::Foundation::ERROR_NOT_SUPPORTED;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    struct TempLog(PathBuf);

    impl TempLog {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "formless-fallback-{}-{stamp}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn live(&self) -> PathBuf {
            self.0.join("daemon.log")
        }

        fn pending(&self) -> PathBuf {
            self.0.join(PENDING_LOG_NAME)
        }
    }

    impl Drop for TempLog {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn unsupported(_: &File) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(ERROR_NOT_SUPPORTED as i32))
    }

    #[test]
    fn rename_allows_retry_and_release_without_mutating_another_link() {
        let f = TempLog::new();
        let live = f.live();
        let alias = f.0.join("keep.log");
        fs::write(&live, b"old contents").unwrap();
        fs::hard_link(&live, &alias).unwrap();
        let mut writer = OpenOptions::new()
            .append(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&live)
            .unwrap();

        discard_live_log_with(&live, unsupported).unwrap();
        // Model a reopen blocked transiently. Old stdio must remain usable.
        fs::create_dir(&live).unwrap();
        assert!(OpenOptions::new()
            .create(true)
            .append(true)
            .open(&live)
            .is_err());
        writer.write_all(b" and old writer").unwrap();
        writer.flush().unwrap();
        fs::remove_dir(&live).unwrap();
        OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&live)
            .unwrap()
            .write_all(b"new live")
            .unwrap();
        // While the first old writer still pins the pending name, a second
        // rotation must not replace that slot or disturb the fresh live log.
        assert!(discard_live_log_with(&live, unsupported).is_err());
        assert_eq!(fs::read(&live).unwrap(), b"new live");
        writer.write_all(b" after refused rotation").unwrap();
        writer.flush().unwrap();
        drop(writer);

        assert_eq!(
            fs::read(&alias).unwrap(),
            b"old contents and old writer after refused rotation"
        );
        assert_eq!(
            fs::symlink_metadata(f.pending()).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        // The same bounded slot is reusable after the old writer closes.
        let second_writer = OpenOptions::new()
            .append(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&live)
            .unwrap();
        discard_live_log_with(&live, unsupported).unwrap();
        OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&live)
            .unwrap()
            .write_all(b"third live")
            .unwrap();
        drop(second_writer);
        assert_eq!(fs::read(&live).unwrap(), b"third live");
        assert_eq!(
            fs::read(&alias).unwrap(),
            b"old contents and old writer after refused rotation"
        );
        assert_eq!(
            fs::symlink_metadata(f.pending()).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn existing_pending_slot_is_never_replaced_or_deleted() {
        let f = TempLog::new();
        fs::write(f.live(), b"old contents").unwrap();
        fs::write(f.pending(), b"keep the pending data").unwrap();

        let error = discard_live_log_with(&f.live(), unsupported).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(f.live()).unwrap(), b"old contents");
        assert_eq!(fs::read(f.pending()).unwrap(), b"keep the pending data");
    }

    #[test]
    fn deletion_failure_after_move_still_allows_reopen_and_preserves_pending() {
        let f = TempLog::new();
        let live = f.live();
        fs::write(&live, b"old contents").unwrap();
        let handle = OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&live)
            .unwrap();

        move_and_delete_log_link_with(&handle, &f.0, |_| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "forced traditional disposition failure",
            ))
        })
        .unwrap();
        drop(handle);
        fs::write(&live, b"new live").unwrap();
        assert_eq!(fs::read(live).unwrap(), b"new live");
        assert_eq!(fs::read(f.pending()).unwrap(), b"old contents");
    }
}

#[cfg(not(windows))]
fn discard_live_log(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
}

/// Enforce retention even below the live size cap, then rotate if needed.
/// `keep` is bounded to 0..=64; zero discards rotated history.
pub fn rotate_if_needed(paths: &Paths, max_bytes: u64, keep: usize) -> std::io::Result<bool> {
    if keep > MAX_LOG_KEEP {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "daemon.log_keep must be between 0 and 64",
        ));
    }
    // Numeric aliases occupy the same retention slot. Prefer the canonical
    // regular file, otherwise normalize the shortest alias before discarding
    // duplicates. Never replace a directory, symlink or special destination.
    let history = rotated_logs(paths)?;
    for index in 1..=keep {
        let canonical = rotated(paths, index);
        match std::fs::symlink_metadata(&canonical) {
            Ok(m) if !m.is_file() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "cannot normalize log history: {} is not a regular file",
                        canonical.display()
                    ),
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let mut normalized = std::collections::BTreeSet::new();
    for (index, path) in history {
        if index > keep as u64 {
            remove_if_present(path)?;
            continue;
        }
        let canonical = rotated(paths, index as usize);
        if normalized.insert(index) && path != canonical {
            // Canonical sorts first, so this slot has only aliases. Promote
            // before removing any duplicates, preserving them if it fails.
            promote_alias(&path, &canonical)?;
        } else if path != canonical {
            remove_if_present(path)?;
        }
    }
    let log = paths.log_file();
    let size = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    if max_bytes == 0 || size <= max_bytes {
        return Ok(false);
    }
    if keep == 0 {
        discard_live_log(&log)?;
        return Ok(true);
    }
    remove_if_present(rotated(paths, keep))?;
    for n in (1..keep).rev() {
        let from = rotated(paths, n);
        if std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_file()) {
            std::fs::rename(&from, rotated(paths, n + 1))?;
        }
    }
    std::fs::rename(&log, rotated(paths, 1))?;
    Ok(true)
}

/// Bytes used by live and all discovered numeric history, without a suffix cap.
pub fn log_usage(paths: &Paths) -> u64 {
    let live = std::fs::symlink_metadata(paths.log_file())
        .ok()
        .filter(|m| m.is_file())
        .map_or(0, |m| m.len());
    let history = match rotated_logs(paths) {
        Ok(files) => files,
        Err(error) => {
            tracing::warn!(%error, "could not enumerate rotated logs; usage includes live file only");
            return live;
        }
    };
    history
        .iter()
        .filter_map(|(_, p)| std::fs::metadata(p).ok())
        .fold(live, |sum, m| sum.saturating_add(m.len()))
}

/// Point this process's stdout and stderr at a fresh `daemon.log` (after a
/// rotation renamed the file they were writing to).
#[cfg(unix)]
pub fn reopen_stdio(paths: &Paths) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    extern "C" {
        fn dup2(src: i32, dst: i32) -> i32;
    }
    let file = open_daemon_log(paths)?;
    for fd in [1, 2] {
        // SAFETY: plain syscall on descriptors this process owns.
        if unsafe { dup2(file.as_raw_fd(), fd) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(windows)]
pub fn reopen_stdio(paths: &Paths) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::windows::io::{AsRawHandle, IntoRawHandle};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };

    let output = open_daemon_log(paths)?;
    let errors = output.try_clone()?;
    // Keep Rust's buffered logging threads from using a handle while it is
    // being replaced. These locks also preserve pending output in the old log.
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let _ = stdout.flush();
    let _ = stderr.flush();

    // SAFETY: both replacement handles are live files, and the daemon is
    // redirecting only its own stdout and stderr. After installing them,
    // keep them open for the process and retire the replaced handles once.
    unsafe {
        let old_output = GetStdHandle(STD_OUTPUT_HANDLE);
        let old_errors = GetStdHandle(STD_ERROR_HANDLE);
        if SetStdHandle(STD_OUTPUT_HANDLE, output.as_raw_handle()) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if SetStdHandle(STD_ERROR_HANDLE, errors.as_raw_handle()) == 0 {
            let error = std::io::Error::last_os_error();
            if SetStdHandle(STD_OUTPUT_HANDLE, old_output) == 0 {
                // The process still uses this replacement on stdout.
                let _ = output.into_raw_handle();
            }
            return Err(error);
        }
        let _ = output.into_raw_handle();
        let _ = errors.into_raw_handle();
        if !old_output.is_null() && old_output != INVALID_HANDLE_VALUE {
            CloseHandle(old_output);
        }
        if !old_errors.is_null() && old_errors != INVALID_HANDLE_VALUE && old_errors != old_output {
            CloseHandle(old_errors);
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn reopen_stdio(_paths: &Paths) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod alias_promotion_tests {
    use super::promote_alias;

    #[test]
    fn late_canonical_file_is_never_overwritten() {
        let base = std::env::temp_dir().join(format!("fl-alias-race-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let source = base.join("daemon.log.01");
        let destination = base.join("daemon.log.1");
        std::fs::write(&source, "old alias").unwrap();
        // Model the directory snapshot/preflight, then a writer creating .1.
        assert!(std::fs::symlink_metadata(&destination).is_err());
        std::fs::write(&destination, "new canonical").unwrap();
        assert_eq!(
            promote_alias(&source, &destination).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "old alias");
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "new canonical"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn late_canonical_symlink_is_never_replaced() {
        let base = std::env::temp_dir().join(format!("fl-alias-link-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let source = base.join("daemon.log.01");
        let destination = base.join("daemon.log.1");
        let target = base.join("unrelated");
        std::fs::write(&source, "old alias").unwrap();
        std::fs::write(&target, "preserve target").unwrap();
        assert!(std::fs::symlink_metadata(&destination).is_err());
        std::os::unix::fs::symlink(&target, &destination).unwrap();
        assert_eq!(
            promote_alias(&source, &destination).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert!(std::fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "preserve target");
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "old alias");
        std::fs::remove_dir_all(base).unwrap();
    }
}
