use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::run_inspection::{Report, Target};
use serde_json::{json, Value};
fn target(n: usize) -> Target {
    Target {
        run_id: format!("recorded-run-{n}"),
        session_id: format!("fixture-session-{n}"),
    }
}
fn value(target: &Target) -> Value {
    json!({"run_id":target.run_id,"session_id":target.session_id,"status":"running","error":format!("  ไทย 😀\r\n{}  ","private line\n".repeat(90)),"steps":2,"tool_calls":1,"prompt_tokens":100,"completion_tokens":20,"created_at":"now","updated_at":"later","start_message_seq":1,"end_message_seq":null,"spend":{"total_usd":null,"known_usd":0.12,"spend_complete":false,"unpriced":["unknown-model"]},"future":{"exact":"private future fields"}})
}
fn metric(n: usize) -> Value {
    let target = target(n);
    json!({"run_id":target.run_id,"session_id":target.session_id,"status":"running","outcome":"running","steps":2,"tool_calls":1,"prompt_tokens":100,"completion_tokens":20,"duration_ms":10,"created_at":"now"})
}
fn metrics(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    shell.update(cx,|shell,cx| {
        shell.run_command(Command::GoTo(Destination::Metrics),window,cx);
        shell.apply_client_update(Update::Page{destination:Destination::Metrics,result:Ok(json!({"summary":{"run_id":"fake-summary-id","session_id":"fake-summary-session"},"runs":[metric(1),metric(2)]}))},window,cx);
        shell.focus_section(PageSection::Conversation,window,cx);shell.take_client_commands();
    });
    window.render_frame(cx);
}
fn choose_first(
    shell: &Entity<Shell>,
    window: &mut Window,
    cx: &mut App,
) -> (u64, Destination, u64, usize) {
    shell.update(cx, |shell, cx| {
        shell.focus_section(PageSection::Conversation, window, cx)
    });
    window.press("down", cx);
    window.render_frame(cx);
    let source = shell.read(cx).run_inspection_source().unwrap();
    assert_eq!(source.3, 1);
    source
}
fn requested(shell: &Entity<Shell>, cx: &mut App, target: &Target) -> uuid::Uuid {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    let mut operations = vec![];
    for command in commands {
        match command {
            WireCommand::InspectRun {
                connection: 0,
                operation,
                target: actual,
            } => {
                assert_eq!(&actual, target);
                operations.push(operation);
            }
            WireCommand::Page(Destination::RunDetails) => {}
            _ => panic!("unexpected wire command during inspection"),
        }
    }
    assert_eq!(operations.len(), 1);
    operations[0]
}
fn reply(
    shell: &Entity<Shell>,
    target: &Target,
    connection: u64,
    operation: uuid::Uuid,
    window: &mut Window,
    cx: &mut App,
) {
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::RunInspection {
                connection,
                operation,
                target: target.clone(),
                result: Report::decode(target, value(target)),
            },
            window,
            cx,
        )
    });
    window.render_frame(cx);
}
#[gpui_kit::test]
fn run_inspection_is_explicit_and_copies_exact_private_and_future_json(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let target = target(1);
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
        window.input("Unsent for A ไทย 😀", cx);
        metrics(&shell, window, cx);
        assert!(
            shell
                .update(cx, |shell, cx| shell.run_command(
                    Command::InspectRun,
                    window,
                    cx
                ))
                .is_some(),
            "Summary cannot inspect even if it has fabricated identity keys"
        );
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        choose_first(&shell, window, cx);
        window.click("inspect-selected-run", cx);
        let operation = requested(&shell, cx, &target);
        assert_eq!(shell.read(cx).destination(), Destination::RunDetails);
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, &target, 0, operation, window, cx);
        let scope = shell.read(cx).resource_copy_target().unwrap();
        assert!(shell.update(cx, |shell, cx| shell
            .copy_resource_details(scope, window, cx)));
        assert_eq!(
            serde_json::from_str::<Value>(&cx.read_from_clipboard().unwrap().text().unwrap())
                .unwrap(),
            value(&target)
        );
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        window.press("down", cx);
        window.render_frame(cx);
        assert!(window
            .find("resource-detail")
            .value()
            .unwrap()
            .contains("Preview limited"));
        let status = shell.read(cx).resource_copy_target().unwrap();
        assert!(shell.update(cx, |shell, cx| shell
            .copy_resource_details(status, window, cx)));
        let copied: Value =
            serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap();
        assert_eq!(copied["error"], value(&target)["error"]);
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(scope, window, cx)));
        window.render_frame(cx);
        window.click("resource-back", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Metrics);
        assert!(
            !shell
                .update(cx, |shell, _| shell.take_client_commands())
                .iter()
                .any(|command| matches!(command, WireCommand::InspectRun { .. })),
            "Back does not inspect automatically"
        );
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Chat), window, cx)
        });
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        assert_eq!(
            shell.read(cx).composer().read(cx).value().as_ref(),
            "Unsent for A ไทย 😀"
        );
    });
}
#[gpui_kit::test]
fn run_inspection_source_buttons_reject_selection_page_navigation_and_modal_changes(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    let target = target(1);
    step(cx, handle, |window, cx| {
        metrics(&shell, window, cx);
        let old_source = choose_first(&shell, window, cx);
        let revision = shell.read(cx).run_inspection_revision();
        window.press("down", cx);
        shell.update(cx, |shell, cx| {
            shell.inspect_presented_run(target.clone(), revision, Some(old_source), window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        metrics(&shell, window, cx);
        let current = choose_first(&shell, window, cx);
        shell.update(cx, |shell, cx| {
            shell.inspect_presented_run(target.clone(), revision, Some(old_source), window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ShowCommands, window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.inspect_presented_run(target.clone(), revision, Some(current), window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        window.render_frame(cx);
        window.press("escape", cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Tools), window, cx)
        });
        shell.update(cx, |shell, _| shell.take_client_commands());
        shell.update(cx, |shell, cx| {
            shell.inspect_presented_run(target.clone(), revision, Some(current), window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
    });
}
#[gpui_kit::test]
fn run_inspection_replies_refresh_errors_and_stale_copies_keep_exact_ownership(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    let target = target(1);
    step(cx, handle, |window, cx| {
        metrics(&shell, window, cx);
        let source = choose_first(&shell, window, cx);
        let revision = shell.read(cx).run_inspection_revision();
        window.click("inspect-selected-run", cx);
        let operation = requested(&shell, cx, &target);
        shell.update(cx, |shell, cx| {
            shell.inspect_presented_run(target.clone(), revision, Some(source), window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        reply(&shell, &target, 1, operation, window, cx);
        reply(&shell, &target, 0, uuid::Uuid::new_v4(), window, cx);
        reply(&shell, &self::target(2), 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::RunDetails,
                    result: Ok(value(&target)),
                },
                window,
                cx,
            )
        });
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, &target, 0, operation, window, cx);
        let old_copy = shell.read(cx).resource_copy_target().unwrap();
        window.click("refresh-resource", cx);
        let refreshed = requested(&shell, cx, &target);
        assert_ne!(refreshed, operation);
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(old_copy, window, cx)));
        reply(&shell, &target, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::RunInspection {
                    connection: 0,
                    operation: refreshed,
                    target: target.clone(),
                    result: Err("Run no longer available".into()),
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
            .contains("Run no longer available"));
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, &target, 0, refreshed, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
    });
}
#[gpui_kit::test]
fn run_inspection_retires_disconnect_same_counter_attachment_and_actual_connection_changes(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    let target = target(1);
    step(cx, handle, |window, cx| {
        metrics(&shell, window, cx);
        choose_first(&shell, window, cx);
        window.click("inspect-selected-run", cx);
        let operation = requested(&shell, cx, &target);
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
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "online".into(),
                },
                window,
                cx,
            )
        });
        reply(&shell, &target, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        metrics(&shell, window, cx);
        choose_first(&shell, window, cx);
        window.click("inspect-selected-run", cx);
        let operation = requested(&shell, cx, &target);
        shell.update(cx, |shell, cx| shell.connect_recording_daemon(cx));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "replacement".into(),
                },
                window,
                cx,
            )
        });
        reply(&shell, &target, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        metrics(&shell, window, cx);
        choose_first(&shell, window, cx);
        let source = shell.read(cx).run_inspection_source().unwrap();
        let revision = shell.read(cx).run_inspection_revision();
        assert_eq!(shell.read(cx).retire_recording_question_connection(), 1);
        shell.update(cx, |shell, cx| {
            shell.inspect_presented_run(target.clone(), revision, Some(source), window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        shell.update(cx, |shell, cx| shell.connect_recording_daemon(cx));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "ready replacement".into(),
                },
                window,
                cx,
            )
        });
        metrics(&shell, window, cx);
        choose_first(&shell, window, cx);
        window.click("inspect-selected-run", cx);
        let operation = requested(&shell, cx, &target);
        reply(&shell, &target, 0, operation, window, cx);
        let revision = shell.read(cx).run_inspection_revision();
        assert_eq!(shell.read(cx).retire_recording_question_connection(), 1);
        shell.update(cx, |shell, cx| {
            shell.inspect_presented_run(target.clone(), revision, None, window, cx)
        });
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "Refresh cannot cross actual connection turnover before its UI update"
        );
    });
}
fn focus_control(id: &'static str, window: &mut Window, cx: &mut App) {
    let mut reached = false;
    for _ in 0..90 {
        window.press("tab", cx);
        settle_frames(window, cx);
        if window.find(id).focused() == Some(true) {
            reached = true;
            break;
        }
    }
    assert!(reached, "{id} keyboard reachable");
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
fn run_inspection_is_keyboard_reachable_at_minimum_size_and_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 0, 0);
    daemon_view(cx, handle, &shell);
    let target = target(1);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx)
        });
        metrics(&shell, window, cx);
        choose_first(&shell, window, cx);
        focus_control("inspect-selected-run", window, cx);
        window.press("enter", cx);
        let operation = requested(&shell, cx, &target);
        reply(&shell, &target, 0, operation, window, cx);
        for id in ["region-conversation", "resource-notice", "resource-detail"] {
            assert!(window.find(id).bounds().size.width <= px(480.), "{id}");
        }
        focus_control("copy-resource", window, cx);
        window.press("enter", cx);
        assert_eq!(
            serde_json::from_str::<Value>(&cx.read_from_clipboard().unwrap().text().unwrap())
                .unwrap(),
            value(&target)
        );
        focus_control("resource-back", window, cx);
        window.press("enter", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Metrics);
    });
}
