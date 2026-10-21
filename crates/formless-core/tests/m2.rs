//! M2 acceptance tests (SPEC §18 M2): sandbox capability escapes, registry
//! lifecycle + metrics exactness, daemon builds & uses a Tool end-to-end.

use formless_core::storage::Store;
use formless_core::Paths;
use formless_runtime::{execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, NoHostOps};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn run_js(
    code: &str,
    input: Value,
    caps: &[&str],
    memory_mb: u32,
) -> Result<Value, formless_runtime::ExecError> {
    let req = ExecRequest {
        code: code.to_string(),
        input,
        caps: Capabilities::parse(&caps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
        memory_mb,
        mode: HostMode::Live,
        overlay: None,
        activity: None,
        modules: Default::default(),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    execute(&req, Arc::new(NoHostOps)).map(|o| o.output)
}

#[test]
fn sandbox_executes_and_enforces_capabilities() {
    // Happy path.
    let out = run_js(
        "function run(i){ return {echo: i.x}; }",
        json!({"x": 7}),
        &[],
        256,
    )
    .unwrap();
    assert_eq!(out["echo"], 7);

    // FS escape: granted one dir, reading elsewhere is denied at the host layer.
    let dir = format!("/tmp/fl-m2-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(format!("{dir}/ok.txt"), "inside").unwrap();
    let ok = run_js(
        &format!("function run(i){{ return fs.read('{dir}/ok.txt'); }}"),
        json!({}),
        &[&format!("fs:read:{dir}")],
        256,
    )
    .unwrap();
    assert_eq!(ok, "inside");
    let err = run_js(
        "function run(i){ return fs.read('/etc/hosts'); }",
        json!({}),
        &[&format!("fs:read:{dir}")],
        256,
    )
    .unwrap_err();
    assert_eq!(
        err.kind,
        ExecErrorKind::Denied,
        "escape must be denied: {}",
        err.message
    );

    // Symlink escape is also denied (canonical containment, SPEC §14).
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hosts", format!("{dir}/link")).unwrap();
        let err = run_js(
            &format!("function run(i){{ return fs.read('{dir}/link'); }}"),
            json!({}),
            &[&format!("fs:read:{dir}")],
            256,
        )
        .unwrap_err();
        assert_eq!(
            err.kind,
            ExecErrorKind::Denied,
            "symlink escape: {}",
            err.message
        );
    }

    // Env leak: undeclared key denied; declared key visible.
    let err = run_js(
        "function run(i){ return env.get('PATH'); }",
        json!({}),
        &[],
        256,
    )
    .unwrap_err();
    assert_eq!(err.kind, ExecErrorKind::Denied);
    let path = run_js(
        "function run(i){ return env.get('PATH'); }",
        json!({}),
        &["env:PATH"],
        256,
    )
    .unwrap();
    assert!(!path.as_str().unwrap().is_empty());

    // Process: scoped command allowed, other command denied, no-shell semantics.
    let out = run_js(
        "function run(i){ const r = process.run('echo', ['hi']); return r.stdout.trim(); }",
        json!({}),
        &["process:echo"],
        256,
    )
    .unwrap();
    assert_eq!(out, "hi");
    let err = run_js(
        "function run(i){ return process.run('ls', ['/']).stdout; }",
        json!({}),
        &["process:echo"],
        256,
    )
    .unwrap_err();
    assert_eq!(err.kind, ExecErrorKind::Denied);
    let err = run_js(
        "function run(i){ return process.run('echo', ['x']).stdout; }",
        json!({}),
        &[],
        256,
    )
    .unwrap_err();
    assert_eq!(err.kind, ExecErrorKind::Denied, "undeclared process denied");

    // Net: undeclared denied before any connection.
    let err = run_js(
        "function run(i){ return fetch('http://127.0.0.1:9/').status; }",
        json!({}),
        &[],
        256,
    )
    .unwrap_err();
    assert_eq!(err.kind, ExecErrorKind::Denied);

    // Streaming reads process data chunk-wise in bounded heap (§7.5).
    let big = "abcd".repeat(40_000);
    std::fs::write(format!("{dir}/big.txt"), &big).unwrap();
    let out = run_js(
        &format!("function run(i){{ let n=0; fs.readChunks('{dir}/big.txt', 1024, c => {{ n += c.length; }}); return n; }}"),
        json!({}),
        &[&format!("fs:read:{dir}")],
        16,
    )
    .unwrap();
    assert_eq!(out, big.len());

    // Memory cap faults only the invocation, tagged resource (§7.5).
    let err = run_js(
        "function run(i){ let a=[]; while(true){ a.push('x'.repeat(100000)); } }",
        json!({}),
        &[],
        16,
    )
    .unwrap_err();
    assert_eq!(
        err.kind,
        ExecErrorKind::Resource,
        "OOM must be resource-tagged: {}",
        err.message
    );
}

#[test]
fn registry_lifecycle_and_metrics_exact() {
    let store = Store::open_in_memory().unwrap();
    let caps = Capabilities::parse(&["fs:read:/tmp".to_string()]).unwrap();
    let (_info, v) = store
        .create_tool_version(
            "echo",
            "v1 desc",
            r#"{"type":"object"}"#,
            r#"{"type":"object"}"#,
            "function run(i){return i;}",
            &caps,
            256,
            "agent",
        )
        .unwrap();
    assert_eq!(v, 1);
    // Same name creates a new version (collision rule, SPEC §7.2).
    let (info, v) = store
        .create_tool_version(
            "echo",
            "v2 desc",
            r#"{"type":"object"}"#,
            r#"{"type":"object"}"#,
            "function run(i){return {} ;}",
            &caps,
            256,
            "agent",
        )
        .unwrap();
    assert_eq!(v, 2);
    assert_eq!(info.current_version, 2);
    // Record invocations; metrics must match exactly.
    for (status, tag, ms) in [
        ("success", "", 10),
        ("success", "", 30),
        ("failure", "", 5),
        ("failure", "validation", 5),
        ("denied", "approval", 0),
    ] {
        store
            .record_invocation(
                &info.id, 2, None, None, None, status, tag, "{}", "{}", "", ms, 200,
            )
            .unwrap();
    }
    let m = store.tool_metrics(&info.id, None).unwrap();
    assert_eq!(
        (
            m.total,
            m.success,
            m.failure,
            m.denied,
            m.validation_failures
        ),
        (5, 2, 2, 1, 1)
    );
    assert_eq!(m.success_rate, Some(0.5));
    assert_eq!(m.p50_ms, Some(30.0)); // success durations [10, 30], p50 rounds to index 1
                                      // Rollback to v1.
    let info = store.rollback_tool(&info.id, 1).unwrap().unwrap();
    assert_eq!(info.current_version, 1);
    // Retire blocks re-creation by name until restore.
    store.set_tool_retired(&info.id, true).unwrap();
    assert!(store
        .create_tool_version(
            "echo",
            "x",
            "{}",
            "{}",
            "function run(i){}",
            &caps,
            256,
            "agent"
        )
        .is_err());
    store.set_tool_retired(&info.id, false).unwrap();
    let (_i, v3) = store
        .create_tool_version(
            "echo",
            "v3",
            "{}",
            "{}",
            "function run(i){}",
            &caps,
            256,
            "agent",
        )
        .unwrap();
    assert_eq!(v3, 3);
    // Approval coverage: approved fs:read:/tmp covers fs:read:/tmp/sub only.
    store.set_approval(&info.id, &caps, 256).unwrap();
    let (approved, mem) = store.get_approval(&info.id).unwrap().unwrap();
    assert_eq!(mem, 256);
    assert!(approved.covers(&Capabilities::parse(&["fs:read:/tmp/sub".into()]).unwrap()));
    assert!(!approved.covers(&Capabilities::parse(&["net".into()]).unwrap()));
}

// --- Daemon end-to-end: agent builds a Tool, approval gates it, then uses it. ---

fn sse_tool_call(id: &str, name: &str, args: Value) -> String {
    let delta = json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": id, "function": {"name": name, "arguments": args.to_string()}}]}}]});
    format!("data: {delta}\n\ndata: [DONE]\n\n")
}
fn sse_text(text: &str) -> String {
    let delta = json!({"choices": [{"delta": {"content": text}}]});
    format!("data: {delta}\n\ndata: [DONE]\n\n")
}

async fn mock_m2_server(workdir: String) -> String {
    use axum::{
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    let app = Router::new()
        .route("/models", get(|| async { Json(json!({"data": [{"id": "mock-1"}]})) }))
        .route("/chat/completions", post(move |Json(body): Json<Value>| {
            let workdir = workdir.clone();
            async move {
                if body["tool_choice"].as_str() == Some("required") {
                    return Json(json!({"choices": [{"message": {"tool_calls": [{"id": "p", "type": "function", "function": {"name": "probe_ping", "arguments": "{}"}}]}}]})).into_response();
                }
                let msgs = body["messages"].as_array().cloned().unwrap_or_default();
                let tool_msgs: Vec<&Value> = msgs.iter().filter(|m| m["role"] == "tool").collect();
                let sse = match tool_msgs.len() {
                    0 => {
                        let code = format!(
                            "function run(input) {{ fs.write('{workdir}/out.txt', input.text); return {{ written: fs.read('{workdir}/out.txt') }}; }}"
                        );
                        sse_tool_call("c1", "create_tool", json!({
                            "name": "echo_write",
                            "description": "Writes text to the workdir file and reads it back.",
                            "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
                            "output_schema": {"type": "object", "properties": {"written": {"type": "string"}}, "required": ["written"]},
                            "implementation_js": code,
                            "capabilities": [format!("fs:write:{workdir}"), format!("fs:read:{workdir}")]
                        }))
                    }
                    1 => sse_tool_call("c2", "echo_write", json!({"text": "hello-m2"})),
                    _ => {
                        let last = tool_msgs.last().map(|m| m["content"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
                        sse_text(&format!("done: {last}"))
                    }
                };
                ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], sse).into_response()
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

/// Runs the scripted build+use flow once against a fresh daemon; the client
/// answers approvals with `approve`. Returns (final assistant text, paths).
#[cfg(unix)]
async fn scripted_flow(
    tag: &str,
    approve: bool,
    scope: formless_protocol::ApprovalScope,
) -> (String, Paths, String) {
    use formless_core::daemon::Daemon;
    use formless_core::Config;
    use formless_protocol::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let workdir = format!("/tmp/fl-{tag}w-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    std::fs::create_dir_all(&workdir).unwrap();
    let base = mock_m2_server(workdir.clone()).await;
    let paths = temp_paths(tag);
    let p = formless_core::config::ProviderCfg {
        base_url: base,
        model: "mock-1".into(),
        max_retries: 1,
        ..Default::default()
    };
    let mut cfg = Config {
        provider: p,
        ..Default::default()
    };
    cfg.daemon.idle_grace_secs = 3600;
    cfg.save(&paths).unwrap();

    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut lines = BufReader::new(read_half).lines();
    // Create session.
    let env = RequestEnvelope {
        id: 1,
        protocol: PROTOCOL_VERSION,
        request: Request::CreateSession {
            name: Some("M2".into()),
        },
    };
    write_half
        .write_all(serde_json::to_string(&env).unwrap().as_bytes())
        .await
        .unwrap();
    write_half.write_all(b"\n").await.unwrap();
    let session_id = loop {
        let line = lines.next_line().await.unwrap().unwrap();
        if let Ok(ServerMessage::Response { id: 1, result, .. }) =
            serde_json::from_str::<ServerMessage>(&line)
        {
            break result.unwrap()["id"].as_str().unwrap().to_string();
        }
    };
    let env = RequestEnvelope {
        id: 2,
        protocol: PROTOCOL_VERSION,
        request: Request::SendMessage {
            session_id: session_id.clone(),
            content: "write hello".into(),
            mode: SendMode::Send,
        },
    };
    write_half
        .write_all(serde_json::to_string(&env).unwrap().as_bytes())
        .await
        .unwrap();
    write_half.write_all(b"\n").await.unwrap();

    let mut final_text = String::new();
    let mut next_id = 100u64;
    while let Some(line) = lines.next_line().await.unwrap() {
        if let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) {
            match ev {
                Event::MessageDelta { delta, .. } => final_text.push_str(&delta),
                Event::ApprovalRequested { approval_id, .. } => {
                    next_id += 1;
                    let env = RequestEnvelope {
                        id: next_id,
                        protocol: PROTOCOL_VERSION,
                        request: Request::ApprovalDecision {
                            approval_id,
                            approved: approve,
                            scope,
                        },
                    };
                    write_half
                        .write_all(serde_json::to_string(&env).unwrap().as_bytes())
                        .await
                        .unwrap();
                    write_half.write_all(b"\n").await.unwrap();
                }
                Event::RunFinished { status, .. } => {
                    assert_eq!(status, "completed");
                    break;
                }
                Event::RunPaused { error, .. } => panic!("run paused: {error}"),
                _ => {}
            }
        }
    }
    server.abort();
    (final_text, paths, workdir)
}

#[cfg(unix)]
#[tokio::test]
async fn daemon_builds_and_uses_tool_with_approval() {
    let (final_text, paths, workdir) =
        scripted_flow("m2a", true, formless_protocol::ApprovalScope::Always).await;
    assert!(final_text.contains("hello-m2"), "final text: {final_text}");
    assert_eq!(
        std::fs::read_to_string(format!("{workdir}/out.txt")).unwrap(),
        "hello-m2"
    );
    let store = Store::open(&paths.db_file()).unwrap();
    let tools = store.list_tools(true).unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo_write");
    assert_eq!(tools[0].invocations, 1);
    assert_eq!(tools[0].success_rate, Some(1.0));
    // Reuse across Sessions: registry persists in the same data dir.
    let (info, ver) = store.get_tool_by_name("echo_write").unwrap().unwrap();
    assert_eq!(info.current_version, 1);
    assert!(ver.code.contains("fs.write"));
}

#[cfg(unix)]
#[tokio::test]
async fn approve_once_runs_the_tool_without_storing_a_grant() {
    let (final_text, paths, workdir) =
        scripted_flow("m2o", true, formless_protocol::ApprovalScope::Once).await;
    assert!(final_text.contains("hello-m2"), "final text: {final_text}");
    assert!(std::path::Path::new(&format!("{workdir}/out.txt")).exists());
    let store = Store::open(&paths.db_file()).unwrap();
    let (info, _) = store.get_tool_by_name("echo_write").unwrap().unwrap();
    assert!(
        store.get_approval(&info.id).unwrap().is_none(),
        "a once-approval must not persist"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn denied_approval_blocks_execution_and_counts_denied() {
    let (final_text, paths, workdir) =
        scripted_flow("m2d", false, formless_protocol::ApprovalScope::Always).await;
    assert!(
        final_text.to_lowercase().contains("denied"),
        "final text: {final_text}"
    );
    assert!(
        !std::path::Path::new(&format!("{workdir}/out.txt")).exists(),
        "denied tool must not run"
    );
    let store = Store::open(&paths.db_file()).unwrap();
    let tools = store.list_tools(true).unwrap();
    let m = store.tool_metrics(&tools[0].id, None).unwrap();
    assert_eq!((m.denied, m.success), (1, 0));
}
