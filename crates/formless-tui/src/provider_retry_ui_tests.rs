#[test]
fn provider_retry_ui_reports_only_acknowledged_outcome() {
    for retrying in [true, false] {
        let mut app = test_app();
        let (client, sent) = DaemonClient::recording();
        app.client = Some(client);
        let (tx, inbox) = mpsc::channel();
        app.inbox = Some(inbox);
        let before = app.notes.len();
        app.slash("resend");
        let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
        assert!(matches!(
            request.request,
            Request::RetryProviderRequest { .. }
        ));
        assert_eq!(app.notes.len(), before, "no optimistic retry message");
        tx.send(ServerMessage::Response {
            id: request.id,
            ok: true,
            result: Some(serde_json::json!({"retrying":retrying})),
            error: None,
        })
        .unwrap();
        app.drain_daemon();
        let note = app.notes.last().unwrap();
        assert!(note.contains(if retrying {
            "Retry requested"
        } else {
            "Nothing was retried"
        }));
        if !retrying {
            assert!(!note.contains("Re-sending"));
        }
    }
}
#[test]
fn provider_retry_ui_missing_ack_is_uncertain_and_keeps_original_session_identity() {
    let mut app = test_app();
    let (client, sent) = DaemonClient::recording();
    app.client = Some(client);
    let (tx, inbox) = mpsc::channel();
    app.inbox = Some(inbox);
    app.slash("resend");
    let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
    app.current = Some("other".into());
    tx.send(ServerMessage::Response {
        id: request.id,
        ok: false,
        result: None,
        error: Some("connection lost".into()),
    })
    .unwrap();
    app.drain_daemon();
    let note = app.notes.last().unwrap();
    assert!(note.starts_with("Session s1:"));
    assert!(note.contains("not confirmed"));
    assert!(!note.contains("not delivered"));
}
#[test]
fn provider_retry_stuck_pause_reason_survives_actual_session_attach() {
    let mut app = test_app();
    // The real attach flow receives ListSessions before opening GetSession.
    // Rendering needs that Session row as well as its detailed snapshot.
    app.sessions.push(
        serde_json::from_value(serde_json::json!({
            "id":"s1", "name":"Paused fixture", "model":null,
            "reasoning_effort":null, "created_at":"2026-01-01T00:00:00Z",
            "updated_at":"2026-01-01T00:00:00Z", "last_run_status":"paused"
        }))
        .unwrap(),
    );
    let (client, sent) = DaemonClient::recording();
    app.client = Some(client);
    let (tx, inbox) = mpsc::channel();
    app.inbox = Some(inbox);
    app.request(Pending::GetSession, Request::GetSession { id: "s1".into() });
    let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
    let mut snapshot = reconnect_snapshot(false, vec![]);
    snapshot["unfinished_run"] = serde_json::json!({"run_id":"r","status":"paused","error":"stuck: repeated daemon correction; change the call before Resume"});
    tx.send(ServerMessage::Response {
        id: request.id,
        ok: true,
        result: Some(snapshot),
        error: None,
    })
    .unwrap();
    app.drain_daemon();
    assert!(app.run_banner.as_ref().unwrap().1.contains("stuck:"));
    let screen = render(&mut app, 140, 40);
    assert!(screen.contains("stuck:"), "{screen}");
    assert!(
        screen.contains("Resume") || screen.contains("resume"),
        "{screen}"
    );
    assert!(
        app.messages.is_empty(),
        "the banner requires no fake history"
    );
    assert!(!screen.contains("Try one — press its number:"), "{screen}");

    // A truly idle Session with the same empty history still gets its welcome.
    app.run_banner = None;
    app.sessions[0].last_run_status = None;
    let idle_screen = render(&mut app, 140, 40);
    assert!(
        idle_screen.contains("Try one — press its number:"),
        "{idle_screen}"
    );
    assert!(!idle_screen.contains("stuck:"), "{idle_screen}");
}
