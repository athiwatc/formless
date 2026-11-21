use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::session_usage::{Report, Target};
use gpui_kit::EntityInputHandler as _;
use serde_json::json;

fn value() -> serde_json::Value {
    json!({"runs":2,"total":{"steps":3,"prompt_tokens":2000,"completion_tokens":100,"cached_tokens":200,"cache_write_tokens":300,"cost":null,"unpriced":["model ไทย 😀"]},"last_run":{"steps":1,"prompt_tokens":1000,"completion_tokens":50,"cached_tokens":100,"cost":0.00123456}})
}

#[gpui_kit::test]
fn usage_connection_lost_notice_requires_a_previously_connected_client(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| shell.connect_recording_daemon(cx));
        for (ready, notice, lost) in [
            (false, "Connecting…", false),
            (false, "Cannot connect", false),
            (true, "Connected", false),
            (false, "Disconnected", true),
            (false, "Connecting…", true),
            (true, "Connected", true),
        ] {
            shell.update(cx, |shell, cx| {
                shell.apply_client_update(
                    Update::Connection {
                        ready,
                        notice: notice.into(),
                    },
                    window,
                    cx,
                );
                shell.open_panel(false, window, cx);
            });
            window.render_frame(cx);
            assert_eq!(
                window
                    .find("session-usage-notice")
                    .value()
                    .unwrap()
                    .contains("Connection lost"),
                lost,
                "{notice}"
            );
        }
    });
}
fn activate(
    shell: &Entity<Shell>,
    session: &formless_protocol::SessionInfo,
    window: &mut Window,
    cx: &mut App,
) -> Target {
    window.render_frame(cx);
    if window.try_find(session_row_id(&session.id)).is_none() {
        window.click("toggle-sidebar", cx);
        window.render_frame(cx);
    }
    window.click(session_row_id(&session.id), cx);
    let generation = shell.read(cx).client_generation();
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::Snapshot {
                generation,
                snapshot: replies::session_snapshot(
                    &session.id,
                    &json!({"session":{"id":session.id},"running":false,"messages":[]}),
                )
                .unwrap(),
            },
            window,
            cx,
        )
    });
    shell.update(cx, |shell, _| {
        shell.take_client_commands();
    });
    Target {
        session_id: session.id.clone(),
        connection: 0,
        generation,
    }
}
fn focus(window: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..40 {
        window.render_frame(cx);
        if window.find(id).focused() == Some(true) {
            break;
        }
        window.press("tab", cx);
    }
    for _ in 0..4 {
        window.render_frame(cx);
        window.simulate_next_frame(cx);
    }
    assert_eq!(window.find(id).focused(), Some(true));
    let control = window.find(id).bounds();
    let panel = window.find("inspector-details").bounds();
    assert!(
        control.top() >= panel.top() && control.bottom() <= panel.bottom(),
        "{id}: {control:?}/{panel:?}"
    );
}
fn requested(shell: &Entity<Shell>, cx: &mut App, target: &Target) -> uuid::Uuid {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        WireCommand::ReadSessionUsage {
            target: actual,
            operation,
        } => {
            assert_eq!(actual, target);
            *operation
        }
        _ => panic!("expected usage read"),
    }
}
fn reply(
    shell: &Entity<Shell>,
    target: Target,
    operation: uuid::Uuid,
    result: Result<Report, String>,
    window: &mut Window,
    cx: &mut App,
) {
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::SessionUsage {
                target,
                operation,
                result,
            },
            window,
            cx,
        )
    });
}

#[gpui_kit::test]
fn recorded_usage_is_explicit_and_copy_includes_only_the_current_displayed_projection(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let target = activate(&shell, &sessions[0], window, cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent ไทย 😀", cx);
        shell.update(cx, |shell, cx| shell.open_panel(false, window, cx));
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "opening Context does not issue a usage read"
        );
        focus(window, cx, "session-usage-read");
        window.press("enter", cx);
        let operation = requested(&shell, cx, &target);
        let old_revision = shell.read(cx).session_usage_revision();
        shell.update(cx, |shell, cx| {
            shell.read_presented_session_usage(target.clone(), old_revision, window, cx)
        });
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "pending reads cannot be repeated"
        );
        reply(
            &shell,
            Target {
                generation: target.generation + 1,
                ..target.clone()
            },
            operation,
            Report::decode(value()),
            window,
            cx,
        );
        window.render_frame(cx);
        assert_eq!(window.find("session-usage-copy").disabled(), Some(true));
        reply(
            &shell,
            target.clone(),
            operation,
            Report::decode(value()),
            window,
            cx,
        );
        focus(window, cx, "session-usage-copy");
        window.press("enter", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            Report::decode(value()).unwrap().copy()
        );
        assert!(window
            .find("session-usage-values")
            .value()
            .unwrap()
            .contains("unavailable (not zero)"));
        assert!(window
            .find("session-usage-values")
            .value()
            .unwrap()
            .contains("0.00123456"));
        assert!(window
            .find("session-usage-notice")
            .value()
            .unwrap()
            .contains("100,000"));
        let version = shell.read(cx).session_usage_revision();
        focus(window, cx, "session-usage-read");
        window.press("enter", cx);
        let newer = requested(&shell, cx, &target);
        cx.write_to_clipboard(ClipboardItem::new_string("keep".into()));
        shell.update(cx, |shell, cx| {
            shell.copy_presented_session_usage(&target, version, cx)
        });
        assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "keep");
        reply(
            &shell,
            target.clone(),
            operation,
            Report::decode(value()),
            window,
            cx,
        );
        reply(
            &shell,
            target,
            newer,
            Err("Recorded fixture unavailable".into()),
            window,
            cx,
        );
        window.render_frame(cx);
        assert_eq!(window.find("session-usage-copy").disabled(), Some(true));
        assert_eq!(
            window.find("session-usage-notice").value(),
            Some("Recorded fixture unavailable")
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "Unsent ไทย 😀");
    });
}

#[gpui_kit::test]
fn usage_preedit_and_repeated_session_navigation_reject_retired_reads_and_copies(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let target = activate(&shell, &sessions[0], window, cx);
        let composer = shell.read(cx).composer().clone();
        composer.update(cx, |input, cx| {
            input.focus(window, cx);
            input.replace_and_mark_text_in_range(None, "กำลัง", None, window, cx);
        });
        assert!(composer.update(cx, |input, cx| input
            .marked_text_range(window, cx)
            .is_some()));
        let revision = shell.read(cx).session_usage_revision();
        shell.update(cx, |shell, cx| {
            shell.read_presented_session_usage(target.clone(), revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        composer.update(cx, |input, cx| input.unmark_text(window, cx));
        shell.update(cx, |shell, cx| {
            shell.read_presented_session_usage(target.clone(), revision, window, cx)
        });
        let old = requested(&shell, cx, &target);
        activate(&shell, &sessions[1], window, cx);
        let latest = activate(&shell, &sessions[0], window, cx);
        assert_ne!(target.generation, latest.generation);
        reply(
            &shell,
            target.clone(),
            old,
            Report::decode(value()),
            window,
            cx,
        );
        shell.update(cx, |shell, cx| shell.open_panel(false, window, cx));
        window.render_frame(cx);
        assert_eq!(window.find("session-usage-copy").disabled(), Some(true));
        shell.update(cx, |shell, cx| {
            shell.read_presented_session_usage(target.clone(), revision, window, cx)
        });
        assert!(shell
            .update(cx, |shell, _| shell.take_client_commands())
            .is_empty());
        let version = shell.read(cx).session_usage_revision();
        shell.update(cx, |shell, cx| {
            shell.read_presented_session_usage(latest.clone(), version, window, cx)
        });
        let operation = requested(&shell, cx, &latest);
        reply(
            &shell,
            latest.clone(),
            operation,
            Report::decode(value()),
            window,
            cx,
        );
        let copy_version = shell.read(cx).session_usage_revision();
        let connection = shell.read(cx).retire_recording_question_connection();
        assert_eq!(connection, 1);
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        shell.update(cx, |shell, cx| {
            shell.copy_presented_session_usage(&latest, copy_version, cx)
        });
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "unchanged"
        );
        window.render_frame(cx);
        assert_eq!(window.find("session-usage-copy").disabled(), Some(true));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "lost".into(),
                },
                window,
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "ready".into(),
                },
                window,
                cx,
            )
        });
        reply(
            &shell,
            latest,
            operation,
            Report::decode(value()),
            window,
            cx,
        );
        window.render_frame(cx);
        assert_eq!(window.find("session-usage-copy").disabled(), Some(true));
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "no reconnect replay"
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "กำลัง");
    });
}

#[gpui_kit::test]
fn usage_controls_remain_inside_the_context_overlay_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let viewport = size(px(480.), px(360.));
    let (handle, shell) = open(cx, viewport, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let target = activate(&shell, &sessions[0], window, cx);
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx);
            shell.open_panel(false, window, cx);
        });
        focus(window, cx, "session-usage-read");
        window.press("enter", cx);
        let operation = requested(&shell, cx, &target);
        reply(
            &shell,
            target,
            operation,
            Report::decode(value()),
            window,
            cx,
        );
        focus(window, cx, "session-usage-copy");
        window.press("enter", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            Report::decode(value()).unwrap().copy()
        );
        assert!(shell.read(cx).background_inert(window));
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(!shell.read(cx).background_inert(window));
    });
}
