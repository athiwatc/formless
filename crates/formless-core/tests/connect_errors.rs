//! Daemon connection failures explain themselves (SPEC §9.1).
#![cfg(unix)]

use formless_core::daemon::explain_connect_error;
use formless_core::Paths;
#[path = "support/unprivileged.rs"]
mod unprivileged;
use std::os::unix::net::{UnixListener, UnixStream};

/// Under /tmp: socket paths are limited to ~100 bytes.
fn paths() -> (std::path::PathBuf, Paths) {
    let base = std::path::PathBuf::from(format!(
        "/tmp/fl-conn-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    (base, paths)
}

fn connect_error(paths: &Paths) -> String {
    let err = UnixStream::connect(paths.socket_path()).unwrap_err();
    explain_connect_error(paths, &err)
}

#[test]
fn no_daemon_names_the_profile() {
    let (_dir, paths) = paths();
    let msg = connect_error(&paths);
    assert!(
        msg.contains("no daemon is running for profile 'default'"),
        "{msg}"
    );
    assert!(msg.contains("--profile"), "{msg}");
}

#[test]
fn stale_socket_says_so() {
    let (_dir, paths) = paths();
    drop(UnixListener::bind(paths.socket_path()).unwrap());
    let msg = connect_error(&paths);
    assert!(msg.contains("left over from a daemon that exited"), "{msg}");
    assert!(msg.contains("formless daemon start"), "{msg}");
}

#[test]
fn someone_elses_socket_is_permission_denied() {
    unprivileged::run("someone_elses_socket_is_permission_denied", || {
        use std::os::unix::fs::PermissionsExt;
        let (dir, paths) = paths();
        let _listener = UnixListener::bind(paths.socket_path()).unwrap();
        // Positive control: this exact listener/path accepts a connection before
        // removing access. The denial below must come from real socket permissions.
        assert!(UnixStream::connect(paths.socket_path()).is_ok());
        std::fs::set_permissions(paths.socket_path(), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let denied = UnixStream::connect(paths.socket_path()).unwrap_err();
        assert_eq!(denied.kind(), std::io::ErrorKind::PermissionDenied);
        let msg = explain_connect_error(&paths, &denied);
        assert!(msg.contains("permission denied"), "{msg}");
        assert!(msg.contains("owned by uid"), "{msg}");
        // ensure_daemon stops here instead of starting a daemon that cannot bind.
        let err = formless_core::daemon::ensure_daemon(&paths)
            .unwrap_err()
            .to_string();
        assert!(err.contains("belongs to another user"), "{err}");
        drop(_listener);
        std::fs::remove_dir_all(dir).unwrap();
    });
}

#[test]
fn startup_timeout_explains_the_daemon_independent_config_remedy() {
    let (dir, paths) = paths();
    std::fs::write(paths.config_file(), "[daemon]\nstart_timeout_secs = 1\n").unwrap();
    std::fs::write(paths.log_file(), "fixture daemon still starting\n").unwrap();
    // Model another launcher already starting the daemon: hold ownership,
    // but never open a socket or spawn a process against a user's profile.
    let owner = formless_core::profile_lock::ProfileLock::owner(&paths).unwrap();
    let err = formless_core::daemon::ensure_daemon(&paths)
        .unwrap_err()
        .to_string();
    drop(owner);
    std::fs::remove_dir_all(dir).unwrap();
    assert!(
        err.contains("within 1s (daemon.start_timeout_secs)"),
        "{err}"
    );
    assert!(err.contains("formless config edit"), "{err}");
    assert!(
        err.contains("start_timeout_secs = 180 in [daemon]"),
        "{err}"
    );
    assert!(
        err.contains("`formless config set` cannot change this key"),
        "{err}"
    );
    assert!(err.contains("fixture daemon still starting"), "{err}");
    assert!(!err.contains("using the default 60s"), "{err}");
}
