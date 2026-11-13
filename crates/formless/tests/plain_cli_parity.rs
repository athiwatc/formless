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
                        Request::ListModules => module_list(),
                        Request::GetModule { .. } => module_detail(),
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
                    let reply = if matches!(&envelope.request,Request::GetModule{id} if id=="missing")
                    {
                        serde_json::to_value(formless_protocol::ServerMessage::err_code(
                            envelope.id,
                            formless_protocol::codes::NOT_FOUND,
                            "module not found".into(),
                        ))
                        .unwrap()
                    } else {
                        json!({"kind":"response", "id":envelope.id, "ok":true, "result":result})
                    };
                    seen.lock().unwrap().push(envelope.request);
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

fn module_list() -> Value {
    json!([
        {"id":"full-module-id","name":"日本語 module","description":"完整 description","current_version":2,"retired":false,"created_at":"first","updated_at":"last","future":7},
        {"id":"retired-id","name":"retired module","description":"retired description","current_version":4,"retired":true,"created_at":"first","updated_at":"last"}
    ])
}
fn module_detail() -> Value {
    json!({"module":module_list()[0],"versions":[
        {"version":1,"author":"user","created_at":"first","code":"// source 日本語\n"},
        {"version":2,"author":"agent","created_at":"last","code":"// source ไทย"}
    ],"used_by":["alpha","日本語 tool"],"future":{"preserved":true}})
}

#[test]
fn shipped_module_reports_preserve_json_exact_source_and_read_only_requests() {
    let fixture = Fixture::new();
    for json in [false, true] {
        fixture.requests.lock().unwrap().clear();
        let args = if json {
            vec!["modules", "list", "--json"]
        } else {
            vec!["modules", "list"]
        };
        let out = fixture.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if json {
            assert_eq!(
                serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                module_list()
            );
        } else {
            let text = String::from_utf8(out.stdout).unwrap();
            for expected in [
                "日本語 module",
                "retired module",
                "active",
                "retired",
                "完整 description",
            ] {
                assert!(text.contains(expected), "{text}");
            }
        }
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListModules]
        ));
    }
    for selector in ["日本語 module", "full-module-id"] {
        for json in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["modules", "show", selector];
            if json {
                args.push("--json");
            }
            let out = fixture.run(&args);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            if json {
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    module_detail()
                );
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                for expected in [
                    "Module: 日本語 module",
                    "ID: full-module-id",
                    "Current version: 2",
                    "完整 description",
                    "v1 · user · first",
                    "v2 · agent · last",
                    "alpha",
                    "日本語 tool",
                ] {
                    assert!(text.contains(expected), "{text}");
                }
                assert!(!text.contains("// source"));
            }
            assert!(
                matches!(&fixture.requests.lock().unwrap()[..],[Request::GetModule{id}] if id==selector)
            );
        }
    }
    for (version, expected) in [("1", "// source 日本語\n"), ("2", "// source ไทย")] {
        let out = fixture.run(&["modules", "show", "full-module-id", "--version", version]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, expected.as_bytes());
    }
    for (selector, version, message) in [
        ("missing", None, "module not found"),
        ("full-module-id", Some("99"), "no stored version 99"),
    ] {
        let mut args = vec!["modules", "show", selector];
        if let Some(version) = version {
            args.extend(["--version", version]);
        }
        let out = fixture.run(&args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8(out.stderr).unwrap().contains(message));
    }
    let before = fixture.connections.load(Ordering::SeqCst);
    for args in [
        vec!["modules", "show", "name", "--version", "1", "--json"],
        vec!["modules", "show", "name", "--version", "0"],
        vec!["modules", "show", "name", "--version", "bad"],
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
