//! Actual CLI commands, isolated directories and child-only environment changes.
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "fl-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let fixture = Self(root);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        fixture
    }
}
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
    // Man pages and completions exceed a pipe buffer. Drain both streams while
    // waiting so the timeout measures the CLI, not a blocked test harness.
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
            panic!("private backup CLI exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}
#[test]
fn restore_preview_leaves_an_existing_locked_profile_untouched() {
    use formless_core::profile_lock::ProfileLock;

    let root = Fixture::new("backup-plan");
    let file = root.0.join("snapshot.bak");
    let args = ["backup", "restore", file.to_str().unwrap(), "--dry-run"];
    let missing = invoke(&root.0, &args);
    assert!(missing.status.success(), "{missing:?}");
    assert!(
        !root.0.join("data").exists() && !root.0.join("config").exists(),
        "dry-run must not create a profile"
    );
    assert!(!file.exists(), "preview must not create the missing source");

    let profile = root.0.file_name().unwrap().to_str().unwrap();
    let paths = formless_core::Paths {
        data_dir: root.0.join("data").join(profile),
        config_dir: root.0.join("config").join(profile),
        profile: profile.to_owned(),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let config = "# Private preview fixture\n";
    std::fs::write(paths.config_file(), config).unwrap();
    let db = paths.db_file();
    let store = formless_core::Store::open(&db).unwrap();
    store
        .create_session(Some("Snapshot Session".into()))
        .unwrap();
    store.backup_into(&file).unwrap();
    store
        .create_session(Some("Current profile Session".into()))
        .unwrap();
    drop(store);
    let source_bytes = std::fs::read(&file).unwrap();
    let current_bytes = std::fs::read(&db).unwrap();
    assert_ne!(source_bytes, current_bytes);

    // The CLI subprocess must succeed without either the ordinary profile
    // lease or the restore launcher lock. Even a dispatch regression could
    // not replace this fixture's database while the guard is held.
    let _guard = ProfileLock::maintenance(&paths, Duration::ZERO)
        .unwrap()
        .freeze()
        .unwrap();
    assert!(ProfileLock::hierarchy_shared(&paths).is_err());
    assert!(ProfileLock::launcher(&paths, Duration::ZERO).is_err());
    let preview = invoke(&root.0, &args);
    assert!(preview.status.success(), "{preview:?}");
    assert!(preview.stderr.is_empty(), "{preview:?}");
    assert_eq!(missing.stdout, preview.stdout, "plan depends only on paths");
    let plan: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(plan["dry_run"], true);
    assert_eq!(plan["source"], serde_json::json!(file));
    assert_eq!(plan["destination"], serde_json::json!(db));
    assert!(plan["steps"].to_string().contains("WAL/SHM"));
    assert_eq!(std::fs::read(&file).unwrap(), source_bytes);
    assert_eq!(std::fs::read(&db).unwrap(), current_bytes);
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        config
    );
    assert!(!paths.data_dir.join("backups").exists());
}

#[test]
fn restore_preview_help_and_backup_hint_describe_the_plan_only() {
    let root = Fixture::new("backup-help");
    let help = invoke(&root.0, &["backup", "restore", "--help"]);
    assert!(help.status.success(), "{help:?}");
    let help = String::from_utf8(help.stdout).unwrap();
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        help.contains("Print the restore plan without verifying the backup"),
        "{help}"
    );
    let alias = invoke(&root.0, &["restore", "--help"]);
    assert!(alias.status.success(), "{alias:?}");
    assert!(!String::from_utf8(alias.stdout)
        .unwrap()
        .contains("--dry-run"));

    let backup = invoke(&root.0, &["backup"]);
    assert!(backup.status.success(), "{backup:?}");
    let text = String::from_utf8(backup.stdout).unwrap();
    assert!(
        text.lines().any(|line| line.starts_with("preview:")
            && line.contains("formless backup restore FILE --dry-run (plan only)")),
        "{text}"
    );
}

#[test]
fn cli_verify_list_restore_drill_read_known_session() {
    let root = Fixture::new("backup-cli");
    let file = root.0.join("snapshot.bak");
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
    let human = invoke(&root.0, &["backup", "list"]);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let human_text = String::from_utf8(human.stdout).unwrap();
    for heading in ["FILE", "SIZE", "VERIFIED AT"] {
        assert!(human_text.contains(heading), "{human_text}");
    }
    assert!(human_text.contains("formless-"), "{human_text}");
    assert!(
        serde_json::from_str::<serde_json::Value>(&human_text).is_err(),
        "default listing is human-readable"
    );
    let list = invoke(&root.0, &["backup", "list", "--json"]);
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

#[test]
fn production_main_thread_runs_help_man_and_completions() {
    let root = Fixture::new("cli-stack");
    for (args, needle) in [
        (&["--help"][..], "Commands:"),
        (&["backup", "--help"][..], "verify"),
        (&["man"][..], ".TH formless"),
        (&["completions", "bash"][..], "_formless()"),
    ] {
        let output = invoke(&root.0, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(needle), "{args:?}: missing {needle}");
    }
}

#[cfg(windows)]
#[test]
fn production_windows_executable_has_the_bounded_main_thread_stack_reserve() {
    // Read the shipped process image, not the test runner. Neither this test
    // nor its child invocation increases any thread/test-runner stack size.
    let image = std::fs::read(env!("CARGO_BIN_EXE_formless")).unwrap();
    assert_eq!(&image[..2], b"MZ");
    let pe = u32::from_le_bytes(image[0x3c..0x40].try_into().unwrap()) as usize;
    assert_eq!(&image[pe..pe + 4], b"PE\0\0");
    let optional = pe + 24;
    let magic = u16::from_le_bytes(image[optional..optional + 2].try_into().unwrap());
    let reserve = match magic {
        0x20b => u64::from_le_bytes(image[optional + 72..optional + 80].try_into().unwrap()),
        0x10b => u32::from_le_bytes(image[optional + 72..optional + 76].try_into().unwrap()) as u64,
        other => panic!("unknown PE optional header {other:#x}"),
    };
    assert_eq!(
        reserve,
        8 * 1024 * 1024,
        "actual production PE main-thread reserve"
    );
}
