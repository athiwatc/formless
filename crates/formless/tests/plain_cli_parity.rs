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
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

struct Fixture {
    root: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<Request>>>,
    gc_failure: Arc<AtomicBool>,
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
        let gc_failure = Arc::new(AtomicBool::new(false));
        let fail_gc = gc_failure.clone();
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
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let envelope: RequestEnvelope = serde_json::from_str(&line).unwrap();
                    seen.lock().unwrap().push(envelope.request.clone());
                    if fail_gc.load(Ordering::SeqCst)
                        && matches!(&envelope.request, Request::Gc { .. })
                    {
                        let reply = serde_json::to_value(formless_protocol::ServerMessage::err(
                            envelope.id,
                            "fixture GC failure".into(),
                        ))
                        .unwrap();
                        if writeln!(socket, "{reply}").is_err() {
                            break;
                        }
                        line.clear();
                        continue;
                    }
                    let result = match &envelope.request {
                        Request::Gc { dry_run } => gc_report(*dry_run),
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
            gc_failure,
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
fn gc_report(dry_run: bool) -> Value {
    json!({"dry_run":dry_run,"total_bytes":9_007_199_254_740_999_u64,"kinds":[{"kind":"MCP logs","items":2,"bytes":1_234_567,"detail":"รายละเอียด 日本語","verification_error":null,"retained":8},{"kind":"artifacts","items":0,"bytes":9_007_199_253_506_432_u64,"detail":"stored metadata","future":{"kept":true}}],"future":{"unmodified":true}})
}

#[test]
fn shipped_gc_json_preserves_the_complete_single_daemon_report_and_human_bytes() {
    let fixture = Fixture::new();
    for dry_run in [false, true] {
        for json_mode in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["gc"];
            if dry_run {
                args.push("--dry-run");
            }
            if json_mode {
                args.push("--json");
            }
            let out = fixture.run(&args);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            if json_mode {
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    gc_report(dry_run)
                );
            } else {
                assert_eq!(
                    out.stdout,
                    if dry_run {
                        include_bytes!("fixtures/gc-human-dry.txt").as_slice()
                    } else {
                        include_bytes!("fixtures/gc-human-real.txt").as_slice()
                    }
                );
            }
            assert!(
                matches!(&fixture.requests.lock().unwrap()[..],[Request::Gc{dry_run:actual}] if *actual==dry_run)
            );
        }
    }
    fixture.gc_failure.store(true, Ordering::SeqCst);
    for dry_run in [false, true] {
        fixture.requests.lock().unwrap().clear();
        let mut args = vec!["gc", "--json"];
        if dry_run {
            args.push("--dry-run");
        }
        let out = fixture.run(&args);
        assert!(!out.status.success() && out.stdout.is_empty());
        assert!(String::from_utf8(out.stderr)
            .unwrap()
            .contains("fixture GC failure"));
        assert!(
            matches!(&fixture.requests.lock().unwrap()[..],[Request::Gc{dry_run:actual}] if *actual==dry_run)
        );
    }
}

#[test]
fn shipped_gc_json_works_offline_and_dry_run_keeps_owned_cleanup_data() {
    let mut fixture = Fixture::new();
    fixture.stop.store(true, Ordering::SeqCst);
    fixture.thread.take().unwrap().join().unwrap();
    let paths = Paths {
        data_dir: fixture.root.join("d/p"),
        config_dir: fixture.root.join("c/p"),
        profile: "p".into(),
    };
    std::fs::remove_file(paths.socket_path()).unwrap();
    let mcp = paths.data_dir.join("mcp");
    std::fs::create_dir_all(&mcp).unwrap();
    let log = mcp.join("owned-test.log");
    let bytes = vec![b'x'; 1_001_234];
    std::fs::write(&log, &bytes).unwrap();
    let store = formless_core::Store::open(&paths.db_file()).unwrap();
    let expected =
        formless_core::housekeeping::gc(&paths, &store, &formless_core::Config::default(), true)
            .unwrap();
    drop(store);
    let out = fixture.run(&["gc", "--dry-run", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value, expected);
    assert_eq!(std::fs::read(&log).unwrap(), bytes);
    let row = value["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "MCP logs")
        .unwrap();
    assert_eq!(row["items"], 1);
    assert_eq!(row["bytes"], 1234);
    let human = fixture.run(&["gc", "--dry-run"]);
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.contains("MCP logs") && text.contains("1.2 KB") && text.contains("would free about")
    );
    assert_eq!(std::fs::read(&log).unwrap(), bytes);
    let out = fixture.run(&["gc", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["dry_run"], false);
    let row = value["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "MCP logs")
        .unwrap();
    assert_eq!(row["items"], 1);
    assert_eq!(row["bytes"], 1234);
    assert_eq!(std::fs::metadata(&log).unwrap().len(), 1_000_000);
    let total: u64 = value["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(value["total_bytes"], total);
    assert!(fixture.requests.lock().unwrap().is_empty());
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
