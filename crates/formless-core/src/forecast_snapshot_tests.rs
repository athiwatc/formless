use super::*;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> (Self, Paths) {
        let root = std::env::temp_dir().join(format!(
            "fl-forecast-snapshot-test-{}",
            uuid::Uuid::new_v4()
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root).unwrap();
        let paths = Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            profile: "snapshot-proof".into(),
        };
        (Self(root), paths)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn walk(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                result.insert(path.strip_prefix(root).unwrap().to_path_buf(), None);
                walk(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    Some(std::fs::read(path).unwrap()),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

fn closed_wal(paths: &Paths) -> String {
    let store = Store::open(&paths.db_file()).unwrap();
    let id = store
        .create_session(Some("committed only in WAL".into()))
        .unwrap()
        .id;
    // This is genuine closed-WAL state, not a main-DB copy with a lost WAL.
    // Fixture-only SQLite setting leaves its committed WAL on final close.
    unsafe {
        let mut effective = 0;
        check(
            ffi::sqlite3_db_config(
                store.conn.handle(),
                ffi::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                1,
                &mut effective,
            ),
            "retain fixture WAL",
        )
        .unwrap();
        assert_eq!(effective, 1);
    }
    drop(store);
    assert!(
        std::fs::metadata(format!("{}-wal", paths.db_file().display()))
            .unwrap()
            .len()
            > 0
    );
    id
}

#[test]
fn missing_profile_stays_absent_without_a_reader_or_temporary_directory() {
    let (fixture, paths) = Fixture::new();
    let before = tree(&fixture.0);
    assert!(open(&paths).unwrap().is_none());
    assert_eq!(tree(&fixture.0), before);
    assert!(!paths.data_dir.exists() && !paths.config_dir.exists());
}

#[test]
fn closed_wal_is_included_and_every_source_byte_and_path_survives_reader_drop() {
    let (fixture, paths) = Fixture::new();
    let expected = closed_wal(&paths);
    let before = tree(&fixture.0);
    let reader = open(&paths).unwrap().unwrap();
    assert!(reader
        .list_sessions()
        .unwrap()
        .iter()
        .any(|s| s.id == expected));
    let private = reader._forecast_snapshot.as_ref().unwrap().0.clone();
    assert!(private.exists());
    assert_eq!(tree(&fixture.0), before);
    assert!(reader.conn.execute("DELETE FROM sessions", []).is_err());
    drop(reader);
    assert!(!private.exists());
    assert_eq!(tree(&fixture.0), before);
}

#[test]
fn old_schema_migrates_only_in_private_copy_without_creating_source_sidecars() {
    let (fixture, paths) = Fixture::new();
    std::fs::create_dir(&paths.data_dir).unwrap();
    let conn = crate::storage::schema_at(23).unwrap();
    conn.pragma_update(None, "user_version", 23).unwrap();
    conn.execute(
        "INSERT INTO sessions(id,name,created_at,updated_at) VALUES('old','Historical','','')",
        [],
    )
    .unwrap();
    conn.execute("VACUUM INTO ?1", [paths.db_file().to_str().unwrap()])
        .unwrap();
    drop(conn);
    let before = tree(&fixture.0);
    let reader = open(&paths).unwrap().unwrap();
    let version: i64 = reader
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, crate::storage::SCHEMA_VERSION);
    assert_eq!(reader.list_sessions().unwrap()[0].id, "old");
    drop(reader);
    assert_eq!(tree(&fixture.0), before);
}

#[test]
fn possibly_hot_rollback_journal_refuses_without_recovery_or_source_changes() {
    let (fixture, paths) = Fixture::new();
    closed_wal(&paths);
    std::fs::write(
        format!("{}-journal", paths.db_file().display()),
        b"unproven rollback state",
    )
    .unwrap();
    let before = tree(&fixture.0);
    let error = match open(&paths) {
        Ok(_) => panic!("unproven rollback state admitted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("rollback journal"));
    assert_eq!(tree(&fixture.0), before);
}

struct External {
    child: Child,
    lines: mpsc::Receiver<String>,
}
impl External {
    fn spawn(file: &Path, mode: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "storage::forecast_snapshot::tests::external_sqlite_fixture",
                "--nocapture",
            ])
            .env("FORMLESS_FORECAST_FIXTURE_DB", file)
            .env("FORMLESS_FORECAST_FIXTURE_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                let _ = tx.send(line);
            }
        });
        Self { child, lines }
    }
    fn marker(&mut self, expected: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let line = self
                .lines
                .recv_timeout(remaining)
                .expect("owned SQLite fixture did not reach its gate");
            if line.contains(expected) {
                return;
            }
        }
    }
    fn release_and_finish(&mut self) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
        self.marker("FORECAST_FIXTURE_DONE");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned fixture failed to exit"
            );
            std::thread::yield_now();
        }
    }
}
impl Drop for External {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn independently_held_wal_connection_refuses_without_touching_any_source_file() {
    let (fixture, paths) = Fixture::new();
    closed_wal(&paths);
    let mut external = External::spawn(&paths.db_file(), "hold");
    external.marker("FORECAST_FIXTURE_READY");
    let before = tree(&fixture.0);
    let error = match open(&paths) {
        Ok(_) => panic!("active external SQLite reader was admitted"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("cannot exclude SQLite writers"),
        "{error:#}"
    );
    assert_eq!(tree(&fixture.0), before);
    external.release_and_finish();
    assert!(
        open(&paths).unwrap().is_some(),
        "a refused acquisition releases all locks"
    );
}

#[test]
fn cooperating_owner_is_refused_without_a_profile_marker_or_source_write() {
    let (fixture, paths) = Fixture::new();
    closed_wal(&paths);
    let owner = ProfileLock::owner(&paths).unwrap();
    let before = tree(&fixture.0);
    assert!(open(&paths).is_err());
    assert_eq!(tree(&fixture.0), before);
    drop(owner);
    assert!(open(&paths).unwrap().is_some());
}

#[test]
fn an_unproven_default_vfs_is_refused_before_source_open() {
    let (fixture, paths) = Fixture::new();
    closed_wal(&paths);
    let before = tree(&fixture.0);
    let mut external = External::spawn(&paths.db_file(), "unproven-vfs");
    external.marker("FORECAST_FIXTURE_READY");
    external.release_and_finish();
    assert_eq!(tree(&fixture.0), before);
}

#[test]
fn native_exclusive_lock_excludes_independent_wal_append_checkpoint_and_reset() {
    let (fixture, paths) = Fixture::new();
    let expected = closed_wal(&paths);
    let before = tree(&fixture.0);
    let lock = NativeLock::acquire(&std::fs::canonicalize(paths.db_file()).unwrap()).unwrap();
    let mut external = External::spawn(&paths.db_file(), "attempt");
    external.marker("FORECAST_FIXTURE_READY");
    external.release_and_finish();
    // Do not read the main source through another descriptor while this
    // POSIX lock lives: closing that descriptor would release fcntl locks.
    let private = SnapshotDir::new().unwrap();
    lock.copy_main(&private.0.join("locked.db")).unwrap();
    drop(lock);
    assert_eq!(
        tree(&fixture.0),
        before,
        "failed checkpoint/reset/append creates no source sidecars or bytes"
    );
    let reader = open(&paths).unwrap().unwrap();
    assert!(reader
        .list_sessions()
        .unwrap()
        .iter()
        .any(|s| s.id == expected));
    drop(reader);
    assert_eq!(tree(&fixture.0), before);
}

#[test]
#[ignore = "private owned subprocess fixture; parent tests supply exact path and gates"]
fn external_sqlite_fixture() {
    let file = std::env::var_os("FORMLESS_FORECAST_FIXTURE_DB").expect("private fixture path");
    let mode = std::env::var("FORMLESS_FORECAST_FIXTURE_MODE").unwrap();
    if mode == "unproven-vfs" {
        // Private subprocess only: register an unproven VFS name without
        // opening any source SQLite connection, then restore registration.
        unsafe {
            assert_eq!(ffi::sqlite3_initialize(), ffi::SQLITE_OK);
            let original = ffi::sqlite3_vfs_find(std::ptr::null());
            assert!(!original.is_null());
            let mut custom = *original;
            custom.zName = c"forecast-test-unproven".as_ptr();
            custom.pNext = std::ptr::null_mut();
            assert_eq!(ffi::sqlite3_vfs_register(&mut custom, 1), ffi::SQLITE_OK);
            let error = match NativeLock::acquire(Path::new(&file)) {
                Ok(_) => panic!("unproven VFS admitted"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("unsupported/custom SQLite VFS"));
            assert_eq!(ffi::sqlite3_vfs_unregister(&mut custom), ffi::SQLITE_OK);
            assert_eq!(ffi::sqlite3_vfs_register(original, 1), ffi::SQLITE_OK);
        }
        println!("FORECAST_FIXTURE_READY");
        std::io::stdout().flush().unwrap();
        let mut command = String::new();
        BufReader::new(std::io::stdin())
            .read_line(&mut command)
            .unwrap();
        assert_eq!(command, "release\n");
        println!("FORECAST_FIXTURE_DONE");
        std::io::stdout().flush().unwrap();
        return;
    }
    let conn = rusqlite::Connection::open(PathBuf::from(file)).unwrap();
    conn.busy_timeout(Duration::ZERO).unwrap();
    if mode == "hold" {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert!(count > 0);
    } else {
        for sql in ["PRAGMA wal_checkpoint(TRUNCATE)","INSERT INTO sessions(id,name,created_at,updated_at) VALUES('not-admitted','Must not commit','','')"] {
                let error=conn.execute_batch(sql).unwrap_err();
                assert!(matches!(error,rusqlite::Error::SqliteFailure(ref inner,_) if matches!(inner.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked)),"{error}");
            }
    }
    println!("FORECAST_FIXTURE_READY");
    std::io::stdout().flush().unwrap();
    let mut command = String::new();
    BufReader::new(std::io::stdin())
        .read_line(&mut command)
        .unwrap();
    assert_eq!(command, "release\n");
    drop(conn);
    println!("FORECAST_FIXTURE_DONE");
    std::io::stdout().flush().unwrap();
}

#[test]
fn snapshot_retains_configuration_namespace_freeze_until_quote_finishes() {
    let (_fixture, paths) = Fixture::new();
    closed_wal(&paths);
    let snapshot = open(&paths).unwrap().unwrap();
    assert!(
        ProfileLock::hierarchy_shared(&paths).is_err(),
        "configuration writer must not race the snapshot quote"
    );
    crate::Config::load_file_only(&paths).unwrap();
    drop(snapshot);
    assert!(
        ProfileLock::hierarchy_shared(&paths).is_ok(),
        "snapshot must release maintenance on drop"
    );
}
