use super::*;

fn message(index: i64, content: &str) -> MessageInfo {
    MessageInfo {
        id: format!("history-{index}"),
        session_id: "s1".into(),
        seq: index,
        role: "user".into(),
        content: content.into(),
        created_at: String::new(),
        meta: None,
    }
}
fn searching() -> App {
    let mut app = crate::tests::test_app();
    app.messages = vec![
        message(0, "first"),
        message(1, "second"),
        message(2, "needle"),
    ];
    app.input.set("private unsent draft");
    app.input.left();
    let history: Vec<_> = input_history(&app.messages, app.current.as_deref()).collect();
    app.input.search_older(&history);
    for c in "needle".chars() {
        app.input.search_type(Some(c), &history);
    }
    assert_eq!(app.input.searching(), Some(("needle", Some(2))));
    app
}
#[test]
fn history_reload_invalidates_shortened_and_shifted_matches_without_sending_the_draft() {
    for messages in [
        vec![message(0, "shorter")],
        vec![
            message(1, "shifted"),
            message(2, "needle"),
            message(3, "unrelated"),
        ],
    ] {
        let mut app = searching();
        let cursor = app.input.cursor_byte();
        let (client, sent) = DaemonClient::recording();
        app.client = Some(client);
        let (tx, rx) = mpsc::channel();
        app.inbox = Some(rx);
        app.pending.insert(77, Pending::GetSession);
        tx.send(ServerMessage::Response {
            id: 77,
            ok: true,
            result: Some(serde_json::json!({"messages":messages})),
            error: None,
        })
        .unwrap();
        app.drain_daemon();
        assert_eq!(app.input.searching(), Some(("needle", None)));
        while sent.try_recv().is_ok() {}
        app.input_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input.expanded(), "private unsent draft");
        assert_eq!(app.input.cursor_byte(), cursor);
        assert!(app.input.searching().is_none());
        assert!(
            sent.try_recv().is_err(),
            "Enter only cancels the stale search; it must not send"
        );
    }
}
#[test]
fn history_reconnect_and_stashed_drafts_invalidate_matches_without_losing_text_or_cursor() {
    let mut app = searching();
    let cursor = app.input.cursor_byte();
    app.reconnect_request = Some((77, "s1".into()));
    app.apply_reconnect_snapshot(77, serde_json::json!({"messages":[message(0,"shorter")], "running":false, "pending_events":[], "queued_items":[], "plan":[], "step_mode":false, "unfinished_run":null}));
    assert_eq!(app.input.searching(), Some(("needle", None)));
    app.input_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.input.expanded(), "private unsent draft");
    assert_eq!(app.input.cursor_byte(), cursor);
    let mut app = searching();
    app.switch_session("s2".into());
    assert!(
        input_history(&app.messages, app.current.as_deref())
            .next()
            .is_none(),
        "old Session messages are never the new Session's input history"
    );
    app.switch_session("s1".into());
    assert_eq!(app.input.searching(), Some(("needle", None)));
    app.input_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.input.expanded(), "private unsent draft");
    assert_eq!(app.input.cursor_byte(), cursor);
}
#[test]
fn history_large_session_borrows_payloads_and_renders_only_a_bounded_match_preview() {
    let mut app = crate::tests::test_app();
    app.messages = (0..64)
        .map(|i| message(i, &format!("prefix {i} 界👩🏽‍💻 {}", "x".repeat(32768))))
        .collect();
    {
        let history: Vec<_> = input_history(&app.messages, app.current.as_deref()).collect();
        assert_eq!(history.len(), 64);
        assert!(history.iter().map(|text| text.len()).sum::<usize>() > 2 * 1024 * 1024);
        for (borrowed, original) in history.iter().zip(&app.messages) {
            assert_eq!(
                borrowed.as_ptr(),
                original.content.as_ptr(),
                "history must borrow rather than copy message payloads"
            );
        }
        app.input.search_older(&history);
        for c in "absent".chars() {
            app.input.search_type(Some(c), &history);
        }
        assert_eq!(app.input.searching(), Some(("absent", None)));
        app.input.search_cancel();
        app.input.search_older(&history);
        for c in "prefix".chars() {
            app.input.search_type(Some(c), &history);
        }
        assert_eq!(app.input.searching(), Some(("prefix", Some(63))));
    }
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 4)).unwrap();
    for _ in 0..4 {
        terminal
            .draw(|frame| draw_input(frame, Rect::new(0, 0, 100, 4), &app, false, None))
            .unwrap();
    }
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("prefix 63"));
    let preview = history_preview(&app.messages[63].content, 80);
    assert!(text::width(&preview) <= 80);
    assert!(preview.len() <= 80 * 32 + 3);
    assert_eq!(
        history_preview(&format!("a{}", "\u{0301}".repeat(1024 * 1024)), 80),
        "…"
    );
}
