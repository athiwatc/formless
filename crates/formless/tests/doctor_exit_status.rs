use formless_core::{Config, Paths};
use std::{path::PathBuf, process::Output, time::Duration};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn invoke(paths: &Paths, args: &[&str]) -> Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_formless"));
    command.env_clear();
    for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .args(args)
        .env("FORMLESS_DATA_DIR", &paths.data_dir)
        .env("FORMLESS_CONFIG_DIR", &paths.config_dir)
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn doctor_reports_unhealthy_status_and_bug_report_still_succeeds() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture =
        Fixture(std::env::temp_dir().join(format!("fl-doc-{}-{nonce}", std::process::id())));
    let paths = Paths::from_dirs(fixture.0.join("d"), fixture.0.join("c")).unwrap();
    Config::default().save(&paths).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(paths.config_file(), std::fs::Permissions::from_mode(0o644))
            .unwrap();
    }
    for args in [&["doctor"][..], &["doctor", "--fix"][..]] {
        let result = invoke(&paths, args).await;
        let text = String::from_utf8(result.stdout).unwrap();
        assert_eq!(result.status.code(), Some(1), "{text}");
        assert!(text.contains("problem(s)"), "{text}");
        #[cfg(unix)]
        if args.contains(&"--fix") {
            assert!(text.contains("checking again:") && text.contains("problem(s) left"));
        }
        assert!(!String::from_utf8_lossy(&result.stderr).contains("error:"));
    }
    let result = invoke(&paths, &["doctor", "--json"]).await;
    assert_eq!(result.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(!report["problems"].as_array().unwrap().is_empty());
    assert!(!String::from_utf8_lossy(&result.stderr).contains("error:"));
    let bad_flags = invoke(&paths, &["doctor", "--json", "--fix"]).await;
    assert_eq!(bad_flags.status.code(), Some(2));

    let path = fixture.0.join("report.md");
    let result = invoke(&paths, &["bug-report", "--output", path.to_str().unwrap()]).await;
    assert!(result.status.success(), "{result:?}");
    let report = std::fs::read_to_string(path).unwrap();
    assert!(report.contains("## Doctor") && report.contains("\"problems\""));
}
