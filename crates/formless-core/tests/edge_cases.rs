//! SPEC §16 edge cases (normative). Each bullet and where it is tested,
//! so a new bullet can't go untested unnoticed (#759). "Unmapped" means no
//! test has been identified for it yet.
//!
//! - Endpoint unreachable mid-Run → `v2_provider.rs` (retries, failover).
//! - Invalid key / model not found / `/models` down → formless-tui
//!   `wizard_failures_say_why`; `context.rs` `a_failed_model_listing_is_retried`.
//! - Tool creation storm, audit-logged → `creation_attempts_are_audited` (here).
//! - Candidate for a retired Tool; promotion racing retirement →
//!   `optimiser::race_tests::a_retired_tool_is_never_promoted`.
//! - Two creators, same name → `two_creators_of_one_name_are_serialised` (here).
//! - Two approvers, same modal; two clients starting a Run → unmapped.
//! - Panic while a daemon lock is held → `m4.rs` (FORMLESS_TEST_PANIC_MARKER).
//! - Hook command pipes → `hooks.rs`. Bundle URLs → `import_url.rs`.
//! - Protocol mismatch / second daemon → `m1.rs`.
//! - SQLite corruption → `a_damaged_database_stops_with_a_backup_to_restore`
//!   and `a_file_that_is_not_a_database_is_never_replaced` (here).
//! - Disk full → unmapped (#730).
//! - Huge Tool output → `artifacts.rs`. JS infinite loop → unmapped.
//! - Malformed model tool-calls → unmapped.

use formless_core::{Paths, Store};
use formless_runtime::Capabilities;
use std::sync::Arc;

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    paths
}

#[test]
fn a_damaged_database_stops_with_a_backup_to_restore() {
    let paths = temp_paths("corrupt");
    let db = paths.db_file();
    {
        let store = Store::open(&db).unwrap();
        let sid = store.create_session(None).unwrap().id;
        for i in 0..300 {
            store
                .append_message(&sid, "user", &format!("message {i} {}", "x".repeat(200)))
                .unwrap();
        }
        store.audit("test", "seed", "x").unwrap();
    }
    // Check the WAL into the file, then keep a backup beside it.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let backup = db.with_file_name("formless.db.v56-20260101T000000Z.bak");
    std::fs::copy(&db, &backup).unwrap();
    // Flip bytes in the middle of the data pages (not the header).
    let mut bytes = std::fs::read(&db).unwrap();
    assert!(bytes.len() > 40_000, "{}", bytes.len());
    for b in &mut bytes[8192..20480] {
        *b ^= 0xA5;
    }
    std::fs::write(&db, &bytes).unwrap();

    let err = match formless_core::daemon::Daemon::new(paths.clone()) {
        Ok(_) => panic!("a damaged database must not open"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("is damaged"), "{err}");
    assert!(
        err.contains("formless restore") && err.contains(".bak"),
        "{err}"
    );
    assert_eq!(
        std::fs::read(&db).unwrap(),
        bytes,
        "the file is left as it was"
    );
    let _ = std::fs::remove_dir_all(paths.data_dir.parent().unwrap());
}

#[test]
fn a_file_that_is_not_a_database_is_never_replaced() {
    let paths = temp_paths("notdb");
    let db = paths.db_file();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"this is not a database at all, just text").unwrap();
    let err = Store::open(&db)
        .err()
        .map(|e| format!("{e:#}"))
        .unwrap_or_default();
    assert!(
        err.contains("is damaged") && err.contains("header"),
        "{err}"
    );
    assert_eq!(
        std::fs::read(&db).unwrap(),
        b"this is not a database at all, just text"
    );
    let _ = std::fs::remove_dir_all(paths.data_dir.parent().unwrap());
}

/// Two writers creating one name at once: serialised into one Tool with
/// two versions, never two Tools (§7.2).
#[test]
fn two_creators_of_one_name_are_serialised() {
    let paths = temp_paths("samename");
    let db = paths.db_file();
    Store::open(&db).unwrap();
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let db = db.clone();
            std::thread::spawn(move || {
                let store = Store::open(&db).unwrap();
                let caps = Capabilities::parse(&[]).unwrap();
                store
                    .create_tool_version(
                        "dup",
                        "d",
                        r#"{"type":"object"}"#,
                        "{}",
                        &format!("function run(){{ return {i}; }}"),
                        &caps,
                        64,
                        "agent",
                    )
                    .unwrap()
            })
        })
        .collect();
    let made: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(made[0].0.id, made[1].0.id, "one Tool");
    let mut versions: Vec<i64> = made.iter().map(|(_, v)| *v).collect();
    versions.sort();
    assert_eq!(versions, vec![1, 2]);
    let _ = std::fs::remove_dir_all(paths.data_dir.parent().unwrap());
}

/// A creation storm is allowed, and every creation is in the audit log.
#[tokio::test(flavor = "multi_thread")]
async fn creation_attempts_are_audited() {
    let paths = temp_paths("storm");
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    drop(store);
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    for i in 0..6 {
        let out = daemon
            .call_tool_direct(
                &sid,
                "create_tool",
                serde_json::json!({
                    "name": format!("storm_{i}"), "description": "storm",
                    "input_schema": {"type": "object"}, "output_schema": {},
                    "implementation_js": "function run(){ return 1; }", "capabilities": []
                }),
            )
            .await;
        assert!(out.contains("\"created\""), "{out}");
    }
    let store = Store::open(&paths.db_file()).unwrap();
    let audit = store.recent_audit(Some("tool.create"), 50).unwrap();
    let names: Vec<&str> = audit.iter().map(|(_, _, d, _)| d.as_str()).collect();
    for i in 0..6 {
        assert!(
            names.iter().any(|d| d.starts_with(&format!("storm_{i} "))),
            "{names:?}"
        );
    }
    let _ = std::fs::remove_dir_all(paths.data_dir.parent().unwrap());
}
