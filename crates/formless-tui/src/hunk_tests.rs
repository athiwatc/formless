// Included in the actual App test module: recording transport, render and key
// handlers exercise the reachable UI rather than a parallel controller model.
fn hunk_fixture() -> HunkReview {
    let mut file = formless_core::hunk_review::ReviewDocument::build(
        "src/ไทย.rs",
        Some(b"old\n"),
        Some(b"new\n"),
    )
    .view()
    .clone();
    file.id = "opaque-file".into();
    file.hunks[0].id = "opaque-hunk".into();
    let mut other = file.clone();
    other.id = "opaque-file-2".into();
    other.path = "other.rs".into();
    other.hunks[0].id = "opaque-hunk-2".into();
    let mut binary = file.clone();
    binary.id = "binary".into();
    binary.path = "image.png".into();
    binary.exclusion_reason = Some("binary: byte-exact rewind only".into());
    // A defensive client never edits an excluded file, even if a malformed
    // response happens to carry hunk rows for it.
    binary.hunks[0].id = "excluded-hunk".into();
    HunkReview {
        review_id: "opaque-review".into(),
        session_id: "s1".into(),
        run_id: Some("run1".into()),
        tool_id: None,
        base_version: None,
        original_candidate_hash: None,
        candidate_hash: None,
        draft_code: None,
        files: vec![file, binary, other],
        undo_changes: vec![],
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(20)).to_rfc3339(),
        warnings: vec![],
    }
}

fn hunk_test_app() -> (App, mpsc::Sender<ServerMessage>, mpsc::Receiver<String>) {
    let mut app = test_app();
    app.current = Some("s1".into());
    app.input.set("@private.txt unsent draft");
    app.attachments
        .push(("private.txt".into(), vec![0, 1, 255]));
    let (client, sent) = DaemonClient::recording();
    let (tx, rx) = mpsc::channel();
    app.client = Some(client);
    app.inbox = Some(rx);
    (app, tx, sent)
}

fn hunk_next(sent: &mpsc::Receiver<String>) -> RequestEnvelope {
    serde_json::from_str(&sent.try_recv().expect("one expected UI request")).unwrap()
}

fn hunk_open(
    app: &mut App,
    tx: &mpsc::Sender<ServerMessage>,
    sent: &mpsc::Receiver<String>,
    review: &HunkReview,
) {
    app.slash("review");
    let request = hunk_next(sent);
    assert!(
        matches!(request.request,Request::InspectRunReview{ref session_id,run_id:None} if session_id=="s1")
    );
    assert!(
        sent.try_recv().is_err(),
        "opening review performs no mutation"
    );
    branch_summary_reply(tx, request.id, serde_json::to_value(review).unwrap());
    app.drain_daemon();
    assert!(app.hunk_review_is_visible());
}

#[test]
fn hunk_comment_draws_the_insertion_point_and_scrolls_escaped_text() {
    use ratatui::backend::{Backend, TestBackend};
    let (mut app, tx, sent) = hunk_test_app();
    hunk_open(&mut app, &tx, &sent, &hunk_fixture());
    app.key(key(KeyCode::Char('c')));
    for c in "abc".chars() {
        app.key(key(KeyCode::Char(c)));
    }
    app.key(key(KeyCode::Left));
    app.key(key(KeyCode::Left));
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let caret = terminal.backend_mut().get_cursor_position().unwrap();
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(caret.x - 1, caret.y)].symbol(), "a");
    assert_eq!(buffer[(caret.x, caret.y)].symbol(), "b");

    app.key(key(KeyCode::Char('X')));
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    terminal
        .backend_mut()
        .assert_cursor_position((caret.x + 1, caret.y));
    assert_eq!(
        terminal.backend().buffer()[(caret.x, caret.y)].symbol(),
        "X"
    );
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    app.paste("界e\u{301}\u{202E}\u{1B}");
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    let second = terminal.backend_mut().get_cursor_position().unwrap();
    let rendered = "界e\u{301}⟨U+202E⟩⟨U+001B⟩";
    assert_eq!(second.y, caret.y + 1);
    assert_eq!(second.x, 1 + text::width(rendered) as u16);
    assert_eq!(
        terminal.backend().buffer()[(second.x, second.y)].symbol(),
        "b"
    );
    assert_eq!(terminal.backend().buffer()[(1, second.y)].symbol(), "界");

    app.paste(&"abc界\n".repeat(4_000));
    app.hunk_review.error = Some("comment rejected".into());
    let mut narrow = Terminal::new(TestBackend::new(20, 10)).unwrap();
    narrow.draw(|frame| draw(frame, &mut app)).unwrap();
    let visible = narrow.backend_mut().get_cursor_position().unwrap();
    assert!(visible.x < 19 && visible.y < 8, "{visible:?}");
    assert!(render(&mut app, 60, 24).contains("Server: comment rejected"));
    app.hunk_review.busy = true;
    narrow.backend_mut().set_cursor_position((0, 0)).unwrap();
    narrow.draw(|frame| draw(frame, &mut app)).unwrap();
    narrow.backend_mut().assert_cursor_position((0, 0));
    for size in [1, 2, 3] {
        let mut tiny = Terminal::new(TestBackend::new(size, size)).unwrap();
        tiny.draw(|frame| draw(frame, &mut app)).unwrap();
        tiny.backend_mut().assert_cursor_position((0, 0));
    }
}

#[test]
fn hunk_review_scroll_stops_at_rendered_edges_and_recovers_immediately() {
    use crossterm::event::{MouseEvent, MouseEventKind};

    let (mut app, tx, sent) = hunk_test_app();
    let mut review = hunk_fixture();
    let hunk = &mut review.files[0].hunks[0];
    hunk.new_lines = 40;
    hunk.lines = (1..=40)
        .map(|n| formless_protocol::ReviewLine {
            kind: "added".into(),
            old_line: None,
            new_line: Some(n),
            text: vec![formless_protocol::ReviewTextSpan {
                text: format!("diff-row-{n:02} {}\u{1b} tail", "界".repeat(45)),
                changed: false,
            }],
            ending: "\n".into(),
        })
        .collect();
    let second_lines = hunk.lines.clone();
    review.files[2].hunks[0].lines = second_lines;
    review.files[2].hunks[0].new_lines = 40;
    hunk_open(&mut app, &tx, &sent, &review);
    let (width, height) = (60, 23);
    let first = render(&mut app, width, height);
    assert!(first.contains("diff-row-01") && !first.contains("diff-row-40"));
    for _ in 0..50 {
        app.key(key(KeyCode::PageDown));
        app.key(key(KeyCode::Down));
        app.mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 4,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
    }
    let bottom = render(&mut app, width, height);
    assert!(bottom.contains("diff-row-40"));
    let offset = app.hunk_review.scroll;
    app.key(key(KeyCode::Up));
    let up = render(&mut app, width, height);
    assert_eq!(app.hunk_review.scroll, offset - 1);
    let first_diff = |screen: &str| {
        screen
            .lines()
            .find(|line| line.contains("diff-row-"))
            .unwrap()
            .to_owned()
    };
    assert_ne!(first_diff(&bottom), first_diff(&up));

    app.key(key(KeyCode::End));
    assert_eq!(render(&mut app, width, height), bottom);
    app.key(key(KeyCode::PageUp));
    render(&mut app, width, height);
    assert!(app.hunk_review.scroll < offset);
    app.key(key(KeyCode::Home));
    assert_eq!(render(&mut app, width, height), first);

    for _ in 0..50 {
        app.key(key(KeyCode::Right));
    }
    let right = render(&mut app, width, height);
    assert!(right.contains("⟨U+001B⟩ tail  ⟨LF⟩"), "{right}");
    let offset = app.hunk_review.left;
    assert!(offset > 0);
    app.key(key(KeyCode::Left));
    assert_ne!(render(&mut app, width, height), right);
    assert_eq!(app.hunk_review.left, offset - 8);

    app.key(key(KeyCode::End));
    render(&mut app, 250, 60);
    assert_eq!((app.hunk_review.scroll, app.hunk_review.left), (0, 0));
    // Focus and resize can both change before the next frame.
    app.key(key(KeyCode::Char('n')));
    app.key(key(KeyCode::End));
    let second = render(&mut app, width, height);
    assert_eq!(app.hunk_review.focus, 1);
    assert!(second.contains("diff-row-40") && !second.contains("diff-row-01"));
    assert!(
        sent.try_recv().is_err(),
        "viewport navigation sends no requests"
    );
}

#[test]
fn hunk_review_reachable_keys_use_opaque_ids_hashes_and_preserve_composer() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    let mut review = hunk_fixture();
    hunk_open(&mut app, &tx, &sent, &review);
    let screen = render(&mut app, 110, 30);
    for expected in [
        "Run file review",
        "image.png",
        "binary:",
        "n/p hunk",
        "N/P pending",
        "a keep",
        "r reject",
        "u undo",
        "c comment",
        "old",
        "new",
    ] {
        assert!(screen.contains(expected), "missing {expected}: {screen}");
    }
    assert!(screen.contains("2 pending · 0 kept · 0 rejected"));
    app.key(key(KeyCode::Char('N')));
    app.key(key(KeyCode::Char('a')));
    let accept = hunk_next(&sent);
    assert!(
        matches!(accept.request,Request::DecideReviewHunk{ref review_id,ref file_id,ref hunk_id,ref expected_hash,ref decision}
        if review_id=="opaque-review"&&file_id=="opaque-file-2"&&hunk_id=="opaque-hunk-2"&&expected_hash==&review.files[2].after_hash&&decision=="accept")
    );
    app.key(key(KeyCode::Char('r')));
    assert!(
        sent.try_recv().is_err(),
        "one physical mutation pending at a time"
    );
    review.files[2].hunks[0].decision = "accepted".into();
    branch_summary_reply(
        &tx,
        accept.id,
        serde_json::to_value(ReviewDecisionResult {
            review: review.clone(),
            change_id: None,
            already_applied: false,
        })
        .unwrap(),
    );
    app.drain_daemon();
    assert!(render(&mut app, 110, 30).contains("1 pending · 1 kept · 0 rejected"));
    app.key(key(KeyCode::Char('P')));
    app.key(key(KeyCode::Char('r')));
    let reject = hunk_next(&sent);
    assert!(
        matches!(reject.request,Request::DecideReviewHunk{ref file_id,ref hunk_id,ref expected_hash,ref decision,..}
        if file_id=="opaque-file"&&hunk_id=="opaque-hunk"&&expected_hash==&review.files[0].after_hash&&decision=="reject")
    );
    review.files[0].after_hash = "reverted-image".into();
    review.files[0].hunks[0].decision = "rejected".into();
    review.undo_changes = vec![
        ReviewChangeInfo {
            change_id: "receipt-old".into(),
            file_id: "opaque-file".into(),
            hunk_id: "opaque-hunk".into(),
            description: "earlier".into(),
            undone: true,
        },
        ReviewChangeInfo {
            change_id: "receipt-current".into(),
            file_id: "opaque-file".into(),
            hunk_id: "opaque-hunk".into(),
            description: "reject exact hunk".into(),
            undone: false,
        },
    ];
    branch_summary_reply(
        &tx,
        reject.id,
        serde_json::to_value(ReviewDecisionResult {
            review: review.clone(),
            change_id: Some("receipt-current".into()),
            already_applied: false,
        })
        .unwrap(),
    );
    app.drain_daemon();
    let focus = app.hunk_review.focus;
    app.key(key(KeyCode::Char('N')));
    assert_eq!(app.hunk_review.focus, focus);
    assert!(sent.try_recv().is_err());
    assert_eq!(app.notes.last().unwrap(), "All hunks decided.");
    assert!(render(&mut app, 110, 30).contains("All hunks decided."));
    app.key(key(KeyCode::Char('a')));
    assert!(
        sent.try_recv().is_err(),
        "a kept label must not disguise reverted bytes"
    );
    assert!(app.notes.last().unwrap().contains("Undo the rejection"));
    app.key(key(KeyCode::Char('u')));
    let undo = hunk_next(&sent);
    assert!(
        matches!(undo.request,Request::UndoReviewChange{ref review_id,ref change_id,ref expected_hash}
        if review_id=="opaque-review"&&change_id=="receipt-current"&&expected_hash=="reverted-image")
    );
    app.key(key(KeyCode::Esc));
    assert!(app.hunk_review.context.is_none());
    branch_summary_reply(
        &tx,
        undo.id,
        serde_json::to_value(ReviewDecisionResult {
            review,
            change_id: Some("receipt-current".into()),
            already_applied: false,
        })
        .unwrap(),
    );
    app.drain_daemon();
    assert!(
        app.hunk_review.context.is_none(),
        "late mutation reply cannot reopen dismissed review"
    );
    assert_eq!(app.input.expanded(), "@private.txt unsent draft");
    assert_eq!(app.attachments[0].1, vec![0, 1, 255]);
    assert!(
        sent.try_recv().is_err(),
        "dismissal never retries a file write"
    );
}

#[test]
fn hunk_review_comments_are_literal_and_idle_reply_does_not_start_a_run() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    hunk_open(&mut app, &tx, &sent, &hunk_fixture());
    app.vim_mode = true;
    app.key(key(KeyCode::Char('c')));
    app.paste("/tmp/not-an-upload\nPlease keep ไทย");
    assert_eq!(
        app.attachments.len(),
        1,
        "comment paste is not an attachment path"
    );
    app.key(key(KeyCode::Enter));
    let request = hunk_next(&sent);
    assert!(
        matches!(request.request,Request::CommentReviewHunk{ref review_id,ref file_id,ref hunk_id,ref comment,..}
        if review_id=="opaque-review"&&file_id=="opaque-file"&&hunk_id=="opaque-hunk"&&comment=="/tmp/not-an-upload\nPlease keep ไทย")
    );
    let message = MessageInfo {
        id: "review-comment".into(),
        session_id: "s1".into(),
        seq: 2,
        role: "user".into(),
        content: "Server path:line context + comment".into(),
        created_at: "".into(),
        meta: None,
    };
    branch_summary_reply(
        &tx,
        request.id,
        serde_json::to_value(ReviewCommentResult {
            message,
            steered: false,
        })
        .unwrap(),
    );
    app.drain_daemon();
    assert!(app
        .messages
        .iter()
        .any(|message| message.id == "review-comment"));
    assert!(app.notes.last().unwrap().contains("no Run was started"));
    assert!(sent.try_recv().is_err());
    assert_eq!(app.input.expanded(), "@private.txt unsent draft");
}

#[test]
fn hunk_review_wrong_owner_hidden_modal_and_reconnect_cannot_mutate() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    app.slash("review");
    let inspect = hunk_next(&sent);
    let mut wrong = hunk_fixture();
    wrong.session_id = "other".into();
    branch_summary_reply(&tx, inspect.id, serde_json::to_value(wrong).unwrap());
    app.drain_daemon();
    assert!(app.hunk_review.context.is_none());
    let review = hunk_fixture();
    hunk_open(&mut app, &tx, &sent, &review);
    app.pager = Some(Pager {
        title: "Other visible page".into(),
        lines: vec!["data".into()],
        top: 0,
        left: 0,
        viewport: Default::default(),
        styled: vec![],
    });
    for c in ['a', 'r', 'u', 'c'] {
        app.key(key(KeyCode::Char(c)));
    }
    assert!(
        sent.try_recv().is_err(),
        "hidden pane has no action authority"
    );
    app.key(key(KeyCode::Esc));
    assert!(app.hunk_review_is_visible());
    app.key(key(KeyCode::Char('r')));
    let reject = hunk_next(&sent);
    app.daemon_lost();
    // A new transport receiving a delayed old-connection frame has no pending
    // intent for it. Reconnect is never permission to repeat the write.
    let (client, fresh_sent) = DaemonClient::recording();
    let (fresh_tx, fresh_rx) = mpsc::channel();
    app.client = Some(client);
    app.inbox = Some(fresh_rx);
    app.offline = None;
    branch_summary_reply(
        &fresh_tx,
        reject.id,
        serde_json::to_value(ReviewDecisionResult {
            review,
            change_id: Some("receipt".into()),
            already_applied: false,
        })
        .unwrap(),
    );
    app.drain_daemon();
    assert!(app.hunk_review.context.is_none());
    assert!(
        fresh_sent.try_recv().is_err(),
        "reconnect does not repeat a side effect"
    );
    assert_eq!(app.input.expanded(), "@private.txt unsent draft");
}

fn hunk_tool_editor(app: &mut App) {
    app.editor_generation = 7;
    app.editor = Some(EditorState {
        viewport: Default::default(),
        draft: None,
        original: None,
        confirm_exit: false,
        review_base_version: Some(3),
        vim: vim::Vim::default(),
        tool_id: "tool-id".into(),
        name: "Test Tool".into(),
        chars: "new\n".chars().collect(),
        cursor: 4,
        mode: EditMode::Code,
    });
    app.editor_review = Some(EditorReview {
        scroll: Default::default(),
        diff: "-old\n+new".into(),
        verdict: "Passes checks".into(),
        ok: true,
    });
}

fn hunk_tool_fixture() -> HunkReview {
    let mut review = hunk_fixture();
    review.run_id = None;
    review.tool_id = Some("tool-id".into());
    review.base_version = Some(3);
    review.files.truncate(1);
    let hash = formless_core::hunk_review::image_hash(Some(b"new\n"));
    review.original_candidate_hash = Some(hash.clone());
    review.candidate_hash = Some(hash.clone());
    review.draft_code = Some("new\n".into());
    review.files[0].after_hash = hash;
    review
}

#[test]
fn hunk_review_tool_replaces_only_matching_draft_and_ctrl_s_keeps_save_gate() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    hunk_tool_editor(&mut app);
    app.vim_mode = true;
    app.key(key(KeyCode::Char('h')));
    let prepare = hunk_next(&sent);
    assert!(
        matches!(prepare.request,Request::PrepareToolReview{ref session_id,ref tool_id,base_version:3,ref candidate_code}
        if session_id=="s1"&&tool_id=="tool-id"&&candidate_code=="new\n")
    );
    let mut review = hunk_tool_fixture();
    branch_summary_reply(&tx, prepare.id, serde_json::to_value(&review).unwrap());
    app.drain_daemon();
    assert!(render(&mut app, 100, 28).contains("Tool draft · not saved"));
    app.key(key(KeyCode::Char('r')));
    let reject = hunk_next(&sent);
    review.draft_code = Some("old\n".into());
    let hash = formless_core::hunk_review::image_hash(Some(b"old\n"));
    review.candidate_hash = Some(hash.clone());
    review.files[0].after_hash = hash;
    review.files[0].hunks[0].decision = "rejected".into();
    branch_summary_reply(
        &tx,
        reject.id,
        serde_json::to_value(ReviewDecisionResult {
            review,
            change_id: Some("tool-receipt".into()),
            already_applied: false,
        })
        .unwrap(),
    );
    app.drain_daemon();
    assert_eq!(app.editor.as_ref().unwrap().text(), "old\n");
    assert!(sent.try_recv().is_err(), "hunk change edits a draft only");
    app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    let save = hunk_next(&sent);
    assert!(
        matches!(save.request,Request::UpdateToolCode{ref id,ref code} if id=="tool-id"&&code=="old\n")
    );
    assert!(app.hunk_review.context.is_none());
    let generation = app.editor_generation;
    app.key(KeyEvent::new(KeyCode::Char('_'), KeyModifiers::CONTROL));
    assert_eq!(app.editor.as_ref().unwrap().text(), "new\n");
    assert!(app.editor_generation > generation);
    assert_eq!(app.editor.as_ref().unwrap().review_base_version, Some(3));
    app.key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::ALT));
    assert_eq!(app.editor.as_ref().unwrap().text(), "old\n");
    assert!(
        sent.try_recv().is_err(),
        "undo/redo never saves a Tool automatically"
    );
}

#[test]
fn hunk_review_tool_aba_editor_generation_and_wrong_reply_hash_are_refused() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    hunk_tool_editor(&mut app);
    app.key(key(KeyCode::Char('h')));
    let prepare = hunk_next(&sent);
    // Same Tool/version/text is insufficient: it is a newer editor instance.
    app.editor_generation += 1;
    branch_summary_reply(
        &tx,
        prepare.id,
        serde_json::to_value(hunk_tool_fixture()).unwrap(),
    );
    app.drain_daemon();
    assert!(app.hunk_review.document.is_none());
    assert_eq!(app.editor.as_ref().unwrap().text(), "new\n");
    app.hunk_review.clear();
    hunk_tool_editor(&mut app);
    app.key(key(KeyCode::Char('h')));
    let prepare = hunk_next(&sent);
    let mut wrong = hunk_tool_fixture();
    wrong.candidate_hash = Some("forged".into());
    branch_summary_reply(&tx, prepare.id, serde_json::to_value(wrong).unwrap());
    app.drain_daemon();
    assert!(app.hunk_review.context.is_none());
    assert_eq!(app.editor.as_ref().unwrap().text(), "new\n");
    assert!(sent.try_recv().is_err());
}

#[test]
fn hunk_review_excluded_files_can_be_inspected_but_never_edited() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    let mut review = hunk_fixture();
    let template = review.files[0].clone();
    review.files = (0..20)
        .map(|i| {
            let mut file = template.clone();
            file.id = format!("excluded-{i}");
            file.path = format!("generated-{i}.rs");
            file.exclusion_reason = Some("Generated file".into());
            file.hunks[0].id = format!("not-editable-{i}");
            file
        })
        .collect();
    hunk_open(&mut app, &tx, &sent, &review);
    for c in ['n', 'p', 'a', 'r', 'u', 'c'] {
        app.key(key(KeyCode::Char(c)));
    }
    assert!(sent.try_recv().is_err());
    assert!(app.hunk_review.comment.is_none());
    app.key(key(KeyCode::Char('f')));
    assert!(
        app.pager
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.contains("generated-19.rs") && line.contains("Generated file")),
        "every excluded file has a discoverable reason"
    );
    app.key(key(KeyCode::End));
    assert!(render(&mut app, 110, 16).contains("generated-19.rs"));
    app.key(key(KeyCode::Char('r')));
    assert!(sent.try_recv().is_err());
    app.key(key(KeyCode::Esc));
    assert!(app.hunk_review_is_visible());
    assert_eq!(app.input.expanded(), "@private.txt unsent draft");
}

#[test]
fn hunk_review_expired_plan_or_changed_tool_cannot_send_a_mutation() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    let mut review = hunk_fixture();
    hunk_open(&mut app, &tx, &sent, &review);
    review.expires_at = "2000-01-01T00:00:00Z".into();
    app.hunk_review.document = Some(review);
    for c in ['a', 'r', 'c', 'u'] {
        app.key(key(KeyCode::Char(c)));
    }
    assert!(sent.try_recv().is_err());
    app.hunk_review.clear();
    hunk_tool_editor(&mut app);
    app.key(key(KeyCode::Char('h')));
    let prepare = hunk_next(&sent);
    app.on_event(Event::ToolDeleted {
        id: "tool-id".into(),
    });
    branch_summary_reply(
        &tx,
        prepare.id,
        serde_json::to_value(hunk_tool_fixture()).unwrap(),
    );
    app.drain_daemon();
    assert!(app.hunk_review.context.is_none());
    assert_eq!(app.editor.as_ref().unwrap().text(), "new\n");
    assert!(sent.try_recv().is_err());
}

#[test]
fn hunk_review_old_syntax_preview_cannot_replace_a_newer_editor_review() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    hunk_tool_editor(&mut app);
    app.editor_review = None;
    app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    let old = hunk_next(&sent);
    assert!(
        matches!(old.request,Request::PreviewToolCode{ref id,ref code} if id=="tool-id"&&code=="new\n")
    );
    // The same editor now contains a newer candidate; an old success is not
    // evidence that this candidate has passed syntax/smoke checks.
    app.paste("changed");
    let current = app.editor.as_ref().unwrap().text();
    branch_summary_reply(
        &tx,
        old.id,
        serde_json::json!({"ok":true,"diff":"old candidate diff"}),
    );
    app.drain_daemon();
    assert!(app.editor_review.is_none());
    assert_eq!(app.editor.as_ref().unwrap().text(), current);
    app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    let fresh = hunk_next(&sent);
    branch_summary_reply(
        &tx,
        fresh.id,
        serde_json::json!({"ok":true,"diff":"fresh candidate diff"}),
    );
    app.drain_daemon();
    assert_eq!(
        app.editor_review.as_ref().unwrap().diff,
        "fresh candidate diff"
    );
    assert!(sent.try_recv().is_err());
}

#[test]
fn hunk_review_timeout_is_uncertain_and_late_reply_never_replays() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    let review = hunk_fixture();
    hunk_open(&mut app, &tx, &sent, &review);
    app.key(key(KeyCode::Char('r')));
    let reject = hunk_next(&sent);
    app.pending_since.insert(
        reject.id,
        Instant::now() - REQUEST_DEADLINE - Duration::from_secs(1),
    );
    app.expire_pending();
    assert!(app.hunk_review.context.is_none());
    assert!(app.notes.last().unwrap().contains("may still finish"));
    branch_summary_reply(
        &tx,
        reject.id,
        serde_json::to_value(ReviewDecisionResult {
            review,
            change_id: Some("durable-receipt".into()),
            already_applied: false,
        })
        .unwrap(),
    );
    app.drain_daemon();
    assert!(app.hunk_review.context.is_none());
    assert!(sent.try_recv().is_err());
    assert_eq!(app.input.expanded(), "@private.txt unsent draft");
    assert_eq!(app.attachments[0].1, vec![0, 1, 255]);
}

#[test]
fn hunk_review_modal_escape_owns_the_key_before_vim_or_composer_control_c() {
    let _env = credential_env::EnvGuard::isolated();
    for vim in [false, true] {
        for dismiss in [
            key(KeyCode::Esc),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let (mut app, tx, sent) = hunk_test_app();
            hunk_tool_editor(&mut app);
            app.vim_mode = vim;
            app.key(key(KeyCode::Char('h')));
            let prepare = hunk_next(&sent);
            branch_summary_reply(
                &tx,
                prepare.id,
                serde_json::to_value(hunk_tool_fixture()).unwrap(),
            );
            app.drain_daemon();
            app.key(dismiss);
            assert!(app.hunk_review.context.is_none());
            assert_eq!(app.editor.as_ref().unwrap().text(), "new\n");
            assert_eq!(
                app.editor.as_ref().unwrap().vim.mode,
                vim::Mode::Insert,
                "modal closes before editor Vim state changes"
            );
            assert_eq!(app.input.expanded(), "@private.txt unsent draft");
            assert_eq!(app.attachments[0].1, vec![0, 1, 255]);
            assert!(!app.should_quit);
            assert!(sent.try_recv().is_err());
            if vim {
                app.key(key(KeyCode::Esc));
                assert_eq!(
                    app.editor.as_ref().unwrap().vim.mode,
                    vim::Mode::Normal,
                    "next key belongs to editor again"
                );
                assert!(sent.try_recv().is_err());
            }
        }
    }
}

#[test]
fn hunk_review_refused_read_and_comment_show_errors_without_losing_drafts() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    app.slash("review");
    let inspect = hunk_next(&sent);
    tx.send(ServerMessage::Response {
        id: inspect.id,
        ok: false,
        result: None,
        error: Some("Run is not reviewable".into()),
    })
    .unwrap();
    app.drain_daemon();
    let screen = render(&mut app, 100, 24);
    assert!(screen.contains("Review could not be read: Run is not reviewable"));
    assert!(
        !screen.contains("Reading a server-owned review"),
        "a refused RPC is not still loading"
    );
    app.key(key(KeyCode::Esc));
    hunk_open(&mut app, &tx, &sent, &hunk_fixture());
    app.key(key(KeyCode::Char('c')));
    app.paste("keep this unsent comment");
    app.key(key(KeyCode::Enter));
    let comment = hunk_next(&sent);
    tx.send(ServerMessage::Response {
        id: comment.id,
        ok: false,
        result: None,
        error: Some("File changed; reopen review".into()),
    })
    .unwrap();
    app.drain_daemon();
    let screen = render(&mut app, 110, 30);
    assert!(screen.contains("File changed; reopen review"));
    assert_eq!(
        app.hunk_review.comment.as_ref().unwrap().expanded(),
        "keep this unsent comment"
    );
    assert_eq!(app.input.expanded(), "@private.txt unsent draft");
    assert!(sent.try_recv().is_err());
}

#[test]
fn hunk_review_comment_limit_counts_utf8_bytes_and_keeps_the_complete_draft() {
    let _env = credential_env::EnvGuard::isolated();
    let (mut app, tx, sent) = hunk_test_app();
    hunk_open(&mut app, &tx, &sent, &hunk_fixture());
    app.key(key(KeyCode::Char('c')));
    let prefix = "ไทย".repeat(455); // 4095 UTF-8 bytes, not 4095 characters.
    let over = format!("{prefix}ab");
    assert_eq!(over.len(), 4097);
    app.paste(&over);
    app.key(key(KeyCode::Enter));
    assert!(
        sent.try_recv().is_err(),
        "over-limit comments never reach the daemon"
    );
    assert_eq!(app.hunk_review.comment.as_ref().unwrap().expanded(), over);
    assert!(render(&mut app, 110, 30).contains("4096 UTF-8 bytes"));
    let exact = format!("{prefix}a");
    assert_eq!(exact.len(), 4096);
    app.hunk_review.comment.as_mut().unwrap().set(&exact);
    app.key(key(KeyCode::Enter));
    let request = hunk_next(&sent);
    assert!(
        matches!(request.request,Request::CommentReviewHunk{ref comment,..} if comment==&exact)
    );
    assert!(sent.try_recv().is_err());
}
