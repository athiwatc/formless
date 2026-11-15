use super::*;
use formless_desktop::client::{Command as WireCommand, Update};

fn hit(id: &str, message: &str) -> formless_protocol::SearchHit {
    formless_protocol::SearchHit {
        session_id: id.into(),
        session_name: format!("Session {id} ไทย 😀"),
        message_id: message.into(),
        seq: 1,
        role: "assistant".into(),
        snippet: "Exact snippet ไทย 漢字 😀".into(),
        created_at: "2026-11-15".into(),
    }
}

fn take_search(shell: &Entity<Shell>, cx: &mut App) -> (uuid::Uuid, String) {
    let commands = shell.update(cx, |shell, _| shell.take_client_commands());
    assert!(!commands.iter().any(|command| matches!(
        command,
        WireCommand::Intent(_)
            | WireCommand::Review { .. }
            | WireCommand::CreateSession
            | WireCommand::EditSession { .. }
            | WireCommand::AnswerQuestion { .. }
    )));
    commands
        .into_iter()
        .find_map(|command| match command {
            WireCommand::SearchProfile {
                connection: 0,
                operation,
                query,
            } => Some((operation, query)),
            _ => None,
        })
        .expect("an explicit read-only search")
}

fn enter_query(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    text: &str,
) -> (uuid::Uuid, String) {
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::ProfileSearch), window, cx);
        });
        let query = shell.read(cx).profile_query();
        query.update(cx, |query, cx| {
            query.set_value(text, window, cx);
            query.focus(window, cx);
        });
        window.render_frame(cx);
        window.press("enter", cx);
        take_search(shell, cx)
    })
}

fn reply(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    request: &(uuid::Uuid, String),
    hits: Vec<formless_protocol::SearchHit>,
) {
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::ProfileSearch {
                    connection: 0,
                    operation: request.0,
                    query: request.1.clone(),
                    result: Ok(hits),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
    });
}

#[gpui_kit::test]
fn profile_search_is_explicit_and_opening_a_result_preserves_the_previous_composer(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent draft ไทย 漢字 😀", cx);
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::ProfileSearch), window, cx);
        });
        window.input("needle", cx);
    });
    let request = step(cx, handle, |window, cx| {
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::SearchProfile { .. })));
        window.press("enter", cx);
        take_search(&shell, cx)
    });
    assert_eq!(request.1, "needle");
    let found = hit(&sessions[1].id, "stored-message");
    reply(cx, handle, &shell, &request, vec![found.clone()]);
    step(cx, handle, |window, cx| {
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        assert!(window
            .find("profile-search-scope")
            .value()
            .unwrap()
            .contains("including archived"));
        window.click("profile-search-copy", cx);
        let copied: formless_protocol::SearchHit =
            serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap();
        assert_eq!(copied, found);
        window.click(("profile-search-row", 0usize), cx);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        window.press("enter", cx);
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[1].id.as_str())
        );
        assert_eq!(shell.read(cx).draft(cx).as_ref(), "");
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        assert_eq!(
            shell.read(cx).draft(cx).as_ref(),
            "Unsent draft ไทย 漢字 😀"
        );
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::Intent(_) | WireCommand::Review { .. })));
    });
}

#[gpui_kit::test]
fn profile_search_retires_old_queries_callbacks_and_disconnected_results_without_opening_neighbours(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, handle, &shell);
    let first = enter_query(cx, handle, &shell, "first");
    reply(cx, handle, &shell, &first, vec![hit(&sessions[0].id, "a")]);
    let old = step(cx, handle, |_, cx| shell.read(cx).profile_revision());
    let second = enter_query(cx, handle, &shell, "second");
    reply(
        cx,
        handle,
        &shell,
        &first,
        vec![hit(&sessions[1].id, "late")],
    );
    step(cx, handle, |window, _| {
        assert!(window.try_find(("profile-search-row", 0usize)).is_none())
    });
    reply(cx, handle, &shell, &second, vec![hit(&sessions[2].id, "b")]);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| shell.open_profile_hit(0, old, window, cx));
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
        sessions[2].archived = true;
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
            shell.open_profile_hit(0, shell.profile_revision(), window, cx);
        });
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
        window.render_frame(cx);
        assert!(window
            .find("profile-search-notice")
            .value()
            .unwrap()
            .contains("archived"));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected".into(),
                },
                window,
                cx,
            )
        });
    });
    reply(
        cx,
        handle,
        &shell,
        &second,
        vec![hit(&sessions[0].id, "retired")],
    );
    step(cx, handle, |window, cx| {
        assert!(window.try_find(("profile-search-row", 0usize)).is_none());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "Reconnected".into(),
                },
                window,
                cx,
            );
            shell.open_profile_hit(0, shell.profile_revision(), window, cx);
        });
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::SearchProfile { .. })));
    });
}

#[gpui_kit::test]
fn profile_search_query_composition_cannot_search_or_open_and_keyboard_results_fit_at_200_percent(
    cx: &mut TestAppContext,
) {
    let viewport = size(px(480.), px(360.));
    let (handle, shell) = open(cx, viewport, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx);
            shell.run_command(Command::GoTo(Destination::ProfileSearch), window, cx);
        });
        window.render_frame(cx);
        let query = shell.read(cx).profile_query();
        let bounds = window.find_by_label("Search Profile messages").bounds();
        TestInput::new(ElementInputHandler::new(bounds, query), window)
            .compose_text(None, "かな", None, cx);
        window.press("enter", cx);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::SearchProfile { .. })));
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
    });
    let request = enter_query(cx, handle, &shell, "needle");
    reply(
        cx,
        handle,
        &shell,
        &request,
        vec![
            hit(&sessions[0].id, "first"),
            hit(&sessions[1].id, "second"),
        ],
    );
    step(cx, handle, |window, cx| {
        window.press("down", cx);
        for _ in 0..6 {
            window.render_frame(cx);
            window.simulate_next_frame(cx);
        }
    });
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        let row = window.find(("profile-search-row", 1usize)).bounds();
        assert!(
            row.top() >= px(0.) && row.bottom() <= viewport.height,
            "chosen result must be visible: {row:?}"
        );
        assert_eq!(shell.read(cx).sidebar().active_id(), None);
        for _ in 0..8 {
            window.press("tab", cx);
            for _ in 0..4 {
                window.render_frame(cx);
                window.simulate_next_frame(cx);
            }
            for id in [
                "profile-search-submit",
                "profile-search-open",
                "profile-search-copy",
                "profile-search-back",
            ] {
                if window.find(id).focused() == Some(true) {
                    let bounds = window.find(id).bounds();
                    assert!(
                        bounds.top() >= px(0.) && bounds.bottom() <= viewport.height,
                        "focused {id} must be visible: {bounds:?}"
                    );
                }
            }
        }
    });
}

#[gpui_kit::test]
fn profile_and_loaded_searches_return_to_the_underlying_destination_without_a_back_loop(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.run_command(Command::GoTo(Destination::Settings), window, cx);
            shell.run_command(Command::GoTo(Destination::Search), window, cx);
            shell.run_command(Command::GoTo(Destination::ProfileSearch), window, cx);
        });
        window.render_frame(cx);
        window.press("escape", cx);
        assert_eq!(shell.read(cx).destination(), Destination::Settings);
    });
}
