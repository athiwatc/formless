//! Shipped headless output contracts with interleaved plan/recap/progress IPC.
//! A private fake daemon owns the only socket; no provider or service is used.
#![cfg(unix)]
#[path = "support/unix_fixture_io.rs"]
mod unix_fixture_io;
use formless_core::{Config, Paths, Store};
use formless_protocol::{Event, Request, RequestEnvelope, ServerMessage};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use unix_fixture_io::FixtureStream;
struct Fixture {
    root: std::path::PathBuf,
    paths: Paths,
    session: formless_protocol::SessionInfo,
    events: Vec<Event>,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_paused(false)
    }
    fn with_paused(paused: bool) -> Self {
        Self::with_history(paused, false)
    }
    fn with_history(paused: bool, obsolete: bool) -> Self {
        Self::with_send_reply(paused, obsolete, json!({"run_id": "headless-761"}))
    }
    fn with_send_reply(paused: bool, obsolete: bool, send_reply: Value) -> Self {
        Self::with_send_reply_and_model(paused, obsolete, send_reply, None)
    }
    fn with_served_model() -> Self {
        Self::with_send_reply_and_model(
            false,
            false,
            json!({"run_id": "headless-761"}),
            Some(
                json!({"model":{"name":"fallback-model","via":"fallback","requested":"requested-model"}})
                    .to_string(),
            ),
        )
    }
    fn with_send_reply_and_model(
        paused: bool,
        obsolete: bool,
        send_reply: Value,
        served_model_meta: Option<String>,
    ) -> Self {
        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
        let root = std::path::PathBuf::from("/tmp").join(format!(
            "fl-ho-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let mut config = Config::default();
        config.provider.base_url = "http://127.0.0.1:9".into();
        config.provider.model = "unused-private-fixture".into();
        config.naming.enabled = false;
        config.save(&paths).unwrap();
        let store = Store::open_in_memory().unwrap();
        let session = store
            .create_session(Some("headless acceptance".into()))
            .unwrap();
        store
            .set_session_model(&session.id, Some("stored-model".into()), Some("low".into()))
            .unwrap();
        let session = store
            .set_session_cwd(&session.id, Some("stored-directory"))
            .unwrap()
            .unwrap();
        let mut answer = store
            .append_message(&session.id, "assistant", "ONLY_FINAL_ANSWER_761")
            .unwrap();
        answer.meta = served_model_meta;
        let sid = &session.id;
        let decode = |value| serde_json::from_value::<Event>(value).unwrap();
        let status = if paused { "paused" } else { "completed" };
        let mut events = vec![
            decode(json!({"type":"run_started","session_id":sid,"run_id":"headless-761"})),
            decode(
                json!({"type":"plan_updated","session_id":sid,"items":[{"content":"PLAN_MARKER_761","status":"in_progress"}],"plan_state":{"run_id":"headless-761","origin":"live","run_status":"running"}}),
            ),
            decode(
                json!({"type":"tool_progress","session_id":sid,"run_id":"headless-761","invocation_id":"invoke-761","tool_name":"fixture_tool","fraction":0.5,"label":"PROGRESS_MARKER_761"}),
            ),
            decode(
                json!({"type":"message_delta","session_id":sid,"run_id":"headless-761","delta":"ONLY_FINAL_ANSWER_761"}),
            ),
            decode(
                json!({"type":"message_complete","session_id":sid,"run_id":"headless-761","message":answer}),
            ),
            decode(
                json!({"type":"run_recap","session_id":sid,"run_id":"headless-761","recap":{"steps":4,"duration_ms":40,"used":["RECAP_MARKER_761"],"built":[],"files":[],"steered":0,"status":status,"total_calls":1,"unfinished_calls":0}}),
            ),
            decode(
                json!({"type":"run_finished","session_id":sid,"run_id":"headless-761","status":status}),
            ),
        ];
        if paused {
            events.insert(5,decode(json!({"type":"run_paused","session_id":sid,"run_id":"headless-761","error":"private acceptance pause","explained":null})));
        }
        let listener = UnixListener::bind(paths.socket_path()).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let exported = session.clone();
        let emitted = events.clone();
        let server = std::thread::spawn(move || {
            let mut peers = Vec::new();
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let stopped = stopped.clone();
                        let session = exported.clone();
                        let events = emitted.clone();
                        let seen = seen.clone();
                        let send_reply = send_reply.clone();
                        peers.push(std::thread::spawn(move || {
                            serve_peer(
                                stream,
                                stopped,
                                session,
                                events,
                                obsolete,
                                &seen,
                                &send_reply,
                            );
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::park_timeout(Duration::from_millis(2))
                    }
                    Err(e) => panic!("private listener: {e}"),
                }
            }
            for peer in peers {
                peer.join().unwrap();
            }
        });
        Self {
            root,
            paths,
            session,
            events,
            requests,
            stop,
            server: Some(server),
        }
    }
    fn run(&self, mode: &str, quiet: bool, file: bool) -> Output {
        self.run_args(mode, quiet, file, &["--session", &self.session.id])
    }
    fn run_args(&self, mode: &str, quiet: bool, file: bool, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        command.env_clear();
        for key in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("FORMLESS_DATA_DIR", &self.paths.data_dir)
            .env("FORMLESS_CONFIG_DIR", &self.paths.config_dir)
            .env("FORMLESS_API_KEY", "private-headless-dummy")
            .env("NO_COLOR", "1")
            .env("RUST_LOG", "error")
            .args(["run", "private headless task", "--output", mode])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if quiet {
            command.arg("--quiet");
        }
        if file {
            command
                .arg("--output-file")
                .arg(self.root.join("answer.txt"));
        }
        let mut child = command.spawn().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let err = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!(
                    "headless output fixture exceeded its deadline; stdout={:?}; stderr={:?}",
                    out.join().unwrap(),
                    err.join().unwrap()
                );
            }
            std::thread::park_timeout(Duration::from_millis(2));
        };
        Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn frame(stream: &mut FixtureStream, message: ServerMessage) {
    writeln!(stream, "{}", serde_json::to_string(&message).unwrap()).unwrap();
}
fn serve_peer(
    stream: UnixStream,
    stop: Arc<AtomicBool>,
    session: formless_protocol::SessionInfo,
    events: Vec<Event>,
    obsolete: bool,
    requests: &Mutex<Vec<Request>>,
    send_reply: &Value,
) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stream =
        FixtureStream::new(stream, stop.clone(), deadline, Duration::from_millis(50)).unwrap();
    let mut pending = Vec::new();
    let mut chunk = [0u8; 4096];
    while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
        let count = match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(count) => count,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) => panic!("private IPC read: {e}"),
        };
        pending.extend_from_slice(&chunk[..count]);
        assert!(pending.len() < 64 * 1024, "bounded fixture NDJSON input");
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let request: RequestEnvelope = serde_json::from_slice(&line).unwrap();
            requests.lock().unwrap().push(request.request.clone());
            let emit_run = matches!(request.request, Request::SendMessage { .. })
                && send_reply["run_id"].is_string();
            let result = match request.request {
                Request::Status | Request::Ping => {
                    json!({"version":env!("CARGO_PKG_VERSION"),"protocol":1,"secure":true})
                }
                Request::ListSessions => json!([session]),
                Request::GetSession { id } if id == session.id => json!({"session":session}),
                Request::Subscribe { .. } => json!({}),
                Request::SetSessionModel { .. } | Request::SetSessionCwd { .. } => json!({}),
                Request::SendMessage { session_id, .. } => {
                    assert_eq!(session_id, session.id);
                    send_reply.clone()
                }
                other => panic!("unexpected private headless request: {other:?}"),
            };
            if emit_run {
                if obsolete {
                    // The ack, rather than an unsolicited RunStarted, owns
                    // this command. Delay the ack across both old and target
                    // events to exercise the pre-ack deferred queue.
                    for event in obsolete_events(&session.id) {
                        frame(&mut stream, ServerMessage::Event(event));
                    }
                    frame(&mut stream, ServerMessage::Event(events[0].clone()));
                    for event in obsolete_events(&session.id).into_iter().skip(1) {
                        frame(&mut stream, ServerMessage::Event(event));
                    }
                } else {
                    // Deliver live metadata while its RPC is outstanding.
                    for event in &events[..events.len() - 1] {
                        frame(&mut stream, ServerMessage::Event(event.clone()));
                    }
                }
            }
            frame(
                &mut stream,
                ServerMessage::Response {
                    id: request.id,
                    ok: true,
                    result: Some(result),
                    error: None,
                },
            );
            if emit_run {
                if obsolete {
                    // An old terminal tail can also arrive after binding.
                    for event in obsolete_events(&session.id).into_iter().skip(1) {
                        frame(&mut stream, ServerMessage::Event(event));
                    }
                    for event in events.iter().skip(1) {
                        frame(&mut stream, ServerMessage::Event(event.clone()));
                    }
                } else {
                    frame(
                        &mut stream,
                        ServerMessage::Event(events.last().unwrap().clone()),
                    );
                }
            }
        }
    }
}

#[test]
fn cli_run_overrides_restore_the_canonical_session_on_success_and_error() {
    for (selector, success) in [
        ("prefix", true),
        ("name", true),
        ("name", false),
        ("exact", true),
        ("continue", true),
        ("last", true),
    ] {
        let reply = if success {
            json!({"run_id": "headless-761"})
        } else {
            json!({})
        };
        let fixture = Fixture::with_send_reply(false, false, reply);
        let id = fixture.session.id.as_str();
        let cwd = fixture.root.canonicalize().unwrap();
        let mut args = vec!["--effort", "high", "--cwd", cwd.to_str().unwrap()];
        match selector {
            "prefix" => args.extend(["--session", &id[..12]]),
            "name" => args.extend(["--session", "HEADLESS ACCEPTANCE"]),
            "exact" => args.extend(["--session", id]),
            "continue" | "last" => {
                std::fs::write(fixture.paths.last_run_session_file(), id).unwrap();
                if selector == "continue" {
                    args.push("--continue");
                } else {
                    args.extend(["--session", "last"]);
                }
            }
            _ => unreachable!(),
        }
        let output = fixture.run_args("final", true, false, &args);
        assert_eq!(
            output.status.code(),
            Some(if success { 0 } else { 1 }),
            "{output:?}"
        );
        if success {
            assert_eq!(output.stdout, b"ONLY_FINAL_ANSWER_761\n");
        } else {
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("did not identify the accepted Run"));
        }

        let seen = fixture.requests.lock().unwrap();
        let requests: Vec<_> = seen
            .iter()
            .filter(|request| {
                matches!(
                    request,
                    Request::SetSessionCwd { .. }
                        | Request::SetSessionModel { .. }
                        | Request::SendMessage { .. }
                )
            })
            .collect();
        assert!(
            matches!(&requests[..], [
                Request::SetSessionCwd { id: cwd_id, cwd: Some(temporary) },
                Request::SetSessionModel { id: model_id, model: Some(model), reasoning_effort: Some(effort) },
                Request::SendMessage { session_id, .. },
                Request::SetSessionModel { id: restore_model_id, model: previous_model, reasoning_effort: previous_effort },
                Request::SetSessionCwd { id: restore_cwd_id, cwd: previous_cwd },
            ] if [cwd_id, model_id, session_id, restore_model_id, restore_cwd_id]
                .into_iter().all(|owner| owner == id)
                && temporary == cwd.to_str().unwrap()
                && model == "stored-model"
                && effort == "high"
                && previous_model == &fixture.session.model
                && previous_effort == &fixture.session.reasoning_effort
                && previous_cwd == &fixture.session.cwd),
            "{selector} success={success}: {requests:?}"
        );
        assert_eq!(
            seen.iter()
                .filter(|request| matches!(request, Request::ListSessions))
                .count(),
            1
        );
    }
}

#[test]
fn interleaved_run_metadata_preserves_every_headless_stdout_mode() {
    for mode in ["text", "final", "json", "jsonl-events"] {
        for quiet in [false, true] {
            for file in [false, true] {
                let fixture = Fixture::new();
                let output = fixture.run(mode, quiet, file);
                assert!(
                    output.status.success(),
                    "{mode} quiet={quiet} file={file}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let bytes = if file {
                    assert!(
                        output.stdout.is_empty(),
                        "output-file must keep stdout empty"
                    );
                    std::fs::read(fixture.root.join("answer.txt")).unwrap()
                } else {
                    output.stdout
                };
                match mode {
                    "text" | "final" => assert_eq!(
                        bytes, b"ONLY_FINAL_ANSWER_761\n",
                        "live plan/recap/progress must not enter answer stdout"
                    ),
                    "json" => {
                        let value: Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(
                            value,
                            json!({
                                "answer": "ONLY_FINAL_ANSWER_761", "status": "completed",
                                "session_id": fixture.session.id, "result": null, "error": null,
                            })
                        );
                    }
                    "jsonl-events" => {
                        let actual: Vec<Value> = String::from_utf8(bytes)
                            .unwrap()
                            .lines()
                            .map(|line| serde_json::from_str(line).unwrap())
                            .collect();
                        let expected: Vec<Value> = fixture
                            .events
                            .iter()
                            .map(|event| serde_json::to_value(event).unwrap())
                            .collect();
                        assert_eq!(actual,expected,"RPC interleaving must preserve event order without loss or duplication");
                    }
                    _ => unreachable!(),
                }
                if quiet {
                    assert!(
                        output.stderr.is_empty(),
                        "quiet successful Run must have no human feedback: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
    }
}

#[test]
fn quiet_headless_output_suppresses_served_model_feedback() {
    let fixture = Fixture::with_served_model();
    let output = fixture.run("text", true, false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ONLY_FINAL_ANSWER_761\n");
    assert!(
        output.stderr.is_empty(),
        "quiet output must suppress served-model feedback: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn redirected_final_headless_output_keeps_served_model_feedback() {
    let fixture = Fixture::with_served_model();
    let output = fixture.run("final", false, false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ONLY_FINAL_ANSWER_761\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("via fallback-model (fallback)"), "{stderr}");
}

#[test]
fn paused_headless_jsonl_retains_terminal_recap_after_pause_notice() {
    let fixture = Fixture::with_paused(true);
    let output = fixture.run("jsonl-events", true, false);
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let expected: Vec<Value> = fixture
        .events
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect();
    assert_eq!(actual,expected,"RunPaused is followed by terminal recap and RunFinished; consume the complete terminal tail exactly once");
}

fn obsolete_events(session: &str) -> Vec<Event> {
    [
        json!({"type":"run_started","session_id":session,"run_id":"obsolete-headless-761"}),
        json!({"type":"run_paused","session_id":session,"run_id":"obsolete-headless-761","error":"OBSOLETE_PAUSE_MUST_STAY_OUT","explained":null}),
        json!({"type":"run_recap","session_id":session,"run_id":"obsolete-headless-761","recap":{"steps":4,"duration_ms":99,"used":["OBSOLETE_RECAP_MUST_STAY_OUT"],"built":[],"files":[],"steered":0,"status":"paused","total_calls":1,"unfinished_calls":0}}),
        json!({"type":"run_finished","session_id":session,"run_id":"obsolete-headless-761","status":"paused"}),
    ].into_iter().map(|value|serde_json::from_value(value).unwrap()).collect()
}

#[test]
fn headless_target_binds_to_ack_and_ignores_same_session_older_terminal_events() {
    for mode in ["text", "final", "json", "jsonl-events"] {
        let fixture = Fixture::with_history(false, true);
        let output = fixture.run(mode, true, false);
        assert!(
            output.status.success(),
            "{mode}: old terminal tail must not end requested Run: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "obsolete pause/recap must not produce feedback: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        match mode {
            "text" | "final" => assert_eq!(output.stdout, b"ONLY_FINAL_ANSWER_761\n"),
            "json" => {
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(value["status"], "completed");
                assert_eq!(value["answer"], "ONLY_FINAL_ANSWER_761");
            }
            "jsonl-events" => {
                let actual: Vec<Value> = String::from_utf8(output.stdout)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                let expected: Vec<Value> = fixture
                    .events
                    .iter()
                    .map(|event| serde_json::to_value(event).unwrap())
                    .collect();
                assert_eq!(actual,expected,"only ack-owned events reach stdout, retaining target pre-ack RunStarted exactly once");
            }
            _ => unreachable!(),
        }
    }
}
