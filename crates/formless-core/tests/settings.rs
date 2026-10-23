//! SetSetting (SPEC §13): validated, allow-listed, written by the daemon.

use formless_core::{Config, Paths};
use formless_protocol::Request;
use std::sync::Arc;

#[test]
fn set_key_validates_types_keys_and_the_allow_list() {
    let mut c = Config::default();
    c.set_key("tools.inline_limit", serde_json::json!(12))
        .unwrap();
    assert_eq!(c.tools.inline_limit, 12);
    c.set_key("provider.reasoning_effort", serde_json::json!("high"))
        .unwrap();
    assert_eq!(c.provider.reasoning_effort.as_deref(), Some("high"));
    c.set_key("provider.reasoning_effort", serde_json::Value::Null)
        .unwrap();
    assert_eq!(c.provider.reasoning_effort, None);
    c.set_key("provider.fallback_models", serde_json::json!(["a", "b"]))
        .unwrap();
    assert_eq!(c.provider.fallback_models, ["a", "b"]);
    assert!(
        c.set_key("tools.inline_limit", serde_json::json!("lots"))
            .is_err(),
        "type checked"
    );
    c.set_key("provider.api_style", serde_json::json!("responses"))
        .unwrap();
    assert_eq!(c.provider.api_style, "responses");
    let err = c
        .set_key("provider.tool_mode", serde_json::json!("magic"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("one of: native, prompt"), "{err}");
    assert!(c
        .set_key("provider.api_style", serde_json::json!(1))
        .is_err());
    assert!(
        c.set_key("tools.inline_limitt", serde_json::json!(3))
            .is_err(),
        "typo caught"
    );
    for blocked in [
        "provider.base_url",
        "provider.extra_headers",
        "relay.enabled",
        "hooks",
        "mcp.servers",
        "provider.auth.mode",
    ] {
        assert!(
            c.set_key(blocked, serde_json::json!("x")).is_err(),
            "{blocked}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_writes_the_file_without_env_overrides() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-set-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-set-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.model = "file-model".into();
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let r = daemon
        .handle(Request::SetSetting {
            key: "tui.multi_pane".into(),
            value: serde_json::json!(true),
        })
        .await
        .unwrap();
    assert_eq!(r["restart_required"], false);
    assert_eq!(r["applies"], "now");
    // A key the TUI only reads when it starts says so.
    let r = daemon
        .handle(Request::SetSetting {
            key: "tui.on_start".into(),
            value: serde_json::json!("new"),
        })
        .await
        .unwrap();
    assert_eq!(r["applies"], "on_tui_start");
    let summary = daemon.handle(Request::GetSettings).await.unwrap();
    assert_eq!(summary["tui"]["on_start"], "new");
    assert!(summary["on_tui_start"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k == "tui.screen_reader"));
    // Spending limits are settable: a decimal, then cleared with null.
    for value in [serde_json::json!(2.5), serde_json::Value::Null] {
        daemon
            .handle(Request::SetSetting {
                key: "limits.daily_usd".into(),
                value: value.clone(),
            })
            .await
            .unwrap();
        let limit = Config::load_file_only(&paths).unwrap().limits.daily_usd;
        assert_eq!(limit, value.as_f64());
    }
    let saved = Config::load_file_only(&paths).unwrap();
    assert!(saved.tui.multi_pane);
    assert_eq!(saved.provider.model, "file-model");
    assert!(daemon
        .handle(Request::SetSetting {
            key: "provider.base_url".into(),
            value: serde_json::json!("http://evil")
        })
        .await
        .is_err());
    assert_eq!(
        daemon.handle(Request::GetSettings).await.unwrap()["tui"]["multi_pane"],
        true
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_grants_show_in_tool_detail_and_revoke_per_item() {
    use formless_runtime::Capabilities;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-grant-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-grant-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();
    let store = formless_core::Store::open(&paths.db_file()).unwrap();
    let caps = Capabilities::parse(&["net:a.example".into(), "net:b.example".into()]).unwrap();
    let (tool, _) = store
        .create_tool_version(
            "fetcher",
            "d",
            "{}",
            "{}",
            "function run(i){return i;}",
            &caps,
            64,
            "agent",
        )
        .unwrap();
    store.set_approval(&tool.id, &caps, 64).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let detail = daemon
        .handle(Request::GetTool {
            id: tool.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        detail["approval"]["capabilities"],
        serde_json::json!(["net:a.example", "net:b.example"])
    );
    daemon
        .handle(Request::RevokeApproval {
            tool_id: tool.id.clone(),
            capability: Some("net:a.example".into()),
        })
        .await
        .unwrap();
    let detail = daemon
        .handle(Request::GetTool {
            id: tool.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        detail["approval"]["capabilities"],
        serde_json::json!(["net:b.example"])
    );
    assert!(daemon
        .handle(Request::RevokeApproval {
            tool_id: tool.id.clone(),
            capability: Some("net:zzz".into())
        })
        .await
        .is_err());
    daemon
        .handle(Request::RevokeApproval {
            tool_id: tool.id.clone(),
            capability: None,
        })
        .await
        .unwrap();
    let detail = daemon
        .handle(Request::GetTool {
            id: tool.id.clone(),
        })
        .await
        .unwrap();
    assert!(detail["approval"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn session_delete_has_an_undo_window() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-undo-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-undo-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.daemon.delete_undo_secs = 1;
    cfg.save(&paths).unwrap();
    let store = formless_core::Store::open(&paths.db_file()).unwrap();
    let id = store.create_session(Some("keep me?".into())).unwrap().id;
    store
        .append_message(&id, "user", "a searchable phrase")
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let listed = |d: Arc<formless_core::daemon::Daemon>| async move {
        d.handle(Request::ListSessions)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len()
    };
    let r = daemon
        .handle(Request::DeleteSession { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(
        (r["deleted"].as_bool(), r["undo_secs"].as_u64()),
        (Some(true), Some(1))
    );
    assert_eq!(listed(daemon.clone()).await, 0, "hidden at once");
    assert!(store.search_sessions("searchable", 10).unwrap().is_empty());
    assert!(daemon
        .handle(Request::GetSession { id: id.clone() })
        .await
        .is_err());
    daemon
        .handle(Request::RestoreSession { id: id.clone() })
        .await
        .unwrap();
    assert_eq!(listed(daemon.clone()).await, 1, "undo brings it back");

    daemon
        .handle(Request::DeleteSession { id: id.clone() })
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
    assert!(
        store.get_session(&id).unwrap().is_none(),
        "gone after the window"
    );
    assert!(daemon.handle(Request::RestoreSession { id }).await.is_err());
}
