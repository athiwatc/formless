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
        assert_eq!(window.find("composer-primary").disabled(), Some(true));
        assert_eq!(window.find("composer-attach").disabled(), Some(true));
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
