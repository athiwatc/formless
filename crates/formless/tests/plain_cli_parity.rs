//! Shipped file commands against an owned, provider-free fake daemon.
#![cfg(unix)]
use formless_core::Paths;
use formless_protocol::{Request, RequestEnvelope};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixListener,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

struct Fixture {
    root: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<Request>>>,
    invocations: Arc<Mutex<Value>>,
    invocation_error: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::path::PathBuf::from("/tmp").join(format!(
            "f1009-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = Paths {
            data_dir: root.join("d/p"),
            config_dir: root.join("c/p"),
            profile: "p".into(),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = UnixListener::bind(paths.socket_path()).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let invocations = Arc::new(Mutex::new(json!([])));
        let invocation_reply = invocations.clone();
        let invocation_error = Arc::new(AtomicBool::new(false));
        let fail_invocations = invocation_error.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let connected = connections.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                connected.fetch_add(1, Ordering::SeqCst);
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let envelope: RequestEnvelope = serde_json::from_str(&line).unwrap();
                    seen.lock().unwrap().push(envelope.request.clone());
                    if matches!(&envelope.request, Request::ListInvocations)
                        && fail_invocations.load(Ordering::SeqCst)
                    {
                        let reply = json!({"kind":"response","id":envelope.id,"ok":false,"error":"fixture invocation failure"});
                        if writeln!(socket, "{reply}").is_err() {
                            break;
                        }
                        line.clear();
                        continue;
                    }
                    let result = match &envelope.request {
                        Request::Subscribe { .. } => json!({"conn":1}),
                        Request::ListInvocations => invocation_reply.lock().unwrap().clone(),
                        Request::ListSessions => json!([
                            {"id":"abc-one","name":"First","created_at":"","updated_at":""},
                            {"id":"abc-two","name":"Second","created_at":"","updated_at":""}
                        ]),
                        Request::RunFiles { session_id, run_id } => {
                            assert_eq!(session_id, "abc-one");
                            json!({"run_id":run_id.as_deref().unwrap_or("latest"), "files":[{"path":"dir/a.txt","status":"modified","revertible":true,"diff":"- old\n+ new"}], "untracked_writes_possible":true})
                        }
                        Request::RewindRun { session_id, .. } => {
                            assert_eq!(session_id, "abc-one");
                            json!({"run_id":"run-one", "restored":["dir/a.txt"], "skipped":[{"path":"dir/changed.txt","reason":"changed since Run"}]})
                        }
                        _ => panic!("unexpected private RPC: {:?}", envelope.request),
                    };
                    let reply =
                        json!({"kind":"response", "id":envelope.id, "ok":true, "result":result});
                    if writeln!(socket, "{reply}").is_err() {
                        break;
                    }
                    line.clear();
                }
            }
        });
        Self {
            root,
            stop,
            thread: Some(thread),
            requests,
            invocations,
            invocation_error,
            connections,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("FORMLESS_") || name.contains("API_KEY") || name.contains("OAUTH") {
                command.env_remove(key);
            }
        }
        command
            .args(["--profile", "p"])
            .args(args)
            .env("FORMLESS_DATA_DIR", self.root.join("d"))
            .env("FORMLESS_CONFIG_DIR", self.root.join("c"))
            .env("FORMLESS_API_KEY", "private-offline-fake-daemon")
            .env("COLUMNS", "20")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut child = self.command(args).spawn().unwrap();
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
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("private fake-daemon CLI timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        std::process::Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        }
    }
}

#[test]
fn shipped_live_invocations_preserve_snapshot_values_and_reject_limits_before_contact() {
    let fixture = Fixture::new();
    let entries = json!([
        {"handle":"handle-first-very-long-unclipped-1234567890","invocation_id":"invocation-full-id","tool_name":"日本語\u{1b}[31m","session_id":"session\nfull","elapsed_ms":1234,"silent_ms":0,"stalled":true,"abandoned":false,"last_fraction":0,"last_label":null},
        {"handle":"handle-second-very-long-unclipped-1234567890","invocation_id":null,"tool_name":"second","session_id":"session-two","elapsed_ms":86400001_u64,"silent_ms":60001,"stalled":false,"abandoned":true,"last_fraction":null,"last_label":"label only\tไทย"}
    ]);
    *fixture.invocations.lock().unwrap() = entries.clone();
    for args in [
        vec!["runs", "running"],
        vec!["runs", "running", "--json"],
        vec!["runs", "--json", "running"],
    ] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if args.contains(&"--json") {
            assert_eq!(
                serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                entries
            );
        } else {
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(
                text.starts_with("Live Invocations (one snapshot)\n"),
                "{text}"
            );
            assert!(
                text.find(entries[0]["handle"].as_str().unwrap()).unwrap()
                    < text.find(entries[1]["handle"].as_str().unwrap()).unwrap()
            );
            for expected in [
                "Invocation ID: invocation-full-id",
                "Elapsed: 1.234s",
                "Silent: 0.000s",
                "Elapsed: 86400.001s",
                "Silent: 60.001s",
                "Stalled: yes",
                "Stalled: no",
                "Abandoned: no",
                "Abandoned: yes — no longer awaited; may still finish",
                "Progress fraction: 0",
                "Progress label: label only\\u{9}ไทย",
                "日本語\\u{1b}[31m",
                "Session: session\\u{a}full",
            ] {
                assert!(text.contains(expected), "missing {expected:?}: {text}");
            }
            assert_eq!(text.matches("Progress fraction:").count(), 1);
            assert_eq!(text.matches("Progress label:").count(), 1);
            assert!(!text.contains('\u{1b}'));
        }
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert!(matches!(&requests[0], Request::Subscribe { .. }));
        assert!(matches!(&requests[1], Request::ListInvocations));
    }
    *fixture.invocations.lock().unwrap() = json!([]);
    let empty = fixture.run(&["runs", "running"]);
    assert!(empty.status.success());
    assert!(String::from_utf8(empty.stdout)
        .unwrap()
        .contains("No live Invocations."));
    let empty = fixture.run(&["runs", "running", "--json"]);
    assert!(empty.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&empty.stdout).unwrap(),
        json!([])
    );
    fixture.invocation_error.store(true, Ordering::SeqCst);
    let failed = fixture.run(&["runs", "running"]);
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert!(String::from_utf8(failed.stderr)
        .unwrap()
        .contains("fixture invocation failure"));
    let before = fixture.connections.load(Ordering::SeqCst);
    for args in [
        vec!["runs", "--limit", "20", "running"],
        vec!["runs", "--limit", "0", "running"],
        vec!["runs", "running", "--limit", "20"],
    ] {
        let invalid = fixture.run(&args);
        assert_eq!(invalid.status.code(), Some(2));
        assert!(invalid.stdout.is_empty());
    }
    assert_eq!(fixture.connections.load(Ordering::SeqCst), before);
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn shipped_files_and_rewind_use_owned_rpc_prefix_resolution_and_json_contract() {
    let fixture = Fixture::new();
    let files = fixture.run(&["sessions", "files", "abc-o", "--run", "run-one", "--json"]);
    assert!(
        files.status.success(),
        "{}",
        String::from_utf8_lossy(&files.stderr)
    );
    let files: Value = serde_json::from_slice(&files.stdout).unwrap();
    assert_eq!(files["files"][0]["path"], "dir/a.txt");
    assert_eq!(files["run_id"], "run-one");
    let files_text = fixture.run(&["sessions", "files", "abc-o"]);
    assert!(
        files_text.status.success(),
        "{}",
        String::from_utf8_lossy(&files_text.stderr)
    );
    let text = String::from_utf8(files_text.stdout).unwrap();
    assert!(text.contains("dir/a.txt"));
    assert!(text.contains("Untracked writes"));
    let rewind = fixture.run(&[
        "sessions",
        "rewind",
        "abc-o",
        "dir/a.txt",
        "--run",
        "run-one",
        "--json",
    ]);
    assert!(
        rewind.status.success(),
        "{}",
        String::from_utf8_lossy(&rewind.stderr)
    );
    let rewind: Value = serde_json::from_slice(&rewind.stdout).unwrap();
    assert_eq!(rewind["skipped"][0]["reason"], "changed since Run");
    assert!(fixture.requests.lock().unwrap().iter().any(|request| matches!(request, Request::RewindRun { session_id, run_id: Some(run), path: Some(path) } if session_id == "abc-one" && run == "run-one" && path == "dir/a.txt")));
    let ambiguous = fixture.run(&["sessions", "rewind", "abc"]);
    assert!(!ambiguous.status.success());
    assert!(String::from_utf8_lossy(&ambiguous.stderr).contains("matches 2 Sessions"));
    let forced = fixture.run(&["sessions", "rewind", "abc-one", "--force"]);
    assert!(!forced.status.success());
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| matches!(r, Request::RewindRun { .. }))
            .count(),
        1
    );
}
