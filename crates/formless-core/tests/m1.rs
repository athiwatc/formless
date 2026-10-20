//! M1 acceptance tests (SPEC §18 M1, §19): storage/compaction round-trip,
//! provider against a mock OpenAI-compatible server, daemon end-to-end.

use axum::{
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use formless_core::config::{Config, ProviderCfg};
use formless_core::provider::{probe, Provider};
use formless_core::storage::{build_context, Store};
use formless_core::Paths;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn temp_paths(tag: &str) -> Paths {
    // Short path: macOS Unix socket paths are limited to ~104 chars.
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

#[test]
fn storage_resume_is_byte_identical_and_compaction_flags() {
    let paths = temp_paths("storage");
    let session_id;
    {
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(Some("Test".into())).unwrap();
        session_id = s.id.clone();
        for i in 0..8 {
            store
                .append_message(
                    &s.id,
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("message {i} {}", "x".repeat(40)),
                )
                .unwrap();
        }
        store.save_checkpoint(&s.id, "earlier summary", 2).unwrap();
        let run = store.create_run(&s.id).unwrap();
        store.set_run_status(&run, "running").unwrap();
    }
    // Reopen: orphan runs interrupted, messages identical.
    let store = Store::open(&paths.db_file()).unwrap();
    store.mark_orphan_runs_interrupted().unwrap();
    let msgs = store.list_messages(&session_id).unwrap();
    assert_eq!(msgs.len(), 8);
    assert_eq!(msgs[3].content, format!("message 3 {}", "x".repeat(40)));
    let (ctx, needs, up_to) = build_context(&store, &session_id, "sys", 60, 0.8).unwrap();
    assert!(ctx
        .iter()
        .any(|v| v.to_string().contains("earlier summary")));
    assert!(needs, "tiny context must request compaction");
    assert!(up_to > 2);
    let (_ctx, needs2, _) = build_context(&store, &session_id, "sys", 1_000_000, 0.8).unwrap();
    assert!(!needs2);
}

struct MockState {
    effort_rejections: AtomicUsize,
}

async fn mock_server() -> String {
    let state = Arc::new(MockState {
        effort_rejections: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/models", get(|| async { Json(json!({"data": [{"id": "mock-1"}, {"id": "mock-2"}]})) }))
        .route("/chat/completions", post({
            let state = state.clone();
            move |Json(body): Json<Value>| {
                let state = state.clone();
                async move {
                    if body.get("reasoning_effort").is_some() && state.effort_rejections.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error": "unknown parameter reasoning_effort"}))).into_response();
                    }
                    if body["tool_choice"].as_str() == Some("required") {
                        return Json(json!({
                            "choices": [{"message": {"tool_calls": [{"id": "1", "type": "function", "function": {"name": "probe_ping", "arguments": "{}"}}]}}]
                        })).into_response();
                    }
                    let user = body["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
                    if body["stream"].as_bool().unwrap_or(false) {
                        let sse = format!(
                            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                            json!({"choices": [{"delta": {"content": "echo: "}}]}),
                            json!({"choices": [{"delta": {"content": user}}]})
                        );
                        return ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], sse).into_response();
                    }
                    Json(json!({"choices": [{"message": {"content": format!("summary of: {user}")}}]})).into_response()
                }
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn mock_cfg(base: &str) -> ProviderCfg {
    ProviderCfg {
        base_url: base.to_string(),
        model: "mock-1".into(),
        max_retries: 2,
        ..Default::default()
    }
}

#[tokio::test]
async fn provider_models_probe_stream_and_effort_retry() {
    let base = mock_server().await;
    let cfg = mock_cfg(&base);
    let p = Provider::new(cfg.clone(), "k".into()).unwrap();
    assert_eq!(p.list_models().await.unwrap(), vec!["mock-1", "mock-2"]);

    let probe = probe(&cfg, "k", "mock-1").await.unwrap();
    assert!(probe.tool_calling);

    let mut deltas = String::new();
    let out = p
        .chat_stream(
            &[serde_json::json!({"role": "user", "content": "hello"})],
            &[],
            "mock-1",
            Some("high"),
            &CancellationToken::new(),
            |d| deltas.push_str(d),
        )
        .await
        .unwrap();
    assert_eq!(out.content, "echo: hello");
    assert_eq!(deltas, "echo: hello");
    assert!(
        out.dropped_reasoning_effort,
        "first request with effort must be rejected then retried without"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_end_to_end_run_persists_and_two_clients_share() {
    use formless_core::daemon::Daemon;
    use formless_protocol::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let base = mock_server().await;
    let paths = temp_paths("daemon");
    let mut cfg = Config {
        provider: mock_cfg(&base),
        ..Default::default()
    };
    cfg.daemon.idle_grace_secs = 3600; // no idle-close during test
    cfg.save(&paths).unwrap();

    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    // Wait for socket.
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(formless_core::daemon::daemon_reachable(&paths));

    // Client A creates a session and runs a message to completion.
    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();

    // Create session.
    let session_id: String = {
        let env = RequestEnvelope {
            id: 1,
            protocol: PROTOCOL_VERSION,
            request: Request::CreateSession {
                name: Some("E2E".into()),
            },
        };
        write_half
            .write_all(serde_json::to_string(&env).unwrap().as_bytes())
            .await
            .unwrap();
        write_half.write_all(b"\n").await.unwrap();
        loop {
            let line = lines.next_line().await.unwrap().unwrap();
            if let Ok(ServerMessage::Response { id: 1, result, .. }) =
                serde_json::from_str::<ServerMessage>(&line)
            {
                break result.unwrap()["id"].as_str().unwrap().to_string();
            }
        }
    };
    // Send message; collect until RunFinished.
    let env = RequestEnvelope {
        id: 2,
        protocol: PROTOCOL_VERSION,
        request: Request::SendMessage {
            session_id: session_id.clone(),
            content: "ping".into(),
            mode: SendMode::Send,
        },
    };
    write_half
        .write_all(serde_json::to_string(&env).unwrap().as_bytes())
        .await
        .unwrap();
    write_half.write_all(b"\n").await.unwrap();
    let mut deltas = String::new();
    let mut finished = false;
    while let Some(line) = lines.next_line().await.unwrap() {
        if let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) {
            match ev {
                Event::MessageDelta { delta, .. } => deltas.push_str(&delta),
                Event::RunFinished { status, .. } => {
                    assert_eq!(status, "completed");
                    finished = true;
                    break;
                }
                _ => {}
            }
        }
    }
    assert!(finished);
    assert_eq!(deltas, "echo: ping");

    // Client B (second TUI equivalent) sees the same session + transcript.
    let stream_b = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (rb, mut wb) = tokio::io::split(stream_b);
    let mut lines_b = BufReader::new(rb).lines();
    let env = RequestEnvelope {
        id: 9,
        protocol: PROTOCOL_VERSION,
        request: Request::GetSession { id: session_id },
    };
    wb.write_all(serde_json::to_string(&env).unwrap().as_bytes())
        .await
        .unwrap();
    wb.write_all(b"\n").await.unwrap();
    let mut got = false;
    while let Some(line) = lines_b.next_line().await.unwrap() {
        if let Ok(ServerMessage::Response { id: 9, result, .. }) =
            serde_json::from_str::<ServerMessage>(&line)
        {
            let msgs = result.unwrap()["messages"].as_array().cloned().unwrap();
            assert_eq!(msgs.len(), 2);
            assert_eq!(msgs[1]["content"], "echo: ping");
            got = true;
            break;
        }
    }
    assert!(got);
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_idle_closes_after_grace() {
    use formless_core::daemon::Daemon;
    let paths = temp_paths("idle");
    let mut cfg = Config::default();
    cfg.daemon.idle_grace_secs = 1;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    // Daemon with no clients and no runs must shut itself down (SPEC §9.1).
    let done = tokio::time::timeout(std::time::Duration::from_secs(15), server).await;
    assert!(done.is_ok(), "daemon did not idle-close");
    assert!(!paths.socket_path().exists(), "socket must be cleaned up");
}
