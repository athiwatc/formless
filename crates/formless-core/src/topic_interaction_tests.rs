use super::*;
use crate::local_harness::{HarnessPermissionRequest, HarnessUpdate, InteractionHandler};
use std::time::Duration;

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

#[test]
fn topic_directory_reports_are_private_and_reject_files_and_escaping_links() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let outputs = f
        .fixture
        .paths
        .data_dir
        .join("topic-attempts")
        .join(&f.attempt.id)
        .join("outputs");
    let outside = f.fixture.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outputs.join("file"), "not a directory").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, outputs.join("escape")).unwrap();
    for path in [outside.to_string_lossy().into_owned(), "file".into()] {
        assert!(f
            .daemon()
            .with_store(|st| st.report_topic_result(
                &f.attempt.id,
                &f.token,
                TopicAttemptStatus::Succeeded,
                "invalid",
                &[TopicArtifact::Directory { path }]
            ))
            .is_err());
    }
    #[cfg(unix)]
    assert!(f
        .daemon()
        .with_store(|st| st.report_topic_result(
            &f.attempt.id,
            &f.token,
            TopicAttemptStatus::Succeeded,
            "invalid",
            &[TopicArtifact::Directory {
                path: "escape".into()
            }]
        ))
        .is_err());
    std::fs::create_dir(outputs.join("empty")).unwrap();
    let result = f
        .daemon()
        .with_store(|st| {
            st.report_topic_result(
                &f.attempt.id,
                &f.token,
                TopicAttemptStatus::Succeeded,
                "valid",
                &[TopicArtifact::Directory {
                    path: "empty".into(),
                }],
            )
        })
        .unwrap();
    assert_eq!(
        result.outputs,
        vec![TopicArtifact::Directory {
            path: outputs
                .join("empty")
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        }]
    );
}

#[tokio::test]
async fn topic_terminal_report_survives_a_fixture_adapter_late_update_error() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let handler = f.handler();
    let output = TopicArtifact::Text {
        title: "Result".into(),
        content: "finished".into(),
    };
    let execution: Result<(crate::local_harness::HarnessResult, Vec<TopicArtifact>)> = async {
        f.daemon()
            .handle(Request::TopicMcp {
                request: TopicMcpRequest {
                    attempt_id: f.attempt.id.clone(),
                    capability_token: f.token.clone(),
                    operation: TopicMcpOperation::ReportResult {
                        status: TopicAttemptStatus::Succeeded,
                        summary: "reported success".into(),
                        outputs: vec![output.clone()],
                    },
                },
            })
            .await?;
        handler.on_update(HarnessUpdate::Message {
            content: "late streamed update".into(),
        })?;
        Ok((
            crate::local_harness::HarnessResult {
                session_id: None,
                message: "late success".into(),
                events: vec![],
            },
            vec![],
        ))
    }
    .await;
    assert!(execution
        .as_ref()
        .unwrap_err()
        .to_string()
        .contains("no longer active"));
    f.daemon()
        .finish_topic_attempt_execution(
            &f.topic.id,
            &f.attempt.id,
            handler.finish().and(execution),
            false,
        )
        .unwrap();
    let saved = f.state();
    assert_eq!(saved.attempts[0].status, TopicAttemptStatus::Succeeded);
    assert_eq!(
        saved.attempts[0].summary.as_deref(),
        Some("reported success")
    );
    assert_eq!(saved.attempts[0].outputs, vec![output.clone()]);
    assert!(saved.features[0].active_attempt_id.is_none());
    assert_eq!(saved.features[0].outputs, vec![output]);
    let session = f.topic.topic_session_id.as_ref().unwrap();
    f.daemon()
        .finish_topic_feature_tool(
            session,
            &json!({"feature_id":f.attempt.feature_id,"summary":"Completed successfully"}),
            true,
        )
        .unwrap();
    let saved = f.state();
    assert_eq!(saved.features[0].status, TopicFeatureStatus::Ready);
    assert_eq!(saved.status, TopicStatus::Complete);
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
async fn topic_permission_rules_apply_audit_reject_precedence_and_user_only_fallback() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let add = |pattern: &str, decision| Request::AddTopicPermissionRule {
        topic_id: f.topic.id.clone(),
        harness: Some(HarnessKind::Claude),
        acp_agent: None,
        kind: None,
        pattern: pattern.into(),
        decision,
    };
    assert!(f
        .daemon()
        .handle(add("", TopicRuleDecision::AllowOnce))
        .await
        .is_err());
    let allowed = f
        .daemon()
        .handle(add("Read *", TopicRuleDecision::AllowOnce))
        .await
        .unwrap();
    let allow = allowed["permission_rules"][0]["rule_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let handler = f.handler();
    assert_eq!(
        handler
            .request_permission(permission(false), &CancellationToken::new())
            .await
            .as_deref(),
        Some("allow")
    );
    let close=next_event(&mut events,|event|matches!(event,Event::HarnessPermissionClosed{by:Some(by),..} if by.starts_with("rule:"))).await;
    assert!(
        matches!(close,Event::HarnessPermissionClosed{by:Some(by),..}if by==format!("rule:{allow}"))
    );
    assert!(f.daemon().topic_notifications.locked().is_empty());
    let rejected = f
        .daemon()
        .handle(add("*rm *", TopicRuleDecision::Reject))
        .await
        .unwrap();
    let reject = rejected["permission_rules"][1]["rule_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut command = permission(false);
    command.raw_input = Some(json!({"command":"do rm file"}));
    assert_eq!(
        handler
            .request_permission(command, &CancellationToken::new())
            .await
            .as_deref(),
        Some("reject")
    );
    let messages = f
        .daemon()
        .with_reader(|st| st.list_messages(&f.attempt.conversation_session_id))
        .unwrap();
    assert!(messages
        .iter()
        .any(|message| message.content.contains(&format!("rule:{reject}"))));
    for trust in [false, true] {
        if trust {
            f.daemon()
                .handle(add("*", TopicRuleDecision::AllowOnce))
                .await
                .unwrap();
            f.daemon()
                .handle(add("*", TopicRuleDecision::Reject))
                .await
                .unwrap();
        }
        let mut request = permission(false);
        if trust {
            request.kind = "trust_workspace".into();
        } else {
            request.title = "Different".into();
        }
        let handler = f.handler();
        let waiting = tokio::spawn(async move {
            handler
                .request_permission(request, &CancellationToken::new())
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
        // Earlier automatic decisions are also audited as requested events.
        let permission_id = if f
            .daemon()
            .harness_permissions
            .locked()
            .contains_key(&permission_id)
        {
            permission_id
        } else {
            loop {
                let Event::HarnessPermissionRequested { permission_id, .. } =
                    next_event(&mut events, |event| {
                        matches!(event, Event::HarnessPermissionRequested { .. })
                    })
                    .await
                else {
                    unreachable!()
                };
                if f.daemon()
                    .harness_permissions
                    .locked()
                    .contains_key(&permission_id)
                {
                    break permission_id;
                }
            }
        };
        assert!(!waiting.is_finished());
        f.daemon()
            .handle(Request::HarnessPermissionDecision {
                permission_id,
                option_id: Some("reject".into()),
            })
            .await
            .unwrap();
        assert_eq!(waiting.await.unwrap().as_deref(), Some("reject"));
    }
    assert_eq!(f.daemon().topic_notifications.locked().len(), 2);
    f.daemon()
        .handle(Request::RemoveTopicPermissionRule {
            topic_id: f.topic.id.clone(),
            rule_id: reject.clone(),
        })
        .await
        .unwrap();
    assert!(f
        .daemon()
        .handle(Request::RemoveTopicPermissionRule {
            topic_id: f.topic.id.clone(),
            rule_id: reject
        })
        .await
        .is_err());
}

#[tokio::test]
async fn topic_permission_rules_persist_across_restart_and_remove() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = TopicFixture::new(&env);
    let saved = f
        .daemon()
        .handle(Request::AddTopicPermissionRule {
            topic_id: f.topic.id.clone(),
            harness: None,
            acp_agent: None,
            kind: Some("read".into()),
            pattern: "*".into(),
            decision: TopicRuleDecision::AllowOnce,
        })
        .await
        .unwrap();
    drop(f.fixture.daemon.take());
    f.fixture.daemon = Some(Arc::new(Daemon::new(f.fixture.paths.clone()).unwrap()));
    assert_eq!(
        serde_json::to_value(f.state().permission_rules).unwrap(),
        saved["permission_rules"]
    );
    let removed = f
        .daemon()
        .handle(Request::RemoveTopicPermissionRule {
            topic_id: f.topic.id.clone(),
            rule_id: saved["permission_rules"][0]["rule_id"]
                .as_str()
                .unwrap()
                .into(),
        })
        .await
        .unwrap();
    assert_eq!(removed["permission_rules"], json!([]));
    assert!(f
        .daemon()
        .handle(Request::RemoveTopicPermissionRule {
            topic_id: "missing".into(),
            rule_id: "missing".into()
        })
        .await
        .is_err());
}

#[tokio::test]
async fn topic_reject_rule_without_reject_option_bypasses_cached_allow_and_notifies() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let handler = f.handler();
    let first = handler.clone();
    let waiting = tokio::spawn(async move {
        first
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
    assert_eq!(waiting.await.unwrap().as_deref(), Some("allow"));
    f.daemon()
        .handle(Request::AddTopicPermissionRule {
            topic_id: f.topic.id.clone(),
            harness: None,
            acp_agent: None,
            kind: None,
            pattern: "*".into(),
            decision: TopicRuleDecision::Reject,
        })
        .await
        .unwrap();
    let mut request = permission(true);
    request.options.retain(|option| option.option_id == "allow");
    let waiting = tokio::spawn(async move {
        handler
            .request_permission(request, &CancellationToken::new())
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
    assert!(!waiting.is_finished());
    assert_eq!(f.daemon().topic_notifications.locked().len(), 2);
    f.daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id,
            option_id: None,
        })
        .await
        .unwrap();
    assert!(waiting.await.unwrap().is_none());
}

#[test]
fn topic_permission_globs_are_full_string_and_unicode_safe() {
    use super::super::topic_interactions::permission_glob;
    for (pattern, value, expected) in [
        ("*", "", true),
        ("a?", "a🦀", true),
        ("Read *", "read file", false),
        ("*rm *", "do rm file", true),
        ("file", "prefix file", false),
        ("a*b*c", "a xx b yy c", true),
        ("a*b", "a xx c", false),
        ("rm *", "rm *.txt", true),
        ("rm *.*", "rm *.txt", true),
        ("*", "* literal * and ?", true),
        ("a*b", "a*literal b", true),
        ("?x", "?x", true),
        ("*?", "*", true),
        ("a?", "a??", false),
    ] {
        assert_eq!(permission_glob(pattern, value), expected);
    }
}

#[tokio::test]
async fn topic_away_notifications_are_redacted_and_test_injected() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_TOPIC_NOTIFICATIONS");
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let cancel = CancellationToken::new();
    let h = f.handler();
    let c = cancel.clone();
    let mut permission = permission(false);
    permission.title = "Read ledger Bearer abcdefghijklmnopqrstuvwxyz".into();
    permission.raw_input = Some(json!({"command":"raw-input-must-not-be-notified"}));
    let waiting = tokio::spawn(async move { h.request_permission(permission, &c).await });
    next_event(&mut events, |e| {
        matches!(e, Event::HarnessPermissionRequested { .. })
    })
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.daemon().topic_notifications.locked().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    {
        let notices = f.daemon().topic_notifications.locked();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].0.starts_with("Formless: "));
        assert!(notices[0].1.contains("Claude: Read ledger"));
        assert!(!notices[0].1.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(!notices[0].1.contains("raw-input"));
    }
    cancel.cancel();
    assert!(waiting.await.unwrap().is_none());
    f.daemon().broadcast(Event::UserQuestion {
        question_id: "user-only-notification".into(),
        session_id: f.attempt.conversation_session_id.clone(),
        topic_id: Some(f.topic.id.clone()),
        attempt_id: Some(f.attempt.id.clone()),
        user_only: true,
        question: "Choose a substitute".into(),
        questions: vec![],
        options: vec![],
    });
    assert_eq!(f.daemon().topic_notifications.locked().len(), 2);
    f.daemon().broadcast(Event::UserQuestionClosed {
        question_id: "user-only-notification".into(),
        answered_by: None,
    });
    env.set("FORMLESS_TOPIC_NOTIFICATIONS", "0");
    f.daemon()
        .notify_topic_attention(Some(&f.topic.id), Some(&f.attempt.id), "Disabled");
    assert_eq!(f.daemon().topic_notifications.locked().len(), 2);
}

#[tokio::test]
async fn topic_model_answer_requires_basis_posts_chat_and_preserves_first_answer() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let handler = f.handler();
    let waiting = tokio::spawn(async move {
        handler
            .ask_question(
                vec![Question::single(
                    "Use CSV?".into(),
                    vec!["Yes".into(), "No".into()],
                )],
                &CancellationToken::new(),
            )
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let session = f.topic.topic_session_id.as_ref().unwrap();
    let mut args = json!({"question_id":question_id,"answers":[{"selected":["Yes"]}],"basis":" "});
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &args)
        .unwrap_err()
        .to_string()
        .contains("basis"));
    args["basis"] = json!("User request: use CSV for the ledger");
    assert_eq!(
        f.daemon()
            .answer_topic_question_tool(session, &args)
            .unwrap()["answered"],
        true
    );
    assert_eq!(waiting.await.unwrap().unwrap()[0].selected, vec!["Yes"]);
    next_event(
        &mut events,
        |event| matches!(event,Event::UserQuestionClosed{answered_by:Some(by),..} if by=="model"),
    )
    .await;
    let messages = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap();
    let audit = messages
        .iter()
        .filter(|message| message.content.contains("Basis: User request: use CSV"))
        .collect::<Vec<_>>();
    assert_eq!(audit.len(), 1);
    assert!(audit[0].content.contains(&f.attempt.id));
    assert!(audit[0].content.contains("Use CSV?"));
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id,
                answer: "No".into(),
                answers: vec![]
            })
            .await
            .unwrap()["answered"],
        false
    );
}

#[tokio::test]
async fn topic_model_answer_rejects_user_only_and_empty_answers() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let handler = f.handler();
    let waiting = tokio::spawn(async move {
        handler
            .ask_question(
                vec![Question::single("Choose".into(), vec![])],
                &CancellationToken::new(),
            )
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let session = f.topic.topic_session_id.as_ref().unwrap();
    assert!(f
        .daemon()
        .answer_topic_question_tool(
            session,
            &json!({"question_id":question_id,"answers":[],"basis":"User said yes"})
        )
        .is_err());
    for event in f.daemon().pending_attention.locked().iter_mut() {
        if let Event::UserQuestion {
            question_id: id,
            user_only,
            ..
        } = event
        {
            if id == &question_id {
                *user_only = true;
            }
        }
    }
    assert!(f
        .daemon()
        .answer_topic_question_tool(
            session,
            &json!({"question_id":question_id,"answers":[{"text":"yes"}],"basis":"User said yes"})
        )
        .unwrap_err()
        .to_string()
        .contains("user_only"));
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id,
                answer: "yes".into(),
                answers: vec![]
            })
            .await
            .unwrap()["answered"],
        true
    );
    assert_eq!(
        waiting.await.unwrap().unwrap()[0].text.as_deref(),
        Some("yes")
    );
    next_event(
        &mut events,
        |event| matches!(event,Event::UserQuestionClosed{answered_by:Some(by),..} if by=="user"),
    )
    .await;
}

#[tokio::test]
async fn topic_shutdown_closes_pending_permission_and_question() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let handler = f.handler();
    let waiting = tokio::spawn(async move {
        handler
            .request_permission(permission(false), &CancellationToken::new())
            .await
    });
    next_event(&mut events, |event| {
        matches!(event, Event::HarnessPermissionRequested { .. })
    })
    .await;
    f.daemon().request_shutdown();
    assert_eq!(waiting.await.unwrap(), None);
    next_event(&mut events, |event| {
        matches!(
            event,
            Event::HarnessPermissionClosed {
                option_id: None,
                ..
            }
        )
    })
    .await;
    let f = TopicFixture::new(&env);
    let mut events = f.events();
    let handler = f.handler();
    let waiting = tokio::spawn(async move {
        handler
            .ask_question(
                vec![Question::single("Proceed?".into(), vec![])],
                &CancellationToken::new(),
            )
            .await
    });
    next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await;
    f.daemon().request_shutdown();
    assert_eq!(waiting.await.unwrap(), None);
    next_event(&mut events, |event| {
        matches!(
            event,
            Event::UserQuestionClosed {
                answered_by: None,
                ..
            }
        )
    })
    .await;
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
        .with_store(|st| {
            st.finish_topic_step(
                &attempts[0].id,
                TopicAttemptStatus::NeedsUser,
                "Need help with X",
                &[],
                None,
            )
        })
        .unwrap();
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
        3
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
    f.daemon()
        .with_store(|st| {
            st.finish_topic_step(
                &f.attempt.id,
                TopicAttemptStatus::Succeeded,
                "Done",
                &[],
                None,
            )
        })
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
