//! Actual CLI commands, isolated directories and child-only environment changes.
use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn invoke(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
    command
        .args(["--profile", root.file_name().unwrap().to_str().unwrap()])
        .args(args)
        .env("FORMLESS_DATA_DIR", root.join("data"))
        .env("FORMLESS_CONFIG_DIR", root.join("config"))
        .env("RUST_LOG", "error")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("FORMLESS_")
            && !matches!(name.as_str(), "FORMLESS_DATA_DIR" | "FORMLESS_CONFIG_DIR")
        {
            command.env_remove(key);
        }
    }
    // HOME remains intact; no startup/daemon/service command is requested.
    // A private explicit dummy key prevents any fallback keychain lookup in
    // bare-backup configuration loading; it is never used for a request.
    command.env("FORMLESS_API_KEY", "offline-backup-fixture-key");
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("private backup CLI exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}
#[test]
fn cli_plan_is_write_free_and_verify_list_restore_drill_read_known_session() {
    let root = Fixture(std::env::temp_dir().join(format!(
            "fl-backup-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
    std::fs::create_dir(&root.0).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let file = root.0.join("snapshot.bak");
    let plan = invoke(
        &root.0,
        &["backup", "restore", file.to_str().unwrap(), "--dry-run"],
    );
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(
        !root.0.join("data").exists() && !root.0.join("config").exists(),
        "dry-run must not create a profile"
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan["dry_run"], true);
    assert!(plan["steps"].to_string().contains("WAL/SHM"));
    let source = root.0.join("source.db");
    let store = formless_core::Store::open(&source).unwrap();
    let known = store
        .create_session(Some("Known recovery Session".into()))
        .unwrap();
    store
        .create_job(
            "MUST NOT RUN",
            Some(&known.id),
            None,
            "2000-01-01T00:00:00Z",
            true,
            "user",
        )
        .unwrap();
    store.backup_into(&file).unwrap();
    drop(store);
    let bytes = std::fs::read(&file).unwrap();
    let verified = invoke(&root.0, &["backup", "verify", file.to_str().unwrap()]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verified: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified["tables"]["sessions"], 1);
    assert!(verified["tables"]["tools"].is_number());
    let drill = invoke(&root.0, &["backup", "drill", file.to_str().unwrap()]);
    assert!(
        drill.status.success(),
        "{}",
        String::from_utf8_lossy(&drill.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&drill.stdout).unwrap();
    assert_eq!(result["passed"], true);
    assert_eq!(result["session_ids"], serde_json::json!([known.id]));
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    let restore = invoke(&root.0, &["backup", "restore", file.to_str().unwrap()]);
    assert!(
        restore.status.success(),
        "{}",
        String::from_utf8_lossy(&restore.stderr)
    );
    let restored = formless_core::Store::open(
        &root
            .0
            .join("data")
            .join(root.0.file_name().unwrap())
            .join("formless.db"),
    )
    .unwrap();
    assert_eq!(restored.list_sessions().unwrap()[0].id, known.id);
    drop(restored);
    // Bare backup remains compatible and creates a verified daily snapshot.
    let backup = invoke(&root.0, &["backup"]);
    assert!(
        backup.status.success(),
        "{}",
        String::from_utf8_lossy(&backup.stderr)
    );
    let list = invoke(&root.0, &["backup", "list"]);
    assert!(list.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["verified_at"].is_string()));
    let doctor = invoke(&root.0, &["doctor", "--restore-drill"]);
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let corrupted = root.0.join("corrupt.bak");
    let mut bad = bytes;
    bad[0] ^= 1;
    std::fs::write(&corrupted, bad).unwrap();
    let failure = invoke(&root.0, &["backup", "verify", corrupted.to_str().unwrap()]);
    assert!(!failure.status.success());
    assert!(!root
        .0
        .join("data")
        .join(root.0.file_name().unwrap())
        .join("daemon.sock")
        .exists());
}
