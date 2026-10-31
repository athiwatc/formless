//! Config edit serialization and truthful approval-bypass audit records.
#[path = "support/env.rs"]
mod credential_env;

use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use serde_json::json;
use std::sync::Arc;

struct Fixture(Paths);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.data_dir.parent().unwrap());
    }
}
fn fixture(env: &credential_env::EnvGuard) -> (Fixture, Arc<Daemon>) {
    env.remove("FORMLESS_MODEL");
    env.remove("FORMLESS_SECURE");
    let root = std::env::temp_dir().join(format!("fl-config-audit-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.provider.model = "disk-model".into();
    cfg.mcp.servers.insert(
        "disabled".into(),
        formless_core::config::McpServerCfg {
            disabled: true,
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    (Fixture(paths), daemon)
}
async fn release(_env: &credential_env::EnvGuard, daemon: Arc<Daemon>) {
    let tasks = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while tasks.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture tasks outlived credential isolation");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_concurrent_edit_paths_preserve_each_change() {
    let env = credential_env::EnvGuard::isolated();
    let (f, daemon) = fixture(&env);
    let requests = [
        Request::SetSetting {
            key: "limits.daily_usd".into(),
            value: json!(3.0),
        },
        Request::UpsertProvider {
            name: "other".into(),
            base_url: "https://example.invalid/v1".into(),
            model: Some("other-model".into()),
            api_key: None,
            headers: None,
            query_params: None,
            api_key_header: None,
        },
        Request::UpsertRole {
            name: "reviewer".into(),
            role: json!({"model":"review-model"}),
        },
        Request::SetMcpToolEnabled {
            name: "disabled".into(),
            tool: "write".into(),
            enabled: false,
        },
    ];
    let start = Arc::new(tokio::sync::Barrier::new(requests.len()));
    let tasks: Vec<_> = requests
        .into_iter()
        .map(|request| {
            let (daemon, start) = (daemon.clone(), start.clone());
            tokio::spawn(async move {
                start.wait().await;
                daemon.handle(request).await
            })
        })
        .collect();
    for task in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("mixed config edit did not finish")
            .unwrap()
            .unwrap();
    }
    let saved = Config::load_file_only(&f.0).unwrap();
    assert_eq!(saved.limits.daily_usd, Some(3.0));
    assert_eq!(saved.providers["other"].model, "other-model");
    assert_eq!(saved.roles["reviewer"].model, "review-model");
    assert_eq!(saved.mcp.servers["disabled"].disabled_tools, ["write"]);
    release(&env, daemon).await;
}

#[tokio::test]
async fn failed_edits_leave_file_unchanged_and_release_the_edit_lock() {
    let env = credential_env::EnvGuard::isolated();
    let (f, daemon) = fixture(&env);
    let before = std::fs::read(f.0.config_file()).unwrap();
    assert!(daemon
        .handle(Request::DeleteRole {
            name: "missing".into()
        })
        .await
        .is_err());
    assert!(daemon
        .handle(Request::SetSetting {
            key: "tools.inline_limit".into(),
            value: json!("bad type")
        })
        .await
        .is_err());
    assert_eq!(std::fs::read(f.0.config_file()).unwrap(), before);
    daemon
        .handle(Request::SetSetting {
            key: "tools.inline_limit".into(),
            value: json!(19),
        })
        .await
        .unwrap();
    assert_eq!(Config::load_file_only(&f.0).unwrap().tools.inline_limit, 19);
    release(&env, daemon).await;
}

#[tokio::test]
async fn mcp_edits_do_not_persist_environment_overrides_and_empty_import_does_not_save() {
    let env = credential_env::EnvGuard::isolated();
    let (f, daemon) = fixture(&env);
    env.set("FORMLESS_MODEL", "environment-model");
    daemon
        .handle(Request::SetMcpToolEnabled {
            name: "disabled".into(),
            tool: "write".into(),
            enabled: false,
        })
        .await
        .unwrap();
    assert_eq!(
        Config::load_file_only(&f.0).unwrap().provider.model,
        "disk-model"
    );
    let current = daemon.handle(Request::GetSettings).await.unwrap();
    assert_eq!(current["provider"]["model"], "environment-model");
    let before = std::fs::metadata(f.0.config_file())
        .unwrap()
        .modified()
        .unwrap();
    let bytes = std::fs::read(f.0.config_file()).unwrap();
    daemon
        .handle(Request::ImportAll {
            bundle: json!({"format":"formless.all/1","config":{}}),
        })
        .await
        .unwrap();
    assert_eq!(std::fs::read(f.0.config_file()).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(f.0.config_file())
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    release(&env, daemon).await;
}

#[tokio::test]
async fn failed_tool_start_records_an_allow_decision_and_denial_does_not() {
    let env = credential_env::EnvGuard::isolated();
    let (f, daemon) = fixture(&env);
    let store = Store::open(&f.0.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let caps = Capabilities::parse(&["net:example.com".into()]).unwrap();
    // A damaged/stored Tool can fail parsing before its run body executes.
    store
        .create_tool_version(
            "bad_start",
            "d",
            "{}",
            "{}",
            "function run( {",
            &caps,
            64,
            "agent",
        )
        .unwrap();
    let (_, ok) = daemon
        .call_tool_direct_outcome(&sid, "bad_start", json!({}))
        .await;
    assert!(!ok);
    let rows = store.recent_audit(Some("tool.run_unasked"), 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].2.starts_with("allow decision (approvals off):"));
    assert!(rows[0].2.contains(&format!("session {sid}")));
    store
        .add_denial("net:example.com", "bad_start", "test")
        .unwrap();
    let (error, ok) = daemon
        .call_tool_direct_outcome(&sid, "bad_start", json!({}))
        .await;
    assert!(!ok && error.contains("always denies"), "{error}");
    assert_eq!(
        store
            .recent_audit(Some("tool.run_unasked"), 10)
            .unwrap()
            .len(),
        1
    );
    release(&env, daemon).await;
}

#[tokio::test]
async fn hook_start_failure_has_a_correlated_allow_decision() {
    let env = credential_env::EnvGuard::isolated();
    let (f, daemon) = fixture(&env);
    let mut cfg = Config::load_file_only(&f.0).unwrap();
    cfg.hooks.push(formless_core::config::HookCfg {
        event: "pre_tool".into(),
        tool: Some("find_tools".into()),
        command: vec![f
            .0
            .data_dir
            .join("missing-hook-executable")
            .to_string_lossy()
            .into_owned()],
        ..Default::default()
    });
    cfg.save(&f.0).unwrap();
    let store = Store::open(&f.0.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let (error, ok) = daemon
        .call_tool_direct_outcome(&sid, "find_tools", json!({"query":"x"}))
        .await;
    assert!(!ok && error.contains("hook"), "{error}");
    let rows = store.recent_audit(Some("hook.run_unasked"), 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].2.starts_with("allow decision (approvals off):"));
    assert!(rows[0].2.contains(&format!("session {sid}")));
    assert!(rows[0].2.contains("run direct") && rows[0].2.contains("event pre_tool"));
    release(&env, daemon).await;
}
