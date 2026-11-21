use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::invocations::Report;
use serde_json::{json, Value};
fn value() -> Value {
    json!([
        {"invocation_id":"invocation-a","handle":"handle-a","tool_name":"Tool ไทย 😀","session_id":"fixture-session-1","elapsed_ms":3000,"silent_ms":1500,"stalled":true,"abandoned":true,"last_fraction":0.25,"last_label":format!("  private ไทย 😀\r\n{}  ","exact label line\n".repeat(60)),"future":{"private":"preserved"}},
        {"handle":"legacy-handle-b","tool_name":"Tool B","session_id":"fixture-session-2","elapsed_ms":1200,"silent_ms":900,"stalled":false,"abandoned":false}
    ])
}
fn requested(shell: &Entity<Shell>, cx: &mut App) -> uuid::Uuid {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    let mut operations = vec![];
    for command in commands {
        match command {
            WireCommand::ReadInvocations {
                connection: 0,
                operation,
            } => operations.push(operation),
            WireCommand::Page(Destination::Invocations) => {}
            _ => panic!("unexpected wire command while reading Invocations"),
        }
    }
    assert_eq!(operations.len(), 1);
    operations[0]
}
fn view(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> uuid::Uuid {
    shell.update(cx, |shell, _| shell.take_client_commands());
    shell.update(cx, |shell, cx| {
        shell.run_command(Command::ViewInvocations, window, cx)
    });
    assert_eq!(shell.read(cx).destination(), Destination::Invocations);
    requested(shell, cx)
}
fn reply(
    shell: &Entity<Shell>,
    connection: u64,
    operation: uuid::Uuid,
    value: Value,
    window: &mut Window,
    cx: &mut App,
) {
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::Invocations {
                connection,
                operation,
                result: Report::decode(value),
            },
            window,
            cx,
        )
    });
    window.render_frame(cx);
}
fn choose_first(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| {
        shell.focus_section(PageSection::Conversation, window, cx)
    });
    window.press("down", cx);
    window.render_frame(cx);
    assert_eq!(shell.read(cx).resource_copy_target().unwrap().2, 1);
}
#[gpui_kit::test]
fn invocations_are_explicit_read_only_snapshots_and_copy_exact_private_future_json(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Tools), window, cx)
        });
        shell.update(cx, |shell, _| shell.take_client_commands());
        window.render_frame(cx);
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "merely viewing Tools reads no Invocations"
        );
        window.click("view-invocations", cx);
        let operation = requested(&shell, cx);
        reply(&shell, 0, operation, value(), window, cx);
        let scope = shell.read(cx).resource_copy_target().unwrap();
        assert!(shell.update(cx, |shell, cx| shell
            .copy_resource_details(scope, window, cx)));
        assert_eq!(
            serde_json::from_str::<Value>(&cx.read_from_clipboard().unwrap().text().unwrap())
                .unwrap(),
            value()
        );
        assert!(window
            .find("resource-notice")
            .value()
            .unwrap()
            .contains("not a running clock"));
        assert!(window
            .find("resource-notice")
            .value()
            .unwrap()
            .contains("lack a durable"));
        choose_first(&shell, window, cx);
        let first = shell.read(cx).resource_copy_target().unwrap();
        assert!(shell.update(cx, |shell, cx| shell
            .copy_resource_details(first, window, cx)));
        assert_eq!(
            serde_json::from_str::<Value>(&cx.read_from_clipboard().unwrap().text().unwrap())
                .unwrap(),
            value()[0]
        );
        assert!(window
            .find("resource-detail")
            .value()
            .unwrap()
            .contains("Preview limited"));
        assert!(window
            .find("resource-selected")
            .value()
            .unwrap()
            .contains("stalled: true"));
        window.press("down", cx);
        window.render_frame(cx);
        assert!(window
            .find("resource-detail")
            .value()
            .unwrap()
            .contains("not returned (legacy handle only)"));
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(first, window, cx)));
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
    });
}
#[gpui_kit::test]
fn invocation_navigation_opens_only_the_captured_available_session_and_preserves_drafts(
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
        let operation = view(&shell, window, cx);
        reply(&shell, 0, operation, value(), window, cx);
        choose_first(&shell, window, cx);
        let source = shell.read(cx).invocation_presented().unwrap();
        let revision = shell.read(cx).invocations_revision();
        window.press("down", cx);
        shell.update(cx, |shell, cx| {
            shell.open_presented_invocation_session(source.clone(), revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        window.press("up", cx);
        window.render_frame(cx);
        let old_copy = shell.read(cx).resource_copy_target().unwrap();
        window.click("open-invocation-session", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[1].id.as_str())
        );
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        assert_eq!(commands.len(), 1);
        assert!(
            matches!(&commands[0],WireCommand::Select{session_id:Some(id),..} if id==&sessions[1].id)
        );
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(old_copy, window, cx)));
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        assert_eq!(
            shell.read(cx).composer().read(cx).value().as_ref(),
            "Unsent A ไทย 😀"
        );
    });
}
#[gpui_kit::test]
fn invocation_snapshot_refresh_and_callback_ownership_reject_stale_and_spoofed_data(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let operation = view(&shell, window, cx);
        reply(&shell, 1, operation, value(), window, cx);
        reply(&shell, 0, uuid::Uuid::new_v4(), value(), window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Invocations,
                    result: Ok(value()),
                },
                window,
                cx,
            )
        });
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, 0, operation, value(), window, cx);
        choose_first(&shell, window, cx);
        let source = shell.read(cx).invocation_presented().unwrap();
        let revision = shell.read(cx).invocations_revision();
        let old_copy = shell.read(cx).resource_copy_target().unwrap();
        window.click("refresh-resource", cx);
        let fresh = requested(&shell, cx);
        shell.update(cx, |shell, cx| {
            shell.open_presented_invocation_session(source.clone(), revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(old_copy, window, cx)));
        reply(&shell, 0, operation, value(), window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, 0, fresh, value(), window, cx);
        choose_first(&shell, window, cx);
        shell.update(cx, |shell, cx| {
            shell.open_presented_invocation_session(source, revision, window, cx)
        });
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "a handle reused by a newer snapshot cannot revive its old button"
        );
        let source = shell.read(cx).invocation_presented().unwrap();
        let revision = shell.read(cx).invocations_revision();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ShowCommands, window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.open_presented_invocation_session(source.clone(), revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        window.render_frame(cx);
        window.press("escape", cx);
        window.click("resource-back", cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        assert_eq!(shell.read(cx).destination(), Destination::Tools);
        shell.update(cx, |shell, cx| {
            shell.open_presented_invocation_session(source, revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
    });
}
#[gpui_kit::test]
fn invocations_retire_connection_attachment_missing_sessions_and_error_state(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let operation = view(&shell, window, cx);
        reply(&shell, 0, operation, value(), window, cx);
        choose_first(&shell, window, cx);
        let source = shell.read(cx).invocation_presented().unwrap();
        let revision = shell.read(cx).invocations_revision();
        sessions[1].archived = true;
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.open_presented_invocation_session(source.clone(), revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        assert_eq!(shell.read(cx).retire_recording_question_connection(), 1);
        shell.update(cx, |shell, cx| {
            shell.open_presented_invocation_session(source, revision, window, cx);
            shell.refresh_presented_invocations(revision, 0, window, cx);
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "offline".into(),
                },
                window,
                cx,
            )
        });
        assert!(shell.read(cx).resource_copy_target().is_none());
        shell.update(cx, |shell, cx| shell.connect_recording_daemon(cx));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "new attachment".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
        });
        reply(&shell, 0, operation, value(), window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        let fresh = view(&shell, window, cx);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Invocations {
                    connection: 0,
                    operation: fresh,
                    result: Err("private read unavailable".into()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window
            .find("resource-notice")
            .value()
            .unwrap()
            .contains("private read unavailable"));
        assert!(shell.read(cx).resource_copy_target().is_none());
        window.click("refresh-resource", cx);
        let operation = requested(&shell, cx);
        reply(&shell, 0, operation, json!([]), window, cx);
        assert!(shell.read(cx).invocation_presented().is_none());
        let target = shell.read(cx).resource_copy_target().unwrap();
        shell.update(cx, |shell, cx| {
            shell.copy_resource_details(target, window, cx)
        });
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "[]");
    });
}
fn focus_control(id: &'static str, window: &mut Window, cx: &mut App) {
    let mut found = false;
    for _ in 0..90 {
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
fn invocations_are_keyboard_reachable_at_minimum_size_and_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx);
            shell.run_command(Command::GoTo(Destination::Tools), window, cx);
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Tools,
                    result: Ok(json!([])),
                },
                window,
                cx,
            );
            shell.focus_section(PageSection::Conversation, window, cx);
            shell.take_client_commands();
        });
        focus_control("view-invocations", window, cx);
        window.press("enter", cx);
        let operation = requested(&shell, cx);
        reply(&shell, 0, operation, value(), window, cx);
        for id in ["region-conversation", "resource-notice", "resource-detail"] {
            assert!(window.find(id).bounds().size.width <= px(480.), "{id}");
        }
        focus_control("copy-resource", window, cx);
        window.press("enter", cx);
        assert_eq!(
            serde_json::from_str::<Value>(&cx.read_from_clipboard().unwrap().text().unwrap())
                .unwrap(),
            value()
        );
        choose_first(&shell, window, cx);
        focus_control("open-invocation-session", window, cx);
        window.press("enter", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Chat);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[1].id.as_str())
        );
    });
}

#[gpui_kit::test]
fn invocation_sidebar_activation_retires_pending_and_loaded_snapshots(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let old = view(&shell, window, cx);
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        assert!(
            shell
                .update(cx, |shell, cx| shell.run_command(
                    Command::ViewInvocations,
                    window,
                    cx
                ))
                .is_none(),
            "sidebar navigation must retire the old pending read"
        );
        let current = requested(&shell, cx);
        reply(&shell, 0, old, value(), window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, 0, current, value(), window, cx);
        window.render_frame(cx);
        window.click(session_row_id(&sessions[1].id), cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Invocations), window, cx)
        });
        assert!(
            shell.read(cx).resource_copy_target().is_none(),
            "leaving through the sidebar drops the loaded page too"
        );
        reply(&shell, 0, current, value(), window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        assert!(
            !shell
                .update(cx, |shell, _| shell.take_client_commands())
                .iter()
                .any(|command| matches!(command, WireCommand::ReadInvocations { .. })),
            "restoring a destination is not an automatic read"
        );
    });
}

#[gpui_kit::test]
fn invocation_refresh_and_open_preserve_composer_preedit(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let operation = view(&shell, window, cx);
        reply(&shell, 0, operation, value(), window, cx);
        choose_first(&shell, window, cx);
        let source = shell.read(cx).invocation_presented().unwrap();
        let revision = shell.read(cx).invocations_revision();
        let composer = shell.read(cx).composer().clone();
        let bounds = window.find("region-conversation").bounds();
        TestInput::new(ElementInputHandler::new(bounds, composer.clone()), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
        shell.update(cx, |shell, cx| {
            shell.refresh_presented_invocations(revision, 0, window, cx);
            shell.open_presented_invocation_session(source, revision, window, cx);
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        assert_eq!(shell.read(cx).destination(), Destination::Invocations);
        assert!(composer.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        composer.update(cx, |input, cx| input.unmark_text(window, cx));
        shell.update(cx, |shell, cx| {
            shell.refresh_presented_invocations(revision, 0, window, cx)
        });
        requested(&shell, cx);
    });
}
