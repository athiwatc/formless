use super::*;
use crate::local_harness::{HarnessPermissionRequest, HarnessUpdate, InteractionHandler};

struct TopicFixture {
    fixture: Fixture,
    topic: TopicInfo,
    attempt: TopicAttemptInfo,
    token: String,
}

impl TopicFixture {
    fn new(env: &crate::credential_env::EnvGuard) -> Self {
        let fixture = Fixture::new(env);
        let proposal = topic_fixture_proposal(&fixture, None);
        let pending = fixture.daemon().propose_topic_request(&proposal).unwrap();
        let topic = fixture
            .daemon()
            .with_store(|st| st.approve_topic(&pending.id))
            .unwrap();
        fixture
            .daemon()
            .with_store(|st| st.claim_topic_workflow(&topic.id))
            .unwrap();
        let token = "fixture capability".repeat(3);
        let attempt = fixture
            .daemon()
            .with_store(|st| {
                st.begin_topic_attempt(
                    &topic.id,
                    &topic.features[0].id,
                    "Interactive step",
                    StepAccess::Write,
                    HarnessKind::Claude,
                    None,
                    None,
                    &[],
                    &token,
                    topic.workspace.as_ref().unwrap(),
                )
            })
            .unwrap();
        Self {
            fixture,
            topic,
            attempt,
            token,
        }
    }
    fn daemon(&self) -> &Arc<Daemon> {
        self.fixture.daemon()
    }
    fn handler(&self) -> Arc<dyn InteractionHandler> {
        self.daemon()
            .topic_interaction_handler(&self.attempt.id, &self.token)
            .unwrap()
    }
    fn state(&self) -> TopicInfo {
        self.daemon()
            .with_reader(|st| st.get_topic(&self.topic.id))
            .unwrap()
            .unwrap()
    }
    fn events(&self) -> mpsc::Receiver<ServerMessage> {
        let (tx, rx) = mpsc::channel(256);
        self.daemon().subs.locked().push(Subscriber {
            conn: 99,
            tx: tx.into(),
            filter: Arc::new(StdMutex::new(EventFilter::default())),
            kick: CancellationToken::new(),
            gaps: BTreeMap::new(),
        });
        rx
    }
}

async fn next_event(
    events: &mut mpsc::Receiver<ServerMessage>,
    accept: impl Fn(&Event) -> bool,
) -> Event {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let ServerMessage::Event(event) = events.recv().await.expect("event channel closed")
            {
                if accept(&event) {
                    return event;
                }
            }
        }
    })
    .await
    .expect("expected event")
}

fn permission(always: bool) -> HarnessPermissionRequest {
    HarnessPermissionRequest {
        title: "Read invoices".into(),
        kind: "read".into(),
        raw_input: Some(json!({"path":"invoices"})),
        options: vec![
            HarnessPermissionOption {
                option_id: "allow".into(),
                name: "Allow".into(),
                kind: if always {
                    HarnessPermissionOptionKind::AllowAlways
                } else {
                    HarnessPermissionOptionKind::AllowOnce
                },
            },
            HarnessPermissionOption {
                option_id: "reject".into(),
                name: "Reject".into(),
                kind: HarnessPermissionOptionKind::RejectOnce,
            },
        ],
    }
}

#[tokio::test]
async fn topic_permission_fixture_sibling_needs_user_does_not_block_answer() {
    let env = crate::credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env);
    let mut proposal = topic_fixture_proposal(&fixture, None);
    proposal.features = vec![
        TopicFeatureProposal {
            source_message_id: proposal.message_ids[0].clone(),
            request: "X".into(),
        },
        TopicFeatureProposal {
            source_message_id: proposal.message_ids[0].clone(),
            request: "Y".into(),
        },
    ];
    let topic = fixture.daemon().propose_topic_request(&proposal).unwrap();
    let topic = fixture
        .daemon()
        .with_store(|st| st.approve_topic(&topic.id))
        .unwrap();
    fixture
        .daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let token = "fixture token".repeat(4);
    let attempts: Vec<_> = topic
        .features
        .iter()
        .map(|feature| {
            fixture
                .daemon()
                .with_store(|st| {
                    st.begin_topic_attempt(
                        &topic.id,
                        &feature.id,
                        "Step",
                        StepAccess::Write,
                        HarnessKind::Claude,
                        None,
                        None,
                        &[],
                        &token,
                        topic.workspace.as_ref().unwrap(),
                    )
                })
                .unwrap()
        })
        .collect();
    let f = TopicFixture {
        fixture,
        topic,
        attempt: attempts[1].clone(),
        token: token.clone(),
    };
    let mut events = f.events();
    let handler = f.handler();
    let waiter = tokio::spawn(async move {
        handler
            .request_permission(permission(false), &CancellationToken::new())
            .await
    });
    let Event::HarnessPermissionRequested { permission_id, .. } =
        next_event(&mut events, |event| {
            matches!(event, Event::HarnessPermissionRequested { .. })
        })
        .await
    else {
        unreachable!()
    };
    f.daemon()
        .handle(Request::TopicMcp {
            request: TopicMcpRequest {
                attempt_id: attempts[0].id.clone(),
                capability_token: token,
                operation: TopicMcpOperation::ReportResult {
                    status: TopicAttemptStatus::NeedsUser,
                    summary: "Need help with X".into(),
                    outputs: vec![],
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(f.state().status, TopicStatus::NeedsUser);
    f.daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id,
            option_id: Some("allow".into()),
        })
        .await
        .unwrap();
    assert_eq!(waiter.await.unwrap().as_deref(), Some("allow"));
    assert_eq!(f.state().status, TopicStatus::Running);
    assert_eq!(
        f.daemon()
            .with_reader(|st| st.get_topic_attempt(&attempts[1].id))
            .unwrap()
            .unwrap()
            .status,
        TopicAttemptStatus::Running
    );
}

#[tokio::test]
async fn topic_permission_fixture_round_trip_replay_wrong_option_and_live_updates() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let handler = f.handler();
    for update in [
        HarnessUpdate::Message {
            content: "Working".into(),
        },
        HarnessUpdate::Thought {
            content: "Checking".into(),
        },
        HarnessUpdate::ToolCall {
            name: "reader".into(),
            input: json!({"file":"invoices"}),
        },
        HarnessUpdate::Plan {
            content: "1. Scan".into(),
        },
    ] {
        handler.on_update(update).unwrap();
    }
    assert_eq!(
        f.daemon()
            .with_reader(|st| st.list_messages(&f.attempt.conversation_session_id))
            .unwrap()
            .len(),
        4
    );
    let waiter = tokio::spawn(async move {
        handler
            .request_permission(permission(false), &CancellationToken::new())
            .await
    });
    let event = next_event(&mut events, |event| {
        matches!(event, Event::HarnessPermissionRequested { .. })
    })
    .await;
    let Event::HarnessPermissionRequested {
        permission_id,
        session_id,
        topic_id,
        attempt_id,
        ..
    } = event
    else {
        unreachable!()
    };
    assert_eq!(session_id, f.attempt.conversation_session_id);
    assert_eq!(topic_id, f.topic.id);
    assert_eq!(attempt_id, f.attempt.id);
    let pending = f.daemon().handle(Request::PendingApprovals).await.unwrap();
    assert!(pending
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["permission_id"] == permission_id));
    assert_eq!(f.state().attempts[0].status, TopicAttemptStatus::NeedsUser);
    assert_eq!(f.state().features[0].status, TopicFeatureStatus::NeedsUser);
    assert!(f
        .daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id: permission_id.clone(),
            option_id: Some("wrong".into())
        })
        .await
        .is_err());
    assert_eq!(f.state().attempts[0].status, TopicAttemptStatus::NeedsUser);
    f.daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id: permission_id.clone(),
            option_id: Some("allow".into()),
        })
        .await
        .unwrap();
    assert_eq!(waiter.await.unwrap().as_deref(), Some("allow"));
    assert_eq!(f.state().attempts[0].status, TopicAttemptStatus::Running);
    assert_eq!(f.state().features[0].status, TopicFeatureStatus::Running);
    let closed = next_event(&mut events, |event| {
        matches!(event, Event::HarnessPermissionClosed { .. })
    })
    .await;
    assert!(
        matches!(closed,Event::HarnessPermissionClosed{option_id:Some(option),..} if option=="allow")
    );
    assert!(f
        .daemon()
        .handle(Request::PendingApprovals)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    assert!(f
        .daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id,
            option_id: None
        })
        .await
        .is_err());
}

#[tokio::test]
async fn topic_permission_fixture_cancel_and_attempt_end_close_as_rejected() {
    let env = crate::credential_env::EnvGuard::isolated();
    for end in [false, true] {
        let f = TopicFixture::new(&env);
        let mut events = f.events();
        let handler = f.handler();
        let waiter = tokio::spawn(async move {
            handler
                .request_permission(permission(false), &CancellationToken::new())
                .await
        });
        next_event(&mut events, |event| {
            matches!(event, Event::HarnessPermissionRequested { .. })
        })
        .await;
        if end {
            f.daemon()
                .handle(Request::TopicMcp {
                    request: TopicMcpRequest {
                        attempt_id: f.attempt.id.clone(),
                        capability_token: f.token.clone(),
                        operation: TopicMcpOperation::ReportResult {
                            status: TopicAttemptStatus::Failed,
                            summary: "Harness exited".into(),
                            outputs: vec![],
                        },
                    },
                })
                .await
                .unwrap();
        } else {
            f.daemon()
                .handle(Request::CancelTopic {
                    topic_id: f.topic.id.clone(),
                })
                .await
                .unwrap();
        }
        assert!(waiter.await.unwrap().is_none());
        assert!(matches!(
            next_event(&mut events, |event| matches!(
                event,
                Event::HarnessPermissionClosed { .. }
            ))
            .await,
            Event::HarnessPermissionClosed {
                option_id: None,
                ..
            }
        ));
        assert_eq!(
            f.state().attempts[0].status,
            if end {
                TopicAttemptStatus::Failed
            } else {
                TopicAttemptStatus::Cancelled
            }
        );
        assert!(f
            .daemon()
            .handle(Request::PendingApprovals)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn topic_permission_fixture_always_is_shared_only_within_one_attempt() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let handler = f.handler();
    let waiter = tokio::spawn(async move {
        handler
            .request_permission(permission(true), &CancellationToken::new())
            .await
    });
    let Event::HarnessPermissionRequested { permission_id, .. } =
        next_event(&mut events, |event| {
            matches!(event, Event::HarnessPermissionRequested { .. })
        })
        .await
    else {
        unreachable!()
    };
    f.daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id,
            option_id: Some("allow".into()),
        })
        .await
        .unwrap();
    assert_eq!(waiter.await.unwrap().as_deref(), Some("allow"));
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            f.handler()
                .request_permission(permission(true), &CancellationToken::new())
        )
        .await
        .unwrap()
        .as_deref(),
        Some("allow")
    );
    assert!(f
        .daemon()
        .handle(Request::PendingApprovals)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    f.daemon()
        .handle(Request::TopicMcp {
            request: TopicMcpRequest {
                attempt_id: f.attempt.id.clone(),
                capability_token: f.token.clone(),
                operation: TopicMcpOperation::ReportResult {
                    status: TopicAttemptStatus::Succeeded,
                    summary: "Done".into(),
                    outputs: vec![],
                },
            },
        })
        .await
        .unwrap();
    let next = f
        .daemon()
        .with_store(|st| {
            st.begin_topic_attempt(
                &f.topic.id,
                &f.topic.features[0].id,
                "Next",
                StepAccess::Write,
                HarnessKind::Claude,
                None,
                None,
                &[],
                &f.token,
                f.topic.workspace.as_ref().unwrap(),
            )
        })
        .unwrap();
    let handler = f
        .daemon()
        .topic_interaction_handler(&next.id, &f.token)
        .unwrap();
    let waiter = tokio::spawn(async move {
        handler
            .request_permission(permission(true), &CancellationToken::new())
            .await
    });
    let Event::HarnessPermissionRequested { permission_id, .. } =
        next_event(&mut events, |event| {
            matches!(event, Event::HarnessPermissionRequested { .. })
        })
        .await
    else {
        unreachable!()
    };
    f.daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id,
            option_id: None,
        })
        .await
        .unwrap();
    assert!(waiter.await.unwrap().is_none());
}

#[tokio::test]
async fn topic_question_fixture_mcp_structured_answers_first_wins_and_cancel() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let mut questions = vec![
        Question::single("Pick files".into(), vec!["A".into(), "B".into()]),
        Question::single("Explain".into(), vec![]),
    ];
    questions[0].multi_select = true;
    for count in [0, 5] {
        let invalid = TopicMcpRequest {
            attempt_id: f.attempt.id.clone(),
            capability_token: f.token.clone(),
            operation: TopicMcpOperation::AskUser {
                questions: vec![questions[0].clone(); count],
            },
        };
        assert!(f
            .daemon()
            .handle(Request::TopicMcp { request: invalid })
            .await
            .is_err());
    }
    let daemon = f.daemon().clone();
    let request = TopicMcpRequest {
        attempt_id: f.attempt.id.clone(),
        capability_token: f.token.clone(),
        operation: TopicMcpOperation::AskUser {
            questions: questions.clone(),
        },
    };
    let waiter = tokio::spawn(async move { daemon.handle(Request::TopicMcp { request }).await });
    let Event::UserQuestion {
        question_id,
        session_id,
        questions: actual,
        topic_id,
        attempt_id,
        ..
    } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    assert_eq!(actual, questions);
    assert_eq!(session_id, f.attempt.conversation_session_id);
    assert_eq!(topic_id.as_deref(), Some(f.topic.id.as_str()));
    assert_eq!(attempt_id.as_deref(), Some(f.attempt.id.as_str()));
    assert_eq!(f.state().attempts[0].status, TopicAttemptStatus::NeedsUser);
    let answers = vec![
        QuestionAnswer {
            selected: vec!["A".into(), "B".into()],
            text: None,
        },
        QuestionAnswer {
            selected: vec![],
            text: Some("Use both".into()),
        },
    ];
    let reply = f
        .daemon()
        .handle(Request::AnswerQuestion {
            question_id: question_id.clone(),
            answer: String::new(),
            answers: answers.clone(),
        })
        .await
        .unwrap();
    assert_eq!(reply["answered"], true);
    assert_eq!(
        waiter.await.unwrap().unwrap(),
        serde_json::to_value(&answers).unwrap()
    );
    assert_eq!(f.state().attempts[0].status, TopicAttemptStatus::Running);
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id,
                answer: "late".into(),
                answers: vec![]
            })
            .await
            .unwrap()["answered"],
        false
    );
    let daemon = f.daemon().clone();
    let request = TopicMcpRequest {
        attempt_id: f.attempt.id.clone(),
        capability_token: f.token.clone(),
        operation: TopicMcpOperation::AskUser { questions },
    };
    let waiter = tokio::spawn(async move { daemon.handle(Request::TopicMcp { request }).await });
    next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await;
    f.daemon()
        .handle(Request::CancelTopic {
            topic_id: f.topic.id.clone(),
        })
        .await
        .unwrap();
    assert!(waiter.await.unwrap().unwrap().is_null());
    assert_eq!(f.state().attempts[0].status, TopicAttemptStatus::Cancelled);
}
