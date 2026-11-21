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
    for state in ["edited", "running", "paused", "queued"] {
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
fn topic_source_deletion_cascades_active_attempts_and_repository_lock() {
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
