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
        let file = paths.data_dir.join(format!(
            "formless.db.v{}-2026010{day}T000000Z.bak",
            formless_core::storage::SCHEMA_VERSION
        ));
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
    let valid_pin = std::fs::read(&pin).unwrap();
    std::fs::write(&pin, b"partial receipt").unwrap();
    let blocked = housekeeping::gc(&paths, &store, &Config::default(), false).unwrap();
    assert!(
        files.iter().all(|path| path.exists()),
        "unknown pinned recovery must preserve all copies"
    );
    assert!(blocked["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["kind"] == "migration backups"
            && row["verification_error"]
                .as_str()
                .is_some_and(|e| e.contains("recovery receipt"))));
    std::fs::write(&pin, valid_pin).unwrap();
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

#[test]
fn restore_refuses_current_stamped_incomplete_schema_without_replacing_destination() {
    let (f, paths) = Fixture::new();
    let original = Store::open(&paths.db_file()).unwrap();
    let known = original
        .create_session(Some("preserve destination".into()))
        .unwrap();
    let source = f.0.join("missing-tools.db");
    original.backup_into(&source).unwrap();
    drop(original);
    let conn = Connection::open(&source).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE tools;")
        .unwrap();
    drop(conn);
    let error = housekeeping::restore(&paths, &source).unwrap_err();
    assert!(
        format!("{error:#}").contains("missing table tools"),
        "{error:#}"
    );
    let destination = Store::open(&paths.db_file()).unwrap();
    assert_eq!(destination.list_sessions().unwrap()[0].id, known.id);
}

#[test]
fn a_receipt_never_blesses_later_wal_only_commits() {
    let (f, paths) = Fixture::new();
    let file = f.0.join("wal.bak");
    let conn = historical::create(&file, 3);
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    checks::verify(&paths, &file).unwrap();
    let before = std::fs::read(&file).unwrap();
    conn.execute(
        "INSERT INTO sessions(id,name,created_at,updated_at) VALUES('wal-only','new','','')",
        [],
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&file).unwrap(),
        before,
        "committed update lives only in WAL"
    );
    assert!(checks::verified(&paths, &file).is_none());
    #[cfg(unix)]
    {
        let alias = f.0.join("alias.bak");
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        assert!(
            checks::verified(&paths, &alias).is_none(),
            "aliases cannot hide the real snapshot WAL"
        );
    }
    assert!(format!("{:#}", checks::verify(&paths, &file).unwrap_err()).contains("live WAL"));
    drop(conn);
}

#[test]
fn clock_rollback_never_prunes_the_verified_snapshot_that_backup_returns() {
    let (_fixture, paths) = Fixture::new();
    let store = Store::open(&paths.db_file()).unwrap();
    std::fs::create_dir_all(housekeeping::backup_dir(&paths)).unwrap();
    let future = housekeeping::backup_dir(&paths).join("formless-99990101T000000Z.db");
    store.backup_into(&future).unwrap();
    let destination = housekeeping::backup(&paths, &store, 1).unwrap();
    assert!(destination.is_file());
    assert!(!future.exists());
    assert_eq!(
        housekeeping::list_backups(&paths),
        vec![destination.clone()]
    );
    assert!(checks::verified(&paths, &destination).is_some());
}

#[tokio::test]
async fn historical_restore_drill_migrates_real_v23_and_reads_known_session() {
    let (fixture, paths) = Fixture::new();
    let file = fixture.0.join("genuine-v23.bak");
    let conn = historical::create(&file, 23);
    conn.execute("INSERT INTO sessions(id,name,created_at,updated_at) VALUES('historical-known','Old Session','','')",[]).unwrap();
    drop(conn);
    let original = std::fs::read(&file).unwrap();
    let report = checks::drill(&paths, &file).await.unwrap();
    assert_eq!(
        report["schema_version"],
        formless_core::storage::SCHEMA_VERSION
    );
    assert_eq!(
        report["session_ids"],
        serde_json::json!(["historical-known"])
    );
    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert!(
        !paths.db_file().exists(),
        "drill never creates the live database"
    );
    assert!(!Config::default().daemon.backup_restore_drill);
}

#[test]
fn new_pre_restore_names_and_legacy_copies_share_one_bound_under_clock_rollback() {
    let (fixture, paths) = Fixture::new();
    let store = Store::open(&paths.db_file()).unwrap();
    store.create_session(Some("original".into())).unwrap();
    let source = fixture.0.join("restore-source.db");
    store.backup_into(&source).unwrap();
    let directory = housekeeping::backup_dir(&paths);
    std::fs::create_dir_all(&directory).unwrap();
    let mut legacy = Vec::new();
    for day in 1..=8 {
        let old = directory.join(format!("before-restore-9999010{day}T000000Z.db"));
        store.backup_into(&old).unwrap();
        legacy.push(old);
    }
    drop(store);
    let saved = housekeeping::restore(&paths, &source).unwrap();
    assert!(saved
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("formless.db.pre-restore-"));
    assert!(
        saved.is_file(),
        "clock rollback must not delete the displaced database"
    );
    let copies = housekeeping::list_restore_backups(&paths);
    assert_eq!(copies.len(), housekeeping::RESTORE_BACKUPS_KEEP);
    assert!(copies.contains(&saved));
    assert!(!legacy[0].exists() && !legacy[1].exists());
    assert!(legacy[2..].iter().all(|copy| copy.exists()));
    assert_eq!(
        housekeeping::list_backups(&paths).len(),
        0,
        "recovery names are not daily copies"
    );
}

#[cfg(unix)]
#[test]
fn nonregular_backup_sources_are_rejected_before_opening_a_fifo() {
    let (fixture, paths) = Fixture::new();
    let file = fixture.0.join("fifo.bak");
    let store = Store::open(&fixture.0.join("source.db")).unwrap();
    store.backup_into(&file).unwrap();
    drop(store);
    checks::verify(&paths, &file).unwrap();
    std::fs::remove_file(&file).unwrap();
    let result = std::process::Command::new("mkfifo")
        .arg(&file)
        .status()
        .unwrap();
    assert!(result.success());
    // No FIFO writer is started: opening this as an ordinary input would block.
    assert!(
        format!("{:#}", checks::verify(&paths, &file).unwrap_err()).contains("not a regular file")
    );
    assert!(
        checks::verified(&paths, &file).is_none(),
        "receipt lookup must reject the replacement FIFO too"
    );
}

#[test]
fn fresh_destination_preflight_failure_does_not_claim_a_nonexistent_recovery_copy() {
    let (fixture, paths) = Fixture::new();
    let source = fixture.0.join("incomplete.db");
    let store = Store::open(&source).unwrap();
    drop(store);
    let conn = Connection::open(&source).unwrap();
    conn.execute("DROP TABLE tools", []).unwrap();
    drop(conn);
    let error = housekeeping::restore(&paths, &source).unwrap_err();
    let detail = format!("{error:#}");
    assert!(detail.contains("destination had no database"), "{detail}");
    assert!(!detail.contains("recovery copy at"), "{detail}");
    assert!(detail.contains("missing table tools"), "{detail}");
    assert!(!paths.db_file().exists());
    assert!(housekeeping::list_restore_backups(&paths).is_empty());
}

#[test]
fn selected_new_format_recovery_wal_and_shm_are_not_retention_candidates() {
    let (fixture, paths) = Fixture::new();
    let canonical = Store::open(&paths.db_file()).unwrap();
    canonical.create_session(Some("initial".into())).unwrap();
    let original = fixture.0.join("original.db");
    canonical.backup_into(&original).unwrap();
    drop(canonical);
    let selected = housekeeping::restore(&paths, &original).unwrap();
    let source = Store::open(&selected).unwrap();
    let later = source.create_session(Some("later-only".into())).unwrap();
    let directory = housekeeping::backup_dir(&paths);
    for day in 1..=8 {
        source
            .backup_into(&directory.join(format!("before-restore-9999010{day}T000000Z.db")))
            .unwrap();
    }
    let bogus = PathBuf::from(format!("{}.arbitrary", selected.display()));
    std::fs::write(&bogus, b"not a copy").unwrap();
    let fake_dir = directory.join(format!(
        "formless.db.pre-restore-99990101T000000Z-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&fake_dir).unwrap();
    assert_eq!(
        housekeeping::list_restore_backups(&paths).len(),
        9,
        "only database copies are inventoried"
    );
    let wal = PathBuf::from(format!("{}-wal", selected.display()));
    let shm = PathBuf::from(format!("{}-shm", selected.display()));
    let mut ids_before: Vec<_> = source
        .list_sessions()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    ids_before.sort();
    let before: Vec<_> = [&selected, &wal, &shm]
        .iter()
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    assert!(
        !before[1].is_empty(),
        "later-only Session is committed in the held source WAL"
    );
    let saved = housekeeping::restore(&paths, &selected).unwrap();
    assert!(saved.is_file() && selected.is_file());
    assert_eq!(
        housekeeping::list_restore_backups(&paths).len(),
        housekeeping::RESTORE_BACKUPS_KEEP
    );
    for (path, bytes) in [&selected, &wal].iter().zip(&before) {
        assert_eq!(
            std::fs::read(path).unwrap(),
            *bytes,
            "selected database/WAL bytes are preserved: {}",
            path.display()
        );
    }
    // SQLite's read-only VACUUM snapshot legitimately updates volatile SHM
    // readmarks. Require it to survive at its original size, rather than
    // mistake normal coordination for damage; committed rows and DB/WAL bytes
    // still have strict before/after oracles.
    assert_eq!(
        std::fs::metadata(&shm).unwrap().len(),
        before[2].len() as u64
    );
    let mut ids_after: Vec<_> = source
        .list_sessions()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    ids_after.sort();
    assert_eq!(ids_after, ids_before);
    assert!(ids_after.contains(&later.id));
    let copies = housekeeping::list_restore_backups(&paths);
    assert!(!copies.contains(&wal) && !copies.contains(&shm));
    assert!(
        bogus.exists() && fake_dir.exists(),
        "unrecognized files/directories are never pruned"
    );
    let restored = Store::open(&paths.db_file()).unwrap();
    assert!(restored
        .list_sessions()
        .unwrap()
        .iter()
        .any(|s| s.id == later.id));
    drop(restored);
    drop(source);
}
