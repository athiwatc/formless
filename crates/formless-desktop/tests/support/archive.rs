use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::session_edit::{Change, Metadata, Outcome};

fn load_details(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    session: &formless_protocol::SessionInfo,
) {
    step(cx, handle, |window, cx| {
        let operation = shell
            .update(cx, |shell, _| shell.take_client_commands())
            .into_iter()
            .find_map(|command| match command {
                WireCommand::ReadSessionDetails {
                    operation,
                    session_id,
                    ..
                } => {
                    assert_eq!(session_id, session.id);
                    Some(operation)
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
    });
}

fn saved_operation(shell: &Entity<Shell>, cx: &mut App, id: &str, expected: Change) -> uuid::Uuid {
    shell
        .update(cx, |shell, _| shell.take_client_commands())
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
            WireCommand::Intent(_) | WireCommand::Review { .. } => {
                panic!("archive must not start or stop provider work")
            }
            _ => None,
        })
        .unwrap()
}

fn acknowledge(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    session: &formless_protocol::SessionInfo,
    operation: uuid::Uuid,
    sessions: &[formless_protocol::SessionInfo],
) {
    step(cx, handle, |window, cx| {
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
            );
            shell.apply_client_update(Update::Sessions(sessions.to_vec()), window, cx);
        });
        window.render_frame(cx);
    });
}

#[gpui_kit::test]
fn archiving_the_open_session_preserves_its_draft_and_restore_never_activates_a_neighbour(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent draft ไทย 漢字 😀", cx);
    });
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::SessionDetails, window, cx);
        });
    });
    load_details(cx, handle, &shell, &sessions[0]);
    let archive = step(cx, handle, |window, cx| {
        window.click("session-edit-archive", cx);
        window.render_frame(cx);
        assert!(window
            .find("session-edit-archive-value")
            .value()
            .unwrap()
            .contains("Running work continues"));
        window.click("session-edit-save", cx);
        saved_operation(&shell, cx, &sessions[0].id, Change::Archive(true))
    });
    sessions[0].archived = true;
    acknowledge(cx, handle, &shell, &sessions[0], archive, &sessions);
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
        assert!(window.try_find("session-edit-dialog").is_some());
        assert!(window
            .find("session-edit-archive-value")
            .value()
            .unwrap()
            .contains("Restore"));
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!shell.read(cx).background_inert(window));
        assert_eq!(window.find("region-conversation").focused(), Some(true));
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx);
        });
        window.render_frame(cx);
        assert_eq!(shell.read(cx).directory_selected_id(), None);
        window.press("enter", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
        assert!(window.try_find(directory_row_id(&sessions[0].id)).is_none());
        window.click("toggle-archived-sessions", cx);
        window.render_frame(cx);
        assert!(window
            .find("directory-status")
            .value()
            .unwrap()
            .contains("Including archived"));
        window.click(directory_row_id(&sessions[0].id), cx);
        window.render_frame(cx);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            None,
            "archived entry opens details, not a transcript or neighbour"
        );
    });
    load_details(cx, handle, &shell, &sessions[0]);
    let restore = step(cx, handle, |window, cx| {
        assert!(window
            .find("session-edit-archive-value")
            .value()
            .unwrap()
            .contains("Restore"));
        window.click("session-edit-save", cx);
        saved_operation(&shell, cx, &sessions[0].id, Change::Archive(false))
    });
    sessions[0].archived = false;
    acknowledge(cx, handle, &shell, &sessions[0], restore, &sessions);
    step(cx, handle, |window, cx| {
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
        window.press("escape", cx);
        window.render_frame(cx);
        window.click(directory_row_id(&sessions[0].id), cx);
        window.render_frame(cx);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        assert_eq!(
            shell.read(cx).draft(cx).as_ref(),
            "Unsent draft ไทย 漢字 😀"
        );
    });
}

#[gpui_kit::test]
fn stale_toggle_buttons_cannot_reverse_a_freshly_loaded_pin_or_archive_state(
    cx: &mut TestAppContext,
) {
    for field in ["session-edit-pin", "session-edit-archive"] {
        let (handle, shell) = open(cx, WIDE, 0, 0);
        let mut sessions = daemon_view(cx, handle, &shell);
        sessions[0].pinned = false;
        sessions[0].archived = false;
        step(cx, handle, |window, cx| {
            shell.update(cx, |shell, cx| {
                shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx)
            });
            window.render_frame(cx);
            window.click(session_row_id(&sessions[0].id), cx);
            shell.update(cx, |shell, cx| {
                shell.run_command(Command::SessionDetails, window, cx);
            });
        });
        load_details(cx, handle, &shell, &sessions[0]);
        step(cx, handle, |window, cx| {
            window.click(field, cx);
            window.render_frame(cx);
            for (id, connection) in [(&sessions[1].id, 0), (&sessions[0].id, 1)] {
                shell.update(cx, |shell, cx| {
                    shell.save_presented_session_toggle(
                        id,
                        connection,
                        if field == "session-edit-pin" {
                            Change::Pin(true)
                        } else {
                            Change::Archive(true)
                        },
                        window,
                        cx,
                    )
                });
                assert!(!shell
                    .update(cx, |shell, _| shell.take_client_commands())
                    .iter()
                    .any(|command| matches!(command, WireCommand::EditSession { .. })));
            }
            window.click("session-edit-reload", cx);
            let operation = shell
                .update(cx, |shell, _| shell.take_client_commands())
                .into_iter()
                .find_map(|command| match command {
                    WireCommand::ReadSessionDetails { operation, .. } => Some(operation),
                    _ => None,
                })
                .unwrap();
            if field == "session-edit-pin" {
                sessions[0].pinned = true;
            } else {
                sessions[0].archived = true;
            }
            shell.update(cx, |shell, cx| {
                shell.apply_client_update(
                    Update::SessionDetails {
                        connection: 0,
                        operation,
                        session_id: sessions[0].id.clone(),
                        result: Ok(Metadata::from_session(&sessions[0]).unwrap()),
                    },
                    window,
                    cx,
                )
            });
            // TestWindow::click redraws before dispatch. Deliver the action
            // captured by the old render to the actual callback instead.
            shell.update(cx, |shell, cx| {
                shell.save_presented_session_toggle(
                    &sessions[0].id,
                    0,
                    if field == "session-edit-pin" {
                        Change::Pin(true)
                    } else {
                        Change::Archive(true)
                    },
                    window,
                    cx,
                )
            });
            assert!(!shell
                .update(cx, |shell, _| shell.take_client_commands())
                .iter()
                .any(|command| matches!(command, WireCommand::EditSession { .. })));
            window.render_frame(cx);
            assert!(window
                .find("session-edit-notice")
                .value()
                .unwrap()
                .contains("displayed action changed"));
        });
    }
}

#[gpui_kit::test]
fn archived_scope_and_tag_filtering_do_not_send_actions_or_choose_missing_results(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, handle, &shell);
    sessions[1].archived = true;
    sessions[1].tags = vec!["Work".into(), "ทริป".into()];
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx);
            shell
                .directory_query()
                .update(cx, |query, cx| query.focus(window, cx));
        });
        window.input("work ทริป", cx);
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(shell.read(cx).directory_selected_id(), None);
        window.click("toggle-archived-sessions", cx);
        window.render_frame(cx);
        assert_eq!(
            shell.read(cx).directory_selected_id(),
            Some(sessions[1].id.as_str())
        );
        assert!(window.try_find(directory_row_id(&sessions[1].id)).is_some());
        window.click("toggle-archived-sessions", cx);
        window.render_frame(cx);
        assert_eq!(shell.read(cx).directory_selected_id(), None);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        window.press("enter", cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(
                command,
                WireCommand::Intent(_)
                    | WireCommand::Review { .. }
                    | WireCommand::EditSession { .. }
            )));
        let input = shell.read(cx).directory_query().clone();
        input.update(cx, |input, cx| input.focus(window, cx));
        let bounds = window.find_by_label("Filter Sessions").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input), window)
            .compose_text(None, "かな", None, cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ToggleArchived, window, cx);
        });
        window.render_frame(cx);
        assert!(!window
            .find("directory-status")
            .value()
            .unwrap()
            .contains("Including archived"));
    });
}

#[gpui_kit::test]
fn the_daemon_directory_keeps_a_complete_row_and_reveals_keyboard_choice_at_200_percent(
    cx: &mut TestAppContext,
) {
    let viewport = size(px(480.), px(360.));
    let (handle, shell) = open(cx, viewport, 0, 0);
    let mut sessions = daemon_view(cx, handle, &shell);
    sessions[1].archived = true;
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx);
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
            shell.run_command(Command::ToggleArchived, window, cx);
            shell
                .directory_query()
                .update(cx, |query, cx| query.focus(window, cx));
        });
        window.input(&sessions[1].id, cx);
    });
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        window.render_frame(cx);
        let results = window.find("directory-results").bounds();
        let row = window.find(directory_row_id(&sessions[1].id)).bounds();
        assert!(
            results.size.height >= row.size.height,
            "a full row must fit in the list: {row:?} / {results:?}"
        );
        window.press("down", cx);
        for _ in 0..4 {
            window.render_frame(cx);
            window.simulate_next_frame(cx);
        }
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let row = window.find(directory_row_id(&sessions[1].id)).bounds();
        assert!(
            row.top() >= px(0.) && row.bottom() <= viewport.height,
            "keyboard choice must be visible: {row:?}"
        );
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
    });
}
