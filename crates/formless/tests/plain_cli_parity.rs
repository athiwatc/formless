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

fn run_report() -> Value {
    let rows=[1,10,100,1000].into_iter().enumerate().map(|(i,n)|json!({"run_id":format!("full-run-日本語-{i}"),"created_at":"2026-11-01T12:34:56Z","outcome":if i==1 {"longer-than-eleven-outcome"} else {"completed"},"steps":n,"tool_calls":n,"prompt_tokens":10,"completion_tokens":5,"duration_ms":1200})).collect::<Vec<_>>();
    json!({"summary":{"runs":4,"completed":0.5,"interrupted":0.25,"paused":0.25,"edited":0.0,"corrected":0.0,"avg_steps":4.0,"avg_tool_calls":2.0,"avg_tokens":100.0,"avg_duration_ms":1200.0},"runs":rows,"future":{"preserved":true}})
}

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
                        Request::ListSessions => json!([
                            {"id":"abc-one","name":"First","created_at":"","updated_at":""},
                            {"id":"abc-two","name":"Second","created_at":"","updated_at":""},
                            {"id":"sel-z","name":"ไทย 日本語","preview":"ข้อความ 日本語","created_at":"","updated_at":"","pinned":true,"tags":["work","ไทย"]},
                            {"id":"sel-a","name":"Alpha","created_at":"","updated_at":"","tags":["workshop"]},
                            {"id":"sel-q","name":"Archived","created_at":"","updated_at":"","pinned":true,"archived":true,"tags":["work"]},
                            {"id":"sel-b","name":"Gamma","created_at":"","updated_at":"","pinned":true,"tags":["work"]}
                        ]),
                        Request::ListMcpPrompts => json!([
                            {"server":"server-日本語","name":"prompt-ไทย","description":"first line\nsecond line","arguments":[{"name":"topic","required":true}],"future":["🦀",3]}
                        ]),
                        Request::GetMcpPrompt { arguments, .. } => {
                            json!({"text":arguments.get("text").and_then(Value::as_str).unwrap_or("rendered 日本語\n\n"),"future":"preserved"})
                        }
                        Request::RunMemories { run_id, session_id } => {
                            assert!(run_id.is_some() != session_id.is_some());
                            if let Some(session) = session_id {
                                assert_eq!(session, "abc-one");
                            }
                            if run_id.as_deref() == Some("unmatched-run") {
                                json!([])
                            } else {
                                json!([
                                    {"id":"disabled-first","text":"ไทย\ncurrent text","scope":"project:/日本語","enabled":false,"author":"agent","session_id":"source-日本語","last_used":"different-run-time","future":{"x":1}},
                                    {"id":"enabled-second","text":"second note","scope":"global","enabled":true,"author":"user","session_id":null}
                                ])
                            }
                        }
                        Request::ListSubagents { session_id } => {
                            assert_eq!(session_id, "abc-one");
                            json!([
                                {"id":"subagent-ไทย-full-reusable-id","session_id":"abc-one","run_id":"run-one","task":"Task 日本語","depth":1,"model":"role · model","status":"running","steps":2,"result":null,"created_at":"first","updated_at":"last","future":7},
                                {"id":"second-subagent","session_id":"abc-one","run_id":"run-two","task":"Second","depth":2,"model":"model","status":"completed","steps":3,"result":"","created_at":"second","updated_at":"last"}
                            ])
                        }
                        Request::GetSubagent { id } => {
                            json!({"subagent":{"id":id,"session_id":"abc-one","run_id":"run-one","task":"Task 日本語","depth":1,"model":"role · model","status":"running","steps":2,"result":null,"created_at":"first","updated_at":"last"},"messages":[{"provider-special":{"part":["🦀",3]},"role":"assistant"},{"unknown":"ไทย"}],"future":7})
                        }
                        Request::RunMetrics {
                            limit,
                            session_id,
                            since,
                        } => {
                            assert_eq!(*limit, Some(4));
                            assert!(session_id.is_none());
                            assert!(since.is_none());
                            run_report()
                        }
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
                    let failure = match &envelope.request {
                        Request::GetMcpPrompt { name, .. } if name == "fixture-failure" => {
                            Some("server fixture failure")
                        }
                        Request::RunMemories {
                            run_id: Some(id), ..
                        } if id == "fixture-failure" => Some("daemon fixture failure"),
                        Request::GetSubagent { id } if id == "fixture-not-found" => {
                            Some("sub-agent fixture missing")
                        }
                        _ => None,
                    };
                    seen.lock().unwrap().push(envelope.request);
                    let reply = if let Some(message) = failure {
                        serde_json::to_value(formless_protocol::ServerMessage::err_code(
                            envelope.id,
                            formless_protocol::codes::NOT_FOUND,
                            message.into(),
                        ))
                        .unwrap()
                    } else {
                        json!({"kind":"response", "id":envelope.id, "ok":true, "result":result})
                    };
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

#[test]
fn session_list_selectors_preserve_order_and_use_only_the_existing_read_request() {
    let fixture = Fixture::new();
    let base = fixture.run(&["sessions", "list", "--json"]);
    assert!(base.status.success(), "{base:?}");
    let original: Vec<Value> = serde_json::from_slice(&base.stdout).unwrap();
    assert_eq!(
        original
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["abc-one", "abc-two", "sel-z", "sel-a", "sel-b"]
    );
    for (flags, ids) in [
        (vec!["--pinned"], vec!["sel-z", "sel-b"]),
        (vec!["--tag", "WoRK"], vec!["sel-z", "sel-b"]),
        (
            vec!["--archived", "--tag", "work", "--pinned"],
            vec!["sel-q"],
        ),
        (vec!["--tag", "ไทย"], vec!["sel-z"]),
        (vec!["--tag", "wor"], vec![]),
        (vec!["--tag", "#work"], vec![]),
    ] {
        let mut args = vec!["sessions", "list"];
        args.extend(flags.clone());
        let human = fixture.run(&args);
        assert!(human.status.success(), "{human:?}");
        let text = String::from_utf8(human.stdout).unwrap();
        if ids.is_empty() {
            assert!(
                text.contains("no Sessions match --tag") && text.contains(flags[1]),
                "{text}"
            );
        } else {
            let positions: Vec<_> = ids.iter().map(|id| text.find(id).unwrap()).collect();
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
            if ids.contains(&"sel-z") {
                assert!(text.contains("★ ไทย 日本語") && text.contains("ข้อความ 日本語"));
            }
            for excluded in ["abc-one", "abc-two", "sel-z", "sel-a", "sel-q", "sel-b"]
                .into_iter()
                .filter(|id| !ids.contains(id))
            {
                assert!(!text.contains(excluded), "excluded {excluded}: {text}");
            }
        }
        args.push("--json");
        let out = fixture.run(&args);
        assert!(out.status.success(), "{out:?}");
        let rows: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            rows.iter()
                .map(|s| s["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ids
        );
        for row in rows.iter().filter(|row| row["archived"] == false) {
            assert_eq!(
                row,
                original
                    .iter()
                    .find(|original| original["id"] == row["id"])
                    .unwrap()
            );
        }
    }
    assert!(fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|request| matches!(request, Request::ListSessions)));
}

#[test]
fn shipped_mcp_prompt_commands_preserve_text_json_named_args_and_errors() {
    let fixture = Fixture::new();
    for args in [
        vec!["mcp", "prompt", "s", "p", "--arg", "missing"],
        vec!["mcp", "prompt", "s", "p", "--arg", "=empty"],
        vec!["mcp", "prompt", "s", "p", "--arg", "x=1", "--arg", "x=2"],
    ] {
        let output = fixture.run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        fixture.connections.load(Ordering::SeqCst),
        0,
        "usage errors must precede any daemon contact"
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
    let list = fixture.run(&["mcp", "prompts", "--json"]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let value: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(value[0]["future"], json!(["🦀", 3]));
    let list = fixture.run(&["mcp", "prompts"]);
    assert!(list.status.success());
    let text = String::from_utf8(list.stdout).unwrap();
    assert!(text.contains("server-日本語"));
    assert!(text.contains("prompt-ไทย"));
    assert!(text.contains("may be incomplete"));
    assert!(!text.contains("second line"));
    for text in ["", "no trailing newline", "Unicode ไทย = value\n\n"] {
        let arg = format!("text={text}");
        for json in [false, true] {
            let mut args = vec![
                "mcp",
                "prompt",
                "server-日本語",
                "prompt-ไทย",
                "--arg",
                &arg,
                "--arg",
                "empty=",
                "--arg",
                "nested=a=b",
            ];
            if json {
                args.push("--json");
            }
            let output = fixture.run(&args);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            if json {
                assert_eq!(
                    serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                    json!({"text":text,"future":"preserved"})
                );
            } else {
                assert_eq!(output.stdout, text.as_bytes());
            }
        }
    }
    for json in [false, true] {
        let mut args = vec!["mcp", "prompt", "server-日本語", "fixture-failure"];
        if json {
            args.push("--json");
        }
        let output = fixture.run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("server fixture failure"));
    }
    let seen = fixture.requests.lock().unwrap();
    assert!(
        seen.iter()
            .all(|req| matches!(req, Request::ListMcpPrompts | Request::GetMcpPrompt { .. })),
        "no Session/Run/configuration request: {seen:?}"
    );
    for req in seen.iter() {
        if let Request::GetMcpPrompt {
            server,
            name,
            arguments,
            positional,
        } = req
        {
            assert_eq!(server, "server-日本語");
            assert!(positional.is_empty());
            if name == "prompt-ไทย" {
                assert_eq!(arguments["empty"], "");
                assert_eq!(arguments["nested"], "a=b");
                assert!(arguments["text"].is_string());
            }
        }
    }
}

#[test]
fn shipped_memory_used_preserves_exact_selectors_current_notes_and_qualified_empty_reports() {
    let fixture = Fixture::new();
    for args in [
        vec!["memory", "used"],
        vec!["memory", "used", "--session", "abc-one", "--run", "exact"],
    ] {
        let output = fixture.run(&args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(fixture.connections.load(Ordering::SeqCst), 0);
    assert!(fixture.requests.lock().unwrap().is_empty());
    for selector in ["abc-one", "abc-o", "First"] {
        let output = fixture.run(&["memory", "used", "--session", selector]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Latest-Run memory report for Session First (abc-one)"));
        assert!(text.contains("ไทย\ncurrent text"));
        assert!(text.contains("off"));
        assert!(text.contains("source-日本語"));
        assert!(!text.contains("different-run-time"));
    }
    let before = fixture.requests.lock().unwrap().len();
    for selector in ["abc", "missing"] {
        let output = fixture.run(&["memory", "used", "--session", selector, "--json"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert!(fixture.requests.lock().unwrap()[before..]
        .iter()
        .all(|req| matches!(req, Request::ListSessions)));
    for id in ["retained-missing-run", "unmatched-run"] {
        for json in [false, true] {
            let mut args = vec!["memory", "used", "--run", id];
            if json {
                args.push("--json");
            }
            let output = fixture.run(&args);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            if json {
                let value: Value = serde_json::from_slice(&output.stdout).unwrap();
                if id == "unmatched-run" {
                    assert_eq!(value, json!([]));
                } else {
                    assert_eq!(value[0]["future"], json!({"x":1}));
                    assert_eq!(value[0]["enabled"], false);
                    assert_eq!(value[1]["id"], "enabled-second");
                }
            } else {
                let text = String::from_utf8(output.stdout).unwrap();
                assert!(text.contains(&format!("requested Run id {id}")));
                if id == "unmatched-run" {
                    assert!(text.contains("no carried memories reported for this selector"));
                } else {
                    assert!(text.contains("ไทย\ncurrent text"));
                }
            }
        }
    }
    let output = fixture.run(&["memory", "used", "--run", "fixture-failure", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("daemon fixture failure"));
    let seen = fixture.requests.lock().unwrap();
    assert!(
        seen.iter()
            .all(|req| matches!(req, Request::ListSessions | Request::RunMemories { .. })),
        "{seen:?}"
    );
    assert_eq!(seen.iter().filter(|req|matches!(req,Request::RunMemories{run_id:Some(id),session_id:None} if id=="retained-missing-run")).count(),2);
}

#[test]
fn shipped_subagent_reports_preserve_session_resolution_exact_ids_and_provider_json() {
    let fixture = Fixture::new();
    for selector in ["abc-one", "abc-o", "First"] {
        let output = fixture.run(&["sessions", "subagents", selector, "--json"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value[0]["result"], Value::Null);
        assert_eq!(value[1]["result"], "");
        assert_eq!(value[0]["future"], 7);
    }
    let inventory = fixture.run(&["sessions", "subagents", "First"]);
    assert!(inventory.status.success());
    let text = String::from_utf8(inventory.stdout).unwrap();
    assert!(text.contains("subagent-ไทย-full-reusable-id"));
    assert!(text.contains("Result: null"));
    assert!(text.contains("Result: \"\""));
    let before = fixture.requests.lock().unwrap().len();
    for selector in ["abc", "missing"] {
        let output = fixture.run(&["sessions", "subagents", selector]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    assert!(fixture.requests.lock().unwrap()[before..]
        .iter()
        .all(|req| matches!(req, Request::ListSessions)));
    let exact = "subagent-ไทย-full-reusable-id";
    for json in [false, true] {
        let mut args = vec!["sessions", "subagent", exact];
        if json {
            args.push("--json");
        }
        let output = fixture.run(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = if json {
            serde_json::from_slice(&output.stdout).unwrap()
        } else {
            let text = String::from_utf8(output.stdout).unwrap();
            let (_, messages) = text
                .split_once("Messages (unpaged transcript returned by daemon):\n")
                .unwrap();
            json!({"messages":serde_json::from_str::<Value>(messages).unwrap()})
        };
        assert_eq!(
            value["messages"],
            json!([{"provider-special":{"part":["🦀",3]},"role":"assistant"},{"unknown":"ไทย"}])
        );
        if json {
            assert_eq!(value["future"], 7);
        }
    }
    let output = fixture.run(&["sessions", "subagent", "fixture-not-found", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("sub-agent fixture missing"));
    let seen = fixture.requests.lock().unwrap();
    assert!(
        seen.iter().all(|req| matches!(
            req,
            Request::ListSessions | Request::ListSubagents { .. } | Request::GetSubagent { .. }
        )),
        "{seen:?}"
    );
    assert_eq!(
        seen.iter()
            .filter(|req| matches!(req,Request::GetSubagent{id} if id==exact))
            .count(),
        2
    );
}

#[test]
fn shipped_run_list_retains_report_json_summary_order_and_numeric_units() {
    let fixture = Fixture::new();
    let output = fixture.run(&["runs", "--limit", "4", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        run_report()
    );
    let output = fixture.run(&["runs", "--limit", "4"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let summary: formless_protocol::RunSummary =
        serde_json::from_value(run_report()["summary"].clone()).unwrap();
    assert_eq!(
        text.lines().next(),
        Some(formless_core::runs::summary_line(&summary).as_str())
    );
    assert!(text.contains("STEPS"));
    assert!(text.contains("TOOLS"));
    assert!(text.contains("TOKENS"));
    assert!(text.contains("DURATION"));
    assert!(text.contains("1000"));
    assert!(text.contains("1.2s"));
    assert!(text.contains("longer-than-eleven-outcome"));
    let mut previous = 0;
    for i in 0..4 {
        let at = text.find(&format!("full-run-日本語-{i}")).unwrap();
        assert!(at > previous);
        previous = at;
    }
    assert!(!text.contains("ok completed"));
    assert!(!text.contains("fail completed"));
    assert!(fixture.requests.lock().unwrap().iter().all(|req| matches!(
        req,
        Request::RunMetrics {
            limit: Some(4),
            session_id: None,
            since: None
        }
    )));
}
