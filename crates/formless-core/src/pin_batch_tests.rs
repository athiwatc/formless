use super::*;
use crate::tools::{preview_tool_definitions, tool_definitions};

fn tool(store: &Store, name: &str) -> String {
    store
        .create_tool_version(
            name,
            "fixture",
            "{}",
            "{}",
            "function run(){return 1;}",
            &Capabilities::default(),
            64,
            "import",
        )
        .unwrap()
        .0
        .id
}
fn snapshot(name: &str, tag: &str) -> String {
    json!({"type":"function","function":{"name":name,"parameters":{"fixture":tag}}}).to_string()
}
fn registry(defs: Vec<Value>) -> Vec<Value> {
    defs.into_iter()
        .filter(|d| !crate::tools::is_builtin(d["function"]["name"].as_str().unwrap()))
        .collect()
}
fn load(store: &Store, session: &str, ids: &[String]) {
    store.load_session_tools(session, ids, "filler").unwrap();
}

#[test]
fn batch_reads_only_targets_and_keeps_conversion_to_missing_across_chunks() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let mut ids = Vec::new();
    for i in 0..260 {
        let id = tool(&store, &format!("tool_{i:03}"));
        store
            .pin_definition(
                &session.id,
                &id,
                i,
                &snapshot(&format!("tool_{i:03}"), "saved"),
            )
            .unwrap();
        ids.push(id);
    }
    let unused = tool(&store, "unused");
    store
        .pin_definition(
            &session.id,
            &unused,
            9,
            "invalid JSON deliberately not parsed",
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE session_tools SET pinned_def=?2 WHERE tool_id=?1",
            params![ids[129], b"blob".as_slice()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE session_tools SET pinned_version='not an integer' WHERE tool_id=?1",
            params![ids[200]],
        )
        .unwrap();
    // Pinned JSON is returned as bytes-as-text, not parsed ahead of listing.
    store
        .conn
        .execute(
            "UPDATE session_tools SET pinned_def='not JSON' WHERE tool_id=?1",
            params![ids[4]],
        )
        .unwrap();
    let refs: Vec<_> = ids.iter().map(String::as_str).collect();
    let pins = store.pinned_definitions_for(&session.id, &refs).unwrap();
    assert_eq!(pins.len(), 258);
    assert!(!pins.contains_key(&unused));
    for id in &ids {
        assert_eq!(
            pins.get(id),
            store.pinned_definition(&session.id, id).unwrap().as_ref()
        );
    }
    assert_eq!(pins[&ids[4]].1, "not JSON");
    store
        .conn
        .execute_batch("DROP TABLE session_tools")
        .unwrap();
    assert!(store
        .pinned_definitions_for(&session.id, &[])
        .unwrap()
        .is_empty());
    assert!(store.pinned_definitions_for(&session.id, &refs).is_err());
}

#[test]
fn listings_preserve_preview_first_pins_snapshots_order_and_inactive_merge() {
    let store = Store::open_in_memory().unwrap();
    let zeta = tool(&store, "zeta");
    let alpha = tool(&store, "alpha");
    let beta = tool(&store, "beta");
    store.set_tool_pinned(&zeta, true).unwrap();
    let session = store.create_session(None).unwrap();
    let preview = preview_tool_definitions(&store, Some(&session.id), 10).unwrap();
    assert!(store.session_tool_ids(&session.id).unwrap().is_empty());
    assert!(store
        .pinned_definition(&session.id, &alpha)
        .unwrap()
        .is_none());
    let first = tool_definitions(&store, Some(&session.id), 10).unwrap();
    assert_eq!(preview, first);
    assert_eq!(
        registry(first.clone())
            .iter()
            .map(|d| d["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["zeta", "alpha", "beta"]
    );
    assert_eq!(
        registry(tool_definitions(&store, None, 10).unwrap()).len(),
        3
    );
    for id in [&zeta, &alpha, &beta] {
        assert!(store.pinned_definition(&session.id, id).unwrap().is_some());
    }
    // A later version cannot replace an already listed Session snapshot.
    tool(&store, "alpha");
    assert_eq!(
        tool_definitions(&store, Some(&session.id), 10).unwrap(),
        first
    );
    store
        .conn
        .execute("UPDATE tools SET retired=1 WHERE id=?1", params![beta])
        .unwrap();
    assert_eq!(
        tool_definitions(&store, Some(&session.id), 10).unwrap(),
        first
    );
    assert_eq!(
        preview_tool_definitions(&store, Some(&session.id), 10).unwrap(),
        first
    );
}

#[test]
fn corrupt_pins_fall_back_locally_without_parsing_unused_rows() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let ids: Vec<_> = ["alpha", "beta", "gamma", "hidden"]
        .iter()
        .map(|name| tool(&store, name))
        .collect();
    load(&store, &session.id, &ids[..3]);
    store
        .pin_definition(&session.id, &ids[0], 7, &snapshot("alpha", "old"))
        .unwrap();
    store
        .pin_definition(&session.id, &ids[1], 8, "not JSON")
        .unwrap();
    store
        .pin_definition(&session.id, &ids[2], 9, "old")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE session_tools SET pinned_def=?2 WHERE tool_id=?1",
            params![ids[2], b"blob".as_slice()],
        )
        .unwrap();
    store
        .pin_definition(&session.id, &ids[3], 9, "hidden")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE session_tools SET source='unloaded',pinned_def=?2 WHERE tool_id=?1",
            params![ids[3], b"unused blob".as_slice()],
        )
        .unwrap();
    let preview = registry(preview_tool_definitions(&store, Some(&session.id), 10).unwrap());
    assert_eq!(preview.len(), 3);
    assert_eq!(preview[0]["function"]["parameters"]["fixture"], "old");
    assert_eq!(
        store
            .pinned_definition(&session.id, &ids[1])
            .unwrap()
            .unwrap()
            .1,
        "not JSON"
    );
    let actual = registry(tool_definitions(&store, Some(&session.id), 10).unwrap());
    assert_eq!(actual, preview);
    assert_eq!(
        store
            .pinned_definition(&session.id, &ids[0])
            .unwrap()
            .unwrap()
            .0,
        7
    );
    for id in &ids[1..3] {
        assert_eq!(
            store.pinned_definition(&session.id, id).unwrap().unwrap().0,
            1
        );
    }
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT typeof(pinned_def) FROM session_tools WHERE tool_id=?1",
                params![ids[3]],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "blob"
    );
}

#[test]
fn fallback_pin_write_invalidates_later_preloaded_snapshots() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let alpha = tool(&store, "alpha");
    let beta = tool(&store, "beta");
    load(&store, &session.id, &[alpha.clone(), beta.clone()]);
    store
        .pin_definition(&session.id, &beta, 7, &snapshot("beta", "before"))
        .unwrap();
    // The first missing pin's write changes a later pin. Legacy per-Tool
    // ordering observes that change, so preloaded data must not hide it.
    store.conn.execute_batch("CREATE TRIGGER change_later_pin AFTER UPDATE OF pinned_def ON session_tools
        WHEN NEW.pinned_version=1 AND (SELECT name FROM tools WHERE id=NEW.tool_id)='alpha'
        BEGIN UPDATE session_tools SET pinned_def=json_set(pinned_def,'$.function.parameters.fixture','after')
        WHERE session_id=NEW.session_id AND tool_id=(SELECT id FROM tools WHERE name='beta'); END;").unwrap();
    let defs = registry(tool_definitions(&store, Some(&session.id), 10).unwrap());
    assert_eq!(defs[1]["function"]["parameters"]["fixture"], "after");
    assert_eq!(
        store
            .pinned_definition(&session.id, &beta)
            .unwrap()
            .unwrap()
            .0,
        7
    );
}

#[test]
fn failed_batch_uses_legacy_reads_without_mass_repinning() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let alpha = tool(&store, "alpha");
    let beta = tool(&store, "beta");
    load(&store, &session.id, &[alpha.clone(), beta.clone()]);
    store
        .pin_definition(&session.id, &alpha, 7, &snapshot("alpha", "keep"))
        .unwrap();
    store
        .pin_definition(&session.id, &beta, 8, &snapshot("beta", "unreadable"))
        .unwrap();
    // A row-dependent SQL failure aborts a multirow query, but an alpha-only
    // legacy read still succeeds. Keep the original target-row error scope.
    store.conn.execute_batch("ALTER TABLE session_tools RENAME TO saved_session_tools;
        CREATE VIEW session_tools AS SELECT session_id,tool_id,source,loaded_at,
        CASE WHEN (SELECT name FROM tools WHERE id=tool_id)='beta' AND source='filler'
        THEN json_extract('broken','$') ELSE pinned_version END AS pinned_version,pinned_def
        FROM saved_session_tools;
        CREATE TRIGGER insert_pin INSTEAD OF INSERT ON session_tools BEGIN
        INSERT OR IGNORE INTO saved_session_tools(session_id,tool_id,source,loaded_at) VALUES(NEW.session_id,NEW.tool_id,NEW.source,NEW.loaded_at);
        UPDATE saved_session_tools SET source='called' WHERE session_id=NEW.session_id AND tool_id=NEW.tool_id; END;
        CREATE TRIGGER update_pin INSTEAD OF UPDATE ON session_tools BEGIN
        UPDATE saved_session_tools SET pinned_version=NEW.pinned_version,pinned_def=NEW.pinned_def WHERE session_id=OLD.session_id AND tool_id=OLD.tool_id; END;").unwrap();
    assert!(store
        .pinned_definitions_for(&session.id, &[&alpha, &beta])
        .is_err());
    assert!(store
        .pinned_definition(&session.id, &alpha)
        .unwrap()
        .is_some());
    assert!(store
        .pinned_definition(&session.id, &beta)
        .unwrap()
        .is_none());
    let defs = registry(tool_definitions(&store, Some(&session.id), 10).unwrap());
    assert_eq!(defs[0]["function"]["parameters"]["fixture"], "keep");
    let version: i64 = store
        .conn
        .query_row(
            "SELECT pinned_version FROM saved_session_tools WHERE tool_id=?1",
            params![alpha],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(version, 7, "a batch error must not repin the readable Tool");
}
