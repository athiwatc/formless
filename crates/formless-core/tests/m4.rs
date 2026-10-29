//! M4 acceptance tests (SPEC §18 M4): pending invocations (await + push),
//! crash recovery, export/import across profiles, reset, server compaction.

use formless_core::config::reset_all;
use formless_core::storage::Store;
use formless_core::Paths;
use formless_runtime::Capabilities;
use serde_json::{json, Value};
use std::sync::Arc;

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    // Unique profile per test dir: keychain users derive from the profile,
    // so tests must never touch the developer's real "default" entries
    // (which can also block on a macOS Keychain access prompt).
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

#[test]
fn crash_recovery_marks_run_and_invocation_interrupted_without_reexecution() {
    let paths = temp_paths("m4crash");
    let (run_id, inv_id, tool_id) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(None).unwrap();
        let run = store.create_run(&s.id).unwrap();
        let (info, _v) = store
            .create_tool_version(
                "t",
                "d",
                r#"{"type":"object"}"#,
                r#"{"type":"object"}"#,
                "function run(i){return {};}",
                &Capabilities::default(),
                256,
                "agent",
            )
            .unwrap();
        let inv = store
            .start_invocation(&info.id, 1, Some(&s.id), Some(&run), None, "{}")
            .unwrap();
        (run, inv, info.id)
    };
    // Simulate daemon restart (Daemon::new calls this; call it directly too).
    let store = Store::open(&paths.db_file()).unwrap();
    store.mark_orphan_runs_interrupted().unwrap();
    assert_eq!(
        store.run_status(&run_id).unwrap().as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        store.invocation_status(&inv_id).unwrap().as_deref(),
        Some("interrupted")
    );
    // No re-execution: exactly one invocation row exists.
    assert_eq!(store.tool_metrics(&tool_id, None).unwrap().total, 1);
}

#[test]
fn reset_wipes_data_config_and_key_file() {
    let paths = temp_paths("m4reset");
    std::fs::write(paths.db_file(), "db").unwrap();
    std::fs::write(paths.config_file(), "cfg").unwrap();
    std::fs::write(paths.key_fallback_file(), "secret-key").unwrap();
    let data = paths.data_dir.clone();
    let cfg = paths.config_dir.clone();
    reset_all(&paths).unwrap();
    assert!(!data.exists(), "data dir must be gone");
    assert!(!cfg.exists(), "config dir must be gone");
    // Key material gone (checked via the file, not load_api_key: sibling
    // tests set FORMLESS_API_KEY process-wide, which load honours first).
    assert!(
        !paths.key_fallback_file().exists(),
        "key fallback file must be gone"
    );
}

// --- Shared daemon test client + mocks ---

#[cfg(unix)]
struct Client {
    write: tokio::io::WriteHalf<tokio::net::UnixStream>,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::io::ReadHalf<tokio::net::UnixStream>>>,
    next: u64,
}

#[cfg(unix)]
impl Client {
    async fn connect(paths: &Paths) -> Self {
        use tokio::io::AsyncBufReadExt;
        let stream = tokio::net::UnixStream::connect(paths.socket_path())
            .await
            .unwrap();
        let (r, w) = tokio::io::split(stream);
        Self {
            write: w,
            lines: tokio::io::BufReader::new(r).lines(),
            next: 0,
        }
    }
    async fn rpc(&mut self, req: formless_protocol::Request) -> Value {
        use tokio::io::AsyncWriteExt;
        self.next += 1;
        let id = self.next;
        let env = formless_protocol::RequestEnvelope {
            id,
            protocol: formless_protocol::PROTOCOL_VERSION,
            request: req,
        };
        self.write
            .write_all(serde_json::to_string(&env).unwrap().as_bytes())
            .await
            .unwrap();
        self.write.write_all(b"\n").await.unwrap();
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .unwrap()
                .expect("daemon closed");
            if let Ok(formless_protocol::ServerMessage::Response {
                id: rid,
                ok,
                result,
                error,
            }) = serde_json::from_str::<formless_protocol::ServerMessage>(&line)
            {
                if rid == id {
                    assert!(ok, "rpc error: {error:?}");
                    return result.unwrap_or(Value::Null);
                }
            }
        }
    }
    /// Like `rpc`, but an error response is returned, not a panic.
    async fn rpc_result(&mut self, req: formless_protocol::Request) -> Result<Value, String> {
        use tokio::io::AsyncWriteExt;
        self.next += 1;
        let id = self.next;
        let env = formless_protocol::RequestEnvelope {
            id,
            protocol: formless_protocol::PROTOCOL_VERSION,
            request: req,
        };
        self.write
            .write_all(serde_json::to_string(&env).unwrap().as_bytes())
            .await
            .unwrap();
        self.write.write_all(b"\n").await.unwrap();
        loop {
            let line = self.lines.next_line().await.unwrap().expect("daemon closed");
            if let Ok(formless_protocol::ServerMessage::Response { id: rid, ok, result, error }) =
                serde_json::from_str::<formless_protocol::ServerMessage>(&line)
            {
                if rid == id {
                    return if ok { Ok(result.unwrap_or(Value::Null)) } else { Err(error.unwrap_or_default()) };
                }
            }
        }
    }
    async fn send(&mut self, req: formless_protocol::Request) {
        use tokio::io::AsyncWriteExt;
        self.next += 1;
        let env = formless_protocol::RequestEnvelope {
            id: self.next,
            protocol: formless_protocol::PROTOCOL_VERSION,
            request: req,
        };
        self.write
            .write_all(serde_json::to_string(&env).unwrap().as_bytes())
            .await
            .unwrap();
        self.write.write_all(b"\n").await.unwrap();
    }
    /// Read events until RunFinished; returns accumulated assistant deltas.
    async fn run_to_finish(&mut self) -> String {
        use formless_protocol::{Event, ServerMessage};
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let next = tokio::time::timeout_at(deadline, self.lines.next_line())
                .await
                .expect("run_to_finish timed out after 60s");
            let Some(line) = next.unwrap() else {
                panic!("daemon closed before RunFinished")
            };
            if let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) {
                match ev {
                    Event::MessageDelta { delta, .. } => text.push_str(&delta),
                    Event::RunFinished { status, .. } => {
                        assert_eq!(status, "completed");
                        break;
                    }
                    Event::RunPaused { error, .. } => panic!("run paused: {error}"),
                    _ => {}
                }
            }
        }
        text
    }
}

#[cfg(unix)]
async fn start_daemon(paths: &Paths) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    server
}

fn write_cfg(paths: &Paths, base: &str, pending_secs: u64, server_compact: bool, ctx_tokens: u32) {
    let p = formless_core::config::ProviderCfg {
        base_url: base.to_string(),
        model: "mock-1".into(),
        max_retries: 1,
        context_tokens: ctx_tokens,
        ..Default::default()
    };
    let mut cfg = formless_core::Config {
        provider: p,
        ..Default::default()
    };
    cfg.daemon.idle_grace_secs = 3600;
    cfg.security.secure = true;
    cfg.tools.pending_threshold_secs = pending_secs;
    cfg.compaction.server_side = server_compact;
    // These tests assert exact event streams; keep the namer (and its
    // model download) out of them.
    cfg.naming.enabled = false;
    cfg.save(paths).unwrap();
}

fn seed_sleeper(paths: &Paths) {
    let store = Store::open(&paths.db_file()).unwrap();
    let schema = r#"{"type":"object","properties":{"ms":{"type":"integer"}},"required":["ms"]}"#;
    let out = r#"{"type":"object","properties":{"slept":{"type":"integer"}},"required":["slept"]}"#;
    store
        .create_tool_version(
            "sleeper",
            "sleeps",
            schema,
            out,
            "function run(i){ sleep(i.ms); return {slept: i.ms}; }",
            &Capabilities::default(),
            256,
            "agent",
        )
        .unwrap();
    let eschema = r#"{"type":"object","properties":{"x":{"type":"integer"}},"required":["x"]}"#;
    store
        .create_tool_version(
            "echo_fast",
            "echo",
            eschema,
            eschema,
            "function run(i){ return {x: i.x}; }",
            &Capabilities::default(),
            256,
            "agent",
        )
        .unwrap();
}

fn sse_tool(cid: &str, name: &str, args: Value) -> String {
    let d = json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": cid, "function": {"name": name, "arguments": args.to_string()}}]}}]});
    format!("data: {d}\n\ndata: [DONE]\n\n")
}
fn sse_text(text: &str) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices": [{"delta": {"content": text}}]})
    )
}

// --- Pending: await path (with remind-once) ---

#[cfg(unix)]
#[tokio::test]
async fn pending_await_path_with_remind_once() {
    use axum::{
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4await");
    seed_sleeper(&paths);
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                let joined = serde_json::to_string(&msgs).unwrap();
                let tools: Vec<&Value> = msgs.iter().filter(|m| m["role"] == "tool").collect();
                let sse = if tools.is_empty() {
                    sse_tool("c1", "sleeper", json!({"ms": 1800}))
                } else if joined.contains("still have pending invocations") && tools.len() == 1 {
                    // Parse the pending handle out of the first tool result.
                    let content = tools[0]["content"].as_str().unwrap_or_default();
                    let handle = serde_json::from_str::<Value>(content)
                        .map(|v| v["invocation_id"].as_str().unwrap_or_default().to_string())
                        .unwrap_or_default();
                    sse_tool("c2", "await_invocation", json!({"invocation_id": handle}))
                } else if tools.len() == 1 {
                    // Attempt a final answer while still pending -> daemon must remind once.
                    sse_text("early final")
                } else {
                    let last = tools
                        .last()
                        .map(|m| m["content"].as_str().unwrap_or_default().to_string())
                        .unwrap_or_default();
                    sse_text(&format!("awaited: {last}"))
                };
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse,
                )
                    .into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &format!("http://{addr}"), 1, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut client = Client::connect(&paths).await;
    let s = client.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    client
        .send(Request::SendMessage {
            session_id: sid,
            content: "sleep please".into(),
            mode: SendMode::Send,
        })
        .await;
    let text = client.run_to_finish().await;
    server.abort();
    assert!(
        text.contains("awaited:") && text.contains("1800"),
        "final: {text}"
    );
    let store = Store::open(&paths.db_file()).unwrap();
    let (info, _v) = store.get_tool_by_name("sleeper").unwrap().unwrap();
    let m = store.tool_metrics(&info.id, None).unwrap();
    assert_eq!((m.success, m.failure), (1, 0));
}

// --- Pending: push delivery path ---

#[cfg(unix)]
#[tokio::test]
async fn pending_push_delivery_reaches_agent() {
    use axum::{
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    use std::sync::atomic::{AtomicBool, Ordering};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4push");
    seed_sleeper(&paths);
    let saw_push = Arc::new(AtomicBool::new(false));
    let saw = saw_push.clone();
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(move |Json(body): Json<Value>| {
                let saw = saw.clone();
                async move {
                    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                    let joined = serde_json::to_string(&msgs).unwrap();
                    if joined.contains("[Pending invocation") && joined.contains("completed]") {
                        saw.store(true, Ordering::SeqCst);
                        return (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            sse_text("pushed ok"),
                        )
                            .into_response();
                    }
                    let tools: Vec<&Value> = msgs.iter().filter(|m| m["role"] == "tool").collect();
                    let sse = if tools.is_empty() {
                        sse_tool("c1", "sleeper", json!({"ms": 1600}))
                    } else if tools.len() == 1 {
                        // Slow provider response lets the sleeper finish meanwhile.
                        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                        sse_tool("c2", "echo_fast", json!({"x": 1}))
                    } else {
                        sse_tool("c3", "echo_fast", json!({"x": 2}))
                    };
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                        sse,
                    )
                        .into_response()
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &format!("http://{addr}"), 1, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut client = Client::connect(&paths).await;
    let s = client.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    client
        .send(Request::SendMessage {
            session_id: sid,
            content: "sleep then echo".into(),
            mode: SendMode::Send,
        })
        .await;
    let text = client.run_to_finish().await;
    server.abort();
    assert_eq!(text, "pushed ok");
    assert!(
        saw_push.load(Ordering::SeqCst),
        "provider must have seen the push-delivered result"
    );
}

// --- Export/import across profiles + server-side compaction ---

#[cfg(unix)]
#[tokio::test]
async fn tool_and_session_export_import_across_profiles() {
    use formless_protocol::Request;
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths_a = temp_paths("m4pa");
    let paths_b = temp_paths("m4pb");
    // Seed A: tool with metrics + approval, session with messages.
    let tool_id;
    let session_id;
    {
        let store = Store::open(&paths_a.db_file()).unwrap();
        let caps = Capabilities::parse(&["fs:read:/tmp".to_string()]).unwrap();
        let (info, _v) = store
            .create_tool_version(
                "porter",
                "portable",
                r#"{"type":"object"}"#,
                r#"{"type":"object"}"#,
                "function run(i){ return {ok:true}; }",
                &caps,
                256,
                "agent",
            )
            .unwrap();
        tool_id = info.id.clone();
        store.set_tool_title(&tool_id, "Port checker").unwrap();
        store
            .record_invocation(
                &tool_id,
                1,
                None,
                None,
                None,
                "success",
                "",
                r#"{"port":80}"#,
                r#"{"open":true}"#,
                "",
                1,
                200,
            )
            .unwrap();
        store.set_approval(&tool_id, &caps, 256).unwrap();
        let s = store.create_session(Some("exported".into())).unwrap();
        session_id = s.id.clone();
        store.append_message(&s.id, "user", "hello export").unwrap();
        store
            .append_message(&s.id, "assistant", "hi there")
            .unwrap();
    }
    write_cfg(&paths_a, "http://127.0.0.1:9", 300, false, 128_000);
    write_cfg(&paths_b, "http://127.0.0.1:9", 300, false, 128_000);
    let server_a = start_daemon(&paths_a).await;
    let mut a = Client::connect(&paths_a).await;
    let bundle = a
        .rpc(Request::ExportTool {
            id: tool_id.clone(),
        })
        .await;
    assert_eq!(bundle["format"], "formless.tool/1");
    assert_eq!(bundle["title"], "Port checker");
    // The latest successful call travels as an example (SPEC §10.4).
    assert_eq!(bundle["example"]["input"]["port"], 80, "{bundle}");
    assert_eq!(bundle["example"]["output"]["open"], true);
    assert!(
        bundle.get("success_rate").is_none() && bundle.get("invocations").is_none(),
        "bundle must carry no metrics: {bundle}"
    );
    let sbundle = a
        .rpc(Request::ExportSession {
            id: session_id,
            format: None,
        })
        .await;
    server_a.abort();

    let server_b = start_daemon(&paths_b).await;
    let mut b = Client::connect(&paths_b).await;
    let imported = b
        .rpc(Request::ImportTool {
            bundle: bundle.clone(),
        })
        .await;
    assert_eq!(imported["name"], "porter");
    assert_eq!(imported["title"], "Port checker", "the title travels");
    assert_eq!(
        imported["invocations"], 0,
        "imported tool metrics must start fresh"
    );
    // Collision: second import becomes a new tool, never a version overwrite.
    let imported2 = b.rpc(Request::ImportTool { bundle }).await;
    assert_eq!(imported2["name"], "porter_imported");
    // A new version without a title keeps the Tool's title (SPEC §7.2).
    let edited = b
        .rpc(Request::UpdateToolCode {
            id: imported2["id"].as_str().unwrap().to_string(),
            code: "function run(i){ return {ok:false}; }".into(),
        })
        .await;
    assert_eq!(edited["tool"]["title"], "Port checker", "{edited}");
    let s2 = b.rpc(Request::ImportSession { bundle: sbundle }).await;
    let got = b
        .rpc(Request::GetSession {
            id: s2["id"].as_str().unwrap().to_string(),
        })
        .await;
    server_b.abort();
    let msgs = got["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0]["content"], "hello export");
    // Untrusted: no approval carried to profile B.
    let store_b = Store::open(&paths_b.db_file()).unwrap();
    let (info_b, ver_b) = store_b.get_tool_by_name("porter").unwrap().unwrap();
    assert_eq!(ver_b.author, "import");
    assert!(
        store_b.get_approval(&info_b.id).unwrap().is_none(),
        "import must not carry approval"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn server_side_compaction_uses_compact_endpoint() {
    use axum::{
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4compact");
    let session_id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(None).unwrap();
        for i in 0..8 {
            store
                .append_message(
                    &s.id,
                    if i % 2 == 0 { "user" } else { "assistant" },
                    &format!("long message {i} {}", "y".repeat(60)),
                )
                .unwrap();
        }
        s.id
    };
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/compact",
            post(|Json(_b): Json<Value>| async { Json(json!({"summary": "SERVER SUMMARY"})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(_b): Json<Value>| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse_text("after compact"),
                )
                    .into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &format!("http://{addr}"), 300, true, 60);
    let server = start_daemon(&paths).await;
    let mut client = Client::connect(&paths).await;
    client
        .send(Request::SendMessage {
            session_id: session_id.clone(),
            content: "continue".into(),
            mode: SendMode::Send,
        })
        .await;
    let text = client.run_to_finish().await;
    server.abort();
    assert_eq!(text, "after compact");
    let store = Store::open(&paths.db_file()).unwrap();
    let (summary, _up) = store
        .latest_checkpoint(&session_id)
        .unwrap()
        .expect("checkpoint saved");
    assert_eq!(summary, "SERVER SUMMARY");
}

/// Wait for the SessionUpdated event the namer broadcasts after a Run.
#[cfg(unix)]
async fn next_session_update(client: &mut Client) -> formless_protocol::SessionInfo {
    use formless_protocol::{Event, ServerMessage};
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let line = tokio::time::timeout_at(deadline, client.lines.next_line())
            .await
            .expect("no SessionUpdated within 30s")
            .unwrap()
            .expect("daemon closed");
        if let Ok(ServerMessage::Event(Event::SessionUpdated { session })) =
            serde_json::from_str::<ServerMessage>(&line)
        {
            return session;
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn untitled_session_is_auto_named_after_first_run_but_user_names_stick() {
    use axum::{
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode, SessionInfo};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4naming");
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(_b): Json<Value>| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse_text("done"),
                )
                    .into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &format!("http://{addr}"), 300, false, 100_000);
    // Heuristic namer only: no model id, so nothing is downloaded or loaded.
    let mut cfg = formless_core::Config::load(&paths).unwrap();
    cfg.naming.enabled = true;
    cfg.naming.model = String::new();
    cfg.save(&paths).unwrap();
    let server = start_daemon(&paths).await;
    let mut client = Client::connect(&paths).await;

    let s: SessionInfo =
        serde_json::from_value(client.rpc(Request::CreateSession { name: None }).await).unwrap();
    assert_eq!(s.name, "Untitled");
    client
        .send(Request::SendMessage {
            session_id: s.id.clone(),
            content: "Convert my CSV exports into JSON files please\nthanks".into(),
            mode: SendMode::Send,
        })
        .await;
    client.run_to_finish().await;
    let named = next_session_update(&mut client).await;
    assert_eq!(named.id, s.id);
    assert_eq!(named.name, "Convert my CSV exports into JSON…");

    // A name the user chose is never replaced by the namer.
    let mine: SessionInfo = serde_json::from_value(
        client
            .rpc(Request::CreateSession {
                name: Some("Mine".into()),
            })
            .await,
    )
    .unwrap();
    client
        .send(Request::SendMessage {
            session_id: mine.id.clone(),
            content: "hello there".into(),
            mode: SendMode::Send,
        })
        .await;
    client.run_to_finish().await;
    server.abort();
    let store = Store::open(&paths.db_file()).unwrap();
    assert_eq!(store.get_session(&mine.id).unwrap().unwrap().name, "Mine");
}

/// Deleting a Session interrupts its Run first, so nothing keeps writing to
/// (or calling the provider for) a deleted Session.
#[cfg(unix)]
#[tokio::test]
async fn deleting_a_session_interrupts_its_run_first() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4del");
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|| async {
                // A provider that never answers in time.
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                sse_text("too late")
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let s = c.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    c.send(Request::SendMessage {
        session_id: sid.clone(),
        content: "hello".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut deleted_sent = false;
    let mut finished = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let line = tokio::time::timeout_at(deadline, c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) else {
            continue;
        };
        match ev {
            Event::RunStarted { .. } if !deleted_sent => {
                deleted_sent = true;
                c.send(Request::DeleteSession { id: sid.clone() }).await;
            }
            Event::RunFinished { status, .. } => finished = Some(status),
            Event::SessionDeleted { id } if id == sid => break,
            _ => {}
        }
    }
    assert_eq!(
        finished.as_deref(),
        Some("interrupted"),
        "run stopped before delete"
    );
    let list = c.rpc(Request::ListSessions).await;
    assert!(!list.to_string().contains(&sid));
    server.abort();
}

/// A tool call cut off at the output token limit is not run; the model is
/// told why. A cut-off answer is flagged to the user.
#[cfg(unix)]
#[tokio::test]
async fn length_truncated_tool_call_is_explained_not_run() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4len");
    seed_sleeper(&paths);
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                let tool = msgs.iter().find(|m| m["role"] == "tool");
                let d = match tool {
                    None => json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1",
                        "function": {"name": "echo_fast", "arguments": "{\"x\": 12"}}]},
                        "finish_reason": "length"}]}),
                    Some(t) => json!({"choices": [{"delta": {"content":
                        format!("saw: {}", t["content"].as_str().unwrap_or_default())},
                        "finish_reason": "length"}]}),
                };
                format!("data: {d}\n\ndata: [DONE]\n\n")
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let s = c.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    c.send(Request::SendMessage {
        session_id: sid.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    let (mut started, mut noted) = (false, false);
    let mut answer = String::new();
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), c.lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) else {
            continue;
        };
        match ev {
            Event::ToolCallStarted { .. } => started = true,
            Event::MessageDelta { delta, .. } => answer.push_str(&delta),
            Event::Note { text, .. } if text.contains("cut off") => noted = true,
            Event::RunFinished { status, .. } => {
                assert_eq!(status, "completed");
                break;
            }
            _ => {}
        }
    }
    assert!(!started, "a truncated call must not run");
    assert!(
        answer.contains("cut off at the output token limit"),
        "{answer}"
    );
    assert!(noted, "a truncated answer is flagged");
    server.abort();
}

/// Interrupting a Run releases a Tool blocked in llm.complete at once,
/// instead of waiting out the nested provider call.
#[cfg(unix)]
#[tokio::test]
async fn interrupt_releases_a_tool_waiting_on_llm_complete() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4llm");
    {
        let store = Store::open(&paths.db_file()).unwrap();
        let schema = r#"{"type":"object","properties":{}}"#;
        store
            .create_tool_version(
                "asker",
                "asks the model",
                schema,
                r#"{"type":"string"}"#,
                "function run(i){ return llm.complete('think hard'); }",
                &Capabilities::parse(&["llm".to_string()]).unwrap(),
                256,
                "agent",
            )
            .unwrap();
    }
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                if body["stream"] == json!(false) {
                    // The nested llm.complete call never answers in time.
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                }
                sse_tool("c1", "asker", json!({}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let s = c.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    c.send(Request::SendMessage {
        session_id: sid.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    let (mut interrupted_at, mut run_done, mut tool_done) = (None, false, false);
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), c.lines.next_line())
            .await
            .expect("run or tool did not stop")
            .unwrap()
            .unwrap();
        let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) else {
            continue;
        };
        match ev {
            Event::ApprovalRequested { approval_id, .. } => {
                c.send(Request::ApprovalDecision {
                    remember_denial: Default::default(),
                    paths: Default::default(),
                    approval_id,
                    approved: true,
                    scope: formless_protocol::ApprovalScope::Once,
                })
                .await;
            }
            Event::ToolCallStarted { tool_name, .. } if tool_name == "asker" => {
                // Let the Tool reach the blocking provider call first.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                interrupted_at = Some(std::time::Instant::now());
                c.send(Request::InterruptRun {
                    session_id: sid.clone(),
                })
                .await;
            }
            Event::RunFinished { status, .. } => {
                assert_eq!(status, "interrupted");
                run_done = true;
            }
            // The sandbox thread itself must come back, not just the Run.
            Event::ToolCallFinished { tool_name, .. } if tool_name == "asker" => tool_done = true,
            _ => {}
        }
        if run_done && tool_done {
            break;
        }
    }
    let took = interrupted_at.expect("tool started").elapsed();
    assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
    server.abort();
}

/// Resume continues an interrupted Run from its transcript: the dangling
/// tool call gets an "interrupted, not retried" result and the agent is
/// told how the previous Run stopped.
#[cfg(unix)]
#[tokio::test]
async fn resume_continues_an_interrupted_run_and_abandon_drops_it() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::Request;
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4resume");
    let session_id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(None).unwrap();
        store.append_message(&s.id, "user", "do it").unwrap();
        let calls = json!({"tool_calls": [{"id": "c9", "type": "function",
            "function": {"name": "sleeper", "arguments": "{\"ms\":1}"}}]});
        store
            .append_message_meta(&s.id, "assistant", "", Some(calls.to_string()))
            .unwrap();
        let run = store.create_run(&s.id).unwrap();
        store.set_run_status(&run, "running").unwrap();
        // Daemon restart marks it interrupted.
        s.id
    };
    let seen = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let seen = seen.clone();
                move |Json(body): Json<Value>| {
                    let seen = seen.clone();
                    async move {
                        seen.lock().unwrap().push(body["messages"].clone());
                        sse_text("picked up where I left off")
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;

    let r = c
        .rpc(Request::ResumeRun {
            session_id: session_id.clone(),
        })
        .await;
    assert_eq!(r["started"], true);
    let text = c.run_to_finish().await;
    assert_eq!(text, "picked up where I left off");
    let msgs = seen.lock().unwrap()[0].clone();
    let tool = msgs
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("dangling call answered");
    assert_eq!(tool["tool_call_id"], "c9");
    assert!(tool["content"].as_str().unwrap().contains("not retried"));
    assert!(msgs.to_string().contains("previous run was interrupted"));

    // The resumed Run completed: nothing left to resume or abandon.
    let r = c
        .rpc(Request::AbandonRun {
            session_id: session_id.clone(),
        })
        .await;
    assert_eq!(r["abandoned"], false);
    {
        let store = Store::open(&paths.db_file()).unwrap();
        let run = store.create_run(&session_id).unwrap();
        store.set_run_status(&run, "paused").unwrap();
    }
    let r = c
        .rpc(Request::AbandonRun {
            session_id: session_id.clone(),
        })
        .await;
    assert_eq!(r["abandoned"], true);
    let store = Store::open(&paths.db_file()).unwrap();
    assert_eq!(store.last_run(&session_id).unwrap().unwrap().1, "abandoned");
    server.abort();
}

/// A stream that goes quiet is flagged (ProviderStalled); RetryProviderRequest
/// re-sends it and the Run completes without losing anything.
#[cfg(unix)]
#[tokio::test]
async fn stalled_stream_is_flagged_and_can_be_resent() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4stall");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(move || {
                let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    let first = format!(
                        "data: {}\n\n",
                        json!({"choices": [{"delta": {"content": if n == 0 { "hmm" } else { "done" }}}]})
                    );
                    let stream = futures_util::stream::unfold(0, move |i| {
                        let first = first.clone();
                        async move {
                            match (i, n) {
                                (0, _) => Some((Ok::<_, std::io::Error>(first), 1)),
                                // First request: open but silent from here on.
                                (_, 0) => {
                                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                                    None
                                }
                                (1, _) => Some((Ok("data: [DONE]\n\n".to_string()), 2)),
                                _ => None,
                            }
                        }
                    });
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from_stream(stream))
                        .unwrap()
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let mut cfg = formless_core::Config::load(&paths).unwrap();
    cfg.provider.stall_notice_secs = 1;
    cfg.save(&paths).unwrap();
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let s = c.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    c.send(Request::SendMessage {
        session_id: sid.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    let (mut stalled, mut restarted) = (false, false);
    let mut final_text = None;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(15), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) else {
            continue;
        };
        match ev {
            Event::ProviderStalled { idle_secs, .. } => {
                assert!(idle_secs >= 1);
                stalled = true;
                c.send(Request::RetryProviderRequest {
                    session_id: sid.clone(),
                })
                .await;
            }
            Event::StreamRestarted { .. } => restarted = true,
            Event::MessageComplete { message, .. } => final_text = Some(message.content),
            Event::RunFinished { status, .. } => {
                assert_eq!(status, "completed");
                break;
            }
            _ => {}
        }
    }
    assert!(stalled && restarted);
    assert_eq!(final_text.as_deref(), Some("done"));
    server.abort();
}

/// A queued message is stored (visible in GetSession) and starts its own
/// Run when the current one ends.
#[cfg(unix)]
#[tokio::test]
async fn queued_message_is_persisted_and_runs_next() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4queue");
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                let last = body["messages"].as_array().unwrap().last().unwrap()["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                sse_text(&format!("re: {last}"))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let s = c.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    c.rpc(Request::SendMessage {
        session_id: sid.clone(),
        content: "one".into(),
        mode: SendMode::Send,
    })
    .await;
    let r = c
        .rpc(Request::SendMessage {
            session_id: sid.clone(),
            content: "two".into(),
            mode: SendMode::Queue,
        })
        .await;
    assert_eq!(r["queued"], true);
    let got = c.rpc(Request::GetSession { id: sid.clone() }).await;
    assert_eq!(got["queued"], json!(["two"]));
    // Stored, not in memory: visible to a fresh store handle too.
    assert_eq!(
        Store::open(&paths.db_file())
            .unwrap()
            .list_queued_messages(&sid)
            .unwrap(),
        ["two"]
    );
    let mut answers = Vec::new();
    while answers.len() < 2 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        if let Ok(ServerMessage::Event(Event::MessageComplete { message, .. })) =
            serde_json::from_str::<ServerMessage>(&line)
        {
            answers.push(message.content);
        }
    }
    assert_eq!(answers, ["re: one", "re: two"]);
    let got = c.rpc(Request::GetSession { id: sid.clone() }).await;
    assert_eq!(got["queued"], json!([]));
    server.abort();
}

/// A handle that was pending when the daemon stopped is reported to its
/// Session on restart, and check_invocation answers "interrupted" for it.
#[cfg(unix)]
#[tokio::test]
async fn pending_handle_survives_restart_as_interrupted() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4handle");
    let sid = {
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(None).unwrap();
        store.add_pending_handle("h-old", &s.id, "sleeper").unwrap();
        s.id
    };
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                match msgs.iter().find(|m| m["role"] == "tool") {
                    None => sse_tool("c1", "check_invocation", json!({"invocation_id": "h-old"})),
                    Some(t) => sse_text(t["content"].as_str().unwrap_or_default()),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    {
        let store = Store::open(&paths.db_file()).unwrap();
        let msgs = store.list_messages(&sid).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.role == "system-note" && m.content.contains("h-old")),
            "the Session is told about the lost handle"
        );
    }
    let mut c = Client::connect(&paths).await;
    c.send(Request::SendMessage {
        session_id: sid.clone(),
        content: "how did it go?".into(),
        mode: SendMode::Send,
    })
    .await;
    let answer = c.run_to_finish().await;
    let v: Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(v["status"], "interrupted", "{answer}");
    // Answered once, then forgotten.
    let store = Store::open(&paths.db_file()).unwrap();
    assert!(store.pending_handle("h-old").unwrap().is_none());
    server.abort();
}

/// A silent Invocation is flagged with a structured event naming its
/// handle; CancelInvocation stops just that Tool and the Run carries on.
#[cfg(unix)]
#[tokio::test]
async fn stalled_invocation_is_flagged_and_can_be_cancelled_alone() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4invstall");
    seed_sleeper(&paths);
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                match msgs.iter().find(|m| m["role"] == "tool") {
                    None => sse_tool("c1", "sleeper", json!({"ms": 60000})),
                    Some(t) => sse_text(&format!(
                        "tool said: {}",
                        t["content"].as_str().unwrap_or_default()
                    )),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let mut cfg = formless_core::Config::load(&paths).unwrap();
    cfg.daemon.stall_warning_secs = 1;
    cfg.save(&paths).unwrap();
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let s = c.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    c.send(Request::SendMessage {
        session_id: sid.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut stalled_at = None;
    let mut answer = String::new();
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) else {
            continue;
        };
        match ev {
            Event::InvocationStalled {
                handle, tool_name, ..
            } => {
                assert_eq!(tool_name, "sleeper");
                stalled_at = Some(std::time::Instant::now());
                c.send(Request::CancelInvocation { handle }).await;
            }
            Event::MessageDelta { delta, .. } => answer.push_str(&delta),
            Event::RunFinished { status, .. } => {
                assert_eq!(status, "completed", "the Run itself carries on");
                break;
            }
            _ => {}
        }
    }
    let took = stalled_at.expect("stall flagged").elapsed();
    assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
    assert!(answer.contains("interrupted"), "{answer}");
    server.abort();
}

/// agents.run is visible: lifecycle events while it works, and its
/// transcript is stored and can be fetched afterwards.
#[cfg(unix)]
#[tokio::test]
async fn subagents_emit_events_and_keep_a_transcript() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4sub");
    {
        let store = Store::open(&paths.db_file()).unwrap();
        store
            .create_tool_version(
                "delegator",
                "hands work to a sub-agent",
                r#"{"type":"object","properties":{}}"#,
                r#"{"type":"string"}"#,
                "function run(i){ return agents.run('count the files'); }",
                &Capabilities::parse(&["agent".to_string()]).unwrap(),
                256,
                "agent",
            )
            .unwrap();
    }
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                if msgs[0]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("Formless sub-agent")
                {
                    return sse_text("there are 3 files");
                }
                match msgs.iter().find(|m| m["role"] == "tool") {
                    None => sse_tool("c1", "delegator", json!({})),
                    Some(_) => sse_text("delegated"),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let s = c.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    c.send(Request::SendMessage {
        session_id: sid.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut seen = Vec::new();
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) else {
            continue;
        };
        match ev {
            Event::ApprovalRequested { approval_id, .. } => {
                c.send(Request::ApprovalDecision {
                    remember_denial: Default::default(),
                    paths: Default::default(),
                    approval_id,
                    approved: true,
                    scope: formless_protocol::ApprovalScope::Once,
                })
                .await;
            }
            Event::SubagentStarted { subagent, .. } => {
                assert_eq!(subagent.task, "count the files");
                assert_eq!(subagent.depth, 1);
                seen.push("started".to_string());
            }
            Event::SubagentStep {
                step, tool_calls, ..
            } => seen.push(format!("step {step} {tool_calls:?}")),
            Event::SubagentFinished { subagent, .. } => {
                assert_eq!(subagent.result.as_deref(), Some("there are 3 files"));
                seen.push(subagent.status);
            }
            Event::RunFinished { .. } => break,
            _ => {}
        }
    }
    assert_eq!(seen, ["started", "step 1 []", "completed"]);
    let list = c
        .rpc(Request::ListSubagents {
            session_id: sid.clone(),
        })
        .await;
    let id = list[0]["id"].as_str().unwrap().to_string();
    let got = c.rpc(Request::GetSubagent { id }).await;
    let roles: Vec<&str> = got["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant"]);
    assert_eq!(got["subagent"]["steps"], 1);
    server.abort();
}

/// The daemon caches config.toml but picks up edits to it (and says so).
#[cfg(unix)]
#[tokio::test]
async fn config_edits_are_picked_up_and_announced() {
    use formless_protocol::{Event, Request, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4cfg");
    write_cfg(&paths, "http://127.0.0.1:9/v1", 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let before = c.rpc(Request::GetSettings).await;
    assert_eq!(before["provider"]["model"], "mock-1");
    let mut cfg = formless_core::Config::load(&paths).unwrap();
    cfg.provider.model = "mock-two".into();
    cfg.save(&paths).unwrap();
    c.send(Request::GetSettings).await;
    let (mut model, mut announced) = (None, false);
    while model.is_none() || !announced {
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), c.lines.next_line())
            .await
            .expect("settings change not seen")
            .unwrap()
            .unwrap();
        match serde_json::from_str::<ServerMessage>(&line) {
            Ok(ServerMessage::Event(Event::SettingsChanged)) => announced = true,
            Ok(ServerMessage::Response {
                result: Some(r), ..
            }) => model = r["provider"]["model"].as_str().map(str::to_string),
            _ => {}
        }
    }
    assert_eq!(model.as_deref(), Some("mock-two"));
    server.abort();
}

/// Separate Runs reach the provider over one pooled connection (one
/// client in the daemon), not a fresh TCP+TLS handshake each time.
#[cfg(unix)]
#[tokio::test]
async fn provider_connections_are_reused_across_runs() {
    use axum::{
        extract::ConnectInfo,
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4pool");
    let peers = Arc::new(std::sync::Mutex::new(Vec::<u16>::new()));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let peers = peers.clone();
                move |ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>| {
                    peers.lock().unwrap().push(addr.port());
                    async { sse_text("ok") }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let s = c.rpc(Request::CreateSession { name: None }).await;
    let sid = s["id"].as_str().unwrap().to_string();
    for text in ["one", "two", "three"] {
        c.send(Request::SendMessage {
            session_id: sid.clone(),
            content: text.into(),
            mode: SendMode::Send,
        })
        .await;
        c.run_to_finish().await;
    }
    let peers = peers.lock().unwrap().clone();
    assert_eq!(peers.len(), 3);
    assert!(
        peers.iter().all(|p| *p == peers[0]),
        "one pooled connection, got client ports {peers:?}"
    );
    server.abort();
}

/// A client that subscribes to one Session gets no transcript or token
/// traffic for others (global events still arrive); others still get all.
#[cfg(unix)]
#[tokio::test]
async fn subscriptions_scope_live_events_per_client() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4subs");
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route("/chat/completions", post(|| async { sse_text("hello") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut watcher = Client::connect(&paths).await;
    let mut all = Client::connect(&paths).await;
    let one = watcher.rpc(Request::CreateSession { name: None }).await;
    let two = watcher.rpc(Request::CreateSession { name: None }).await;
    let (one, two) = (
        one["id"].as_str().unwrap().to_string(),
        two["id"].as_str().unwrap().to_string(),
    );
    watcher
        .rpc(Request::Subscribe {
            sessions: Some(vec![one.clone()]),
            deltas: true,
            client: Some("test".into()),
        })
        .await;
    all.send(Request::SendMessage {
        session_id: two.clone(),
        content: "hi".into(),
        mode: SendMode::Send,
    })
    .await;
    assert_eq!(
        all.run_to_finish().await,
        "hello",
        "unfiltered client sees deltas"
    );
    // The watcher saw the Run start and finish, but none of its traffic.
    let mut kinds = Vec::new();
    loop {
        let line =
            tokio::time::timeout(std::time::Duration::from_secs(5), watcher.lines.next_line())
                .await
                .expect("timed out")
                .unwrap()
                .unwrap();
        if let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) {
            let done = matches!(ev, Event::RunFinished { .. });
            kinds.push(
                serde_json::to_value(&ev).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
            if done {
                break;
            }
        }
    }
    assert!(kinds.contains(&"run_started".to_string()), "{kinds:?}");
    assert!(
        !kinds
            .iter()
            .any(|k| k == "message_delta" || k == "message_appended" || k == "message_complete"),
        "{kinds:?}"
    );
    server.abort();
}

/// Clients on the same Session see each other, and user messages say
/// which client sent them (SPEC §10.4).
#[cfg(unix)]
#[tokio::test]
async fn clients_on_a_session_see_each_other() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4clients");
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route("/chat/completions", post(|| async { sse_text("hello") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut tui = Client::connect(&paths).await;
    let mut web = Client::connect(&paths).await;
    let id = tui.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let sub = |client: &str| Request::Subscribe {
        sessions: Some(vec![id.clone()]),
        deltas: true,
        client: Some(client.into()),
    };
    let me = tui.rpc(sub("TUI")).await;
    assert!(me["conn"].as_u64().is_some());
    web.rpc(sub("web")).await;
    // Read the TUI's events until one matches.
    async fn until(c: &mut Client, mut f: impl FnMut(&Event) -> bool) -> Event {
        loop {
            let line = tokio::time::timeout(std::time::Duration::from_secs(5), c.lines.next_line())
                .await
                .expect("timed out")
                .unwrap()
                .unwrap();
            if let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) {
                if f(&ev) {
                    return ev;
                }
            }
        }
    }
    let labels = |ev: Event| match ev {
        Event::SessionClients { clients, .. } => {
            let mut l: Vec<String> = clients.into_iter().map(|c| c.label).collect();
            l.sort();
            l
        }
        _ => unreachable!(),
    };
    let ev = until(&mut tui, |e| matches!(e, Event::SessionClients { .. })).await;
    assert_eq!(labels(ev), ["TUI", "web"]);
    web.rpc(Request::SendMessage {
        session_id: id.clone(),
        content: "from the browser".into(),
        mode: SendMode::Send,
    })
    .await;
    let ev = until(
        &mut tui,
        |e| matches!(e, Event::MessageAppended { message, .. } if message.role == "user"),
    )
    .await;
    let Event::MessageAppended { message, .. } = ev else {
        unreachable!()
    };
    let meta: Value = serde_json::from_str(message.meta.as_deref().unwrap()).unwrap();
    assert_eq!(meta["client"]["label"], "web");
    // The web client leaves.
    drop(web);
    let ev = until(
        &mut tui,
        |e| matches!(e, Event::SessionClients { clients, .. } if clients.len() == 1),
    )
    .await;
    assert_eq!(labels(ev), ["TUI"]);
    server.abort();
}

/// Applying learned guidance does not rewrite a running Session's system
/// prompt (its cached prefix); new Sessions get it.
#[cfg(unix)]
#[tokio::test]
async fn sessions_keep_their_system_prompt_when_guidance_changes() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4prompt");
    let systems = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let systems = systems.clone();
                move |Json(body): Json<Value>| {
                    let system = body["messages"][0]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    systems.lock().unwrap().push(system);
                    async { sse_text("ok") }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let send = |sid: String| Request::SendMessage {
        session_id: sid,
        content: "hi".into(),
        mode: SendMode::Send,
    };
    let s1 = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(send(s1.clone())).await;
    c.run_to_finish().await;
    {
        let store = Store::open(&paths.db_file()).unwrap();
        let p = store
            .create_proposal("prompt", "ALWAYS-SAY-PLEASE", "test", 0)
            .unwrap();
        store.update_proposal_eval(&p.id, 0.5, 0.9, 7).unwrap();
        assert!(store
            .set_proposal_status(&p.id, "applied")
            .unwrap()
            .is_some());
    }
    c.send(send(s1.clone())).await;
    c.run_to_finish().await;
    let s2 = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(send(s2)).await;
    c.run_to_finish().await;
    let systems = systems.lock().unwrap().clone();
    assert_eq!(systems.len(), 3);
    assert_eq!(systems[0], systems[1], "running Session keeps its prompt");
    assert!(!systems[1].contains("ALWAYS-SAY-PLEASE"));
    assert!(
        systems[2].contains("ALWAYS-SAY-PLEASE"),
        "new Sessions get it"
    );
    server.abort();
}

/// Model stats carry provider-reported token and cache totals.
#[cfg(unix)]
#[tokio::test]
async fn model_stats_include_token_and_cache_totals() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4cachestats");
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|| async {
                format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices": [{"delta": {"content": "ok"}}]}),
                    json!({"choices": [], "usage": {"prompt_tokens": 200, "completion_tokens": 5,
                        "prompt_tokens_details": {"cached_tokens": 150}}})
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let sid = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    for _ in 0..2 {
        c.send(Request::SendMessage {
            session_id: sid.clone(),
            content: "hi".into(),
            mode: SendMode::Send,
        })
        .await;
        c.run_to_finish().await;
    }
    let stats = c.rpc(Request::ListModelStats { since: None }).await;
    let m = &stats[0];
    assert_eq!(m["model"], "mock-1");
    assert_eq!(
        (
            m["prompt_tokens"].as_i64(),
            m["cached_tokens"].as_i64(),
            m["completion_tokens"].as_i64()
        ),
        (Some(400), Some(300), Some(10))
    );
    server.abort();
}

/// Compaction appends to the earlier summary instead of rewriting it, so
/// the cached prefix through the old summary stays byte-identical.
#[cfg(unix)]
#[tokio::test]
async fn compaction_appends_to_the_previous_summary() {
    use axum::{
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4append");
    let sid = {
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(None).unwrap();
        let first = store.append_message(&s.id, "user", "early").unwrap();
        store
            .save_checkpoint(&s.id, "OLD-SUMMARY", first.seq)
            .unwrap();
        for i in 0..6 {
            store
                .append_message(&s.id, "user", &format!("q{i} {}", "x".repeat(600)))
                .unwrap();
            store
                .append_message(&s.id, "assistant", &"y".repeat(600))
                .unwrap();
        }
        s.id
    };
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                if body["stream"] == json!(false) {
                    // The summariser sees only the new span, not the old summary.
                    let input = body["messages"][1]["content"].as_str().unwrap_or_default();
                    assert!(!input.contains("OLD-SUMMARY"), "{input}");
                    return Json(json!({"choices": [{"message": {"content": "NEW-BLOCK"}}]}))
                        .into_response();
                }
                sse_text("ok").into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 2000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    c.send(Request::SendMessage {
        session_id: sid.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    c.run_to_finish().await;
    let store = Store::open(&paths.db_file()).unwrap();
    let (summary, _) = store.latest_checkpoint(&sid).unwrap().unwrap();
    assert_eq!(summary, "OLD-SUMMARY\n\nNEW-BLOCK");
    server.abort();
}

/// Results that complete together are delivered as one appended note in
/// a fixed order, not one note each in whatever order they raced.
#[cfg(unix)]
#[tokio::test]
async fn pending_results_are_delivered_in_one_ordered_note() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4batch");
    seed_sleeper(&paths);
    let notes = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let notes = notes.clone();
                move |Json(body): Json<Value>| {
                    let notes = notes.clone();
                    async move {
                        let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                        let pushed: Vec<String> = msgs
                            .iter()
                            .filter_map(|m| m["content"].as_str())
                            .filter(|c| c.contains("[Pending invocations completed]"))
                            .map(str::to_string)
                            .collect();
                        if !pushed.is_empty() {
                            *notes.lock().unwrap() = pushed;
                            return sse_text("done");
                        }
                        let tools = msgs.iter().filter(|m| m["role"] == "tool").count();
                        if tools == 0 {
                            let d = json!({"choices": [{"delta": {"tool_calls": [
                                {"index": 0, "id": "a", "function": {"name": "sleeper", "arguments": "{\"ms\":1300}"}},
                                {"index": 1, "id": "b", "function": {"name": "sleeper", "arguments": "{\"ms\":1400}"}}
                            ]}}]});
                            return format!("data: {d}\n\ndata: [DONE]\n\n");
                        }
                        // Both finish while this step is in flight.
                        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
                        sse_tool(&format!("e{tools}"), "echo_fast", json!({"x": 1}))
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 1, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let sid = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(Request::SendMessage {
        session_id: sid,
        content: "two sleeps".into(),
        mode: SendMode::Send,
    })
    .await;
    assert_eq!(c.run_to_finish().await, "done");
    let notes = notes.lock().unwrap().clone();
    assert_eq!(notes.len(), 1, "one note for both: {notes:?}");
    let lines: Vec<&str> = notes[0].lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(lines.len(), 2, "{}", notes[0]);
    // Started first, listed first.
    assert!(
        lines[0].contains("1300") && lines[1].contains("1400"),
        "{}",
        notes[0]
    );
    server.abort();
}

/// The agent can add an MCP server only after the user approves the exact
/// URL and reason; then its tools are in the registry.
#[cfg(unix)]
#[tokio::test]
async fn agent_adds_an_mcp_server_after_approval() {
    use axum::{
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4addmcp");
    // A minimal Streamable HTTP MCP server with one tool.
    let mcp_app = Router::new().route(
        "/mcp",
        post(|Json(msg): Json<Value>| async move {
            if msg.get("id").is_none() {
                return axum::http::StatusCode::ACCEPTED.into_response();
            }
            let result = match msg["method"].as_str().unwrap_or_default() {
                "initialize" => json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
                "tools/list" => json!({"tools": [{"name": "lookup", "description": "Look things up", "inputSchema": {"type": "object"}}]}),
                _ => json!({}),
            };
            Json(json!({"jsonrpc": "2.0", "id": msg["id"], "result": result})).into_response()
        }),
    );
    let mcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mcp_url = format!("http://{}/mcp", mcp_listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(mcp_listener, mcp_app).await.unwrap() });
    let url2 = mcp_url.clone();
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post(move |Json(body): Json<Value>| {
                let url = url2.clone();
                async move {
                    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                    match msgs.iter().find(|m| m["role"] == "tool") {
                        None => sse_tool(
                            "c1",
                            "add_mcp_server",
                            json!({"name": "web", "transport": "http", "url": url, "reason": "to look things up"}),
                        ),
                        Some(t) => sse_text(t["content"].as_str().unwrap_or_default()),
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let sid = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(Request::SendMessage {
        session_id: sid,
        content: "add a lookup server".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut answer = String::new();
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) else {
            continue;
        };
        match ev {
            Event::ApprovalRequested {
                approval_id,
                tool_name,
                detail,
                ..
            } => {
                assert_eq!(tool_name, "add_mcp_server");
                let detail = detail.expect("the modal says exactly what is added");
                assert!(
                    detail.contains(&mcp_url) && detail.contains("to look things up"),
                    "{detail}"
                );
                c.send(Request::ApprovalDecision {
                    remember_denial: Default::default(),
                    paths: Default::default(),
                    approval_id,
                    approved: true,
                    scope: formless_protocol::ApprovalScope::Once,
                })
                .await;
            }
            Event::MessageDelta { delta, .. } => answer.push_str(&delta),
            Event::RunFinished { .. } => break,
            _ => {}
        }
    }
    assert!(answer.contains("\"added\":\"web\""), "{answer}");
    let store = Store::open(&paths.db_file()).unwrap();
    assert!(store
        .get_tool_by_name("mcp__web__lookup")
        .unwrap()
        .is_some());
    assert!(formless_core::Config::load(&paths)
        .unwrap()
        .mcp
        .servers
        .contains_key("web"));
    server.abort();
}

/// Enabled skills are listed in new Sessions' system prompt.
#[cfg(unix)]
#[tokio::test]
async fn enabled_skills_are_indexed_in_the_system_prompt() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4skills");
    let src = paths.data_dir.join("src/tidy");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("SKILL.md"),
        "---\nname: tidy-csv\ndescription: Clean up CSV files\n---\nSteps.",
    )
    .unwrap();
    {
        let store = Store::open(&paths.db_file()).unwrap();
        formless_core::skills::install(&paths, &store, src.to_str().unwrap(), None, None, "user")
            .unwrap();
        store.set_skill_enabled("tidy-csv", true).unwrap();
    }
    let systems = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let systems = systems.clone();
                move |Json(body): Json<Value>| {
                    systems.lock().unwrap().push(
                        body["messages"][0]["content"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    );
                    async { sse_text("ok") }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let sid = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(Request::SendMessage {
        session_id: sid,
        content: "hi".into(),
        mode: SendMode::Send,
    })
    .await;
    c.run_to_finish().await;
    let system = systems.lock().unwrap()[0].clone();
    assert!(
        system.contains("- tidy-csv: Clean up CSV files"),
        "{system}"
    );
    server.abort();
}

/// Opt-in spending limits pause a Run, warn first, and Resume goes past
/// them once (SPEC §5.2).
#[cfg(unix)]
#[tokio::test]
async fn a_step_limit_warns_then_pauses_and_resume_goes_past_it() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4budget");
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let calls = calls.clone();
                move || {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if n < 8 {
                            sse_tool(&format!("c{n}"), "no_such_tool", json!({}))
                        } else {
                            sse_text("done")
                        }
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let mut cfg = formless_core::Config::load(&paths).unwrap();
    cfg.limits.run_steps = Some(5);
    cfg.save(&paths).unwrap();
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let id = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(Request::SendMessage {
        session_id: id.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut warned = false;
    let mut explained = None;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        match serde_json::from_str::<ServerMessage>(&line) {
            Ok(ServerMessage::Event(Event::Note { text, .. })) if text.starts_with("Spending:") => {
                warned = true
            }
            Ok(ServerMessage::Event(Event::RunPaused { explained: e, .. })) => explained = e,
            Ok(ServerMessage::Event(Event::RunFinished { status, .. })) => {
                assert_eq!(status, "paused");
                break;
            }
            _ => {}
        }
    }
    assert!(warned, "a warning at 80%");
    let explained = explained.unwrap();
    assert_eq!(explained.category, "budget");
    assert!(
        explained.message.contains("5 of 5 steps"),
        "{}",
        explained.message
    );
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    // Resume continues past the limit, to the end.
    c.send(Request::ResumeRun {
        session_id: id.clone(),
    })
    .await;
    assert_eq!(c.run_to_finish().await, "done");
    server.abort();
}

/// `/limit N` (SetStepLimit) caps the next Run without any configured
/// limit, pauses it there, and is cleared when that Run ends.
#[cfg(unix)]
#[tokio::test]
async fn a_per_run_step_limit_pauses_and_is_cleared_after() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4limit");
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let calls = calls.clone();
                move || {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if n < 6 {
                            sse_tool(&format!("c{n}"), "no_such_tool", json!({}))
                        } else {
                            sse_text("done")
                        }
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let id = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = c
        .rpc(Request::SetStepLimit {
            session_id: id.clone(),
            max_steps: Some(3),
        })
        .await;
    assert_eq!(r["max_steps"], 3);
    assert_eq!(r["active_run"], false);
    c.send(Request::SendMessage {
        session_id: id.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut explained = None;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        match serde_json::from_str::<ServerMessage>(&line) {
            Ok(ServerMessage::Event(Event::RunPaused { explained: e, .. })) => explained = e,
            Ok(ServerMessage::Event(Event::RunFinished { status, .. })) => {
                assert_eq!(status, "paused");
                break;
            }
            _ => {}
        }
    }
    let explained = explained.unwrap();
    assert_eq!(explained.category, "budget");
    assert!(
        explained.message.contains("3 of the 3 steps set with /limit"),
        "{}",
        explained.message
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    // Resume continues; the limit went with the paused Run.
    c.send(Request::ResumeRun {
        session_id: id.clone(),
    })
    .await;
    assert_eq!(c.run_to_finish().await, "done");
    server.abort();
}

/// `/retry --model X` (RetryRun) re-runs the last message on another
/// model for that Run only, keeping the original answer in a fork.
#[cfg(unix)]
#[tokio::test]
async fn retry_reruns_the_last_message_on_another_model() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Request, SendMode};
    use std::sync::Mutex;
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4retry");
    let models = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}, {"id": "other"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let models = models.clone();
                move |Json(body): Json<Value>| {
                    let m = body["model"].as_str().unwrap_or_default().to_string();
                    models.lock().unwrap().push(m.clone());
                    async move { sse_text(&format!("answer from {m}")) }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let id = c.rpc(Request::CreateSession { name: Some("Q".into()) }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(Request::SendMessage {
        session_id: id.clone(),
        content: "question".into(),
        mode: SendMode::Send,
    })
    .await;
    let first = c.run_to_finish().await;
    c.send(Request::RetryRun {
        session_id: id.clone(),
        model: Some("other".into()),
        keep_original: true,
    })
    .await;
    assert_eq!(c.run_to_finish().await, "answer from other");
    let used = models.lock().unwrap().clone();
    assert_eq!(used.last().map(String::as_str), Some("other"), "{used:?}");
    assert_ne!(used[0], "other");
    // The Session's own model is unchanged: the next Run uses it again.
    c.send(Request::RetryRun {
        session_id: id.clone(),
        model: None,
        keep_original: false,
    })
    .await;
    assert_eq!(c.run_to_finish().await, first);
    // One user message, one answer: the retry replaced, not appended.
    let msgs = c.rpc(Request::GetSession { id: id.clone() }).await["messages"].clone();
    let roles: Vec<&str> = msgs
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant"], "{msgs}");
    // The first retry kept the original answer in a fork.
    let sessions = c.rpc(Request::ListSessions).await;
    assert!(
        sessions
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "Q · original answer"),
        "{sessions}"
    );
    server.abort();
}

/// A Run's file changes are snapshotted, listed with diffs, summed up in
/// the recap, and reverted by `/rewind` (SPEC §5.7).
#[cfg(unix)]
#[tokio::test]
async fn run_file_changes_are_listed_and_rewound() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4files");
    let work = paths.data_dir.parent().unwrap().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work = work.canonicalize().unwrap();
    let w = work.display().to_string();
    std::fs::write(work.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(work.join("c.txt"), "gone soon").unwrap();
    {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[format!("fs:write:{w}"), format!("fs:read:{w}")]).unwrap();
        store
            .create_tool_version(
                "writer",
                "edits files",
                r#"{"type":"object"}"#,
                "{}",
                &format!(
                    "function run(i){{ fs.write('{w}/a.txt', 'one\\nTWO\\n'); fs.write('{w}/b.txt', 'new'); \
                     if (fs.exists('{w}/c.txt')) fs.remove('{w}/c.txt'); return {{ok: true}}; }}"
                ),
                &caps,
                64,
                "agent",
            )
            .unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let calls = calls.clone();
                move || {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        if n < 4 {
                            sse_tool(&format!("c{n}"), "writer", json!({}))
                        } else {
                            sse_text("done")
                        }
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let id = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(Request::SendMessage {
        session_id: id.clone(),
        content: "edit".into(),
        mode: SendMode::Send,
    })
    .await;
    let (mut files, mut recap) = (None, None);
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        match serde_json::from_str::<ServerMessage>(&line) {
            Ok(ServerMessage::Event(Event::ApprovalRequested { approval_id, .. })) => {
                c.send(Request::ApprovalDecision {
                    remember_denial: Default::default(),
                    paths: Default::default(),
                    approval_id,
                    approved: true,
                    scope: formless_protocol::ApprovalScope::Session,
                })
                .await;
            }
            Ok(ServerMessage::Event(Event::RunFilesChanged { files: f, .. })) => files = Some(f),
            Ok(ServerMessage::Event(Event::RunRecap { recap: r, .. })) => recap = Some(r),
            Ok(ServerMessage::Event(Event::RunFinished { status, .. })) => {
                assert_eq!(status, "completed");
                break;
            }
            _ => {}
        }
    }
    let status = |files: &[formless_protocol::RunFileInfo], name: &str| {
        files
            .iter()
            .find(|f| f.path.ends_with(name))
            .map(|f| f.status.clone())
    };
    let files = files.expect("files changed event");
    assert_eq!(status(&files, "a.txt").as_deref(), Some("modified"));
    assert_eq!(status(&files, "b.txt").as_deref(), Some("created"));
    assert_eq!(status(&files, "c.txt").as_deref(), Some("removed"));
    let recap = recap.expect("a recap after 5 steps");
    assert!(recap.steps >= 4, "{recap:?}");
    assert!(recap.used.iter().any(|u| u.starts_with("writer ×")), "{:?}", recap.used);
    assert_eq!(recap.files.len(), 3, "{:?}", recap.files);
    // The diff shows the edit.
    let r = c
        .rpc(Request::RunFiles {
            session_id: id.clone(),
            run_id: None,
        })
        .await;
    let a = r["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"].as_str().unwrap().ends_with("a.txt"))
        .unwrap()
        .clone();
    assert!(a["diff"].as_str().unwrap().contains("- two\n+ TWO"), "{a}");
    // Rewind one file, then the rest.
    let r = c
        .rpc(Request::RewindRun {
            session_id: id.clone(),
            run_id: None,
            path: Some("b.txt".into()),
        })
        .await;
    assert_eq!(r["restored"].as_array().unwrap().len(), 1, "{r}");
    assert!(!work.join("b.txt").exists());
    let r = c
        .rpc(Request::RewindRun {
            session_id: id.clone(),
            run_id: None,
            path: None,
        })
        .await;
    assert_eq!(r["restored"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(std::fs::read_to_string(work.join("a.txt")).unwrap(), "one\ntwo\n");
    assert_eq!(std::fs::read_to_string(work.join("c.txt")).unwrap(), "gone soon");
    server.abort();
}

/// `update_plan` (SPEC §5.8) stores the checklist on the Session,
/// broadcasts it, and rejects malformed steps.
#[cfg(unix)]
#[tokio::test]
async fn update_plan_is_stored_and_broadcast() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4plan");
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let calls = calls.clone();
                move |Json(body): Json<Value>| {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    let last = body["messages"].as_array().unwrap().last().unwrap().clone();
                    async move {
                        match n {
                            0 => sse_tool("p1", "update_plan", json!({"items": [
                                {"content": "Read the files", "status": "completed"},
                                {"content": "Fix the bug", "status": "in_progress"},
                                {"content": "Run the tests", "status": "pending"}]})),
                            1 => {
                                assert!(last["content"].as_str().unwrap().contains("1/3 done"), "{last}");
                                sse_tool("p2", "update_plan", json!({"items": [{"content": "x", "status": "maybe"}]}))
                            }
                            _ => {
                                assert!(last["content"].as_str().unwrap().contains("error"), "{last}");
                                sse_text("done")
                            }
                        }
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let id = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(Request::SendMessage {
        session_id: id.clone(),
        content: "fix it".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut updates = Vec::new();
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        match serde_json::from_str::<ServerMessage>(&line) {
            Ok(ServerMessage::Event(Event::PlanUpdated { items, .. })) => updates.push(items),
            Ok(ServerMessage::Event(Event::RunFinished { status, .. })) => {
                assert_eq!(status, "completed");
                break;
            }
            Ok(ServerMessage::Event(Event::RunPaused { error, .. })) => panic!("{error}"),
            _ => {}
        }
    }
    assert_eq!(updates.len(), 1, "the invalid plan is not broadcast");
    assert_eq!(updates[0][1].status, "in_progress");
    let plan = c.rpc(Request::GetSession { id: id.clone() }).await["plan"].clone();
    assert_eq!(plan.as_array().unwrap().len(), 3, "{plan}");
    server.abort();
}

/// Step mode (SPEC §5.9): each call waits; an edit changes its
/// arguments, a skip tells the model it did not run.
#[cfg(unix)]
#[tokio::test]
async fn step_mode_pauses_edits_and_skips_calls() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    use std::sync::atomic::{AtomicUsize, Ordering};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4step");
    seed_sleeper(&paths);
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route(
            "/chat/completions",
            post({
                let calls = calls.clone();
                move || {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        match n {
                            0 => sse_tool("s1", "echo_fast", json!({"x": 1})),
                            1 => sse_tool("s2", "echo_fast", json!({"x": 2})),
                            _ => sse_text("done"),
                        }
                    }
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let id = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.rpc(Request::SetStepMode {
        session_id: id.clone(),
        on: true,
    })
    .await;
    c.send(Request::SendMessage {
        session_id: id.clone(),
        content: "go".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut steps = 0;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        match serde_json::from_str::<ServerMessage>(&line) {
            Ok(ServerMessage::Event(Event::StepPending { step_id, tool_name, .. })) => {
                assert_eq!(tool_name, "echo_fast");
                steps += 1;
                let (action, arguments) = if steps == 1 {
                    ("edit", Some(r#"{"x": 5}"#.to_string()))
                } else {
                    ("skip", None)
                };
                c.send(Request::StepDecision {
                    step_id,
                    action: action.into(),
                    arguments,
                })
                .await;
            }
            Ok(ServerMessage::Event(Event::RunFinished { status, .. })) => {
                assert_eq!(status, "completed");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(steps, 2);
    let msgs = c.rpc(Request::GetSession { id: id.clone() }).await["messages"].clone();
    let tool: Vec<&str> = msgs
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert!(tool[0].contains('5'), "edited arguments ran: {tool:?}");
    assert!(tool[1].contains("skipped"), "{tool:?}");
    // The stored call carries the edited arguments too.
    let call = msgs
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant" && m["meta"].as_str().is_some_and(|x| x.contains("s1")))
        .unwrap()
        .clone();
    assert!(call["meta"].as_str().unwrap().contains(r#"{\"x\": 5}"#), "{call}");
    server.abort();
}

/// `/compare A B` (SPEC §5.10): the same prompt on each model in its own
/// fork; the answers come back together to the original Session.
#[cfg(unix)]
#[tokio::test]
async fn compare_runs_each_model_in_a_fork_and_reports_both() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("m4cmp");
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "alpha"}, {"id": "beta"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                let m = body["model"].as_str().unwrap_or_default().to_string();
                sse_text(&format!("answer from {m}"))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let id = c.rpc(Request::CreateSession { name: Some("Q".into()) }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bad = c
        .rpc_result(Request::CompareModels {
            session_id: id.clone(),
            models: vec!["alpha".into()],
            prompt: Some("hi".into()),
        })
        .await;
    assert!(bad.is_err(), "needs two models");
    c.send(Request::CompareModels {
        session_id: id.clone(),
        models: vec!["alpha".into(), "beta".into()],
        prompt: Some("which is best?".into()),
    })
    .await;
    let results = loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        if let Ok(ServerMessage::Event(Event::ComparisonReady { session_id, results, .. })) =
            serde_json::from_str::<ServerMessage>(&line)
        {
            assert_eq!(session_id, id);
            break results;
        }
    };
    assert_eq!(results.len(), 2);
    let get = |m: &str| results.iter().find(|r| r.model == m).unwrap().clone();
    assert_eq!(get("alpha").answer, "answer from alpha");
    assert_eq!(get("beta").answer, "answer from beta");
    assert_ne!(get("alpha").session_id, id, "each in its own fork");
    // The original Session is untouched.
    let msgs = c.rpc(Request::GetSession { id: id.clone() }).await["messages"].clone();
    assert_eq!(msgs.as_array().unwrap().len(), 0, "{msgs}");
    server.abort();
}

/// A Run that panics frees its Session (#607): the Run is left paused with
/// the reason, and the next message runs normally without a restart.
#[cfg(unix)]
#[tokio::test]
async fn a_panicking_run_frees_its_session() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    use formless_protocol::{Event, Request, SendMode, ServerMessage};
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    std::env::set_var("FORMLESS_TEST_PANIC_MARKER", "panic-now-7c1");
    let paths = temp_paths("m4panic");
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }),
        )
        .route("/chat/completions", post(|| async { sse_text("fine") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    write_cfg(&paths, &base, 30, false, 128_000);
    let server = start_daemon(&paths).await;
    let mut c = Client::connect(&paths).await;
    let id = c.rpc(Request::CreateSession { name: None }).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    c.send(Request::SendMessage {
        session_id: id.clone(),
        content: "please panic-now-7c1".into(),
        mode: SendMode::Send,
    })
    .await;
    let mut paused = None;
    loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(30), c.lines.next_line())
            .await
            .expect("timed out")
            .unwrap()
            .unwrap();
        match serde_json::from_str::<ServerMessage>(&line) {
            Ok(ServerMessage::Event(Event::RunPaused { error, .. })) => paused = Some(error),
            Ok(ServerMessage::Event(Event::RunFinished { status, .. })) => {
                assert_eq!(status, "paused");
                break;
            }
            _ => {}
        }
    }
    assert!(paused.unwrap().contains("internal error"));
    c.send(Request::SendMessage {
        session_id: id.clone(),
        content: "hello again".into(),
        mode: SendMode::Send,
    })
    .await;
    assert_eq!(c.run_to_finish().await, "fine", "the Session works again");
    server.abort();
}
