use super::*;
use std::sync::atomic::Ordering;

fn message(store: &Store, session: &str, meta: Option<&str>) -> MessageInfo {
    store
        .append_message_meta(session, "assistant", "body", meta.map(str::to_owned))
        .unwrap()
}

#[test]
fn metadata_removal_preserves_values_order_body_and_tolerant_conversion() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let other = store.create_session(None).unwrap();
    let inputs = [
        None,
        Some("{invalid"),
        Some("[]"),
        Some(" {\"keep\":\"thinking_blocks ไทย\"} "),
        Some(r#"{"thinking_blocks":null,"keep":"ไทย"}"#),
        Some(r#"{"\u0074hinking_blocks":null,"keep":"é"}"#),
        Some(r#"{"thinking_blocks":[],"keep":{"nested":1}}"#),
        Some(r#""thinking_blocks""#),
        Some("null"),
    ];
    let rows: Vec<_> = inputs
        .iter()
        .map(|meta| message(&store, &session.id, *meta))
        .collect();
    let body = "large body ไทย ".repeat(100_000);
    store
        .conn
        .execute(
            "UPDATE messages SET content=?1 WHERE id=?2",
            params![body, rows[4].id],
        )
        .unwrap();
    let blob = message(&store, &session.id, None);
    store
        .conn
        .execute(
            "UPDATE messages SET meta=?1 WHERE id=?2",
            params![b"not text metadata".as_slice(), blob.id],
        )
        .unwrap();
    let other_row = message(&store, &other.id, Some(r#"{"thinking_blocks":null}"#));
    store.conn.execute_batch("CREATE TABLE meta_updates(seq INTEGER); CREATE TRIGGER trace_meta AFTER UPDATE OF meta ON messages BEGIN INSERT INTO meta_updates VALUES(old.seq); END;").unwrap();
    let before = store.rewrites.load(Ordering::SeqCst);
    assert_eq!(store.drop_thinking_blocks(&session.id).unwrap(), 3);
    assert_eq!(store.rewrites.load(Ordering::SeqCst), before + 1);
    for (index, row) in rows.iter().enumerate() {
        let actual: Option<String> = store
            .conn
            .query_row("SELECT meta FROM messages WHERE id=?1", [&row.id], |row| {
                row.get(0)
            })
            .unwrap();
        let expected = match index {
            4 => Some(json!({"keep":"ไทย"}).to_string()),
            5 => Some(json!({"keep":"é"}).to_string()),
            6 => Some(json!({"keep":{"nested":1}}).to_string()),
            _ => inputs[index].map(str::to_owned),
        };
        assert_eq!(actual, expected, "row {index}");
    }
    let audit: Vec<i64> = store
        .conn
        .prepare("SELECT seq FROM meta_updates ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(audit, [5, 6, 7]);
    let untouched: bool = store
        .conn
        .query_row(
            "SELECT content=?1 FROM messages WHERE id=?2",
            params![body, rows[4].id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(untouched);
    let raw: Vec<u8> = store
        .conn
        .query_row("SELECT meta FROM messages WHERE id=?1", [&blob.id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(raw, b"not text metadata");
    let raw: String = store
        .conn
        .query_row(
            "SELECT meta FROM messages WHERE id=?1",
            [&other_row.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, r#"{"thinking_blocks":null}"#);
    assert_eq!(store.drop_thinking_blocks(&session.id).unwrap(), 0);
    assert_eq!(store.drop_thinking_blocks("missing-session").unwrap(), 0);
    assert_eq!(store.rewrites.load(Ordering::SeqCst), before + 1);
}

#[test]
fn unused_corrupt_content_is_not_decoded_but_ids_still_fail_before_updates() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let row = message(&store, &session.id, Some(r#"{"thinking_blocks":null}"#));
    store
        .conn
        .execute(
            "UPDATE messages SET content=?1 WHERE id=?2",
            params![b"blob body".as_slice(), row.id],
        )
        .unwrap();
    assert!(store.list_messages(&session.id).is_err());
    assert_eq!(store.drop_thinking_blocks(&session.id).unwrap(), 1);
    let body: Vec<u8> = store
        .conn
        .query_row(
            "SELECT content FROM messages WHERE id=?1",
            [&row.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(body, b"blob body");

    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let first = message(&store, &session.id, Some(r#"{"thinking_blocks":null}"#));
    let second = message(&store, &session.id, Some(r#"{"thinking_blocks":null}"#));
    store
        .conn
        .execute(
            "UPDATE messages SET id=?1 WHERE id=?2",
            params![b"blob-id".as_slice(), second.id],
        )
        .unwrap();
    let before = store.rewrites.load(Ordering::SeqCst);
    assert!(store.drop_thinking_blocks(&session.id).is_err());
    let meta: String = store
        .conn
        .query_row(
            "SELECT meta FROM messages WHERE id=?1",
            [&first.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(meta, r#"{"thinking_blocks":null}"#);
    assert_eq!(store.rewrites.load(Ordering::SeqCst), before);
}

#[test]
fn late_update_failure_keeps_prior_writes_without_rewrite_note() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let first = message(&store, &session.id, Some(r#"{"thinking_blocks":null}"#));
    let second = message(&store, &session.id, Some(r#"{"thinking_blocks":null}"#));
    store.conn.execute_batch("CREATE TRIGGER reject_second BEFORE UPDATE OF meta ON messages WHEN old.seq=2 BEGIN SELECT RAISE(ABORT,'fixture update failure'); END;").unwrap();
    let before = store.rewrites.load(Ordering::SeqCst);
    let error = store.drop_thinking_blocks(&session.id).unwrap_err();
    assert!(error.to_string().contains("fixture update failure"));
    let read = |id: &str| {
        store
            .conn
            .query_row("SELECT meta FROM messages WHERE id=?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
    };
    assert_eq!(read(&first.id), "{}");
    assert_eq!(read(&second.id), r#"{"thinking_blocks":null}"#);
    assert_eq!(store.rewrites.load(Ordering::SeqCst), before);
}

#[test]
fn metadata_is_collected_before_update_triggers_add_rows() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    message(&store, &session.id, Some(r#"{"thinking_blocks":null}"#));
    message(&store, &session.id, Some(r#"{"thinking_blocks":null}"#));
    store.conn.execute_batch(r#"CREATE TRIGGER append_during_update AFTER UPDATE OF meta ON messages WHEN old.seq=1 BEGIN
        INSERT INTO messages(id,session_id,seq,role,content,created_at,meta) VALUES('added-during-update',new.session_id,999,'assistant','later','fixture','{"thinking_blocks":null}');
        END;"#).unwrap();
    assert_eq!(store.drop_thinking_blocks(&session.id).unwrap(), 2);
    let meta: String = store
        .conn
        .query_row(
            "SELECT meta FROM messages WHERE id='added-during-update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(meta, r#"{"thinking_blocks":null}"#);
}
