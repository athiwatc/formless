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
                    let result = match &envelope.request {
                        Request::RunMetrics {
                            limit,
                            session_id,
                            since,
                        } => filtered_run_report(
                            limit.unwrap_or(20),
                            session_id.as_deref(),
                            since.as_deref(),
                        ),
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
                    seen.lock().unwrap().push(envelope.request);
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

fn filtered_run_report(limit: usize, session: Option<&str>, since: Option<&str>) -> Value {
    let rows = (0..40)
        .map(|i| formless_protocol::RunMetricsInfo {
            run_id: format!("run-{i:02}"),
            session_id: if i < 25 { "abc-two" } else { "abc-one" }.into(),
            status: "completed".into(),
            outcome: "completed".into(),
            rating: String::new(),
            steps: 1,
            tool_calls: 1,
            prompt_tokens: 50,
            completion_tokens: 10,
            duration_ms: 1000,
            created_at: format!(
                "2026-11-{:02}T12:00:{:02}+00:00",
                if i < 25 { 12 } else { 10 },
                59 - i
            ),
        })
        .filter(|row| {
            session.is_none_or(|session| row.session_id == session)
                && since.is_none_or(|since| row.created_at.as_str() >= since)
        })
        .take(limit)
        .collect::<Vec<_>>();
    json!({"summary":formless_core::runs::summarize(&rows),"runs":rows,"forecasts":{"independently_scoped":true},"future":"retained"})
}

#[test]
fn shipped_run_filters_send_resolved_scope_and_preserve_the_complete_report() {
    let fixture = Fixture::new();
    for (args, session, limit) in [
        (vec!["runs", "--json"], None, 20),
        (
            vec!["runs", "--session", "abc-o", "--json"],
            Some("abc-one"),
            20,
        ),
        (
            vec!["runs", "--session", "First", "--limit", "7", "--json"],
            Some("abc-one"),
            7,
        ),
    ] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value, filtered_run_report(limit, session, None));
        if session.is_some() {
            assert_eq!(value["runs"][0]["run_id"], "run-25");
            assert_eq!(value["summary"]["runs"], limit.min(15));
        }
        let requests = fixture.requests.lock().unwrap();
        let last = requests.last().unwrap();
        assert!(
            matches!(last,Request::RunMetrics{limit:Some(actual),session_id,since:None} if *actual==limit&&session_id.as_deref()==session)
        );
        assert_eq!(requests.len(), if session.is_some() { 2 } else { 1 });
        if session.is_some() {
            assert!(matches!(&requests[0], Request::ListSessions));
        }
    }
    let human = fixture.run(&["runs", "--session", "abc-o"]);
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("run-25") && !text.contains("run-00"));
    for selector in ["abc", "unknown"] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&["runs", "--session", selector]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListSessions]
        ));
    }
    // Exercise the live CLI cutoff, without assuming a wall-clock test date.
    fixture.requests.lock().unwrap().clear();
    let out = fixture.run(&["runs", "--session", "abc-o", "--since", "24h", "--json"]);
    assert!(out.status.success());
    let requests = fixture.requests.lock().unwrap();
    let [Request::ListSessions, Request::RunMetrics {
        limit: Some(20),
        session_id: Some(id),
        since: Some(cutoff),
    }] = &requests[..]
    else {
        panic!("{requests:?}")
    };
    assert_eq!(id, "abc-one");
    assert!(chrono::DateTime::parse_from_rfc3339(cutoff).is_ok());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        filtered_run_report(20, Some("abc-one"), Some(cutoff))
    );
    drop(requests);
    // A fixed cutoff makes the fixture's filter-before-limit contract explicit.
    assert_eq!(
        filtered_run_report(20, Some("abc-two"), Some("2026-11-12T12:00:49+00:00"))["runs"]
            .as_array()
            .unwrap()
            .len(),
        11
    );
    let before = fixture.connections.load(Ordering::SeqCst);
    for args in [
        vec!["runs", "--session", "X", "show", "id"],
        vec!["runs", "--since", "24h", "cancel", "id"],
        vec!["runs", "--since", "18446744073709551615d"],
        vec!["runs", "show", "id", "--session", "X"],
    ] {
        let out = fixture.run(&args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
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
