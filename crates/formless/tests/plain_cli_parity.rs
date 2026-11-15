//! Shipped file commands against an owned, provider-free fake daemon.
#![cfg(unix)]
#[path = "support/unix_fixture_io.rs"]
mod unix_fixture_io;
use formless_core::Paths;
use formless_protocol::{Request, RequestEnvelope};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixListener,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use unix_fixture_io::FixtureStream;

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
    session_rows: Arc<Mutex<Value>>,
    organization_failure: Arc<AtomicBool>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_tools(
            json!([{"id":"full-tool-id","name":"FetchPage","description":"","retired":false,"current_version":1,"capabilities":[],"memory_mb":16,"invocations":0,"success_rate":null,"needs_attention":false,"created_at":"","updated_at":""}]),
        )
    }
    fn with_tools(tool_rows: Value) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::path::PathBuf::from("/tmp").join(format!(
            "f1009-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
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
        let session_rows = Arc::new(Mutex::new(json!([
            {"id":"abc-one","name":"First","created_at":"","updated_at":""},
            {"id":"abc-two","name":"Second","created_at":"","updated_at":""},
            {"id":"sel-z","name":"ไทย 日本語","preview":"ข้อความ 日本語","created_at":"","updated_at":"","pinned":true,"tags":["work","ไทย"]},
            {"id":"sel-a","name":"Alpha","created_at":"","updated_at":"","tags":["workshop"]},
            {"id":"sel-q","name":"Archived","created_at":"","updated_at":"","pinned":true,"archived":true,"tags":["work"]},
            {"id":"sel-b","name":"Gamma","created_at":"","updated_at":"","pinned":true,"tags":["work"]}
        ])));
        let sessions = session_rows.clone();
        let organization_failure = Arc::new(AtomicBool::new(false));
        let fail_organization = organization_failure.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let connected = connections.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                connected.fetch_add(1, Ordering::SeqCst);
                let mut socket = FixtureStream::new(
                    socket,
                    stopped.clone(),
                    Instant::now() + Duration::from_secs(15),
                    Duration::from_secs(2),
                )
                .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let envelope: RequestEnvelope = serde_json::from_str(&line).unwrap();
                    if fail_organization.load(Ordering::SeqCst)
                        && matches!(
                            &envelope.request,
                            Request::SetSessionPinned { .. } | Request::SetSessionTags { .. }
                        )
                    {
                        seen.lock().unwrap().push(envelope.request.clone());
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
                        Request::ListModules => module_list(),
                        Request::GetModule { .. } => module_detail(),
                        Request::ListTools => tool_rows.clone(),
                        Request::ExportTool { id } => {
                            assert_eq!(id, "full-tool-id");
                            json!({"fixture":"unchanged bundle"})
                        }
                        Request::ProposalDiff { id } => proposal_comparison(id),
                        Request::ListSessions => sessions.lock().unwrap().clone(),
                        Request::GetSession { id } => {
                            let rows = sessions.lock().unwrap();
                            let session = rows
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|s| s["id"] == *id)
                                .unwrap();
                            session_snapshot(session.clone())
                        }
                        Request::ListMcpPrompts => json!([
                            {"server":"server-日本語","name":"prompt-ไทย","description":"first line\nsecond line","arguments":[{"name":"topic","required":true}],"future":["🦀",3]}
                        ]),
                        Request::GetMcpPrompt { arguments, .. } => {
                            json!({"text":arguments.get("text").and_then(Value::as_str).unwrap_or("rendered 日本語\n\n"),"future":"preserved"})
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
                    let reply = if matches!(&envelope.request,Request::GetModule{id} if id=="missing")
                    {
                        serde_json::to_value(formless_protocol::ServerMessage::err_code(
                            envelope.id,
                            formless_protocol::codes::NOT_FOUND,
                            "module not found".into(),
                        ))
                        .unwrap()
                    } else if matches!(&envelope.request,Request::ProposalDiff{id} if id=="missing")
                    {
                        serde_json::to_value(formless_protocol::ServerMessage::err_code(
                            envelope.id,
                            formless_protocol::codes::NOT_FOUND,
                            "proposal not found".into(),
                        ))
                        .unwrap()
                    } else if matches!(&envelope.request, Request::GetMcpPrompt {name,..} if name=="fixture-failure")
                    {
                        serde_json::to_value(formless_protocol::ServerMessage::err_code(
                            envelope.id,
                            formless_protocol::codes::NOT_FOUND,
                            "server fixture failure".into(),
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
            session_rows,
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
fn session_snapshot(session: Value) -> Value {
    json!({"session":session,"messages":[{"id":"msg-user","session_id":"session","seq":1,"role":"user","content":"Unicode transcript 日本語\nsecond line","created_at":""},{"id":"msg-tool","session_id":"session","seq":2,"role":"tool","content":"tool first\nsecond tool","created_at":""}],"unfinished_run":{"run_id":"paused-run","status":"paused","error":null},"future":{"retained":true}})
}

#[test]
fn shipped_sessions_explain_archive_emptiness_and_present_stored_metadata() {
    let fixture = Fixture::new();
    let rows = json!([
        {"id":"session-active","name":"日本語 ไทย","created_at":"","updated_at":"2026-11-01T12:34:56Z","message_count":7,"preview":"preview 日本語","last_run_status":"paused","pinned":true,"archived":false,"tags":["work","ไทย\u{1b}[31m"]},
        {"id":"session-archived","name":"Old","created_at":"","updated_at":"","message_count":0,"archived":true,"tags":[]}
    ]);
    *fixture.session_rows.lock().unwrap() = rows.clone();
    for archived in [false, true] {
        let mut args = vec!["sessions", "list"];
        if archived {
            args.push("--archived");
        }
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&args);
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(
            text.contains("LAST RECORDED RUN") && text.contains("TAGS"),
            "{text}"
        );
        if archived {
            assert!(text.contains("not recorded") && text.contains("Old"));
        } else {
            for expected in [
                "★ 日本語 ไทย",
                "paused",
                "work, ไทย\\u{1b}[31m",
                "preview 日本語",
                "7",
                "2026-11-01T12:34",
            ] {
                assert!(text.contains(expected), "{text}");
            }
            assert!(!text.contains('\u{1b}'));
        }
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListSessions]
        ));
        args.push("--json");
        let out = fixture.run(&args);
        assert!(out.status.success());
        let actual: Vec<formless_protocol::SessionInfo> =
            serde_json::from_slice(&out.stdout).unwrap();
        let expected: Vec<formless_protocol::SessionInfo> =
            serde_json::from_value(json!([rows[usize::from(archived)].clone()])).unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
    for (id, index) in [("session-active", 0), ("session-archived", 1)] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&["sessions", "show", id]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        for expected in [
            "Unicode transcript 日本語\nsecond line",
            "tool result: tool first",
            "Run paused\nResume this Session in the TUI or web after reviewing the cause.",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        if index == 0 {
            for expected in [
                "Archived: no",
                "Pinned: yes",
                "Tags: work, ไทย\\u{1b}[31m",
                "Last recorded Run status: paused",
            ] {
                assert!(text.contains(expected), "{text}");
            }
        } else {
            for expected in [
                "Archived: yes",
                "Pinned: no",
                "Tags: none",
                "Last recorded Run status: not recorded",
            ] {
                assert!(text.contains(expected), "{text}");
            }
        }
        assert!(!text.contains('\u{1b}'));
        assert!(
            matches!(&fixture.requests.lock().unwrap()[..],[Request::ListSessions,Request::GetSession{id:requested}] if requested==id)
        );
        let out = fixture.run(&["sessions", "show", id, "--json"]);
        assert!(out.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap(),
            session_snapshot(rows[index].clone())
        );
    }
    for archived in [false, true] {
        for empty in [false, true] {
            *fixture.session_rows.lock().unwrap() = if empty {
                json!([])
            } else {
                json!([
                    rows[usize::from(!archived)].clone(),
                    rows[usize::from(!archived)].clone()
                ])
            };
            let mut args = vec!["sessions", "list"];
            if archived {
                args.push("--archived");
            }
            let out = fixture.run(&args);
            assert!(out.status.success());
            let text = String::from_utf8(out.stdout).unwrap();
            let expected = if empty {
                "No Sessions in this response."
            } else if archived {
                "No archived Sessions. 2 active Sessions; use formless sessions list."
            } else {
                "No active Sessions. 2 archived Sessions; use formless sessions list --archived."
            };
            assert_eq!(text, format!("{expected}\n"));
            args.push("--json");
            let out = fixture.run(&args);
            assert!(out.status.success());
            assert_eq!(
                serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                json!([])
            );
        }
    }
}

#[test]
fn shipped_ambiguous_session_selectors_show_ids_and_never_dispatch_an_action() {
    let fixture = Fixture::new();
    let rows:Vec<Value>=(0..12).map(|n|json!({"id":format!("12345678-common-full-{n:02}"),"name":"日本語\u{1b}[31m\nไทย","archived":n%2==1,"created_at":"","updated_at":""})).collect();
    *fixture.session_rows.lock().unwrap() = json!(rows);
    for args in [
        vec!["sessions", "show", "12345678", "--json"],
        vec!["sessions", "archive", "12345678"],
        vec!["sessions", "files", "12345678"],
    ] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        let text = String::from_utf8(out.stderr).unwrap();
        for row in rows.iter().take(10) {
            assert!(text.contains(row["id"].as_str().unwrap()), "{text}");
        }
        assert!(!text.contains(rows[10]["id"].as_str().unwrap()));
        for expected in [
            "Session ID prefix",
            "give more of the ID",
            "matches 12 Sessions",
            "2 more matching Sessions omitted",
            "[archived]",
            "[active]",
            "日本語\\u{1b}[31m\\u{a}ไทย",
            "sessions list --archived",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains('\u{1b}'));
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListSessions]
        ));
    }
    *fixture.session_rows.lock().unwrap() = json!([
        {"id":"id-first","name":"Same 日本語","created_at":"","updated_at":""},
        {"id":"id-second","name":"same 日本語","archived":true,"created_at":"","updated_at":""}
    ]);
    let out = fixture.run(&["sessions", "show", "  SAME 日本語 "]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let text = String::from_utf8(out.stderr).unwrap();
    for expected in [
        "Session name",
        "choose one of the displayed IDs",
        "id-first",
        "id-second",
        "[archived]",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    for id in ["id-first", "id-second"] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&["sessions", "show", id, "--json"]);
        assert!(out.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap(),
            session_snapshot(
                fixture
                    .session_rows
                    .lock()
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["id"] == id)
                    .unwrap()
                    .clone()
            )
        );
        assert!(
            matches!(&fixture.requests.lock().unwrap()[..],[Request::ListSessions,Request::GetSession{id:requested}] if requested==id)
        );
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
fn shipped_tool_lookup_misses_never_continue_to_an_action_and_export_stays_exact() {
    let fixture = Fixture::new();
    for action in ["show", "disable", "export"] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&["tools", action, "FP"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        let error = String::from_utf8(out.stderr).unwrap();
        assert!(
            error.contains("Partial-name suggestions") && error.contains("\"FetchPage\""),
            "{error}"
        );
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListTools]
        ));
    }
    for query in ["full", "unknown", ""] {
        let out = fixture.run(&["tools", "export", query]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8(out.stderr)
            .unwrap()
            .contains("formless tools list"));
    }
    for query in ["FetchPage", "full-tool-id"] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&["tools", "export", query]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap(),
            json!({"fixture":"unchanged bundle"})
        );
        assert!(
            matches!(&fixture.requests.lock().unwrap()[..],[Request::ListTools,Request::ExportTool{id}] if id=="full-tool-id")
        );
    }
}

fn proposal_comparison(id: &str) -> Value {
    json!({"target":if id=="memory" {"memory"} else {"learned guidance"},"applied":id=="memory","diff":[["-","old 日本語"],["+","ไทย long full line ".repeat(80)],[" ",""]],"future":{"preserved":true}})
}

#[test]
fn shipped_proposal_diff_is_read_only_and_preserves_the_complete_comparison() {
    let fixture = Fixture::new();
    for id in ["guidance", "memory"] {
        for json in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["prompts", "diff", id];
            if json {
                args.push("--json");
            }
            let out = fixture.run(&args);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let expected = proposal_comparison(id);
            if json {
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    expected
                );
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                assert!(text.contains(&format!("Target: {}", expected["target"].as_str().unwrap())));
                assert!(text.contains(if id == "memory" {
                    "Already applied: yes"
                } else {
                    "Already applied: no"
                }));
                assert!(text.ends_with(&format!(
                    "-old 日本語\n+{}\n \n",
                    "ไทย long full line ".repeat(80)
                )));
            }
            let requests = fixture.requests.lock().unwrap();
            assert_eq!(requests.len(), 1, "{requests:?}");
            assert!(matches!(&requests[0],Request::ProposalDiff{id:actual} if actual==id));
        }
    }
    let missing = fixture.run(&["prompts", "diff", "missing"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8(missing.stderr)
        .unwrap()
        .contains("proposal not found"));
}
fn tool_filter_rows() -> Value {
    serde_json::from_str(include_str!("fixtures/tool-filter-list.json")).unwrap()
}

#[test]
fn shipped_tool_filters_keep_default_pipe_bytes_and_apply_the_same_json_selection() {
    let fixture = Fixture::with_tools(tool_filter_rows());
    let default = fixture.run(&["tools", "list"]);
    assert!(
        default.status.success(),
        "{}",
        String::from_utf8_lossy(&default.stderr)
    );
    assert_eq!(
        default.stdout,
        include_bytes!("fixtures/tool-list-default.txt")
    );
    let pinned = fixture.run(&["tools", "list", "--show-pinned"]);
    assert!(pinned.status.success());
    let text = String::from_utf8(pinned.stdout).unwrap();
    assert!(text.contains("PINNED") && text.contains("yes") && text.contains("no"));
    for (flags, expected) in [
        (
            vec!["--filter", "not-retired"],
            vec!["Alpha", "Beta", "Delta"],
        ),
        (vec!["--filter", "failing"], vec!["Beta", "Gamma"]),
        (
            vec!["--filter", "pinned", "--tag", "#WORK"],
            vec!["Alpha", "Gamma"],
        ),
        (vec!["--filter", "off", "--tag", "work"], vec![]),
    ] {
        for json in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["tools", "list"];
            args.extend(flags.iter().copied());
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
                let rows: Value = serde_json::from_slice(&out.stdout).unwrap();
                assert_eq!(
                    rows.as_array()
                        .unwrap()
                        .iter()
                        .map(|row| row["name"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    expected
                );
                for row in rows.as_array().unwrap() {
                    let original: formless_protocol::ToolInfo = serde_json::from_value(
                        tool_filter_rows()
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|old| old["id"] == row["id"])
                            .unwrap()
                            .clone(),
                    )
                    .unwrap();
                    assert_eq!(row, &serde_json::to_value(original).unwrap());
                }
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                if expected.is_empty() {
                    assert!(text.contains("No Tools match --filter off and --tag \"work\""));
                    assert!(!text.contains("no tools yet"));
                }
                for name in ["Alpha", "Beta", "Gamma", "Delta"] {
                    assert_eq!(text.contains(name), expected.contains(&name), "{text}");
                }
            }
            assert!(matches!(
                &fixture.requests.lock().unwrap()[..],
                [Request::ListTools]
            ));
        }
    }
    let invalid = fixture.run(&["tools", "list", "--filter", "active"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
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
