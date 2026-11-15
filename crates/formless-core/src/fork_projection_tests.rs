use super::*;

#[test]
fn a_prefix_fork_ignores_corrupt_later_bodies_and_keeps_prefix_errors_strict() {
    let store = Store::open_in_memory().unwrap();
    let source = store.create_session(Some("source".into())).unwrap();
    let first = store
        .append_message(&source.id, "user", "first ไทย")
        .unwrap();
    let later = store
        .append_message(&source.id, "assistant", "later")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE messages SET content=?1 WHERE id=?2",
            params![b"blob body".as_slice(), later.id],
        )
        .unwrap();
    assert!(store.list_messages(&source.id).is_err());
    let fork = store
        .fork_session(&source.id, None, Some(first.seq))
        .unwrap()
        .unwrap();
    assert_eq!(fork.fork_point_seq, Some(first.seq));
    let messages = store.list_messages(&fork.id).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "first ไทย");
    assert_ne!(messages[0].id, first.id);
    let count = || {
        store
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    let before = count();
    assert!(store
        .fork_session(&source.id, None, Some(later.seq))
        .is_err());
    assert!(store.fork_session(&source.id, None, None).is_err());
    assert_eq!(count(), before, "failed full/included-row forks roll back");
}

#[test]
fn bounded_forks_preserve_payloads_boundaries_settings_and_checkpoints() {
    let store = Store::open_in_memory().unwrap();
    let source = store.create_session(Some("source".into())).unwrap();
    store
        .set_session_model(
            &source.id,
            Some("fixture-model".into()),
            Some("high".into()),
        )
        .unwrap();
    store
        .set_session_cwd(&source.id, Some("/fixture/path"))
        .unwrap();
    store
        .set_session_instructions(&source.id, Some("instructions ไทย"))
        .unwrap();
    let first = store
        .append_message_meta(
            &source.id,
            "user",
            "é ไทย",
            Some(" {\"keep\":\"日本語\"} ".into()),
        )
        .unwrap();
    let call = store.append_message_meta(&source.id, "assistant", "call", Some(r#"{"tool_calls":[{"id":"call-one","type":"function","function":{"name":"fixture","arguments":"{}"}}]}"#.into())).unwrap();
    let result = store
        .append_message_meta(
            &source.id,
            "tool",
            "large body ".repeat(100_000).as_str(),
            Some(r#"{"tool_call_id":"call-one"}"#.into()),
        )
        .unwrap();
    store
        .save_checkpoint(&source.id, "early checkpoint", first.seq)
        .unwrap();
    store
        .save_checkpoint(&source.id, "later checkpoint", result.seq)
        .unwrap();
    let original = store.list_messages(&source.id).unwrap();
    for (cut, expected) in [
        (Some(-1), 0),
        (Some(0), 0),
        (Some(call.seq), 2),
        (Some(result.seq), 3),
        (Some(result.seq + 50), 3),
        (None, 3),
    ] {
        let fork = store
            .fork_session(&source.id, Some("branch".into()), cut)
            .unwrap()
            .unwrap();
        assert_eq!(fork.forked_from.as_deref(), Some(source.id.as_str()));
        assert_eq!(
            fork.fork_point_seq,
            Some(if expected == 0 {
                0
            } else {
                original[expected - 1].seq
            })
        );
        assert_eq!(fork.model.as_deref(), Some("fixture-model"));
        assert_eq!(fork.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(fork.cwd.as_deref(), Some("/fixture/path"));
        assert_eq!(fork.instructions.as_deref(), Some("instructions ไทย"));
        let copied = store.list_messages(&fork.id).unwrap();
        assert_eq!(copied.len(), expected);
        for (a, b) in original.iter().zip(&copied) {
            assert_ne!(a.id, b.id);
            assert_eq!(
                (&a.seq, &a.role, &a.content, &a.created_at, &a.meta),
                (&b.seq, &b.role, &b.content, &b.created_at, &b.meta)
            );
        }
        let checkpoints = store.list_checkpoints(&fork.id).unwrap();
        assert_eq!(
            checkpoints.len(),
            match expected {
                0 => 0,
                2 => 1,
                _ => 2,
            }
        );
    }
    assert!(store
        .fork_session("missing", None, Some(1))
        .unwrap()
        .is_none());
    store.soft_delete_session(&source.id).unwrap();
    assert!(store
        .fork_session(&source.id, None, Some(1))
        .unwrap()
        .is_none());
    assert!(store
        .fork_session(&source.id, None, None)
        .unwrap()
        .is_none());
}

#[test]
fn bounded_fork_keeps_optional_metadata_conversion_and_rolls_back_copy_failures() {
    let store = Store::open_in_memory().unwrap();
    let source = store.create_session(None).unwrap();
    let first = store.append_message(&source.id, "user", "first").unwrap();
    store
        .conn
        .execute(
            "UPDATE messages SET meta=?1 WHERE id=?2",
            params![b"blob metadata".as_slice(), first.id],
        )
        .unwrap();
    let fork = store
        .fork_session(&source.id, None, Some(first.seq))
        .unwrap()
        .unwrap();
    assert_eq!(store.list_messages(&fork.id).unwrap()[0].meta, None);
    store.conn.execute_batch("CREATE TRIGGER reject_fork_copy BEFORE INSERT ON messages WHEN new.content='reject-copy' BEGIN SELECT RAISE(ABORT,'fixture copy failure'); END;").unwrap();
    // Seed the rejected content without firing the insert trigger.
    store
        .conn
        .execute(
            "UPDATE messages SET content='reject-copy' WHERE id=?1",
            [&first.id],
        )
        .unwrap();
    let count = store
        .conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    let error = store
        .fork_session(&source.id, None, Some(first.seq))
        .unwrap_err();
    assert!(error.to_string().contains("fixture copy failure"));
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        count
    );
}
