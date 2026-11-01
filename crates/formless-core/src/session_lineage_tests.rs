use super::*;

#[test]
fn roots_imports_and_unknown_legacy_lineage_stay_null_but_forks_capture_real_points() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(Some("Parent".into())).unwrap();
    assert_eq!((parent.forked_from, parent.fork_point_seq), (None, None));
    for text in ["first", "second", "third"] {
        st.append_message(&parent.id, "user", text).unwrap();
    }
    for (requested, point, count) in [
        (None, 3, 3),
        (Some(1), 1, 1),
        (Some(0), 0, 0),
        (Some(999), 3, 3),
        (Some(-1), 0, 0),
    ] {
        let child = st
            .fork_session(&parent.id, None, requested)
            .unwrap()
            .unwrap();
        assert_eq!(child.forked_from.as_deref(), Some(parent.id.as_str()));
        assert_eq!(child.fork_point_seq, Some(point));
        assert_eq!(st.list_messages(&child.id).unwrap().len(), count);
        assert_eq!(st.get_session(&child.id).unwrap().unwrap(), child);
        assert!(st.list_sessions().unwrap().iter().any(|s| s.id == child.id
            && s.forked_from == child.forked_from
            && s.fork_point_seq == child.fork_point_seq));
    }
    let empty = st.create_session(None).unwrap();
    assert_eq!(
        st.fork_session(&empty.id, None, None)
            .unwrap()
            .unwrap()
            .fork_point_seq,
        Some(0)
    );
    let imported = st
        .import_session(
            "Imported",
            None,
            None,
            &[("user".into(), "Independent imported history".into(), None)],
            &[],
        )
        .unwrap();
    assert_eq!(
        (imported.forked_from, imported.fork_point_seq),
        (None, None)
    );
    let count = st.list_sessions().unwrap().len();
    assert!(st
        .fork_session("missing-parent", None, None)
        .unwrap()
        .is_none());
    assert_eq!(st.list_sessions().unwrap().len(), count);
}

#[test]
fn forks_preserve_gapped_sequences_checkpoints_settings_and_independent_usage() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(None).unwrap();
    st.set_session_model(&parent.id, Some("fixture-model".into()), Some("low".into()))
        .unwrap();
    st.set_session_instructions(&parent.id, Some("Standing instructions"))
        .unwrap();
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    st.set_session_cwd(&parent.id, Some(&cwd)).unwrap();
    st.set_session_system_prompt(&parent.id, "Frozen original prompt")
        .unwrap();
    let first = st
        .append_message_meta(
            &parent.id,
            "user",
            "first",
            Some(r#"{"fixture":"retained"}"#.into()),
        )
        .unwrap();
    let gap = st
        .append_message(&parent.id, "assistant", "removed message")
        .unwrap();
    let last = st.append_message(&parent.id, "user", "last").unwrap();
    st.ex("DELETE FROM messages WHERE id=?1", params![gap.id])
        .unwrap();
    st.save_checkpoint(&parent.id, "first checkpoint", first.seq)
        .unwrap();
    st.save_checkpoint(&parent.id, "last checkpoint", last.seq)
        .unwrap();
    let run = st.create_run(&parent.id).unwrap();
    st.record_usage(
        &parent.id,
        &run,
        "fixture-model",
        &crate::provider::Usage {
            prompt_tokens: 42,
            completion_tokens: 7,
            cost: Some(0.2),
            ..Default::default()
        },
    )
    .unwrap();

    let prefix = st.fork_session(&parent.id, None, Some(2)).unwrap().unwrap();
    assert_eq!(
        prefix.fork_point_seq,
        Some(1),
        "record the actual copied source point, not requested missing seq 2"
    );
    assert_eq!(
        st.list_checkpoints(&prefix.id).unwrap(),
        vec![("first checkpoint".into(), 1)]
    );
    let child = st.fork_session(&parent.id, None, None).unwrap().unwrap();
    assert_eq!(child.fork_point_seq, Some(3));
    assert_eq!(child.model.as_deref(), Some("fixture-model"));
    assert_eq!(child.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(child.instructions.as_deref(), Some("Standing instructions"));
    assert_eq!(child.cwd.as_deref(), Some(cwd.as_str()));
    assert_eq!(
        st.session_system_prompt(&child.id).unwrap(),
        None,
        "a fork builds a fresh prompt instead of retaining legacy unsafe frozen metadata"
    );
    let messages = st.list_messages(&child.id).unwrap();
    assert_eq!(messages.iter().map(|m| m.seq).collect::<Vec<_>>(), [1, 3]);
    assert_eq!(messages[0].meta, first.meta);
    assert_eq!(messages[0].created_at, first.created_at);
    assert_ne!(
        messages[0].id, first.id,
        "fork messages have independent IDs"
    );
    assert_eq!(
        st.list_checkpoints(&child.id).unwrap(),
        st.list_checkpoints(&parent.id).unwrap()
    );
    assert!(
        st.usage_rows(Some(&child.id), None).unwrap().0.is_empty(),
        "copied transcript is never copied provider usage"
    );
    assert!(st.last_run(&child.id).unwrap().is_none());
    assert_eq!(st.run_usage(&run).unwrap().prompt_tokens, 42);
    let continuation = st
        .append_message(&child.id, "assistant", "child continuation")
        .unwrap();
    assert_eq!(continuation.seq, 4);
    let nested = st.fork_session(&child.id, None, None).unwrap().unwrap();
    assert_eq!(nested.forked_from.as_deref(), Some(child.id.as_str()));
    assert_eq!(nested.fork_point_seq, Some(4));
    assert_eq!(st.list_messages(&parent.id).unwrap().len(), 2);
}

#[test]
fn deleting_a_parent_preserves_children_and_their_recorded_identity() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(None).unwrap();
    st.append_message(&parent.id, "user", "retained child history")
        .unwrap();
    let child = st.fork_session(&parent.id, None, None).unwrap().unwrap();
    st.soft_delete_session(&parent.id).unwrap();
    assert_eq!(
        st.get_session(&child.id)
            .unwrap()
            .unwrap()
            .forked_from
            .as_deref(),
        Some(parent.id.as_str())
    );
    st.restore_session(&parent.id).unwrap();
    assert!(st.delete_session(&parent.id).unwrap());
    let retained = st.get_session(&child.id).unwrap().unwrap();
    assert_eq!(retained.forked_from.as_deref(), Some(parent.id.as_str()));
    assert_eq!(retained.fork_point_seq, Some(1));
    assert_eq!(
        st.list_messages(&child.id).unwrap()[0].content,
        "retained child history"
    );
    assert_eq!(st.list_sessions().unwrap().len(), 1);
}

#[test]
fn checkpoint_evidence_rebinds_to_unique_child_ids_and_respects_the_cut() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(None).unwrap();
    st.append_message(&parent.id, "user", "first").unwrap();
    let early = st.save_checkpoint(&parent.id, "early summary", 1).unwrap();
    let origin = crate::provenance::Origin::new("read_fixture", "file", "/private/early-fixture");
    st.record_data_origin(&parent.id, &format!("checkpoint:{early}"), &origin)
        .unwrap();
    st.append_message(&parent.id, "assistant", "later").unwrap();
    let later = st.save_checkpoint(&parent.id, "later summary", 2).unwrap();
    let later_origin =
        crate::provenance::Origin::new("read_later", "file", "/private/later-fixture");
    st.record_data_origin(&parent.id, &format!("checkpoint:{later}"), &later_origin)
        .unwrap();
    let child = st.fork_session(&parent.id, None, Some(1)).unwrap().unwrap();
    let (child_checkpoint, summary, point) =
        st.latest_checkpoint_record(&child.id).unwrap().unwrap();
    assert_ne!(child_checkpoint, early);
    assert_ne!(child_checkpoint, later);
    assert_eq!((summary, point), ("early summary".into(), 1));
    assert_eq!(
        st.data_origins(&child.id, &format!("checkpoint:{child_checkpoint}"))
            .unwrap(),
        vec![origin]
    );
    assert!(st
        .data_origins(&child.id, &format!("checkpoint:{early}"))
        .unwrap()
        .is_empty());
    let framed = build_context(&st, &child.id, "Trusted instructions", 100_000, 0.8)
        .unwrap()
        .0;
    let system = framed[0]["content"].as_str().unwrap();
    assert!(system.contains("early summary") && system.contains("/private/early-fixture"));
    assert!(!system.contains("later summary") && !system.contains("/private/later-fixture"));
}

#[test]
fn copy_failure_and_outer_rollback_cannot_leave_partial_lineage_or_history() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(None).unwrap();
    st.append_message(&parent.id, "user", "reject-copy")
        .unwrap();
    st.conn.execute_batch("CREATE TEMP TRIGGER reject_copy BEFORE INSERT ON messages WHEN NEW.content='reject-copy' BEGIN SELECT RAISE(ABORT,'controlled copy failure'); END;").unwrap();
    assert!(st
        .fork_session(&parent.id, Some("Must roll back".into()), None)
        .is_err());
    assert_eq!(st.list_sessions().unwrap().len(), 1);
    assert_eq!(st.list_messages(&parent.id).unwrap().len(), 1);
    st.conn.execute_batch("DROP TRIGGER reject_copy;").unwrap();
    let result: Result<()> = st.transaction(|st| {
        let child = st.fork_session(&parent.id, None, None)?.unwrap();
        assert_eq!(child.fork_point_seq, Some(1));
        anyhow::bail!("controlled outer rollback")
    });
    assert!(result.is_err());
    assert_eq!(st.list_sessions().unwrap().len(), 1);
    let partial: i64 = st
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id<>?1",
            params![parent.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(partial, 0);
}

#[test]
fn prospective_snapshot_forks_use_same_lineage_without_mutating_the_source() {
    let st = Store::open_in_memory().unwrap();
    let parent = st.create_session(None).unwrap();
    st.append_message(&parent.id, "user", "comparison preview")
        .unwrap();
    let preview = st.preview_copy().unwrap();
    let fork = preview
        .fork_session(
            &parent.id,
            Some("Prospective model comparison".into()),
            None,
        )
        .unwrap()
        .unwrap();
    assert_eq!(fork.forked_from.as_deref(), Some(parent.id.as_str()));
    assert_eq!(fork.fork_point_seq, Some(1));
    assert!(st.get_session(&fork.id).unwrap().is_none());
    assert_eq!(st.list_sessions().unwrap().len(), 1);
    assert_eq!(preview.list_sessions().unwrap().len(), 2);
}

fn run_anchors(
    st: &Store,
    run: &str,
) -> (Option<String>, Option<i64>, Option<String>, Option<i64>) {
    st.conn.query_row("SELECT start_message_id,start_message_seq,end_message_id,end_message_seq FROM runs WHERE id=?1", params![run], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
}

#[test]
fn run_boundaries_capture_real_message_ids_and_never_move_on_relabelling() {
    let st = Store::open_in_memory().unwrap();
    let session = st.create_session(None).unwrap();
    let run = st.create_run(&session.id).unwrap();
    assert_eq!(run_anchors(&st, &run), (None, Some(0), None, None));
    let answer = st
        .append_message(&session.id, "assistant", "first answer")
        .unwrap();
    st.set_run_status(&run, "paused").unwrap();
    let expected = (None, Some(0), Some(answer.id.clone()), Some(1));
    assert_eq!(run_anchors(&st, &run), expected);
    let note = st
        .append_message(&session.id, "system-note", "Resume the paused answer")
        .unwrap();
    st.set_run_status(&run, "resumed").unwrap();
    let next = st.create_run(&session.id).unwrap();
    assert_eq!(
        run_anchors(&st, &next),
        (Some(note.id), Some(2), None, None)
    );
    let completion = st
        .append_message(&session.id, "assistant", "continued answer")
        .unwrap();
    st.set_run_status(&next, "completed").unwrap();
    assert_eq!(run_anchors(&st, &next).2, Some(completion.id));
    assert_eq!(
        run_anchors(&st, &run),
        expected,
        "later resumed/abandoned labels cannot claim a newer closing message"
    );
    st.set_run_status(&run, "abandoned").unwrap();
    assert_eq!(run_anchors(&st, &run), expected);

    let fork = st.fork_session(&session.id, None, None).unwrap().unwrap();
    let copied_tail = st.list_messages(&fork.id).unwrap().pop().unwrap();
    let child_run = st.create_run(&fork.id).unwrap();
    assert_eq!(run_anchors(&st, &child_run).0, Some(copied_tail.id));
    assert_eq!(run_anchors(&st, &child_run).1, Some(3));
}

#[test]
fn crash_orphaned_runs_keep_unknown_closing_boundaries_after_restart_actions() {
    let st = Store::open_in_memory().unwrap();
    let session = st.create_session(None).unwrap();
    let message = st
        .append_message(&session.id, "user", "interrupted work")
        .unwrap();
    let run = st.create_run(&session.id).unwrap();
    st.mark_orphan_runs_interrupted().unwrap();
    st.append_message(&session.id, "user", "later history")
        .unwrap();
    st.set_run_status(&run, "abandoned").unwrap();
    assert_eq!(
        run_anchors(&st, &run),
        (Some(message.id), Some(1), None, None),
        "startup cannot fabricate a historic closing point from today's transcript"
    );
}
