fn progress_event(kind: &str, id: Option<&str>, extra: serde_json::Value) -> Event {
    let mut value = serde_json::json!({"type":kind,"session_id":"s","run_id":"r","tool_name":"same","version":1,"depth":0,"arguments":"{}","invocation_id":id});
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
}

#[test]
fn invocation_progress_identity_isolates_parallel_siblings_and_legacy_ambiguity() {
    let mut app = test_app();
    app.current = Some("s".into());
    for id in ["a", "b"] {
        app.on_event(progress_event(
            "tool_call_started",
            Some(id),
            serde_json::json!({}),
        ));
    }
    for (id, f) in [("a", 0.2), ("b", 0.8)] {
        app.on_event(progress_event(
            "tool_progress",
            Some(id),
            serde_json::json!({"fraction":f,"label":id}),
        ));
    }
    assert_eq!(app.live_tools[0].progress, Some((Some(0.2), "a".into())));
    assert_eq!(app.live_tools[1].progress, Some((Some(0.8), "b".into())));
    for id in [None, Some("unknown")] {
        app.on_event(progress_event(
            "tool_progress",
            id,
            serde_json::json!({"fraction":0.9,"label":"wrong"}),
        ));
        app.on_event(progress_event(
            "tool_call_finished",
            id,
            serde_json::json!({"ok":true}),
        ));
    }
    assert!(app.live_tools.iter().all(|c| c.status == "running"));
    assert_eq!(app.live_tools[0].progress.as_ref().unwrap().0, Some(0.2));
    app.on_event(progress_event(
        "tool_call_finished",
        Some("a"),
        serde_json::json!({"ok":true}),
    ));
    assert_eq!(app.live_tools[0].status, "ok");
    assert_eq!(app.live_tools[1].status, "running");
    app.on_event(progress_event(
        "tool_progress",
        None,
        serde_json::json!({"fraction":0.95,"label":"legacy unique"}),
    ));
    assert_eq!(app.live_tools[1].progress.as_ref().unwrap().0, Some(0.95));
}

#[test]
fn invocation_progress_stall_activity_pending_and_reconnect_keep_exact_identity() {
    let mut app = test_app();
    app.current = Some("s".into());
    for id in ["a", "b"] {
        app.on_event(progress_event(
            "tool_call_started",
            Some(id),
            serde_json::json!({}),
        ));
        app.on_event(progress_event(
            "invocation_pending",
            Some(id),
            serde_json::json!({"handle":format!("h-{id}")}),
        ));
        app.on_event(progress_event("invocation_stalled", Some(id), serde_json::json!({"handle":format!("h-{id}"),"silent_secs":30,"last_fraction":0.5,"last_label":format!("stuck-{id}")})));
    }
    assert_eq!(app.live_tools[0].handle.as_deref(), Some("h-a"));
    assert_eq!(app.live_tools[1].handle.as_deref(), Some("h-b"));
    app.on_event(progress_event(
        "invocation_activity",
        Some("a"),
        serde_json::json!({"handle":"h-a"}),
    ));
    assert_eq!(app.live_tools[0].stalled_secs, None);
    assert_eq!(app.live_tools[1].stalled_secs, Some(30));
    app.on_event(progress_event(
        "invocation_activity",
        Some("unknown"),
        serde_json::json!({"handle":"h-b"}),
    ));
    assert_eq!(app.live_tools[1].stalled_secs, Some(30));
    let snapshot: Vec<formless_protocol::InvocationInfo> = serde_json::from_value(serde_json::json!([
        {"invocation_id":"a","handle":"h-a","tool_name":"same","session_id":"s","elapsed_ms":30,"silent_ms":10,"stalled":false,"abandoned":false,"last_fraction":0.2,"last_label":"a"},
        {"invocation_id":"b","handle":"h-b","tool_name":"same","session_id":"s","elapsed_ms":30,"silent_ms":30000,"stalled":true,"abandoned":false,"last_fraction":0.5,"last_label":"b"}
    ])).unwrap();
    app.live_tools.clear();
    app.reconcile_invocation_progress(&snapshot);
    app.reconcile_invocation_progress(&snapshot);
    assert_eq!(app.live_tools.len(), 2);
    assert_eq!(app.live_tools[0].progress, Some((Some(0.2), "a".into())));
    assert_eq!(app.live_tools[1].stalled_secs, Some(30));
    app.on_event(progress_event(
        "invocation_completed",
        Some("a"),
        serde_json::json!({"handle":"h-a","ok":true}),
    ));
    app.reconcile_invocation_progress(&snapshot);
    assert_eq!(app.live_tools.len(), 2);
    assert_eq!(app.live_tools[0].status, "ok");
    let event = progress_event(
        "invocation_stalled",
        Some("b"),
        serde_json::json!({"handle":"h-b","silent_secs":30,"last_fraction":0.5,"last_label":"stuck\u{001b}[2J"}),
    );
    let text = plain::format_event(&event, &mut false).unwrap();
    assert!(text.contains("[b]"));
    assert!(text.contains("50%"));
    assert!(text.contains("stuck"));
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn invocation_progress_pending_before_start_merges_metadata_without_losing_state() {
    let mut app = test_app();
    app.current = Some("s".into());
    app.on_event(Event::RunStarted {
        session_id: "s".into(),
        run_id: "r".into(),
    });
    app.on_event(progress_event(
        "invocation_pending",
        Some("early"),
        serde_json::json!({"handle":"h-early"}),
    ));
    assert_eq!(app.live_tools.len(), 1);
    assert!(app.live_tools[0].metadata_pending);
    let began = app.live_tools[0].started;
    app.on_event(progress_event(
        "tool_progress",
        Some("early"),
        serde_json::json!({"depth":3,"fraction":0.4,"label":"before start"}),
    ));
    app.on_event(progress_event("invocation_stalled", Some("early"), serde_json::json!({"handle":"h-early","silent_secs":30,"last_fraction":0.4,"last_label":"before start"})));
    app.on_event(progress_event(
        "tool_call_started",
        Some("early"),
        serde_json::json!({"depth":3,"version":7,"arguments":"{\"x\":1}"}),
    ));
    assert_eq!(app.live_tools.len(), 1);
    let call = &app.live_tools[0];
    assert!(!call.metadata_pending);
    assert_eq!(call.depth, 3);
    assert_eq!(call.version, 7);
    assert_eq!(call.arguments, "{\"x\":1}");
    assert_eq!(call.handle.as_deref(), Some("h-early"));
    assert_eq!(call.progress, Some((Some(0.4), "before start".into())));
    assert_eq!(call.stalled_secs, Some(30));
    assert_eq!(call.started, began);
    app.on_event(progress_event(
        "tool_call_finished",
        Some("early"),
        serde_json::json!({"ok":true}),
    ));
    app.on_event(progress_event(
        "tool_call_started",
        Some("early"),
        serde_json::json!({}),
    ));
    app.on_event(progress_event(
        "invocation_pending",
        Some("early"),
        serde_json::json!({"handle":"h-late"}),
    ));
    assert_eq!(app.live_tools.len(), 1);
    assert_eq!(app.live_tools[0].status, "ok");
    assert_eq!(app.live_tools[0].handle.as_deref(), Some("h-early"));
    app.on_event(Event::RunFinished {
        session_id: "s".into(),
        run_id: "r".into(),
        status: "completed".into(),
    });
    app.on_event(progress_event(
        "invocation_pending",
        Some("late"),
        serde_json::json!({"handle":"h-late"}),
    ));
    assert!(app.live_tools.is_empty());
    app.on_event(Event::RunStarted {
        session_id: "s".into(),
        run_id: "r2".into(),
    });
    app.on_event(progress_event(
        "invocation_pending",
        Some("late"),
        serde_json::json!({"handle":"h-late"}),
    ));
    app.on_event(progress_event(
        "tool_call_started",
        Some("late"),
        serde_json::json!({}),
    ));
    assert!(app.live_tools.is_empty());
}

#[test]
fn invocation_progress_reconnect_placeholder_accepts_start_metadata_and_is_bounded() {
    let mut app = test_app();
    app.current = Some("s".into());
    let snapshot = serde_json::from_value(serde_json::json!({"invocation_id":"restored","handle":"h-restored","tool_name":"same","session_id":"s","elapsed_ms":30,"silent_ms":30000,"stalled":true,"abandoned":false,"last_fraction":0.6,"last_label":"restored label"})).unwrap();
    app.reconcile_invocation_progress(&[snapshot]);
    app.on_event(progress_event(
        "tool_call_started",
        Some("restored"),
        serde_json::json!({"depth":2,"version":8,"arguments":"[]"}),
    ));
    assert_eq!(app.live_tools.len(), 1);
    assert_eq!(app.live_tools[0].version, 8);
    assert_eq!(app.live_tools[0].depth, 2);
    assert_eq!(
        app.live_tools[0].progress,
        Some((Some(0.6), "restored label".into()))
    );
    assert_eq!(app.live_tools[0].handle.as_deref(), Some("h-restored"));
    for index in 0..140 {
        app.on_event(progress_event(
            "invocation_pending",
            Some(&format!("pending-{index}")),
            serde_json::json!({"handle":format!("handle-{index}")}),
        ));
    }
    assert_eq!(
        app.live_tools.iter().filter(|c| c.metadata_pending).count(),
        128
    );
}

#[test]
fn invocation_progress_known_background_id_outlives_run_and_newer_run_scope() {
    let mut app = test_app();
    app.current = Some("s".into());
    app.on_event(Event::RunStarted {
        session_id: "s".into(),
        run_id: "r2".into(),
    });
    app.on_event(progress_event(
        "tool_call_started",
        Some("new"),
        serde_json::json!({"run_id":"r2"}),
    ));
    let background = serde_json::from_value(serde_json::json!({"invocation_id":"background","handle":"h-background","tool_name":"same","session_id":"s","elapsed_ms":30,"silent_ms":30000,"stalled":true,"abandoned":false,"last_fraction":0.5,"last_label":"previous Run"})).unwrap();
    app.reconcile_invocation_progress(&[background]);
    app.on_event(progress_event(
        "tool_progress",
        Some("background"),
        serde_json::json!({"fraction":0.6,"label":"still running"}),
    ));
    app.on_event(progress_event(
        "tool_call_started",
        Some("background"),
        serde_json::json!({"depth":2,"version":9,"arguments":"[]"}),
    ));
    let background = &app.live_tools[1];
    assert_eq!(background.version, 9);
    assert_eq!(background.depth, 2);
    assert_eq!(
        background.progress,
        Some((Some(0.6), "still running".into()))
    );
    app.on_event(progress_event("invocation_stalled", Some("background"), serde_json::json!({"handle":"h-background","silent_secs":40,"last_fraction":0.6,"last_label":"still running"})));
    assert_eq!(app.live_tools[1].stalled_secs, Some(40));
    assert_eq!(app.live_tools[0].progress, None);
    app.on_event(Event::RunFinished {
        session_id: "s".into(),
        run_id: "r2".into(),
        status: "completed".into(),
    });
    assert_eq!(app.live_tools.len(), 1);
    app.on_event(progress_event(
        "tool_progress",
        Some("background"),
        serde_json::json!({"run_id":"","fraction":0.7,"label":"manual context"}),
    ));
    assert_eq!(app.live_tools[0].progress.as_ref().unwrap().0, Some(0.7));
    app.on_event(progress_event(
        "tool_call_finished",
        Some("background"),
        serde_json::json!({"ok":true}),
    ));
    app.on_event(progress_event(
        "tool_call_started",
        Some("background"),
        serde_json::json!({}),
    ));
    app.on_event(progress_event(
        "invocation_pending",
        Some("background"),
        serde_json::json!({"handle":"h-again"}),
    ));
    app.on_event(progress_event(
        "invocation_pending",
        Some("unknown-old"),
        serde_json::json!({"handle":"h-old"}),
    ));
    app.on_event(progress_event(
        "tool_call_started",
        Some("unknown-old"),
        serde_json::json!({}),
    ));
    assert_eq!(app.live_tools.len(), 1);
    assert_eq!(app.live_tools[0].status, "ok");
}
