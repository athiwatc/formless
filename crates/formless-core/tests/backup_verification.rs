#[path = "support/historical.rs"]
mod historical;
use formless_core::{backup_verification as checks, housekeeping, Config, Paths, Store};
use rusqlite::Connection;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> (Self, Paths) {
        let root = std::env::temp_dir().join(format!("fl-verify-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        (Self(root), paths)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn genuine_historical_snapshots_verify_without_upgrade_or_source_mutation() {
    let (f, paths) = Fixture::new();
    for version in [1, 3, 17, 23, 38, 46] {
        let file = f.0.join(format!("v{version}.bak"));
        let conn = historical::create(&file, version);
        conn.execute(
            "INSERT INTO sessions(id,name,created_at,updated_at) VALUES('known','Known','','')",
            [],
        )
        .unwrap();
        drop(conn);
        let before = std::fs::read(&file).unwrap();
        let report = checks::verify(&paths, &file).unwrap();
        assert_eq!(report.schema_version, version);
        assert_eq!(report.tables["sessions"], 1);
        assert_eq!(report.bytes, before.len() as u64);
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert_eq!(
            checks::verified(&paths, &file).unwrap().sha256,
            report.sha256
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for entry in
                std::fs::read_dir(housekeeping::backup_dir(&paths).join(".verified")).unwrap()
            {
                assert_eq!(
                    entry.unwrap().metadata().unwrap().permissions().mode() & 0o077,
                    0
                );
            }
        }
    }
}

#[test]
fn corruption_missing_inventory_and_foreign_keys_fail_and_changed_bytes_invalidate_receipt() {
    let (f, paths) = Fixture::new();
    let file = f.0.join("healthy.bak");
    let conn = historical::create(&file, 3);
    drop(conn);
    checks::verify(&paths, &file).unwrap();
    let healthy = std::fs::read(&file).unwrap();
    let mut corrupt = healthy.clone();
    corrupt[0] ^= 1;
    std::fs::write(&file, &corrupt).unwrap();
    assert!(checks::verified(&paths, &file).is_none());
    assert!(checks::verify(&paths, &file).is_err());
    std::fs::write(&file, &healthy).unwrap();
    let conn = Connection::open(&file).unwrap();
    conn.execute("DROP TABLE messages", []).unwrap();
    drop(conn);
    assert!(format!("{:#}", checks::verify(&paths, &file).unwrap_err())
        .contains("missing table messages"));
    std::fs::write(&file, &healthy).unwrap();
    let conn = Connection::open(&file).unwrap();
    conn.execute("INSERT INTO messages(id,session_id,seq,role,content,created_at) VALUES('orphan','absent',1,'user','x','')",[]).unwrap();
    drop(conn);
    assert!(
        format!("{:#}", checks::verify(&paths, &file).unwrap_err()).contains("foreign_key_check")
    );
}

#[test]
fn migration_retention_verifies_newest_preserves_pinned_copy_and_audits_actual_deletes() {
    let (_f, paths) = Fixture::new();
    let store = Store::open(&paths.db_file()).unwrap();
    let mut files = Vec::new();
    for day in 1..=5 {
        let file = paths
            .data_dir
            .join(format!("formless.db.v23-2026010{day}T000000Z.bak"));
        store.backup_into(&file).unwrap();
        files.push(file);
    }
    let pin = paths.db_file().with_extension("migration-backup.json");
    std::fs::write(
        &pin,
        serde_json::json!({"backup_name":files[0].file_name().unwrap().to_str().unwrap()})
            .to_string(),
    )
    .unwrap();
    let good = std::fs::read(&files[4]).unwrap();
    std::fs::write(&files[4], b"not sqlite").unwrap();
    let report = housekeeping::gc(&paths, &store, &Config::default(), false).unwrap();
    assert!(files.iter().all(|p| p.exists()));
    assert!(report["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["kind"] == "migration backups" && r["verification_error"].is_string()));
    std::fs::write(&files[4], good).unwrap();
    housekeeping::gc(&paths, &store, &Config::default(), false).unwrap();
    assert!(files[0].exists() && files[3].exists() && files[4].exists());
    assert!(!files[1].exists() && !files[2].exists());
    let conn = Connection::open(paths.db_file()).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit WHERE action='backup.prune'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn offline_drill_reads_known_session_and_does_not_run_due_jobs_or_copy_live_config() {
    let (f, paths) = Fixture::new();
    let store = Store::open(&paths.db_file()).unwrap();
    let session = store.create_session(Some("Known Session".into())).unwrap();
    store
        .create_job(
            "MUST NEVER EXECUTE",
            Some(&session.id),
            None,
            "2000-01-01T00:00:00Z",
            true,
            "user",
        )
        .unwrap();
    let file = f.0.join("drill.db");
    store.backup_into(&file).unwrap();
    drop(store);
    std::fs::write(
        paths.config_file(),
        "invalid live configuration must never be loaded",
    )
    .unwrap();
    let before = std::fs::read(&file).unwrap();
    let live = std::fs::read(paths.db_file()).unwrap();
    let report = checks::drill(&paths, &file).await.unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["session_ids"], serde_json::json!([session.id]));
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert_eq!(std::fs::read(paths.db_file()).unwrap(), live);
    let conn = Connection::open(paths.db_file()).unwrap();
    let jobs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE enabled=1 AND approved=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(jobs, 1);
}
