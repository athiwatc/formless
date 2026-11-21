use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::topics::{Action, Reply};
use gpui_kit::SharedString;

fn workspace(cx: &mut TestAppContext, handle: WindowHandle<Root>, shell: &Entity<Shell>) {
    let (sessions, topic) = fixture::topic_workspace();
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.connect_recording_daemon(cx);
            for update in [
                Update::Connection {
                    ready: true,
                    notice: "Connected".into(),
                },
                Update::Sessions(sessions.clone()),
                Update::TopicSupport(true),
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Main(sessions[0].clone())),
                },
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Inventory(vec![topic])),
                },
            ] {
                s.apply_client_update(update, window, cx);
            }
            s.take_client_commands();
        });
    });
}

fn reveal_topic_control(window: &mut Window, cx: &mut App, id: &'static str) {
    window.scroll(
        "topic-workbench",
        ScrollDelta::Pixels(point(px(0.), px(10000.))),
        cx,
    );
    for _ in 0..24 {
        window.render_frame(cx);
        if window.try_find(id).is_some_and(|element| element.visible()) {
            return;
        }
        window.scroll(
            "topic-workbench",
            ScrollDelta::Pixels(point(px(0.), px(-100.))),
            cx,
        );
    }
    panic!("control {id} was not reachable");
}

#[gpui_kit::test]
fn topic_controls_require_explicit_user_actions_and_keep_failed_changes_unsaved(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("topic-topic-formless", cx);
        shell.update(cx, |s, _| s.take_client_commands());
        window.render_frame(cx);
        window.click("topic-controls-toggle", cx);
        window.render_frame(cx);
        assert!(
            shell.update(cx, |s, _| s.take_client_commands()).is_empty(),
            "opening controls sends no mutation"
        );
        window.click("topic-context-toggle", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let actions = commands
            .iter()
            .filter_map(|command| match command {
                WireCommand::Topic {
                    operation: Some(op),
                    action: Action::SetContext { allow, .. },
                    ..
                } => Some((*op, *allow)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(operation, allow)] = actions.as_slice() else {
            panic!("one explicit context decision")
        };
        let operation = *operation;
        assert!(*allow);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: Some(operation),
                    result: Err("topic control request is not implemented yet".into()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("topic-context-scope")
            .value()
            .unwrap()
            .contains("limited to this topic"));
        let inputs = shell.read(cx).topic_policy_inputs().unwrap();
        inputs[0].update(cx, |input, cx| input.set_value("*cargo test*", window, cx));
        reveal_topic_control(window, cx, "topic-rule-allow");
        window.click("topic-rule-allow", cx);
        window.render_frame(cx);
        assert!(
            shell.update(cx, |s, _| s.take_client_commands()).is_empty(),
            "choosing a draft decision never saves a rule"
        );
        reveal_topic_control(window, cx, "topic-rule-add");
        window.click("topic-rule-add", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let actions = commands
            .iter()
            .filter_map(|command| match command {
                WireCommand::Topic {
                    operation: Some(op),
                    action:
                        Action::AddRule {
                            pattern,
                            decision: formless_protocol::TopicRuleDecision::AllowOnce,
                            ..
                        },
                    ..
                } if pattern == "*cargo test*" => Some(*op),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [operation] = actions.as_slice() else {
            panic!("one explicit rule save")
        };
        let operation = *operation;
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: Some(operation),
                    result: Err("topic control request is not implemented yet".into()),
                },
                window,
                cx,
            )
        });
        assert_eq!(inputs[0].read(cx).value().as_ref(), "*cargo test*");
        window.render_frame(cx);
        assert!(
            window.find("topic-notice").visible(),
            "a failed save must show its error without another scroll"
        );
        assert_eq!(
            window.find("topic-notice").value().as_deref(),
            Some("topic control request is not implemented yet"),
            "the visible feedback must show the failure, not the pending notice"
        );
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("topic-chat"));
        assert!(
            shell.update(cx, |s, _| s.take_client_commands()).is_empty(),
            "failure does not replay or start work"
        );
    });
}

#[gpui_kit::test]
fn policy_receipt_keeps_the_current_read_only_attempt_and_shows_unknown_sign_in_honestly(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("topic-attempt-build-x", cx);
        shell.update(cx, |s, _| s.take_client_commands());
        window.render_frame(cx);
        window.click("topic-controls-toggle", cx);
        window.render_frame(cx);
        window.click("topic-context-toggle", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let operation = commands
            .iter()
            .find_map(|command| match command {
                WireCommand::Topic {
                    operation: Some(op),
                    action: Action::SetContext { .. },
                    ..
                } => Some(*op),
                _ => None,
            })
            .unwrap();
        let (_, mut topic) = fixture::topic_workspace();
        topic.allow_external_context = true;
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: Some(operation),
                    result: Ok(Reply::Policy(Box::new(topic))),
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Harnesses(vec![
                        formless_protocol::HarnessAvailability {
                            kind: formless_protocol::HarnessKind::Codex,
                            acp_agent: None,
                            installed: true,
                            signed_in: None,
                            version: None,
                            detail: None,
                        },
                    ])),
                },
                window,
                cx,
            );
        });
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("claude-chat"));
        window.render_frame(cx);
        assert!(window
            .find("topic-context-scope")
            .value()
            .unwrap()
            .contains("allowed"));
        reveal_topic_control(window, cx, "topic-harness-ready-0");
        assert!(window
            .find("topic-harness-ready-0")
            .value()
            .unwrap()
            .contains("sign-in status unknown"));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

fn proposal_source(window: &mut Window, cx: &mut App, shell: &Entity<Shell>) {
    window.render_frame(cx);
    window.click("topic-main-chat", cx);
    let generation = shell.read(cx).client_generation();
    shell.update(cx, |s, cx| {
        s.apply_client_update(Update::Snapshot {
            generation,
            snapshot: replies::session_snapshot("main", &serde_json::json!({
                "session":{"id":"main"}, "running":false,
                "messages":[{"id":"request","session_id":"main","seq":1,"role":"user","content":"Organize my monthly work","created_at":"now"}]
            })).unwrap(),
        }, window, cx);
        s.apply_client_update(Update::TopicCandidates {
            generation, session_id: "main".into(), messages: vec![formless_desktop::topics::Candidate { id: "request".into(), seq: 1, preview: "Organize my monthly work".into() }],
        }, window, cx);
        s.take_client_commands();
    });
    window.render_frame(cx);
    window.click("topic-move-open", cx);
    let inputs = shell.read(cx).topic_inputs();
    inputs[0].update(cx, |s, cx| s.set_value("Monthly work", window, cx));
    inputs[1].update(cx, |s, cx| s.set_value("", window, cx));
    window.render_frame(cx);
}

#[gpui_kit::test]
fn main_chat_can_propose_a_child_under_an_existing_topic(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        proposal_source(window, cx, &shell);
        window.click("topic-parent-open", cx);
        window.render_frame(cx);
        window.click("topic-parent-topic-formless", cx);
        window.render_frame(cx);
        window.click("topic-message-request", cx);
        window.click("topic-propose", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let proposals = commands
            .iter()
            .filter_map(|command| match command {
                WireCommand::Topic {
                    action: Action::Propose(proposal),
                    ..
                } => Some(proposal),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [proposal] = proposals.as_slice() else {
            panic!("one proposal")
        };
        assert_eq!(proposal.source_session_id, "main");
        assert_eq!(proposal.parent_topic_id.as_deref(), Some("topic-formless"));
        assert!(
            proposal.outline.is_empty(),
            "the Formless model can plan without a preset"
        );
    });
}

#[gpui_kit::test]
fn nested_topics_use_canonical_parent_navigation_and_pending_children_do_not_hide_the_parent_after_rejection(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let (mut sessions, mut project) = fixture::topic_workspace();
        project.name = "Project A".into();
        project.attempts.clear();
        let (_, mut story) = fixture::topic_workspace();
        story.id = "story".into();
        story.name = "Story B".into();
        story.parent_topic_id = Some(project.id.clone());
        story.source_session_id = "topic-chat".into();
        story.topic_session_id = Some("story-chat".into());
        for attempt in &mut story.attempts {
            attempt.topic_id = story.id.clone();
        }
        let mut story_session = sessions[1].clone();
        story_session.id = "story-chat".into();
        story_session.name = "Story B".into();
        sessions.push(story_session);
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::Sessions(sessions), window, cx);
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Inventory(vec![story.clone(), project.clone()])),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        window.click("topic-story", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("story-chat"));
        assert!(window.find("topic-breadcrumb-topic-formless").visible());
        assert!(window.find("topic-breadcrumb-story").visible());
        window.click("topic-breadcrumb-topic-formless", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("topic-chat"));
        let mut pending = story.clone();
        pending.id = "new-story".into();
        pending.name = "New story".into();
        pending.status = formless_protocol::TopicStatus::PendingApproval;
        pending.topic_session_id = None;
        pending.attempts.clear();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Inventory(vec![story, project, pending.clone()])),
                },
                window,
                cx,
            );
            s.take_client_commands();
        });
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("topic-chat"));
        assert!(window
            .find("topic-detail-name")
            .value()
            .unwrap()
            .contains("New story"));
        window.click("topic-reject", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let operation = commands
            .iter()
            .find_map(|command| match command {
                WireCommand::Topic {
                    operation: Some(operation),
                    action: Action::Reject(id),
                    ..
                } if id == "new-story" => Some(*operation),
                _ => None,
            })
            .expect("rejects the child, not its parent");
        pending.status = formless_protocol::TopicStatus::Rejected;
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: Some(operation),
                    result: Ok(Reply::Topic(Box::new(pending))),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("topic-detail-name")
            .value()
            .unwrap()
            .contains("Project A"));
        assert!(window.try_find("topic-approve").is_none());
    });
}

#[gpui_kit::test]
fn outline_presets_allow_same_harness_reverse_handoffs_and_other_harnesses(
    cx: &mut TestAppContext,
) {
    use formless_protocol::{HarnessKind::*, StepAccess};
    for (first, second) in [
        (Claude, Claude),
        (Codex, Codex),
        (Codex, Claude),
        (Pi, Antigravity),
        (OpenCode, Pi),
    ] {
        let (handle, shell) = open(cx, WIDE, 0, 0);
        workspace(cx, handle, &shell);
        step(cx, handle, |window, cx| {
            proposal_source(window, cx, &shell);
            window.click("topic-outline-accounting", cx);
            let accounting = shell.read(cx).topic_step_inputs();
            assert_eq!(
                accounting
                    .iter()
                    .map(|(_, inputs)| inputs[0].read(cx).value().to_string())
                    .collect::<Vec<_>>(),
                ["Scan receipts", "Do accounting", "Create reports"]
            );
            window.render_frame(cx);
            window.click("topic-outline-code", cx);
            let outline = shell.read(cx).topic_step_inputs();
            assert_eq!(outline.len(), 2);
            for ((id, inputs), harness) in outline.iter().zip([first, second]) {
                window.render_frame(cx);
                window.click(SharedString::from(format!("topic-step-edit-{id}")), cx);
                window.render_frame(cx);
                window.click(
                    SharedString::from(format!("topic-step-harness-{id}-Some({harness:?})")),
                    cx,
                );
                inputs[2].update(cx, |s, cx| s.set_value("chosen-model", window, cx));
                window.render_frame(cx);
                window.click(SharedString::from(format!("topic-step-edit-{id}")), cx);
            }
            window.render_frame(cx);
            window.click("topic-message-request", cx);
            window.click("topic-propose", cx);
            let commands = shell.update(cx, |s, _| s.take_client_commands());
            let proposals = commands
                .iter()
                .filter_map(|command| match command {
                    WireCommand::Topic {
                        action: Action::Propose(proposal),
                        ..
                    } => Some(proposal),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let [proposal] = proposals.as_slice() else {
                panic!("one proposal, not an approval or start")
            };
            assert!(proposal.workspace.is_none(), "a repository is not required");
            assert_eq!(proposal.outline[0].harness, Some(first));
            assert_eq!(proposal.outline[1].harness, Some(second));
            assert_eq!(proposal.outline[0].access, StepAccess::Write);
            assert_eq!(proposal.outline[1].access, StepAccess::ReadOnly);
            assert!(proposal
                .outline
                .iter()
                .all(|step| step.model.as_deref() == Some("chosen-model")));
            assert_eq!(proposal.message_ids, ["request"]);
        });
    }
}

#[gpui_kit::test]
fn orchestrating_without_attempts_is_normal_and_cancel_is_a_separate_action(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let (_, mut topic) = fixture::topic_workspace();
        topic.status = formless_protocol::TopicStatus::Running;
        topic.attempts.clear();
        for feature in &mut topic.features {
            feature.status = formless_protocol::TopicFeatureStatus::Pending;
        }
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Inventory(vec![topic])),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click("topic-topic-formless", cx);
        shell.update(cx, |s, _| s.take_client_commands());
        window.render_frame(cx);
        assert!(window
            .find("topic-detail-name")
            .value()
            .unwrap()
            .contains("Working"));
        assert!(window.try_find("topic-recover").is_none());
        window.click("topic-cancel", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let actions = commands
            .iter()
            .filter_map(|command| match command {
                WireCommand::Topic { action, .. } => Some(action),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(matches!(actions.as_slice(), [Action::Cancel(id)] if id == "topic-formless"));
    });
}

#[gpui_kit::test]
fn topic_chat_can_answer_the_formless_model_while_attempt_chats_remain_read_only(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("topic-topic-formless", cx);
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::Snapshot { generation, snapshot: replies::session_snapshot("topic-chat", &serde_json::json!({ "session":{"id":"topic-chat"}, "running":false, "messages":[] })).unwrap() }, window, cx);
            s.composer().update(cx, |input, cx| input.set_value("Use the cash accounting method", window, cx));
        });
        window.render_frame(cx);
        assert_ne!(window.find("composer-primary").disabled(), Some(true));
        assert_ne!(window.find("composer-attach").disabled(), Some(true));
        assert!(window
            .find("topic-workflow")
            .value()
            .unwrap()
            .contains("not a fixed pipeline"));
    });
}

#[gpui_kit::test]
fn main_topic_harness_navigation_uses_canonical_transcripts_and_read_only_send(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert!(window.find("topic-main-chat").visible());
        assert!(window.find("topic-topic-formless").visible());
        assert!(window.find("topic-attempt-build-x").visible());
        assert!(window.find("topic-attempt-review-x").visible());
        window.click("topic-topic-formless", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("topic-chat"));
        assert!(window
            .find("topic-feature-feature-x")
            .value()
            .unwrap()
            .contains("Ready for you"));
        assert!(window
            .find("topic-feature-feature-y")
            .value()
            .unwrap()
            .contains("Needs you"));
        window.click("topic-attempt-build-x", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("claude-chat"));
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::Snapshot {
                generation,
                snapshot: replies::session_snapshot("claude-chat", &serde_json::json!({
                    "session":{"id":"claude-chat"}, "running":false,
                    "messages":[{"id":"build-message","session_id":"claude-chat","seq":1,"role":"user","content":"Build feature X","created_at":"now"}]
                })).unwrap(),
            }, window, cx);
            s.composer().update(cx, |input, cx| input.set_value("Must not send", window, cx));
            s.take_client_commands();
        });
        window.render_frame(cx);
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
        assert_eq!(window.find("composer-attach").disabled(), Some(true));
        shell.update(cx, |s, cx| {
            let state = s.command_state(window, cx);
            assert!(!commands::entry(
                Command::RunAction(formless_desktop::run_actions::Kind::Delete),
                &state
            )
            .enabled());
            s.run_command(
                Command::RunAction(formless_desktop::run_actions::Kind::Delete),
                window,
                cx,
            );
            assert!(s.take_client_commands().is_empty());
        });
        window.click("topic-main-chat", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("main"));
    });
}

#[gpui_kit::test]
fn harness_details_export_its_own_transcript_without_allowing_setting_writes(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("topic-attempt-build-x", cx);
        shell.update(cx, |s, cx| {
            s.take_client_commands();
            let state = s.command_state(window, cx);
            assert!(commands::entry(Command::SessionDetails, &state).enabled());
            assert!(!commands::entry(Command::SessionModel, &state).enabled());
            s.run_command(Command::SessionDetails, window, cx);
        });
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let operation = commands
            .iter()
            .find_map(|command| match command {
                WireCommand::ReadSessionDetails {
                    operation,
                    session_id,
                    ..
                } => {
                    assert_eq!(session_id, "claude-chat");
                    Some(*operation)
                }
                _ => None,
            })
            .unwrap();
        let mut session = fixture::topic_workspace().0[0].clone();
        session.id = "claude-chat".into();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::SessionDetails {
                    connection: 0,
                    operation,
                    session_id: session.id.clone(),
                    result: Ok(
                        formless_desktop::session_edit::Metadata::from_session(&session).unwrap(),
                    ),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("session-edit-help")
            .value()
            .unwrap()
            .contains("read only"));
        assert_eq!(window.find("session-edit-save").disabled(), Some(true));
        window.click("session-edit-model", cx);
        shell.update(cx, |s, cx| {
            s.take_client_commands();
            // Exercise the callback as well as the disabled Save control.
            s.save_presented_session_selection(s.session_edit_revision().unwrap(), window, cx);
            assert!(s.take_client_commands().is_empty());
            s.read_presented_session_catalog(s.session_edit_revision().unwrap(), window, cx);
            assert!(
                s.take_client_commands().is_empty(),
                "a stale model editor callback must remain disabled in the inspector"
            );
        });
        window.scroll(
            "session-edit-body",
            ScrollDelta::Pixels(point(px(0.), px(-10000.))),
            cx,
        );
        window.render_frame(cx);
        assert!(window.find("session-export-json").visible());
        assert_ne!(window.find("session-export-json").disabled(), Some(true));
        window.click("session-export-json", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(
            matches!(commands.as_slice(), [WireCommand::ExportSession {session_id, ..}] if session_id == "claude-chat")
        );
        let [WireCommand::ExportSession {
            operation, format, ..
        }] = commands.as_slice()
        else {
            unreachable!()
        };
        let exported = "Persisted harness transcript ไทย 😀\n";
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::SessionExport {
                    connection: 0,
                    operation: *operation,
                    session_id: "claude-chat".into(),
                    format: *format,
                    result: Ok(formless_desktop::session_export::Document {
                        name: "Harness attempt".into(),
                        text: exported.into(),
                    }),
                },
                window,
                cx,
            )
        });
        for _ in 0..100 {
            window.render_frame(cx);
            if window.find("session-export-copy").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("session-export-copy").focused(), Some(true));
        window.press("space", cx);
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), exported);
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn topic_accept_and_cancel_emit_distinct_single_intents(cx: &mut TestAppContext) {
    for accept in [true, false] {
        let (handle, shell) = open(cx, WIDE, 0, 0);
        workspace(cx, handle, &shell);
        let (_, mut topic) = fixture::topic_workspace();
        topic.status = formless_protocol::TopicStatus::PendingApproval;
        topic.topic_session_id = None;
        topic.attempts.clear();
        step(cx, handle, |window, cx| {
            shell.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::Topics {
                        connection: 0,
                        operation: None,
                        result: Ok(Reply::Inventory(vec![topic])),
                    },
                    window,
                    cx,
                )
            });
            window.render_frame(cx);
            assert!(
                window.find("topic-consent").visible(),
                "daemon proposal appears in its source chat without a navigation step"
            );
            window.click("topic-topic-formless", cx);
            window.render_frame(cx);
            assert!(window
                .find("topic-consent")
                .value()
                .unwrap()
                .contains("start this workflow"));
            window.click(
                if accept {
                    "topic-approve"
                } else {
                    "topic-reject"
                },
                cx,
            );
            window.render_frame(cx);
            assert_eq!(window.find("topic-approve").disabled(), Some(true));
            let commands = shell.update(cx, |s, _| s.take_client_commands());
            let actions = commands
                .iter()
                .filter_map(|c| {
                    if let WireCommand::Topic { action, .. } = c {
                        Some(action)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(actions.len(), 1);
            if accept {
                assert!(matches!(actions[0],Action::MoveAndStart(id) if id=="topic-formless"));
            } else {
                assert!(matches!(actions[0],Action::Reject(id) if id=="topic-formless"));
            }
            assert_eq!(
                shell.read(cx).sidebar().active_id(),
                Some("main"),
                "no local move before daemon receipt"
            );
        });
    }
}

#[gpui_kit::test]
fn failed_topic_can_recover_then_retry_without_another_move(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let (_, mut topic) = fixture::topic_workspace();
        topic.status = formless_protocol::TopicStatus::Failed;
        // Recovery preserves history. A later failed launch must still be
        // recoverable when those older attempts are terminal.
        for attempt in &mut topic.attempts {
            attempt.status = formless_protocol::TopicAttemptStatus::Abandoned;
        }
        for feature in &mut topic.features {
            feature.status = formless_protocol::TopicFeatureStatus::Failed;
        }
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Inventory(vec![topic.clone()])),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        window.click("topic-topic-formless", cx);
        shell.update(cx, |s, _| s.take_client_commands());
        window.render_frame(cx);
        assert!(window
            .find("topic-detail-name")
            .value()
            .unwrap()
            .contains("Needs attention"));
        assert!(window
            .find("topic-recovery-help")
            .value()
            .unwrap()
            .contains("does not move them again"));
        assert!(window.try_find("topic-approve").is_none());
        assert!(window.try_find("topic-cancel").is_none());
        window.click("topic-recover", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let actions = commands
            .iter()
            .filter_map(|command| match command {
                WireCommand::Topic {
                    operation: Some(op),
                    action,
                    ..
                } => Some((*op, action)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(operation, action)] = actions.as_slice() else {
            panic!("one explicit recovery")
        };
        assert!(matches!(action, Action::Recover(id) if id == "topic-formless"));
        // The real v2 daemon returns Failed after recovery. Only resetting an
        // item changes it to Approved; neither operation starts any work.
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: Some(*operation),
                    result: Ok(Reply::Topic(Box::new(topic.clone()))),
                },
                window,
                cx,
            );
            assert!(s
                .take_client_commands()
                .iter()
                .all(|c| !matches!(c, WireCommand::Topic { .. })));
        });
        window.render_frame(cx);
        assert!(window.try_find("topic-start").is_none());
        assert!(window
            .find("topic-recovery-help")
            .value()
            .unwrap()
            .contains("Reset an item for retry below, then choose Start workflow"));
        let feature = topic.features[0].id.clone();
        window.scroll(
            "topic-workbench",
            ScrollDelta::Pixels(point(px(0.), px(-400.))),
            cx,
        );
        window.render_frame(cx);
        window.click(SharedString::from(format!("topic-retry-{feature}")), cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let actions = commands
            .iter()
            .filter_map(|command| match command {
                WireCommand::Topic {
                    operation: Some(operation),
                    action,
                    ..
                } => Some((*operation, action)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let [(operation, action)] = actions.as_slice() else {
            panic!("one explicit reset")
        };
        assert!(
            matches!(action, Action::Retry {topic, feature: id} if topic == "topic-formless" && id == &feature)
        );
        topic.status = formless_protocol::TopicStatus::Approved;
        topic.features[0].status = formless_protocol::TopicFeatureStatus::Pending;
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: Some(*operation),
                    result: Ok(Reply::Topic(Box::new(topic.clone()))),
                },
                window,
                cx,
            );
            assert!(s
                .take_client_commands()
                .iter()
                .all(|c| !matches!(c, WireCommand::Topic { .. })));
        });
        window.render_frame(cx);
        window.scroll(
            "topic-workbench",
            ScrollDelta::Pixels(point(px(0.), px(400.))),
            cx,
        );
        window.render_frame(cx);
        window.click("topic-start", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        let actions = commands
            .iter()
            .filter_map(|command| match command {
                WireCommand::Topic { action, .. } => Some(action),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(matches!(actions.as_slice(), [Action::Start(id)] if id == "topic-formless"));
    });
}

#[gpui_kit::test]
fn rejected_selection_does_not_hide_the_next_main_chat_proposal(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let (_, mut rejected) = fixture::topic_workspace();
        rejected.status = formless_protocol::TopicStatus::Rejected;
        rejected.topic_session_id = None;
        rejected.attempts.clear();
        let mut pending = rejected.clone();
        pending.id = "next-proposal".into();
        pending.status = formless_protocol::TopicStatus::PendingApproval;
        pending.name = "New X/Y proposal".into();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Topic(Box::new(rejected.clone()))),
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Inventory(vec![rejected, pending])),
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("main"));
        assert!(window
            .find("topic-consent")
            .value()
            .unwrap()
            .contains("New X/Y proposal"));
        assert!(window.find("topic-approve").visible());
    });
}

#[gpui_kit::test]
fn harness_topic_chat_does_not_block_editing_a_selected_regular_session(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.click("topic-attempt-build-x", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("claude-chat"));
    });
    let (sessions, _) = fixture::topic_workspace();
    super::session_edit_tests::show(cx, handle, &shell, &sessions[0]);
    step(cx, handle, |_window, cx| {
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("claude-chat"));
    });
}

#[gpui_kit::test]
fn narrow_topic_navigation_remains_available_in_sidebar_overlay(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, size(px(560.), px(800.)), 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("toggle-sidebar", cx);
        window.render_frame(cx);
        assert!(window.find("topic-attempt-review-x").visible());
        window.click("topic-attempt-review-x", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("codex-chat"));
        assert!(!shell.read(cx).background_inert(window));
        assert!(window
            .find("topic-workspace-label")
            .value()
            .unwrap()
            .contains("read only"));
    });
}

#[gpui_kit::test]
fn pending_proposal_opens_from_settings_but_archived_topic_never_relabels_another_chat(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let (mut sessions, mut topic) = fixture::topic_workspace();
        sessions[1].archived = true;
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::Sessions(sessions), window, cx)
        });
        window.render_frame(cx);
        window.click("topic-topic-formless", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("main"));
        assert!(window.try_find("topic-detail-name").is_none());

        topic.status = formless_protocol::TopicStatus::PendingApproval;
        topic.topic_session_id = None;
        topic.attempts.clear();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Inventory(vec![topic])),
                },
                window,
                cx,
            );
            s.run_command(Command::GoTo(Destination::Settings), window, cx);
        });
        window.render_frame(cx);
        window.click("topic-topic-formless", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert!(window.find("topic-consent").visible());
        assert_eq!(shell.read(cx).sidebar().active_id(), Some("main"));
    });
}

#[gpui_kit::test]
fn inventory_refresh_keeps_proposal_edits_and_a_cwdless_source_clears_old_repository(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    workspace(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: replies::session_snapshot(
                        "main",
                        &serde_json::json!({"session":{"id":"main"},"messages":[],"running":false}),
                    )
                    .unwrap(),
                },
                window,
                cx,
            );
            s.apply_client_update(
                Update::TopicCandidates {
                    generation,
                    session_id: "main".into(),
                    messages: vec![formless_desktop::topics::Candidate {
                        id: "message-x".into(),
                        seq: 1,
                        preview: "Add X".into(),
                    }],
                },
                window,
                cx,
            );
        });
        window.render_frame(cx);
        window.click("topic-move-open", cx);
        let inputs = shell.read(cx).topic_inputs();
        inputs[0].update(cx, |s, cx| s.set_value("My topic", window, cx));
        inputs[1].update(cx, |s, cx| s.set_value("/old/repo", window, cx));
        window.render_frame(cx);
        window.click("topic-message-message-x", cx);
        window.click("topics-refresh", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands.iter().any(|c| matches!(
            c,
            WireCommand::Topic {
                operation: None,
                action: Action::List,
                ..
            }
        )));
        let (_, topic) = fixture::topic_workspace();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Topics {
                    connection: 0,
                    operation: None,
                    result: Ok(Reply::Inventory(vec![topic])),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.find("topic-propose").visible());
        assert_ne!(window.find("topic-propose").disabled(), Some(true));
        assert_eq!(inputs[0].read(cx).value().as_ref(), "My topic");
        assert_eq!(inputs[1].read(cx).value().as_ref(), "/old/repo");
        window.click("topic-move-open", cx);
        window.render_frame(cx);
        window.click("topic-move-open", cx);
        assert!(
            inputs[1].read(cx).value().is_empty(),
            "new proposal cannot inherit a repository from an earlier source"
        );
    });
}
