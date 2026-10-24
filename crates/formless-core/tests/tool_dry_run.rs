//! Dry runs: a Tool run by hand without real side effects (SPEC §10.4).
#![cfg(unix)]

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_changes_nothing_and_says_what_it_would_have_done() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-dry-{short}"));
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("out.txt"), "old").unwrap();
    let work = work.canonicalize().unwrap();
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let w = work.display().to_string();
    let (id, session) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[
            format!("fs:read:{w}"),
            format!("fs:write:{w}"),
            "process:touch".into(),
        ])
        .unwrap();
        let code = format!(
            "function run(i){{ fs.write('{w}/out.txt', 'new'); const after = fs.read('{w}/out.txt'); \
             const r = process.run('touch', ['{w}/touched']); return {{after, code: r.code}}; }}"
        );
        let (info, _) = store
            .create_tool_version(
                "dry_demo",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                &code,
                &caps,
                64,
                "agent",
            )
            .unwrap();
        // Approved before, as a user would have: no prompt this time.
        store.set_approval(&info.id, &caps, 64).unwrap();
        (info.id, store.create_session(None).unwrap().id)
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let r = daemon
        .handle(Request::DryRunTool {
            id: id.clone(),
            input: serde_json::json!({}),
            session_id: session,
        })
        .await
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["after"], "new", "the run sees its own writes");
    let effects: Vec<String> = serde_json::from_value(r["effects"].clone()).unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert!(
        effects[0].starts_with("write 3 bytes to ") && effects[0].ends_with("out.txt"),
        "{effects:?}"
    );
    assert_eq!(effects[1], format!("run touch {w}/touched"));
    // Nothing happened on disk, and nothing was recorded.
    assert_eq!(
        std::fs::read_to_string(work.join("out.txt")).unwrap(),
        "old"
    );
    assert!(!work.join("touched").exists());
    let store = Store::open(&paths.db_file()).unwrap();
    assert_eq!(store.tool_metrics(&id, None).unwrap().total, 0);
    let _ = std::fs::remove_dir_all(dir);
}
