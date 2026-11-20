use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::session_fork::{Action, Outcome, Receipt};
use serde_json::json;
fn choose(shell: &Entity<Shell>, id: &str, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| {
        shell.run_command(Command::GoTo(Destination::Sessions), window, cx);
        shell.focus_section(PageSection::Conversation, window, cx);
    });
    for _ in 0..4 {
        window.press("up", cx);
    }
    for _ in 0..4 {
        if shell.read(cx).directory_selected_id() == Some(id) {
            break;
        }
        window.press("down", cx);
    }
    assert_eq!(shell.read(cx).directory_selected_id(), Some(id));
    shell.update(cx, |shell, _| shell.take_client_commands());
    window.render_frame(cx);
}
fn review(shell: &Entity<Shell>, id: &str, window: &mut Window, cx: &mut App) -> uuid::Uuid {
    choose(shell, id, window, cx);
    window.press(
        if cfg!(target_os = "macos") {
            "cmd-shift-p"
        } else {
            "ctrl-shift-p"
        },
        cx,
    );
    window.input("Review a new Session fork", cx);
    window.press("enter", cx);
    window.render_frame(cx);
    assert_eq!(shell.read(cx).destination(), Destination::ForkReview);
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, WireCommand::ForkSession(_))),
        "review creates nothing"
    );
    shell.read(cx).fork_review_ticket().unwrap()
}
fn requested(shell: &Entity<Shell>, source: &str, cx: &mut App) -> Action {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    assert_eq!(commands.len(), 1);
    match commands.into_iter().next().unwrap() {
        WireCommand::ForkSession(action) => {
            assert_eq!(action.source_id, source);
            assert_eq!(action.connection, 0);
            action
        }
        _ => panic!("one explicit fork request expected"),
    }
}
#[gpui_kit::test]
fn session_fork_requires_explicit_confirmation_and_never_activates_or_starts_the_child(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: replies::session_snapshot(
                        &sessions[0].id,
                        &json!({"session":{"id":sessions[0].id},"running":false,"messages":[]}),
                    )
                    .unwrap(),
                },
                window,
                cx,
            );
            shell.focus_section(PageSection::Composer, window, cx);
        });
        window.input("Unsent A ไทย 😀", cx);
        let composer = shell.read(cx).composer().clone();
        let ticket = review(&shell, &sessions[1].id, window, cx);
        assert!(window
            .find("fork-source")
            .value()
            .unwrap()
            .contains(&sessions[1].id));
        assert!(window
            .find("fork-scope")
            .value()
            .unwrap()
            .contains("ALL history persisted"));
        window.click("fork-confirm", cx);
        let action = requested(&shell, &sessions[1].id, cx);
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "duplicate old confirmation cannot enqueue"
        );
        shell.update(cx, |shell, cx| {
            shell.record_fork_receipt(
                &action,
                Outcome::Applied(Receipt {
                    child_id: "new-child".into(),
                    fork_point_seq: 41,
                }),
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("fork-notice")
            .value()
            .unwrap()
            .contains("new-child"));
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "known receipt does not make old confirmation reusable"
        );
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        assert_eq!(shell.read(cx).client_generation(), generation);
        window.click("fork-back", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        assert!(window
            .find("fork-status")
            .value()
            .unwrap()
            .contains("new-child"));
        let next = review(&shell, &sessions[2].id, window, cx);
        assert_ne!(next, ticket);
        assert!(window
            .find("fork-notice")
            .value()
            .unwrap()
            .contains("Nothing has been sent"));
        assert!(!window
            .find("fork-notice")
            .value()
            .unwrap()
            .contains("new-child"));
        window.click("fork-back", cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Chat), window, cx)
        });
        assert_eq!(shell.read(cx).composer(), &composer);
        assert_eq!(composer.read(cx).value().as_ref(), "Unsent A ไทย 😀");
        assert!(shell.read(cx).last_intent().is_none());
    });
}
#[gpui_kit::test]
fn session_fork_cancel_navigation_modals_and_composition_retire_old_confirmation(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let ticket = review(&shell, &sessions[0].id, window, cx);
        window.click("fork-back", cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        let ticket = review(&shell, &sessions[1].id, window, cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ShowCommands, window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        window.render_frame(cx);
        window.press("escape", cx);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(vec![sessions[0].clone()]), window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "a missing source creates nothing"
        );
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx)
        });
        choose(&shell, &sessions[0].id, window, cx);
        let input = shell.read(cx).directory_query().clone();
        let bounds = window.find_by_label("Filter Sessions").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ReviewFork, window, cx);
        });
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::ForkSession(_))));
    });
}
#[gpui_kit::test]
fn session_fork_connection_and_profile_turnover_cannot_use_an_old_review(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let ticket = review(&shell, &sessions[0].id, window, cx);
        assert_eq!(shell.read(cx).retire_recording_question_connection(), 1);
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        shell.update(cx, |shell, cx| shell.connect_recording_daemon(cx));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "replacement".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
        });
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        assert!(shell.read(cx).fork_review_ticket().is_none());
    });
}
#[gpui_kit::test]
fn session_fork_lost_receipt_blocks_all_further_forks_and_surfaces_after_leaving(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        review(&shell, &sessions[0].id, window, cx);
        window.click("fork-confirm", cx);
        let action = requested(&shell, &sessions[0].id, cx);
        shell.update(cx, |shell, cx| shell.record_fork_write(&action, cx));
        window.click("fork-back", cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        assert_eq!(shell.read(cx).retire_recording_question_connection(), 1);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "offline".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "online".into(),
                },
                window,
                cx,
            );
            shell.poll_client(window, cx);
        });
        window.render_frame(cx);
        assert!(window
            .find("fork-status")
            .value()
            .unwrap()
            .contains("UNKNOWN"));
        // A late old reply cannot resolve an already retired transport write.
        shell.update(cx, |shell, cx| {
            shell.record_fork_receipt(
                &action,
                Outcome::Applied(Receipt {
                    child_id: "late-child".into(),
                    fork_point_seq: 1,
                }),
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("fork-status")
            .value()
            .unwrap()
            .contains("UNKNOWN"));
        choose(&shell, &sessions[1].id, window, cx);
        assert!(shell
            .update(cx, |shell, cx| shell.run_command(
                Command::ReviewFork,
                window,
                cx
            ))
            .is_some());
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
    });
}
#[gpui_kit::test]
fn session_fork_queue_failure_is_known_unsent_and_requires_a_new_review(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let ticket = review(&shell, &sessions[0].id, window, cx);
        for _ in 0..16 {
            shell.update(cx, |shell, cx| {
                shell.run_command(Command::GoTo(Destination::ForkReview), window, cx)
            });
        }
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        window.render_frame(cx);
        assert!(window
            .find("fork-notice")
            .value()
            .unwrap()
            .contains("not queued"));
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert_eq!(commands.len(), 16);
        assert!(commands
            .iter()
            .all(|command| matches!(command, WireCommand::Page(Destination::ForkReview))));
        shell.update(cx, |shell, cx| {
            shell.confirm_presented_fork(ticket, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        review(&shell, &sessions[0].id, window, cx);
        window.click("fork-confirm", cx);
        requested(&shell, &sessions[0].id, cx);
    });
}
fn focus_control(id: &'static str, window: &mut Window, cx: &mut App) {
    let mut found = false;
    for _ in 0..60 {
        window.press("tab", cx);
        settle_frames(window, cx);
        if window.find(id).focused() == Some(true) {
            found = true;
            break;
        }
    }
    assert!(found, "{id} keyboard reachable");
    let bounds = window.find(id).bounds();
    assert!(
        bounds.top() >= px(0.)
            && bounds.bottom() <= px(360.)
            && bounds.left() >= px(0.)
            && bounds.right() <= px(480.),
        "{id}: {bounds:?}"
    );
}
#[gpui_kit::test]
fn session_fork_review_and_receipt_are_keyboard_readable_at_minimum_size_200_percent(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx)
        });
        choose(&shell, &sessions[0].id, window, cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ReviewFork, window, cx)
        });
        window.render_frame(cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        assert!(window.find("fork-scope").bounds().size.width <= px(480.));
        let before = window.find("fork-scope").bounds().top();
        window.press("pagedown", cx);
        settle_frames(window, cx);
        assert!(window.find("fork-scope").bounds().top() < before);
        focus_control("fork-confirm", window, cx);
        window.press("enter", cx);
        let action = requested(&shell, &sessions[0].id, cx);
        shell.update(cx, |shell, cx| {
            shell.record_fork_receipt(
                &action,
                Outcome::Applied(Receipt {
                    child_id: "created-child".into(),
                    fork_point_seq: 0,
                }),
                cx,
            )
        });
        focus_control("fork-back", window, cx);
        window.press("enter", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Status, window, cx)
        });
        settle_frames(window, cx);
        window.press("home", cx);
        settle_frames(window, cx);
        assert!(window.find("fork-status").bounds().size.width <= px(480.));
        assert!(window
            .find("fork-status")
            .value()
            .unwrap()
            .contains("created-child"));
    });
}
