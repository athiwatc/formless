use super::session_edit_tests::show;
use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::session_edit::{Change, Metadata, Outcome, MAX_INPUT};
use gpui_kit::EntityInputHandler as _;

fn saved(shell: &Entity<Shell>, cx: &mut App, id: &str, expected: Change) -> uuid::Uuid {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    assert!(!commands
        .iter()
        .any(|command| matches!(command, WireCommand::Intent(_) | WireCommand::Review { .. })));
    commands
        .into_iter()
        .find_map(|command| match command {
            WireCommand::EditSession {
                operation,
                session_id,
                change,
                ..
            } => {
                assert_eq!(session_id, id);
                assert_eq!(change, expected);
                Some(operation)
            }
            _ => None,
        })
        .unwrap()
}

fn ack(
    shell: &Entity<Shell>,
    window: &mut Window,
    cx: &mut App,
    session: &formless_protocol::SessionInfo,
    operation: uuid::Uuid,
) {
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::SessionEdited {
                connection: 0,
                operation,
                session_id: session.id.clone(),
                outcome: Outcome::Applied(Metadata::from_session(session).unwrap()),
            },
            window,
            cx,
        )
    });
}

#[gpui_kit::test]
fn instructions_replace_only_the_frozen_session_field_and_keep_unsaved_unicode_edits(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent composer ไทย 😀", cx);
    });
    sessions[1].instructions = Some("Previous instructions".into());
    show(cx, handle, &shell, &sessions[1]);
    step(cx, handle, |window, cx| {
        window.click("session-edit-instructions", cx);
        window.render_frame(cx);
        assert!(window
            .find("session-edit-instructions-help")
            .value()
            .unwrap()
            .contains("next agent step"));
        let input = shell.read(cx).session_edit_input().unwrap();
        let raw = " \nFirst ไทย\n\tIndented 漢字 😀\n ";
        let expected = "First ไทย\n\tIndented 漢字 😀";
        input.update(cx, |input, cx| input.set_value(raw, window, cx));
        window.click("session-edit-copy", cx);
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), raw);
        window.click("session-edit-save", cx);
        let operation = saved(
            &shell,
            cx,
            &sessions[1].id,
            Change::Instructions(Some(expected.into())),
        );
        input.update(cx, |input, cx| {
            input.set_value("A newer unsaved edit 😀", window, cx)
        });
        sessions[1].instructions = Some(expected.into());
        ack(&shell, window, cx, &sessions[1], operation);
        window.render_frame(cx);
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value(),
            "A newer unsaved edit 😀"
        );
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "Unsent composer ไทย 😀");
        input.update(cx, |input, cx| input.set_value(" \n\t ", window, cx));
        window.click("session-edit-save", cx);
        let operation = saved(&shell, cx, &sessions[1].id, Change::Instructions(None));
        sessions[1].instructions = None;
        ack(&shell, window, cx, &sessions[1], operation);
        window.press("escape", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "Unsent composer ไทย 😀");
    });
}

#[gpui_kit::test]
fn unsupported_instructions_preserve_other_metadata_and_any_owned_draft(cx: &mut TestAppContext) {
    for owned in [false, true] {
        let (handle, shell) = open(cx, WIDE, 0, 0);
        let mut sessions = daemon_view(cx, handle, &shell);
        sessions[0].instructions = Some(if owned {
            "Bounded source".into()
        } else {
            "x".repeat(MAX_INPUT + 1)
        });
        show(cx, handle, &shell, &sessions[0]);
        step(cx, handle, |window, cx| {
            window.click("session-edit-instructions", cx);
            window.render_frame(cx);
            if owned {
                shell
                    .read(cx)
                    .session_edit_input()
                    .unwrap()
                    .update(cx, |input, cx| {
                        input.set_value("My retained instructions ไทย", window, cx)
                    });
            } else {
                assert!(shell.read(cx).session_edit_input().is_none());
                assert_eq!(window.find("session-edit-save").disabled(), Some(true));
                assert_eq!(window.find("session-edit-copy").disabled(), Some(true));
            }
            window.click("session-edit-name", cx);
            window.render_frame(cx);
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .update(cx, |input, cx| {
                    input.set_value("Name still editable", window, cx)
                });
            window.click("session-edit-save", cx);
            let operation = saved(
                &shell,
                cx,
                &sessions[0].id,
                Change::Name("Name still editable".into()),
            );
            sessions[0].name = "Name still editable".into();
            sessions[0].instructions = Some("x".repeat(MAX_INPUT + 1));
            ack(&shell, window, cx, &sessions[0], operation);
            window.render_frame(cx);
            window.click("session-edit-instructions", cx);
            window.render_frame(cx);
            assert!(window
                .find("session-edit-instructions-unavailable")
                .value()
                .unwrap()
                .contains("above the native"));
            assert_eq!(window.find("session-edit-save").disabled(), Some(true));
            if owned {
                assert_eq!(
                    shell
                        .read(cx)
                        .session_edit_input()
                        .unwrap()
                        .read(cx)
                        .value(),
                    "My retained instructions ไทย"
                );
                window.click("session-edit-copy", cx);
                assert_eq!(
                    cx.read_from_clipboard().unwrap().text().unwrap(),
                    "My retained instructions ไทย"
                );
            }
            assert!(!shell
                .update(cx, |shell, _| shell.take_client_commands())
                .iter()
                .any(|command| matches!(command, WireCommand::EditSession { .. })));
        });
    }
}

#[gpui_kit::test]
fn instructions_guard_preedit_oversized_commits_and_unknown_outcomes(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, &sessions[0]);
    step(cx, handle, |window, cx| {
        window.click("session-edit-instructions", cx);
        window.render_frame(cx);
        let input = shell.read(cx).session_edit_input().unwrap();
        input.update(cx, |input, cx| {
            input.set_value("Earlier instructions ไทย", window, cx);
            input.focus(window, cx);
        });
        let bounds = window
            .find_by_label("Session standing instructions")
            .bounds();
        TestInput::new(ElementInputHandler::new(bounds, input.clone()), window)
            .compose_text(None, "かな", None, cx);
        window.click("session-edit-save", cx);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::EditSession { .. })));
        // Let the button's focus transition finish before another preedit.
        input.update(cx, |input, cx| input.focus(window, cx));
        window.render_frame(cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).session_edit_input().unwrap();
        let bounds = window
            .find_by_label("Session standing instructions")
            .bounds();
        TestInput::new(ElementInputHandler::new(bounds, input.clone()), window)
            .compose_text(None, "かな", None, cx);
        assert!(
            input.update(cx, |input, cx| input
                .marked_text_range(window, cx)
                .is_some()),
            "the Escape case must begin with live preedit"
        );
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("session-edit-dialog").is_some());
        input.update(cx, |input, cx| {
            input.set_value("Earlier instructions ไทย", window, cx);
            input.focus(window, cx);
        });
    });
    step(cx, handle, |window, cx| {
        window.press(&chord("a"), cx);
        let input = shell.read(cx).session_edit_input().unwrap();
        let bounds = window
            .find_by_label("Session standing instructions")
            .bounds();
        TestInput::new(ElementInputHandler::new(bounds, input), window)
            .commit_text(&"x".repeat(MAX_INPUT + 1), cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value(),
            "Earlier instructions ไทย"
        );
        window.click("session-edit-save", cx);
        let operation = saved(
            &shell,
            cx,
            &sessions[0].id,
            Change::Instructions(Some("Earlier instructions ไทย".into())),
        );
        let connection = shell.read(cx).retire_recording_question_connection();
        assert_ne!(connection, 0);
        window.render_frame(cx);
        assert_eq!(window.find("session-edit-save").disabled(), Some(true));
        ack(&shell, window, cx, &sessions[0], operation);
        window.render_frame(cx);
        assert_eq!(window.find("session-edit-save").disabled(), Some(true));
        window.click("session-edit-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "Earlier instructions ไทย"
        );
        window.click("session-edit-reload", cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert!(!commands
            .iter()
            .any(|command| matches!(command, WireCommand::EditSession { .. })));
        assert!(commands.iter().any(|command|matches!(command,WireCommand::ReadSessionDetails{connection:current,..} if *current==connection)));
    });
}
