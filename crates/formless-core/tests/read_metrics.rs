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

#[tokio::test(flavor = "multi_thread")]
async fn audit_log_is_listed_filtered_and_paged() {
    use formless_protocol::{AuditEntry, Request};
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-aud-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-aud-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    for (actor, action, detail) in [
        ("user", "tool.approve", "fetch_page"),
        ("agent", "tool.create", "csv_sum v1"),
        ("user", "skill.enable", "pdf"),
        ("user", "tool.retire", "old_csv"),
    ] {
        store.audit(actor, action, detail).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let list = |filter: Option<&str>, limit: usize, before: Option<String>| {
        let d = daemon.clone();
        let filter = filter.map(str::to_string);
        async move {
            let v = d
                .handle(Request::ListAudit {
                    filter,
                    limit: Some(limit),
                    before,
                })
                .await
                .unwrap();
            serde_json::from_value::<Vec<AuditEntry>>(v).unwrap()
        }
    };
    let all = list(None, 100, None).await;
    assert_eq!(all[0].action, "tool.retire", "newest first");
    let csv = list(Some("CSV"), 100, None).await;
    assert_eq!(
        csv.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(),
        ["tool.retire", "tool.create"]
    );
    let first = list(None, 2, None).await;
    let next = list(None, 2, Some(first[1].created_at.clone())).await;
    assert_eq!(
        next.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(),
        ["tool.create", "tool.approve"]
    );
}
