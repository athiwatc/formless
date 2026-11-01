//! Actual shipped commands against a private foreground daemon and loopback
//! provider. No service manager, external provider, real profile or credentials.
use formless_core::{Config, Paths, Store};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
struct Fixture {
    root: PathBuf,
    paths: Paths,
}
impl Fixture {
    fn new() -> Self {
        // Keep private socket fixtures short on macOS as well as Linux.
        #[cfg(unix)]
        let temp = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let temp = std::env::temp_dir();
        let root = temp.join(format!(
            "f633-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let paths = Paths {
            data_dir: root.join("d/p"),
            config_dir: root.join("c/p"),
            profile: "p".into(),
        };
        Self { root, paths }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_formless"));
        for (key, _) in std::env::vars_os() {
            let k = key.to_string_lossy().to_ascii_uppercase();
            if k.starts_with("FORMLESS_") || k.contains("API_KEY") || k.contains("OAUTH") {
                c.env_remove(key);
            }
        }
        c.args(["--profile", "p"])
            .args(args)
            .env("FORMLESS_DATA_DIR", self.root.join("d"))
            .env("FORMLESS_CONFIG_DIR", self.root.join("c"))
            .env("FORMLESS_API_KEY", "private-loopback633-dummy")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
    fn json(&self, args: &[&str]) -> Value {
        let out = finish(self.command(args).spawn().unwrap());
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn finish(mut child: Child) -> Output {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(mut s) = stdout {
            s.read_to_end(&mut b).unwrap();
        }
        b
    });
    let err = std::thread::spawn(move || {
        let mut b = Vec::new();
        if let Some(mut s) = stderr {
            s.read_to_end(&mut b).unwrap();
        }
        b
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned633command timed out")
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}
struct Provider {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Provider {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut buf = Vec::new();
                let mut block = [0; 4096];
                let header_end;
                loop {
                    let n = socket.read(&mut block).unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&block[..n]);
                    assert!(buf.len() < 1024 * 1024);
                    if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = at + 4;
                        let head = String::from_utf8_lossy(&buf[..at]);
                        let len = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|s| s.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        while buf.len() < header_end + len {
                            let n = socket.read(&mut block).unwrap();
                            assert!(n > 0);
                            buf.extend_from_slice(&block[..n]);
                        }
                        break;
                    }
                }
                let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let header = String::from_utf8_lossy(&buf[..at]);
                if header.starts_with("GET ") {
                    assert!(
                        header.lines().next().unwrap().contains("/models "),
                        "unexpected private metadata route"
                    );
                    let data =
                        json!({"data":[{"id":"private633","context_window":65536}]}).to_string();
                    let _=write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len());
                    continue;
                }
                let body: Value = serde_json::from_slice(&buf[at + 4..]).unwrap();
                let messages = body["messages"].as_array().unwrap();
                let done = messages.last().is_some_and(|m| m["role"] == "tool");
                let delta = if done {
                    json!({"content":"finished private633"})
                } else {
                    json!({"tool_calls":[{"index":0,"id":"private633call","type":"function","function":{"name":"fixture633","arguments":"{}"}}]})
                };
                let event = json!({"choices":[{"delta":delta,"finish_reason":if done {"stop"}else{"tool_calls"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}});
                let data = format!("data: {event}\n\ndata: [DONE]\n\n");
                let _=write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{data}",data.len());
            }
        });
        Self {
            url,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let result = self.thread.take().unwrap().join();
        if !std::thread::panicking() {
            assert!(result.is_ok(), "private provider thread failed");
        }
    }
}
fn setup(f: &Fixture, url: &str) -> String {
    let mut cfg = Config::default();
    cfg.security.secure = true;
    cfg.provider.base_url = url.into();
    cfg.provider.model = "private633".into();
    cfg.provider.api_style = "openai".into();
    cfg.provider.tool_mode = "native".into();
    cfg.provider.max_retries = 0;
    cfg.provider.fallback_models.clear();
    cfg.provider.routing_models.clear();
    cfg.provider.routing_fast_models.clear();
    cfg.provider.routing_strong_models.clear();
    cfg.roles.clear();
    cfg.providers.clear();
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.tools.approval_summaries = false;
    cfg.optimiser.max_attempts_per_day = 0;
    cfg.daemon.backups_keep = 0;
    cfg.relay.enabled = false;
    cfg.mcp.servers.clear();
    cfg.save(&f.paths).unwrap();
    let st = Store::open(&f.paths.db_file()).unwrap();
    st.create_tool_version(
        "fixture633",
        "private fixture",
        "{\"type\":\"object\"}",
        "{}",
        "function run(){return {ok:true};}",
        &formless_runtime::Capabilities::parse(&["process".into()]).unwrap(),
        64,
        "user",
    )
    .unwrap();
    st.create_session(Some("private633".into())).unwrap().id
}
fn daemon(f: &Fixture) -> Owned {
    let cfg = Config::load_file_only(&f.paths).unwrap();
    assert!(cfg.provider.base_url.starts_with("http://127.0.0.1:"));
    let mut command = f.command(&["daemon", "serve", "--stay"]);
    command
        .env("FORMLESS_BASE_URL", &cfg.provider.base_url)
        .env("FORMLESS_MODEL", &cfg.provider.model)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut daemon = Owned(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(15);
    while !formless_core::daemon::daemon_reachable(&f.paths) {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "private daemon exited"
        );
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    daemon
}
fn start(f: &Fixture, sid: &str) -> Owned {
    let mut cmd = f.command(&["chat", "--session", sid]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = Owned(cmd.spawn().unwrap());
    child
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"run private fixture633\n")
        .unwrap();
    child
}
fn pending(f: &Fixture) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let value = f.json(&["approvals", "pending", "--json"]);
        if value.as_array().unwrap().len() == 1 {
            return value[0].clone();
        }
        assert!(Instant::now() < deadline, "no private pending approval");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn cli_only_approval_unblocks_run_follow_finishes_and_decision_names_cli() {
    let f = Fixture::new();
    let p = Provider::new();
    let sid = setup(&f, &p.url);
    let mut daemon = daemon(&f);
    let _chat = start(&f, &sid);
    let approval = pending(&f);
    let id = approval["approval_id"].as_str().unwrap();
    let before = f.json(&["runs", "--json"]);
    let run_id = before["runs"][0]["run_id"].as_str().unwrap();
    assert_eq!(
        f.json(&["runs", "show", run_id, "--json"])["status"],
        "running"
    );
    let mut follow = f
        .command(&["sessions", "show", &sid, "--follow", "--jsonl"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert!(follow.try_wait().unwrap().is_none());
    let refused = finish(
        f.command(&["approvals", "approve", id, "--session", "--json"])
            .spawn()
            .unwrap(),
    );
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    assert_eq!(pending(&f)["approval_id"], id);
    assert_eq!(
        f.json(&["approvals", "approve", id, "--once", "--json"])["resolved"],
        true
    );
    let out = finish(follow);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(events
        .iter()
        .any(|e| e["type"] == "run_finished" && e["status"] == "completed"));
    assert!(events
        .iter()
        .any(|e| e["message"]["content"] == "finished private633"));
    assert_eq!(
        f.json(&["runs", "show", run_id, "--json"])["status"],
        "completed"
    );
    assert_eq!(
        f.json(&["runs", "cancel", run_id, "--json"])["interrupted"],
        false
    );
    let late = finish(
        f.command(&["approvals", "approve", id, "--once", "--json"])
            .spawn()
            .unwrap(),
    );
    assert!(!late.status.success());
    let replay = finish(
        f.command(&["sessions", "show", &sid, "--follow", "--json"])
            .spawn()
            .unwrap(),
    );
    assert!(replay.status.success());
    let mut stop = f.command(&["daemon", "stop"]);
    stop.stdout(Stdio::null()).stderr(Stdio::null());
    assert!(stop.status().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemon.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let st = Store::open(&f.paths.db_file()).unwrap();
    let audit = st.recent_audit(Some("approval.decision"), 10).unwrap();
    assert!(audit.iter().any(|(_, _, detail, _)| {
        let v: Value = serde_json::from_str(detail).unwrap();
        v["approval_id"] == id && v["by"] == "CLI" && v["scope"] == "once"
    }));
}
#[test]
fn cancellation_is_run_specific_and_ctrl_c_only_detaches() {
    let f = Fixture::new();
    let p = Provider::new();
    let sid = setup(&f, &p.url);
    let st = Store::open(&f.paths.db_file()).unwrap();
    let old = st.create_run(&sid).unwrap();
    st.set_run_status(&old, "completed").unwrap();
    drop(st);
    let _daemon = daemon(&f);
    let _chat = start(&f, &sid);
    let approval = pending(&f);
    let id = approval["approval_id"].as_str().unwrap();
    let value = f.json(&["runs", "--json"]);
    let run = value["runs"][0]["run_id"].as_str().unwrap();
    assert_ne!(run, old);
    assert_eq!(
        f.json(&["runs", "cancel", &old, "--json"])["interrupted"],
        false
    );
    assert_eq!(
        f.json(&["runs", "show", run, "--json"])["status"],
        "running"
    );
    #[cfg(unix)]
    {
        let mut follow = f
            .command(&["sessions", "show", &sid, "--follow", "--jsonl"])
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert!(follow.try_wait().unwrap().is_none());
        // Only this fixture's owned observer receives SIGINT.
        assert_eq!(
            unsafe { libc::kill(follow.id() as libc::pid_t, libc::SIGINT) },
            0
        );
        assert_eq!(finish(follow).status.code(), Some(130));
        assert_eq!(
            f.json(&["runs", "show", run, "--json"])["status"],
            "running"
        );
        assert_eq!(pending(&f)["approval_id"], id);
    }
    let follow = f
        .command(&["sessions", "show", &sid, "--follow", "--jsonl"])
        .spawn()
        .unwrap();
    assert_eq!(
        f.json(&["runs", "cancel", run, "--json"])["interrupted"],
        true
    );
    assert_eq!(finish(follow).status.code(), Some(130));
}
#[test]
fn observing_without_daemon_does_not_start_or_create_profile() {
    let f = Fixture::new();
    for args in [
        vec!["approvals", "pending", "--json"],
        vec!["approvals", "approve", "x", "--once"],
        vec!["runs", "show", "x", "--json"],
        vec!["runs", "cancel", "x", "--json"],
        vec!["sessions", "show", "x", "--follow"],
    ] {
        assert!(!finish(f.command(&args).spawn().unwrap()).status.success());
        assert!(!f.root.join("d").exists());
        assert!(!f.root.join("c").exists());
    }
}

#[cfg(unix)]
#[test]
fn follow_resynchronizes_lost_finish_without_switching_to_replacement_run() {
    use formless_protocol::{Event, Request, RequestEnvelope, ServerMessage};
    use std::io::BufRead;
    let f = Fixture::new();
    std::fs::create_dir_all(&f.paths.data_dir).unwrap();
    let st = Store::open(&f.paths.db_file()).unwrap();
    let session = st.create_session(Some("peer633".into())).unwrap();
    let first = st.append_message(&session.id, "user", "first633").unwrap();
    let last = st
        .append_message(&session.id, "assistant", "final633")
        .unwrap();
    let replacement = st
        .append_message(&session.id, "assistant", "replacement must stay out")
        .unwrap();
    drop(st);
    let listener = std::os::unix::net::UnixListener::bind(f.paths.socket_path()).unwrap();
    let sid = session.id.clone();
    let query = sid.clone();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
        let mut initial = true;
        let mut seen_exact = false;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let env: RequestEnvelope = serde_json::from_str(&line).unwrap();
            let result = match env.request {
                Request::Subscribe { client, .. } => {
                    assert_eq!(client.as_deref(), Some("CLI"));
                    json!({})
                }
                Request::ListSessions => json!([session]),
                Request::GetRun { run_id } => {
                    assert_eq!(run_id, "target633");
                    seen_exact = true;
                    json!({"run_id":"target633","status":"completed","end_message_seq":last.seq})
                }
                Request::GetSession { id } => {
                    assert_eq!(id, sid);
                    let result = if initial {
                        json!({"messages":[first.clone()],"pending_events":[],"latest_run":{"run_id":"target633","status":"running"}})
                    } else {
                        json!({"messages":[first.clone(),last.clone(),replacement.clone()],"pending_events":[],"latest_run":{"run_id":"replacement633","status":"running"}})
                    };
                    if initial {
                        initial = false;
                        for event in [
                            Event::RunStarted {
                                session_id: sid.clone(),
                                run_id: "replacement633".into(),
                            },
                            Event::MessageDelta {
                                session_id: sid.clone(),
                                run_id: "replacement633".into(),
                                delta: "LIVE REPLACEMENT MUST STAY OUT".into(),
                            },
                        ] {
                            writeln!(
                                socket,
                                "{}",
                                serde_json::to_string(&ServerMessage::Event(event)).unwrap()
                            )
                            .unwrap();
                        }
                        let event = Event::DeltasDropped {
                            session_id: sid.clone(),
                            run_id: "target633".into(),
                            kinds: Vec::new(),
                            handle: None,
                            model: None,
                        };
                        writeln!(
                            socket,
                            "{}",
                            serde_json::to_string(&ServerMessage::Event(event)).unwrap()
                        )
                        .unwrap();
                    }
                    result
                }
                other => panic!("observer issued action: {other:?}"),
            };
            writeln!(
                socket,
                "{}",
                serde_json::to_string(&ServerMessage::Response {
                    id: env.id,
                    ok: true,
                    result: Some(result),
                    error: None
                })
                .unwrap()
            )
            .unwrap();
        }
        assert!(seen_exact);
    });
    let out = finish(
        f.command(&["sessions", "show", &query, "--follow", "--jsonl"])
            .spawn()
            .unwrap(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let values: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        values
            .iter()
            .filter(|v| v["message"]["content"] == "first633")
            .count(),
        1
    );
    assert_eq!(
        values
            .iter()
            .filter(|v| v["message"]["content"] == "final633")
            .count(),
        1
    );
    assert!(!values
        .iter()
        .any(|v| v["message"]["content"] == "replacement must stay out"
            || v["delta"] == "LIVE REPLACEMENT MUST STAY OUT"
            || v["run_id"] == "replacement633"));
    peer.join().unwrap();
}

#[cfg(unix)]
#[test]
fn ctrl_c_detaches_while_snapshot_reply_is_pending() {
    use formless_protocol::{Request, RequestEnvelope, ServerMessage};
    use std::io::BufRead;
    let f = Fixture::new();
    std::fs::create_dir_all(&f.paths.data_dir).unwrap();
    let st = Store::open(&f.paths.db_file()).unwrap();
    let session = st.create_session(Some("held633".into())).unwrap();
    drop(st);
    let listener = std::os::unix::net::UnixListener::bind(f.paths.socket_path()).unwrap();
    let sid = session.id.clone();
    let query = sid.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let env: RequestEnvelope = serde_json::from_str(&line).unwrap();
            let value = match env.request {
                Request::Subscribe { .. } => json!({}),
                Request::ListSessions => json!([session]),
                Request::GetSession { id } => {
                    assert_eq!(id, sid);
                    tx.send(()).unwrap();
                    continue;
                }
                other => panic!("detaching observer sent {other:?}"),
            };
            writeln!(
                socket,
                "{}",
                serde_json::to_string(&ServerMessage::Response {
                    id: env.id,
                    ok: true,
                    result: Some(value),
                    error: None
                })
                .unwrap()
            )
            .unwrap();
        }
    });
    let child = f
        .command(&["sessions", "show", &query, "--follow", "--jsonl"])
        .spawn()
        .unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = Instant::now();
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let out = finish(child);
    assert_eq!(
        out.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    peer.join().unwrap();
}

#[cfg(unix)]
#[test]
fn broad_session_grant_requires_actual_tty_double_confirmation_even_with_yes() {
    use std::os::fd::FromRawFd;
    fn tty_approve(f: &Fixture, id: &str, answers: &[u8]) -> Output {
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        // The PTY exists only for this owned CLI stdin, not the test runner.
        let mut master = unsafe { std::fs::File::from_raw_fd(master) };
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        let mut cmd = f.command(&["approvals", "approve", id, "--session", "--yes", "--json"]);
        cmd.stdin(Stdio::from(slave));
        let child = cmd.spawn().unwrap();
        master.write_all(answers).unwrap();
        let out = finish(child);
        drop(master);
        out
    }
    let f = Fixture::new();
    let p = Provider::new();
    let sid = setup(&f, &p.url);
    let _daemon = daemon(&f);
    let _chat = start(&f, &sid);
    let approval = pending(&f);
    let id = approval["approval_id"].as_str().unwrap();
    let refused = tty_approve(&f, id, b"yes\nno\n");
    assert!(!refused.status.success());
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr)
            .matches("[y/N]")
            .count(),
        2
    );
    assert_eq!(pending(&f)["approval_id"], id);
    let follow = f
        .command(&["sessions", "show", &sid, "--follow", "--jsonl"])
        .spawn()
        .unwrap();
    let allowed = tty_approve(&f, id, b"yes\nyes\n");
    assert!(
        allowed.status.success(),
        "{}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&allowed.stderr)
            .matches("[y/N]")
            .count(),
        2
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&allowed.stdout).unwrap()["resolved"],
        true
    );
    assert!(finish(follow).status.success());
    let grants = f.json(&["approvals", "list", "--json"]);
    assert!(grants
        .as_array()
        .unwrap()
        .iter()
        .any(|grant| grant["scope"] == "session" && grant["granted_by"] == "CLI"));
}

#[test]
fn inspecting_and_following_stored_terminal_runs_report_distinct_exit_codes() {
    let f = Fixture::new();
    let p = Provider::new();
    setup(&f, &p.url);
    let st = Store::open(&f.paths.db_file()).unwrap();
    let mut cases = Vec::new();
    for (status, code) in [
        ("completed", 0),
        ("paused", 1),
        ("failed", 1),
        ("interrupted", 130),
    ] {
        let session = st
            .create_session(Some(format!("terminal633-{status}")))
            .unwrap();
        st.append_message(&session.id, "assistant", status).unwrap();
        let run = st.create_run(&session.id).unwrap();
        st.set_run_status(&run, status).unwrap();
        if code == 1 {
            st.set_run_error(&run, "private633 terminal error").unwrap();
        }
        cases.push((session.id, run, status, code));
    }
    drop(st);
    let _daemon = daemon(&f);
    for (sid, run, status, code) in cases {
        let shown = finish(
            f.command(&["runs", "show", &run, "--json"])
                .spawn()
                .unwrap(),
        );
        assert_eq!(shown.status.code(), Some(code));
        assert_eq!(
            serde_json::from_slice::<Value>(&shown.stdout).unwrap()["status"],
            status
        );
        let follow = finish(
            f.command(&["sessions", "show", &sid, "--follow", "--jsonl"])
                .spawn()
                .unwrap(),
        );
        assert_eq!(
            follow.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&follow.stderr)
        );
        let events: Vec<Value> = String::from_utf8(follow.stdout)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| e["message"]["content"] == status)
                .count(),
            1
        );
    }
}

#[test]
fn headless_denial_resolves_only_that_request_without_remembering_a_denial() {
    let f = Fixture::new();
    let p = Provider::new();
    let sid = setup(&f, &p.url);
    let _daemon = daemon(&f);
    let _chat = start(&f, &sid);
    let approval = pending(&f);
    let id = approval["approval_id"].as_str().unwrap();
    let follow = f
        .command(&["sessions", "show", &sid, "--follow", "--jsonl"])
        .spawn()
        .unwrap();
    assert_eq!(
        f.json(&["approvals", "deny", id, "--json"])["resolved"],
        true
    );
    assert!(finish(follow).status.success());
    assert!(f
        .json(&["approvals", "pending", "--json"])
        .as_array()
        .unwrap()
        .is_empty());
    assert!(f
        .json(&["approvals", "denials", "--json"])
        .as_array()
        .unwrap()
        .is_empty());
    let repeat = finish(
        f.command(&["approvals", "deny", id, "--json"])
            .spawn()
            .unwrap(),
    );
    assert_eq!(repeat.status.code(), Some(2));
}

#[cfg(unix)]
fn peer(
    f: &Fixture,
    mut handle: impl FnMut(formless_protocol::Request) -> (Vec<formless_protocol::Event>, Value)
        + Send
        + 'static,
) -> std::thread::JoinHandle<()> {
    use formless_protocol::{RequestEnvelope, ServerMessage};
    use std::io::BufRead;
    std::fs::create_dir_all(&f.paths.data_dir).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(f.paths.socket_path()).unwrap();
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
            let (events, result) = handle(request.request);
            for ev in events {
                writeln!(
                    socket,
                    "{}",
                    serde_json::to_string(&ServerMessage::Event(ev)).unwrap()
                )
                .unwrap();
            }
            writeln!(
                socket,
                "{}",
                serde_json::to_string(&ServerMessage::Response {
                    id: request.id,
                    ok: true,
                    result: Some(result),
                    error: None
                })
                .unwrap()
            )
            .unwrap();
        }
    })
}

#[cfg(unix)]
#[test]
fn finished_event_with_failed_status_persistence_exits_with_diagnostic() {
    use formless_protocol::{Event, Request};
    let f = Fixture::new();
    std::fs::create_dir_all(&f.paths.data_dir).unwrap();
    let st = Store::open(&f.paths.db_file()).unwrap();
    let session = st.create_session(Some("status633".into())).unwrap();
    drop(st);
    let sid = session.id.clone();
    let query = sid.clone();
    let server = peer(&f, move |request| match request {
        Request::Subscribe { .. } => (vec![], json!({})),
        Request::ListSessions => (vec![], json!([session])),
        Request::GetSession { id } => {
            assert_eq!(id, sid);
            (
                vec![Event::RunFinished {
                    session_id: sid.clone(),
                    run_id: "status633".into(),
                    status: "completed".into(),
                }],
                json!({"messages":[],"pending_events":[],"latest_run":{"run_id":"status633","status":"running"}}),
            )
        }
        Request::GetRun { run_id } => {
            assert_eq!(run_id, "status633");
            (vec![], json!({"run_id":"status633","status":"running"}))
        }
        other => panic!("read-only observer sent {other:?}"),
    });
    let started = Instant::now();
    let out = finish(
        f.command(&["sessions", "show", &query, "--follow", "--jsonl"])
            .spawn()
            .unwrap(),
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("durable outcome"));
    assert!(started.elapsed() < Duration::from_secs(2));
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn applied_decision_with_failed_audit_warns_without_retry() {
    use formless_protocol::{ApprovalScope, Request};
    let f = Fixture::new();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = count.clone();
    let server = peer(&f, move |request| match request {
        Request::Subscribe { client, .. } => {
            assert_eq!(client.as_deref(), Some("CLI"));
            (vec![], json!({}))
        }
        Request::PendingApprovals => (
            vec![],
            json!([{"type":"approval_requested","approval_id":"audit633","session_id":"session633","tool_name":"private633","version":1,"capabilities":["process"],"memory_mb":64}]),
        ),
        Request::ApprovalDecision {
            approval_id,
            approved,
            scope,
            paths,
            remember_denial,
        } => {
            assert_eq!(approval_id, "audit633");
            assert!(approved);
            assert_eq!(scope, ApprovalScope::Once);
            assert!(paths.is_empty());
            assert!(!remember_denial);
            counted.fetch_add(1, Ordering::SeqCst);
            (vec![], json!({"resolved":true,"audit_recorded":false}))
        }
        other => panic!("unexpected retry/action: {other:?}"),
    });
    let out = finish(
        f.command(&["approvals", "approve", "audit633", "--once", "--json"])
            .spawn()
            .unwrap(),
    );
    assert!(out.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["audit_recorded"],
        false
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("do not retry"));
    server.join().unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn ctrl_c_detaches_during_a_saturated_private_socket_connect() {
    use std::os::fd::AsRawFd;
    let f = Fixture::new();
    std::fs::create_dir_all(&f.paths.data_dir).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(f.paths.socket_path()).unwrap();
    // Linux allows one pending connection with backlog zero. Hold that sole
    // slot without accepting, so the observer blocks in the OS connect call.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
    let _full = std::os::unix::net::UnixStream::connect(f.paths.socket_path()).unwrap();
    let mut child = f
        .command(&["sessions", "show", "x", "--follow", "--jsonl"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(250));
    assert!(child.try_wait().unwrap().is_none());
    let started = Instant::now();
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let out = finish(child);
    assert_eq!(
        out.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[cfg(unix)]
#[test]
fn new_user_message_before_replacement_run_started_stays_out_of_finished_target() {
    use formless_protocol::{Event, Request};
    let f = Fixture::new();
    std::fs::create_dir_all(&f.paths.data_dir).unwrap();
    let st = Store::open(&f.paths.db_file()).unwrap();
    let session = st.create_session(Some("before633".into())).unwrap();
    let first = st.append_message(&session.id, "user", "first633").unwrap();
    let last = st
        .append_message(&session.id, "assistant", "target633 final")
        .unwrap();
    let other = st
        .append_message(&session.id, "user", "B USER MUST STAY OUT")
        .unwrap();
    drop(st);
    let sid = session.id.clone();
    let query = sid.clone();
    let mut initial = true;
    let server = peer(&f, move |request| match request {
        Request::Subscribe { .. } => (vec![], json!({})),
        Request::ListSessions => (vec![], json!([session])),
        Request::GetSession { id } => {
            assert_eq!(id, sid);
            if initial {
                initial = false;
                (
                    vec![
                        Event::MessageAppended {
                            session_id: sid.clone(),
                            message: other.clone(),
                        },
                        Event::RunStarted {
                            session_id: sid.clone(),
                            run_id: "replacement633".into(),
                        },
                    ],
                    json!({"messages":[first],"pending_events":[],"latest_run":{"run_id":"target633","status":"running"}}),
                )
            } else {
                (
                    vec![],
                    json!({"messages":[first,last,other],"pending_events":[],"latest_run":{"run_id":"replacement633","status":"running"}}),
                )
            }
        }
        Request::GetRun { run_id } => {
            assert_eq!(run_id, "target633");
            (
                vec![],
                json!({"run_id":"target633","status":"completed","end_message_seq":last.seq}),
            )
        }
        other => panic!("observer sent action: {other:?}"),
    });
    let out = finish(
        f.command(&["sessions", "show", &query, "--follow", "--jsonl"])
            .spawn()
            .unwrap(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["message"]["content"] == "first633")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e["message"]["content"] == "target633 final")
            .count(),
        1
    );
    assert!(!events
        .iter()
        .any(|e| e["message"]["content"] == "B USER MUST STAY OUT"
            || e["run_id"] == "replacement633"));
    server.join().unwrap();
}
