use super::*;
use rusqlite::types::Value as SqlValue;

fn legacy_attachment_count(st: &Store, session_id: &str) -> Result<usize> {
    Ok(st
        .list_messages(session_id)?
        .iter()
        .filter_map(|m| m.meta.as_deref())
        .filter_map(|m| serde_json::from_str::<Value>(m).ok())
        .map(|m| m["attachments"].as_array().map_or(0, Vec::len))
        .sum())
}

fn append(st: &Store, session: &str, role: &str, content: &str, meta: Option<&str>) {
    st.append_message_meta(session, role, content, meta.map(str::to_string))
        .unwrap();
}

#[test]
fn attachment_entry_count_matches_legacy_for_mixed_meta_and_large_content() {
    let st = Store::open_in_memory().unwrap();
    let sid = st.create_session(None).unwrap().id;
    let other = st.create_session(None).unwrap().id;
    let empty = st.create_session(None).unwrap().id;
    let escaped = r#"{"\u0061ttachments":[{"name":"文件.txt"},{"name":"a\\b"}]}"#;
    assert!(
        !escaped.contains("\"attachments\""),
        "escaped key must not be a literal attachments field"
    );
    assert_eq!(
        serde_json::from_str::<Value>(escaped).unwrap()["attachments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let not_key = r#"{"\u0062ttachments":[{},{},{},{},{}]}"#;
    let cases = [
        ("user", "zero", Some(r#"{"attachments":[]}"#), 0usize),
        (
            "user",
            "one",
            Some(r#"{"attachments":[{"name":"one.txt"}]}"#),
            1,
        ),
        (
            "user",
            "several",
            Some(r#"{"attachments":[{"name":"文件.png"},{"name":"café"},{"name":"a"}]}"#),
            3,
        ),
        (
            "assistant",
            "assistant-meta",
            Some(r#"{"attachments":[{},{"name":"plan"}]}"#),
            2,
        ),
        (
            "tool",
            "tool-meta",
            Some(r#"{"attachments":[1,"x",null,{"name":"out"}]}"#),
            4,
        ),
        (
            "system-note",
            "any-role",
            Some(r#"{"attachments":[{"name":"note"},{"name":"二"}]}"#),
            2,
        ),
        ("user", "null-meta", None, 0),
        ("user", "malformed", Some("{"), 0),
        ("user", "empty-meta", Some(""), 0),
        ("user", "json-string", Some(r#""just-a-string""#), 0),
        ("user", "json-number", Some("42"), 0),
        ("user", "json-array", Some("[1,2,3]"), 0),
        ("user", "json-bool", Some("true"), 0),
        ("user", "json-null", Some("null"), 0),
        (
            "user",
            "attach-string",
            Some(r#"{"attachments":"file"}"#),
            0,
        ),
        (
            "user",
            "attach-object",
            Some(r#"{"attachments":{"name":"x"}}"#),
            0,
        ),
        ("user", "attach-number", Some(r#"{"attachments":1}"#), 0),
        ("user", "attach-null", Some(r#"{"attachments":null}"#), 0),
        ("user", "escaped-key", Some(escaped), 2),
        ("user", "escaped-other", Some(not_key), 0),
        (
            "user",
            "nested",
            Some(r#"{"note":{"attachments":[{},{}]},"attachments":[{}]}"#),
            1,
        ),
    ];
    let mut expected = 0usize;
    for (role, content, meta, add) in cases {
        append(&st, &sid, role, content, meta);
        expected += add;
    }
    let large = "x".repeat(1_048_576);
    append(
        &st,
        &sid,
        "assistant",
        &large,
        Some(r#"{"attachments":[{"name":"big"}]}"#),
    );
    expected += 1;
    append(&st, &sid, "tool", &large, None);
    let seq: i64 = st
        .conn
        .query_row(
            "SELECT COALESCE(MAX(seq),0) FROM messages WHERE session_id=?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap();
    let blob_meta = br#"{"attachments":[{},{},{}]}"#.to_vec();
    let blob_id = uuid::Uuid::new_v4().to_string();
    st.conn
        .execute(
            "INSERT INTO messages(id,session_id,seq,role,content,created_at,meta) \
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                blob_id,
                sid,
                seq + 1,
                "user",
                "blob-meta",
                "2026-01-01T00:00:00Z",
                SqlValue::Blob(blob_meta.clone()),
            ],
        )
        .unwrap();
    append(
        &st,
        &other,
        "user",
        "elsewhere",
        Some(r#"{"attachments":[{},{},{},{},{},{},{},{},{}]}"#),
    );
    let before = st.list_messages(&sid).unwrap();
    let count = st.attachment_entry_count(&sid).unwrap();
    assert_eq!(count, legacy_attachment_count(&st, &sid).unwrap());
    assert_eq!(count, expected);
    assert_eq!(st.list_messages(&sid).unwrap(), before);
    assert_eq!(st.attachment_entry_count(&empty).unwrap(), 0);
    assert_eq!(st.attachment_entry_count("missing-session").unwrap(), 0);
    assert_eq!(st.attachment_entry_count(&other).unwrap(), 9);
    let stored: Vec<u8> = st
        .conn
        .query_row(
            "SELECT meta FROM messages WHERE id=?1",
            params![blob_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, blob_meta);
    let kind: String = st
        .conn
        .query_row(
            "SELECT typeof(meta) FROM messages WHERE id=?1",
            params![blob_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "blob");
    let large_len: i64 = st
        .conn
        .query_row(
            "SELECT MAX(length(content)) FROM messages WHERE session_id=?1",
            params![sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(large_len as usize, large.len());
}

#[test]
fn attachment_entry_count_counts_meta_when_content_is_unreadable() {
    let st = Store::open_in_memory().unwrap();
    let sid = st.create_session(None).unwrap().id;
    st.conn
        .execute_batch("DROP TRIGGER IF EXISTS messages_fts_ai;")
        .unwrap();
    let meta = r#"{"attachments":[{},{},{}]}"#;
    st.conn
        .execute(
            "INSERT INTO messages(id,session_id,seq,role,content,created_at,meta) \
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                "blob-content",
                sid,
                1_i64,
                "assistant",
                SqlValue::Blob(vec![0xff, 0x00, 0xfe]),
                "2026-01-01T00:00:00Z",
                meta,
            ],
        )
        .unwrap();
    append(
        &st,
        &sid,
        "user",
        "readable",
        Some(r#"{"attachments":[{}]}"#),
    );
    assert!(st.list_messages(&sid).is_err());
    assert_eq!(st.attachment_entry_count(&sid).unwrap(), 4);
    let stored: String = st
        .conn
        .query_row(
            "SELECT meta FROM messages WHERE id='blob-content'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, meta);
}

#[test]
fn attachment_entry_count_propagates_query_and_iteration_errors() {
    let missing = Store::open_in_memory().unwrap();
    let sid = missing.create_session(None).unwrap().id;
    missing
        .conn
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE IF EXISTS messages_fts;
             DROP TABLE messages;",
        )
        .unwrap();
    let missing_error = missing.attachment_entry_count(&sid).unwrap_err();
    assert!(
        format!("{missing_error:#}")
            .to_lowercase()
            .contains("no such table"),
        "{missing_error:#}"
    );

    let overflow = Store::open_in_memory().unwrap();
    let sid = overflow.create_session(None).unwrap().id;
    overflow
        .conn
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE IF EXISTS messages_fts;
             DROP TABLE messages;
             CREATE VIEW messages AS
             SELECT 'm' AS id,
                    sessions.id AS session_id,
                    1 AS seq,
                    'user' AS role,
                    'c' AS content,
                    't' AS created_at,
                    CAST(abs(CASE WHEN length(sessions.id) > 0 THEN -9223372036854775808 ELSE 0 END) AS TEXT) AS meta
             FROM sessions;",
        )
        .unwrap();
    let overflow_error = overflow.attachment_entry_count(&sid).unwrap_err();
    assert!(
        format!("{overflow_error:#}")
            .to_lowercase()
            .contains("overflow"),
        "{overflow_error:#}"
    );
}
