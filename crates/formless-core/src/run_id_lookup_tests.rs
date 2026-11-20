use super::*;

#[test]
fn exact_and_prefix_run_queries_search_the_primary_key() {
    let store = Store::open_in_memory().unwrap();
    for (sql, values) in [
        (RUN_INSPECT_EXACT, vec!["abcd"]),
        (RUN_ID_PREFIX_RANGE, vec!["abc", "abd"]),
        (RUN_ID_PREFIX_TAIL, vec!["\u{10ffff}"]),
    ] {
        let mut stmt = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let plan: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(values), |row| row.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plan.iter().any(|step| {
                step.starts_with("SEARCH runs") && step.contains("sqlite_autoindex_runs_1")
            }),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.starts_with("SCAN runs")),
            "{plan:?}"
        );
    }
}

#[test]
fn run_lookup_prefers_exact_ids_and_bounds_literal_prefixes() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.inspect_run("").unwrap().is_none());
    let session = store.create_session(None).unwrap();
    let ids = [
        "a",
        "ab1",
        "ab2",
        "a*?[%_literal",
        "a\0after",
        "x\u{10ffff}extra",
        "y-other",
        "\u{d7ff}suffix",
        "\u{e000}suffix",
        "\u{10ffff}end",
    ];
    for id in ids {
        store.conn.execute(
            "INSERT INTO runs(id,session_id,status,created_at,updated_at) VALUES(?1,?2,'running','','')",
            params![id, session.id],
        ).unwrap();
    }
    assert!(store
        .inspect_run("")
        .unwrap_err()
        .to_string()
        .contains("ambiguous Run id"));
    for (query, expected) in [
        ("a", "a"),
        ("ab1", "ab1"),
        ("a*?[%_", "a*?[%_literal"),
        ("a\0", "a\0after"),
        ("x\u{10ffff}", "x\u{10ffff}extra"),
        ("\u{d7ff}", "\u{d7ff}suffix"),
        ("\u{10ffff}", "\u{10ffff}end"),
    ] {
        let run = store.inspect_run(query).unwrap().unwrap();
        assert_eq!(run["run_id"], expected);
        assert_eq!(run["status"], "running");
        assert_eq!(run["session_id"], session.id);
    }
    assert!(store
        .inspect_run("ab")
        .unwrap_err()
        .to_string()
        .contains("ambiguous Run id"));
    assert!(store.inspect_run("missing").unwrap().is_none());
}

#[test]
fn foreign_database_encoding_keeps_literal_run_lookup() {
    let path = std::env::temp_dir().join(format!("fl-run-encoding-{}.db", uuid::Uuid::new_v4()));
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA encoding='UTF-16le'; CREATE TABLE encoding_fixture(value);")
        .unwrap();
    drop(conn);
    let store = Store::open(&path).unwrap();
    let session = store.create_session(None).unwrap();
    let id = "a99ff626-74d6-4fde-8bd4-1dca56cf4411";
    store.conn.execute(
        "INSERT INTO runs(id,session_id,status,created_at,updated_at) VALUES(?1,?2,'running','','')",
        params![id, session.id],
    ).unwrap();
    assert!(store.inspect_run("Š").unwrap().is_none());
    assert_eq!(store.inspect_run("a99").unwrap().unwrap()["run_id"], id);
    assert_eq!(store.inspect_run(id).unwrap().unwrap()["run_id"], id);
    drop(store);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn opt_in_token_details_follow_the_exact_resolved_run_and_preserve_missing_data() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    for id in ["run-a", "run-ab-long", "run-b"] {
        store.conn.execute(
            "INSERT INTO runs(id,session_id,status,created_at,updated_at) VALUES(?1,?2,'running','','')",
            params![id, session.id],
        ).unwrap();
    }
    for (id, cached, cache_write, reasoning) in [
        ("run-a", 5, 0, 1),
        ("run-a", 2, 0, 3),
        ("run-ab-long", 90, 90, 90),
    ] {
        store
            .record_usage(
                &session.id,
                id,
                "fixture",
                &crate::provider::Usage {
                    cached_tokens: cached,
                    cache_write_tokens: cache_write,
                    reasoning_tokens: reasoning,
                    reported: 3,
                    ..Default::default()
                },
            )
            .unwrap();
    }

    let original = store.inspect_run("run-a").unwrap().unwrap();
    assert!(original.get("token_details").is_none());
    let mut detailed = store
        .inspect_run_with_token_details("run-a", true)
        .unwrap()
        .unwrap();
    let details = detailed
        .as_object_mut()
        .unwrap()
        .remove("token_details")
        .unwrap();
    assert_eq!(
        details,
        json!({"cached_tokens":7,"cache_write_tokens":0,"reasoning_tokens":4})
    );
    assert_eq!(detailed, original);
    let prefix = store
        .inspect_run_with_token_details("run-ab", true)
        .unwrap()
        .unwrap();
    assert_eq!(prefix["run_id"], "run-ab-long");
    assert_eq!(
        prefix["token_details"],
        json!({"cached_tokens":90,"cache_write_tokens":90,"reasoning_tokens":90})
    );
    assert!(store
        .inspect_run_with_token_details("run-", true)
        .unwrap_err()
        .to_string()
        .contains("ambiguous Run id"));
    assert!(store
        .inspect_run_with_token_details("missing", true)
        .unwrap()
        .is_none());
    let missing_usage = store
        .inspect_run_with_token_details("run-b", true)
        .unwrap()
        .unwrap();
    assert_eq!(
        missing_usage["token_details"],
        json!({"cached_tokens":null,"cache_write_tokens":null,"reasoning_tokens":null})
    );
}
