//! Sub-agents and llm.complete follow the Session's model and effort
//! (SPEC §7.5/§7.6), not the daemon default.

#[path = "support/env.rs"]
mod credential_env;

use axum::routing::post;
use formless_core::{Paths, Store};
use std::sync::{Arc, Mutex};

#[tokio::test(flavor = "multi_thread")]
async fn llm_complete_uses_the_session_model_and_effort() {
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FORMLESS_API_KEY", "test-key");
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body);
                    axum::Json(serde_json::json!({"choices": [{"message": {"content": "pong"}}]}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-sm-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-sm-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = formless_core::Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "daemon-default".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    store
        .set_session_model(&sid, Some("session-local".into()), Some("low".into()))
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let (model, effort) = daemon.session_model_effort(&sid, &cfg);
    assert_eq!(
        (model.as_str(), effort.as_deref()),
        ("session-local", Some("low"))
    );

    let out = daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "ask_llm", "description": "asks", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": "function run(i){ return {text: llm.complete('ping')}; }",
            "capabilities": ["llm"]}))
        .await;
    assert!(out.contains("\"created\""), "{out}");
    let (tool, ver) = store.get_tool_by_name("ask_llm").unwrap().unwrap();
    store
        .set_approval(
            &tool.id,
            &formless_runtime::Capabilities::parse(&ver.capabilities).unwrap(),
            ver.memory_mb,
        )
        .unwrap();
    let r = daemon
        .call_tool_direct(&sid, "ask_llm", serde_json::json!({}))
        .await;
    assert!(r.contains("pong"), "{r}");
    let body = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(body["model"], "session-local");
    assert_eq!(body["reasoning_effort"], "low");
}
