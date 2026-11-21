use super::*;
use formless_protocol::{
    HarnessKind, StepAccess, TopicArtifact, TopicAttemptStatus, TopicFeatureProposal,
    TopicProposalRequest, TopicStatus, WorkspaceKind,
};

fn fixture() -> (Store, SessionInfo, MessageInfo, TopicProposalRequest) {
    let store = Store::open_in_memory().unwrap();
    let source = store.create_session(None).unwrap();
    let message = store
        .append_message(&source.id, "user", "Build X and Y")
        .unwrap();
    let request = TopicProposalRequest {
        parent_topic_id: None,
        source_session_id: source.id.clone(),
        message_ids: vec![message.id.clone()],
        features: vec![],
        name: "Work".into(),
        workspace: Some(PathBuf::from("/topic/repo")),
        outline: vec![],
    };
    (store, source, message, request)
}

#[test]
fn topic_main_identity_is_non_creating_and_persistent() {
    let store = Store::open_in_memory().unwrap();
    assert!(!store.is_main_chat("unknown").unwrap());
    assert!(store.list_sessions().unwrap().is_empty());
    let first = store.main_chat().unwrap();
    assert!(store.is_main_chat(&first.id).unwrap());
    assert_eq!(store.main_chat().unwrap().id, first.id);
}

#[test]
fn topic_generated_messages_cannot_be_proposed_or_approved_and_forks_keep_marker() {
    let (store, source, message, proposal) = fixture();
    let pending = store
        .propose_topic(&proposal, WorkspaceKind::Directory, None)
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE messages SET generated_by='topic_kickoff' WHERE id=?1",
            [&message.id],
        )
        .unwrap();
    assert!(store
        .approve_topic(&pending.id)
        .unwrap_err()
        .to_string()
        .contains("generated"));
    assert!(store
        .get_message(&source.id, &message.id)
        .unwrap()
        .is_some());
    assert!(store
        .get_topic(&pending.id)
        .unwrap()
        .unwrap()
        .topic_session_id
        .is_none());
    assert!(store
        .propose_topic(&proposal, WorkspaceKind::Directory, None)
        .unwrap_err()
        .to_string()
        .contains("generated"));
    let fork = store.fork_session(&source.id, None, None).unwrap().unwrap();
    assert_eq!(
        store.list_messages(&fork.id).unwrap()[0]
            .generated_by
            .as_deref(),
        Some("topic_kickoff")
    );
    let generated = store
        .append_message_generated(
            &source.id,
            "user",
            "new kickoff",
            None,
            Some("topic_kickoff"),
        )
        .unwrap();
    assert_eq!(generated.generated_by.as_deref(), Some("topic_kickoff"));
    assert_eq!(
        store
            .get_message(&source.id, &generated.id)
            .unwrap()
            .unwrap()
            .generated_by,
        generated.generated_by
    );
}

#[test]
fn topic_generated_marker_migrates_v73_and_survives_reopen() {
    let root = std::env::temp_dir().join(format!("fl-generated-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("formless.db");
    let store = Store::open(&path).unwrap();
    let session = store.create_session(None).unwrap();
    let old = store
        .append_message(&session.id, "user", "User request")
        .unwrap();
    store
        .conn
        .execute_batch("ALTER TABLE messages DROP COLUMN generated_by;PRAGMA user_version=73;")
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert!(store
        .get_message(&session.id, &old.id)
        .unwrap()
        .unwrap()
        .generated_by
        .is_none());
    let generated = store
        .append_message_generated(&session.id, "user", "Kickoff", None, Some("topic_kickoff"))
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .get_message(&session.id, &generated.id)
            .unwrap()
            .unwrap()
            .generated_by
            .as_deref(),
        Some("topic_kickoff")
    );
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn topic_context_branch_scope_search_and_external_opt_in() {
    let (store, source, _, _) = fixture();
    let parent = nested_topic(&store, &source.id, None).unwrap();
    let parent = store.approve_topic(&parent.id).unwrap();
    store.claim_topic_workflow(&parent.id).unwrap();
    let ancestor = store
        .begin_topic_attempt(
            &parent.id,
            &parent.features[0].id,
            "Prior",
            StepAccess::ReadOnly,
            HarnessKind::Claude,
            None,
            None,
            &[],
            &"token".repeat(8),
            Path::new("/topic/prior"),
        )
        .unwrap();
    store
        .report_topic_result(
            &ancestor.id,
            &"token".repeat(8),
            TopicAttemptStatus::Succeeded,
            "done",
            &[],
        )
        .unwrap();
    store
        .finish_topic_feature(
            parent.topic_session_id.as_ref().unwrap(),
            &parent.features[0].id,
            true,
            "done",
        )
        .unwrap();
    let parent_session = parent.topic_session_id.as_ref().unwrap();
    let child = nested_topic(&store, parent_session, None).unwrap();
    let child = store.approve_topic(&child.id).unwrap();
    store.claim_topic_workflow(&child.id).unwrap();
    let own = store
        .begin_topic_attempt(
            &child.id,
            &child.features[0].id,
            "Own",
            StepAccess::ReadOnly,
            HarnessKind::Claude,
            None,
            None,
            &[],
            &"child".repeat(8),
            Path::new("/topic/own"),
        )
        .unwrap();
    let sibling = nested_topic(&store, &source.id, Some(parent.id.clone())).unwrap();
    let sibling = store.approve_topic(&sibling.id).unwrap();
    let sibling_session = sibling.topic_session_id.unwrap();
    let own_session = child.topic_session_id.as_ref().unwrap();
    for session in [
        own_session,
        parent_session,
        &own.conversation_session_id,
        &ancestor.conversation_session_id,
        &sibling_session,
        &source.id,
    ] {
        store
            .append_message(session, "user", "A NeEdLe to locate")
            .unwrap();
    }
    for session in [
        own_session,
        parent_session,
        &own.conversation_session_id,
        &ancestor.conversation_session_id,
    ] {
        assert!(store.topic_read_conversation(&child.id, session).is_ok());
    }
    for session in [&source.id, &sibling_session] {
        assert!(store.topic_read_conversation(&child.id, session).is_err());
    }
    let matches = store
        .topic_search_conversations(&child.id, "needle")
        .unwrap();
    assert_eq!(matches.as_array().unwrap().len(), 4);
    assert_eq!(
        store.topic_search_conversations(&child.id, " ").unwrap(),
        json!([])
    );
    assert!(store
        .topic_search_conversations(&child.id, &"x".repeat(1001))
        .is_err());
    assert!(!child.allow_external_context);
    store.set_topic_context_access(&child.id, true).unwrap();
    assert!(store.topic_read_conversation(&child.id, &source.id).is_ok());
    assert!(store
        .topic_read_conversation(&child.id, &sibling_session)
        .is_ok());
    assert_eq!(
        store
            .topic_search_conversations(&child.id, "NEEDLE")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        6
    );
    store.soft_delete_session(&sibling_session).unwrap();
    assert!(store
        .topic_read_conversation(&child.id, &sibling_session)
        .is_err());
    for _ in 0..45 {
        store.append_message(own_session, "user", "needle").unwrap();
    }
    assert_eq!(
        store
            .topic_search_conversations(&child.id, "needle")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        40
    );
}

#[test]
fn topic_context_search_snippet_maps_unicode_expansion_to_original_characters() {
    let (store, _, _, proposal) = fixture();
    let pending = store
        .propose_topic(&proposal, WorkspaceKind::Directory, None)
        .unwrap();
    let topic = store.approve_topic(&pending.id).unwrap();
    let session = topic.topic_session_id.unwrap();
    store
        .append_message(&session, "user", &format!("{} needle", "İ".repeat(100)))
        .unwrap();
    let found = store
        .topic_search_conversations(&topic.id, "NEEDLE")
        .unwrap();
    assert_eq!(found.as_array().unwrap().len(), 1);
    let snippet = found[0]["snippet"].as_str().unwrap();
    assert!(snippet.ends_with(" needle"));
    assert!(snippet.starts_with('İ'));
    assert!(snippet.chars().count() <= 240);
    let inside_expansion = store
        .topic_search_conversations(&topic.id, "\u{307}")
        .unwrap();
    assert!(inside_expansion[0]["snippet"]
        .as_str()
        .unwrap()
        .starts_with('İ'));
}

#[test]
fn topic_needs_user_feature_does_not_block_active_sibling() {
    let (store, _, message, mut request) = fixture();
    request.features = vec![
        TopicFeatureProposal {
            source_message_id: message.id.clone(),
            request: "X".into(),
        },
        TopicFeatureProposal {
            source_message_id: message.id,
            request: "Y".into(),
        },
    ];
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    store.approve_topic(&topic.id).unwrap();
    store.claim_topic_workflow(&topic.id).unwrap();
    let token = "capability".repeat(8);
    let attempts: Vec<_> = topic
        .features
        .iter()
        .map(|feature| {
            store
                .begin_topic_attempt(
                    &topic.id,
                    &feature.id,
                    "Step",
                    StepAccess::Write,
                    HarnessKind::Claude,
                    None,
                    None,
                    &[],
                    &token,
                    Path::new("/topic/build"),
                )
                .unwrap()
        })
        .collect();
    store
        .report_topic_result(
            &attempts[0].id,
            &token,
            TopicAttemptStatus::NeedsUser,
            "Question",
            &[],
        )
        .unwrap();
    assert!(store
        .authenticate_topic_attempt(&attempts[1].id, &token)
        .is_ok());
    assert!(store
        .report_topic_result(
            &attempts[1].id,
            &token,
            TopicAttemptStatus::Succeeded,
            "Y done",
            &[]
        )
        .is_ok());
}

#[test]
fn topic_results_accept_sha1_and_sha256_revisions() {
    for (revision, valid) in [
        ("a".repeat(40), true),
        ("b".repeat(64), true),
        ("g".repeat(64), false),
        ("a".repeat(39), false),
        ("b".repeat(65), false),
    ] {
        let (store, _, _, request) = fixture();
        let topic = store
            .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
            .unwrap();
        store.approve_topic(&topic.id).unwrap();
        store.claim_topic_workflow(&topic.id).unwrap();
        let token = "capability".repeat(8);
        let attempt = store
            .begin_topic_attempt(
                &topic.id,
                &topic.features[0].id,
                "Step",
                StepAccess::Write,
                HarnessKind::Claude,
                None,
                None,
                &[],
                &token,
                Path::new("/topic/build"),
            )
            .unwrap();
        assert_eq!(
            store
                .report_topic_result(
                    &attempt.id,
                    &token,
                    TopicAttemptStatus::Succeeded,
                    "Done",
                    &[TopicArtifact::GitRevision { revision }]
                )
                .is_ok(),
            valid
        );
    }
}

#[test]
fn topic_empty_features_fallback_approval_idempotence_and_rejection() {
    let (store, source, message, request) = fixture();
    let other = store
        .append_message(&source.id, "user", "Stay here")
        .unwrap();
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    assert_eq!(topic.features.len(), 1);
    assert_eq!(topic.features[0].request, message.content);
    assert!(store.claim_topic_workflow(&topic.id).is_err());
    let approved = store.approve_topic(&topic.id).unwrap();
    let session = approved.topic_session_id.clone().unwrap();
    assert_eq!(
        store.approve_topic(&topic.id).unwrap().topic_session_id,
        Some(session.clone())
    );
    assert!(store
        .get_message(&source.id, &message.id)
        .unwrap()
        .is_none());
    assert!(store.get_message(&source.id, &other.id).unwrap().is_some());
    assert_eq!(
        store
            .get_message(&session, &message.id)
            .unwrap()
            .unwrap()
            .content,
        message.content
    );
    let count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1",
            [&session],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let redirects: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1 AND role='system'",
            [&source.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(redirects, 1);
    let mut rejected_request = request;
    rejected_request.message_ids = vec![other.id];
    let rejected = store
        .propose_topic(&rejected_request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    assert_eq!(
        store.reject_topic(&rejected.id, "No").unwrap().status,
        TopicStatus::Rejected
    );
    assert!(store.approve_topic(&rejected.id).is_err());
    assert!(store.claim_topic_workflow(&rejected.id).is_err());
}

#[test]
fn topic_xy_moves_original_once_and_features_progress_independently() {
    let (store, source, message, mut request) = fixture();
    request.features = ["X", "Y"]
        .into_iter()
        .map(|text| TopicFeatureProposal {
            source_message_id: message.id.clone(),
            request: text.into(),
        })
        .collect();
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    assert_eq!(topic.features.len(), 2);
    let approved = store.approve_topic(&topic.id).unwrap();
    let session = approved.topic_session_id.unwrap();
    assert!(store
        .get_message(&source.id, &message.id)
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .get_message(&session, &message.id)
            .unwrap()
            .unwrap()
            .content,
        "Build X and Y"
    );
    let count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1",
            [&session],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    store.claim_topic_workflow(&topic.id).unwrap();
    let token = "secret".repeat(8);
    let step = store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            "Implement X",
            StepAccess::Write,
            HarnessKind::Claude,
            Some("configured-model"),
            None,
            &[],
            &token,
            Path::new("/topic/build"),
        )
        .unwrap();
    assert!(store.authenticate_topic_attempt(&step.id, "wrong").is_err());
    assert!(store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            "Duplicate",
            StepAccess::ReadOnly,
            HarnessKind::Codex,
            None,
            None,
            &[],
            &token,
            Path::new("/topic/build")
        )
        .is_err());
    let outputs = vec![TopicArtifact::Text {
        title: "X".into(),
        content: "done".into(),
    }];
    let result = store
        .report_topic_result(
            &step.id,
            &token,
            TopicAttemptStatus::Succeeded,
            "Finished X",
            &outputs,
        )
        .unwrap();
    assert_eq!(result.outputs, outputs);
    assert_eq!(result.summary.as_deref(), Some("Finished X"));
    assert_eq!(result.model.as_deref(), Some("configured-model"));
    assert!(store.authenticate_topic_attempt(&step.id, &token).is_err());
    store
        .conn
        .execute(
            "UPDATE topic_features SET status='ready',outputs=?2 WHERE id=?1",
            params![
                topic.features[0].id,
                serde_json::to_string(&outputs).unwrap()
            ],
        )
        .unwrap();
    let state = store.get_topic(&topic.id).unwrap().unwrap();
    assert_eq!(
        state.features[0].status,
        formless_protocol::TopicFeatureStatus::Ready
    );
    assert_eq!(state.features[0].outputs, outputs);
    assert_eq!(state.features[0].attempt_count, 1);
    assert_eq!(
        state.features[1].status,
        formless_protocol::TopicFeatureStatus::Pending
    );
    assert_eq!(state.features[1].attempt_count, 0);
    store
        .conn
        .execute(
            "UPDATE topic_attempts SET access='corrupt' WHERE id=?1",
            [&step.id],
        )
        .unwrap();
    assert!(store.get_topic_attempt(&step.id).is_err());
}

#[test]
fn topic_invalid_feature_sources_blank_duplicates_and_empty_selection_are_rejected() {
    let (store, _, message, mut request) = fixture();
    for (source, text) in [("unknown", "X"), (message.id.as_str(), " ")] {
        request.features = vec![TopicFeatureProposal {
            source_message_id: source.into(),
            request: text.into(),
        }];
        assert!(store
            .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
            .is_err());
    }
    request.features = vec![
        TopicFeatureProposal {
            source_message_id: message.id,
            request: "X".into()
        };
        2
    ];
    assert!(store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .is_err());
    request.message_ids.clear();
    assert!(store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .is_err());
}

#[test]
fn topic_approval_rejects_edited_or_busy_source() {
    for state in ["edited", "running", "paused", "interrupted", "queued"] {
        let (store, source, message, request) = fixture();
        let topic = store
            .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
            .unwrap();
        match state {
            "edited" => {
                store
                    .conn
                    .execute(
                        "UPDATE messages SET content='changed' WHERE id=?1",
                        [&message.id],
                    )
                    .unwrap();
            }
            "running" | "paused" | "interrupted" => {
                let run = store.create_run(&source.id).unwrap();
                store.set_run_status(&run, state).unwrap();
            }
            "queued" => store.enqueue_message(&source.id, "later").unwrap(),
            _ => unreachable!(),
        }
        assert!(store.approve_topic(&topic.id).is_err(), "{state}");
        assert_eq!(
            store.get_topic(&topic.id).unwrap().unwrap().status,
            TopicStatus::PendingApproval
        );
        assert!(store
            .get_message(&source.id, &message.id)
            .unwrap()
            .is_some());
    }
}

#[test]
fn topic_retry_failed_and_corrupt_status_fail_closed() {
    let (store, _, _, request) = fixture();
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    store.approve_topic(&topic.id).unwrap();
    store
        .conn
        .execute("UPDATE topics SET status='failed' WHERE id=?1", [&topic.id])
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE topic_features SET status='failed' WHERE topic_id=?1",
            [&topic.id],
        )
        .unwrap();
    assert_eq!(
        store
            .retry_topic_feature(&topic.id, &topic.features[0].id)
            .unwrap()
            .status,
        TopicStatus::Approved
    );
    store
        .conn
        .execute(
            "UPDATE topic_features SET status='corrupt' WHERE topic_id=?1",
            [&topic.id],
        )
        .unwrap();
    assert!(store.get_topic(&topic.id).is_err());
}

#[test]
fn topic_approval_ignores_historical_paused_runs_and_new_runs_retire_resume_targets() {
    let (store, source, _, request) = fixture();
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    let old = store.create_run(&source.id).unwrap();
    store.set_run_status(&old, "paused").unwrap();
    store
        .set_run_error(&old, "provider failure retained")
        .unwrap();
    let latest = store.create_run(&source.id).unwrap();
    assert_eq!(
        store.run_status(&old).unwrap().as_deref(),
        Some("abandoned")
    );
    assert_eq!(
        store.run_error(&old).unwrap().as_deref(),
        Some("provider failure retained")
    );
    store.set_run_status(&latest, "abandoned").unwrap();
    store.set_run_status(&old, "paused").unwrap();
    assert!(store.unfinished_last_run(&source.id).unwrap().is_none());
    store.approve_topic(&topic.id).unwrap();
    assert_eq!(store.abandon_resumable_runs(&source.id).unwrap(), 1);
    assert_eq!(
        store.run_status(&old).unwrap().as_deref(),
        Some("abandoned")
    );
}

#[test]
fn topic_source_can_be_deleted_or_purged_after_rejection() {
    for purge in [false, true] {
        let (store, source, _, request) = fixture();
        let topic = store
            .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
            .unwrap();
        store.reject_topic(&topic.id, "not now").unwrap();
        if purge {
            store.soft_delete_session(&source.id).unwrap();
            assert!(store
                .sessions_deleted_before("9999")
                .unwrap()
                .contains(&source.id));
        }
        assert!(store.delete_session(&source.id).unwrap());
        assert!(store.get_session(&source.id).unwrap().is_none());
        assert!(store.get_topic(&topic.id).unwrap().is_none());
    }
}

#[test]
fn topic_session_deletion_cascades_active_attempts_and_repository_lock() {
    let (store, source, _, request) = fixture();
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    store.approve_topic(&topic.id).unwrap();
    store.claim_topic_workflow(&topic.id).unwrap();
    let attempt = store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            "Implement",
            StepAccess::Write,
            HarnessKind::Claude,
            None,
            None,
            &[],
            &"token".repeat(8),
            Path::new("/topic/build"),
        )
        .unwrap();
    assert!(store.delete_session(&source.id).unwrap());
    let surviving = store.get_topic(&topic.id).unwrap().unwrap();
    assert!(store.get_topic_attempt(&attempt.id).unwrap().is_some());
    assert!(store
        .delete_session(surviving.topic_session_id.as_ref().unwrap())
        .unwrap());
    assert!(store.get_topic(&topic.id).unwrap().is_none());
    assert!(store.get_topic_attempt(&attempt.id).unwrap().is_none());
    let locks: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM topic_repo_locks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(locks, 0);
}

#[test]
fn topic_main_chat_is_recreated_only_after_purge_not_during_undo_window() {
    let store = Store::open_in_memory().unwrap();
    let original = store.main_chat().unwrap();
    store.soft_delete_session(&original.id).unwrap();
    assert!(store
        .main_chat()
        .unwrap_err()
        .to_string()
        .contains("pending deletion"));
    store.restore_session(&original.id).unwrap();
    assert_eq!(store.main_chat().unwrap().id, original.id);
    store.soft_delete_session(&original.id).unwrap();
    store.delete_session(&original.id).unwrap();
    let replacement = store.main_chat().unwrap();
    assert_ne!(replacement.id, original.id);
    assert_eq!(store.main_chat().unwrap().id, replacement.id);
    assert!(store.is_main_chat(&replacement.id).unwrap());
    assert!(!store.is_main_chat(&original.id).unwrap());
}

#[test]
fn topic_cancel_and_retry_matrix_preserves_messages_and_never_starts_work() {
    for status in [
        "approved",
        "running",
        "needs_user",
        "pending_approval",
        "rejected",
    ] {
        let (store, source, message, request) = fixture();
        let topic = store
            .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
            .unwrap();
        if status != "pending_approval" && status != "rejected" {
            store.approve_topic(&topic.id).unwrap();
        }
        if status == "rejected" {
            store.reject_topic(&topic.id, "No").unwrap();
        }
        let mut active = None;
        if status == "running" || status == "needs_user" {
            store.claim_topic_workflow(&topic.id).unwrap();
            active = Some(
                store
                    .begin_topic_attempt(
                        &topic.id,
                        &topic.features[0].id,
                        "Step",
                        StepAccess::Write,
                        HarnessKind::Pi,
                        None,
                        None,
                        &[],
                        &"token".repeat(8),
                        Path::new("/topic/build"),
                    )
                    .unwrap(),
            );
            if status == "needs_user" {
                store
                    .conn
                    .execute(
                        "UPDATE topics SET status='needs_user' WHERE id=?1",
                        [&topic.id],
                    )
                    .unwrap();
            }
        }
        let result = store.cancel_topic(&topic.id);
        if matches!(status, "pending_approval" | "rejected") {
            assert!(result.is_err());
            assert!(store
                .get_message(&source.id, &message.id)
                .unwrap()
                .is_some());
            continue;
        }
        let cancelled = result.unwrap();
        assert_eq!(cancelled.status, TopicStatus::Cancelled);
        assert_eq!(
            cancelled.features[0].status,
            formless_protocol::TopicFeatureStatus::Cancelled
        );
        assert_eq!(
            store.cancel_topic(&topic.id).unwrap().status,
            TopicStatus::Cancelled
        );
        assert!(store.claim_topic_workflow(&topic.id).is_err());
        assert!(store
            .begin_topic_attempt(
                &topic.id,
                &topic.features[0].id,
                "Late",
                StepAccess::Write,
                HarnessKind::Claude,
                None,
                None,
                &[],
                &"token".repeat(8),
                Path::new("/topic/build")
            )
            .is_err());
        if let Some(active) = active {
            assert_eq!(
                store.get_topic_attempt(&active.id).unwrap().unwrap().status,
                TopicAttemptStatus::Cancelled
            );
        }
        let retried = store
            .retry_topic_feature(&topic.id, &topic.features[0].id)
            .unwrap();
        assert_eq!(retried.status, TopicStatus::Approved);
        assert_eq!(
            retried.features[0].status,
            formless_protocol::TopicFeatureStatus::Pending
        );
        assert_eq!(retried.attempts.len(), cancelled.attempts.len());
        assert!(store
            .get_message(&source.id, &message.id)
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .list_messages(retried.topic_session_id.as_deref().unwrap())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .conn
                .query_row("SELECT COUNT(*) FROM topic_repo_locks", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    for status in ["failed", "complete", "needs_user", "cancelled"] {
        let (store, _, _, request) = fixture();
        let topic = store
            .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
            .unwrap();
        store.approve_topic(&topic.id).unwrap();
        store
            .conn
            .execute(
                "UPDATE topics SET status=?2 WHERE id=?1",
                params![topic.id, status],
            )
            .unwrap();
        assert_eq!(
            store
                .retry_topic_feature(&topic.id, &topic.features[0].id)
                .unwrap()
                .status,
            TopicStatus::Approved
        );
    }
}

#[test]
fn topic_running_claim_and_retry_are_rejected_without_mutation() {
    let (store, _, _, request) = fixture();
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    store.approve_topic(&topic.id).unwrap();
    let running = store.claim_topic_workflow(&topic.id).unwrap();
    assert!(store.claim_topic_workflow(&topic.id).is_err());
    assert!(store
        .retry_topic_feature(&topic.id, &topic.features[0].id)
        .is_err());
    assert_eq!(store.get_topic(&topic.id).unwrap().unwrap(), running);
}

#[test]
fn topic_outline_empty_valid_twenty_valid_blank_and_too_many_invalid() {
    use formless_protocol::TopicStepOutline;
    let (store, _, _, mut request) = fixture();
    assert!(store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .is_ok());
    let step = TopicStepOutline {
        title: "Step".into(),
        instructions: String::new(),
        harness: Some(HarnessKind::OpenCode),
        acp_agent: None,
        model: None,
        access: StepAccess::ReadOnly,
    };
    request.outline = vec![step.clone(); 20];
    assert_eq!(
        store
            .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
            .unwrap()
            .outline
            .len(),
        20
    );
    request.outline.push(step);
    assert!(store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .is_err());
    request.outline.truncate(1);
    request.outline[0].title = "  ".into();
    assert!(store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .is_err());
}

#[test]
fn topic_result_restrictions_empty_outputs_and_attempt_limit() {
    let (store, _, _, request) = fixture();
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    store.approve_topic(&topic.id).unwrap();
    store.claim_topic_workflow(&topic.id).unwrap();
    let token = "token".repeat(8);
    for count in 1..=12 {
        let attempt = store
            .begin_topic_attempt(
                &topic.id,
                &topic.features[0].id,
                "Step",
                StepAccess::ReadOnly,
                HarnessKind::Antigravity,
                None,
                None,
                &[],
                &token,
                Path::new("/topic/build"),
            )
            .unwrap();
        assert!(store
            .report_topic_result(
                &attempt.id,
                "wrong",
                TopicAttemptStatus::Succeeded,
                "done",
                &[]
            )
            .is_err());
        assert!(store
            .report_topic_result(
                &attempt.id,
                &token,
                TopicAttemptStatus::Running,
                "done",
                &[]
            )
            .is_err());
        let result = store
            .report_topic_result(&attempt.id, &token, TopicAttemptStatus::Succeeded, "", &[])
            .unwrap();
        assert!(result.outputs.is_empty());
        assert_eq!(
            store.get_topic(&topic.id).unwrap().unwrap().features[0].attempt_count,
            count
        );
    }
    assert!(store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            "Thirteenth",
            StepAccess::Write,
            HarnessKind::Claude,
            None,
            None,
            &[],
            &token,
            Path::new("/topic/build")
        )
        .is_err());
}

fn nested_topic(store: &Store, source: &str, parent: Option<String>) -> Result<TopicInfo> {
    let message = store.append_message(source, "user", "Nested work")?;
    store.propose_topic(
        &TopicProposalRequest {
            parent_topic_id: parent,
            source_session_id: source.into(),
            message_ids: vec![message.id],
            features: vec![],
            name: "Nested".into(),
            workspace: Some("/topic/repo".into()),
            outline: vec![],
        },
        WorkspaceKind::Git,
        Some(&"a".repeat(40)),
    )
}

#[test]
fn topic_nested_hierarchy_defaults_explicit_parent_and_cascades() {
    for purge in [false, true] {
        let store = Store::open_in_memory().unwrap();
        let main = store.main_chat().unwrap();
        let root = nested_topic(&store, &main.id, None).unwrap();
        assert!(root.parent_topic_id.is_none());
        assert!(serde_json::to_value(&root)
            .unwrap()
            .get("parent_topic_id")
            .is_none());
        assert!(nested_topic(&store, &main.id, Some(root.id.clone())).is_err());
        let root = store.approve_topic(&root.id).unwrap();
        let root_session = root.topic_session_id.clone().unwrap();
        let child = nested_topic(&store, &root_session, None).unwrap();
        assert_eq!(child.parent_topic_id.as_deref(), Some(root.id.as_str()));
        let child = store.approve_topic(&child.id).unwrap();
        let child_session = child.topic_session_id.clone().unwrap();
        let grandchild = nested_topic(&store, &child_session, None).unwrap();
        assert_eq!(
            grandchild.parent_topic_id.as_deref(),
            Some(child.id.as_str())
        );
        let grandchild = store.approve_topic(&grandchild.id).unwrap();
        let grandchild_session = grandchild.topic_session_id.clone().unwrap();
        let pending = nested_topic(&store, &grandchild_session, None).unwrap();
        assert_eq!(
            pending.parent_topic_id.as_deref(),
            Some(grandchild.id.as_str())
        );
        let explicit = nested_topic(&store, &main.id, Some(root.id.clone())).unwrap();
        let explicit = store.approve_topic(&explicit.id).unwrap();
        let explicit_session = explicit.topic_session_id.clone().unwrap();
        let parent: String = store
            .conn
            .query_row(
                "SELECT parent_session_id FROM sessions WHERE id=?1",
                [&explicit_session],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(parent, root_session);
        assert_eq!(store.list_topics().unwrap().len(), 5);
        store.cancel_topic(&root.id).unwrap();
        assert_eq!(
            store.get_topic(&child.id).unwrap().unwrap().status,
            TopicStatus::Approved
        );
        assert_eq!(
            store.get_topic(&grandchild.id).unwrap().unwrap().status,
            TopicStatus::Approved
        );
        if purge {
            store.soft_delete_session(&root_session).unwrap();
            assert!(store
                .sessions_deleted_before("9999")
                .unwrap()
                .contains(&root_session));
        }
        store.delete_session(&root_session).unwrap();
        assert!(store.list_topics().unwrap().is_empty());
        for id in [
            root_session,
            child_session,
            grandchild_session,
            explicit_session,
        ] {
            assert!(store.get_session(&id).unwrap().is_none());
        }
        assert!(store.get_session(&main.id).unwrap().is_some());
    }
}

#[test]
fn topic_parent_validation_and_corrupt_cycles_fail_closed() {
    let (store, source, _, _) = fixture();
    assert!(nested_topic(&store, &source.id, Some("missing".into())).is_err());
    let rejected = nested_topic(&store, &source.id, None).unwrap();
    store.reject_topic(&rejected.id, "No").unwrap();
    assert!(nested_topic(&store, &source.id, Some(rejected.id)).is_err());
    let a = nested_topic(&store, &source.id, None).unwrap();
    let a = store.approve_topic(&a.id).unwrap();
    let b = nested_topic(&store, a.topic_session_id.as_deref().unwrap(), None).unwrap();
    let b = store.approve_topic(&b.id).unwrap();
    store
        .conn
        .execute(
            "UPDATE topics SET parent_topic_id=?2 WHERE id=?1",
            params![a.id, b.id],
        )
        .unwrap();
    assert!(
        nested_topic(&store, b.topic_session_id.as_deref().unwrap(), None)
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
    assert!(nested_topic(&store, &source.id, Some(b.id))
        .unwrap_err()
        .to_string()
        .contains("cycle"));
}

#[test]
fn topic_acp_selector_is_required_iff_acp_and_persisted() {
    use formless_protocol::TopicStepOutline;
    let (store, _, _, mut request) = fixture();
    request.outline = vec![TopicStepOutline {
        title: "Agent".into(),
        instructions: String::new(),
        harness: Some(HarnessKind::Acp),
        acp_agent: None,
        model: None,
        access: StepAccess::Write,
    }];
    for (kind, agent) in [
        (Some(HarnessKind::Acp), None),
        (Some(HarnessKind::Acp), Some(" ")),
        (Some(HarnessKind::Muse), Some("agent")),
        (None, Some("agent")),
    ] {
        request.outline[0].harness = kind;
        request.outline[0].acp_agent = agent.map(str::to_owned);
        assert!(store
            .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
            .is_err());
    }
    request.outline[0].harness = Some(HarnessKind::Acp);
    request.outline[0].acp_agent = Some("custom agent/id".into());
    let topic = store
        .propose_topic(&request, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    store.approve_topic(&topic.id).unwrap();
    store.claim_topic_workflow(&topic.id).unwrap();
    let attempt = store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            "ACP",
            StepAccess::Write,
            HarnessKind::Acp,
            None,
            Some("custom agent/id"),
            &[],
            &"token".repeat(8),
            Path::new("/topic/build"),
        )
        .unwrap();
    assert_eq!(attempt.acp_agent.as_deref(), Some("custom agent/id"));
}

#[test]
fn topic_approved_explicit_parent_survives_source_purge_but_pending_does_not() {
    let (store, source, message, mut proposal) = fixture();
    let owner = store.create_session(None).unwrap();
    let parent = nested_topic(&store, &owner.id, None).unwrap();
    let parent = store.approve_topic(&parent.id).unwrap();
    let artifact = store
        .save_artifact(Some(&source.id), "/retained.txt", "", 0)
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE messages SET meta=?2 WHERE id=?1",
            params![
                message.id,
                json!({"attachments":[{"id":artifact}]}).to_string()
            ],
        )
        .unwrap();
    proposal.parent_topic_id = Some(parent.id.clone());
    let topic = store
        .propose_topic(&proposal, WorkspaceKind::Git, Some(&"a".repeat(40)))
        .unwrap();
    let topic = store.approve_topic(&topic.id).unwrap();
    let pending = nested_topic(&store, &source.id, Some(parent.id.clone())).unwrap();
    store.delete_session(&source.id).unwrap();
    let surviving = store.get_topic(&topic.id).unwrap().unwrap();
    assert_eq!(surviving.parent_topic_id, Some(parent.id));
    assert_eq!(surviving.source_session_id, source.id);
    assert!(store.get_topic(&pending.id).unwrap().is_none());
    let session = topic.topic_session_id.unwrap();
    assert!(store.get_message(&session, &message.id).unwrap().is_some());
    assert!(store
        .topic_artifact_access(&artifact, &session, None, Some(&source.id))
        .unwrap());
    assert!(store.get_artifact(&artifact).unwrap().is_some());
}
