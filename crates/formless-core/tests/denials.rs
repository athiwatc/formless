//! Remembered denials, the security overview, and the broad-capability
//! notice on create_tool (SPEC §10.3).

#[path = "support/env.rs"]
mod credential_env;

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::{Arc, Once};

fn setup(_env: &credential_env::EnvGuard, tag: &str) -> (Paths, std::path::PathBuf) {
    // Status resolves credentials even though these tests never call a provider.
    // Keep the whole test process on an in-memory backend, never the user's store.
    static MOCK: Once = Once::new();
    MOCK.call_once(|| {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
    });
    let id = uuid::Uuid::new_v4();
    let dir = std::env::temp_dir().join(format!("fl-{tag}-{id}"));
    let mut paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    paths.profile = format!("test-denials-{id}");
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    (paths, dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remembered_denial_refuses_without_asking_and_says_why() {
    let env = credential_env::EnvGuard::isolated();
    let (paths, dir) = setup(&env, "deny");
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
        // Only an explicit wildcard denies all proper subdomains.
        store
            .add_denial("net:*.example.com", "other_tool", "TUI")
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
    assert_eq!(listed[0]["capability"], "net:*.example.com");
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
    let env = credential_env::EnvGuard::isolated();
    let (paths, dir) = setup(&env, "broad");
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

/// The default mode, end to end: approvals off, a sensitive call runs with
/// nobody to ask, and it is on record (SPEC §10.3).
#[tokio::test(flavor = "multi_thread")]
async fn by_default_sensitive_calls_run_unasked_and_are_logged() {
    let env = credential_env::EnvGuard::isolated();
    let (paths, dir) = setup(&env, "open");
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
    assert!(!r.to_string().contains("denied"), "{r}");
    let store = Store::open(&paths.db_file()).unwrap();
    assert_eq!(
        store
            .recent_audit(Some("tool.run_unasked"), 5)
            .unwrap()
            .len(),
        1
    );
    let status = daemon.handle(Request::Status).await.unwrap();
    assert_eq!(status["secure"], false);
    assert_eq!(status["auth"]["credential_present"], false);
    let _ = std::fs::remove_dir_all(dir);
}
