use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::topics::{Action, Reply};

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
