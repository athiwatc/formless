use super::*;
use formless_desktop::client::Update;
use formless_desktop::steps::Inventory;
use serde_json::json;
fn snapshot(id: &str, waiting: bool) -> replies::SessionSnapshot {
    let mut s = event_snapshot(id, "same canonical content");
    s.steps = Inventory::decode(
        id,
        Some(
            &json!({"prompts":if waiting {vec![json!({"session_id":id,"run_id":"run-owner","step_id":"step-owner","tool_name":"fixture-read","arguments_preview":"{}","arguments_bytes":2,"index":0,"count":1})]}else{vec![]},"more":false}),
        ),
    );
    s
}
#[gpui_kit::test]
fn step_inventory_restores_without_events_and_retires_on_resolution_selection_and_disconnect(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let id = sessions[0].id.clone();
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |s, cx| {
            s.take_client_commands();
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: snapshot(&id, true),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        let text = window
            .find("daemon-step-inventory")
            .value()
            .unwrap()
            .to_string();
        assert!(
            text.contains("Step step-owner")
                && text.contains("Run run-owner")
                && text.contains("read only")
        );
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.input("draft ไทย", cx);
        // Same canonical text, changed inventory: list reuse must not retain the old prompt.
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: snapshot(&id, false),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("daemon-step-inventory").is_none());
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft ไทย");
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Snapshot {
                    generation: generation - 1,
                    snapshot: snapshot(&id, true),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("daemon-step-inventory").is_none());
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: snapshot(&id, true),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("daemon-step-inventory").is_some());
        window.click(session_row_id(&sessions[1].id), cx);
        window.render_frame(cx);
        assert!(window.try_find("daemon-step-inventory").is_none());
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: snapshot(&id, true),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("daemon-step-inventory").is_none());
        let id = sessions[1].id.clone();
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot: snapshot(&id, true),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("daemon-step-inventory").is_some());
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "fixture disconnect".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("daemon-step-inventory").is_none());
        assert!(
            shell
                .update(cx, |s, _| s.take_client_commands())
                .into_iter()
                .all(|c| matches!(c, formless_desktop::client::Command::Select { .. })),
            "displaying inventories must never enqueue a decision or other mutation"
        );
    });
}

#[gpui_kit::test]
fn step_inventory_long_preview_keeps_composer_usable_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let id = sessions[0].id.clone();
        let generation = shell.read(cx).client_generation();
        let mut reply = snapshot(&id, true);
        if let Inventory::Snapshot(s) = &mut reply.steps {
            s.prompts[0].arguments_preview = "ก".repeat(1365);
            s.prompts[0].arguments_bytes = 9999;
        }
        shell.update(cx, |s, cx| {
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
        window.click_at("region-composer", point(px(40.), px(20.)), cx);
        window.input("draft survives", cx);
    });
    cx.simulate_window_resize(handle.into(), MINIMUM);
    set_text_scale(cx, handle, &shell, 200);
    step(cx, handle, |window, cx| {
        settle_frames(window, cx);
        assert_composer_in_view(window, MINIMUM, "restored Step preview at 200%");
        assert!(window.find("daemon-step-inventory").bounds().size.height <= px(120.));
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "draft survives");
        let generation = shell.read(cx).client_generation();
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::SnapshotError {
                    generation,
                    session_id: sessions[0].id.clone(),
                    error: "fixture retired read".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("daemon-step-inventory").is_none());
    });
}
