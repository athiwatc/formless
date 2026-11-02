fn coverage_file(status: &str) -> formless_protocol::RunFileInfo {
    formless_protocol::RunFileInfo {
        path: "unsafe\u{1b}[2J.txt".into(),
        status: status.into(),
        revertible: false,
        kind: "symlink".into(),
        revert_reason: Some("symlink target unavailable\u{1b}[2J".into()),
        diff: None,
    }
}

#[test]
fn tracked_path_notes_keep_counts_and_zero_coverage_truthful() {
    let zero = run_files_text(&[], true);
    assert!(zero.starts_with("No tracked path changes."));
    assert!(zero.contains(RUN_FILES_CAVEAT));
    assert!(!zero.contains("0 files changed"));
    let partial = run_files_text(
        &[coverage_file("modified"), coverage_file("unchanged")],
        true,
    );
    assert!(partial.starts_with("1 tracked path(s) changed:"));
    assert!(partial.contains("attempts safe restoration"));
    assert!(!partial.contains('\u{1b}'));
    assert!(!run_files_text(&[], false).contains(RUN_FILES_CAVEAT));
    assert_eq!(
        run_files_text(&[coverage_file("unchanged")], false),
        "No current changes among tracked paths."
    );
    let pager = files_pager_lines(&[coverage_file("modified")], true).join("\n");
    assert!(pager.contains("symlink target unavailable"));
    assert!(pager.contains(RUN_FILES_CAVEAT));
    assert!(!pager.contains('\u{1b}'));
}

#[test]
fn recap_coverage_is_visible_collapsed_and_expanded() {
    let recap = formless_protocol::RunRecap {
        steps: 4,
        untracked_writes_possible: true,
        files: vec!["modified x".into()],
        ..Default::default()
    };
    for open in [false, true] {
        let text = recap_lines(&recap, open, false)
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("1 tracked paths"));
        assert!(text.contains(RUN_FILES_CAVEAT));
    }
    let event = Event::RunFilesChanged {
        session_id: "s".into(),
        run_id: "r".into(),
        files: vec![],
        untracked_writes_possible: true,
    };
    let mut streaming = true;
    let text = plain::format_event(&event, &mut streaming).unwrap();
    assert!(text.starts_with('\n'));
    assert!(text.contains(RUN_FILES_CAVEAT));
    assert!(!streaming);
    let event = Event::RunRecap {
        session_id: "s".into(),
        run_id: "r".into(),
        recap,
    };
    assert!(plain::format_event(&event, &mut streaming)
        .unwrap()
        .contains(RUN_FILES_CAVEAT));
}

#[test]
fn actual_tui_event_keeps_coverage_notice_with_no_tracked_rows() {
    let mut app = test_app();
    app.current = Some("s".into());
    app.on_event(Event::RunFilesChanged {
        session_id: "s".into(),
        run_id: "r".into(),
        files: vec![],
        untracked_writes_possible: true,
    });
    assert!(app.notes.last().unwrap().contains(RUN_FILES_CAVEAT));
    let before = app.notes.len();
    app.on_event(Event::RunFilesChanged {
        session_id: "other".into(),
        run_id: "r".into(),
        files: vec![],
        untracked_writes_possible: true,
    });
    assert_eq!(app.notes.len(), before);
    app.on_event(Event::RunRecap {
        session_id: "s".into(),
        run_id: "r".into(),
        recap: formless_protocol::RunRecap {
            untracked_writes_possible: true,
            ..Default::default()
        },
    });
    assert!(app.recap.as_ref().unwrap().untracked_writes_possible);
    let _ = std::fs::remove_dir_all(app.paths.data_dir.parent().unwrap());
}
