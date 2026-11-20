//! A shipped observer restores the paused correction reason from a stored
//! Session snapshot, including historical Runs with unknown message boundaries.
//! A private fake daemon owns the only socket; no provider or service is used.
#![cfg(unix)]
#[path = "support/unix_fixture_io.rs"]
mod unix_fixture_io;
use formless_core::{Config, Paths, Store};
use formless_protocol::{Request, RequestEnvelope, ServerMessage};
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
use unix_fixture_io::FixtureStream;
struct Fixture {
    root: std::path::PathBuf,
    paths: Paths,
    session: formless_protocol::SessionInfo,
    snapshot: Value,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(boundary: bool) -> Self {
        let root = std::path::PathBuf::from("/tmp").join(format!(
            "fl-ps-{}-{}",
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
        let reason =
            "stuck: repeated daemon correction; revise arguments or switch model then Resume";
        let snapshot = json!({"session":session,"messages":[],"plan":[],"running":false,"unfinished_run":{"run_id":"paused-970","status":"paused","error":reason},"latest_run":{"run_id":"paused-970","status":"paused","error":reason,"end_message_seq":if boundary {json!(0)}else{Value::Null}},"pending_events":[]});
        let listener = UnixListener::bind(paths.socket_path()).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let exported = session.clone();
        let emitted = snapshot.clone();
        let server = std::thread::spawn(move || {
            let mut peers = Vec::new();
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let stopped = stopped.clone();
                        let session = exported.clone();
                        let snapshot = emitted.clone();
                        peers.push(std::thread::spawn(move || {
                            serve_peer(stream, stopped, session, snapshot)
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
            snapshot,
            stop,
            server: Some(server),
        }
    }
    fn run(&self, flags: &[&str]) -> Output {
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
            .args(["sessions", "show", &self.session.id])
            .args(flags)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
    snapshot: Value,
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
            let result = match request.request {
                Request::Status | Request::Ping => {
                    json!({"version":env!("CARGO_PKG_VERSION"),"protocol":1,"secure":true})
                }
                Request::ListSessions => json!([session]),
                Request::Subscribe { .. } => json!({}),
                Request::GetSession { id } => {
                    assert_eq!(id, session.id);
                    snapshot.clone()
                }
                Request::GetRun {
                    run_id,
                    include_token_details: false,
                } => {
                    assert_eq!(run_id, "paused-970");
                    snapshot["latest_run"].clone()
                }
                other => panic!("unexpected private headless request: {other:?}"),
            };
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
fn shipped_session_observers_restore_stuck_reason_without_fabricating_terminal_events() {
    for boundary in [false, true] {
        let fixture = Fixture::new(boundary);
        let human = fixture.run(&[]);
        assert!(
            human.status.success(),
            "{}",
            String::from_utf8_lossy(&human.stderr)
        );
        let text = String::from_utf8(human.stdout).unwrap();
        assert!(text.contains("stuck: repeated daemon correction"));
        assert!(text.contains("Resume"));
        let json = fixture.run(&["--json"]);
        assert!(json.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&json.stdout).unwrap(),
            fixture.snapshot
        );
        let followed = fixture.run(&["--follow", "--jsonl"]);
        assert_eq!(followed.status.code(), Some(1));
        let notice = String::from_utf8(followed.stderr).unwrap();
        assert!(notice.contains("stuck: repeated daemon correction"));
        assert!(notice.contains("Resume"));
        for line in String::from_utf8(followed.stdout).unwrap().lines() {
            let event: Value = serde_json::from_str(line).unwrap();
            assert_ne!(event["type"], "run_paused");
            assert_ne!(
                event["type"], "run_finished",
                "stored snapshot must not invent live terminal events"
            );
        }
    }
}
