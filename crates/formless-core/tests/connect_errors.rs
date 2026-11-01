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
