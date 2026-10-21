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
        store
            .record_invocation(
                &tool_id, 1, None, None, None, "success", "", "{}", "{}", "", 1, 200,
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
    assert!(
        bundle.get("success_rate").is_none() && bundle.get("invocations").is_none(),
        "bundle must carry no metrics: {bundle}"
    );
    let sbundle = a.rpc(Request::ExportSession { id: session_id }).await;
    server_a.abort();

    let server_b = start_daemon(&paths_b).await;
    let mut b = Client::connect(&paths_b).await;
    let imported = b
        .rpc(Request::ImportTool {
            bundle: bundle.clone(),
        })
        .await;
    assert_eq!(imported["name"], "porter");
    assert_eq!(
        imported["invocations"], 0,
        "imported tool metrics must start fresh"
    );
    // Collision: second import becomes a new tool, never a version overwrite.
    let imported2 = b.rpc(Request::ImportTool { bundle }).await;
    assert_eq!(imported2["name"], "porter_imported");
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
