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

/// Rotate when the live log is larger than `max_bytes`; returns whether it
/// did. `keep` = how many rotated files to keep (0 = discard the old log).
pub fn rotate_if_needed(paths: &Paths, max_bytes: u64, keep: usize) -> std::io::Result<bool> {
    let log = paths.log_file();
    let size = std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    if max_bytes == 0 || size <= max_bytes {
        return Ok(false);
    }
    if keep == 0 {
        std::fs::remove_file(&log)?;
        return Ok(true);
    }
    let _ = std::fs::remove_file(rotated(paths, keep));
    for n in (1..keep).rev() {
        let from = rotated(paths, n);
        if from.exists() {
            std::fs::rename(&from, rotated(paths, n + 1))?;
        }
    }
    std::fs::rename(&log, rotated(paths, 1))?;
    Ok(true)
}

/// Bytes used by the live log and its rotated copies.
pub fn log_usage(paths: &Paths) -> u64 {
    std::iter::once(paths.log_file())
        .chain((1..=64).map(|n| rotated(paths, n)))
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
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
