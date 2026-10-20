//! API keys must survive across processes: the wizard saves the key in the
//! TUI process, but the daemon (a separate process) is what sends it.
//! Regression test for the keyring mock-store bug: with no platform
//! credential-store feature enabled, `keyring` uses an in-process mock, the
//! save "succeeds", and the daemon then sends requests with no key (401).

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
    std::env::remove_var("FORMLESS_API_KEY");
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
