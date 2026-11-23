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
        Self::new_with_requests(env, &["User request: use CSV"])
    }
    fn new_with_request(env: &crate::credential_env::EnvGuard, request: &str) -> Self {
        Self::new_with_requests(env, &[request])
    }
    fn new_with_requests(env: &crate::credential_env::EnvGuard, requests: &[&str]) -> Self {
        let fixture = Fixture::new(env);
        let mut proposal = topic_fixture_proposal(&fixture, None);
        let sources = fixture
            .daemon()
            .with_store(|st| {
                requests
                    .iter()
                    .map(|request| st.append_message(&proposal.source_session_id, "user", request))
                    .collect::<Result<Vec<_>>>()
            })
            .unwrap();
        proposal.message_ids = sources.iter().map(|source| source.id.clone()).collect();
        proposal.features = sources
            .iter()
            .zip(requests)
            .rev()
            .map(|(source, request)| TopicFeatureProposal {
                source_message_id: source.id.clone(),
                request: (*request).into(),
            })
            .collect();
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

async fn assert_model_request_rejected_but_user_can_answer(
    request: &str,
    questions: Vec<Question>,
    answers: Vec<QuestionAnswer>,
) {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, request);
    let mut events = f.events();
    let handler = f.handler();
    let requested = questions.clone();
    let waiting = tokio::spawn(async move {
        handler
            .ask_question(requested, &CancellationToken::new())
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
    let args = json!({
        "question_id": question_id,
        "answers": answers.clone(),
        "basis": "A nonempty model assertion",
        "evidence": {
            "message_id": f.topic.features[0].source_message_id.clone(),
            "quote": request,
        },
    });
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &args)
        .is_err());
    let messages = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap();
    assert!(!messages
        .iter()
        .any(|message| message.content.contains("Orchestrator answer")));
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id,
                answer: String::new(),
                answers: answers.clone(),
            })
            .await
            .unwrap()["answered"],
        true
    );
    assert_eq!(waiting.await.unwrap().unwrap(), answers);
}

#[tokio::test]
async fn topic_model_answer_rejects_ambiguous_and_mismatched_request_forms() {
    let basic = || {
        vec![Question::single(
            "Ledger format?".into(),
            vec!["CSV".into(), "JSON".into()],
        )]
    };
    let selected_csv = || {
        vec![QuestionAnswer {
            selected: vec!["CSV".into()],
            text: None,
        }]
    };
    for request in [
        "Report format: CSV",
        "Ledger format: not CSV",
        "Ledger format: CSV or JSON",
        "If Ledger format is needed: CSV",
        "Please use Ledger format: CSV",
    ] {
        assert_model_request_rejected_but_user_can_answer(request, basic(), selected_csv()).await;
    }
    assert_model_request_rejected_but_user_can_answer(
        "If needed, Ledger format: CSV",
        vec![Question::single(
            "If needed, Ledger format?".into(),
            vec!["CSV".into(), "JSON".into()],
        )],
        selected_csv(),
    )
    .await;
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format and backup: CSV",
        vec![Question::single(
            "Ledger format and backup?".into(),
            vec!["CSV".into(), "JSON".into()],
        )],
        selected_csv(),
    )
    .await;
    let mut described = basic().remove(0);
    described.options[0].description = Some("CSV plus unapproved retention settings".into());
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV",
        vec![described],
        selected_csv(),
    )
    .await;
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV.",
        vec![Question::single(
            "Ledger format?".into(),
            vec!["CSV".into(), "CSV.".into()],
        )],
        selected_csv(),
    )
    .await;
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV?",
        vec![Question::single(
            "Ledger format?".into(),
            vec!["CSV?".into(), "JSON".into()],
        )],
        vec![QuestionAnswer {
            selected: vec!["CSV?".into()],
            text: None,
        }],
    )
    .await;
    assert_model_request_rejected_but_user_can_answer(
        "Don't use Ledger format: CSV",
        vec![Question::single(
            "Don't use Ledger format?".into(),
            vec!["CSV".into()],
        )],
        vec![QuestionAnswer {
            selected: vec!["CSV".into()],
            text: None,
        }],
    )
    .await;
    for uncertainty in ["Maybe", "Perhaps", "Might", "Could", "Possibly", "Probably"] {
        let stem = format!("{uncertainty} Ledger format");
        assert_model_request_rejected_but_user_can_answer(
            &format!("{stem}: CSV"),
            vec![Question::single(format!("{stem}?"), vec!["CSV".into()])],
            selected_csv(),
        )
        .await;
    }
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV",
        basic(),
        vec![QuestionAnswer {
            selected: vec!["JSON".into()],
            text: None,
        }],
    )
    .await;
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV",
        basic(),
        vec![QuestionAnswer {
            selected: vec![],
            text: Some("CSV".into()),
        }],
    )
    .await;
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV",
        vec![
            basic().remove(0),
            Question::single("Report format?".into(), vec!["CSV".into()]),
        ],
        vec![selected_csv()[0].clone(), selected_csv()[0].clone()],
    )
    .await;
    let mut multi_select = basic().remove(0);
    multi_select.multi_select = true;
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV",
        vec![multi_select],
        selected_csv(),
    )
    .await;
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV",
        vec![Question::single(
            "Ledger format?".into(),
            vec!["CSV".into(), "CSV".into()],
        )],
        selected_csv(),
    )
    .await;
    assert_model_request_rejected_but_user_can_answer(
        "Ledger format: CSV",
        vec![Question::single(
            "Ledger format?".into(),
            vec![String::new()],
        )],
        vec![QuestionAnswer {
            selected: vec![],
            text: Some("CSV".into()),
        }],
    )
    .await;
}

#[tokio::test]
async fn topic_model_answer_rejects_changed_and_out_of_scope_sources() {
    for case in [
        "conflict",
        "newer",
        "edit",
        "delete",
        "generated",
        "forged",
        "sibling-feature",
        "sibling-topic",
        "assistant",
        "missing-approval",
        "source-chat",
    ] {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = if case == "conflict" {
            TopicFixture::new_with_requests(&env, &["Ledger format: JSON", "Ledger format: CSV"])
        } else if case == "sibling-feature" {
            TopicFixture::new_with_requests(&env, &["Ledger format: CSV", "Report format: CSV"])
        } else {
            TopicFixture::new_with_request(&env, "Ledger format: CSV")
        };
        if case == "source-chat" {
            let source_session = f.topic.source_session_id.clone();
            f.daemon()
                .with_store(|st| {
                    st.set_session_kind_for_test(&source_session, "chat")?;
                    st.append_message(&source_session, "user", "Unrelated new source-chat input")?;
                    Ok(())
                })
                .unwrap();
        }
        if case == "missing-approval" {
            let session = f.topic.topic_session_id.as_ref().unwrap().clone();
            let approval_note = f
                .daemon()
                .with_reader(|st| st.list_messages(&session))
                .unwrap()
                .into_iter()
                .find(|message| message.generated_by.as_deref() == Some("topic_grounding_context"))
                .unwrap();
            f.daemon()
                .with_store(|st| st.delete_topic_message_for_test(&session, &approval_note.id))
                .unwrap();
        }
        let sibling_topic_source = if case == "sibling-topic" {
            let source = f
                .daemon()
                .with_store(|st| {
                    st.append_message(&f.topic.source_session_id, "user", "Ledger format: JSON")
                })
                .unwrap();
            let proposal = TopicProposalRequest {
                parent_topic_id: None,
                source_session_id: f.topic.source_session_id.clone(),
                message_ids: vec![source.id.clone()],
                features: vec![TopicFeatureProposal {
                    source_message_id: source.id.clone(),
                    request: "Ledger format: JSON".into(),
                }],
                name: "Sibling topic".into(),
                workspace: None,
                outline: vec![],
            };
            let pending = f.daemon().propose_topic_request(&proposal).unwrap();
            f.daemon()
                .with_store(|st| st.approve_topic(&pending.id))
                .unwrap();
            Some(source.id)
        } else {
            None
        };
        let mut events = f.events();
        let handler = f.handler();
        let waiting = tokio::spawn(async move {
            handler
                .ask_question(
                    vec![Question::single(
                        "Ledger format?".into(),
                        vec!["CSV".into(), "JSON".into()],
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
        let current_source_id = f.topic.features[0].source_message_id.clone();
        let (evidence_id, quote) = match case {
            "newer" => {
                f.daemon()
                    .with_store(|st| st.append_message(session, "user", "Unrelated new input"))
                    .unwrap();
                (current_source_id.clone(), "Ledger format: CSV".to_owned())
            }
            "edit" => {
                let mut message = f
                    .daemon()
                    .with_reader(|st| st.get_message(session, &current_source_id))
                    .unwrap()
                    .unwrap();
                message.content = "Ledger format: JSON".into();
                f.daemon()
                    .with_store(|st| st.update_topic_message(&message))
                    .unwrap();
                (current_source_id.clone(), "Ledger format: CSV".to_owned())
            }
            "generated" => {
                f.daemon()
                    .with_store(|st| {
                        st.set_topic_message_generated_for_test(
                            session,
                            &current_source_id,
                            "topic_attempt_instructions",
                        )
                    })
                    .unwrap();
                (current_source_id.clone(), "Ledger format: CSV".to_owned())
            }
            "assistant" => {
                f.daemon()
                    .with_store(|st| {
                        st.set_topic_message_role_for_test(session, &current_source_id, "assistant")
                    })
                    .unwrap();
                (current_source_id.clone(), "Ledger format: CSV".to_owned())
            }
            "delete" => {
                f.daemon()
                    .with_store(|st| st.delete_topic_message_for_test(session, &current_source_id))
                    .unwrap();
                (current_source_id.clone(), "Ledger format: CSV".to_owned())
            }
            "forged" => ("missing-message-id".into(), "Ledger format: CSV".into()),
            "sibling-feature" => (
                f.topic.features[1].source_message_id.clone(),
                "Ledger format: CSV".into(),
            ),
            "sibling-topic" => (sibling_topic_source.unwrap(), "Ledger format: JSON".into()),
            _ => (current_source_id.clone(), "Ledger format: CSV".to_owned()),
        };
        let args = json!({
            "question_id": question_id,
            "answers": [{"selected":["CSV"]}],
            "basis": "The model asserts CSV",
            "evidence": {"message_id": evidence_id, "quote": quote},
        });
        assert!(
            f.daemon()
                .answer_topic_question_tool(session, &args)
                .is_err(),
            "case: {case}"
        );
        assert_eq!(
            f.daemon()
                .handle(Request::AnswerQuestion {
                    question_id,
                    answer: String::new(),
                    answers: vec![QuestionAnswer {
                        selected: vec!["CSV".into()],
                        text: None
                    }],
                })
                .await
                .unwrap()["answered"],
            true,
            "case: {case}"
        );
        assert_eq!(waiting.await.unwrap().unwrap()[0].selected, vec!["CSV"]);
    }
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
    let result = f
        .daemon()
        .accept_topic_harness_result(
            &f.attempt.id,
            handler.finish().and(execution).map(|(result, _)| result),
            false,
        )
        .unwrap();
    f.daemon()
        .finish_topic_attempt_execution(&f.topic.id, &f.attempt.id, Ok((result, vec![])), false)
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
async fn topic_model_answer_requires_grounded_evidence_and_preserves_first_answer() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, "Ledger format: CSV");
    let mut events = f.events();
    let handler = f.handler();
    let waiting = tokio::spawn(async move {
        handler
            .ask_question(
                vec![Question::single(
                    "Ledger format?".into(),
                    vec!["CSV".into(), "JSON".into()],
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
    let source_id = f.topic.features[0].source_message_id.clone();
    let mut args = json!({"question_id":question_id,"answers":[{"selected":["CSV"]}],"basis":"The model says CSV","evidence":{"message_id":source_id,"quote":"fabricated evidence"}});
    let mut basis_only_args = args.clone();
    basis_only_args.as_object_mut().unwrap().remove("evidence");
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &basis_only_args)
        .is_err());
    assert!(f.daemon().questions.locked().contains_key(&question_id));
    let basis_only_messages = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap();
    assert!(!basis_only_messages
        .iter()
        .any(|message| message.content.contains("Orchestrator answer")));
    assert!(!basis_only_messages
        .iter()
        .any(|message| message.generated_by.as_deref() == Some("topic_human_answer")));
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &args)
        .is_err());
    let messages_before = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap();
    assert!(!messages_before
        .iter()
        .any(|message| message.generated_by.as_deref() == Some("topic_human_answer")));
    assert!(!messages_before
        .iter()
        .any(|message| message.content.contains("Orchestrator answer")));
    args["evidence"]["quote"] = json!("Ledger format: CSV");
    assert_eq!(
        f.daemon()
            .answer_topic_question_tool(session, &args)
            .unwrap()["answered"],
        true
    );
    assert_eq!(waiting.await.unwrap().unwrap()[0].selected, vec!["CSV"]);
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
        .filter(|message| message.content.contains("Basis: The model says CSV"))
        .collect::<Vec<_>>();
    assert_eq!(audit.len(), 1);
    assert!(audit[0].content.contains(&f.attempt.id));
    assert!(audit[0].content.contains("Ledger format?"));
    assert!(audit[0]
        .content
        .contains("Evidence quote: Ledger format: CSV"));
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
async fn topic_model_answer_reuses_exact_same_attempt_human_answer_receipt() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, "Ledger format: CSV");
    let question = Question::single("Ledger format?".into(), vec!["CSV".into(), "JSON".into()]);
    let handler = f.handler();
    let first_question = question.clone();
    let first_waiting = tokio::spawn(async move {
        handler
            .ask_question(vec![first_question], &CancellationToken::new())
            .await
    });
    let mut events = f.events();
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id: question_id.clone(),
                answer: "CSV".into(),
                answers: vec![],
            })
            .await
            .unwrap()["answered"],
        true
    );
    let first_answer = first_waiting.await.unwrap().unwrap();
    assert!(first_answer[0].selected.is_empty());
    assert_eq!(first_answer[0].text.as_deref(), Some("CSV"));
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id: question_id.clone(),
                answer: "duplicate answer".into(),
                answers: vec![],
            })
            .await
            .unwrap()["answered"],
        false
    );
    let session = f.topic.topic_session_id.as_ref().unwrap();
    let human_receipts = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap()
        .into_iter()
        .filter(|message| message.generated_by.as_deref() == Some("topic_human_answer"))
        .collect::<Vec<_>>();
    assert_eq!(human_receipts.len(), 1);
    let receipt = human_receipts.into_iter().next().unwrap();
    let metadata: Value = serde_json::from_str(receipt.meta.as_deref().unwrap()).unwrap();
    assert_eq!(
        metadata["topic_human_answer"]["source_question_id"],
        question_id
    );
    assert_eq!(
        metadata["topic_human_answer"]["answers"][0]["selected"],
        json!([])
    );
    assert_eq!(
        metadata["topic_human_answer"]["answers"][0]["text"],
        json!("CSV")
    );

    let handler = f.handler();
    let repeated_question = question.clone();
    let second_waiting = tokio::spawn(async move {
        handler
            .ask_question(vec![repeated_question], &CancellationToken::new())
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let mut args = json!({
        "question_id": question_id,
        "answers": [{"selected":["JSON"]}],
        "basis": "A mismatched model assertion",
        "evidence": {"message_id": receipt.id, "quote": receipt.content},
    });
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &args)
        .is_err());
    args["answers"] = json!([{"text":"CSV"}]);
    assert_eq!(
        f.daemon()
            .answer_topic_question_tool(session, &args)
            .unwrap()["answered"],
        true
    );
    let repeated_answer = second_waiting.await.unwrap().unwrap();
    assert!(repeated_answer[0].selected.is_empty());
    assert_eq!(repeated_answer[0].text.as_deref(), Some("CSV"));

    let mut edited_receipt = receipt.clone();
    edited_receipt.content = "Edited human-answer receipt".into();
    f.daemon()
        .with_store(|st| st.update_topic_message(&edited_receipt))
        .unwrap();
    let handler = f.handler();
    let edited_question = question.clone();
    let edited_waiting = tokio::spawn(async move {
        handler
            .ask_question(vec![edited_question], &CancellationToken::new())
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let edited_evidence = json!({
        "question_id": question_id,
        "answers": [{"text":"CSV"}],
        "basis": "The original answer receipt was edited",
        "evidence": {"message_id": edited_receipt.id, "quote": edited_receipt.content},
    });
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &edited_evidence)
        .is_err());
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id: edited_evidence["question_id"].as_str().unwrap().into(),
                answer: String::new(),
                answers: vec![QuestionAnswer {
                    selected: vec!["CSV".into()],
                    text: None,
                }],
            })
            .await
            .unwrap()["answered"],
        true
    );
    assert_eq!(
        edited_waiting.await.unwrap().unwrap()[0].selected,
        vec!["CSV"]
    );
    let latest_receipt = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap()
        .into_iter()
        .rev()
        .find(|message| message.generated_by.as_deref() == Some("topic_human_answer"))
        .unwrap();

    f.daemon()
        .with_store(|st| st.append_message(session, "user", "Please reconsider the format"))
        .unwrap();
    let handler = f.handler();
    let final_question = question.clone();
    let final_waiting = tokio::spawn(async move {
        handler
            .ask_question(vec![final_question], &CancellationToken::new())
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let stale_evidence = json!({
        "question_id": question_id,
        "answers": [{"selected":["CSV"]}],
        "basis": "The old human answer",
        "evidence": {"message_id": latest_receipt.id, "quote": latest_receipt.content},
    });
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &stale_evidence)
        .is_err());
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id: stale_evidence["question_id"].as_str().unwrap().into(),
                answer: String::new(),
                answers: vec![QuestionAnswer {
                    selected: vec!["CSV".into()],
                    text: None,
                }],
            })
            .await
            .unwrap()["answered"],
        true
    );
    assert_eq!(
        final_waiting.await.unwrap().unwrap()[0].selected,
        vec!["CSV"]
    );
}

#[tokio::test]
async fn topic_model_answer_rejects_partial_human_answer_receipt() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, "Ledger format: CSV");
    let questions = vec![
        Question::single("Ledger format?".into(), vec!["CSV".into(), "JSON".into()]),
        Question::single(
            "Retention policy?".into(),
            vec!["Short".into(), "Long".into()],
        ),
    ];
    let handler = f.handler();
    let first_questions = questions.clone();
    let first_waiting = tokio::spawn(async move {
        handler
            .ask_question(first_questions, &CancellationToken::new())
            .await
    });
    let mut events = f.events();
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id,
                answer: "CSV".into(),
                answers: vec![QuestionAnswer {
                    selected: vec!["CSV".into()],
                    text: None,
                }],
            })
            .await
            .unwrap()["answered"],
        true
    );
    let partial_answer = first_waiting.await.unwrap().unwrap();
    assert_eq!(partial_answer.len(), 2);
    assert_eq!(partial_answer[0].selected, vec!["CSV"]);
    assert!(partial_answer[1].selected.is_empty());
    assert!(partial_answer[1].text.is_none());

    let session = f.topic.topic_session_id.as_ref().unwrap();
    let receipt = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap()
        .into_iter()
        .find(|message| message.generated_by.as_deref() == Some("topic_human_answer"))
        .unwrap();
    let handler = f.handler();
    let repeated_questions = questions.clone();
    let second_waiting = tokio::spawn(async move {
        handler
            .ask_question(repeated_questions, &CancellationToken::new())
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let evidence = json!({"message_id":receipt.id,"quote":receipt.content});
    let incomplete = json!({
        "question_id":question_id,
        "answers":[{"selected":["CSV"]}],
        "basis":"The first choice was answered",
        "evidence":evidence,
    });
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &incomplete)
        .is_err());
    let guessed = json!({
        "question_id":incomplete["question_id"],
        "answers":[{"selected":["CSV"]},{"selected":["Short"]}],
        "basis":"The missing choice was inferred",
        "evidence":incomplete["evidence"],
    });
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &guessed)
        .is_err());
    assert!(f
        .daemon()
        .questions
        .locked()
        .contains_key(guessed["question_id"].as_str().unwrap()));
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id: guessed["question_id"].as_str().unwrap().into(),
                answer: "CSV and short retention".into(),
                answers: vec![
                    QuestionAnswer {
                        selected: vec!["CSV".into()],
                        text: None,
                    },
                    QuestionAnswer {
                        selected: vec!["Short".into()],
                        text: None,
                    },
                ],
            })
            .await
            .unwrap()["answered"],
        true
    );
    let final_answers = second_waiting.await.unwrap().unwrap();
    assert_eq!(final_answers[0].selected, vec!["CSV"]);
    assert_eq!(final_answers[1].selected, vec!["Short"]);
}

#[tokio::test]
async fn topic_model_answer_rejects_receipts_for_changed_question_options() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, "Ledger format: CSV");
    let mut events = f.events();
    let question = Question::single("Ledger format?".into(), vec!["CSV".into(), "JSON".into()]);
    let handler = f.handler();
    let first_question = question.clone();
    let first_waiting = tokio::spawn(async move {
        handler
            .ask_question(vec![first_question], &CancellationToken::new())
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id,
                answer: String::new(),
                answers: vec![QuestionAnswer {
                    selected: vec![],
                    text: Some("CSV".into()),
                }],
            })
            .await
            .unwrap()["answered"],
        true
    );
    assert_eq!(
        first_waiting.await.unwrap().unwrap()[0].text.as_deref(),
        Some("CSV")
    );
    let session = f.topic.topic_session_id.as_ref().unwrap();
    let receipt = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap()
        .into_iter()
        .find(|message| message.generated_by.as_deref() == Some("topic_human_answer"))
        .unwrap();

    let handler = f.handler();
    let changed_question = Question::single(
        "Ledger format?".into(),
        vec!["CSV".into(), "JSON".into(), "XML".into()],
    );
    let second_waiting = tokio::spawn(async move {
        handler
            .ask_question(vec![changed_question], &CancellationToken::new())
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let args = json!({
        "question_id": question_id,
        "answers": [{"text":"CSV"}],
        "basis": "The older human answer",
        "evidence": {"message_id": receipt.id, "quote": receipt.content},
    });
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &args)
        .is_err());
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id: args["question_id"].as_str().unwrap().into(),
                answer: String::new(),
                answers: vec![QuestionAnswer {
                    selected: vec![],
                    text: Some("CSV".into()),
                }],
            })
            .await
            .unwrap()["answered"],
        true
    );
    assert_eq!(
        second_waiting.await.unwrap().unwrap()[0].text.as_deref(),
        Some("CSV")
    );
}

#[tokio::test]
async fn topic_human_answer_receipt_snapshot_is_atomic_and_source_chat_input_supersedes() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, "Ledger format: CSV");
    let source_session = f.topic.source_session_id.clone();
    f.daemon()
        .with_store(|st| st.set_session_kind_for_test(&source_session, "chat"))
        .unwrap();
    let question = Question::single("Ledger format?".into(), vec!["CSV".into(), "JSON".into()]);
    let handler = f.handler();
    let first_question = question.clone();
    let waiting = tokio::spawn(async move {
        handler
            .ask_question(vec![first_question], &CancellationToken::new())
            .await
    });
    let mut events = f.events();
    let event = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await;
    let Event::UserQuestion { question_id, .. } = &event else {
        unreachable!()
    };
    let context = f
        .daemon()
        .topic_human_answer_context(question_id, &event)
        .unwrap();
    let (snapshot_tx, snapshot_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let daemon = f.daemon().clone();
    let answer_id = question_id.clone();
    let answer = tokio::task::spawn_blocking(move || {
        daemon
            .answer_topic_human_question_with_hook(
                context,
                &answer_id,
                "",
                vec![QuestionAnswer {
                    selected: vec!["CSV".into()],
                    text: None,
                }],
                move |_| {
                    snapshot_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            )
            .unwrap()
    });
    snapshot_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(f.daemon().store.try_lock().is_err());
    let (attempting_tx, attempting_rx) = std::sync::mpsc::channel();
    let daemon = f.daemon().clone();
    let writer_session = source_session.clone();
    let writer = tokio::task::spawn_blocking(move || {
        attempting_tx.send(()).unwrap();
        daemon
            .with_store(|st| {
                st.append_message(&writer_session, "user", "Unrelated correction after answer")
            })
            .unwrap()
    });
    attempting_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release_tx.send(()).unwrap();
    let (delivered, receipt) = answer.await.unwrap();
    assert!(delivered);
    let receipt = receipt.unwrap();
    writer.await.unwrap();
    assert_eq!(waiting.await.unwrap().unwrap()[0].selected, vec!["CSV"]);

    let session = f.topic.topic_session_id.as_ref().unwrap();
    let handler = f.handler();
    let repeated = question.clone();
    let repeated_waiting = tokio::spawn(async move {
        handler
            .ask_question(vec![repeated], &CancellationToken::new())
            .await
    });
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let args = json!({
        "question_id": question_id,
        "answers": [{"selected":["CSV"]}],
        "basis": "Same human choice",
        "evidence": {"message_id": receipt.id, "quote": receipt.content},
    });
    assert!(f
        .daemon()
        .answer_topic_question_tool(session, &args)
        .is_err());
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id: args["question_id"].as_str().unwrap().into(),
                answer: "CSV".into(),
                answers: vec![],
            })
            .await
            .unwrap()["answered"],
        true
    );
    assert_eq!(
        repeated_waiting.await.unwrap().unwrap()[0].text.as_deref(),
        Some("CSV")
    );
}

#[tokio::test]
async fn topic_human_answer_closed_waiter_creates_no_receipt() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, "Ledger format: CSV");
    let cancel = CancellationToken::new();
    let waiter_cancel = cancel.clone();
    let handler = f.handler();
    let waiting = tokio::spawn(async move {
        handler
            .ask_question(
                vec![Question::single(
                    "Ledger format?".into(),
                    vec!["CSV".into()],
                )],
                &waiter_cancel,
            )
            .await
    });
    let mut events = f.events();
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    cancel.cancel();
    assert!(waiting.await.unwrap().is_none());
    assert_eq!(
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id,
                answer: "CSV".into(),
                answers: vec![],
            })
            .await
            .unwrap()["answered"],
        false
    );
    let session = f.topic.topic_session_id.as_ref().unwrap();
    assert!(!f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap()
        .iter()
        .any(|message| message.generated_by.as_deref() == Some("topic_human_answer")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn topic_human_answer_send_failure_rolls_back_receipt() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, "Ledger format: CSV");
    let waiter_cancel = CancellationToken::new();
    let question = Question::single("Ledger format?".into(), vec!["CSV".into()]);
    let handler = f.handler();
    let waiting =
        tokio::spawn(async move { handler.ask_question(vec![question], &waiter_cancel).await });
    let mut events = f.events();
    let event = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await;
    let Event::UserQuestion { question_id, .. } = &event else {
        unreachable!()
    };
    let context = f
        .daemon()
        .topic_human_answer_context(question_id, &event)
        .unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let daemon = f.daemon().clone();
    let answer_id = question_id.clone();
    let answer = tokio::task::spawn_blocking(move || {
        daemon.answer_topic_human_question_with_hook(
            context,
            &answer_id,
            "",
            vec![QuestionAnswer {
                selected: vec!["CSV".into()],
                text: None,
            }],
            move |sender| {
                ready_tx.send(()).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !sender.is_closed() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "question receiver stayed open"
                    );
                    std::thread::yield_now();
                }
            },
        )
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    waiting.abort();
    let (delivered, receipt) = answer.await.unwrap().unwrap();
    assert!(!delivered);
    assert!(receipt.is_none());
    assert!(waiting.await.unwrap_err().is_cancelled());
    let session = f.topic.topic_session_id.as_ref().unwrap();
    assert!(!f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap()
        .iter()
        .any(|message| message.generated_by.as_deref() == Some("topic_human_answer")));
}

#[tokio::test]
async fn topic_human_answer_commit_failure_reports_delivery_without_receipt_or_retry() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = TopicFixture::new_with_request(&env, "Ledger format: CSV");
    f.daemon()
        .with_store(|st| st.inject_topic_human_answer_receipt_commit_failure_for_test())
        .unwrap();
    let handler = f.handler();
    let waiting = tokio::spawn(async move {
        handler
            .ask_question(
                vec![Question::single(
                    "Ledger format?".into(),
                    vec!["CSV".into(), "JSON".into()],
                )],
                &CancellationToken::new(),
            )
            .await
    });
    let mut events = f.events();
    let Event::UserQuestion { question_id, .. } = next_event(&mut events, |event| {
        matches!(event, Event::UserQuestion { .. })
    })
    .await
    else {
        unreachable!()
    };
    let answer = || Request::AnswerQuestion {
        question_id: question_id.clone(),
        answer: "CSV".into(),
        answers: vec![QuestionAnswer {
            selected: vec!["CSV".into()],
            text: None,
        }],
    };
    assert_eq!(f.daemon().handle(answer()).await.unwrap()["answered"], true);
    assert_eq!(waiting.await.unwrap().unwrap()[0].selected, vec!["CSV"]);
    assert_eq!(
        f.daemon().handle(answer()).await.unwrap()["answered"],
        false
    );

    let session = f.topic.topic_session_id.as_ref().unwrap();
    let messages = f
        .daemon()
        .with_reader(|st| st.list_messages(session))
        .unwrap();
    assert!(!messages
        .iter()
        .any(|message| message.generated_by.as_deref() == Some("topic_human_answer")));
    assert!(!messages
        .iter()
        .any(|message| message.content.contains("Orchestrator answer")));
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            ServerMessage::Event(Event::MessageAppended { message, .. })
                if message.generated_by.as_deref() == Some("topic_human_answer")
        ))
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
