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
    stop.store(true, Ordering::SeqCst);
    thread.join().unwrap();
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
fn estimate_new_profile_does_not_create_profile_directories() {
    let root = Fixture(std::env::temp_dir().join(format!("fl-estimate-new-{}", unique())));
    let paths = formless_core::Paths {
        data_dir: root.0.join("absent-data"),
        config_dir: root.0.join("absent-config"),
        profile: "forecast-new-fixture".into(),
    };
    let mut cmd = command(&paths);
    cmd.args(["run", "--estimate", "task", "--model", "model", "--json"]);
    let result = output(cmd);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!paths.data_dir.exists() && !paths.config_dir.exists());
}
