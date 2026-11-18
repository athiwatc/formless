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
