//! M3 acceptance tests (SPEC §18 M3): replay isolation, promotion bar,
//! widening approval, give-up, rollback.

#[path = "support/env.rs"]
mod credential_env;

use formless_core::storage::Store;
use formless_core::Paths;
use formless_runtime::{execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, NoHostOps};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn run_mode(
    code: &str,
    input: Value,
    caps: &[&str],
    mode: HostMode,
) -> Result<Value, formless_runtime::ExecError> {
    let req = ExecRequest {
        output_schema: None,
        code: code.to_string(),
        input,
        caps: Capabilities::parse(&caps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
        memory_mb: 256,
        mode,
        overlay: None,
        activity: None,
        modules: Default::default(),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    execute(&req, Arc::new(NoHostOps)).map(|o| o.output)
}

#[test]
fn replay_mode_touches_no_real_files_network_or_secrets() {
    let _env = credential_env::EnvGuard::isolated();
    let dir = format!("/tmp/fl-m3-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    std::fs::create_dir_all(&dir).unwrap();
    // Write-then-read works inside the overlay, nothing lands on disk.
    let out = run_mode(
        &format!("function run(i){{ fs.write('{dir}/proof.txt', 'replayed'); return fs.read('{dir}/proof.txt'); }}"),
        json!({}),
        &[&format!("fs:write:{dir}"), &format!("fs:read:{dir}")],
        HostMode::Replay,
    )
    .unwrap();
    assert_eq!(out, "replayed");
    assert!(
        !std::path::Path::new(&format!("{dir}/proof.txt")).exists(),
        "replay write leaked to real fs"
    );

    // Network and process are denied outright in replay (§8.4).
    let err = run_mode(
        "function run(i){ return fetch('http://127.0.0.1:9/').status; }",
        json!({}),
        &["net"],
        HostMode::Replay,
    )
    .unwrap_err();
    assert_eq!(err.kind, ExecErrorKind::Denied);
    assert!(
        err.message.contains("net denied in replay"),
        "{}",
        err.message
    );
    let err = run_mode(
        "function run(i){ return process.run('echo', ['x']).stdout; }",
        json!({}),
        &["process:echo"],
        HostMode::Replay,
    )
    .unwrap_err();
    assert_eq!(err.kind, ExecErrorKind::Denied);
    assert!(
        err.message.contains("process denied in replay"),
        "{}",
        err.message
    );

    // No real secrets: declared env reads empty in replay.
    let out = run_mode(
        "function run(i){ return env.get('PATH'); }",
        json!({}),
        &["env:PATH"],
        HostMode::Replay,
    )
    .unwrap();
    assert_eq!(out, "");
}

#[test]
fn trigger_window_and_replay_set_sampling() {
    let _env = credential_env::EnvGuard::isolated();
    let store = Store::open_in_memory().unwrap();
    let caps = Capabilities::default();
    let (info, _v) = store
        .create_tool_version(
            "w",
            "d",
            r#"{"type":"object"}"#,
            r#"{"type":"object"}"#,
            "function run(i){return {};}",
            &caps,
            256,
            "agent",
        )
        .unwrap();
    // 12 failures + 12 successes -> rate 0.5 over window 100, sample 24.
    for _ in 0..12 {
        store
            .record_invocation(
                &info.id,
                1,
                None,
                None,
                None,
                "failure",
                "",
                r#"{"x":-1}"#,
                "",
                "neg",
                1,
                200,
            )
            .unwrap();
    }
    for _ in 0..12 {
        store
            .record_invocation(
                &info.id,
                1,
                None,
                None,
                None,
                "success",
                "",
                r#"{"x":1}"#,
                "{}",
                "",
                1,
                200,
            )
            .unwrap();
    }
    let (sample, rate) = store.windowed_success(&info.id, 100).unwrap();
    assert_eq!(sample, 24);
    assert_eq!(rate, Some(0.5));
    // Window of last 10 is all successes (recorded last).
    let (sample, rate) = store.windowed_success(&info.id, 10).unwrap();
    assert_eq!((sample, rate), (10, Some(1.0)));
    // Replay set: all failures + equal-sized success sample.
    let set = store.replay_set(&info.id).unwrap();
    assert_eq!(set.len(), 24);
    assert_eq!(set.iter().filter(|r| r.recorded_success).count(), 12);
    assert_eq!(set.iter().filter(|r| !r.recorded_success).count(), 12);
}

// --- Daemon optimiser end-to-end with a scripted candidate provider. ---

const BROKEN: &str =
    "function run(i){ if(i.x < 0) throw new Error('negative'); return {result: i.x}; }";
const IN_SCHEMA: &str =
    r#"{"type":"object","properties":{"x":{"type":"integer"}},"required":["x"]}"#;
const OUT_SCHEMA: &str =
    r#"{"type":"object","properties":{"result":{"type":"integer"}},"required":["result"]}"#;

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

fn seed_broken(paths: &Paths, name: &str, workdir: &str) -> String {
    let store = Store::open(&paths.db_file()).unwrap();
    let caps = Capabilities::parse(&[format!("fs:write:{workdir}")]).unwrap();
    let (info, _v) = store
        .create_tool_version(
            name,
            "broken test tool",
            IN_SCHEMA,
            OUT_SCHEMA,
            BROKEN,
            &caps,
            256,
            "agent",
        )
        .unwrap();
    for _ in 0..12 {
        store
            .record_invocation(
                &info.id,
                1,
                None,
                None,
                None,
                "failure",
                "",
                r#"{"x":-3}"#,
                "",
                "negative",
                1,
                200,
            )
            .unwrap();
    }
    for _ in 0..12 {
        store
            .record_invocation(
                &info.id,
                1,
                None,
                None,
                None,
                "success",
                "",
                r#"{"x":4}"#,
                r#"{"result":4}"#,
                "",
                1,
                200,
            )
            .unwrap();
    }
    info.id
}

async fn mock_optimiser_server(workdir: String) -> String {
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
                let prompt = body["messages"].as_array().map(|m| m.iter().map(|x| x["content"].as_str().unwrap_or_default()).collect::<String>()).unwrap_or_default();
                assert_eq!(body["response_format"]["type"], "json_schema", "real optimiser generation must use schema mode");
                assert_eq!(body["response_format"]["json_schema"]["schema"]["properties"]["implementation_js"]["type"], "string");
                assert_eq!(body["response_format"]["json_schema"]["strict"], false);
                let cand = |code: String, caps: Value| {
                    json!({
                        "description": "candidate",
                        "input_schema": serde_json::from_str::<Value>(IN_SCHEMA).unwrap(),
                        "output_schema": serde_json::from_str::<Value>(OUT_SCHEMA).unwrap(),
                        "implementation_js": code,
                        "capabilities": caps,
                        "memory_mb": 256
                    })
                };
                let candidate = if prompt.contains("`widener`") {
                    cand("function run(i){ return {result: Math.abs(i.x)}; }".to_string(), json!([format!("fs:write:{workdir}"), "net"]))
                } else if prompt.contains("`hopeless`") {
                    cand(BROKEN.to_string(), json!([format!("fs:write:{workdir}")]))
                } else {
                    cand(
                        format!("function run(i){{ fs.write('{workdir}/replay-proof.txt', 'x'); return {{result: Math.abs(i.x)}}; }}"),
                        json!([format!("fs:write:{workdir}")]),
                    )
                };
                Json(json!({"choices": [{"message": {"content": candidate.to_string()}}]})).into_response()
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

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
            next: 1,
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
    /// Collect events until `pred` matches or timeout. Returns matched event text dump.
    async fn until(&mut self, what: &str, secs: u64) -> String {
        use formless_protocol::{Event, ServerMessage};
        let mut seen = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
        while tokio::time::Instant::now() < deadline {
            let next = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                self.lines.next_line(),
            )
            .await;
            let Ok(Ok(Some(line))) = next else { continue };
            seen.push_str(&line);
            seen.push('\n');
            if let Ok(ServerMessage::Event(ev)) = serde_json::from_str::<ServerMessage>(&line) {
                let hit = match &ev {
                    Event::OptimiserActivity { text, .. } => text.contains(what),
                    Event::PromotionApprovalRequested { .. } => what == "promotion_approval",
                    Event::ToolUpdated { tool } => {
                        what == "tool_updated" && tool.current_version >= 2
                    }
                    _ => false,
                };
                if hit {
                    return seen;
                }
            }
        }
        panic!("timed out waiting for '{what}'; saw:\n{seen}");
    }
    async fn promotion_id(&mut self, seen: &str) -> String {
        let _ = self;
        for line in seen.lines() {
            if let Ok(formless_protocol::ServerMessage::Event(
                formless_protocol::Event::PromotionApprovalRequested { promotion_id, .. },
            )) = serde_json::from_str::<formless_protocol::ServerMessage>(line)
            {
                return promotion_id;
            }
        }
        panic!("no promotion_id in events");
    }
}

#[cfg(unix)]
async fn start_daemon_m3(
    _env: &credential_env::EnvGuard,
    tag: &str,
    tool_name: &str,
) -> (
    Paths,
    String,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    use formless_core::daemon::Daemon;
    use formless_core::Config;
    _env.set("FORMLESS_API_KEY", "test-key");
    let workdir = format!("/tmp/fl-{tag}w-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    std::fs::create_dir_all(&workdir).unwrap();
    let base = mock_optimiser_server(workdir.clone()).await;
    let paths = temp_paths(tag);
    let tool_id = seed_broken(&paths, tool_name, &workdir);
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
    cfg.security.secure = true;
    cfg.optimiser.cooldown_secs = 0;
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
    (paths, tool_id, workdir, server)
}

#[cfg(unix)]
#[tokio::test]
async fn optimiser_promotes_only_on_replay_win_and_rollback_restores() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_protocol::Request;
    let (paths, tool_id, workdir, server) = start_daemon_m3(&_env, "m3p", "halver").await;
    let mut client = Client::connect(&paths).await;
    client
        .send(Request::RunOptimiser {
            tool_id: Some(tool_id.clone()),
        })
        .await;
    client.until("promoted candidate", 30).await;
    server.abort();

    let store = Store::open(&paths.db_file()).unwrap();
    let (info, ver) = store.get_tool_by_name("halver").unwrap().unwrap();
    assert_eq!(info.current_version, 2);
    assert_eq!(ver.author, "optimiser");
    assert!(ver.code.contains("Math.abs"));
    let attempts = store.list_attempts(&tool_id, 10).unwrap();
    assert_eq!(attempts[0].status, "promoted");
    assert_eq!(attempts[0].candidate_score, Some(1.0));
    assert_eq!(attempts[0].baseline_score, Some(0.5));
    assert_eq!(attempts[0].regressions, 0);
    // Candidate wrote a file during replay only -> real fs proof absent.
    assert!(!std::path::Path::new(&format!("{workdir}/replay-proof.txt")).exists());
    // create_tool is not a registry tool and was never optimised.
    assert!(store.get_tool_by_name("create_tool").unwrap().is_none());

    // Rollback restores v1 behaviour (throws on negatives again).
    let info = store.rollback_tool(&tool_id, 1).unwrap().unwrap();
    assert_eq!(info.current_version, 1);
    let (_i, v1) = store.get_tool_current(&tool_id).unwrap().unwrap();
    let err = run_mode(
        &v1.code,
        json!({"x": -3}),
        &[&format!("fs:write:{workdir}")],
        HostMode::Live,
    )
    .unwrap_err();
    assert!(err.message.contains("negative"), "{}", err.message);
}

#[cfg(unix)]
#[tokio::test]
async fn widening_candidate_requires_approval_and_deny_blocks() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_protocol::Request;
    let (paths, tool_id, _workdir, server) = start_daemon_m3(&_env, "m3w", "widener").await;
    let mut client = Client::connect(&paths).await;
    client
        .send(Request::RunOptimiser {
            tool_id: Some(tool_id.clone()),
        })
        .await;
    let seen = client.until("promotion_approval", 30).await;
    let pid = client.promotion_id(&seen).await;
    client
        .send(Request::PromotionDecision {
            promotion_id: pid,
            approved: false,
        })
        .await;
    client.until("candidate discarded", 30).await;
    server.abort();
    let store = Store::open(&paths.db_file()).unwrap();
    let (info, _v) = store.get_tool_by_name("widener").unwrap().unwrap();
    assert_eq!(info.current_version, 1, "denied widening must not promote");
}

#[cfg(unix)]
#[tokio::test]
async fn three_failed_candidates_flag_needs_attention_and_stop() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_protocol::Request;
    let (paths, tool_id, _workdir, server) = start_daemon_m3(&_env, "m3h", "hopeless").await;
    let mut client = Client::connect(&paths).await;
    for _ in 0..3 {
        client
            .send(Request::RunOptimiser {
                tool_id: Some(tool_id.clone()),
            })
            .await;
        client.until("candidate discarded", 30).await;
    }
    client.until("needs attention", 10).await;
    // Fourth run: needs-attention guardrail stops new attempts.
    client
        .send(Request::RunOptimiser {
            tool_id: Some(tool_id.clone()),
        })
        .await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    server.abort();
    let store = Store::open(&paths.db_file()).unwrap();
    let (info, _v) = store.get_tool_by_name("hopeless").unwrap().unwrap();
    assert!(info.needs_attention);
    assert_eq!(info.current_version, 1);
    assert_eq!(store.list_attempts(&tool_id, 50).unwrap().len(), 3);
}

#[cfg(unix)]
#[test]
fn process_children_do_not_inherit_daemon_secrets() {
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FORMLESS_TEST_SECRET", "leak-1");
    _env.set("FL_M3_OTHER_SECRET", "leak-2");
    _env.set("FL_M3_GRANTED", "visible");
    let out = run_mode(
        "function run(i){ return process.run('printenv', []).stdout; }",
        json!({}),
        &["process:printenv", "env:FL_M3_GRANTED"],
        HostMode::Live,
    )
    .unwrap();
    let env = out.as_str().unwrap();
    assert!(!env.contains("leak-1"), "FORMLESS_* leaked: {env}");
    assert!(!env.contains("leak-2"), "undeclared variable leaked: {env}");
    assert!(env.contains("FL_M3_GRANTED=visible"), "{env}");
    assert!(env.contains("PATH="), "base environment missing: {env}");

    // FORMLESS_* stays hidden even when declared.
    let out = run_mode(
        "function run(i){ return process.run('printenv', []).stdout; }",
        json!({}),
        &["process:printenv", "env:FORMLESS_TEST_SECRET"],
        HostMode::Live,
    )
    .unwrap();
    assert!(!out.as_str().unwrap().contains("leak-1"));
}

/// Runs `code` live and sets the cancel flag after `after`; returns the
/// error and how long the Invocation took.
fn run_cancelled(
    code: &str,
    caps: &[&str],
    after: std::time::Duration,
) -> (formless_runtime::ExecError, std::time::Duration) {
    let cancel = Arc::new(AtomicBool::new(false));
    let req = ExecRequest {
        output_schema: None,
        code: code.to_string(),
        input: json!({}),
        caps: Capabilities::parse(&caps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
        memory_mb: 256,
        mode: HostMode::Live,
        overlay: None,
        activity: None,
        modules: Default::default(),
        cancel: cancel.clone(),
    };
    std::thread::spawn(move || {
        std::thread::sleep(after);
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let started = std::time::Instant::now();
    let err = execute(&req, Arc::new(NoHostOps)).unwrap_err();
    (err, started.elapsed())
}

#[cfg(unix)]
#[test]
fn cancel_stops_a_running_process_and_its_children() {
    let _env = credential_env::EnvGuard::isolated();
    let marker = format!("/tmp/fl-m3-kill-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    // The shell's child would create the marker after 1s if it survived.
    let script = format!("sleep 1 && touch {marker} & sleep 30");
    let (err, took) = run_cancelled(
        &format!("function run(i){{ return process.run('sh', ['-c', {script:?}]); }}"),
        &["process:sh"],
        std::time::Duration::from_millis(200),
    );
    assert_eq!(err.kind, ExecErrorKind::Cancelled, "{}", err.message);
    assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        !std::path::Path::new(&marker).exists(),
        "grandchild must be killed with the group"
    );
}

#[test]
fn cancel_stops_a_fetch_that_never_answers() {
    let _env = credential_env::EnvGuard::isolated();
    // Accepts connections but never replies.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let _held: Vec<_> = listener.incoming().collect();
    });
    let (err, took) = run_cancelled(
        &format!("function run(i){{ return fetch('http://{addr}/'); }}"),
        // A local address needs a grant that names it (SPEC §7.4).
        &["net:127.0.0.1"],
        std::time::Duration::from_millis(200),
    );
    assert_eq!(err.kind, ExecErrorKind::Cancelled, "{}", err.message);
    assert!(took < std::time::Duration::from_secs(5), "took {took:?}");
}

#[test]
fn timers_are_asynchronous_ordered_and_clearable() {
    let _env = credential_env::EnvGuard::isolated();
    let code = r#"
      async function run(i) {
        const order = [];
        setTimeout(() => order.push("late"), 60);
        const cancelled = setTimeout(() => order.push("cleared"), 10);
        setTimeout((x) => order.push(x), 20, "early");
        clearTimeout(cancelled);
        order.push("sync");
        let ticks = 0;
        await new Promise((done) => {
          const id = setInterval(() => { if (++ticks === 3) { clearInterval(id); done(); } }, 5);
        });
        const raced = await Promise.race([
          new Promise((r) => setTimeout(() => r("timeout"), 30)),
          new Promise((r) => setTimeout(() => r("fast"), 1)),
        ]);
        await new Promise((r) => setTimeout(r, 80));
        return { order, ticks, raced };
      }"#;
    let out = run_mode(code, json!({}), &[], HostMode::Live).unwrap();
    assert_eq!(out["order"], json!(["sync", "early", "late"]));
    assert_eq!(out["ticks"], 3);
    assert_eq!(out["raced"], "fast");
}

#[test]
fn cancel_interrupts_sleep_promptly() {
    let _env = credential_env::EnvGuard::isolated();
    let (err, took) = run_cancelled(
        "function run(i){ for (;;) sleep(60000); }",
        &[],
        std::time::Duration::from_millis(200),
    );
    assert_eq!(err.kind, ExecErrorKind::Cancelled, "{}", err.message);
    assert!(took < std::time::Duration::from_secs(2), "took {took:?}");
    let (err, took) = run_cancelled(
        "async function run(i){ await new Promise((r) => setTimeout(r, 60000)); }",
        &[],
        std::time::Duration::from_millis(200),
    );
    assert_eq!(err.kind, ExecErrorKind::Cancelled, "{}", err.message);
    assert!(took < std::time::Duration::from_secs(2), "took {took:?}");
}
