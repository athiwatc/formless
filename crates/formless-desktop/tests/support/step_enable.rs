use super::*;
use formless_desktop::client::{Command as Wire, Update};
use formless_desktop::step_mode::{Dispatch, Outcome, Status};
use serde_json::json;
fn show(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    id: &str,
    mode: Option<bool>,
) {
    step(cx, handle, |window, cx| {
        let mut value = json!({"session":{"id":id},"messages":[],"running":false});
        if let Some(mode) = mode {
            value["step_mode"] = json!(mode);
        }
        let snapshot = replies::session_snapshot(id, &value).unwrap();
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot,
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
    });
}
fn setup(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Shell>, String) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let id = sessions[0].id.clone();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&id), cx);
        shell.update(cx, |s, _| s.take_client_commands());
    });
    show(cx, handle, &shell, &id, Some(false));
    (handle, shell, id)
}
fn take(shell: &Entity<Shell>, cx: &mut App) -> Dispatch {
    let mut commands = shell.update(cx, |s, _| s.take_client_commands());
    assert_eq!(commands.len(), 1);
    match commands.remove(0) {
        Wire::EnableStepMode(d) => d,
        _ => panic!("unexpected command"),
    }
}
#[gpui_kit::test]
fn step_mode_enable_preserves_draft_and_never_reuses_a_spent_callback(cx: &mut TestAppContext) {
    let (handle, shell, id) = setup(cx);
    let (action, composer) = step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent ไทย 😀", cx);
        let action = shell.read(cx).step_mode_enable_target().unwrap();
        let input = shell.read(cx).composer().clone();
        shell.update(cx, |s, cx| s.enable_mode(action.clone(), window, cx));
        (action, input)
    });
    let first = step(cx, handle, |window, cx| {
        let d = take(&shell, cx);
        assert_eq!(d.action(), &action);
        shell.update(cx, |s, cx| s.enable_mode(action.clone(), window, cx));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert_eq!(shell.read(cx).composer(), &composer);
        assert_eq!(composer.read(cx).value().as_ref(), "Unsent ไทย 😀");
        shell.update(cx, |s, cx| s.finish_recorded_mode(&d, Outcome::Enabled, cx));
        d
    });
    // A fresh sample can show Off after another client changed it. An old
    // callback must not turn it back On; an explicit new operation may.
    show(cx, handle, &shell, &id, Some(false));
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| s.enable_mode(action, window, cx));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        let fresh = shell.read(cx).step_mode_enable_target().unwrap();
        shell.update(cx, |s, cx| s.enable_mode(fresh.clone(), window, cx));
        let next = take(&shell, cx);
        drop(first);
        assert!(matches!(shell.read(cx).mode_enable_status(),Status::Reserved(a) if a==fresh));
        drop(next);
        assert_eq!(composer.read(cx).value().as_ref(), "Unsent ไทย 😀");
    });
}
#[gpui_kit::test]
fn step_mode_enable_refuses_unsupported_scope_and_background_modal(cx: &mut TestAppContext) {
    let (handle, shell, id) = setup(cx);
    let old = step(cx, handle, |_, cx| {
        shell.read(cx).step_mode_enable_target().unwrap()
    });
    show(cx, handle, &shell, &id, None);
    step(cx, handle, |window, cx| {
        assert!(shell.read(cx).step_mode_enable_target().is_none());
        shell.update(cx, |s, cx| s.enable_mode(old.clone(), window, cx));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
    show(cx, handle, &shell, &id, Some(false));
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).composer().clone();
        input.update(cx, |input, cx| input.focus(window, cx));
        window.render_frame(cx);
        let bounds = window.find_by_label("Message").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input.clone()), window)
            .compose_text(None, "かな", None, cx);
        shell.update(cx, |s, cx| s.enable_mode(old.clone(), window, cx));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        assert!(input.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        input.update(cx, |input, cx| input.unmark_text(window, cx));
        window.press(palette_key(), cx);
        window.render_frame(cx);
        shell.update(cx, |s, cx| s.enable_mode(old.clone(), window, cx));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        window.press("escape", cx);
        shell.update(cx, |s, cx| s.connect_recording_daemon(cx));
        shell.update(cx, |s, cx| s.enable_mode(old, window, cx));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}
#[gpui_kit::test]
fn step_mode_enable_is_keyboard_reachable_at_minimum_window_200_percent(cx: &mut TestAppContext) {
    for key in ["space", "enter"] {
        let (handle, shell, _) = setup(cx);
        cx.simulate_window_resize(handle.into(), MINIMUM);
        step(cx, handle, |window, cx| {
            shell.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, window, cx));
            for _ in 0..60 {
                settle_frames(window, cx);
                if window.find("step-mode-enable").focused() == Some(true) {
                    break;
                }
                window.press("tab", cx);
            }
            assert_eq!(window.find("step-mode-enable").focused(), Some(true));
            assert!(window.find("step-mode-enable").visible());
            window.press(key, cx);
        });
        step(cx, handle, |window, cx| {
            settle_frames(window, cx);
            let _ = take(&shell, cx);
            assert_composer_in_view(window, MINIMUM, "after enabling Step mode");
        });
    }
}
#[gpui_kit::test]
fn step_mode_enable_and_v1_policy_leave_history_space_at_200_percent(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = fixture::sessions(1);
    let id = sessions[0].id.clone();
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.connect_recording_future_attempts(cx);
            s.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "future fixture".into(),
                },
                window,
                cx,
            );
            s.apply_client_update(Update::Sessions(sessions), window, cx);
        });
        window.render_frame(cx);
        window.click(session_row_id(&id), cx);
        shell.update(cx, |s, _| s.take_client_commands());
    });
    show(cx, handle, &shell, &id, Some(false));
    cx.simulate_window_resize(handle.into(), MINIMUM);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| s.set_text_scale(TextScale::LARGEST, window, cx));
        settle_frames(window, cx);
        assert!(
            window.find("timeline-area").bounds().size.height > px(0.),
            "mode controls must not consume all history space"
        );
        assert!(window.find("step-mode-help").bounds().right() <= MINIMUM.width);
        assert_fitted(window, &shell, MINIMUM, cx, "mode and v1 policy");
    });
}
