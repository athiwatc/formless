fn run_state_metadata(run: &str, status: &str, time: &str) -> formless_protocol::PlanState {
    formless_protocol::PlanState {
        run_id: Some(run.into()),
        updated_at: Some(time.into()),
        run_status: Some(status.into()),
        previous_task: false,
        origin: "live".into(),
    }
}
fn run_state_recap(status: &str, count: i64) -> formless_protocol::RunRecap {
    formless_protocol::RunRecap {
        status: status.into(),
        total_calls: Some(count),
        unfinished_calls: 2,
        steps: 4,
        used: vec![format!("same ×{}", (count - 2).max(0))],
        built: vec!["created example v1".into()],
        untracked_writes_possible: true,
        ..Default::default()
    }
}
fn run_state_snapshot(run: &str) -> serde_json::Value {
    serde_json::json!({"session":{"id":"s1"},"messages":[],"running":false,"pending_events":[],"queued_items":[],"plan":[{"content":"original step","status":"in_progress"}],"plan_state":run_state_metadata(run,"paused","2026-11-02T01:00:00Z"),"latest_run":{"run_id":run,"status":"paused"},"last_recap":{"run_id":run,"recap":run_state_recap("paused",7)}})
}
#[test]
fn run_state_actual_session_response_restores_exact_recap_and_lifecycle_once() {
    let mut app = test_app();
    let (client, sent) = DaemonClient::recording();
    let (tx, inbox) = mpsc::channel();
    app.client = Some(client);
    app.inbox = Some(inbox);
    app.request(Pending::GetSession, Request::GetSession { id: "s1".into() });
    let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
    tx.send(ServerMessage::Response {
        id: request.id,
        ok: true,
        result: Some(run_state_snapshot("r1")),
        error: None,
    })
    .unwrap();
    app.drain_daemon();
    assert_eq!(app.recap_run_id.as_deref(), Some("r1"));
    assert_eq!(app.recap.as_ref().unwrap().total_calls, Some(7));
    assert_eq!(
        app.plan_state.as_ref().unwrap().run_status.as_deref(),
        Some("paused")
    );
    app.recap_open = true;
    app.on_event(Event::RunRecap {
        session_id: "s1".into(),
        run_id: "r1".into(),
        recap: run_state_recap("paused", 7),
    });
    assert!(
        app.recap_open,
        "duplicate durable event does not collapse expanded recap"
    );
    let text = recap_lines(app.recap.as_ref().unwrap(), true, false)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("paused"));
    assert!(text.contains("7 calls"));
    assert!(text.contains("2 unfinished calls"));
    assert!(text.contains(RUN_FILES_CAVEAT));
    let text = plan_lines(&app.plan, app.plan_state.as_ref(), false, 120, false)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("paused"));
    assert!(text.contains("original step"));
}
#[test]
fn run_state_delayed_session_response_cannot_replace_newer_run_events() {
    let mut app = test_app();
    let (client, sent) = DaemonClient::recording();
    let (tx, inbox) = mpsc::channel();
    app.client = Some(client);
    app.inbox = Some(inbox);
    app.request(Pending::GetSession, Request::GetSession { id: "s1".into() });
    let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
    app.on_event(Event::RunStarted {
        session_id: "s1".into(),
        run_id: "r2".into(),
    });
    app.on_event(Event::PlanUpdated {
        session_id: "s1".into(),
        items: vec![PlanItem {
            content: "new step".into(),
            status: "pending".into(),
        }],
        plan_state: Some(run_state_metadata("r2", "running", "2026-11-02T02:00:00Z")),
    });
    app.on_event(Event::RunRecap {
        session_id: "s1".into(),
        run_id: "r2".into(),
        recap: run_state_recap("interrupted", 42),
    });
    app.on_event(Event::ToolCallStarted {
        invocation_id: Some("new-call".into()),
        session_id: "s1".into(),
        run_id: "r2".into(),
        tool_name: "same".into(),
        version: 1,
        arguments: "{}".into(),
        depth: 0,
    });
    tx.send(ServerMessage::Response {
        id: request.id,
        ok: true,
        result: Some(run_state_snapshot("r1")),
        error: None,
    })
    .unwrap();
    app.drain_daemon();
    assert_eq!(app.live_tools.len(), 1);
    assert_eq!(app.live_tools[0].invocation_id.as_deref(), Some("new-call"));
    assert_eq!(app.plan[0].content, "new step");
    assert_eq!(app.recap_run_id.as_deref(), Some("r2"));
    assert_eq!(app.recap.as_ref().unwrap().total_calls, Some(42));
    app.on_event(Event::RunRecap {
        session_id: "s1".into(),
        run_id: "r1".into(),
        recap: run_state_recap("completed", 1),
    });
    assert_eq!(app.recap_run_id.as_deref(), Some("r2"));
}
#[test]
fn run_state_reconnect_reduces_racing_plan_and_recap_and_legacy_missing_state() {
    let mut app = test_app();
    let (client, _) = DaemonClient::recording();
    app.client = Some(client);
    app.offline = Some((Instant::now(), Instant::now()));
    app.reconnect_request = Some((77, "s1".into()));
    app.on_event(Event::RunStarted {
        session_id: "s1".into(),
        run_id: "r2".into(),
    });
    app.on_event(Event::PlanUpdated {
        session_id: "s1".into(),
        items: vec![PlanItem {
            content: "new task".into(),
            status: "in_progress".into(),
        }],
        plan_state: Some(run_state_metadata("r2", "running", "2026-11-02T02:00:00Z")),
    });
    app.on_event(Event::RunRecap {
        session_id: "s1".into(),
        run_id: "r2".into(),
        recap: run_state_recap("failed", 9),
    });
    app.on_event(Event::RunRecap {
        session_id: "s1".into(),
        run_id: "r1".into(),
        recap: run_state_recap("completed", 1),
    });
    app.apply_reconnect_snapshot(77, run_state_snapshot("r1"));
    assert_eq!(app.plan[0].content, "new task");
    assert_eq!(
        app.plan_state.as_ref().unwrap().run_id.as_deref(),
        Some("r2")
    );
    assert_eq!(app.recap_run_id.as_deref(), Some("r2"));
    assert_eq!(app.recap.as_ref().unwrap().total_calls, Some(9));
    let mut no_recap = run_state_snapshot("r3");
    no_recap["last_recap"] = serde_json::Value::Null;
    no_recap["plan_state"] = serde_json::Value::Null;
    app.restore_run_state(&no_recap);
    assert!(app.recap.is_none());
    assert!(app.plan_state.is_none());
    let mut copied = run_state_metadata("foreign", "interrupted", "2026-11-02T01:00:00Z");
    copied.origin = "copied".into();
    copied.run_id = None;
    copied.previous_task = true;
    let label = plan_state_label(Some(&copied));
    assert!(label.contains("previous task"));
    assert!(label.contains("copied"));
    assert!(label.contains("interrupted"));
}

#[test]
fn run_state_all_unfinished_calls_never_render_as_finished_or_successful() {
    let recap = formless_protocol::RunRecap {
        status: "paused".into(),
        total_calls: Some(5),
        unfinished_calls: 5,
        steps: 4,
        used: vec![],
        ..Default::default()
    };
    for open in [false, true] {
        let text = recap_lines(&recap, open, false)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("5 calls"));
        assert!(text.contains("5 unfinished calls"));
        assert!(!text.contains("5 finished calls"));
        assert!(!text.contains("success"));
        assert!(!text.contains("    Used"));
    }
}
