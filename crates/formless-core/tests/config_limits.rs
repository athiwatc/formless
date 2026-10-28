//! Former hidden constants are settings (SPEC §13).

use formless_core::{Config, Paths, Store};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn nesting_and_sleep_limits_come_from_config() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-lim-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-lim-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.tools.max_tool_depth = 2;
    cfg.tools.max_sleep_secs = 1;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let mk = |name: &str, js: &str| {
        serde_json::json!({"name": name, "description": "d", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": js, "capabilities": []})
    };
    daemon
        .call_tool_direct(
            &sid,
            "create_tool",
            mk(
                "recurse",
                "function run(i){ return tools.call('recurse', {}); }",
            ),
        )
        .await;
    let r = daemon
        .call_tool_direct(&sid, "recurse", serde_json::json!({}))
        .await;
    assert!(r.contains("tools.max_tool_depth (2)"), "{r}");

    daemon
        .call_tool_direct(
            &sid,
            "create_tool",
            mk(
                "napper",
                "async function run(i){ await sleep(5000); return {}; }",
            ),
        )
        .await;
    let started = std::time::Instant::now();
    let r = daemon
        .call_tool_direct(&sid, "napper", serde_json::json!({}))
        .await;
    assert!(!r.starts_with("error"), "{r}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "sleep capped at 1s"
    );
}

#[test]
fn yes_no_env_vars_accept_the_usual_spellings_and_reject_others() {
    use formless_core::config::env_flag;
    let var = "FORMLESS_TEST_FLAG_7531";
    for (v, want) in [("1", true), ("TRUE", true), ("yes", true), ("on", true)] {
        std::env::set_var(var, v);
        assert_eq!(env_flag(var).unwrap(), Some(want), "{v}");
    }
    for v in ["0", "false", "No", "off"] {
        std::env::set_var(var, v);
        assert_eq!(env_flag(var).unwrap(), Some(false), "{v}");
    }
    std::env::set_var(var, "");
    assert_eq!(env_flag(var).unwrap(), None);
    std::env::set_var(var, "maybe");
    assert!(env_flag(var).is_err());
    std::env::remove_var(var);
    assert_eq!(env_flag(var).unwrap(), None);
}

#[test]
fn trusted_folders_match_by_components_on_every_os_shape() {
    let sec = formless_core::config::SecurityCfg {
        trusted_paths: vec![
            "/home/me/code/app/".into(),
            r"C:\Code\App".into(),
            r"\\server\share\proj".into(),
            "relative/dir".into(),
            "/".into(),
            r"C:\".into(),
            "/home/me/../etc".into(),
        ],
        ..Default::default()
    };
    let (ok, dropped) = sec.roots_checked();
    assert_eq!(ok.len(), 3, "{ok:?}");
    assert_eq!(dropped.len(), 4, "{dropped:?}");
    assert!(sec.path_trusted("/home/me/code/app/src/main.rs"));
    assert!(!sec.path_trusted("/home/me/code/app-secrets/x"));
    assert!(!sec.path_trusted("/home/me/code/app/../../.ssh/id"));
    assert!(
        sec.path_trusted(r"c:\code\app\out.txt"),
        "drive paths ignore case"
    );
    assert!(
        sec.path_trusted("C:/Code/App/sub/out.txt"),
        "either separator"
    );
    assert!(!sec.path_trusted(r"C:\Code\Apple\x"));
    assert!(sec.path_trusted(r"\\server\share\proj\a.txt"));
    assert!(!sec.path_trusted(r"\\server\share\other"));
}
