//! Replay answers a wrapper's nested MCP calls from recorded responses
//! and never starts a server (SPEC §8.4).

use formless_core::{Config, Paths, Store};
use formless_runtime::{new_overlay, Capabilities};
use serde_json::json;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn replay_serves_recorded_mcp_responses() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-mcprep-{short}"));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let caps = Capabilities::parse(&["mcp:issues".to_string()]).unwrap();
    let (raw, _) = store
        .create_tool_version(
            "mcp__issues__list_issues",
            "List issues",
            r#"{"type":"object"}"#,
            "{}",
            "// MCP tool list_issues",
            &caps,
            64,
            "mcp",
        )
        .unwrap();
    // A live call, as recorded: redacted input, the untrusted-wrapped output.
    let recorded = json!({"source": "mcp:issues", "note": "data",
        "content": [{"title": "a"}, {"title": "b"}]});
    store
        .record_invocation(
            &raw.id,
            1,
            None,
            None,
            Some("parent"),
            "success",
            "",
            &json!({"state": "open"}).to_string(),
            &recorded.to_string(),
            "",
            1,
            100,
        )
        .unwrap();
    drop(store);
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    // The fixed wrapper counts titles; the call matches the recording.
    let fixed = "async function run(i){ const r = await tools.call('mcp__issues__list_issues', {state: 'open'}); return {count: r.content.length}; }";
    let schema = r#"{"type":"object","required":["count"]}"#;
    assert!(
        daemon
            .replay_one(
                "open_count",
                fixed,
                &caps,
                64,
                schema,
                json!({}),
                None,
                0,
                new_overlay()
            )
            .await,
        "recorded MCP response served"
    );
    let out = daemon
        .replay_exec(
            "open_count",
            "async function run(i){ return await tools.call('mcp__issues__list_issues', {state: 'closed'}); }",
            &caps,
            64,
            json!({}),
            "open_count",
            None,
            0,
            new_overlay(),
        )
        .await;
    assert!(
        out.as_ref()
            .is_err_and(|e| e.contains("unrecorded MCP call in replay")),
        "{out:?}"
    );
    let _ = std::fs::remove_dir_all(base);
}
