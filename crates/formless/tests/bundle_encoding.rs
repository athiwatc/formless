//! Real import commands against an owned, idle daemon. Imported fixtures are
//! data only: no Tool, skill script or Run is executed.
use base64::Engine;
use formless_core::{Config, Paths};
use serde_json::json;
use std::{
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    daemon: Option<Child>,
}
impl Fixture {
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_formless"));
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("FORMLESS_")
            {
                cmd.env_remove(key);
            }
        }
        cmd.args(["--profile", "bundle-fixture"])
            .args(args)
            .env("FORMLESS_DATA_DIR", self.root.join("data"))
            .env("FORMLESS_CONFIG_DIR", self.root.join("config"))
            .env("FORMLESS_API_KEY", "owned-idle-bundle-fixture")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .stdin(Stdio::null());
        cmd
    }
    fn output(&self, args: &[&str]) -> Output {
        let mut cmd = self.command(args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("fixture CLI exceeded deadline: {args:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "bundle-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let paths = Paths {
            data_dir: root.join("data/bundle-fixture"),
            config_dir: root.join("config/bundle-fixture"),
            profile: "bundle-fixture".into(),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut config = Config::default();
        config.naming.enabled = false;
        config.naming.auto_download = false;
        config.optimiser.scan_interval_secs = 86400;
        config.provider.base_url = "http://127.0.0.1:9/v1".into();
        config.provider.model = "idle-fixture".into();
        config.save(&paths).unwrap();
        let mut fixture = Self { root, daemon: None };
        let mut cmd = fixture.command(&["daemon", "serve", "--stay"]);
        cmd.stdout(Stdio::null()).stderr(Stdio::from(
            std::fs::File::create(fixture.root.join("daemon-stderr")).unwrap(),
        ));
        fixture.daemon = Some(cmd.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let out = fixture.output(&["daemon", "status", "--json"]);
            if out.status.success()
                && serde_json::from_slice::<serde_json::Value>(&out.stdout)
                    .is_ok_and(|v| v["pid"].as_u64().is_some_and(|p| p > 0))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned daemon did not start: {out:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        fixture
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn daemon_status_check_requires_a_live_reply_without_starting_a_daemon() {
    let mut fixture = Fixture::new();
    let healthy = fixture.output(&["daemon", "status", "--check"]);
    assert!(healthy.status.success(), "{healthy:?}");
    assert!(healthy.stdout.is_empty());

    let mut child = fixture.daemon.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let unhealthy = fixture.output(&["daemon", "status", "--check"]);
    assert!(!unhealthy.status.success(), "{unhealthy:?}");
    let status = fixture.output(&["daemon", "status", "--json"]);
    assert!(status.status.success(), "{status:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&status.stdout).unwrap(),
        json!({"running": false}),
    );
}

#[cfg(unix)]
#[test]
fn daemon_serve_cleans_up_after_sigterm_and_sigint() {
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let mut fixture = Fixture::new();
        let paths = Paths {
            data_dir: fixture.root.join("data/bundle-fixture"),
            config_dir: fixture.root.join("config/bundle-fixture"),
            profile: "bundle-fixture".into(),
        };
        assert!(paths.lock_file().exists());
        assert!(paths.socket_path().exists());
        let child = fixture.daemon.as_mut().unwrap();
        // The fixture owns this unreaped child, so its PID cannot be reused.
        assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, signal) }, 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not exit on signal {signal}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "signal {signal}: {status}");
        assert!(!paths.lock_file().exists());
        assert!(!paths.socket_path().exists());
    }
}

#[test]
fn every_cli_import_accepts_utf8_bom_and_names_utf16_conversion_errors() {
    let fixture = Fixture::new();
    let skill=base64::engine::general_purpose::STANDARD.encode("---\nname: bom-fixture\ndescription: Simple fixture notes\n---\nPrivate fixture instructions.\n");
    let bundles = [
        (
            "tools",
            json!({"format":"formless.tool/1","name":"bom_fixture","description":"Simple fixture value","input_schema":{},"output_schema":{},"implementation_js":"function run(input) { return input; }","capabilities":[],"memory_mb":16}),
        ),
        (
            "skills",
            json!({"format":"formless.skill/1","files":[{"path":"SKILL.md","content_b64":skill}]}),
        ),
        (
            "sessions",
            json!({"format":"formless.session/1","name":"BOM Session ชื่อ","messages":[{"role":"user","content":"private fixture"}]}),
        ),
    ];
    for (kind, bundle) in bundles {
        let file = fixture.root.join(format!("{kind} with BOM.json"));
        std::fs::write(&file, format!("\u{feff}{bundle}\r\n")).unwrap();
        let path = file.to_str().unwrap();
        let out = fixture.output(&[kind, "import", path]);
        assert!(out.status.success(), "{kind}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("imported"),
            "{out:?}"
        );
        for bom in [[0xff, 0xfe], [0xfe, 0xff]] {
            std::fs::write(&file, [&bom[..], &[0, 123][..]].concat()).unwrap();
            let out = fixture.output(&[kind, "import", path]);
            let error = String::from_utf8_lossy(&out.stderr);
            assert!(!out.status.success(), "{kind}: {out:?}");
            assert!(
                error.contains(path)
                    && error.contains("UTF-16")
                    && error.contains("re-save it as UTF-8"),
                "{kind}: {error}"
            );
        }
        std::fs::write(&file, "\u{feff}{bad").unwrap();
        let out = fixture.output(&[kind, "import", path]);
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && error.contains(path) && error.contains("not a JSON bundle"),
            "{kind}: {out:?}"
        );
    }
}
