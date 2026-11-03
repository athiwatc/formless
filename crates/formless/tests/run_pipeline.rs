//! The shipped pipeline command against an owned foreground daemon and HTTP
//! fixture. Child-only environment overrides preserve HOME and real settings.
use formless_core::{Config, Paths, Store};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
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
struct Owned(Option<Child>);
impl Drop for Owned {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}
struct Reply {
    text: String,
    usage: bool,
}
impl Reply {
    fn text(text: &str) -> Self {
        Self {
            text: text.into(),
            usage: true,
        }
    }
}
struct Provider {
    url: String,
    seen: Arc<Mutex<Vec<Value>>>,
    replies: Arc<Mutex<VecDeque<Reply>>>,
    failure: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
fn read_request(socket: &mut std::net::TcpStream, stop: &AtomicBool) -> Result<Value, String> {
    socket.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut bytes = Vec::new();
    let mut expected = None;
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err("owned provider stopping".into());
        }
        if Instant::now() > deadline {
            return Err("private HTTP request exceeded ten seconds".into());
        }
        let mut chunk = [0u8; 4096];
        match socket.read(&mut chunk) {
            Ok(0) => return Err("private HTTP request ended early".into()),
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        }
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("private HTTP request exceeded its bound".into());
        }
        if expected.is_none() {
            if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let header =
                    std::str::from_utf8(&bytes[..index]).map_err(|_| "non-UTF8 HTTP headers")?;
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        (key.eq_ignore_ascii_case("content-length"))
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .ok_or("missing bounded Content-Length")?;
                if length > 2 * 1024 * 1024 {
                    return Err("private HTTP body exceeds bound".into());
                }
                expected = Some((index + 4, length));
            } else if bytes.len() > 16 * 1024 {
                return Err("private HTTP headers exceed bound".into());
            }
        }
        if let Some((offset, length)) = expected {
            if bytes.len() >= offset + length {
                return serde_json::from_slice(&bytes[offset..offset + length])
                    .map_err(|e| e.to_string());
            }
        }
    }
}
fn write_response(
    socket: &mut std::net::TcpStream,
    bytes: &[u8],
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut offset = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while offset < bytes.len() {
        if stop.load(Ordering::SeqCst) || Instant::now() > deadline {
            return Err("owned HTTP write stopped or exceeded deadline".into());
        }
        match socket.write(&bytes[offset..]) {
            Ok(0) => return Err("private HTTP response closed".into()),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
impl Provider {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let replies = Arc::new(Mutex::new(VecDeque::<Reply>::new()));
        let failure = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (observed, queue, failed, stopped) =
            (seen.clone(), replies.clone(), failure.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let mut socket = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => {
                        *failed.lock().unwrap() = Some(e.to_string());
                        break;
                    }
                };
                let body = match read_request(&mut socket, &stopped) {
                    Ok(body) => body,
                    Err(e) => {
                        if !stopped.load(Ordering::SeqCst) {
                            *failed.lock().unwrap() = Some(e);
                        }
                        continue;
                    }
                };
                observed.lock().unwrap().push(body.clone());
                let reply = queue
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Reply::text("UNEXPECTED private request"));
                let usage = if reply.usage {
                    json!({"prompt_tokens":9,"completion_tokens":3,"cost":0.02})
                } else {
                    Value::Null
                };
                let (kind, text) = if body["stream"] == true {
                    let start = json!({"model":"model","choices":[{"index":0,"delta":{"content":reply.text},"finish_reason":null}]});
                    let end = json!({"model":"model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":usage});
                    (
                        "text/event-stream",
                        format!("data: {start}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
                    )
                } else {
                    ("application/json",json!({"model":"model","choices":[{"message":{"role":"assistant","content":reply.text},"finish_reason":"stop"}],"usage":usage}).to_string())
                };
                let response=format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len());
                if let Err(e) = write_response(&mut socket, response.as_bytes(), &stopped) {
                    if !stopped.load(Ordering::SeqCst) {
                        *failed.lock().unwrap() = Some(e);
                    }
                }
            }
        });
        Self {
            url,
            seen,
            replies,
            failure,
            stop,
            thread: Some(thread),
        }
    }
    fn queue(&self, text: &str) {
        self.replies.lock().unwrap().push_back(Reply::text(text));
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
struct Fixture {
    root: PathBuf,
    paths: Paths,
    daemon: Option<Owned>,
    provider: Provider,
}
impl Fixture {
    fn new() -> Self {
        #[cfg(unix)]
        let base = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        let root = base.join(format!("f634-{}", unique()));
        std::fs::create_dir(&root).unwrap();
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
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let provider = Provider::new();
        let mut cfg = Config::default();
        cfg.provider.base_url = provider.url.clone();
        cfg.provider.model = "model".into();
        cfg.provider.max_retries = 0;
        cfg.provider.tool_use_nudge = false;
        cfg.provider
            .model_context_tokens
            .insert("model".into(), 100_000);
        cfg.provider.prices.insert(
            "model".into(),
            formless_core::config::ModelPrice {
                input: 1.0,
                output: 1.0,
                ..Default::default()
            },
        );
        cfg.limits.run_usd = Some(0.001);
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.tools.approval_summaries = false;
        cfg.save(&paths).unwrap();
        Self {
            root,
            paths,
            daemon: None,
            provider,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        for (key, _) in std::env::vars_os() {
            let value = key.to_string_lossy().to_ascii_uppercase();
            if value.starts_with("FORMLESS_")
                || value.contains("API_KEY")
                || value.contains("OAUTH")
            {
                command.env_remove(key);
            }
        }
        command
            .args(["--profile", "p"])
            .args(args)
            .current_dir(&self.root)
            .env("FORMLESS_DATA_DIR", self.root.join("d"))
            .env("FORMLESS_CONFIG_DIR", self.root.join("c"))
            .env("FORMLESS_API_KEY", "private-pipeline-cli-dummy")
            .env("FORMLESS_SECURE", "1")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    fn start(&mut self) {
        let stderr = std::fs::File::create(self.root.join("daemon.stderr")).unwrap();
        let mut command = self.command(&["daemon", "serve", "--stay"]);
        command.stdout(Stdio::null()).stderr(stderr);
        self.daemon = Some(Owned(Some(command.spawn().unwrap())));
        let deadline = Instant::now() + Duration::from_secs(15);
        while !formless_core::daemon::daemon_reachable(&self.paths) {
            let child = self.daemon.as_mut().unwrap().0.as_mut().unwrap();
            assert!(
                child.try_wait().unwrap().is_none(),
                "private pipeline daemon exited: {}",
                std::fs::read_to_string(self.root.join("daemon.stderr")).unwrap_or_default()
            );
            assert!(
                Instant::now() < deadline,
                "private pipeline daemon did not become ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Output {
        let mut command = self.command(args);
        if stdin.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut owned = Owned(Some(command.spawn().unwrap()));
        let stdout = owned.0.as_mut().unwrap().stdout.take().unwrap();
        let stderr = owned.0.as_mut().unwrap().stderr.take().unwrap();
        fn drain(mut input: impl Read, limit: usize) -> Vec<u8> {
            let mut result = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = input.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                if result.len() < limit {
                    result.extend_from_slice(&chunk[..n.min(limit - result.len())]);
                }
            }
            result
        }
        let out = std::thread::spawn(move || drain(stdout, 9 * 1024 * 1024));
        let err = std::thread::spawn(move || drain(stderr, 128 * 1024));
        let write_error = stdin.and_then(|bytes| {
            owned
                .0
                .as_mut()
                .unwrap()
                .stdin
                .take()
                .unwrap()
                .write_all(bytes)
                .err()
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = owned.0.as_mut().unwrap().try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                owned.0.as_mut().unwrap().kill().unwrap();
                owned.0.as_mut().unwrap().wait().unwrap();
                let captured_out = out.join().unwrap();
                let captured_err = err.join().unwrap();
                panic!(
                    "owned pipeline CLI exceeded twenty seconds: stdout={} stderr={}",
                    String::from_utf8_lossy(&captured_out),
                    String::from_utf8_lossy(&captured_err)
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let result = Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        };
        if result.status.success() {
            assert!(
                write_error.is_none(),
                "successful CLI failed to consume provided input"
            );
        }
        result
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.daemon.take());
        self.provider.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn shipped_pipeline_validates_repairs_once_keeps_stdout_clean_and_never_persists_run_instructions()
{
    let mut f = Fixture::new();
    let st = Store::open(&f.paths.db_file()).unwrap();
    let session = st.create_session(Some("pipeline".into())).unwrap().id;
    drop(st);
    let schema = f.root.join("schema.json");
    std::fs::write(&schema,br#"{"type":"object","required":["n"],"properties":{"n":{"type":"integer"}},"additionalProperties":false}"#).unwrap();
    let destination = f.root.join("answer.json");
    std::fs::write(&destination, b"old destination").unwrap();
    let old_cfg = std::fs::read(f.paths.config_file()).unwrap();
    f.start();
    f.provider.queue("INVALID-FIRST");
    f.provider.queue(r#"{"n":2}"#);
    let out = f.run(
        &[
            "run",
            "task",
            "--session",
            &session,
            "--schema",
            schema.to_str().unwrap(),
            "--system",
            "private Run-only ไทย marker",
            "--max-usd",
            "0.10",
            "--output",
            "final",
            "--output-file",
            destination.to_str().unwrap(),
            "--quiet",
        ],
        None,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert_eq!(std::fs::read(&destination).unwrap(), b"{\"n\":2}\n");
    assert_eq!(f.provider.count(), 2);
    let st = Store::open(&f.paths.db_file()).unwrap();
    assert!(st
        .get_session(&session)
        .unwrap()
        .unwrap()
        .instructions
        .is_none());
    assert!(!st
        .session_system_prompt(&session)
        .unwrap()
        .unwrap()
        .contains("private Run-only"));
    assert!(!st
        .list_messages(&session)
        .unwrap()
        .iter()
        .any(|m| m.content.contains("private Run-only") || m.content.contains("INVALID-FIRST")));
    drop(st);
    assert_eq!(std::fs::read(f.paths.config_file()).unwrap(), old_cfg);
    let before = std::fs::read(&destination).unwrap();
    for mode in ["final", "json"] {
        f.provider.queue("INVALID-SECOND");
        f.provider.queue(r#"{"n":"bad"}"#);
        let out = f.run(
            &[
                "run",
                "task",
                "--session",
                &session,
                "--schema",
                schema.to_str().unwrap(),
                "--max-usd",
                "0.10",
                "--output",
                mode,
                "--output-file",
                destination.to_str().unwrap(),
                "--quiet",
            ],
            None,
        );
        assert_eq!(
            out.status.code(),
            Some(3),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty());
        assert_eq!(std::fs::read(&destination).unwrap(), before);
        assert!(!String::from_utf8_lossy(&out.stderr).contains("INVALID-SECOND"));
    }
    assert_eq!(f.provider.count(), 6);
    for mode in ["final", "json"] {
        f.provider.queue("INVALID-STDOUT");
        f.provider.queue(r#"{"n":"bad"}"#);
        let out = f.run(
            &[
                "run",
                "task",
                "--session",
                &session,
                "--schema",
                schema.to_str().unwrap(),
                "--max-usd",
                "0.10",
                "--output",
                mode,
                "--quiet",
            ],
            None,
        );
        assert_eq!(
            out.status.code(),
            Some(3),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("INVALID-STDOUT"));
        if mode == "final" {
            assert!(out.stdout.is_empty());
        } else {
            let value: Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(value["status"], "schema_mismatch");
            assert!(value["answer"].is_null());
            assert!(value["result"].is_null());
        }
    }
    assert_eq!(f.provider.count(), 10);
    let out = f.run(
        &[
            "run",
            "task",
            "--session",
            &session,
            "--max-usd",
            "0",
            "--output",
            "final",
            "--output-file",
            destination.to_str().unwrap(),
            "--quiet",
        ],
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    assert_eq!(f.provider.count(), 10);
    assert_eq!(std::fs::read(&destination).unwrap(), before);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("observed spend:"));
    assert!(err.contains("$0.000000"));
    let out = f.run(&["run", "-", "--schema", "-", "--max-usd", "1"], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert_eq!(f.provider.count(), 10);
    f.provider.queue(r#"{"n":3}"#);
    let schema_bytes = std::fs::read(&schema).unwrap();
    let out = f.run(
        &[
            "run",
            "task",
            "--session",
            &session,
            "--schema",
            "-",
            "--max-usd",
            "0.10",
            "--output",
            "final",
            "--quiet",
        ],
        Some(&schema_bytes),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"{\"n\":3}\n");
    assert_eq!(f.provider.count(), 11);
    let st = Store::open(&f.paths.db_file()).unwrap();
    assert!(st.pending_attachments(&session).unwrap().is_empty());
    assert!(!st
        .list_messages(&session)
        .unwrap()
        .iter()
        .any(|m| m.content.contains("INVALID-STDOUT")));
    drop(st);
    let failure = f.provider.failure.lock().unwrap().clone();
    assert!(failure.is_none(), "{failure:?}");
    assert!(
        f.provider.replies.lock().unwrap().is_empty(),
        "every queued response was used exactly once"
    );
}

#[test]
fn shipped_schema_json_result_is_structured_on_stdout_and_in_output_files() {
    let mut f = Fixture::new();
    let store = Store::open(&f.paths.db_file()).unwrap();
    let session = store
        .create_session(Some("structured result".into()))
        .unwrap()
        .id;
    drop(store);
    let schema = f.root.join("bom-schema.json");
    let schema_bytes = [b"\xef\xbb\xbf".as_slice(), br#"{"type":"object","required":["n"],"properties":{"n":{"type":"integer"}},"additionalProperties":false}"#].concat();
    std::fs::write(&schema, &schema_bytes).unwrap();
    let destination = f.root.join("structured-result.json");
    f.start();
    let answer = " {\"n\":2} ";
    for from_stdin in [false, true] {
        for to_file in [false, true] {
            f.provider.queue(answer);
            let mut args = vec![
                "run",
                "task",
                "--session",
                &session,
                "--schema",
                if from_stdin {
                    "-"
                } else {
                    schema.to_str().unwrap()
                },
                "--max-usd",
                "0.10",
                "--output",
                "json",
                "--quiet",
            ];
            if to_file {
                std::fs::write(&destination, b"previous output").unwrap();
                args.extend(["--output-file", destination.to_str().unwrap()]);
            }
            let out = f.run(&args, from_stdin.then_some(schema_bytes.as_slice()));
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let bytes = if to_file {
                assert!(out.stdout.is_empty());
                std::fs::read(&destination).unwrap()
            } else {
                out.stdout
            };
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["session_id"], session);
            assert_eq!(value["status"], "completed");
            assert_eq!(
                value["answer"], answer,
                "existing answer text remains compatible"
            );
            assert_eq!(value["result"], json!({"n":2}));
            assert!(value["result"].is_object());
            assert!(value["error"].is_null());
        }
    }
    // JSON-looking free text is not a schema-validated result.
    f.provider.queue(answer);
    let out = f.run(
        &[
            "run",
            "task",
            "--session",
            &session,
            "--max-usd",
            "0.10",
            "--output",
            "json",
            "--quiet",
        ],
        None,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["answer"], answer);
    assert!(value.get("result").unwrap().is_null());
    assert_eq!(f.provider.count(), 5);
    assert!(Store::open(&f.paths.db_file())
        .unwrap()
        .pending_attachments(&session)
        .unwrap()
        .is_empty());
}

#[test]
fn shipped_pipeline_model_override_never_auto_probes_or_changes_the_session() {
    let mut f = Fixture::new();
    let mut cfg = Config::load(&f.paths).unwrap();
    cfg.provider.auto_test_models = true;
    cfg.provider.aliases.insert("pick".into(), "model".into());
    cfg.provider
        .aliases
        .insert("model".into(), "wrong-second-resolution".into());
    cfg.save(&f.paths).unwrap();
    let old_cfg = std::fs::read(f.paths.config_file()).unwrap();
    let st = Store::open(&f.paths.db_file()).unwrap();
    let session = st.create_session(Some("model override".into())).unwrap().id;
    st.set_session_model(&session, Some("standing-model".into()), Some("high".into()))
        .unwrap();
    drop(st);
    f.start();
    let zero = f.run(
        &[
            "run",
            "task",
            "--session",
            &session,
            "--model",
            "pick",
            "--effort",
            "low",
            "--max-usd",
            "0",
            "--output",
            "final",
            "--quiet",
        ],
        None,
    );
    assert_eq!(
        zero.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&zero.stderr)
    );
    assert!(zero.stdout.is_empty());
    assert_eq!(
        f.provider.count(),
        0,
        "neither selecting nor restoring a model can bypass the zero Run cap"
    );
    assert!(String::from_utf8_lossy(&zero.stderr).contains("$0.000000"));
    f.provider.queue("explicit answer");
    let positive = f.run(
        &[
            "run",
            "task",
            "--session",
            &session,
            "--model",
            "pick",
            "--effort",
            "low",
            "--max-usd",
            "0.10",
            "--output",
            "final",
            "--quiet",
        ],
        None,
    );
    assert!(
        positive.status.success(),
        "{}",
        String::from_utf8_lossy(&positive.stderr)
    );
    assert_eq!(positive.stdout, b"explicit answer\n");
    let seen = f.provider.seen.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "only the positive Run dispatches, without probe or restore requests"
    );
    assert_eq!(
        seen[0]["model"], "model",
        "the admitted resolved alias is not resolved a second time"
    );
    assert_eq!(seen[0]["reasoning_effort"], "low");
    let st = Store::open(&f.paths.db_file()).unwrap();
    let current = st.get_session(&session).unwrap().unwrap();
    assert_eq!(current.model.as_deref(), Some("standing-model"));
    assert_eq!(current.reasoning_effort.as_deref(), Some("high"));
    let forecasts = st.recorded_forecasts().unwrap();
    assert_eq!(forecasts.len(), 2);
    for (_, quote) in forecasts {
        assert_eq!(quote.basis.model, "model");
        assert_eq!(quote.basis.reasoning_effort.as_deref(), Some("low"));
    }
    assert_eq!(std::fs::read(f.paths.config_file()).unwrap(), old_cfg);
    assert!(f.provider.replies.lock().unwrap().is_empty());
    assert!(f.provider.failure.lock().unwrap().is_none());
}
