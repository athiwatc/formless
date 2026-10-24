//! Switching a Tool off without retiring it (SPEC §7.2).

use formless_core::tools::{is_builtin, search_tools_bm25, tool_definitions};
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn a_switched_off_tool_is_hidden_and_uncallable_until_switched_on() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-off-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let (id, session) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[]).unwrap();
        let (info, _) = store
            .create_tool_version(
                "echo_text",
                "Echo text back",
                r#"{"type":"object"}"#,
                "{}",
                "function run(i){ return {echo: i.text}; }",
                &caps,
                64,
                "agent",
            )
            .unwrap();
        (info.id, store.create_session(None).unwrap().id)
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let off = daemon
        .handle(Request::SetToolDisabled {
            id: id.clone(),
            disabled: true,
        })
        .await
        .unwrap();
    assert_eq!(off["disabled"], true);
    assert_eq!(off["current_version"], 1, "no new version");
    assert_eq!(off["retired"], false, "not a retirement");
    {
        let store = Store::open(&paths.db_file()).unwrap();
        assert!(search_tools_bm25(&store, "echo text", 5)
            .unwrap()
            .is_empty());
        let fresh = store.create_session(None).unwrap();
        let listed: Vec<String> = tool_definitions(&store, Some(&fresh.id), 40)
            .unwrap()
            .iter()
            .map(|d| d["function"]["name"].as_str().unwrap().to_string())
            .filter(|n| !is_builtin(n))
            .collect();
        assert!(listed.is_empty(), "{listed:?}");
    }
    let err = daemon
        .handle(Request::RunTool {
            id: id.clone(),
            input: serde_json::json!({"text": "hi"}),
            session_id: session.clone(),
        })
        .await
        .map(|v| v.to_string())
        .unwrap_or_else(|e| e.to_string());
    assert!(err.contains("switched off"), "{err}");

    daemon
        .handle(Request::SetToolDisabled {
            id: id.clone(),
            disabled: false,
        })
        .await
        .unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    assert_eq!(search_tools_bm25(&store, "echo text", 5).unwrap().len(), 1);
    let ok = daemon
        .handle(Request::RunTool {
            id,
            input: serde_json::json!({"text": "hi"}),
            session_id: session,
        })
        .await
        .unwrap();
    assert!(ok.to_string().contains("hi"), "{ok}");
    let _ = std::fs::remove_dir_all(dir);
}
