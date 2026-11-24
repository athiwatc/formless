use super::*;
use std::sync::atomic::Ordering;

#[test]
fn targeted_message_edit_preserves_fields_truncation_and_rejected_read_only_cases() {
    let store = Store::open_in_memory().unwrap();
    let session = store
        .create_session(Some("private edit fixture".into()))
        .unwrap();
    let other = store.create_session(Some("other fixture".into())).unwrap();
    let prefix = store.append_message(&session.id, "user", "prefix").unwrap();
    let target = store
        .append_message_meta(
            &session.id,
            "user",
            "original",
            Some(r#"{"fixture":"ไทย"}"#.into()),
        )
        .unwrap();
    let assistant = store
        .append_message(&session.id, "assistant", "later reply")
        .unwrap();
    let tool = store
        .append_message(&session.id, "tool", "later result")
        .unwrap();
    let foreign = store.append_message(&other.id, "user", "foreign").unwrap();
    store
        .save_checkpoint(&session.id, "prefix summary", prefix.seq)
        .unwrap();
    store
        .save_checkpoint(&session.id, "covers target", target.seq)
        .unwrap();
    store
        .save_checkpoint(&session.id, "covers later", tool.seq)
        .unwrap();
    let state = serde_json::to_string(&formless_protocol::PlanState::default()).unwrap();
    let mut prefix_plan = String::new();
    for (message, content) in [
        (&prefix, "prefix plan"),
        (&target, "target plan"),
        (&assistant, "later plan"),
    ] {
        let items = serde_json::to_string(&vec![formless_protocol::PlanItem {
            content: content.into(),
            status: "pending".into(),
        }])
        .unwrap();
        store.conn.execute("INSERT INTO session_plan_revisions(session_id,anchor_seq,anchor_message_id,items,state,created_at) VALUES(?1,?2,?3,?4,?5,'fixture')",params![session.id,message.seq,message.id,items,state]).unwrap();
        if message.id == prefix.id {
            prefix_plan = items;
        }
    }
    store
        .conn
        .execute(
            "UPDATE sessions SET updated_at='2000-01-01' WHERE id=?1",
            params![session.id],
        )
        .unwrap();
    let changes: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    let rewrites = store.rewrites.load(Ordering::SeqCst);
    for id in [
        "missing",
        foreign.id.as_str(),
        assistant.id.as_str(),
        tool.id.as_str(),
    ] {
        assert!(store
            .edit_message(&session.id, id, "unused")
            .unwrap()
            .is_none());
    }
    assert!(store.get_message(&other.id, &target.id).unwrap().is_none());
    assert_eq!(
        store
            .conn
            .query_row("SELECT total_changes()", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        changes
    );
    assert_eq!(store.rewrites.load(Ordering::SeqCst), rewrites);
    let edited = store
        .edit_message(&session.id, &target.id, "แก้ไข 日本語")
        .unwrap()
        .unwrap();
    let mut expected = target.clone();
    expected.content = "แก้ไข 日本語".into();
    assert_eq!(
        serde_json::to_value(&edited).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    assert_eq!(store.rewrites.load(Ordering::SeqCst), rewrites + 1);
    assert_eq!(
        store
            .list_messages(&session.id)
            .unwrap()
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        [prefix.id.as_str(), target.id.as_str()]
    );
    assert_eq!(
        store.latest_checkpoint(&session.id).unwrap(),
        Some(("prefix summary".into(), prefix.seq))
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM session_plan_revisions WHERE session_id=?1",
                params![session.id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT plan FROM sessions WHERE id=?1",
                params![session.id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        prefix_plan
    );
    assert_ne!(
        store.get_session(&session.id).unwrap().unwrap().updated_at,
        "2000-01-01"
    );
    assert_eq!(
        store
            .get_message(&other.id, &foreign.id)
            .unwrap()
            .unwrap()
            .content,
        "foreign"
    );
}

#[test]
fn targeted_message_lookup_narrows_only_unrelated_corruption_and_keeps_target_mapper_errors() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let target = store
        .append_message(&session.id, "user", "valid target")
        .unwrap();
    let unrelated = store
        .append_message(&session.id, "assistant", "unrelated 文件")
        .unwrap();
    let unrelated_rowid: i64 = store
        .conn
        .query_row(
            "SELECT rowid FROM messages WHERE id=?1",
            params![unrelated.id],
            |row| row.get(0),
        )
        .unwrap();
    let indexed_unrelated: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages_unicode_fts WHERE rowid=?1",
            params![unrelated_rowid],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexed_unrelated, 1);
    store
        .conn
        .execute(
            "UPDATE messages SET content=x'FF' WHERE id=?1",
            params![unrelated.id],
        )
        .unwrap();
    assert!(
        store.list_messages(&session.id).is_err(),
        "full transcript API still decodes the corrupt row"
    );
    assert_eq!(
        store
            .get_message(&session.id, &target.id)
            .unwrap()
            .unwrap()
            .content,
        "valid target"
    );
    assert!(store
        .edit_message(&session.id, &target.id, "edited")
        .unwrap()
        .is_some());
    let remaining_unrelated_index: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages_unicode_fts WHERE rowid=?1",
            params![unrelated_rowid],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_unrelated_index, 0);
    store
        .conn
        .execute(
            "UPDATE messages SET content=x'FF' WHERE id=?1",
            params![target.id],
        )
        .unwrap();
    let rewrites = store.rewrites.load(Ordering::SeqCst);
    assert!(store.get_message(&session.id, &target.id).is_err());
    assert!(store
        .edit_message(&session.id, &target.id, "must not write")
        .is_err());
    assert_eq!(store.rewrites.load(Ordering::SeqCst), rewrites);
    store
        .conn
        .execute(
            "UPDATE messages SET content='valid again',meta=x'FF' WHERE id=?1",
            params![target.id],
        )
        .unwrap();
    assert!(
        store
            .get_message(&session.id, &target.id)
            .unwrap()
            .unwrap()
            .meta
            .is_none(),
        "unreadable metadata remains absence, as in row_msg"
    );
    assert!(store
        .edit_message(&session.id, &target.id, "metadata policy kept")
        .unwrap()
        .unwrap()
        .meta
        .is_none());
}

#[test]
fn targeted_message_edit_keeps_rewrite_notification_before_a_failing_transaction() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let target = store.append_message(&session.id, "user", "before").unwrap();
    store.conn.execute_batch("CREATE TRIGGER fixture_reject_edit BEFORE UPDATE OF content ON messages BEGIN SELECT RAISE(ABORT,'synthetic edit failure'); END;").unwrap();
    let rewrites = store.rewrites.load(Ordering::SeqCst);
    assert!(store
        .edit_message(&session.id, &target.id, "after")
        .is_err());
    assert_eq!(
        store.rewrites.load(Ordering::SeqCst),
        rewrites + 1,
        "existing pre-transaction invalidation is preserved"
    );
    assert_eq!(
        store
            .get_message(&session.id, &target.id)
            .unwrap()
            .unwrap()
            .content,
        "before"
    );
}
