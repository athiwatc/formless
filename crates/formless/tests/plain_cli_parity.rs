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
                    let result = match &envelope.request {
                        Request::UsageByDay { .. } => grouped_usage(),
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
            .env_remove("NO_COLOR")
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

fn grouped_usage() -> Value {
    json!([
        {"key":"日本語","prompt_tokens":7,"completion_tokens":8,"cached_tokens":2,"cost":null,"future":true},
        {"key":"ไทย","prompt_tokens":3,"completion_tokens":4,"cached_tokens":1,"cost":0.125}
    ])
}

#[test]
fn shipped_grouped_usage_sends_one_read_and_keeps_all_daemon_values() {
    let fixture = Fixture::new();
    for group in [
        None,
        Some("day"),
        Some("model"),
        Some("provider"),
        Some("purpose"),
        Some("role"),
        Some("session"),
    ] {
        for json in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["usage"];
            if let Some(group) = group {
                args.extend(["--by", group]);
            }
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
                    grouped_usage()
                );
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                assert!(text.find("日本語").unwrap() < text.find("ไทย").unwrap());
                assert!(text.contains('-') && text.contains("0.125"));
                assert!(
                    !text.contains('\x1b'),
                    "redirected usage must be plain: {text:?}"
                );
            }
            let requests = fixture.requests.lock().unwrap();
            let [Request::UsageByDay { since, by }] = &requests[..] else {
                panic!("{requests:?}")
            };
            assert!(since.is_none());
            assert_eq!(by.as_deref(), group.filter(|value| *value != "day"));
        }
    }
    fixture.requests.lock().unwrap().clear();
    let earliest = chrono::Utc::now() - chrono::Duration::days(30);
    let out = fixture.run(&["usage", "--since", "30d", "--json"]);
    let latest = chrono::Utc::now() - chrono::Duration::days(30);
    assert!(out.status.success());
    let requests = fixture.requests.lock().unwrap();
    let [Request::UsageByDay {
        since: Some(since),
        by: None,
    }] = &requests[..]
    else {
        panic!("{requests:?}")
    };
    let cutoff = chrono::DateTime::parse_from_rfc3339(since)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(cutoff >= earliest && cutoff <= latest);
    assert_eq!(since, &cutoff.to_rfc3339());
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
