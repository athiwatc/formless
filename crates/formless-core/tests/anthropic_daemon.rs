//! The native Anthropic path through the daemon (SPEC §4.1): failed tool
//! results reach the API flagged `is_error`, and a provider repointed to
//! another preset's endpoint takes that preset's API style.

#[path = "support/env.rs"]
mod credential_env;

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
    let _env = credential_env::EnvGuard::isolated();
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

    _env.set("FORMLESS_API_KEY", "k");
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
            client_msg_id: None,
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
    let _env = credential_env::EnvGuard::isolated();
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

/// A mock `/messages` answering each request with the next scripted
/// (status, body); the last one repeats. Records the request bodies.
async fn scripted(script: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let app = axum::Router::new().route(
        "/messages",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<Value>| {
                let seen = seen.clone();
                let script = script.clone();
                async move {
                    let n = {
                        let mut s = seen.lock().unwrap();
                        s.push(body);
                        s.len() - 1
                    };
                    let (status, body) = script[n.min(script.len() - 1)].clone();
                    let kind = if status == 200 {
                        "text/event-stream"
                    } else {
                        "application/json"
                    };
                    axum::response::Response::builder()
                        .status(status)
                        .header("content-type", kind)
                        .body(axum::body::Body::from(body))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

/// One streamed answer: optional signed thinking, then text, then `stop`.
fn answer(thinking: bool, text: &str, stop: &str) -> (u16, String) {
    let mut ev = vec![json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}})];
    let mut i = 0;
    if thinking {
        ev.push(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}));
        ev.push(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hm"}}));
        ev.push(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig-1"}}));
        ev.push(json!({"type": "content_block_stop", "index": 0}));
        i = 1;
    }
    ev.push(json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}}));
    ev.push(json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": text}}));
    ev.push(json!({"type": "content_block_stop", "index": i}));
    ev.push(json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 1}}));
    ev.push(json!({"type": "message_stop"}));
    (200, sse(&ev))
}

async fn run_turns(
    _env: &credential_env::EnvGuard,
    base: String,
    tag: &str,
    turns: &[&str],
) -> (Store, String) {
    _env.set("FORMLESS_API_KEY", "k");
    let paths = temp(tag);
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.api_style = "anthropic".into();
    cfg.provider.model = "claude-x".into();
    cfg.provider.max_retries = 0;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    for turn in turns {
        let before = store.last_run(&sid).unwrap().map(|r| r.0);
        daemon
            .handle(Request::SendMessage {
                session_id: sid.clone(),
                content: turn.to_string(),
                mode: formless_protocol::SendMode::Send,
                client_msg_id: None,
            })
            .await
            .unwrap();
        for _ in 0..200 {
            let last = store.last_run(&sid).unwrap();
            if last
                .as_ref()
                .is_some_and(|(id, s)| Some(id) != before.as_ref() && s != "running")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    // A terminal storage status precedes the Run tail. Keep the caller's
    // credential guard alive until that detached task has released the daemon.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while Arc::strong_count(&daemon) != 1 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Anthropic Run tasks did not finish within 10 seconds");
    drop(daemon);
    (store, sid)
}

/// A paused turn keeps its signed thinking for the resume; an empty pause
/// is not resent (#1090).
#[tokio::test(flavor = "multi_thread")]
async fn a_paused_turn_resumes_with_its_thinking() {
    let _env = credential_env::EnvGuard::isolated();
    let (base, seen) = scripted(vec![
        answer(true, "part", "pause_turn"),
        answer(false, " rest", "end_turn"),
    ])
    .await;
    let (store, sid) = run_turns(&_env, base, "anth-pause", &["go"]).await;
    assert_eq!(store.last_run(&sid).unwrap().unwrap().1, "completed");
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    let resent = bodies[1]["messages"].to_string();
    assert!(resent.contains("sig-1"), "{resent}");

    let (base, seen) = scripted(vec![answer(false, "", "pause_turn")]).await;
    let (store, sid) = run_turns(&_env, base, "anth-pause-empty", &["go"]).await;
    assert_eq!(store.last_run(&sid).unwrap().unwrap().1, "completed");
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "an empty pause is not resent"
    );
}

/// A full context window whose compaction fails keeps the partial answer
/// and says why, instead of failing the Run (#1091).
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_context_compaction_keeps_the_partial_answer() {
    let _env = credential_env::EnvGuard::isolated();
    for partial in ["partial", ""] {
        let fail = (
            400,
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": "nope"}})
                .to_string(),
        );
        let (base, seen) = scripted(vec![
            answer(false, "first answer", "end_turn"),
            answer(false, "second answer", "end_turn"),
            answer(false, "third answer", "end_turn"),
            answer(false, partial, "model_context_window_exceeded"),
            fail,
        ])
        .await;
        let (store, sid) =
            run_turns(&_env, base, "anth-ctx", &["one", "two", "three", "four"]).await;
        assert_eq!(store.last_run(&sid).unwrap().unwrap().1, "completed");
        let msgs = store.list_messages(&sid).unwrap();
        let last = msgs.iter().rev().find(|m| m.role == "assistant").unwrap();
        assert_eq!(last.content, partial);
        assert!(seen.lock().unwrap().len() >= 5, "compaction was tried");
        assert!(last
            .meta
            .as_deref()
            .unwrap_or_default()
            .contains("model_context_window_exceeded"));
        let meta: Value = serde_json::from_str(last.meta.as_deref().unwrap()).unwrap();
        assert_eq!(meta["truncated"], true);
        let context = formless_core::storage::build_context(&store, &sid, "system", 200_000, 1.0)
            .unwrap()
            .0;
        let answer = context
            .iter()
            .rev()
            .find(|m| m["role"] == "assistant")
            .unwrap();
        assert!(answer["content"]
            .as_str()
            .unwrap()
            .contains("[harness note]"));
        assert!(answer["content"].as_str().unwrap().contains("cut off"));
        assert!(!answer["content"].as_str().unwrap().trim().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn output_limit_partials_persist_and_resend_as_incomplete() {
    let _env = credential_env::EnvGuard::isolated();
    let (base, _) = scripted(vec![answer(false, "unfinished", "max_tokens")]).await;
    let (store, sid) = run_turns(&_env, base, "anth-output-limit", &["go"]).await;
    let messages = store.list_messages(&sid).unwrap();
    let answer = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .unwrap();
    let meta: Value = serde_json::from_str(answer.meta.as_deref().unwrap()).unwrap();
    assert_eq!(meta["truncated"], true);
    assert_eq!(meta["stop_reason"], "max_tokens");
    let context = formless_core::storage::build_context(&store, &sid, "system", 200_000, 1.0)
        .unwrap()
        .0;
    let answer = context
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    let text = answer["content"].as_str().unwrap();
    assert!(text.starts_with("unfinished\n\n[harness note]"), "{text}");
    assert!(text.contains("cut off (max_tokens)"), "{text}");
}
