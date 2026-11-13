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
    organization_failure: Arc<AtomicBool>,
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
        let organization_failure = Arc::new(AtomicBool::new(false));
        let fail_organization = organization_failure.clone();
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
                    if fail_organization.load(Ordering::SeqCst)
                        && matches!(
                            &envelope.request,
                            Request::SetSessionPinned { .. } | Request::SetSessionTags { .. }
                        )
                    {
                        let reply = serde_json::to_value(formless_protocol::ServerMessage::err(
                            envelope.id,
                            "fixture organization failure".into(),
                        ))
                        .unwrap();
                        if writeln!(socket, "{reply}").is_err() {
                            break;
                        }
                        line.clear();
                        continue;
                    }
                    let result = match &envelope.request {
                        Request::SetSessionPinned { id, pinned } => {
                            organization_reply(id, Some(!pinned), false)
                        }
                        Request::SetSessionTags { id, tags } => {
                            organization_reply(id, None, tags.is_empty())
                        }
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
            organization_failure,
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
fn organization_reply(id: &str, pinned: Option<bool>, clear: bool) -> Value {
    json!({"id":id,"name":"Stored 日本語","created_at":"","updated_at":"","pinned":pinned.unwrap_or(false),"tags":if clear {json!([])} else {json!(["normalized","ไทย\u{1b}[31m"])},"future":{"kept":true}})
}

#[test]
fn shipped_session_organization_uses_exact_mutations_and_returned_stored_state() {
    let fixture = Fixture::new();
    for (verb, requested) in [("pin", true), ("unpin", false)] {
        for json_mode in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["sessions", verb, "abc-o"];
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
                    organization_reply("abc-one", Some(!requested), false)
                );
            } else {
                assert_eq!(
                    String::from_utf8(out.stdout).unwrap(),
                    format!(
                        "Session ID: abc-one\nStored pinned: {}\n",
                        if requested { "no" } else { "yes" }
                    )
                );
            }
            assert!(
                matches!(&fixture.requests.lock().unwrap()[..],[Request::ListSessions,Request::SetSessionPinned{id,pinned}] if id=="abc-one" && *pinned==requested)
            );
        }
    }
    for json_mode in [false, true] {
        fixture.requests.lock().unwrap().clear();
        let mut args = vec!["sessions", "tag", "First"];
        if json_mode {
            args.push("--json");
        }
        let out = fixture.run(&args);
        assert!(out.status.success());
        if json_mode {
            let value: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(value["id"], "abc-one");
            assert_eq!(value["tags"], json!([]));
        } else {
            assert_eq!(out.stdout, b"Session ID: abc-one\nStored tags: none\n");
        }
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListSessions]
        ));
    }
    for clear in [false, true] {
        for json_mode in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = if clear {
                vec!["sessions", "tag", "abc-one", "--clear"]
            } else {
                vec!["sessions", "tag", "abc-one", " #Work ", "ไทย", "#WORK"]
            };
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
                    organization_reply("abc-one", None, clear)
                );
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                assert!(text.contains("Stored tags (replacement result):"));
                assert!(text.contains(if clear {
                    "none"
                } else {
                    "normalized, ไทย\\u{1b}[31m"
                }));
                assert!(!text.contains('\u{1b}'));
            }
            let seen = fixture.requests.lock().unwrap();
            assert!(
                matches!(&seen[..],[Request::ListSessions,Request::SetSessionTags{id,..}] if id=="abc-one")
            );
            let Request::SetSessionTags { tags, .. } = &seen[1] else {
                panic!()
            };
            assert_eq!(
                tags,
                &if clear {
                    vec![]
                } else {
                    vec![
                        " #Work ".to_string(),
                        "ไทย".to_string(),
                        "#WORK".to_string(),
                    ]
                }
            );
        }
    }
    fixture.requests.lock().unwrap().clear();
    let out = fixture.run(&["sessions", "tag", "abc-one", "--clear", "work"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty() && fixture.requests.lock().unwrap().is_empty());
    for selector in ["abc", "missing"] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&["sessions", "pin", selector]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListSessions]
        ));
    }
    fixture.organization_failure.store(true, Ordering::SeqCst);
    for args in [
        vec!["sessions", "pin", "abc-one"],
        vec!["sessions", "unpin", "abc-one", "--json"],
        vec!["sessions", "tag", "abc-one", "replacement"],
        vec!["sessions", "tag", "abc-one", "--clear", "--json"],
    ] {
        let out = fixture.run(&args);
        assert!(!out.status.success() && out.stdout.is_empty());
        assert!(String::from_utf8(out.stderr)
            .unwrap()
            .contains("fixture organization failure"));
    }
    let out = fixture.run(&["sessions", "tag", "--help"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for expected in [
        "replace the entire stored tag list",
        "read only",
        "replace all tags",
        "--clear",
        "--json",
    ] {
        assert!(text.contains(expected), "{text}");
    }
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
