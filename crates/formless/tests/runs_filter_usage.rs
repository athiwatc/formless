//! Combined Runs surfaces must reject incompatible options before profile setup.
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn incompatible_runs_options_exit_before_creating_a_profile_or_connecting() {
    let root = std::env::temp_dir().join(format!(
        "fl-runs-usage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cases: &[(&[&str], &str)] = &[
        (&["runs", "--limit", "20", "running"], "--limit applies"),
        (&["runs", "--limit", "0", "running"], "--limit applies"),
        (&["runs", "--limit", "20", "show", "id"], "--limit applies"),
        (
            &["runs", "--limit", "20", "cancel", "id"],
            "--limit applies",
        ),
        (
            &["runs", "--session", "X", "running"],
            "apply only to the Runs list",
        ),
        (
            &["runs", "--since", "24h", "running"],
            "apply only to the Runs list",
        ),
        (
            &["runs", "--session", "X", "show", "id"],
            "apply only to the Runs list",
        ),
        (
            &["runs", "--since", "24h", "cancel", "id"],
            "apply only to the Runs list",
        ),
        (&["runs", "running", "--limit", "20"], "unexpected argument"),
        (
            &["runs", "--since", "18446744073709551615d"],
            "invalid value",
        ),
    ];
    for (args, expected) in cases {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_formless"));
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("FORMLESS_") || name.contains("API_KEY") || name.contains("OAUTH") {
                cmd.env_remove(key);
            }
        }
        let mut child = cmd
            .args(*args)
            .env("FORMLESS_DATA_DIR", root.join("data"))
            .env("FORMLESS_CONFIG_DIR", root.join("config"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("command exceeded deadline: {args:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expected),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !root.exists(),
            "invalid arguments created profile state: {args:?}"
        );
    }
}
