use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::session_edit::{Change, Metadata, Outcome, MAX_INPUT};

fn show(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    session: &formless_protocol::SessionInfo,
) {
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx);
            shell
                .directory_query()
                .update(cx, |query, cx| query.focus(window, cx));
        });
        window.input(&session.id, cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click("edit-daemon-session", cx);
        window.render_frame(cx);
        assert_eq!(window.find("session-edit-cancel").focused(), Some(true));
        assert_eq!(window.find("session-edit-save").disabled(), Some(true));
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        let operation = commands
            .iter()
            .find_map(|command| match command {
                WireCommand::ReadSessionDetails {
                    operation,
                    session_id,
                    ..
                } => {
                    assert_eq!(session_id, &session.id);
                    Some(*operation)
                }
                _ => None,
            })
            .unwrap();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionDetails {
                    connection: 0,
                    operation,
                    session_id: session.id.clone(),
                    result: Ok(Metadata::from_session(session).unwrap()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_ne!(window.find("session-edit-save").disabled(), Some(true));
    });
}

#[gpui_kit::test]
fn session_metadata_edits_target_the_chosen_list_session_and_preserve_other_drafts(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Composer stays intact ไทย", cx);
    });
    show(cx, handle, &shell, &sessions[1]);
    step(cx, handle, |window, cx| {
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        assert!(shell.read(cx).background_inert(window));
        window.press("f6", cx);
        assert_eq!(window.find("session-edit-cancel").focused(), Some(true));
        let input = shell.read(cx).session_edit_input().unwrap();
        input.update(cx, |input, cx| {
            input.set_value(" Renamed ไทย 😀 ", window, cx)
        });
        window.click("session-edit-save", cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        let operation = commands
            .iter()
            .find_map(|command| match command {
                WireCommand::EditSession {
                    operation,
                    session_id,
                    change,
                    ..
                } => {
                    assert_eq!(session_id, &sessions[1].id);
                    assert_eq!(change, &Change::Name("Renamed ไทย 😀".into()));
                    Some(*operation)
                }
                _ => None,
            })
            .unwrap();
        sessions[1].name = "Renamed ไทย 😀".into();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionEdited {
                    connection: 0,
                    operation: uuid::Uuid::new_v4(),
                    session_id: sessions[1].id.clone(),
                    outcome: Outcome::Applied(Metadata::from_session(&sessions[1]).unwrap()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("session-edit-save").disabled(), Some(true));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionEdited {
                    connection: 0,
                    operation,
                    session_id: sessions[1].id.clone(),
                    outcome: Outcome::Applied(Metadata::from_session(&sessions[1]).unwrap()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click("session-edit-tags", cx);
        window.render_frame(cx);
        let input = shell.read(cx).session_edit_input().unwrap();
        input.update(cx, |input, cx| {
            input.set_value("#Work\nwork\nไทย 漢字 😀", window, cx)
        });
        window.click("session-edit-save", cx);
        let operation = shell
            .update(cx, |shell, _| shell.take_client_commands())
            .into_iter()
            .find_map(|command| match command {
                WireCommand::EditSession {
                    operation,
                    session_id,
                    change,
                    ..
                } => {
                    assert_eq!(session_id, sessions[1].id);
                    assert_eq!(
                        change,
                        Change::Tags(vec!["#Work".into(), "work".into(), "ไทย 漢字 😀".into()])
                    );
                    Some(operation)
                }
                _ => None,
            })
            .unwrap();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionEdited {
                    connection: 0,
                    operation,
                    session_id: sessions[1].id.clone(),
                    outcome: Outcome::Rejected("Fixture rejection".into()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_ne!(window.find("session-edit-save").disabled(), Some(true));
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value(),
            "#Work\nwork\nไทย 漢字 😀"
        );
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "no automatic retry after rejection"
        );
        window.click("session-edit-save", cx);
        let operation = shell
            .update(cx, |shell, _| shell.take_client_commands())
            .into_iter()
            .find_map(|command| match command {
                WireCommand::EditSession { operation, .. } => Some(operation),
                _ => None,
            })
            .unwrap();
        sessions[1].tags = vec!["work".into(), "ไทย 漢字 😀".into()];
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionEdited {
                    connection: 0,
                    operation,
                    session_id: sessions[1].id.clone(),
                    outcome: Outcome::Applied(Metadata::from_session(&sessions[1]).unwrap()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click("session-edit-pin", cx);
        window.render_frame(cx);
        window.click("session-edit-save", cx);
        assert!(shell.update(cx,|shell,_|shell.take_client_commands()).into_iter().any(|command|matches!(command,WireCommand::EditSession {session_id,change:Change::Pin(value),..} if session_id==sessions[1].id && value!=sessions[1].pinned)));
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!shell.read(cx).background_inert(window));
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Chat), window, cx);
        });
        assert_eq!(
            shell.read(cx).composer().read(cx).value(),
            "Composer stays intact ไทย"
        );
    });
}

#[gpui_kit::test]
fn metadata_reconnect_retires_a_waiting_save_without_losing_the_edit(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, &sessions[0]);
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).session_edit_input().unwrap();
        input.update(cx, |input, cx| {
            input.set_value("Pending edit 😀", window, cx)
        });
        window.click("session-edit-save", cx);
        let operation = shell
            .update(cx, |shell, _| shell.take_client_commands())
            .into_iter()
            .find_map(|command| match command {
                WireCommand::EditSession { operation, .. } => Some(operation),
                _ => None,
            })
            .unwrap();
        let connection = shell.read(cx).retire_recording_question_connection();
        assert_ne!(connection, 0);
        window.render_frame(cx);
        assert_eq!(window.find("session-edit-save").disabled(), Some(true));
        assert!(window
            .find("session-edit-notice")
            .value()
            .unwrap()
            .contains("unknown"));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionEdited {
                    connection: 0,
                    operation,
                    session_id: sessions[0].id.clone(),
                    outcome: Outcome::Applied(Metadata::from_session(&sessions[0]).unwrap()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("session-edit-save").disabled(), Some(true));
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value(),
            "Pending edit 😀"
        );
        window.click("session-edit-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "Pending edit 😀"
        );
        window.click("session-edit-reload", cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert!(
            !commands
                .iter()
                .any(|command| matches!(command, WireCommand::EditSession { .. })),
            "reconnect and reload cannot replay the save"
        );
        let read = commands
            .into_iter()
            .find_map(|command| match command {
                WireCommand::ReadSessionDetails { operation, .. } => Some(operation),
                _ => None,
            })
            .unwrap();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionDetails {
                    connection,
                    operation: read,
                    session_id: sessions[0].id.clone(),
                    result: Ok(Metadata::from_session(&sessions[0]).unwrap()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_ne!(window.find("session-edit-save").disabled(), Some(true));
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value(),
            sessions[0].name.as_str()
        );
    });
}

#[gpui_kit::test]
fn metadata_input_guards_composition_and_stays_visible_after_resize(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, &sessions[0]);
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).session_edit_input().unwrap();
        let bounds = window.find_by_label("Session name").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input), window)
            .compose_text(None, "かな", None, cx);
        window.click("session-edit-save", cx);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::EditSession { .. })));
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("session-edit-dialog").is_some());
        window.click("session-edit-tags", cx);
        window.render_frame(cx);
        let input = shell.read(cx).session_edit_input().unwrap();
        input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        window.press(&chord("a"), cx);
        window.input("one ไทย\ntwo 漢字\nthree 😀\nfour\nfive\nsix", cx);
    });
    for (width, height, percent) in [
        (480., 360., 200),
        (1440., 900., 100),
        (800., 700., 150),
        (480., 360., 200),
    ] {
        cx.simulate_window_resize(handle.into(), size(px(width), px(height)));
        step(cx, handle, |window, cx| {
            shell.update(cx, |shell, cx| {
                shell.set_text_scale(TextScale::from_percent(percent), window, cx)
            });
            for _ in 0..4 {
                window.render_frame(cx);
                window.simulate_next_frame(cx);
            }
        });
        cx.run_until_parked();
        step(cx, handle, |window, cx| {
            window.render_frame(cx);
            let input = window.find_by_label("Session tags, one per line");
            let body = window.find("session-edit-body").bounds();
            let bounds = input.bounds();
            assert_eq!(input.focused(), Some(true));
            assert!(
                bounds.top() >= body.top() && bounds.bottom() <= body.bottom(),
                "{percent}%: {bounds:?} outside {body:?}"
            );
        });
    }
    step(cx, handle, |window, cx| {
        window.press(&chord("a"), cx);
        let input = shell.read(cx).session_edit_input().unwrap();
        let bounds = window.find_by_label("Session tags, one per line").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input), window)
            .commit_text(&"x".repeat(MAX_INPUT + 1), cx);
    });
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).session_edit_input().unwrap();
        assert!(input.read(cx).value().contains("six"));
        input.update(cx, |input, cx| {
            input.focus(window, cx);
        });
        window.press(&chord("z"), cx);
        assert!(input.read(cx).value().len() <= MAX_INPUT);
    });
}
