use super::*;

fn tool(store: &Store, name: &str) {
    store.conn.execute("INSERT INTO tools(id,name,name_lower,created_at,updated_at) VALUES(?1,?1,?1,'fixture','fixture')", [name]).unwrap();
}

fn run(store: &Store, session: &str, id: &str, date: &str, names: &[&str]) {
    store.conn.execute("INSERT INTO runs(id,session_id,status,created_at,updated_at) VALUES(?1,?2,'completed',?3,?3)",params![id,session,date]).unwrap();
    for (index, name) in names.iter().enumerate() {
        store.conn.execute("INSERT INTO invocations(id,tool_id,version,run_id,status,input,created_at) VALUES(?1,?2,1,?3,'success','{}','same timestamp')",params![format!("{id}-{index}"),name,id]).unwrap();
    }
}

fn seq(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).into()).collect()
}

#[test]
fn tool_sequence_names_preserve_order_duplicates_and_full_equality() {
    let store = Store::open_in_memory().unwrap();
    let current = store.create_session(None).unwrap();
    let other = store.create_session(None).unwrap();
    for name in ["fetch", "summarize", "ไทย"] {
        tool(&store, name);
    }
    run(
        &store,
        &other.id,
        "other",
        "001",
        &["fetch", "summarize", "fetch", "ไทย"],
    );
    // Same timestamps use insertion rowid, including duplicate names.
    let expected = seq(&["fetch", "summarize", "fetch", "ไทย"]);
    assert_eq!(store.run_tool_names("other").unwrap(), expected);
    assert_eq!(
        store
            .run_invocations("other")
            .unwrap()
            .into_iter()
            .map(|(name, _, _)| name)
            .collect::<Vec<_>>(),
        expected
    );
    for names in [
        vec![],
        vec!["fetch"],
        vec!["fetch", "summarize", "ไทย"],
        vec!["summarize", "fetch", "fetch", "ไทย"],
    ] {
        assert!(!store
            .tool_sequence_seen_elsewhere(&current.id, &seq(&names))
            .unwrap());
    }
    assert!(store
        .tool_sequence_seen_elsewhere(&current.id, &expected)
        .unwrap());
    store
        .conn
        .execute(
            "UPDATE invocations SET created_at='zz later' WHERE id='other-0'",
            [],
        )
        .unwrap();
    assert_eq!(
        store.run_tool_names("other").unwrap(),
        seq(&["summarize", "fetch", "ไทย", "fetch"])
    );
}

#[test]
fn tool_sequence_candidates_keep_the_newest_500_eligible_runs() {
    let store = Store::open_in_memory().unwrap();
    let current = store.create_session(None).unwrap();
    let other = store.create_session(None).unwrap();
    for name in ["match", "different"] {
        tool(&store, name);
    }
    run(&store, &other.id, "old match", "0000", &["match"]);
    for index in 1..=499 {
        run(
            &store,
            &other.id,
            &format!("candidate-{index}"),
            &format!("{index:04}"),
            &["different"],
        );
    }
    // These newer Runs must not consume a candidate slot.
    run(&store, &current.id, "same session", "9999", &["match"]);
    run(&store, &other.id, "no invocations", "9998", &[]);
    run(&store, &other.id, "unfinished", "9997", &["match"]);
    store
        .conn
        .execute("UPDATE runs SET status='running' WHERE id='unfinished'", [])
        .unwrap();
    assert!(!store
        .tool_sequence_seen_elsewhere(&current.id, &[])
        .unwrap());
    assert!(store
        .tool_sequence_seen_elsewhere(&current.id, &seq(&["match"]))
        .unwrap());
    run(&store, &other.id, "candidate-500", "0500", &["different"]);
    assert!(!store
        .tool_sequence_seen_elsewhere(&current.id, &seq(&["match"]))
        .unwrap());
    run(&store, &other.id, "new match", "0501", &["match"]);
    assert!(store
        .tool_sequence_seen_elsewhere(&current.id, &seq(&["match"]))
        .unwrap());
}

#[test]
fn tool_sequence_names_skip_unused_corrupt_fields_but_propagate_name_and_sql_errors() {
    let store = Store::open_in_memory().unwrap();
    let current = store.create_session(None).unwrap();
    let other = store.create_session(None).unwrap();
    tool(&store, "fetch");
    run(&store, &other.id, "other", "001", &["fetch"]);
    for field in ["input", "status"] {
        store
            .conn
            .execute(&format!("UPDATE invocations SET {field}=x'FF'"), [])
            .unwrap();
        assert!(store.run_invocations("other").is_err());
        assert!(store
            .tool_sequence_seen_elsewhere(&current.id, &seq(&["fetch"]))
            .unwrap());
        store
            .conn
            .execute(&format!("UPDATE invocations SET {field}='valid'"), [])
            .unwrap();
    }
    store
        .conn
        .execute("UPDATE tools SET name=x'FF'", [])
        .unwrap();
    assert!(store
        .tool_sequence_seen_elsewhere(&current.id, &seq(&["fetch"]))
        .is_err());
    store.conn.execute("DROP TABLE invocations", []).unwrap();
    assert!(store.run_tool_names("other").is_err());
    assert!(store
        .tool_sequence_seen_elsewhere(&current.id, &seq(&["fetch"]))
        .is_err());
}
