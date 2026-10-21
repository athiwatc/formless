//! Creation smoke tests are bounded (SPEC §7.1): an endless loop never
//! hangs create_tool or create_module.

use formless_core::{Config, Paths, Store};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn endless_loops_in_smoke_tests_stop() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-smk-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-smk-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.tools.smoke_timeout_secs = 1;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let started = std::time::Instant::now();
    let out = daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "spinner", "description": "loops", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": "function run(i){ while(true){} }", "capabilities": []}))
        .await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "bounded"
    );
    assert!(out.contains("did not finish within 1s"), "{out}");
    assert!(
        out.contains("\"created\""),
        "a Tool is inconclusive, not refused: {out}"
    );

    let out = daemon
        .call_tool_direct(
            &sid,
            "create_module",
            serde_json::json!({
            "name": "busy", "description": "loops on load",
            "implementation_js": "while(true){} module.exports = {};"}),
        )
        .await;
    assert!(
        out.starts_with("error:") && out.contains("did not finish"),
        "{out}"
    );
}
