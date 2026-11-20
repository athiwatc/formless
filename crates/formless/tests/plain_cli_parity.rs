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

type ToolRunReply = Result<Value, &'static str>;

fn tool_run_response(id: u64, reply: ToolRunReply) -> Value {
    match reply {
        Ok(value) => json!({"kind":"response","id":id,"ok":true,"result":value}),
        Err(error) => json!({"kind":"response","id":id,"ok":false,"error":error}),
    }
}

struct Fixture {
    root: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    requests: Arc<Mutex<Vec<Request>>>,
    connections: Arc<AtomicUsize>,
    invocations: Arc<Mutex<Value>>,
    invocation_error: Arc<AtomicBool>,
    usage_failure: Arc<AtomicUsize>,
    gc_failure: Arc<AtomicBool>,
    session_rows: Arc<Mutex<Value>>,
    organization_failure: Arc<AtomicBool>,
    tool_run_reply: Arc<Mutex<Option<ToolRunReply>>>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_tool_details(false)
    }
    fn with_tool_details(tool_details: bool) -> Self {
        Self::with_tool_rows_and_details(
            json!([{"id":"full-tool-id","name":"FetchPage","description":"","retired":false,"current_version":1,"capabilities":[],"memory_mb":16,"invocations":0,"success_rate":null,"needs_attention":false,"created_at":"","updated_at":""}]),
            tool_details,
        )
    }
    fn with_tools(tool_rows: Value) -> Self {
        Self::with_tool_rows_and_details(tool_rows, false)
    }
    fn with_tool_rows_and_details(tool_rows: Value, tool_details: bool) -> Self {
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
        let gc_failure = Arc::new(AtomicBool::new(false));
        let fail_gc = gc_failure.clone();
        let usage_failure = Arc::new(AtomicUsize::new(0));
        let failure = usage_failure.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let connected = connections.clone();
        let invocations = Arc::new(Mutex::new(json!([])));
        let invocation_reply = invocations.clone();
        let invocation_error = Arc::new(AtomicBool::new(false));
        let fail_invocations = invocation_error.clone();
        let tool_run_reply = Arc::new(Mutex::new(None::<ToolRunReply>));
        let tool_result = tool_run_reply.clone();
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
                let mut awaiting_tool_approval = None;
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
                    if fail_gc.load(Ordering::SeqCst)
                        && matches!(&envelope.request, Request::Gc { .. })
                    {
                        seen.lock().unwrap().push(envelope.request.clone());
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
                    if matches!(&envelope.request, Request::RestartMcpServer{name} if name=="request-failure")
                    {
                        seen.lock().unwrap().push(envelope.request.clone());
                        let reply =
                            serde_json::to_value(formless_protocol::ServerMessage::err_code(
                                envelope.id,
                                formless_protocol::codes::NOT_FOUND,
                                "fixture MCP request missing".into(),
                            ))
                            .unwrap();
                        if writeln!(socket, "{reply}").is_err() {
                            break;
                        }
                        line.clear();
                        continue;
                    }
                    if let Request::RunTool { id, session_id, .. } | Request::DryRunTool { id, session_id, .. } = &envelope.request {
                        assert_eq!(id, "full-tool-id");
                        assert_eq!(session_id, "owned-tool-session");
                        let fixture_reply = tool_result.lock().unwrap().clone();
                        let reply = if let Some(fixture_reply) = fixture_reply {
                            tool_run_response(envelope.id, fixture_reply)
                        } else if matches!(&envelope.request, Request::RunTool { .. }) {
                            assert!(awaiting_tool_approval.replace(envelope.id).is_none());
                            json!({
                                "kind": "event", "type": "approval_requested",
                                "approval_id": "fixture-approval", "session_id": session_id,
                                "tool_name": "FetchPage", "version": 1,
                                "capabilities": ["fixture-capability"], "memory_mb": 16
                            })
                        } else {
                            tool_run_response(envelope.id, Ok(json!({"ok":true,"result":"synthetic preview","effects":[]})))
                        };
                        seen.lock().unwrap().push(envelope.request);
                        if writeln!(socket, "{reply}").is_err() {
                            break;
                        }
                        line.clear();
                        continue;
                    }
                    if let Request::ApprovalDecision { approved, .. } = &envelope.request {
                        let run = awaiting_tool_approval.take().expect("private mock Tool Run");
                        let result = if *approved {
                            Ok(json!({"ok":true,"result":"synthetic result"}))
                        } else {
                            Err("fixture call denied")
                        };
                        let reply = tool_run_response(run, result);
                        seen.lock().unwrap().push(envelope.request);
                        if writeln!(socket, "{reply}").is_err() {
                            break;
                        }
                        line.clear();
                        continue;
                    }
                    let result = match &envelope.request {
                        Request::CreateSession { name } => {
                            assert_eq!(name.as_deref(), Some("tools run FetchPage"));
                            json!({"id":"owned-tool-session"})
                        }
                        Request::SetSessionArchived { id, archived } => {
                            assert_eq!(id, "owned-tool-session");
                            assert!(*archived);
                            json!({})
                        }
                        Request::SetSessionPinned { id, pinned } => {
                            organization_reply(id, Some(!pinned), false)
                        }
                        Request::SetSessionTags { id, tags } => {
                            organization_reply(id, None, tags.is_empty())
                        }
                        Request::ListModules => module_list(),
                        Request::GetModule { .. } => module_detail(),
                        Request::ListTools if tool_details => json!([
                            detail_tool("tool-newline", "reader", 1),
                            detail_tool("tool-no-newline", "reader-no-newline", 2),
                            detail_tool("tool-missing-current", "reader-missing", 99)
                        ]),
                        Request::GetTool { id } if tool_details => detail_report(id),
                        Request::ListToolMetrics { since: None } if tool_details => {
                            tool_success_rate_rows()
                        }
                        Request::ListBuiltinStats { since: None }
                        | Request::ListModelStats { since: None }
                            if tool_details =>
                        {
                            json!([])
                        }
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
                            limit: Some(4),
                            session_id: None,
                            since: None,
                        } => run_report(),
                        Request::RunMetrics {
                            limit,
                            session_id,
                            since,
                        } => filtered_run_report(
                            limit.unwrap_or(20),
                            session_id.as_deref(),
                            since.as_deref(),
                        ),
                        Request::RunFiles { session_id, run_id } => {
                            assert_eq!(session_id, "abc-one");
                            json!({"run_id":run_id.as_deref().unwrap_or("latest"), "files":[{"path":"dir/a.txt","status":"modified","revertible":true,"diff":"- old\n+ new"}], "untracked_writes_possible":true})
                        }
                        Request::RewindRun { session_id, .. } => {
                            assert_eq!(session_id, "abc-one");
                            json!({"run_id":"run-one", "restored":["dir/a.txt"], "skipped":[{"path":"dir/changed.txt","reason":"changed since Run"}]})
                        }
                        Request::PreviewSchedule { schedule } => {
                            match schedule_preview_fixture(schedule) {
                                Ok(preview) => preview,
                                Err(error) => {
                                    seen.lock().unwrap().push(envelope.request);
                                    let reply = formless_protocol::ServerMessage::err(
                                        envelope.id,
                                        error.to_string(),
                                    );
                                    let reply = serde_json::to_string(&reply).unwrap();
                                    if writeln!(socket, "{reply}").is_err() {
                                        break;
                                    }
                                    line.clear();
                                    continue;
                                }
                            }
                        }
                        Request::SearchSessions { .. } => timestamp_search_hits(),
                        Request::ListApprovals => timestamp_grants(),
                        Request::ListDenials => timestamp_denials(),
                        Request::ListRelayClients => timestamp_devices(),
                        Request::ListJobs => timestamp_jobs(),
                        Request::JobRuns { id, .. } => timestamp_job_runs(id),
                        Request::Gc { dry_run } => gc_report(*dry_run),
                        Request::RestartMcpServer { name } => mcp_test_report(name),
                        Request::AddMcpServer { .. } => {
                            json!({"added":"fixture-add","tools":null,"error":"fixture embedded add error"})
                        }
                        Request::SessionUsage { .. } => session_usage_report(),
                        Request::ContextUsage { .. } => session_context_report(),
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
                        Request::Subscribe { .. } => json!({"conn":1}),
                        Request::GetRun { run_id } => inspected_run(run_id),
                        Request::CancelRunId { run_id } => cancellation_reply(run_id),
                        Request::UsageByDay { .. } => grouped_usage(),
                        Request::ListInvocations => invocation_reply.lock().unwrap().clone(),
                        _ => panic!("unexpected private RPC: {:?}", envelope.request),
                    };
                    let fail = match &envelope.request {
                        Request::SessionUsage { .. } => failure.load(Ordering::SeqCst) == 1,
                        Request::ContextUsage { .. } => failure.load(Ordering::SeqCst) == 2,
                        _ => false,
                    };
                    let reply = if matches!(&envelope.request, Request::ListInvocations)
                        && fail_invocations.load(Ordering::SeqCst)
                    {
                        json!({"kind":"response","id":envelope.id,"ok":false,"error":"fixture invocation failure"})
                    } else if matches!(&envelope.request, Request::RunMemories {run_id:Some(id),..} if id=="fixture-failure")
                    {
                        serde_json::to_value(formless_protocol::ServerMessage::err_code(
                            envelope.id,
                            formless_protocol::codes::NOT_FOUND,
                            "daemon fixture failure".into(),
                        ))
                        .unwrap()
                    } else if matches!(&envelope.request, Request::GetSubagent {id} if id=="fixture-not-found")
                    {
                        serde_json::to_value(formless_protocol::ServerMessage::err_code(
                            envelope.id,
                            formless_protocol::codes::NOT_FOUND,
                            "sub-agent fixture missing".into(),
                        ))
                        .unwrap()
                    } else if fail {
                        serde_json::to_value(formless_protocol::ServerMessage::err(
                            envelope.id,
                            "fixture usage read failure".into(),
                        ))
                        .unwrap()
                    } else if matches!(&envelope.request,Request::GetModule{id} if id=="missing") {
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
            invocations,
            invocation_error,
            usage_failure,
            gc_failure,
            session_rows,
            organization_failure,
            tool_run_reply,
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
        Self::capture(self.command(args))
    }

    fn run_in_timezone(&self, args: &[&str], timezone: &str) -> std::process::Output {
        let mut command = self.command(args);
        command.env("TZ", timezone);
        Self::capture(command)
    }

    fn capture(command: Command) -> std::process::Output {
        Self::capture_with_input(command, None)
    }

    fn capture_with_input(mut command: Command, input: Option<&[u8]>) -> std::process::Output {
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().unwrap();
        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input).unwrap();
        }
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
fn shipped_tool_run_json_preserves_replies_verdicts_and_cleanup() {
    let fixture = Fixture::new();
    let run = |dry_run: bool, json: bool, reply: ToolRunReply| {
        *fixture.tool_run_reply.lock().unwrap() = Some(reply);
        fixture.requests.lock().unwrap().clear();
        let mut args = vec!["tools", "run", "FetchPage", "--input", r#"{"x":"ไทย"}"#];
        if dry_run {
            args.push("--dry-run");
        }
        if json {
            args.push("--json");
        }
        let output = fixture.run(&args);
        let requests = fixture.requests.lock().unwrap();
        assert!(
            matches!(&requests[..], [
                Request::ListTools,
                Request::CreateSession { .. },
                Request::RunTool { .. },
                Request::SetSessionArchived { archived: true, .. }
            ] if !dry_run)
                || matches!(&requests[..], [
                Request::ListTools,
                Request::CreateSession { .. },
                Request::DryRunTool { .. },
                Request::SetSessionArchived { archived: true, .. }
            ] if dry_run),
            "{requests:?}"
        );
        output
    };
    let text_json = json!({"ok":true,"result":"  {\"text\": \"ไทย 🦀\"}\n","extra":{"n":0}});
    for (dry_run, status, value) in [
        (
            false,
            0,
            json!({"ok":true,"result":"error is part of this successful text"}),
        ),
        (false, 0, text_json.clone()),
        (
            false,
            1,
            json!({"ok":false,"result":"unavailable","extra":null}),
        ),
        (false, 1, json!({"result":"\n error: legacy"})),
        (false, 0, json!({"result":"ordinary legacy reply"})),
        (
            true,
            0,
            json!({"ok":true,"result":{"text":"ไทย","n":false},"effects":["write fixture"],"logs":["debug ไทย"],"extra":{"kept":true}}),
        ),
        (
            true,
            1,
            json!({"ok":false,"error":"preview failed","effects":[],"logs":["fixture report"]}),
        ),
    ] {
        let out = run(dry_run, true, Ok(value.clone()));
        assert_eq!(out.status.code(), Some(status), "{:?}", out.stderr);
        assert_eq!(
            out.stdout,
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap()).into_bytes()
        );
        assert!(out.stderr.is_empty(), "{:?}", out.stderr);
    }
    for dry_run in [false, true] {
        let out = run(dry_run, true, Err("mock request refused"));
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(out.stderr, b"mock request refused\n");
        assert!(out.stdout.is_empty());
    }
    let human = run(false, false, Ok(text_json));
    assert_eq!(human.stdout, "{\n  \"text\": \"ไทย 🦀\"\n}\n".as_bytes());
    assert_eq!(human.status.code(), Some(0));
    let human_dry = run(
        true,
        false,
        Ok(
            json!({"ok":false,"error":"preview failed","effects":["write fixture"],"logs":["invisible"]}),
        ),
    );
    assert_eq!(human_dry.status.code(), Some(1));
    assert!(human_dry.stdout.is_empty());
    assert_eq!(human_dry.stderr, b"dry run: nothing was changed.\nit would have:\n  - write fixture\nerror: preview failed\n");
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
        let out = fixture.run_in_timezone(&args, "Asia/Bangkok");
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
                "2026-11-01 19:34 +07:00",
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

fn timestamp_jobs() -> Value {
    json!([
        {"id":"clock","approved":true,"enabled":true,"cron":"0 9 * * *",
         "next_run":"2026-11-01T09:00:00+07:00","last_status":"completed","prompt":"clock fixture"},
        {"id":"legacy","next_run":"unparseable","prompt":"legacy fixture"},
        {"id":"missing","next_run":null,"prompt":"missing fixture"}
    ])
}

fn timestamp_job_runs(id: &str) -> Value {
    let row = |id, started, finished| {
        json!({
            "id":id, "job_id":"clock", "session_id":null, "trigger":"schedule",
            "status":"completed", "cost":null, "detail":"fixture",
            "started_at":started, "finished_at":finished
        })
    };
    if id == "fold" {
        json!([
            row("first", "2026-11-01T05:30:00Z", None),
            row("second", "2026-11-01T06:30:00Z", None)
        ])
    } else {
        assert_eq!(id, "clock");
        json!([row(
            "done",
            "2026-11-01T02:00:00Z",
            Some("2026-11-01T09:00:00.499+07:00")
        )])
    }
}

fn successful_text(output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn shipped_jobs_timestamps_share_local_time_and_keep_wire_values() {
    let fixture = Fixture::new();
    let timezone = "Asia/Bangkok";
    for (args, expected_json) in [
        (&["jobs", "list"][..], timestamp_jobs()),
        (
            &["jobs", "history", "clock"][..],
            timestamp_job_runs("clock"),
        ),
    ] {
        let text = successful_text(fixture.run_in_timezone(args, timezone));
        assert!(text.contains("2026-11-01 09:00 +07:00"), "{text}");
        let json_args = args.iter().copied().chain(["--json"]).collect::<Vec<_>>();
        let text_json = successful_text(fixture.run_in_timezone(&json_args, timezone));
        assert_eq!(
            serde_json::from_str::<Value>(&text_json).unwrap(),
            expected_json
        );
        if args[1] == "list" {
            assert!(text.contains("unparseable") && text.contains("missing"));
        } else {
            assert!(text.contains("499ms"), "{text}");
        }
    }

    let text =
        successful_text(fixture.run_in_timezone(&["jobs", "history", "fold"], "America/New_York"));
    assert!(text.contains("2026-11-01 01:30 -04:00"), "{text}");
    assert!(text.contains("2026-11-01 01:30 -05:00"), "{text}");
    let text = successful_text(fixture.run_in_timezone(&["jobs", "list"], "America/New_York"));
    assert!(text.contains("2026-10-31 22:00 -04:00"), "{text}");
}

fn timestamp_search_hits() -> Value {
    json!([{
        "session_id":"session-clock","session_name":"Clock",
        "message_id":"message-clock","seq":3,"role":"assistant",
        "snippet":"matching synthetic result","created_at":"2026-10-31T22:30:00-03:30"
    }])
}

fn timestamp_grants() -> Value {
    json!([{
        "tool_id":"fixture-tool","tool_name":"Fixture","capability":"net:example.test",
        "scope":"hour","granted_by":"fixture","trusted":false,"session_id":null,
        "granted_at":"2026-11-01T02:00:00Z",
        "expires_at":"2026-11-01T10:00:00+07:00",
        "last_used":"2026-10-31T23:15:00-03:30"
    }])
}

fn timestamp_denials() -> Value {
    json!([
        {"capability":"net:example.test","tool_name":"Fixture","denied_by":"fixture",
         "created_at":"2026-11-01T02:00:00Z"},
        {"capability":"net:legacy.test","tool_name":"Legacy","denied_by":"",
         "created_at":"2026-11-01T02:00 legacy"}
    ])
}

fn timestamp_devices() -> Value {
    json!([{
        "cid":"device-clock","user_agent":"Synthetic Browser",
        "first_seen":"2026-11-01T02:00:00Z","last_seen":"2026-11-01T10:00:00+07:00"
    }])
}

#[test]
fn shipped_session_run_and_approval_timestamps_use_numeric_local_offsets() {
    let fixture = Fixture::new();
    let session = json!({
        "id":"session-clock","name":"Clock","created_at":"2026-11-01T01:00:00Z",
        "updated_at":"2026-11-01T02:00:00Z"
    });
    *fixture.session_rows.lock().unwrap() = json!([session.clone()]);
    let parsed: formless_protocol::SessionInfo = serde_json::from_value(session.clone()).unwrap();
    let timezone = "Asia/Bangkok";
    for (args, expected, wire) in [
        (
            &["sessions", "list"][..],
            &["2026-11-01 09:00 +07:00"][..],
            json!([parsed]),
        ),
        (
            &["sessions", "show", "session-clock"][..],
            &["updated 2026-11-01 09:00 +07:00"][..],
            session_snapshot(session),
        ),
        (
            &["sessions", "search", "synthetic"][..],
            &["#3 2026-11-01 09:00 +07:00"][..],
            timestamp_search_hits(),
        ),
        (
            &["runs", "--limit", "4"][..],
            &["2026-11-01 19:34:56 +07:00"][..],
            run_report(),
        ),
        (
            &["approvals", "list"][..],
            &[
                "2026-11-01 09:00 +07:00",
                "2026-11-01 10:00 +07:00",
                "2026-11-01 09:45 +07:00",
            ][..],
            timestamp_grants(),
        ),
        (
            &["approvals", "denials"][..],
            &["2026-11-01 09:00 +07:00", "Legacy", "2026-11-01 02:00"][..],
            timestamp_denials(),
        ),
    ] {
        let human = successful_text(fixture.run_in_timezone(args, timezone));
        for part in expected {
            assert!(human.contains(part), "{args:?}: {human}");
        }
        let args = args.iter().copied().chain(["--json"]).collect::<Vec<_>>();
        let output = successful_text(fixture.run_in_timezone(&args, timezone));
        assert_eq!(serde_json::from_str::<Value>(&output).unwrap(), wire);
    }
}

#[test]
fn shipped_relay_device_timestamps_preserve_offsets_in_json() {
    let fixture = Fixture::new();
    let run_status = |args: &[&str]| {
        let mut command = fixture.command(args);
        // Avoid reading the user's keyring even on a machine with stored relay credentials.
        command
            .env("FORMLESS_RELAY_SECRET", "synthetic-timestamp-fixture")
            .env("TZ", "Asia/Bangkok");
        successful_text(Fixture::capture(command))
    };
    let human = run_status(&["relay", "status"]);
    assert!(human.contains(
        "Synthetic Browser  (since 2026-11-01 09:00 +07:00, last seen 2026-11-01 10:00 +07:00)"
    ));
    let json = serde_json::from_str::<Value>(&run_status(&["relay", "status", "--json"])).unwrap();
    assert_eq!(json["devices"], timestamp_devices());
    assert_eq!(json["daemon_running"], true);
    assert_eq!(json.as_object().unwrap().len(), 5);
}

fn schedule_preview_fixture(schedule: &str) -> anyhow::Result<Value> {
    let cron = formless_core::jobs::to_cron(schedule)?;
    let next: &[&str] = match cron.as_str() {
        "0 9 * * 1-5" => &[
            "2026-11-23T09:00:00+07:00",
            "2026-11-24T09:00:00+07:00",
            "2026-11-25T09:00:00+07:00",
        ],
        "0 0 29 2 *" => &["2096-02-29T00:00:00+07:00"],
        "0 0 31 2 *" => &[],
        _ => panic!("unexpected preview cron: {cron}"),
    };
    Ok(json!({"cron":cron,"next":next,"future":"日本語 · เวลา"}))
}

#[test]
fn shipped_jobs_preview_requests_only_the_daemons_schedule_preview() {
    let fixture = Fixture::new();
    for (schedule, count) in [
        ("every weekday at 9am", 3),
        ("0 9 * * 1-5", 3),
        ("0 0 29 2 *", 1),
        ("0 0 31 2 *", 0),
    ] {
        let expected = schedule_preview_fixture(schedule).unwrap();
        let next = expected["next"].as_array().unwrap();
        assert_eq!(next.len(), count);
        for json_mode in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["jobs", "preview", schedule];
            if json_mode {
                args.push("--json");
            }
            let out = fixture.run(&args);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let stdout = String::from_utf8(out.stdout).unwrap();
            if json_mode {
                assert_eq!(serde_json::from_str::<Value>(&stdout).unwrap(), expected);
                assert!(stdout.contains("日本語 · เวลา"));
            } else {
                let mut lines = stdout.lines();
                let cron = expected["cron"].as_str().unwrap();
                assert_eq!(lines.next(), Some(format!("Cron: {cron}").as_str()));
                if count == 0 {
                    assert_eq!(lines.next(), Some("No upcoming times were returned."));
                    assert!(lines.next().is_none());
                } else {
                    assert_eq!(
                        lines.next(),
                        Some("Projected from the daemon's current local time:")
                    );
                    assert_eq!(
                        lines.map(str::trim).collect::<Vec<_>>(),
                        next.iter()
                            .map(|time| time.as_str().unwrap())
                            .collect::<Vec<_>>()
                    );
                }
            }
            assert!(matches!(
                &fixture.requests.lock().unwrap()[..],
                [Request::PreviewSchedule { schedule: sent }] if sent == schedule
            ));
        }
    }
}

#[test]
fn shipped_jobs_preview_help_and_parser_errors_leave_jobs_untouched() {
    let fixture = Fixture::new();
    let help = fixture.run(&["jobs", "preview", "--help"]);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    for expected in [
        "recurring schedule",
        "daemon's current local time",
        "jobs add",
        "--json",
    ] {
        assert!(help.contains(expected), "{help}");
    }
    let invalid_flag = fixture.run(&["jobs", "preview", "--at", "in 30m"]);
    assert!(!invalid_flag.status.success() && invalid_flag.stdout.is_empty());
    assert!(fixture.requests.lock().unwrap().is_empty());

    for schedule in ["60 9 * * *", "in 30m"] {
        let expected_error = formless_core::jobs::to_cron(schedule)
            .unwrap_err()
            .to_string();
        for extra in [&[][..], &["--json"]] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["jobs", "preview", schedule];
            args.extend_from_slice(extra);
            let out = fixture.run(&args);
            assert!(!out.status.success() && out.stdout.is_empty());
            assert!(String::from_utf8(out.stderr)
                .unwrap()
                .contains(&expected_error));
            assert!(matches!(
                &fixture.requests.lock().unwrap()[..],
                [Request::PreviewSchedule { schedule: sent }] if sent == schedule
            ));
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

fn mcp_test_report(name: &str) -> Value {
    match name {
        "bad-shape" => json!({"future":true}),
        "wrong-name" => json!({"restarted":"other","tools":0,"error":null}),
        _ => {
            json!({"restarted":name,"tools":match name {"ok"=>json!(3),"zero"=>json!(0),_=>Value::Null},"error":match name {"failed"|"missing"=>json!("fixture restart error 日本語"),"empty-error"=>json!(""),"bad-error"=>json!(false),_=>Value::Null},"future":{"retained":true}})
        }
    }
}

#[test]
fn shipped_mcp_test_reports_verdict_after_full_output_and_leaves_add_behavior_alone() {
    let fixture = Fixture::new();
    for (name, code) in [
        ("ok", 0),
        ("zero", 0),
        ("failed", 1),
        ("missing", 1),
        ("empty-error", 1),
        ("bad-tools", 1),
        ("bad-error", 1),
        ("bad-shape", 1),
        ("wrong-name", 1),
    ] {
        for json_mode in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["mcp", "test", name];
            if json_mode {
                args.push("--json");
            }
            let out = fixture.run(&args);
            assert_eq!(
                out.status.code(),
                Some(code),
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            if json_mode {
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    mcp_test_report(name)
                );
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                assert!(text.starts_with(&format!("server:  {name}\n")));
                if name == "ok" {
                    assert_eq!(text, "server:  ok\ntools:   3\n");
                }
                if name == "zero" {
                    assert_eq!(text, "server:  zero\ntools:   0\n");
                }
                if name == "failed" {
                    assert!(text.contains("fixture restart error 日本語"));
                }
            }
            if ["ok", "zero", "failed", "missing", "empty-error"].contains(&name) {
                assert!(
                    out.stderr.is_empty(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                );
            } else {
                assert!(String::from_utf8_lossy(&out.stderr).contains("invalid MCP restart reply"));
            }
            assert!(
                matches!(&fixture.requests.lock().unwrap()[..],[Request::RestartMcpServer{name:actual}] if actual==name)
            );
        }
    }
    for json_mode in [false, true] {
        let mut args = vec!["mcp", "test", "request-failure"];
        if json_mode {
            args.push("--json");
        }
        let out = fixture.run(&args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8(out.stderr)
            .unwrap()
            .contains("fixture MCP request missing"));
    }
    fixture.requests.lock().unwrap().clear();
    let out = fixture.run(&[
        "mcp",
        "add",
        "fixture-add",
        "--",
        "owned-not-executed",
        "argument",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .contains("fixture embedded add error"));
    assert!(
        matches!(&fixture.requests.lock().unwrap()[..],[Request::AddMcpServer{name,..}] if name=="fixture-add")
    );
}

fn detail_tool(id: &str, name: &str, current: i64) -> Value {
    json!({"id":id,"name":name,"pinned":true,"description":"fixture","retired":false,"current_version":current,"capabilities":[],"memory_mb":16,"invocations":0,"success_rate":null,"needs_attention":false,"created_at":"","updated_at":""})
}
fn detail_report(id: &str) -> Value {
    let (name, current) = match id {
        "tool-no-newline" => ("reader-no-newline", 2),
        "tool-missing-current" => ("reader-missing", 99),
        _ => ("reader", 1),
    };
    let metrics = match id {
        "tool-newline" => fixture_tool_metrics(199, 1, Some(0.995)),
        "tool-no-newline" => fixture_tool_metrics(9999, 1, Some(0.9999)),
        _ => fixture_tool_metrics(4, 0, Some(1.0)),
    };
    json!({"tool":detail_tool(id,name,current),"versions":[
        {"version":2,"author":"agent","created_at":"second","capabilities":[],"memory_mb":16,"input_schema":"{}","output_schema":"{}","code":"// source ไทย"},
        {"version":1,"author":"user","created_at":"first","capabilities":[],"memory_mb":16,"input_schema":"{}","output_schema":"{}","code":"// source 日本語\n"}
    ],"metrics":metrics,"recent_calls":[{"id":"call-one","version":1,"status":"failed","input":"PrivateCallInputSentinel","output":"PrivateCallOutputSentinel","error":"fixture call failed","duration_ms":12,"created_at":"call-time"}]})
}

fn fixture_tool_metrics(
    success: i64,
    failure: i64,
    success_rate: Option<f64>,
) -> formless_protocol::ToolMetricsInfo {
    formless_protocol::ToolMetricsInfo {
        total: success + failure,
        success,
        failure,
        success_rate,
        ..Default::default()
    }
}

fn tool_success_rate_rows() -> Value {
    json!([
        {"tool_id":"unknown","invocations":0,"success_rate":null},
        {"tool_id":"zero","invocations":3,"success_rate":0.0},
        {"tool_id":"fraction","invocations":3,"success_rate":2.0 / 3.0},
        {"tool_id":"near","invocations":200,"success_rate":0.995},
        {"tool_id":"rare","invocations":10000,"success_rate":0.9999},
        {"tool_id":"perfect","invocations":4,"success_rate":1.0},
    ])
}

#[test]
fn shipped_tool_detail_success_rate_labels_leave_json_numeric() {
    let fixture = Fixture::with_tool_details(true);
    for (name, rate, label) in [
        ("reader", 0.995, ">99%"),
        ("reader-no-newline", 0.9999, ">99%"),
        ("reader-missing", 1.0, "100%"),
    ] {
        let json: Value = serde_json::from_str(&successful_text(
            fixture.run(&["tools", "show", name, "--json"]),
        ))
        .unwrap();
        assert_eq!(json["metrics"]["success_rate"], serde_json::json!(rate));
        let text = successful_text(fixture.run(&["tools", "show", name]));
        let calls = text
            .lines()
            .find(|line| line.starts_with("calls:"))
            .unwrap();
        assert!(
            calls.contains(&format!("· success {label} · p50 ")),
            "{calls}"
        );
    }
}

#[test]
fn shipped_metrics_success_rate_labels_leave_json_numeric() {
    let fixture = Fixture::with_tool_details(true);
    let json: Value =
        serde_json::from_str(&successful_text(fixture.run(&["metrics", "--json"]))).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"models":[],"tools":tool_success_rate_rows(),"builtins":[]})
    );
    let text = successful_text(fixture.run(&["metrics"]));
    for (id, expected) in [
        ("unknown", "-"),
        ("zero", "0%"),
        ("fraction", "67%"),
        ("near", ">99%"),
        ("rare", ">99%"),
        ("perfect", "100%"),
    ] {
        let row = text
            .lines()
            .find(|line| line.split_whitespace().next() == Some(id))
            .unwrap();
        assert_eq!(row.split_whitespace().last(), Some(expected), "{row}");
    }
}

#[test]
fn shipped_tool_detail_source_mode_is_exact_and_human_history_is_readable() {
    let fixture = Fixture::with_tool_details(true);
    for (name, expected) in [
        ("reader", "// source 日本語\n"),
        ("reader-no-newline", "// source ไทย"),
    ] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&["tools", "show", name, "--code"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, expected.as_bytes());
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListTools, Request::GetTool { .. }]
        ));
    }
    let missing = fixture.run(&["tools", "show", "reader-missing", "--code"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(missing.stdout.is_empty());
    let human = fixture.run(&["tools", "show", "reader"]);
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    for expected in [
        "pinned:       yes",
        "versions (newest first):",
        "v2 · agent · second",
        "v1 · user · first",
        "call-time · v1 · failed · 12ms · fixture call failed",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    for private in [
        "PrivateCallInputSentinel",
        "PrivateCallOutputSentinel",
        "// source",
    ] {
        assert!(!text.contains(private));
    }
    let json = fixture.run(&["tools", "show", "reader", "--json"]);
    assert!(json.status.success());
    let expected: formless_protocol::ToolDetail =
        serde_json::from_value(detail_report("tool-newline")).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&json.stdout).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    fixture.requests.lock().unwrap().clear();
    let conflict = fixture.run(&["tools", "show", "reader", "--code", "--json"]);
    assert_eq!(conflict.status.code(), Some(2));
    assert!(conflict.stdout.is_empty());
    assert!(fixture.requests.lock().unwrap().is_empty());
}
fn session_usage_report() -> Value {
    let usage = json!({"steps":0,"prompt_tokens":10,"completion_tokens":2,"cached_tokens":0,"cache_write_tokens":1,"cost":null,"unpriced":["日本語 model"],"future":true});
    json!({"runs":12,"total":usage,"last_run":null,"future":"retained"})
}
fn session_context_report() -> Value {
    json!({"model":"モデル ไทย","used_tokens":99,"context_tokens":1000,"context_source":"configured","measured":false,"breakdown":{"system_prompt":10,"tool_schemas":11,"history":12,"attachments":3},"future":7})
}

#[test]
fn shipped_session_usage_resolves_existing_selectors_buffers_both_reports_and_keeps_units() {
    let fixture = Fixture::new();
    for selector in ["abc-one", "abc-o", "First", " first "] {
        for json in [false, true] {
            fixture.requests.lock().unwrap().clear();
            let mut args = vec!["sessions", "usage", selector];
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
                    json!({"session_id":"abc-one","usage":session_usage_report(),"context":session_context_report()})
                );
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                for expected in [
                    "Reported Run count: 12",
                    "latest Run usage unavailable",
                    "Estimated cost (USD): -",
                    "Unpriced models: 日本語 model",
                    "Used tokens (estimated): 99",
                    "Attachment entries in stored message metadata: 3",
                ] {
                    assert!(text.contains(expected), "{text}");
                }
            }
            assert!(
                matches!(&fixture.requests.lock().unwrap()[..],[Request::ListSessions,Request::SessionUsage{session_id:a},Request::ContextUsage{session_id:b}] if a=="abc-one"&&b=="abc-one")
            );
        }
    }
    for selector in ["abc", "unknown"] {
        fixture.requests.lock().unwrap().clear();
        let out = fixture.run(&["sessions", "usage", selector]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [Request::ListSessions]
        ));
    }
    for failure in [1, 2] {
        fixture.usage_failure.store(failure, Ordering::SeqCst);
        let out = fixture.run(&["sessions", "usage", "abc-one"]);
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8(out.stderr)
            .unwrap()
            .contains("fixture usage read failure"));
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

fn inspected_run(status: &str) -> Value {
    let mut value = json!({"run_id":"full-run-日本語-id","session_id":"session-ไทย","status":status,"error":if status=="failed" {Some("failure\n\u{1b}[31mไทย")} else {None},"created_at":"stored-created","updated_at":"stored-updated","start_message_seq":0,"end_message_seq":null,"steps":2,"tool_calls":3,"prompt_tokens":0,"completion_tokens":0,"future":{"retained":true}});
    if status == "paused" {
        value["spend"] = json!({"known_usd":0.25,"total_usd":null,"usage_complete":false,"spend_complete":false,"unpriced":["モデル"]});
    }
    value
}
fn cancellation_reply(id: &str) -> Value {
    json!({"run_id":"full-run-日本語-id","interrupted":id=="yes","future":7})
}

#[test]
fn shipped_run_inspection_reports_precede_existing_exits_and_json_stays_complete() {
    let fixture = Fixture::new();
    for (status, exit) in [
        ("running", 0),
        ("completed", 0),
        ("paused", 1),
        ("failed", 1),
        ("abandoned", 1),
        ("interrupted", 130),
        ("unknown", 0),
    ] {
        for mode in 0..3 {
            fixture.requests.lock().unwrap().clear();
            let args = match mode {
                0 => vec!["runs", "show", status],
                1 => vec!["runs", "show", status, "--json"],
                _ => vec!["runs", "--json", "show", status],
            };
            let out = fixture.run(&args);
            assert_eq!(
                out.status.code(),
                Some(exit),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            if mode > 0 {
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    inspected_run(status)
                );
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                assert!(
                    text.contains("Run ID: full-run-日本語-id")
                        && text.contains(&format!("Stored status: {status}"))
                );
                assert!(text.contains("Recorded prompt tokens: 0"));
                if status == "failed" {
                    assert!(!text.contains('\u{1b}'));
                    assert!(text.contains("failure\\u{a}\\u{1b}[31mไทย"));
                }
                if status == "paused" {
                    assert!(
                        text.contains("Known USD subtotal: 0.25")
                            && text.contains("Total USD: -")
                            && text.contains("Spend complete: false")
                    );
                }
            }
            assert!(
                matches!(&fixture.requests.lock().unwrap()[..],[Request::Subscribe{..},Request::GetRun{run_id}] if run_id==status)
            );
        }
    }
    for id in ["yes", "no"] {
        for mode in 0..3 {
            fixture.requests.lock().unwrap().clear();
            let args = match mode {
                0 => vec!["runs", "cancel", id],
                1 => vec!["runs", "cancel", id, "--json"],
                _ => vec!["runs", "--json", "cancel", id],
            };
            let out = fixture.run(&args);
            assert!(out.status.success());
            if mode > 0 {
                assert_eq!(
                    serde_json::from_slice::<Value>(&out.stdout).unwrap(),
                    cancellation_reply(id)
                );
            } else {
                let text = String::from_utf8(out.stdout).unwrap();
                assert!(text.contains(if id == "yes" {
                    "Cancellation requested"
                } else {
                    "No active accepting Run"
                }));
                assert!(!text.contains("stopped") && !text.contains("finished"));
            }
            assert!(
                matches!(&fixture.requests.lock().unwrap()[..],[Request::Subscribe{..},Request::CancelRunId{run_id}] if run_id==id)
            );
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

#[test]
fn shipped_tool_run_input_sources_keep_dispatch_and_once_approval() {
    let fixture = Fixture::new();
    let file = fixture.root.join("input.json");
    let text = "{\n  \"argument\": \"ไทย 日本語\"\n}";
    let input: Value = serde_json::from_str(text).unwrap();
    std::fs::write(&file, text).unwrap();
    let file = file.to_str().unwrap();
    for (args, stdin, expected, approved) in [
        (vec!["--input", text, "--yes"], None, input.clone(), true),
        (
            vec!["--input-file", file, "--yes"],
            None,
            input.clone(),
            true,
        ),
        (
            vec!["--input-file", "-", "--yes"],
            Some(text.as_bytes()),
            input.clone(),
            true,
        ),
        (vec!["--yes"], None, json!({}), true),
        (vec!["--input-file", file], None, input.clone(), false),
    ] {
        fixture.requests.lock().unwrap().clear();
        let args = ["tools", "run", "FetchPage"]
            .into_iter()
            .chain(args)
            .collect::<Vec<_>>();
        let out = Fixture::capture_with_input(fixture.command(&args), stdin);
        assert_eq!(out.status.code(), Some(i32::from(!approved)), "{out:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        if approved {
            assert_eq!(out.stdout, b"synthetic result\n");
            assert!(stderr.contains("sending approval for this call"));
        } else {
            assert!(out.stdout.is_empty());
            assert!(stderr.contains("sending denial") && stderr.contains("fixture call denied"));
        }
        use formless_protocol::ApprovalScope;
        assert!(matches!(
            &fixture.requests.lock().unwrap()[..],
            [
                Request::ListTools,
                Request::CreateSession { .. },
                Request::RunTool { id, input, session_id },
                Request::ApprovalDecision { approval_id, approved: actual, scope: ApprovalScope::Once, remember_denial: false, .. },
                Request::SetSessionArchived { id: archived_id, archived: true }
            ] if id == "full-tool-id" && *input == expected
                && session_id == "owned-tool-session"
                && archived_id == session_id && approval_id == "fixture-approval"
                && *actual == approved
        ));
    }
    fixture.requests.lock().unwrap().clear();
    let preview = fixture.run(&[
        "tools",
        "run",
        "FetchPage",
        "--input-file",
        file,
        "--dry-run",
    ]);
    assert!(preview.status.success(), "{preview:?}");
    assert!(matches!(&fixture.requests.lock().unwrap()[..],
        [Request::ListTools, Request::CreateSession{..}, Request::DryRunTool{input:actual,..}, Request::SetSessionArchived{..}]
        if *actual == input));
}

#[test]
fn shipped_tool_run_rejects_bad_input_without_opening_a_profile_or_contacting_daemon() {
    let fixture = Fixture::new();
    let input = fixture.root.join("invalid.json");
    let missing = fixture.root.join("missing.json");
    let absent = fixture.root.join("absent");
    let connected = fixture.connections.load(Ordering::SeqCst);
    let check = |file: &str, data: Option<&[u8]>, hint: &str| {
        for existing in [false, true] {
            let mut command = fixture.command(&["tools", "run", "FetchPage", "--input-file", file]);
            if !existing {
                command
                    .env("FORMLESS_DATA_DIR", absent.join("data"))
                    .env("FORMLESS_CONFIG_DIR", absent.join("config"));
            }
            let result = Fixture::capture_with_input(command, data);
            assert_eq!(result.status.code(), Some(2), "{result:?}");
            assert!(result.stdout.is_empty());
            let error = String::from_utf8(result.stderr).unwrap();
            assert!(
                error.contains(if file == "-" { "stdin" } else { file }),
                "{error}"
            );
            assert!(error.contains(hint), "{error}");
        }
    };
    check(missing.to_str().unwrap(), None, "cannot read");
    check(fixture.root.to_str().unwrap(), None, "regular file");
    for (data, hint) in [
        (&[0xff][..], "UTF-8"),
        (&b"invalid fixture payload"[..], "JSON object"),
        (&b"[]"[..], "JSON object"),
    ] {
        std::fs::write(&input, data).unwrap();
        check(input.to_str().unwrap(), None, hint);
        check("-", Some(data), hint);
    }
    std::fs::write(&input, vec![b' '; 1024 * 1024 + 1]).unwrap();
    check(input.to_str().unwrap(), None, "1 MiB");
    assert!(!absent.exists());
    assert!(fixture.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.connections.load(Ordering::SeqCst), connected);
}
