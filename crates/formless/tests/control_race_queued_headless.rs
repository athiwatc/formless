//! A queued headless acknowledgement owns no Run and must not attach to an
//! older Run whose events arrived while SendMessage was outstanding.
//! A private fake daemon owns the only socket; no provider or service is used.
#![cfg(unix)]
use formless_core::{Config, Paths, Store};
use formless_protocol::{Event, Request, RequestEnvelope, ServerMessage};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
struct Fixture {
    root: std::path::PathBuf,
    paths: Paths,
    session: formless_protocol::SessionInfo,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::path::PathBuf::from("/tmp").join(format!(
            "fl-qh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
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
        let events:Vec<Event> = [
            json!({"type":"run_started","session_id":session.id,"run_id":"older-run-964"}),
            json!({"type":"message_delta","session_id":session.id,"run_id":"older-run-964","delta":"OLDER_ANSWER_MUST_STAY_OUT"}),
            json!({"type":"run_finished","session_id":session.id,"run_id":"older-run-964","status":"completed"}),
        ].into_iter().map(|value|serde_json::from_value(value).unwrap()).collect();
        let listener = UnixListener::bind(paths.socket_path()).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
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
                        peers.push(std::thread::spawn(move || {
                            serve_peer(stream, stopped, session, events)
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
            stop,
            server: Some(server),
        }
    }
    fn run(&self, mode: &str, quiet: bool, file: bool) -> Output {
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
            .args([
                "run",
                "private headless task",
                "--session",
                &self.session.id,
                "--output",
                mode,
            ])
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
fn frame(stream: &mut UnixStream, message: ServerMessage) {
    writeln!(stream, "{}", serde_json::to_string(&message).unwrap()).unwrap();
}
fn serve_peer(
    mut stream: UnixStream,
    stop: Arc<AtomicBool>,
    session: formless_protocol::SessionInfo,
    events: Vec<Event>,
) {
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
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
            let send = matches!(request.request, Request::SendMessage { .. });
            let result = match request.request {
                Request::Status | Request::Ping => {
                    json!({"version":env!("CARGO_PKG_VERSION"),"protocol":1,"secure":true})
                }
                Request::ListSessions => json!([session]),
                Request::Subscribe { .. } => json!({}),
                Request::SendMessage { session_id, .. } => {
                    assert_eq!(session_id, session.id);
                    json!({"queued":true})
                }
                other => panic!("unexpected private headless request: {other:?}"),
            };
            if send {
                for event in &events {
                    frame(&mut stream, ServerMessage::Event(event.clone()));
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
        }
    }
}

#[test]
fn queued_headless_ack_has_no_target_run_or_answer_in_any_output_mode() {
    for mode in ["text", "final", "json", "jsonl-events"] {
        for quiet in [false, true] {
            for file in [false, true] {
                let fixture = Fixture::new();
                let output_path = fixture.root.join("answer.txt");
                std::fs::write(&output_path, b"EXISTING_RESULT_FILE_964").unwrap();
                let output = fixture.run(mode, quiet, file);
                assert_eq!(
                    output.status.code(),
                    Some(2),
                    "queued SendMessage is not completion: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .to_lowercase()
                        .contains("queued"),
                    "queued disposition is material even with --quiet"
                );
                assert_eq!(
                    std::fs::read(&output_path).unwrap(),
                    b"EXISTING_RESULT_FILE_964",
                    "queued Run must not replace existing result file"
                );
                if mode == "json" && !file {
                    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(value["status"], "queued");
                    assert!(value["answer"].is_null());
                    assert!(value["error"].is_null());
                    assert_eq!(value["session_id"], fixture.session.id);
                    assert_eq!(value.as_object().unwrap().len(), 4);
                } else {
                    assert!(output.stdout.is_empty(),"queued request cannot stream old events, invent a Run, or produce an answer: {}",String::from_utf8_lossy(&output.stdout));
                }
            }
        }
    }
}
