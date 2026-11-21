use super::*;
use formless_desktop::client::{Command as Wire, Update};
use formless_desktop::step_decision::{Choice, Dispatch};
use serde_json::json;
fn begin(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    complete: bool,
) -> String {
    let sessions = daemon_view(cx, handle, shell);
    let id = sessions[0].id.clone();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&id), cx);
        let generation = shell.read(cx).client_generation();
        let prompt = json!({"session_id":id,"run_id":"run-native-step","step_id":"native-step","tool_name":"fixture-read","arguments_preview":if complete {" {\"ไทย\":1} ".to_string()}else{"x".repeat(4096)},"arguments_bytes":if complete {" {\"ไทย\":1} ".len()}else{9999},"index":0,"count":2});
        let reply=replies::session_snapshot(&id,&json!({"session":{"id":id},"messages":[],"running":true,"pending_steps":{"prompts":[prompt],"more":false}})).unwrap();
        shell.update(cx, |s, cx| {
            s.take_client_commands();
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: reply,
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        if complete {
            window.click("step-review-open", cx);
            window.render_frame(cx);
            assert!(window.try_find("step-review-dialog").is_some());
            assert_eq!(window.find("step-review-close").focused(), Some(true));
        }
    });
    id
}
fn dispatch(shell: &Entity<Shell>, cx: &mut App) -> Dispatch {
    let commands = shell.update(cx, |s, _| s.take_client_commands());
    let mut ds = commands
        .into_iter()
        .map(|c| match c {
            Wire::DecideStep(d) => d,
            _ => panic!("unexpected command in Step review"),
        })
        .collect::<Vec<_>>();
    assert_eq!(ds.len(), 1);
    ds.pop().unwrap()
}
fn focus(window: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..24 {
        settle_frames(window, cx);
        if window.find(id).focused() == Some(true) {
            return;
        }
        window.press("tab", cx);
    }
    panic!("Step control {id} was not keyboard reachable");
}
#[gpui_kit::test]
fn native_step_default_close_is_safe_and_all_four_choices_are_explicit(cx: &mut TestAppContext) {
    for (button, choice) in [
        ("step-continue", Choice::Continue),
        ("step-skip", Choice::Skip),
        ("step-edit", Choice::Edit(" {\"ไทย\":1} ".into())),
        ("step-all", Choice::All),
    ] {
        let (handle, shell) = open(cx, WIDE, 0, 0);
        begin(cx, handle, &shell, true);
        step(cx, handle, |window, cx| {
            window.press("enter", cx);
            window.render_frame(cx);
            assert!(window.try_find("step-review-dialog").is_none());
            assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
            assert_eq!(window.find("step-review-open").focused(), Some(true));
            window.press("enter", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("step-original-arguments").value(),
                Some(" {\"ไทย\":1} ")
            );
            window.click("step-copy-original", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                " {\"ไทย\":1} "
            );
            window.click(button, cx);
            let d = dispatch(&shell, cx);
            assert_eq!(d.action().choice, choice);
            assert_eq!(d.action().prompt.step_id, "native-step");
            assert!(shell.read(cx).record_step_write(&d));
            shell.update(cx, |s, cx| {
                s.record_step_outcome(
                    &d,
                    json!({"step_id":"native-step","action":choice.wire()}),
                    cx,
                )
            });
            window.render_frame(cx);
            assert!(window
                .find("step-review-status")
                .value()
                .unwrap()
                .contains("not proof of Tool execution"));
            assert_eq!(window.find(button).disabled(), Some(true));
            shell.update(cx, |s, cx| s.submit_step_review(choice.clone(), window, cx));
            assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
            window.render_frame(cx);
            assert!(
                window
                    .find("step-review-status")
                    .value()
                    .unwrap()
                    .contains("Decision accepted"),
                "a repeated callback must not erase the accepted receipt"
            );
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(window.try_find("step-review-dialog").is_none());
            assert_eq!(window.find_by_label("Message").focused(), Some(true));
        });
    }
}
#[gpui_kit::test]
fn native_step_invalid_edit_preedit_and_editor_enter_never_send(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    begin(cx, handle, &shell, true);
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).step_edit_input().unwrap();
        input.update(cx, |i, cx| {
            i.set_value("[]", window, cx);
            i.focus(window, cx)
        });
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        window.click("step-edit", cx);
        window.render_frame(cx);
        assert!(window
            .find("step-review-notice")
            .value()
            .unwrap()
            .contains("JSON object"));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        input.update(cx, |i, cx| {
            i.set_value("{}", window, cx);
            i.focus(window, cx)
        });
        window.render_frame(cx);
        let bounds = window.find_by_label("Edited Step arguments").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input), window)
            .compose_text(None, "かな", None, cx);
        shell.update(cx, |s, cx| {
            s.submit_step_review(Choice::Continue, window, cx)
        });
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("step-review-dialog").is_some());
    });
}
#[gpui_kit::test]
fn native_step_oversized_paste_and_edit_keep_previous_text(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    begin(cx, handle, &shell, true);
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).step_edit_input().unwrap();
        input.update(cx, |i, cx| i.focus(window, cx));
        cx.write_to_clipboard(ClipboardItem::new_string("x".repeat(4097)));
        window.press("cmd-v", cx);
        assert_eq!(input.read(cx).value().as_ref(), " {\"ไทย\":1} ");
        input.update(cx, |i, cx| i.set_value("x".repeat(4097), window, cx));
        window.click("step-edit", cx);
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        assert_eq!(
            shell
                .read(cx)
                .step_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            " {\"ไทย\":1} "
        );
    });
}
#[gpui_kit::test]
fn native_step_truncation_stale_context_and_unknown_write_cannot_be_retried(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    begin(cx, handle, &shell, false);
    step(cx, handle, |window, _| {
        assert!(window.try_find("step-review-open").is_none())
    });
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let id = begin(cx, handle, &shell, true);
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).step_edit_input().unwrap();
        input.update(cx, |i, cx| i.set_value("{\"draft\":2}", window, cx));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "fixture disconnected".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("step-continue").disabled(), Some(true));
        shell.update(cx, |s, cx| {
            s.submit_step_review(Choice::Continue, window, cx)
        });
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        window.click("step-copy-edited", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "{\"draft\":2}"
        );
        window.press("escape", cx);
        window.render_frame(cx);
        shell.update(cx,|s,cx|{s.apply_client_update(Update::Connection{ready:true,notice:"fixture reconnect".into()},window,cx);let mut reply=event_snapshot(&id,"canonical");reply.steps=formless_desktop::steps::Inventory::decode(&id,Some(&json!({"prompts":[{"session_id":id,"run_id":"run-native-step","step_id":"native-step","tool_name":"fixture-read","arguments_preview":" {\"ไทย\":1} ","arguments_bytes":" {\"ไทย\":1} ".len(),"index":0,"count":2}],"more":false})));s.apply_client_update(Update::Snapshot{generation:s.client_generation(),snapshot:reply},window,cx);});
        window.render_frame(cx);
        window.click("step-review-open", cx);
        window.render_frame(cx);
        assert_eq!(
            shell
                .read(cx)
                .step_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "{\"draft\":2}"
        );
        window.click("step-continue", cx);
        let d = dispatch(&shell, cx);
        assert!(shell.read(cx).record_step_write(&d));
        drop(d);
        shell.update(cx, |s, cx| s.poll_client(window, cx));
        window.render_frame(cx);
        assert!(window
            .find("step-review-status")
            .value()
            .unwrap()
            .contains("outcome unknown"));
        assert_eq!(window.find("step-continue").disabled(), Some(true));
        shell.update(cx, |s, cx| {
            s.submit_step_review(Choice::Continue, window, cx)
        });
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}
#[gpui_kit::test]
fn native_step_dialog_traps_focus_and_reveals_controls_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    begin(cx, handle, &shell, true);
    cx.simulate_window_resize(handle.into(), MINIMUM);
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert!(shell.read(cx).background_inert(window));
        assert_eq!(
            shell.update(cx, |s, cx| s.run_command(Command::Questions, window, cx)),
            Some("Close Step review before changing the underlying view")
        );
        for id in [
            "step-copy-original",
            "step-copy-edited",
            "step-continue",
            "step-skip",
            "step-edit",
            "step-all",
        ] {
            focus(window, cx, id);
            let b = window.find(id).bounds();
            let body = window.find("step-review-body").bounds();
            assert!(
                b.top() >= body.top() && b.bottom() <= body.bottom(),
                "{id}: {b:?}/{body:?}"
            );
        }
        focus(window, cx, "step-review-close");
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(!shell.read(cx).background_inert(window));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn native_step_profile_replacement_retires_review_and_does_not_reuse_edited_arguments(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    begin(cx, handle, &shell, true);
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).step_edit_input().unwrap();
        input.update(cx, |i, cx| {
            i.set_value("{\"old_profile_edit\":1}", window, cx)
        });
        window.click("step-continue", cx);
        let old = dispatch(&shell, cx);
        shell.update(cx, |s, cx| s.connect_recording_daemon(cx));
        window.render_frame(cx);
        assert!(window.try_find("step-review-dialog").is_none());
        assert!(!shell.read(cx).background_inert(window));
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        drop(old);
    });
    begin(cx, handle, &shell, true);
    step(cx, handle, |window, cx| {
        assert_eq!(
            shell
                .read(cx)
                .step_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            " {\"ไทย\":1} "
        );
        assert!(window
            .find("step-original-arguments")
            .value()
            .unwrap()
            .contains("ไทย"));
    });
}
