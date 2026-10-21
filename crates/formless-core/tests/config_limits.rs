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
