//! Running a Tool by hand (SPEC §10.4).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_runs_by_hand_and_is_recorded() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-runtool-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-runtool-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let (tool, _) = store
        .create_tool_version(
            "shout",
            "Upper-case some text",
            r#"{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}"#,
            "{}",
            "function run(input) { return { loud: input.text.toUpperCase() }; }",
            &caps,
            64,
            "agent",
        )
        .unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let r = daemon
        .handle(Request::RunTool {
            id: tool.id.clone(),
            input: serde_json::json!({"text": "hi"}),
            session_id: sid.clone(),
        })
        .await
        .unwrap();
    let result = r["result"].as_str().unwrap();
    assert!(result.contains("HI"), "{result}");
    // It counts as an Invocation of the Tool, in that Session.
    let d = store.tool_detail(&tool.id).unwrap().unwrap();
    assert_eq!(d.recent_calls.len(), 1);
    assert_eq!(d.recent_calls[0].session_id.as_deref(), Some(sid.as_str()));
    assert!(daemon
        .handle(Request::RunTool {
            id: tool.id.clone(),
            input: serde_json::json!(["not", "an", "object"]),
            session_id: sid.clone(),
        })
        .await
        .is_err());
    // A new description and input schema: a new version by the user,
    // with the same code.
    let r = daemon
        .handle(Request::UpdateToolSpec {
            id: tool.id.clone(),
            description: Some("Shout text back in capitals".into()),
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {"text": {"type": "string", "description": "what to shout"}},
                "required": ["text"]
            })),
            output_schema: None,
        })
        .await
        .unwrap();
    assert_eq!(r["version"], 2);
    let d = store.tool_detail(&tool.id).unwrap().unwrap();
    assert_eq!(d.tool.description, "Shout text back in capitals");
    let v2 = d.versions.last().unwrap();
    assert_eq!(v2.author, "user");
    assert!(v2.input_schema.contains("what to shout"));
    assert_eq!(v2.code, d.versions[0].code);
    assert!(daemon
        .handle(Request::UpdateToolSpec {
            id: tool.id.clone(),
            description: Some("  ".into()),
            input_schema: None,
            output_schema: None,
        })
        .await
        .is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
