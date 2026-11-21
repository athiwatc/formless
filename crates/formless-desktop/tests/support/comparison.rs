use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::comparison::{Pair, Report};
use serde_json::{json, Value};

fn value(pair: &Pair) -> Value {
    let usage = json!({"scope":"recorded branch","recorded_prompt_tokens":1,"recorded_completion_tokens":2,"recorded_cached_tokens":0,"recorded_cache_write_tokens":0,"requests":1,"unallocated_requests":0,"unallocated_prompt_tokens":0,"unallocated_completion_tokens":0,"unknown_runs":1,"usage_complete":false,"cost":null,"priced_cost_subtotal":0.01,"unallocated_priced_cost_subtotal":0.0,"known_records_cost_range":null,"estimated_requests":1,"unpriced":[],"warnings":["missing usage"]});
    let mut sessions = fixture::sessions(2);
    sessions[0].id = pair.left.clone();
    sessions[1].id = pair.right.clone();
    json!({"left":{"session":sessions[0],"usage":usage},"right":{"session":sessions[1],"usage":usage},"common_ancestor":null,"fork_point_seq":null,"diff_from_seq":0,"prefix_changed":false,"messages":[{"kind":"left_only","left":{"id":"message","session_id":pair.left,"seq":1,"role":"assistant","content":"  exact ไทย 😀\r\n```rust\n code\n```  ","meta":"{\"private\":true}","created_at":"now"},"right":null}],"warnings":[]})
}
fn choose(shell: &Entity<Shell>, id: &str, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| {
        shell.focus_section(PageSection::Conversation, window, cx)
    });
    for _ in 0..4 {
        window.press("up", cx);
    }
    for _ in 0..4 {
        if shell.read(cx).directory_selected_id() == Some(id) {
            break;
        }
        window.press("down", cx);
    }
    assert_eq!(shell.read(cx).directory_selected_id(), Some(id));
    window.render_frame(cx);
}
fn start(shell: &Entity<Shell>, pair: &Pair, window: &mut Window, cx: &mut App) -> uuid::Uuid {
    shell.update(cx, |shell, cx| {
        shell.run_command(Command::GoTo(Destination::Sessions), window, cx);
        shell.take_client_commands();
    });
    choose(shell, &pair.left, window, cx);
    window.click("mark-comparison-left", cx);
    choose(shell, &pair.right, window, cx);
    window.click("compare-marked-sessions", cx);
    requested(shell, pair, cx)
}
fn requested(shell: &Entity<Shell>, pair: &Pair, cx: &mut App) -> uuid::Uuid {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    let mut reads = vec![];
    for command in commands {
        match command {
            WireCommand::CompareSessions {
                connection: 0,
                operation,
                pair: actual,
            } => {
                assert_eq!(actual, *pair);
                reads.push(operation);
            }
            WireCommand::Page(Destination::Comparison) => {}
            _ => panic!("unexpected command during comparison"),
        }
    }
    assert_eq!(reads.len(), 1);
    reads[0]
}
fn reply(
    shell: &Entity<Shell>,
    pair: &Pair,
    connection: u64,
    operation: uuid::Uuid,
    window: &mut Window,
    cx: &mut App,
) {
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::SessionComparison {
                connection,
                operation,
                pair: pair.clone(),
                result: Report::decode(pair, value(pair)),
            },
            window,
            cx,
        )
    });
    window.render_frame(cx);
}
#[gpui_kit::test]
fn comparison_is_explicit_read_only_and_copies_exact_known_json(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let pair = Pair {
        left: sessions[0].id.clone(),
        right: sessions[1].id.clone(),
    };
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent ไทย 😀", cx);
        let operation = start(&shell, &pair, window, cx);
        assert_eq!(shell.read(cx).destination(), Destination::Comparison);
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, &pair, 1, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, &pair, 0, uuid::Uuid::new_v4(), window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, &pair, 0, operation, window, cx);
        let target = shell.read(cx).resource_copy_target().unwrap();
        assert!(shell.update(cx, |shell, cx| shell
            .copy_resource_details(target, window, cx)));
        assert_eq!(
            serde_json::from_str::<Value>(&cx.read_from_clipboard().unwrap().text().unwrap())
                .unwrap(),
            value(&pair)
        );
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        for _ in 0..4 {
            window.press("down", cx);
        }
        let message_target = shell.read(cx).resource_copy_target().unwrap();
        assert!(shell.update(cx, |shell, cx| shell.copy_resource_details(
            message_target,
            window,
            cx
        )));
        assert_eq!(
            serde_json::from_str::<Value>(&cx.read_from_clipboard().unwrap().text().unwrap())
                .unwrap(),
            value(&pair)["messages"][0]
        );
        assert!(!shell.update(cx, |shell, cx| shell
            .copy_resource_details(target, window, cx)));
        window.render_frame(cx);
        window.click("refresh-resource", cx);
        let refreshed = requested(&shell, &pair, cx);
        assert_ne!(refreshed, operation);
        assert!(!shell.update(cx, |shell, cx| shell.copy_resource_details(
            message_target,
            window,
            cx
        )));
        reply(&shell, &pair, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, &pair, 0, refreshed, window, cx);
        window.click("resource-back", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
        assert_eq!(
            shell.read(cx).directory_query().read(cx).value().as_ref(),
            ""
        );
        assert!(window
            .find("comparison-mark")
            .value()
            .unwrap()
            .contains(&pair.left));
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Chat), window, cx)
        });
        assert_eq!(
            shell.read(cx).composer().read(cx).value().as_ref(),
            "Unsent ไทย 😀"
        );
    });
}
#[gpui_kit::test]
fn comparison_retires_navigation_disconnect_and_same_counter_attachment(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let pair = Pair {
        left: sessions[0].id.clone(),
        right: sessions[1].id.clone(),
    };
    step(cx, handle, |window, cx| {
        let operation = start(&shell, &pair, window, cx);
        let old = shell.read(cx).comparison_revision();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx)
        });
        reply(&shell, &pair, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        shell.update(cx, |shell, cx| {
            shell.compare_presented_pair(pair.clone(), old, window, cx)
        });
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|c| matches!(c, WireCommand::CompareSessions { .. })));
        let operation = start(&shell, &pair, window, cx);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "disconnected".into(),
                },
                window,
                cx,
            )
        });
        reply(&shell, &pair, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "connected".into(),
                },
                window,
                cx,
            )
        });
        let operation = start(&shell, &pair, window, cx);
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
        reply(&shell, &pair, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|c| matches!(c, WireCommand::CompareSessions { .. })));
    });
}
#[gpui_kit::test]
fn comparison_modal_and_composition_guards_reject_background_actions(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let pair = Pair {
        left: sessions[0].id.clone(),
        right: sessions[1].id.clone(),
    };
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx)
        });
        choose(&shell, &pair.left, window, cx);
        window.click("mark-comparison-left", cx);
        choose(&shell, &pair.right, window, cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        let revision = shell.read(cx).comparison_revision();
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::ShowCommands, window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.compare_presented_pair(pair.clone(), revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        window.render_frame(cx);
        window.press("escape", cx);
        let query = shell.read(cx).directory_query().clone();
        let bounds = window.find_by_label("Filter Sessions").bounds();
        TestInput::new(ElementInputHandler::new(bounds, query.clone()), window).compose_text(
            None,
            "にほんご",
            None,
            cx,
        );
        shell.update(cx, |shell, cx| {
            shell.compare_presented_pair(pair.clone(), revision, window, cx)
        });
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|c| matches!(c, WireCommand::CompareSessions { .. })));
        assert_eq!(shell.read(cx).destination(), Destination::Sessions);
    });
}
fn focus_control(id: &'static str, window: &mut Window, cx: &mut App) {
    let mut reached = false;
    for _ in 0..80 {
        window.press("tab", cx);
        settle_frames(window, cx);
        if window.find(id).focused() == Some(true) {
            reached = true;
            break;
        }
    }
    assert!(reached, "{id} reachable by keyboard");
    let bounds = window.find(id).bounds();
    assert!(
        bounds.top() >= px(0.) && bounds.bottom() <= px(360.),
        "{id}: {bounds:?}"
    );
}
#[gpui_kit::test]
fn comparison_minimum_window_at_200_percent_keeps_keyboard_actions_reachable(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(480.), px(360.)), 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let pair = Pair {
        left: sessions[0].id.clone(),
        right: sessions[1].id.clone(),
    };
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Sessions), window, cx)
        });
        choose(&shell, &pair.left, window, cx);
        assert!(
            window.find("directory-results").bounds().size.height >= px(64.),
            "room for a Session row below wrapped controls"
        );
        focus_control("mark-comparison-left", window, cx);
        window.press("enter", cx);
        choose(&shell, &pair.right, window, cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
        focus_control("compare-marked-sessions", window, cx);
        window.press("enter", cx);
        let operation = requested(&shell, &pair, cx);
        reply(&shell, &pair, 0, operation, window, cx);
        for id in ["region-conversation", "resource-notice", "resource-detail"] {
            assert!(window.find(id).bounds().size.width <= px(480.), "{id}");
        }
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Conversation, window, cx)
        });
        let mut reached = false;
        for _ in 0..60 {
            window.press("tab", cx);
            settle_frames(window, cx);
            if window.find("copy-resource").focused() == Some(true) {
                reached = true;
                break;
            }
        }
        assert!(reached, "Copy details reachable by keyboard");
        let bounds = window.find("copy-resource").bounds();
        assert!(
            bounds.top() >= px(0.) && bounds.bottom() <= px(360.),
            "{bounds:?}"
        );
        window.press("enter", cx);
        assert_eq!(
            serde_json::from_str::<Value>(&cx.read_from_clipboard().unwrap().text().unwrap())
                .unwrap(),
            value(&pair)
        );
    });
}

#[gpui_kit::test]
fn comparison_errors_foreign_pairs_and_missing_sessions_never_install_replacement_data(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let pair = Pair {
        left: sessions[0].id.clone(),
        right: sessions[1].id.clone(),
    };
    step(cx, handle, |window, cx| {
        let operation = start(&shell, &pair, window, cx);
        let foreign = Pair {
            left: pair.left.clone(),
            right: sessions[2].id.clone(),
        };
        reply(&shell, &foreign, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Page {
                    destination: Destination::Comparison,
                    result: Ok(value(&pair)),
                },
                window,
                cx,
            )
        });
        assert!(shell.read(cx).resource_copy_target().is_none());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::SessionComparison {
                    connection: 0,
                    operation,
                    pair: pair.clone(),
                    result: Err("missing or inaccessible".into()),
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
            .contains("missing or inaccessible"));
        assert!(shell.read(cx).resource_copy_target().is_none());
        reply(&shell, &pair, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        window.click("refresh-resource", cx);
        let operation = requested(&shell, &pair, cx);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(vec![sessions[0].clone()]), window, cx)
        });
        reply(&shell, &pair, 0, operation, window, cx);
        assert!(shell.read(cx).resource_copy_target().is_none());
        assert!(window
            .find("resource-notice")
            .value()
            .unwrap()
            .contains("no longer listed"));
    });
}
