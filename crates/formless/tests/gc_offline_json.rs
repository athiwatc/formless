//! Exercise offline GC against owned files without starting a daemon.
use formless_core::{Config, Paths, Store};
use serde_json::Value;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("FORMLESS_") || name.contains("API_KEY") || name.contains("OAUTH") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .args(["--profile", "p"])
            .args(args)
            .env("FORMLESS_DATA_DIR", self.0.join("d"))
            .env("FORMLESS_CONFIG_DIR", self.0.join("c"))
            .env("FORMLESS_API_KEY", "private-unused-offline-fixture")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Owned fixture reports are small enough for both pipes; bound the child.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("offline GC fixture timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn offline_gc_json_reports_owned_cleanup_and_dry_run_preserves_bytes() {
    let fixture = Fixture(std::env::temp_dir().join(format!("fl-offline-gc-{}", uuid_id())));
    let paths = Paths::from_dirs(fixture.0.join("d/p"), fixture.0.join("c/p")).unwrap();
    Config::default().save(&paths).unwrap();
    drop(Store::open(&paths.db_file()).unwrap());
    assert!(
        !paths.socket_path().exists(),
        "no server fixture is created"
    );
    let logs = paths.data_dir.join("mcp");
    std::fs::create_dir_all(&logs).unwrap();
    let log = logs.join("owned.log");
    let original = vec![b'x'; 1_001_234];
    std::fs::write(&log, &original).unwrap();

    for args in [
        ["gc", "--dry-run", "--json"].as_slice(),
        ["gc", "--dry-run"].as_slice(),
    ] {
        let output = fixture.run(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        if args.contains(&"--json") {
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["dry_run"], true);
            let rows = report["kinds"].as_array().unwrap();
            let row = rows.iter().find(|row| row["kind"] == "MCP logs").unwrap();
            assert_eq!(row["items"], 1);
            assert_eq!(row["bytes"], 1234);
            let total: u64 = rows.iter().map(|row| row["bytes"].as_u64().unwrap()).sum();
            assert_eq!(report["total_bytes"], total);
        } else {
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("MCP logs") && text.contains("would free about"));
        }
        assert_eq!(std::fs::read(&log).unwrap(), original);
        assert!(!paths.socket_path().exists());
    }
    let output = fixture.run(&["gc", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["dry_run"], false);
    assert_eq!(std::fs::metadata(&log).unwrap().len(), 1_000_000);
    assert!(!paths.socket_path().exists());
}

fn uuid_id() -> String {
    // No extra dependency in the CLI crate's integration test.
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
