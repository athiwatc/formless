//! Storage performance work (schema v39): the indexes the query plans
//! asked for, and the Session summary (`message_count`, `preview`,
//! `last_run_status`) denormalised onto the Session row and kept true by
//! triggers, so the Sessions list is one scan instead of three correlated
//! subqueries per Session.

use formless_core::storage::SCHEMA_VERSION;
use formless_core::Store;
use rusqlite::{params, Connection};
use std::path::PathBuf;

fn temp_db(tag: &str) -> PathBuf {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let db = PathBuf::from(format!("/tmp/fl-perf-{tag}-{short}/f.db"));
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    db
}

fn index_names(db: &PathBuf) -> Vec<String> {
    let c = Connection::open(db).unwrap();
    let mut stmt = c
        .prepare("SELECT name FROM sqlite_master WHERE type='index' ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn v39_ships_the_missing_indexes() {
    let db = temp_db("idx");
    let store = Store::open(&db).unwrap();
    assert!(SCHEMA_VERSION >= 39);
    store.create_session(Some("s".into())).unwrap();
    let names = index_names(&db);
    for want in [
        "idx_runs_session",
        "idx_inv_run",
        "idx_inv_session",
        "idx_inv_running",
        "idx_artifacts_session",
        "idx_audit_created",
        "idx_checkpoints_session",
        "idx_run_usage_created",
    ] {
        assert!(names.iter().any(|n| n == want), "missing index {want}");
    }
}

#[test]
fn opening_a_v67_database_adds_query_indexes_once_without_changing_checkpoint_rows() {
    let db = temp_db("checkpoint-usage-indexes");
    let store = Store::open(&db).unwrap();
    let session = store.create_session(Some("fixture".into())).unwrap().id;
    store.save_checkpoint(&session, "first", 5).unwrap();
    store
        .save_checkpoint(&session, "later row at the same sequence", 5)
        .unwrap();
    drop(store);

    let raw = Connection::open(&db).unwrap();
    raw.execute_batch(
        "DROP INDEX idx_checkpoints_session;
         DROP INDEX idx_run_usage_created;
         DROP TABLE messages_unicode_fts;
         PRAGMA user_version=67;",
    )
    .unwrap();
    let before: String = raw.query_row(
        "SELECT summary FROM checkpoints WHERE session_id=?1 ORDER BY up_to_seq DESC, rowid DESC LIMIT 1",
        [&session],
        |row| row.get(0),
    ).unwrap();
    drop(raw);
    assert!(!index_names(&db)
        .iter()
        .any(|n| n == "idx_checkpoints_session"));

    let store = Store::open(&db).unwrap();
    assert_eq!(
        store.latest_checkpoint(&session).unwrap(),
        Some((before, 5))
    );
    assert_eq!(store.list_checkpoints(&session).unwrap().len(), 2);
    let migrated_indexes = index_names(&db);
    assert!(migrated_indexes
        .iter()
        .any(|n| n == "idx_checkpoints_session"));
    assert!(migrated_indexes
        .iter()
        .any(|n| n == "idx_run_usage_created"));
    let raw = Connection::open(&db).unwrap();
    let schema: i64 = raw
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .unwrap();
    assert_eq!(
        raw.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        SCHEMA_VERSION
    );
    drop(store);
    drop(Store::open(&db).unwrap());
    assert_eq!(
        raw.pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        schema
    );
    assert_eq!(migrated_indexes, index_names(&db));
    drop(raw);
    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn session_summary_is_maintained_by_triggers() {
    let db = temp_db("summary");
    let store = Store::open(&db).unwrap();
    let sid = store.create_session(Some("summary".into())).unwrap().id;

    // Nothing yet.
    let s = store.get_session(&sid).unwrap().unwrap();
    assert_eq!(s.message_count, 0);
    assert_eq!(s.preview, None);

    // Appends count, the preview follows the newest user message.
    let m1 = store
        .append_message(&sid, "user", "first question")
        .unwrap();
    store
        .append_message(&sid, "assistant", "an answer")
        .unwrap();
    let s = store.get_session(&sid).unwrap().unwrap();
    assert_eq!(s.message_count, 2, "assistant message counted too");
    assert_eq!(s.preview.as_deref(), Some("first question"));
    store
        .append_message(&sid, "user", "second question")
        .unwrap();
    let s = store.get_session(&sid).unwrap().unwrap();
    assert_eq!(s.message_count, 3);
    assert_eq!(s.preview.as_deref(), Some("second question"));

    // An edit rewrites the preview in place.
    store
        .edit_message(&sid, &m1.id, "edited first question")
        .unwrap();
    let s = store.get_session(&sid).unwrap().unwrap();
    assert_eq!(s.message_count, 1, "everything after the edit is gone");
    assert_eq!(s.preview.as_deref(), Some("edited first question"));

    // Deleting a middle message decrements without touching the preview;
    // deleting the newest user message recomputes it. (Deletes go through
    // a second connection: the Store only deletes messages as part of an
    // edit or a Session delete.)
    let a = store.append_message(&sid, "user", "keep me").unwrap();
    let b = store.append_message(&sid, "user", "drop me").unwrap();
    let raw = Connection::open(&db).unwrap();
    raw.execute("DELETE FROM messages WHERE id=?1", [&b.id])
        .unwrap();
    let s = store.get_session(&sid).unwrap().unwrap();
    assert_eq!(s.message_count, 2);
    assert_eq!(s.preview.as_deref(), Some("keep me"), "preview recomputed");
    raw.execute("DELETE FROM messages WHERE id=?1", [&a.id])
        .unwrap();
    let s = store.get_session(&sid).unwrap().unwrap();
    assert_eq!(s.message_count, 1);
    assert_eq!(s.preview.as_deref(), Some("edited first question"));

    // Runs drive last_run_status, and the newest Run wins even when an
    // older one changes afterwards.
    let r1 = store.create_run(&sid).unwrap();
    assert_eq!(
        store.get_session(&sid).unwrap().unwrap().last_run_status,
        Some("running".to_string())
    );
    store.set_run_status(&r1, "completed").unwrap();
    assert_eq!(
        store.get_session(&sid).unwrap().unwrap().last_run_status,
        Some("completed".to_string())
    );
    let r2 = store.create_run(&sid).unwrap();
    store.set_run_status(&r1, "interrupted").unwrap();
    assert_eq!(
        store.get_session(&sid).unwrap().unwrap().last_run_status,
        Some("running".to_string()),
        "the newest Run is still the one listed"
    );
    store.set_run_status(&r2, "completed").unwrap();

    // The list agrees with the row.
    let listed = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .find(|x| x.id == sid)
        .unwrap();
    assert_eq!(listed.message_count, 1);
    assert_eq!(listed.preview.as_deref(), Some("edited first question"));
    assert_eq!(listed.last_run_status.as_deref(), Some("completed"));
}

/// Migration against a real, pre-v39 database: every Session's summary must
/// equal what the old correlated subqueries computed. Set `FORMLESS_BENCH_DB`
/// to a copy (never the live file: the migration writes).
#[test]
#[ignore = "needs FORMLESS_BENCH_DB pointing at a copy of a real database"]
fn migrates_a_real_database() {
    let path = std::env::var("FORMLESS_BENCH_DB")
        .expect("set FORMLESS_BENCH_DB to a copy of a historical database");
    let path = PathBuf::from(path);
    let started = std::time::Instant::now();
    let store = Store::open(&path).unwrap();
    println!("opened+migrated in {:?}", started.elapsed());

    // The truth, computed the pre-v39 way, through a second connection.
    let c = Connection::open(&path).unwrap();
    let old: Vec<(String, i64, Option<String>, Option<String>)> = {
        let mut stmt = c
            .prepare(
                "SELECT s.id,
                        (SELECT COUNT(*) FROM messages m WHERE m.session_id=s.id),
                        (SELECT content FROM messages m WHERE m.session_id=s.id AND m.role='user'
                           ORDER BY m.seq DESC LIMIT 1),
                        (SELECT status FROM runs r WHERE r.session_id=s.id
                           ORDER BY r.created_at DESC, r.rowid DESC LIMIT 1)
                 FROM sessions s",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let listed = store.list_sessions().unwrap();
    for (id, count, preview, status) in old {
        let Some(row) = listed.iter().find(|s| s.id == id) else {
            continue; // soft-deleted, not listed
        };
        assert_eq!(row.message_count, count, "message_count for {id}");
        let want = preview
            .as_deref()
            .map(|p| {
                p.lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            })
            .map(|p| {
                if p.chars().count() > 80 {
                    p.chars().take(80).collect::<String>() + "…"
                } else {
                    p
                }
            });
        assert_eq!(row.preview, want, "preview for {id}");
        assert_eq!(row.last_run_status, status, "last_run_status for {id}");
    }

    // Same process, same connection settings: the v39 list against the
    // pre-v39 query it replaced.
    let new_sql = "SELECT s.* FROM sessions s WHERE deleted_at IS NULL \
                   ORDER BY archived, pinned DESC, updated_at DESC";
    let old_sql = "SELECT s.*,
        (SELECT COUNT(*) FROM messages m WHERE m.session_id=s.id) AS message_count,
        (SELECT content FROM messages m WHERE m.session_id=s.id AND m.role='user' ORDER BY m.seq DESC LIMIT 1) AS preview,
        (SELECT status FROM runs r WHERE r.session_id=s.id ORDER BY r.created_at DESC, r.rowid DESC LIMIT 1) AS last_run_status
        FROM sessions s WHERE deleted_at IS NULL ORDER BY archived, pinned DESC, updated_at DESC";
    let time = |sql: &str| {
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "PRAGMA busy_timeout=5000; PRAGMA cache_size=-32000; PRAGMA mmap_size=268435456;",
        )
        .unwrap();
        let run = || {
            let mut stmt = c.prepare_cached(sql).unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .count()
        };
        let _ = run();
        let t = std::time::Instant::now();
        for _ in 0..500 {
            std::hint::black_box(run());
        }
        t.elapsed() / 500
    };
    println!("list_sessions, v39 query: {:?}/op", time(new_sql));
    println!("list_sessions, v38 query: {:?}/op", time(old_sql));
    let t = std::time::Instant::now();
    for _ in 0..500 {
        std::hint::black_box(store.list_sessions().unwrap());
    }
    println!("Store::list_sessions:    {:?}/op", t.elapsed() / 500);
}

/// Per-operation latency of the paths the agent loop and the TUI hit.
/// `cargo test -p formless-core --release --test storage_perf -- --ignored
/// --nocapture` (release, or the numbers mean nothing).
#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn write_and_read_latency() {
    let db = temp_db("latency");
    let store = Store::open(&db).unwrap();
    let sid = store.create_session(Some("bench".into())).unwrap().id;
    let run = store.create_run(&sid).unwrap();
    let (tool, version) = store
        .create_tool_version(
            "bench_tool",
            "d",
            "{}",
            "{}",
            "function run(i){return i}",
            &Default::default(),
            256,
            "test",
        )
        .unwrap();

    fn time(label: &str, iters: usize, f: impl Fn()) {
        f();
        let t = std::time::Instant::now();
        for _ in 0..iters {
            f();
        }
        let per_op = t.elapsed() / iters as u32;
        println!("{label:44} {per_op:?}/op");
        // A deliberately generous smoke ceiling, not a machine-specific benchmark.
        assert!(
            per_op < std::time::Duration::from_millis(100),
            "{label}: {per_op:?}/op exceeds 100 ms"
        );
    }

    time("append_message", 400, || {
        std::hint::black_box(
            store
                .append_message(&sid, "user", "a typical message")
                .unwrap(),
        );
    });
    time("start_invocation+finish_invocation", 400, || {
        let id = store
            .start_invocation(&tool.id, version, Some(&sid), Some(&run), None, "{}")
            .unwrap();
        store
            .finish_invocation(&id, &tool.id, "success", "t", "{}", "", 5, 200)
            .unwrap();
    });
    time("bump_run_steps", 400, || {
        std::hint::black_box(store.bump_run_steps(&run).unwrap());
    });
    time("list_sessions", 400, || {
        std::hint::black_box(store.list_sessions().unwrap());
    });
    time("list_messages", 400, || {
        std::hint::black_box(store.list_messages(&sid).unwrap());
    });
    time("search_sessions", 200, || {
        std::hint::black_box(store.search_sessions("typical", 20).unwrap());
    });
}

#[test]
fn tool_summary_groups_invocations_and_windowed_success_by_registry() {
    let db = temp_db("tool-summary");
    let store = Store::open(&db).unwrap();
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let session = store
        .create_session(Some("summary fixture".into()))
        .unwrap()
        .id;
    let (tool, version) = store
        .create_tool_version(
            "summary_fixture",
            "summary fixture",
            r#"{"type":"object"}"#,
            "{}",
            "function run(){}",
            &caps,
            64,
            "test author",
        )
        .unwrap();
    for (status, tag) in [("success", ""), ("failure", "validation"), ("denied", "")] {
        store
            .record_invocation(
                &tool.id,
                version,
                Some(&session),
                None,
                None,
                status,
                tag,
                "{}",
                "",
                "",
                1,
                200,
            )
            .unwrap();
    }

    let listed = store.list_tool_summaries(false).unwrap();
    let summary = listed.iter().find(|row| row.id == tool.id).unwrap();
    assert_eq!(summary.invocations, 3);
    assert_eq!(summary.success_rate, Some(0.5));
    assert_eq!(summary.author, "test author");
    assert!(summary.last_used_at.is_some());
    assert_eq!(summary.capabilities, Vec::<String>::new());
    let windowed = store.windowed_successes(2).unwrap();
    assert_eq!(windowed[&tool.id], (2, Some(0.5)));
    drop(store);
    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn registry_rollup_migration_backfills_existing_invocations() {
    let db = temp_db("registry-rollup-backfill");
    let store = Store::open(&db).unwrap();
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let session = store
        .create_session(Some("rollup migration fixture".into()))
        .unwrap()
        .id;
    let (tool, version) = store
        .create_tool_version(
            "rollup_migration_fixture",
            "rollup migration fixture",
            r#"{"type":"object"}"#,
            "{}",
            "function run(){}",
            &caps,
            64,
            "test author",
        )
        .unwrap();
    for (status, tag) in [("success", ""), ("failure", "validation")] {
        store
            .record_invocation(
                &tool.id,
                version,
                Some(&session),
                None,
                None,
                status,
                tag,
                "{}",
                "",
                "",
                1,
                100,
            )
            .unwrap();
    }
    drop(store);

    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER tool_invocation_summary_insert;
             DROP TRIGGER tool_invocation_summary_update;
             DROP TRIGGER tool_invocation_summary_delete;
             DROP TRIGGER tool_invocation_sessions_insert;
             DROP TRIGGER tool_invocation_sessions_delete;
             DROP TABLE tool_invocation_sessions;
             DROP TABLE tool_invocation_summary;
             DROP INDEX idx_inv_tool_created;
             DROP TABLE messages_unicode_fts;
             PRAGMA user_version=78;",
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&db).unwrap();
    let migrated = store.list_tool_summaries(false).unwrap();
    let summary = migrated.iter().find(|row| row.id == tool.id).unwrap();
    assert_eq!(summary.invocations, 2);
    assert_eq!(summary.success_rate, Some(0.5));
    assert_eq!(summary.author, "test author");
    assert!(summary.last_used_at.is_some());
    drop(store);
    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn tool_definitions_exclude_tools_without_a_current_version() {
    let db = temp_db("missing-current-tool-version");
    let store = Store::open(&db).unwrap();
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let (valid, _) = store
        .create_tool_version(
            "valid_current_version_fixture",
            "valid current version fixture",
            r#"{"type":"object"}"#,
            "{}",
            "function run(){}",
            &caps,
            64,
            "test",
        )
        .unwrap();
    let (dangling, _) = store
        .create_tool_version(
            "missing_current_version_fixture",
            "missing current version fixture",
            r#"{"type":"object"}"#,
            "{}",
            "function run(){}",
            &caps,
            64,
            "test",
        )
        .unwrap();
    drop(store);

    let connection = Connection::open(&db).unwrap();
    connection
        .execute(
            "UPDATE tools SET current_version=999 WHERE id=?1",
            params![&dangling.id],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&db).unwrap();
    let listed = store.list_tool_summaries(false).unwrap();
    assert!(listed.iter().any(|tool| tool.id == dangling.id));
    let definitions = formless_core::tools::tool_definitions(&store, None, 40).unwrap();
    assert!(definitions
        .iter()
        .any(|definition| definition["function"]["name"].as_str() == Some(valid.name.as_str())));
    assert!(!definitions.iter().any(|definition| {
        definition["function"]["name"].as_str() == Some(dangling.name.as_str())
    }));
    drop(store);
    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn windowed_successes_ignores_retired_and_attention_tools() {
    let db = temp_db("windowed-eligible-tools");
    let store = Store::open(&db).unwrap();
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let session = store
        .create_session(Some("windowed fixture".into()))
        .unwrap()
        .id;
    let mut ids = Vec::new();
    for name in ["windowed_active", "windowed_retired", "windowed_attention"] {
        let (tool, version) = store
            .create_tool_version(
                name,
                name,
                r#"{"type":"object"}"#,
                "{}",
                "function run(){}",
                &caps,
                64,
                "test",
            )
            .unwrap();
        store
            .record_invocation(
                &tool.id,
                version,
                Some(&session),
                None,
                None,
                "failure",
                "",
                "{}",
                "",
                "",
                1,
                100,
            )
            .unwrap();
        ids.push(tool.id);
    }
    drop(store);

    let connection = Connection::open(&db).unwrap();
    connection
        .execute("UPDATE tools SET retired=1 WHERE id=?1", params![&ids[1]])
        .unwrap();
    connection
        .execute(
            "UPDATE tools SET needs_attention=1 WHERE id=?1",
            params![&ids[2]],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&db).unwrap();
    let windowed = store.windowed_successes(1).unwrap();
    assert!(windowed.contains_key(&ids[0]));
    assert!(!windowed.contains_key(&ids[1]));
    assert!(!windowed.contains_key(&ids[2]));
    drop(store);
    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
#[ignore = "release-mode scale check for the registry summary hot path"]
fn registry_summary_and_tool_definitions_meet_the_500_by_200_budget() {
    const TOOLS: usize = 500;
    const INVOCATIONS_PER_TOOL: usize = 200;
    let db = temp_db("registry-scale");
    let store = Store::open(&db).unwrap();
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let session_id = store
        .create_session(Some("registry scale fixture".into()))
        .unwrap()
        .id;
    let mut tools = Vec::with_capacity(TOOLS);
    for index in 0..TOOLS {
        let (tool, version) = store
            .create_tool_version(
                &format!("registry_scale_{index:03}"),
                "synthetic registry fixture",
                r#"{"type":"object"}"#,
                "{}",
                "function run(){}",
                &caps,
                64,
                "benchmark",
            )
            .unwrap();
        tools.push((tool.id, version));
    }
    drop(store);

    let mut connection = Connection::open(&db).unwrap();
    let transaction = connection.unchecked_transaction().unwrap();
    {
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO invocations(id,tool_id,version,session_id,status,tag,created_at)
                 VALUES(?1,?2,?3,?4,?5,?6,'2026-01-01T00:00:00+00:00')",
            )
            .unwrap();
        for (tool_index, (tool_id, version)) in tools.iter().enumerate() {
            for invocation_index in 0..INVOCATIONS_PER_TOOL {
                let status = if invocation_index % 2 == 0 {
                    "success"
                } else {
                    "failure"
                };
                insert
                    .execute(params![
                        format!("scale-{tool_index}-{invocation_index}"),
                        tool_id,
                        version,
                        &session_id,
                        status,
                        "",
                    ])
                    .unwrap();
            }
        }
    }
    transaction.commit().unwrap();
    drop(connection);

    let store = Store::open(&db).unwrap();
    let started = std::time::Instant::now();
    let listed = store.list_tool_summaries(false).unwrap();
    let list_elapsed = started.elapsed();
    let definitions_started = std::time::Instant::now();
    let definitions = formless_core::tools::tool_definitions(&store, None, 40).unwrap();
    let definitions_elapsed = definitions_started.elapsed();
    let elapsed = started.elapsed();

    assert_eq!(listed.len(), TOOLS);
    assert!(listed
        .iter()
        .all(|tool| tool.invocations == INVOCATIONS_PER_TOOL as i64));
    assert!(definitions.len() >= 40);
    println!("500 Tools × 200 invocations: list {list_elapsed:?}, tool_definitions {definitions_elapsed:?}, total {elapsed:?}");
    assert!(
        elapsed < std::time::Duration::from_millis(25),
        "registry list + definitions took {elapsed:?}, expected under 25ms in release"
    );
    drop(store);
    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}
