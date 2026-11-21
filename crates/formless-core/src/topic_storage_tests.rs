use super::*;
use formless_protocol::{HarnessRole, TopicFeatureProposal, TopicProposalRequest, TopicStatus};

fn fixture() -> (Store, SessionInfo, MessageInfo, TopicProposalRequest) {
    let store = Store::open_in_memory().unwrap();
    let source = store.create_session(None).unwrap();
    let message = store
        .append_message(&source.id, "user", "Build X and Y")
        .unwrap();
    let request = TopicProposalRequest {
        source_session_id: source.id.clone(),
        message_ids: vec![message.id.clone()],
        features: vec![],
        name: "Work".into(),
        repo: PathBuf::from("/topic/repo"),
        builder: HarnessKind::Claude,
        builder_model: None,
        reviewer: HarnessKind::Codex,
        reviewer_model: None,
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
fn topic_empty_features_fallback_approval_idempotence_and_rejection() {
    let (store, source, message, request) = fixture();
    let other = store
        .append_message(&source.id, "user", "Stay here")
        .unwrap();
    let topic = store.propose_topic(&request, &"a".repeat(40)).unwrap();
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
        .propose_topic(&rejected_request, &"a".repeat(40))
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
    let topic = store.propose_topic(&request, &"a".repeat(40)).unwrap();
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
    let build = store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            HarnessRole::Build,
            HarnessKind::Claude,
            1,
            &token,
            Path::new("/topic/build"),
            &"a".repeat(40),
        )
        .unwrap();
    assert!(store
        .authenticate_topic_attempt(&build.id, "wrong")
        .is_err());
    let revision = "b".repeat(40);
    store
        .ready_for_topic_review(&build.id, &token, &revision, "tests")
        .unwrap();
    let review = store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            HarnessRole::Review,
            HarnessKind::Codex,
            1,
            &token,
            Path::new("/topic/review"),
            &revision,
        )
        .unwrap();
    assert!(store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            HarnessRole::Review,
            HarnessKind::Codex,
            1,
            &token,
            Path::new("/topic/review2"),
            &revision
        )
        .is_err());
    store
        .submit_topic_review(
            &review.id,
            &token,
            &revision,
            &formless_protocol::TopicReviewOutcome::Passed,
        )
        .unwrap();
    let state = store.get_topic(&topic.id).unwrap().unwrap();
    assert_eq!(
        state.features[0].status,
        formless_protocol::TopicFeatureStatus::Ready
    );
    assert_eq!(
        state.features[1].status,
        formless_protocol::TopicFeatureStatus::Pending
    );
    store
        .conn
        .execute(
            "UPDATE topic_attempts SET role='corrupt' WHERE id=?1",
            [&review.id],
        )
        .unwrap();
    assert!(store.get_topic_attempt(&review.id).is_err());
}

#[test]
fn topic_invalid_feature_sources_blank_duplicates_and_empty_selection_are_rejected() {
    let (store, _, message, mut request) = fixture();
    for (source, text) in [("unknown", "X"), (message.id.as_str(), " ")] {
        request.features = vec![TopicFeatureProposal {
            source_message_id: source.into(),
            request: text.into(),
        }];
        assert!(store.propose_topic(&request, &"a".repeat(40)).is_err());
    }
    request.features = vec![
        TopicFeatureProposal {
            source_message_id: message.id,
            request: "X".into()
        };
        2
    ];
    assert!(store.propose_topic(&request, &"a".repeat(40)).is_err());
    request.message_ids.clear();
    assert!(store.propose_topic(&request, &"a".repeat(40)).is_err());
}

#[test]
fn topic_approval_rejects_edited_or_busy_source() {
    for state in ["edited", "running", "paused", "queued"] {
        let (store, source, message, request) = fixture();
        let topic = store.propose_topic(&request, &"a".repeat(40)).unwrap();
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
            "running" | "paused" => {
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
    let topic = store.propose_topic(&request, &"a".repeat(40)).unwrap();
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
fn topic_source_can_be_deleted_or_purged_after_rejection() {
    for purge in [false, true] {
        let (store, source, _, request) = fixture();
        let topic = store.propose_topic(&request, &"a".repeat(40)).unwrap();
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
fn topic_source_deletion_cascades_active_attempts_and_repository_lock() {
    let (store, source, _, request) = fixture();
    let topic = store.propose_topic(&request, &"a".repeat(40)).unwrap();
    store.approve_topic(&topic.id).unwrap();
    store.claim_topic_workflow(&topic.id).unwrap();
    let attempt = store
        .begin_topic_attempt(
            &topic.id,
            &topic.features[0].id,
            HarnessRole::Build,
            HarnessKind::Claude,
            1,
            &"token".repeat(8),
            Path::new("/topic/build"),
            &"a".repeat(40),
        )
        .unwrap();
    assert!(store.delete_session(&source.id).unwrap());
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
