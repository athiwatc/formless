//! The native Anthropic path through the daemon (SPEC §4.1): failed tool
//! results reach the API flagged `is_error`, and a provider repointed to
//! another preset's endpoint takes that preset's API style.

use axum::routing::post;
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn temp(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_tool_results_are_sent_with_is_error() {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let app = axum::Router::new().route(
        "/messages",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<Value>| {
                let seen = seen.clone();
                async move {
                    let first = {
                        let mut s = seen.lock().unwrap();
                        s.push(body);
                        s.len() == 1
                    };
                    let start = json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}});
                    let stop = json!({"type": "message_stop"});
                    let events = if first {
                        vec![
                            start,
                            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "tu_1", "name": "no_such_tool"}}),
                            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
                            json!({"type": "content_block_stop", "index": 0}),
                            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 1}}),
                            stop,
                        ]
                    } else {
                        vec![
                            start,
                            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "done"}}),
                            json!({"type": "content_block_stop", "index": 0}),
                            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}}),
                            stop,
                        ]
                    };
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse(&events)))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    std::env::set_var("FORMLESS_API_KEY", "k");
    let paths = temp("anth-err");
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.api_style = "anthropic".into();
    cfg.provider.model = "claude-x".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    daemon
        .handle(Request::SendMessage {
            session_id: sid.clone(),
            content: "go".into(),
            mode: formless_protocol::SendMode::Send,
        })
        .await
        .unwrap();
    for _ in 0..200 {
        if store
            .last_run(&sid)
            .unwrap()
            .is_some_and(|(_, s)| s != "running")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(store.last_run(&sid).unwrap().unwrap().1, "completed");
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let msgs = bodies[1]["messages"].as_array().unwrap();
    let result = &msgs.last().unwrap()["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["tool_use_id"], "tu_1");
    assert_eq!(result["is_error"], true, "{result}");
}

#[tokio::test(flavor = "multi_thread")]
async fn repointing_a_provider_rederives_its_api_style() {
    let paths = temp("anth-style");
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let upsert = |url: &str| Request::UpsertProvider {
        name: "work".into(),
        base_url: url.into(),
        model: None,
        api_key: None,
        headers: None,
        query_params: None,
        api_key_header: None,
    };
    let style = || {
        Config::load_file_only(&paths).unwrap().providers["work"]
            .api_style
            .clone()
    };
    daemon
        .handle(upsert("https://api.openai.com/v1"))
        .await
        .unwrap();
    assert_eq!(style(), "chat");
    daemon
        .handle(upsert("https://api.anthropic.com"))
        .await
        .unwrap();
    assert_eq!(style(), "anthropic");
    daemon
        .handle(upsert("https://api.openai.com/v1"))
        .await
        .unwrap();
    assert_eq!(style(), "chat");
}
