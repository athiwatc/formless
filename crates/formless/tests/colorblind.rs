//! Shipped CLI status/config surfaces; private foreground daemon and local
//! records only. No service manager, keyring or external provider contact.
use formless_core::{Config, Paths, Store};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    root: PathBuf,
    paths: Paths,
    provider: std::net::TcpListener,
}

fn create_fixture_root(parent: &Path, nonce: u128) -> PathBuf {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(false);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);

    // Parallel fixtures can observe the same clock tick. Claim the directory
    // exclusively so the daemon and its database belong to one fixture.
    for attempt in 0..100 {
        let root = parent.join(format!("f635-{}-{nonce:x}-{attempt}", std::process::id()));
        match builder.create(&root) {
            Ok(()) => return root,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create private fixture directory: {error}"),
        }
    }
    panic!("could not reserve a private fixture directory");
}

impl Fixture {
    fn new() -> Self {
        #[cfg(unix)]
        let temp = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let temp = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = create_fixture_root(&temp, nonce);
        let paths = Paths {
            data_dir: root.join("d"),
            config_dir: root.join("c"),
            profile: "default".into(),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let provider = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        provider.set_nonblocking(true).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = format!("http://{}", provider.local_addr().unwrap());
        cfg.provider.model = "private635".into();
        cfg.provider.auto_test_models = false;
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.tools.approval_summaries = false;
        cfg.optimiser.max_attempts_per_day = 0;
        cfg.relay.enabled = false;
        cfg.daemon.backups_keep = 0;
        cfg.mcp.servers.clear();
        cfg.save(&paths).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = formless_runtime::Capabilities::parse(&["process".into()]).unwrap();
        for (name, status) in [
            ("good635", Some("success")),
            ("bad635", Some("failure")),
            ("warn635", None),
        ] {
            let (tool, version) = store
                .create_tool_version(
                    name,
                    "ไทย status fixture",
                    "{\"type\":\"object\"}",
                    "{}",
                    "function run(){return {};}",
                    &caps,
                    64,
                    "user",
                )
                .unwrap();
            store
                .set_tool_title(&tool.id, "ไทย status fixture")
                .unwrap();
            if name == "good635" {
                store.set_approval(&tool.id, &caps, 64).unwrap();
            }
            if let Some(status) = status {
                store
                    .record_invocation(
                        &tool.id, version, None, None, None, status, "fixture", "{}", "{}", "", 1,
                        10,
                    )
                    .unwrap();
            }
            if name == "bad635" {
                store.set_needs_attention(&tool.id, true).unwrap();
            }
            if name == "warn635" {
                store.set_tool_disabled(&tool.id, true).unwrap();
            }
        }
        Self {
            root,
            paths,
            provider,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_formless"));
        c.env_clear();
        for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                c.env(name, value);
            }
        }
        c.args(args)
            .current_dir(&self.root)
            .env("FORMLESS_DATA_DIR", &self.paths.data_dir)
            .env("FORMLESS_CONFIG_DIR", &self.paths.config_dir)
            .env("FORMLESS_API_KEY", "private635-dummy")
            .env("RUST_LOG", "error")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        c
    }
    fn run(&self, args: &[&str], no_color: Option<&str>) -> Output {
        let mut c = self.command(args);
        if let Some(value) = no_color {
            c.env("NO_COLOR", value);
        }
        finish(c.spawn().unwrap())
    }
    fn daemon(&self) -> Owned {
        let mut c = self.command(&["daemon", "serve", "--stay"]);
        c.stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = Owned(c.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !formless_core::daemon::daemon_reachable(&self.paths) {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "private daemon exited"
            );
            assert!(Instant::now() < deadline, "private daemon startup deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        child
    }
    fn no_provider_contact(&self) {
        assert_eq!(
            self.provider.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "status/theme operations never probe a model"
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn fixture_roots_with_the_same_timestamp_are_distinct() {
    struct TestRoot(PathBuf);
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Test allocation directly without constructing the socket-bound fixture.
    let parent = TestRoot(create_fixture_root(&std::env::temp_dir(), 0));
    let start = std::sync::Barrier::new(2);
    let allocate = || {
        start.wait();
        create_fixture_root(&parent.0, 0)
    };
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(allocate);
        let second = allocate();
        (first.join().unwrap(), second)
    });
    assert_ne!(first, second);

    let marker = first.join("owner");
    std::fs::write(&marker, "first").unwrap();
    let third = create_fixture_root(&parent.0, 0);
    assert_ne!(third, first);
    assert_ne!(third, second);
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "first");
    assert!(!second.join("owner").exists());
    assert!(!third.join("owner").exists());

    #[cfg(unix)]
    for directory in [&first, &second, &third] {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(directory).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}

fn finish(child: Child) -> Output {
    let mut child = Owned(child);
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
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
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "owned CLI deadline");
        std::thread::sleep(Duration::from_millis(5));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}
fn success(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn shipped_piped_tables_keep_original_cells_even_with_no_color_or_colorblind_theme() {
    let f = Fixture::new();
    let daemon = f.daemon();
    let before: serde_json::Value =
        serde_json::from_str(&success(f.run(&["tools", "list", "--json"], None))).unwrap();
    // A pipe without NO_COLOR or an explicit theme keeps its original fields.
    let ordinary = success(f.run(&["tools", "list"], None));
    let row = ordinary
        .lines()
        .find(|line| line.starts_with("good635 "))
        .unwrap();
    assert!(row.split_whitespace().any(|cell| cell == "100%"));
    assert!(
        !row.contains("ok 100%") && !row.contains("ok active"),
        "{row}"
    );
    let set = success(f.run(&["config", "set", "tui.theme", "colorblind"], Some("1")));
    assert!(set.contains("colorblind"));
    assert_eq!(
        success(f.run(&["config", "get", "tui.theme"], Some("1"))).trim(),
        "colorblind"
    );
    for no_color in [None, Some("1"), Some("")] {
        let text = success(f.run(&["tools", "list"], no_color));
        assert!(!text.contains('\x1b'));
        for value in [
            "good635",
            "bad635",
            "warn635",
            "100%",
            "0%",
            "active",
            "needs attention",
            "off",
            "ไทย",
        ] {
            assert!(text.contains(value), "missing {value}: {text}");
        }
        assert_eq!(text, ordinary, "theme and NO_COLOR never add pipe fields");
        let json: serde_json::Value =
            serde_json::from_str(&success(f.run(&["tools", "list", "--json"], no_color))).unwrap();
        assert_eq!(
            json, before,
            "human markers never alter machine-readable values"
        );
    }
    let grants = success(f.run(&["approvals", "list"], Some("1")));
    assert!(
        grants.contains("always") && !grants.contains("warn always"),
        "piped scope retains the original cell value: {grants}"
    );
    f.no_provider_contact();
    drop(daemon);
}

#[cfg(unix)]
fn terminal(f: &Fixture, no_color: Option<&str>) -> String {
    use std::os::fd::FromRawFd;
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: 30,
        ws_col: 240,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        },
        0
    );
    let mut master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    let mut command = f.command(&["tools", "list"]);
    if let Some(value) = no_color {
        command.env("NO_COLOR", value);
    }
    command.stdout(Stdio::from(slave));
    let mut child = Owned(command.spawn().unwrap());
    drop(command);
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                Err(e) => panic!("owned PTY read: {e}"),
            }
        }
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "owned PTY CLI deadline");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(status.success());
    String::from_utf8(reader.join().unwrap())
        .unwrap()
        .replace("\r\n", "\n")
}
#[cfg(unix)]
#[test]
fn shipped_real_terminal_uses_shared_colorblind_rgb_and_no_color_keeps_identical_content() {
    let f = Fixture::new();
    let daemon = f.daemon();
    success(f.run(&["config", "set", "tui.theme", "colorblind"], None));
    let colored = terminal(&f, None);
    for tone in [
        formless_tui::SemanticTone::Good,
        formless_tui::SemanticTone::Warn,
        formless_tui::SemanticTone::Bad,
    ] {
        let (r, g, b) = formless_tui::colorblind_rgb(tone);
        assert!(
            colored.contains(&format!("\x1b[38;2;{r};{g};{b}m")),
            "canonical RGB missing"
        );
    }
    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    let visible = ansi.replace_all(&colored, "");
    assert_eq!(
        terminal(&f, Some("")),
        colored,
        "empty NO_COLOR keeps terminal colors"
    );
    for no_color in ["1", "0"] {
        let plain = terminal(&f, Some(no_color));
        assert!(!plain.contains('\x1b'));
        assert_eq!(
            plain, visible,
            "NO_COLOR preserves status information and layout"
        );
    }
    f.no_provider_contact();
    drop(daemon);
}

#[test]
fn static_session_preview_marks_clipped_tool_output_while_json_keeps_it() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.paths.db_file()).unwrap();
    let session = store
        .create_session(Some("preview fixture".into()))
        .unwrap();
    let prefix = "x".repeat(119);
    let tool = format!("{prefix}👍🏽\r\nsecond tool line");
    let assistant = "first answer line\nsecond answer line";
    store.append_message(&session.id, "tool", &tool).unwrap();
    store
        .append_message(&session.id, "assistant", assistant)
        .unwrap();
    let _daemon = fixture.daemon();

    let human = fixture.run(&["sessions", "show", &session.id], Some("1"));
    assert!(human.status.success(), "{human:?}");
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text
        .lines()
        .any(|line| line == format!("  tool result: {prefix}…")));
    assert!(!text.contains("second tool line"));
    assert!(text.contains(assistant));

    let json = fixture.run(&["sessions", "show", &session.id, "--json"], Some("1"));
    assert!(json.status.success(), "{json:?}");
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["messages"][0]["content"], tool);
    assert_eq!(value["messages"][1]["content"], assistant);
    fixture.no_provider_contact();
}

#[test]
fn jobs_history_distinguishes_missing_empty_and_populated_jobs_in_both_formats() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.paths.db_file()).unwrap();
    let job = store
        .create_job(
            "private history fixture",
            None,
            None,
            "2999-01-01T00:00:00Z",
            false,
            "user",
        )
        .unwrap();
    let _daemon = fixture.daemon();
    for json in [false, true] {
        let mut args = vec!["jobs", "history", "unknown-ไทย-日本語"];
        if json {
            args.push("--json");
        }
        let out = fixture.run(&args, Some("1"));
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8(out.stderr)
            .unwrap()
            .contains("no job 'unknown-ไทย-日本語'"));
        args[2] = &job.id;
        let out = fixture.run(&args, Some("1"));
        assert!(out.status.success(), "{out:?}");
        if json {
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
                serde_json::json!([])
            );
        } else {
            assert!(String::from_utf8(out.stdout)
                .unwrap()
                .contains(&format!("{} has not run yet", job.id)));
        }
    }
    store
        .job_run_start(&job.id, None, "manual", "completed", "รายละเอียด 日本語")
        .unwrap();
    for json in [false, true] {
        let mut args = vec!["jobs", "history", &job.id];
        if json {
            args.push("--json");
        }
        let out = fixture.run(&args, Some("1"));
        assert!(out.status.success(), "{out:?}");
        if json {
            let expected = serde_json::to_value(store.job_runs(&job.id, 20).unwrap()).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
                expected
            );
        } else {
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(
                text.contains("completed") && text.contains("รายละเอียด 日本語"),
                "{text}"
            );
            assert!(!text.contains("More may exist."), "{text}");
        }
    }
    assert_eq!(
        fixture.provider.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn jobs_history_limits_the_newest_firings_in_both_formats() {
    let fixture = Fixture::new();
    let store = Store::open(&fixture.paths.db_file()).unwrap();
    let job = store
        .create_job(
            "private limit fixture",
            None,
            None,
            "2999-01-01T00:00:00Z",
            false,
            "user",
        )
        .unwrap();
    for n in 1..=501 {
        store
            .job_run_start(&job.id, None, "manual", "completed", &format!("firing-{n}"))
            .unwrap();
    }
    let _daemon = fixture.daemon();
    for (option, limit, notice) in [
        (None, 20, "Showing newest 20 firings. More may exist."),
        (Some("50"), 50, "Showing newest 50 firings. More may exist."),
        (
            Some("500"),
            500,
            "Showing the maximum of 500 firings. More may exist.",
        ),
    ] {
        let mut args = vec!["jobs", "history", &job.id];
        if let Some(option) = option {
            args.extend(["--limit", option]);
        }
        let expected = store.job_runs(&job.id, limit).unwrap();
        assert_eq!(expected.len(), limit);
        let out = fixture.run(&args, Some("1"));
        assert!(out.status.success(), "{out:?}");
        let text = String::from_utf8(out.stdout).unwrap();
        let lines: Vec<_> = text
            .split_whitespace()
            .filter(|s| s.starts_with("firing-"))
            .collect();
        assert_eq!(
            lines,
            expected
                .iter()
                .map(|row| row.detail.as_str())
                .collect::<Vec<_>>()
        );
        assert!(text.contains(notice), "{text}");

        args.push("--json");
        let out = fixture.run(&args, Some("1"));
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
}
