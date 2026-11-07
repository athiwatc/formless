//! Daemon log rotation (SPEC §9.4): `daemon.log` is rotated to
//! `daemon.log.1` … `daemon.log.<keep>` once it exceeds `log_max_bytes`.

use crate::Paths;
use std::path::PathBuf;

/// Set (to any value) in a detached daemon's environment by
/// `spawn_daemon`: its stdout/stderr are `daemon.log`, so it may rotate and
/// reopen them. An in-process daemon (tests, `daemon serve` in a terminal)
/// never touches its own stdio.
pub const DAEMON_LOG_ENV: &str = "FORMLESS_DAEMON_LOG";

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

/// Enforce retention even below the live size cap, then rotate if needed.
/// `keep` is bounded to 0..=64; zero discards rotated history.
pub fn rotate_if_needed(paths: &Paths, max_bytes: u64, keep: usize) -> std::io::Result<bool> {
    if keep > MAX_LOG_KEEP {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "daemon.log_keep must be between 0 and 64",
        ));
    }
    for (index, path) in rotated_logs(paths)? {
        if index > keep as u64 {
            remove_if_present(path)?;
        }
    }
    let log = paths.log_file();
    let size = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    if max_bytes == 0 || size <= max_bytes {
        return Ok(false);
    }
    if keep == 0 {
        std::fs::remove_file(&log)?;
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
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())?;
    for fd in [1, 2] {
        // SAFETY: plain syscall on descriptors this process owns.
        if unsafe { dup2(file.as_raw_fd(), fd) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn reopen_stdio(_paths: &Paths) -> std::io::Result<()> {
    Ok(())
}
