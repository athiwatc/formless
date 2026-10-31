//! API keys must survive across processes: the wizard saves the key in the
//! TUI process, but the daemon (a separate process) is what sends it.
//! Regression test for the keyring mock-store bug: with no platform
//! credential-store feature enabled, `keyring` uses an in-process mock, the
//! save "succeeds", and the daemon then sends requests with no key (401).

#[path = "support/env.rs"]
mod credential_env;

use formless_core::config::{delete_api_key, load_api_key, save_api_key};
use formless_core::Paths;

fn test_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

#[test]
fn api_key_saved_in_one_process_is_loadable_in_another() {
    let _env = credential_env::EnvGuard::isolated();
    _env.remove("FORMLESS_API_KEY");
    let paths = test_paths("keyxproc");

    if std::env::var("FORMLESS_KEY_XPROC_CHILD").is_ok() {
        // Child process: save, then exit. Nothing in-process may be reused.
        save_api_key(&paths_for_child(), "xproc-secret").unwrap();
        return;
    }

    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(exe)
        .args([
            "--exact",
            "api_key_saved_in_one_process_is_loadable_in_another",
            "--test-threads=1",
        ])
        .env("FORMLESS_KEY_XPROC_CHILD", "1")
        .env("FORMLESS_KEY_XPROC_DATA", paths.data_dir.to_str().unwrap())
        .env(
            "FORMLESS_KEY_XPROC_CONFIG",
            paths.config_dir.to_str().unwrap(),
        )
        .env("FORMLESS_KEY_XPROC_PROFILE", &paths.profile)
        .env_remove("FORMLESS_API_KEY")
        .status()
        .unwrap();
    assert!(status.success(), "child save process failed: {status}");

    let loaded = load_api_key(&paths);
    assert!(
        paths.key_fallback_file().exists(),
        "0600 fallback file is always written so rebuilds keep the key"
    );
    delete_api_key(&paths);
    assert_eq!(
        loaded.as_deref(),
        Some("xproc-secret"),
        "key saved by another process must load here (real keychain or fallback file)"
    );
}

fn paths_for_child() -> Paths {
    Paths {
        data_dir: std::env::var("FORMLESS_KEY_XPROC_DATA").unwrap().into(),
        config_dir: std::env::var("FORMLESS_KEY_XPROC_CONFIG").unwrap().into(),
        profile: std::env::var("FORMLESS_KEY_XPROC_PROFILE").unwrap(),
    }
}

#[cfg(unix)]
#[test]
fn secret_files_are_private_from_the_start_and_loose_ones_are_tightened() {
    let _env = credential_env::EnvGuard::isolated();
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!(
        "fl-secret-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let file = dir.join("api_key");
    formless_core::config::write_secret_file(&file, "sk-test").unwrap();
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert!(
        std::fs::read_dir(&dir).unwrap().count() == 1,
        "no temp file left"
    );
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        formless_core::config::read_secret_file(&file).as_deref(),
        Some("sk-test")
    );
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "tightened on read");
    let _ = std::fs::remove_dir_all(dir);
}
