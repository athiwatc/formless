//! The shipped CLI estimate path must remain local, including with unusable
//! OAuth credentials and a provider endpoint that counts every HTTP request.
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
fn unique() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct FixtureThread {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for FixtureThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
fn command(paths: &formless_core::Paths) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_formless"));
    for (key, _) in std::env::vars_os() {
        let k = key.to_string_lossy();
        if k.starts_with("FORMLESS_") {
            c.env_remove(key);
        }
    }
    c.env("FORMLESS_DATA_DIR", &paths.data_dir)
        .env("FORMLESS_CONFIG_DIR", &paths.config_dir)
        .env(
            "FORMLESS_OAUTH_ACCESS_TOKEN",
            "expired-forecast-fixture-token",
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}
fn output(mut command: Command) -> std::process::Output {
    let mut child = command.spawn().unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("read-only estimate did not exit");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    child.wait_with_output().unwrap()
}
#[test]
fn estimate_json_exits_zero_without_http_auth_upload_run_or_marker() {
    let root = Fixture(std::env::temp_dir().join(format!("fl-estimate-cli-{}", unique())));
    let paths =
        formless_core::Paths::from_dirs(root.0.join("data"), root.0.join("config")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let counted = calls.clone();
    let stopped = stop.clone();
    let mut cfg = formless_core::Config::default();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.provider.model = "model".into();
    cfg.provider.auth.mode = "oauth".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let thread = std::thread::spawn(move || {
        while !stopped.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));
                    let mut buf = [0; 2048];
                    let _ = socket.read(&mut buf);
                    let body = b"{}";
                    let _ = write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(body);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(_) => break,
            }
        }
    });
    let counter_thread = FixtureThread {
        stop,
        thread: Some(thread),
    };
    let st = formless_core::Store::open(&paths.db_file()).unwrap();
    let sid = st.create_session(Some("existing".into())).unwrap().id;
    let file = root.0.join("attachment.txt");
    std::fs::write(&file, b"private fixture data").unwrap();
    let before = std::fs::read(paths.config_file()).unwrap();
    drop(st); // Offline snapshot requires no other native SQLite writer.
    let mut cmd = command(&paths);
    cmd.args([
        "run",
        "--estimate",
        "inspect attachment",
        "--session",
        &sid,
        "--model",
        "model",
        "--json",
        "--attach",
        file.to_str().unwrap(),
    ]);
    let result = output(cmd);
    drop(counter_thread);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let forecast: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(forecast["basis"]["prompt_tokens"].as_u64().unwrap() > 0);
    assert_eq!(forecast["comparable_runs"], 0);
    assert!(forecast["steps"].is_null() && forecast["cost_usd"].is_null());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "neither completions, /models nor OAuth refresh may be called"
    );
    let st = formless_core::Store::open(&paths.db_file()).unwrap();
    assert_eq!(st.list_sessions().unwrap().len(), 1);
    assert!(st.list_messages(&sid).unwrap().is_empty());
    assert!(st.run_metrics(100).unwrap().is_empty());
    assert!(st.pending_attachments(&sid).unwrap().is_empty());
    assert!(!paths.last_run_session_file().exists());
    assert_eq!(std::fs::read(paths.config_file()).unwrap(), before);
}
#[test]
fn estimate_output_formats_stay_local_on_a_new_profile() {
    let root = Fixture(std::env::temp_dir().join(format!("fl-estimate-new-{}", unique())));
    let paths = formless_core::Paths {
        data_dir: root.0.join("absent-data"),
        config_dir: root.0.join("absent-config"),
        profile: "forecast-new-fixture".into(),
    };
    let estimate = |flags: &[&str]| {
        let mut cmd = command(&paths);
        cmd.args(["run", "--estimate", "task", "--model", "model"])
            .args(flags);
        let result = output(cmd);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    };
    let json: serde_json::Value = serde_json::from_slice(&estimate(&["--output", "json"])).unwrap();
    let alias: serde_json::Value = serde_json::from_slice(&estimate(&["--json"])).unwrap();
    // An empty local fixture has no comparable Runs or time-dependent data.
    assert_eq!(json, alias);
    let forecast = serde_json::from_value(json).unwrap();
    let human = String::from_utf8(estimate(&[])).unwrap();
    assert!(
        human.ends_with('\n') && !human.ends_with("\n\n"),
        "{human:?}"
    );
    assert!(human.lines().all(|line| !line.ends_with(' ')), "{human:?}");
    assert_eq!(
        human,
        format!("{}\n", formless_core::forecast::describe(&forecast))
    );
    assert_eq!(estimate(&["--output", "text"]), human.as_bytes());

    for mode in ["jsonl-events", "final"] {
        let mut cmd = command(&paths);
        // Keep the input pipe open. A command that reads it will hit the
        // fixture deadline instead of exiting with the output-mode error.
        cmd.args(["run", "--estimate", "-", "--output", mode])
            .stdin(Stdio::piped());
        let invalid = output(cmd);
        assert_eq!(invalid.status.code(), Some(2));
        assert!(invalid.stdout.is_empty());
        let error = String::from_utf8_lossy(&invalid.stderr);
        assert!(
            error.contains("--estimate") && error.contains(mode),
            "{error}"
        );
    }
    assert!(!paths.data_dir.exists() && !paths.config_dir.exists());
}

// Accepted reachability probes may already be closed when the fixture handles
// them. Use portable nonblocking I/O instead of SO_RCVTIMEO (EINVAL for such
// sockets on macOS), retaining bytes across WouldBlock and enforcing the same
// ten-second CLI deadline plus an owned stop flag. Never discard partial frames.
#[cfg(unix)]
fn fixture_read_frame(
    socket: &mut impl Read,
    pending: &mut Vec<u8>,
    stopped: &AtomicBool,
    deadline: Instant,
) -> std::io::Result<Option<Vec<u8>>> {
    loop {
        if stopped.load(Ordering::SeqCst) {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "fixture request exceeded the CLI deadline",
            ));
        }
        if let Some(end) = pending.iter().position(|b| *b == b'\n') {
            if end > formless_protocol::MAX_FRAME_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "fixture request frame exceeds protocol limit",
                ));
            }
            return Ok(Some(pending.drain(..=end).collect()));
        }
        if pending.len() > formless_protocol::MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "fixture request frame exceeds protocol limit",
            ));
        }
        let mut bytes = [0; 4096];
        match socket.read(&mut bytes) {
            Ok(0) if pending.is_empty() => return Ok(None),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "fixture peer closed inside an NDJSON frame",
                ))
            }
            Ok(n) => pending.extend_from_slice(&bytes[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset && pending.is_empty() => {
                return Ok(None)
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(unix)]
fn fixture_write_frame(
    socket: &mut impl Write,
    frame: &formless_protocol::ServerMessage,
    stopped: &AtomicBool,
    deadline: Instant,
) -> std::io::Result<bool> {
    let mut bytes = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    let mut sent = 0;
    while sent < bytes.len() {
        if stopped.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "fixture response exceeded the CLI deadline",
            ));
        }
        match socket.write(&bytes[sent..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "fixture response write made no progress",
                ))
            }
            Ok(n) => sent += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(false)
            }
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(unix)]
#[test]
fn live_estimate_formats_synthetic_history_without_altering_machine_output() {
    use formless_protocol::{Request, RequestEnvelope, RunForecast, ServerMessage};
    use serde_json::json;
    use std::os::unix::{fs::DirBuilderExt, net::UnixListener};

    // Keep this socket inside an exclusively owned, short path even when
    // the OS chooses a long user-specific temporary directory.
    let root = std::path::Path::new("/tmp").join(format!("fl-ef-{}", unique()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let root = Fixture(root);
    let paths = formless_core::Paths {
        data_dir: root.0.join("d"),
        config_dir: root.0.join("c"),
        profile: "default".into(),
    };
    std::fs::create_dir(&paths.data_dir).unwrap();
    let forecast: RunForecast = serde_json::from_value(json!({
        "fingerprint": "forecast-fixture",
        "basis": {
            "provider_key": "fixture", "model": "fixture", "role": "fixture",
            "session_id": null, "cwd_hash": "fixture", "tools_hash": "fixture",
            "prompt_tokens": 12
        },
        "comparable_runs": 4,
        "steps": {"p50": 11.6, "p90": 31.4},
        "tokens": {"p50": 48213.125, "p90": 90550.75},
        "cost_usd": {"p50": 0.000001, "p90": 0.004},
        "duration_ms": {"p50": 499.75, "p90": 84211.75},
        "context_tokens": 100000, "context_source": "fixture", "will_compact": false,
        "compaction_probability": 0.126, "cached_fraction": {"p50": 0.426, "p90": 0.8},
        "unpriced": false, "warnings": [], "confirmation_required": false,
        "hint": "Synthetic forecast"
    }))
    .unwrap();
    let machine = serde_json::to_value(&forecast).unwrap();
    let exact_json = format!("{}\n", serde_json::to_string_pretty(&forecast).unwrap());
    let socket = paths.socket_path();
    assert!(socket.starts_with(&root.0));
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = requests.clone();
    let thread = std::thread::spawn(move || {
        while !stopped.load(Ordering::SeqCst) {
            let (mut socket, _) = match listener.accept() {
                Ok(peer) => peer,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => panic!("private forecast fixture accept: {error}"),
            };
            socket.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut pending = Vec::new();
            while let Some(line) =
                fixture_read_frame(&mut socket, &mut pending, &stopped, deadline).unwrap()
            {
                let envelope: RequestEnvelope = serde_json::from_slice(&line).unwrap();
                let result = match envelope.request {
                    Request::Status => {
                        json!({"version":env!("CARGO_PKG_VERSION"),"protocol":1,"secure":true})
                    }
                    Request::ForecastRun {
                        input,
                        for_send: false,
                    } => {
                        assert_eq!(input.draft, "synthetic task");
                        assert_eq!(input.model.as_deref(), Some("fixture"));
                        assert!(input.session_id.is_none() && input.attachments.is_empty());
                        counted.fetch_add(1, Ordering::SeqCst);
                        machine.clone()
                    }
                    request => panic!("unexpected request from estimate: {request:?}"),
                };
                let reply = ServerMessage::Response {
                    id: envelope.id,
                    ok: true,
                    result: Some(result),
                    error: None,
                };
                if !fixture_write_frame(&mut socket, &reply, &stopped, deadline).unwrap() {
                    break;
                }
            }
        }
    });
    let server = FixtureThread {
        stop,
        thread: Some(thread),
    };
    let estimate = |flags: &[&str]| {
        let result = output({
            let mut cmd = command(&paths);
            cmd.current_dir(&root.0)
                .args(["run", "--estimate", "synthetic task", "--model", "fixture"])
                .args(flags);
            cmd
        });
        assert!(result.status.success(), "{:?}", result);
        assert!(result.stderr.is_empty(), "{:?}", result);
        result.stdout
    };
    for flags in [&["--json"][..], &["--output", "json"][..]] {
        assert_eq!(estimate(flags), exact_json.as_bytes());
    }
    let human = estimate(&[]);
    assert_eq!(estimate(&["--output", "text"]), human);
    drop(server);
    assert_eq!(requests.load(Ordering::SeqCst), 4);
    assert!(!paths.db_file().exists());
    assert!(!paths.config_dir.exists());
    assert_eq!(
        String::from_utf8(human).unwrap(),
        "Synthetic forecast\nSteps: p50 12 / p90 31\nTokens: p50 48,213 / p90 90,551\nCost: p50 $1.00e-6 / p90 $0.0040\nDuration: p50 0.5s / p90 1m24s\nCached fraction: p50 43% / p90 80%\nCompaction probability: 13%\n"
    );
}

#[cfg(unix)]
#[test]
fn headless_confirmation_uses_one_connection_and_refuses_without_yes() {
    use formless_protocol::{Event, RequestEnvelope, ServerMessage};
    use serde_json::{json, Value};
    use std::{os::unix::net::UnixListener, sync::Mutex};
    struct Server {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                t.join().unwrap();
            }
        }
    }
    for yes in [false, true] {
        let root = Fixture(std::env::temp_dir().join(format!("fl-fc-{}", unique())));
        let paths = formless_core::Paths::from_dirs(root.0.join("d"), root.0.join("c")).unwrap();
        let mut cfg = formless_core::Config::default();
        cfg.provider.base_url = "http://127.0.0.1:9".into();
        cfg.provider.model = "model".into();
        cfg.limits.forecast_confirm_usd = Some(1.0);
        cfg.save(&paths).unwrap();
        let st = formless_core::Store::open_in_memory().unwrap();
        let session = st.create_session(Some("fixture".into())).unwrap();
        let mut forecast = formless_core::forecast::estimate_store(
            &st,
            &cfg,
            &formless_protocol::ForecastInput {
                session_id: Some(session.id.clone()),
                draft: "task".into(),
                ..Default::default()
            },
        )
        .unwrap();
        forecast.confirmation_required = true;
        forecast.confirmation_id = Some("connection-bound".into());
        forecast.hint = "$2 p90".into();
        std::fs::create_dir_all(paths.socket_path().parent().unwrap()).unwrap();
        let listener = UnixListener::bind(paths.socket_path()).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let calls = Arc::new(Mutex::new(Vec::<(usize, String, Value)>::new()));
        let observed = calls.clone();
        let sid = session.id.clone();
        let thread = std::thread::spawn(move || {
            let mut peers = Vec::new();
            let mut connection = 0;
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        connection += 1;
                        let connection = connection;
                        let stopped = stopped.clone();
                        let observed = observed.clone();
                        let forecast = forecast.clone();
                        let session = session.clone();
                        peers.push(std::thread::spawn(move||{
                            socket.set_nonblocking(true).expect("fixture Unix socket must support nonblocking I/O");
                            let deadline=Instant::now()+Duration::from_secs(10);
                            let mut pending=Vec::new();let mut approved=false;let mut previewed=false;
                            while let Some(line)=fixture_read_frame(&mut socket,&mut pending,&stopped,deadline).expect("bounded fixture NDJSON read") {
                                let envelope:RequestEnvelope=serde_json::from_slice(&line).unwrap();
                                let request=serde_json::to_value(envelope.request).unwrap();let kind=request["type"].as_str().unwrap().to_string();
                                observed.lock().unwrap().push((connection,kind.clone(),request.clone()));
                                let mut finish=false;
                                let value=match kind.as_str(){
                                    "status"=>json!({"version":env!("CARGO_PKG_VERSION"),"protocol":1,"secure":true}),
                                    "list_sessions"=>json!([session]),
                                    "forecast_run"=>{assert_eq!(request["for_send"],true);previewed=true;json!(forecast)},
                                    "confirm_forecast"=>{assert!(previewed,"consent used a different connection");assert_eq!(request["id"],"connection-bound");approved=true;json!({"confirmed":true})},
                                    "send_message"=>{assert!(approved,"headless send bypassed confirmation");finish=true;json!({"run_id":"fixture-run"})},
                                    _=>json!({"ok":true}),
                                };
                                let reply=ServerMessage::Response{id:envelope.id,ok:true,result:Some(value),error:None};
                                if !fixture_write_frame(&mut socket,&reply,&stopped,deadline).expect("bounded fixture NDJSON response"){break;}
                                if finish {
                                    let event=ServerMessage::Event(Event::RunFinished{session_id:session.id.clone(),run_id:"fixture-run".into(),status:"completed".into()});
                                    if !fixture_write_frame(&mut socket,&event,&stopped,deadline).expect("bounded fixture NDJSON event"){break;}
                                }
                            }
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => break,
                }
            }
            for peer in peers {
                peer.join().unwrap();
            }
        });
        let server = Server {
            stop,
            thread: Some(thread),
        };
        let mut cmd = command(&paths);
        cmd.args(["run", "task", "--session", &sid, "--quiet"]);
        if yes {
            cmd.arg("--yes");
        }
        let result = output(cmd);
        drop(server);
        assert_eq!(
            result.status.code(),
            Some(if yes { 0 } else { 2 }),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let requests = calls.lock().unwrap();
        let actions: Vec<_> = requests
            .iter()
            .filter(|(_, k, _)| {
                matches!(
                    k.as_str(),
                    "forecast_run" | "confirm_forecast" | "send_message"
                )
            })
            .collect();
        assert_eq!(actions.len(), if yes { 3 } else { 1 });
        assert!(actions.iter().all(|(c, _, _)| *c == actions[0].0));
        if yes {
            assert_eq!(actions[2].1, "send_message");
            assert!(actions[2].2["client_msg_id"]
                .as_str()
                .is_some_and(|s| !s.is_empty()));
        }
        assert!(!requests
            .iter()
            .any(|(_, k, _)| matches!(k.as_str(), "create_session" | "attach_file")));
    }
}

#[cfg(unix)]
#[test]
fn nonblocking_fixture_preserves_fragmented_and_coalesced_utf8_frames() {
    struct Fragmented(std::collections::VecDeque<std::io::Result<Vec<u8>>>, usize);
    impl Read for Fragmented {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1 += 1;
            let part = self.0.pop_front().unwrap_or(Ok(Vec::new()))?;
            assert!(part.len() <= buf.len());
            buf[..part.len()].copy_from_slice(&part);
            Ok(part.len())
        }
    }
    let mut reader = Fragmented(
        std::collections::VecDeque::from([
            Ok(b"{\"word\":\"\xe4".to_vec()),
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            Ok(b"\xbd\xa0\"}\n{\"next\":true}\n".to_vec()),
        ]),
        0,
    );
    let stop = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut pending = Vec::new();
    let first = fixture_read_frame(&mut reader, &mut pending, &stop, deadline)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&first).unwrap(),
        serde_json::json!({"word":"你"})
    );
    let second = fixture_read_frame(&mut reader, &mut pending, &stop, deadline)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&second).unwrap(),
        serde_json::json!({"next":true})
    );
    assert_eq!(
        reader.1, 3,
        "coalesced second frame must not consume another read"
    );
    assert!(pending.is_empty());
    assert!(
        fixture_read_frame(&mut reader, &mut pending, &stop, deadline)
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn nonblocking_fixture_retries_partial_response_writes_without_restarting_frames() {
    struct Fragmented {
        bytes: Vec<u8>,
        calls: usize,
    }
    impl Write for Fragmented {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            match self.calls {
                2 => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
                4 => Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
                n => {
                    let count = if n == 1 {
                        3
                    } else if n == 3 {
                        5
                    } else {
                        buf.len()
                    };
                    self.bytes.extend_from_slice(&buf[..count]);
                    Ok(count)
                }
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let frame = formless_protocol::ServerMessage::Response {
        id: 7,
        ok: true,
        result: Some(serde_json::json!({"word":"你"})),
        error: None,
    };
    let mut writer = Fragmented {
        bytes: Vec::new(),
        calls: 0,
    };
    let stopped = AtomicBool::new(false);
    assert!(fixture_write_frame(
        &mut writer,
        &frame,
        &stopped,
        Instant::now() + Duration::from_secs(10)
    )
    .unwrap());
    let mut expected = serde_json::to_vec(&frame).unwrap();
    expected.push(b'\n');
    assert_eq!(writer.bytes, expected);
    assert_eq!(writer.calls, 5);
}

#[cfg(unix)]
#[test]
fn nonblocking_fixture_accepts_an_already_closed_reachability_probe() {
    struct SocketFile(std::path::PathBuf);
    impl Drop for SocketFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let root = Fixture(std::env::temp_dir().join(format!("fl-fc-probe-{}", unique())));
    let paths = formless_core::Paths::from_dirs(root.0.join("d"), root.0.join("c")).unwrap();
    let socket = SocketFile(paths.socket_path());
    std::fs::create_dir_all(socket.0.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket.0).unwrap();
    let probe = std::os::unix::net::UnixStream::connect(&socket.0).unwrap();
    drop(probe); // Reproduce the daemon_reachable connect-and-close before accept.
    let (mut reader, _) = listener.accept().unwrap();
    reader
        .set_nonblocking(true)
        .expect("closed-peer accepted Unix socket remains a valid nonblocking descriptor");
    assert!(fixture_read_frame(
        &mut reader,
        &mut Vec::new(),
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(10)
    )
    .unwrap()
    .is_none());
}

#[cfg(unix)]
#[test]
fn nonblocking_fixture_rejects_partial_eof_and_obeys_owned_stop_and_original_deadline() {
    let mut partial = std::io::Cursor::new(b"{\"partial\":".to_vec());
    let mut pending = Vec::new();
    let error = fixture_read_frame(
        &mut partial,
        &mut pending,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    assert_eq!(pending, b"{\"partial\":");
    let mut unread = std::io::Cursor::new(b"{}\n".to_vec());
    assert!(fixture_read_frame(
        &mut unread,
        &mut Vec::new(),
        &AtomicBool::new(true),
        Instant::now() + Duration::from_secs(10)
    )
    .unwrap()
    .is_none());
    assert_eq!(
        unread.position(),
        0,
        "stop must not consume or dispatch a pending frame"
    );
    let error = fixture_read_frame(
        &mut unread,
        &mut Vec::new(),
        &AtomicBool::new(false),
        Instant::now() - Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(unread.position(), 0);
}
