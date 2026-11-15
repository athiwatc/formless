use super::*;
use formless_desktop::client::{Command as Wire, Update};
use formless_desktop::session_edit::{Change, Metadata, Outcome};
use gpui_kit::Focusable;
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
            .contains("paid background model test"));
        assert!(window.find("session-model-paid-footer").visible());
        let input = s.read(cx).session_edit_input().unwrap();
        input.update(cx, |i, cx| i.set_value(" role:reviewer ", window, cx));
        let revision = s.read(cx).session_edit_revision().unwrap();
        window.click("session-edit-save", cx);
        let (op, change) = saved(&s, cx);
        assert!(
            matches!(change.request(&sessions[1].id),formless_protocol::Request::SetSessionModel{id,model:Some(m),reasoning_effort:Some(e)}if id==sessions[1].id&&m=="role:reviewer"&&e=="low")
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
            matches!(change.request(&sessions[1].id),formless_protocol::Request::SetSessionModel{model:Some(m),reasoning_effort:Some(e),..}if m=="role:reviewer"&&e=="high")
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
fn session_model_form_and_paid_test_notice_are_reachable_at_200_percent_minimum_window(
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
                matches!(c.request(&other[1].id),formless_protocol::Request::SetSessionModel{model:Some(m),..}if m=="role:new-profile")
            );
        });
    }
}
