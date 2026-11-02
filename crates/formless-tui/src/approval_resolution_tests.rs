fn approval_ack_test_reply(app: &mut App, resolved: bool) {
    let ids: Vec<u64> = app
        .pending
        .iter()
        .filter_map(|(id, kind)| matches!(kind, Pending::ApprovalDecision { .. }).then_some(*id))
        .collect();
    assert!(
        !ids.is_empty(),
        "decision must have a tracked acknowledgement"
    );
    let (tx, inbox) = mpsc::channel();
    app.inbox = Some(inbox);
    for id in ids {
        tx.send(ServerMessage::Response {
            id,
            ok: true,
            result: Some(serde_json::json!({"resolved":resolved})),
            error: None,
        })
        .unwrap();
    }
    app.drain_daemon();
}
#[test]
fn approval_closed_retires_only_exact_prompt_without_denial_or_grant() {
    let mut app = test_app();
    app.on_event(reconnect_approval("closed", "s1"));
    app.on_event(reconnect_approval("other", "s2"));
    app.approval_summaries
        .insert("closed".into(), "summary".into());
    app.broad_arm = Some(("closed".into(), 'y'));
    let before = app.notes.len();
    app.on_event(Event::ApprovalClosed {
        approval_id: "closed".into(),
    });
    assert_eq!(app.approvals.len(), 1);
    assert_eq!(app.approvals[0].approval_id, "other");
    assert!(!app.approval_summaries.contains_key("closed"));
    assert!(app.broad_arm.is_none());
    assert!(app.notes[before..]
        .iter()
        .all(|n| !n.contains("denied") && !n.contains("granted")));
    let before = app.notes.len();
    app.on_event(Event::ApprovalClosed {
        approval_id: "closed".into(),
    });
    assert_eq!(app.notes.len(), before);
    app.reconnect_request = Some((77, "s1".into()));
    app.reconnect_events = vec![Event::ApprovalClosed {
        approval_id: "closed".into(),
    }];
    app.apply_reconnect_snapshot(
        77,
        reconnect_snapshot(true, vec![reconnect_approval("closed", "s1")]),
    );
    assert!(!app.approvals.iter().any(|a| a.approval_id == "closed"));
}
#[test]
fn approval_keypress_waits_for_positive_ack_before_marking_applied_or_batch() {
    let mut app = test_app();
    let (client, sent) = DaemonClient::recording();
    app.client = Some(client);
    app.on_event(reconnect_approval("decision", "s1"));
    app.approval_batches.insert(
        "decision".into(),
        ApprovalBatch {
            batch_id: "batch".into(),
            index: 1,
            calls: vec![],
        },
    );
    app.input_focus = false;
    let before = app.notes.len();
    app.key(key(KeyCode::Char('s')));
    let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
    assert!(matches!(request.request, Request::ApprovalDecision { .. }));
    assert!(app.batch_answers.is_empty());
    assert!(!app.notes[before..]
        .iter()
        .any(|n| n.starts_with("approved ")));
    app.on_event(Event::ApprovalAnswered {
        approval_id: "decision".into(),
        approved: true,
        by: Some("TUI".into()),
    });
    approval_ack_test_reply(&mut app, true);
    assert_eq!(app.batch_answers["batch"], vec![(1, "this Session".into())]);
    assert_eq!(
        app.notes
            .iter()
            .filter(|n| n.starts_with("approved "))
            .count(),
        1
    );
    let (tx, inbox) = mpsc::channel();
    app.inbox = Some(inbox);
    tx.send(ServerMessage::Response {
        id: request.id,
        ok: true,
        result: Some(serde_json::json!({"resolved":true})),
        error: None,
    })
    .unwrap();
    app.drain_daemon();
    assert_eq!(
        app.notes
            .iter()
            .filter(|n| n.starts_with("approved "))
            .count(),
        1
    );
}
#[test]
fn approval_cancel_first_or_failed_ack_never_claims_applied() {
    for rpc_ok in [true, false] {
        let mut app = test_app();
        let (client, sent) = DaemonClient::recording();
        app.client = Some(client);
        let (tx, inbox) = mpsc::channel();
        app.inbox = Some(inbox);
        app.on_event(reconnect_approval("decision", "s1"));
        app.input_focus = false;
        let before = app.notes.len();
        app.key(key(KeyCode::Char('o')));
        let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
        app.on_event(Event::ApprovalClosed {
            approval_id: "decision".into(),
        });
        tx.send(ServerMessage::Response {
            id: request.id,
            ok: rpc_ok,
            result: rpc_ok.then(|| serde_json::json!({"resolved":false})),
            error: (!rpc_ok).then(|| "connection failed".into()),
        })
        .unwrap();
        app.drain_daemon();
        assert!(!app.notes[before..]
            .iter()
            .any(|n| n.starts_with("approved ") || n.starts_with("denied ")));
        assert!(app.notes.last().unwrap().contains(if rpc_ok {
            "no decision was applied"
        } else {
            "not confirmed"
        }));
    }
}
