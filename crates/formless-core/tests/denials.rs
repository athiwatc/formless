//! Remembered denials, the security overview, and the broad-capability
//! notice on create_tool (SPEC §10.3).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

fn setup(tag: &str) -> (Paths, std::path::PathBuf) {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    (paths, dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remembered_denial_refuses_without_asking_and_says_why() {
    let (paths, dir) = setup("deny");
    let (id, session) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&["net:api.example.com".to_string()]).unwrap();
        let (info, _) = store
            .create_tool_version(
                "caller",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                "function run(i){ return {ok: true}; }",
                &caps,
                64,
                "agent",
            )
            .unwrap();
        // Denying the parent domain covers its subdomains.
        store
            .add_denial("net:example.com", "other_tool", "TUI")
            .unwrap();
        (info.id, store.create_session(None).unwrap().id)
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let r = daemon
        .handle(Request::RunTool {
            id,
            input: serde_json::json!({}),
            session_id: session,
        })
        .await
        .unwrap();
    let text = r["result"].as_str().unwrap_or_default().to_string() + &r.to_string();
    assert!(text.contains("always denies net:api.example.com"), "{text}");
    let listed = daemon.handle(Request::ListDenials).await.unwrap();
    assert_eq!(listed[0]["capability"], "net:example.com");
    let status = daemon.handle(Request::SecurityStatus).await.unwrap();
    assert_eq!(status["denials"], 1);
    assert!(formless_protocol::security_report(&status).contains("Always denied: 1"));
    let forgot = daemon
        .handle(Request::ForgetDenial { capability: None })
        .await
        .unwrap();
    assert_eq!(forgot["forgotten"], 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_broad_tool_suggests_least_privilege() {
    let (paths, dir) = setup("broad");
    let sid = Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let out = daemon
        .call_tool_direct(
            &sid,
            "create_tool",
            serde_json::json!({
            "name": "get_page", "description": "Fetch a page",
            "input_schema": {"type": "object", "properties": {"url": {"type": "string"}}},
            "output_schema": {}, "implementation_js": "function run(i){ return 'x'; }",
            "capabilities": ["net"]}),
        )
        .await;
    assert!(
        out.contains("least_privilege") && out.contains("net:$input.url"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
