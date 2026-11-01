//! Shipped CLI branch operations use private fixtures and child-only env.
use formless_core::{Config, Paths, Store};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    paths: Paths,
}
impl Fixture {
    fn new(create: bool) -> Self {
        let name = format!(
            "branches-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let root = std::env::temp_dir().join(&name);
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let paths = Paths {
            data_dir: root.join("data").join(&name),
            config_dir: root.join("config").join(&name),
            profile: name,
        };
        if create {
            std::fs::create_dir_all(&paths.data_dir).unwrap();
            std::fs::create_dir_all(&paths.config_dir).unwrap();
        }
        Self { root, paths }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("FORMLESS_")
            {
                command.env_remove(key);
            }
        }
        command
            .args(["--profile", self.paths.profile.as_str()])
            .args(args)
            .env("FORMLESS_DATA_DIR", self.root.join("data"))
            .env("FORMLESS_CONFIG_DIR", self.root.join("config"))
            .env("FORMLESS_API_KEY", "private-offline-branch-fixture")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // HOME and all machine settings stay unchanged.
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn output(mut command: Command) -> Output {
    let mut child = command.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let mut stderr = child.stderr.take().unwrap();
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            panic!("private branch CLI exceeded20s");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &entry.path(), out);
            } else {
                assert!(entry.file_type().unwrap().is_file());
                out.insert(
                    entry.path().strip_prefix(root).unwrap().into(),
                    std::fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn offline_compare_json_and_human_output_preserve_profile_bytes_and_make_zero_http_calls() {
    let fixture = Fixture::new(true);
    let endpoint = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    endpoint.set_nonblocking(true).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{}", endpoint.local_addr().unwrap());
    cfg.provider.auth.mode = "oauth".into();
    cfg.provider.auth.client_id = "private-fixture".into();
    cfg.provider.auth.token_url = format!("{}/token", cfg.provider.base_url);
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&fixture.paths).unwrap();
    let st = Store::open(&fixture.paths.db_file()).unwrap();
    let parent = st.create_session(Some("Original".into())).unwrap();
    st.append_message(&parent.id, "user", "shared prompt")
        .unwrap();
    st.append_message(&parent.id, "assistant", "original answer")
        .unwrap();
    let fork = st
        .fork_session(&parent.id, Some("Branch".into()), Some(1))
        .unwrap()
        .unwrap();
    st.append_message(&fork.id, "assistant", "different answer")
        .unwrap();
    st.create_job(
        "MUST NOT RUN",
        Some(&parent.id),
        None,
        "2000-01-01T00:00:00Z",
        true,
        "user",
    )
    .unwrap();
    drop(st); // No live SQLite connection: snapshot must preserve DB/WAL/SHM.
    let before = files(&fixture.root);
    let json =
        success(output(fixture.command(&[
            "sessions", "compare", &parent.id, &fork.id, "--json",
        ])));
    let value: Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(value["common_ancestor"], parent.id);
    assert_eq!(value["fork_point_seq"], 1);
    assert_eq!(value["right"]["usage"]["requests"], 0);
    assert!(
        value["left"]["usage"]["cost"].is_null(),
        "untracked old transcript charges remain unknown"
    );
    assert!(value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["right"]["content"] == "different answer"));
    let human = String::from_utf8(success(output(
        fixture.command(&["sessions", "compare", "Original", "Branch"]),
    )))
    .unwrap();
    assert!(
        human.contains("fork point seq 1")
            && human.contains("usage scope:")
            && human.contains("cost unknown")
            && human.contains("different answer"),
        "{human}"
    );
    assert_eq!(files(&fixture.root),before,"comparison must not start daemon/jobs, write credentials, migrate the source or alter SQLite sidecars");
    assert!(
        matches!(endpoint.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock),
        "zero provider/token endpoint connections"
    );
}

#[test]
fn comparing_missing_profile_fails_without_creating_it() {
    let fixture = Fixture::new(false);
    let result = output(fixture.command(&["sessions", "compare", "a", "b", "--json"]));
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("No existing profile database"));
    assert!(!fixture.root.join("data").exists());
    assert!(!fixture.root.join("config").exists());
}

struct OwnedDaemon(Child);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
#[test]
fn actual_cli_fork_json_and_live_compare_use_stored_sessions_without_starting_runs() {
    let fixture = Fixture::new(true);
    let endpoint = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    endpoint.set_nonblocking(true).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{}", endpoint.local_addr().unwrap());
    cfg.provider.model.clear();
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
    cfg.save(&fixture.paths).unwrap();
    let st = Store::open(&fixture.paths.db_file()).unwrap();
    let parent = st.create_session(Some("CLI source".into())).unwrap();
    st.append_message(&parent.id, "user", "first").unwrap();
    st.append_message(&parent.id, "assistant", "second")
        .unwrap();
    drop(st);
    // This one foreground process is owned/reaped by the fixture. No OS service
    // manager, real profile, external provider or keychain is involved.
    let mut command = fixture.command(&["daemon", "serve"]);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut daemon = OwnedDaemon(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !formless_core::daemon::daemon_reachable(&fixture.paths) {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "owned fixture daemon exited before ready"
        );
        assert!(
            Instant::now() < deadline,
            "owned fixture daemon did not become ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let bytes = success(output(fixture.command(&[
        "sessions",
        "fork",
        &parent.id,
        "--up-to-seq",
        "1",
        "--name",
        "CLI branch",
        "--json",
    ])));
    let fork: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(fork["forked_from"], parent.id);
    assert_eq!(fork["fork_point_seq"], 1);
    assert_eq!(fork["name"], "CLI branch");
    let child = fork["id"].as_str().unwrap();
    let compared: Value = serde_json::from_slice(&success(output(
        fixture.command(&["sessions", "compare", &parent.id, child, "--json"]),
    )))
    .unwrap();
    assert_eq!(compared["right"]["session"]["id"], child);
    assert_eq!(compared["right"]["usage"]["requests"], 0);
    success(output(fixture.command(&["daemon", "stop"])));
    let deadline = Instant::now() + Duration::from_secs(10);
    while daemon.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "owned daemon failed to stop");
        std::thread::sleep(Duration::from_millis(5));
    }
    let st = Store::open(&fixture.paths.db_file()).unwrap();
    assert_eq!(st.list_messages(child).unwrap().len(), 1);
    assert!(st.last_run(child).unwrap().is_none());
    assert!(st.last_run(&parent.id).unwrap().is_none());
    assert!(st.usage_rows(None, None).unwrap().0.is_empty());
    assert!(
        matches!(endpoint.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock),
        "fork/compare cannot trigger provider requests"
    );
}
