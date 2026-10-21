//! read_metrics (SPEC §8.9): the agent reads registry-wide health.

use formless_core::{Paths, Store};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn agent_reads_tools_audit_models_and_runs() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-rm-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-rm-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let mk = |name: &str| {
        serde_json::json!({"name": name, "description": "d", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": "function run(i){ if (i.fail) throw new Error('x'); return {}; }", "capabilities": []})
    };
    for n in ["used_tool", "idle_tool"] {
        daemon.call_tool_direct(&sid, "create_tool", mk(n)).await;
    }
    for fail in [false, true, true] {
        daemon
            .call_tool_direct(&sid, "used_tool", serde_json::json!({"fail": fail}))
            .await;
    }
    store.audit("user", "tool.retire", "old_tool").unwrap();
    store.audit("user", "proposal.applied", "guidance").unwrap();
    let rm = |args: serde_json::Value| {
        let (d, s) = (daemon.clone(), sid.clone());
        async move {
            let out = d.call_tool_direct(&s, "read_metrics", args).await;
            serde_json::from_str::<serde_json::Value>(&out).unwrap_or_else(|_| panic!("{out}"))
        }
    };
    let all = rm(serde_json::json!({"kind": "tools"})).await;
    assert_eq!(all["total"], 2);
    assert_eq!(all["tools"][0]["name"], "used_tool", "most used first");
    let unused = rm(serde_json::json!({"kind": "tools", "filter": "unused"})).await;
    assert_eq!(unused["tools"][0]["name"], "idle_tool");
    assert_eq!(unused["total"], 1);
    let failing = rm(serde_json::json!({"kind": "tools", "filter": "failing"})).await;
    assert_eq!(failing["tools"][0]["name"], "used_tool");
    let audit = rm(serde_json::json!({"kind": "audit", "filter": "tool."})).await;
    let actions: Vec<&str> = audit["audit"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["action"].as_str())
        .collect();
    assert!(
        actions.contains(&"tool.retire") && !actions.contains(&"proposal.applied"),
        "{actions:?}"
    );
    assert!(rm(serde_json::json!({"kind": "models"})).await["models"].is_array());
    assert!(rm(serde_json::json!({"kind": "runs"})).await["summary"].is_object());
    assert!(rm(serde_json::json!({"kind": "builtins"})).await["builtins"].is_array());
    let bad = daemon
        .call_tool_direct(&sid, "read_metrics", serde_json::json!({"kind": "secrets"}))
        .await;
    assert!(bad.starts_with("error:"), "{bad}");
}
