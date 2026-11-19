use super::*;
use formless_desktop::client::{Command as Wire, Update};
use formless_desktop::session_edit::{Change, Metadata, Outcome};
use gpui_kit::Focusable;

#[gpui_kit::test]
fn composer_model_control_opens_the_current_session_and_preserves_the_draft(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let mut current = sessions[0].clone();
    current.model = Some("role:reviewer".into());
    current.reasoning_effort = Some("low".into());
    let composer = step(cx, handle, |window, cx| {
        window.click(session_row_id(&current.id), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Keep this draft ไทย 😀", cx);
        window.press("shift-left", cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        shell.read(cx).composer().clone()
    });
    cx.simulate_window_resize(handle.into(), MINIMUM);
    step(cx, handle, |window, cx| {
        let selection = shell.read(cx).composer_selection(cx);
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx)
        });
        settle_frames(window, cx);
        tab_to(window, "composer-model", cx);
        settle_frames(window, cx);
        assert!(window.find("composer-model").visible());
        assert_eq!(window.find("composer-model").label(), Some("Model or Role"));
        assert_eq!(
            shell.read(cx).focused_section(window, cx),
            Some(PageSection::Composer)
        );
        window.press("enter", cx);
        settle_frames(window, cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        let [Wire::ReadSessionDetails {
            connection,
            operation,
            session_id,
        }] = commands.as_slice()
        else {
            panic!(
                "expected one metadata read, got {} commands",
                commands.len()
            );
        };
        assert_eq!(session_id, &current.id);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionDetails {
                    connection: *connection,
                    operation: *operation,
                    session_id: session_id.clone(),
                    result: Ok(Metadata::from_session(&current).unwrap()),
                },
                window,
                cx,
            )
        });
        settle_frames(window, cx);
        assert!(window
            .find("session-model-confirmed")
            .value()
            .unwrap()
            .contains("role:reviewer"));
        assert_eq!(
            shell
                .read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "role:reviewer"
        );
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        window.press("escape", cx);
        settle_frames(window, cx);
        assert_eq!(window.find("composer-model").focused(), Some(true));
        assert_eq!(shell.read(cx).composer(), &composer);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "Keep this draft ไทย 😀");
        assert_eq!(shell.read(cx).composer_selection(cx), selection);
        assert!(shell.read(cx).last_intent().is_none());
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        window.press("enter", cx);
        shell.update(cx, |shell, cx| {
            shell.take_client_commands();
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected".into(),
                },
                window,
                cx,
            );
        });
        window.press("escape", cx);
        settle_frames(window, cx);
        assert!(composer.focus_handle(cx).contains_focused(window, cx));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "Keep this draft ไทย 😀");
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
    });
}

#[gpui_kit::test]
fn composer_model_palette_reuses_the_chat_and_composition_guards(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.render_frame(cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        let composer = shell.read(cx).composer().clone();
        let bounds = window.find_by_label("Message").bounds();
        TestInput::new(ElementInputHandler::new(bounds, composer.clone()), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
        assert_eq!(
            shell.update(cx, |shell, cx| {
                shell.run_command(Command::SessionModel, window, cx)
            }),
            Some(commands::COMPOSING)
        );
        assert!(composer.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        composer.update(cx, |input, cx| input.unmark_text(window, cx));

        window.press(palette_key(), cx);
        window.render_frame(cx);
        window.input("review model role", cx);
        window.render_frame(cx);
        assert_eq!(palette_titles(window), ["Review Session model or Role"]);
        assert!(shell
            .update(cx, |shell, cx| {
                shell.run_command(Command::SessionModel, window, cx)
            })
            .is_some());
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(window.find("session-edit-cancel").visible());
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert!(
            matches!(commands.as_slice(), [Wire::ReadSessionDetails { session_id, .. }] if session_id == &sessions[0].id)
        );
        window.press("escape", cx);
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "にほんご");
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx)
        });
        shell.update(cx, |shell, _| shell.take_client_commands());
        assert_eq!(
            shell.update(cx, |shell, cx| {
                shell.run_command(Command::SessionModel, window, cx)
            }),
            Some(commands::NO_COMPOSER)
        );
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
    });
}

fn setup(
    cx: &mut TestAppContext,
) -> (
    WindowHandle<Root>,
    Entity<Shell>,
    Vec<formless_protocol::SessionInfo>,
) {
    let (h, s) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, h, &s);
    sessions[1].model = Some("old-model".into());
    sessions[1].reasoning_effort = Some("low".into());
    step(cx, h, |window, cx| {
        window.click(session_row_id(&sessions[0].id), cx);
        s.update(cx, |s, cx| {
            s.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Composer remains ไทย 😀", cx);
    });
    super::session_edit_tests::show(cx, h, &s, &sessions[1]);
    (h, s, sessions)
}
fn saved(s: &Entity<Shell>, cx: &mut App) -> (uuid::Uuid, Change) {
    let commands = s.update(cx, |s, _| s.take_client_commands());
    assert_eq!(commands.len(), 1);
    let Wire::EditSession {
        operation, change, ..
    } = commands.into_iter().next().unwrap()
    else {
        panic!("expected Session setter")
    };
    (operation, change)
}
fn ack(
    s: &Entity<Shell>,
    window: &mut Window,
    cx: &mut App,
    session: &formless_protocol::SessionInfo,
    op: uuid::Uuid,
) {
    s.update(cx, |s, cx| {
        s.apply_client_update(
            Update::SessionEdited {
                connection: 0,
                operation: op,
                session_id: session.id.clone(),
                outcome: Outcome::Applied(Metadata::from_session(session).unwrap()),
            },
            window,
            cx,
        )
    });
}
#[gpui_kit::test]
fn session_model_role_and_effort_preserve_confirmed_companion_unsaved_fields_and_composer(
    cx: &mut TestAppContext,
) {
    let (h, s, mut sessions) = setup(cx);
    step(cx, h, |window, cx| {
        let composer = s.read(cx).composer().clone();
        let original = s.read(cx).session_edit_input().unwrap();
        original.update(cx, |i, cx| i.set_value("Unsaved name", window, cx));
        window.click("session-edit-effort", cx);
        s.read(cx)
            .session_edit_input()
            .unwrap()
            .update(cx, |i, cx| i.set_value("high", window, cx));
        window.click("session-edit-model", cx);
        assert!(window
            .find("session-model-cost")
            .value()
            .unwrap()
            .contains("does not run a Provider test"));
        assert!(window.find("session-model-paid-footer").visible());
        let input = s.read(cx).session_edit_input().unwrap();
        input.update(cx, |i, cx| i.set_value(" role:reviewer ", window, cx));
        let revision = s.read(cx).session_edit_revision().unwrap();
        window.click("session-edit-save", cx);
        let (op, change) = saved(&s, cx);
        assert!(
            matches!(change.request(&sessions[1].id),formless_protocol::Request::CompareSetSessionModel{id,desired,..}if id==sessions[1].id&&desired.model.as_deref()==Some("role:reviewer")&&desired.reasoning_effort.as_deref()==Some("low"))
        );
        sessions[1].model = Some("role:reviewer".into());
        ack(&s, window, cx, &sessions[1], op);
        s.update(cx, |s, cx| {
            s.save_presented_session_selection(revision, window, cx)
        });
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        window.click("session-edit-effort", cx);
        assert_eq!(
            s.read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "high"
        );
        window.click("session-edit-save", cx);
        let (_op, change) = saved(&s, cx);
        assert!(
            matches!(change.request(&sessions[1].id),formless_protocol::Request::CompareSetSessionModel{desired,..}if desired.model.as_deref()==Some("role:reviewer")&&desired.reasoning_effort.as_deref()==Some("high"))
        );
        assert_eq!(s.read(cx).composer(), &composer);
        assert_eq!(
            composer.read(cx).value().as_ref(),
            "Composer remains ไทย 😀"
        );
        assert_eq!(
            s.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
    });
}
#[gpui_kit::test]
fn session_model_unavailable_pair_is_never_a_clear_and_draft_is_copyable(cx: &mut TestAppContext) {
    let (h, s) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, h, &s);
    sessions[1].model = Some("x".repeat(formless_desktop::session_edit::MAX_MODEL_BYTES + 1));
    super::session_edit_tests::show(cx, h, &s, &sessions[1]);
    step(cx, h, |window, cx| {
        window.click("session-edit-model", cx);
        assert_eq!(window.find("session-edit-save").disabled(), Some(true));
        assert!(window
            .find("session-model-unavailable")
            .value()
            .unwrap()
            .contains("disabled"));
        s.read(cx)
            .session_edit_input()
            .unwrap()
            .update(cx, |i, cx| i.set_value("draft retained", window, cx));
        window.click("session-edit-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "draft retained"
        );
        let rev = s.read(cx).session_edit_revision().unwrap();
        s.update(cx, |s, cx| {
            s.save_presented_session_selection(rev, window, cx)
        });
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}
#[gpui_kit::test]
fn session_model_composition_and_reloaded_form_revision_reject_stale_saves(
    cx: &mut TestAppContext,
) {
    let (h, s, sessions) = setup(cx);
    step(cx, h, |window, cx| {
        window.click("session-edit-model", cx);
        let input = s.read(cx).session_edit_input().unwrap();
        input.update(cx, |i, cx| i.focus(window, cx));
        window.render_frame(cx);
        let bounds = window.find_by_label("Session model or Role").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input.clone()), window)
            .compose_text(None, "かな", None, cx);
        window.click("session-edit-save", cx);
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
        window.click("session-edit-effort", cx);
        assert_eq!(s.read(cx).session_edit_input(), Some(input.clone()));
        assert!(input.update(cx, |i, cx| i.marked_text_range(window, cx).is_some()));
        input.update(cx, |i, cx| i.unmark_text(window, cx));
        let old = s.read(cx).session_edit_revision().unwrap();
        window.click("session-edit-reload", cx);
        let commands = s.update(cx, |s, _| s.take_client_commands());
        let Wire::ReadSessionDetails { operation, .. } = &commands[0] else {
            panic!("expected explicit reload")
        };
        s.update(cx, |s, cx| {
            s.apply_client_update(
                Update::SessionDetails {
                    connection: 0,
                    operation: *operation,
                    session_id: sessions[1].id.clone(),
                    result: Ok(Metadata::from_session(&sessions[1]).unwrap()),
                },
                window,
                cx,
            )
        });
        s.update(cx, |s, cx| {
            s.save_presented_session_selection(old, window, cx)
        });
        assert!(s.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}
#[gpui_kit::test]
fn session_model_form_and_cost_notice_are_reachable_at_200_percent_minimum_window(
    cx: &mut TestAppContext,
) {
    let (h, s, _) = setup(cx);
    cx.simulate_window_resize(h.into(), MINIMUM);
    step(cx, h, |window, cx| {
        s.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, window, cx));
        for _ in 0..80 {
            settle_frames(window, cx);
            if window.find("session-edit-model").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("session-edit-model").focused(), Some(true));
        assert!(window.find("session-edit-model").visible());
        window.press("space", cx);
        settle_frames(window, cx);
        let input = s.read(cx).session_edit_input().unwrap();
        input.update(cx, |i, cx| i.set_value("role:reviewer", window, cx));
        for _ in 0..80 {
            settle_frames(window, cx);
            if input.focus_handle(cx).contains_focused(window, cx) {
                break;
            }
            window.press("tab", cx);
        }
        assert!(input.focus_handle(cx).contains_focused(window, cx));
        assert!(window.find_by_label("Session model or Role").visible());
        assert!(window.find("session-model-paid-footer").visible());
        for _ in 0..80 {
            settle_frames(window, cx);
            if window.find("session-edit-save").focused() == Some(true) {
                break;
            }
            window.press("tab", cx);
        }
        assert_eq!(window.find("session-edit-save").focused(), Some(true));
        assert!(window.find("session-edit-save").visible());
        assert!(window.find("session-model-paid-footer").visible());
        window.press("enter", cx);
        let (_op, change) = saved(&s, cx);
        assert!(change.is_selection());
    });
}

#[gpui_kit::test]
fn session_model_profile_replacement_with_reused_ids_and_connection_needs_explicit_reload(
    cx: &mut TestAppContext,
) {
    for sent in [false, true] {
        let (h, s, _) = setup(cx);
        let old = step(cx, h, |window, cx| {
            window.click("session-edit-model", cx);
            s.read(cx)
                .session_edit_input()
                .unwrap()
                .update(cx, |i, cx| i.set_value("role:reviewer", window, cx));
            let revision = s.read(cx).session_edit_revision().unwrap();
            if sent {
                window.click("session-edit-save", cx);
                let _ = saved(&s, cx);
            }
            revision
        });
        let mut other = daemon_view(cx, h, &s);
        other[1].model = Some("old-model".into());
        other[1].reasoning_effort = Some("low".into());
        step(cx, h, |window, cx| {
            s.update(cx, |s, cx| {
                s.apply_client_update(Update::Sessions(other.clone()), window, cx)
            });
            s.update(cx, |s, _| s.take_client_commands());
            if sent {
                assert!(window
                    .find("session-edit-notice")
                    .value()
                    .unwrap()
                    .contains("save outcome is unknown"));
            }
            s.update(cx, |s, cx| {
                s.save_presented_session_selection(old, window, cx)
            });
            let current = s.read(cx).session_edit_revision().unwrap();
            s.update(cx, |s, cx| {
                s.save_presented_session_selection(current, window, cx)
            });
            assert!(s.update(cx,|s,_|s.take_client_commands()).is_empty(),"no model save may cross Profile attachment even when IDs and connection counters match");
            assert_eq!(
                s.read(cx)
                    .session_edit_input()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                "role:reviewer"
            );
            assert_eq!(window.find("session-edit-save").disabled(), Some(true));
            window.click("session-edit-reload", cx);
            let commands = s.update(cx, |s, _| s.take_client_commands());
            let Wire::ReadSessionDetails {
                operation,
                session_id,
                ..
            } = &commands[0]
            else {
                panic!("explicit reload required")
            };
            assert_eq!(session_id, &other[1].id);
            s.update(cx, |s, cx| {
                s.apply_client_update(
                    Update::SessionDetails {
                        connection: 0,
                        operation: *operation,
                        session_id: other[1].id.clone(),
                        result: Ok(Metadata::from_session(&other[1]).unwrap()),
                    },
                    window,
                    cx,
                )
            });
            assert_eq!(
                s.read(cx)
                    .session_edit_input()
                    .unwrap()
                    .read(cx)
                    .value()
                    .as_ref(),
                "old-model"
            );
            s.read(cx)
                .session_edit_input()
                .unwrap()
                .update(cx, |i, cx| i.set_value("role:new-profile", window, cx));
            window.click("session-edit-save", cx);
            let (_op, c) = saved(&s, cx);
            assert!(
                matches!(c.request(&other[1].id),formless_protocol::Request::CompareSetSessionModel{desired,..}if desired.model.as_deref()==Some("role:new-profile"))
            );
        });
    }
}
