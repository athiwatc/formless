//! Exercise the shipped restore command with private DB-only fixtures. No
//! daemon is spawned and no service-manager, keyring or user profile is used.
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn restore_prints_missing_reference_warnings_for_a_fresh_profile_and_preserves_rows() {
    let root = Fixture(std::env::temp_dir().join(format!(
            "fl-cli-restore-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    std::fs::create_dir(&root.0).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source =
        formless_core::Paths::from_dirs(root.0.join("source-data"), root.0.join("source-config"))
            .unwrap();
    let store = formless_core::Store::open(&source.db_file()).unwrap();
    store
        .create_session(Some("restore fixture".into()))
        .unwrap();
    store
        .save_artifact(
            None,
            source
                .data_dir
                .join("artifacts/missing.txt")
                .to_str()
                .unwrap(),
            "fixture",
            6,
        )
        .unwrap();
    for version in [1, 2] {
        store
            .add_skill_version(
                "example",
                "fixture",
                version,
                source
                    .data_dir
                    .join(format!("skills/example/v{version}"))
                    .to_str()
                    .unwrap(),
                "fixture",
                "user",
                false,
            )
            .unwrap();
    }
    let backup = root.0.join("snapshot.db");
    store.backup_into(&backup).unwrap();
    drop(store);
    let data = root.0.join("destination-data");
    let config = root.0.join("destination-config");
    let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
    command
        .arg("--profile")
        .arg("restore-reference-fixture")
        .arg("restore")
        .arg(&backup)
        .env("FORMLESS_DATA_DIR", &data)
        .env("FORMLESS_CONFIG_DIR", &config)
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Remove credentials only in this child, leaving HOME and host state intact.
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("FORMLESS_")
            && !matches!(name.as_str(), "FORMLESS_DATA_DIR" | "FORMLESS_CONFIG_DIR")
        {
            command.env_remove(key);
        }
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("private restore fixture timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("database only"), "{stdout}");
    assert!(stdout.contains("previous:  none"), "{stdout}");
    assert!(
        stderr.contains("warning: restored artifact rows: 1 references; 1 missing/invalid"),
        "{stderr}"
    );
    assert!(
        stderr.contains("warning: restored skill versions: 2 references; 2 missing/invalid"),
        "{stderr}"
    );
    let restored =
        formless_core::Store::open(&data.join("restore-reference-fixture/formless.db")).unwrap();
    assert_eq!(restored.list_artifacts().unwrap().len(), 1);
    assert_eq!(restored.get_skill("example").unwrap().unwrap().version, 2);
    assert_eq!(restored.list_sessions().unwrap()[0].name, "restore fixture");
    drop(restored);
}
