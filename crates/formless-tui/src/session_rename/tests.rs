use super::*;
use crate::tests::{forecast_reply, reconnect_approval, test_app};
use serde_json::{json, Value};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn recorded_app() -> (App, mpsc::Sender<ServerMessage>, mpsc::Receiver<String>) {
    let mut app = test_app();
    let (client, sent) = DaemonClient::recording();
    let (replies, inbox) = mpsc::channel();
    app.client = Some(client);
    app.inbox = Some(inbox);
    (app, replies, sent)
}

fn request(sent: &mpsc::Receiver<String>) -> RequestEnvelope {
    serde_json::from_str(&sent.try_recv().unwrap()).unwrap()
}

fn reply(app: &mut App, tx: &mpsc::Sender<ServerMessage>, id: u64, result: Value) {
    tx.send(ServerMessage::Response {
        id,
        ok: true,
        result: Some(result),
        error: None,
    })
    .unwrap();
    app.drain_daemon();
}

fn editor(app: &App) -> &input::InputLine {
    app.renaming.as_ref().unwrap().input.as_ref().unwrap()
}

fn name(app: &App) -> String {
    editor(app).expanded()
}

fn session(id: &str, name: &str) -> SessionInfo {
    serde_json::from_value(json!({
        "id":id, "name":name, "created_at":"", "updated_at":""
    }))
    .unwrap()
}

#[test]
fn rename_another_session_keeps_the_complete_composer_and_draft_store() {
    let (mut app, _replies, sent) = recorded_app();
    std::fs::create_dir_all(&app.paths.data_dir).unwrap();
    app.sessions = vec![session("other", "Other name")];
    app.input.set("draft before ");
    app.input.paste(&"wide ไทย 界\n".repeat(30));
    assert!(app.input.to_string().contains("[pasted"));
    app.input.prepare_selection(true);
    app.input.left();
    let text = app.input.expanded();
    let display = app.input.to_string();
    let cursor = app.input.cursor_byte();
    let selection = app.input.selection();
    assert!(selection.is_some());
    app.attachments.push(("current.txt".into(), vec![1]));
    let mut other = Draft::default();
    other.input.set("other unsent message");
    other.attachments.push(("other.txt".into(), vec![2]));
    app.drafts.insert("other".into(), other);
    app.save_drafts();
    let path = drafts_file(&app.paths);
    let old: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    app.open_view(View::Sessions);
    assert!(matches!(request(&sent).request, Request::ListSessions));
    app.key(key(KeyCode::Char('r')));
    assert_eq!(app.view, View::Chat);
    assert_eq!(name(&app), "Other name");
    app.save_drafts();
    let during: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(during, old);

    app.key(KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT));
    app.paste("  /literal\r\nnew name  ");
    app.key(key(KeyCode::Enter));
    assert!(
        matches!(request(&sent).request, Request::RenameSession {id,name}
        if id == "other" && name == "/literal new name")
    );
    assert!(sent.try_recv().is_err());
    assert!(app.renaming.is_none());
    assert_eq!(app.current.as_deref(), Some("s1"));
    assert_eq!(app.input.expanded(), text);
    assert_eq!(app.input.to_string(), display);
    assert_eq!(app.input.cursor_byte(), cursor);
    assert_eq!(app.input.selection(), selection);
    assert_eq!(app.attachments, vec![("current.txt".into(), vec![1])]);
    assert_eq!(app.drafts["other"].input.expanded(), "other unsent message");
    assert_eq!(
        app.drafts["other"].attachments,
        vec![("other.txt".into(), vec![2])]
    );
    app.save_drafts();
    let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after, old);
}

#[test]
fn unchanged_rename_preserves_existing_name_and_collapsed_composer_pastes() {
    let (mut app, _tx, sent) = recorded_app();
    app.input.paste(&"large payload\n".repeat(30));
    app.input.left();
    let before = (
        app.input.to_string(),
        app.input.cursor_byte(),
        app.input.expanded(),
    );
    assert!(before.0.contains("[pasted"));

    let existing = "  name\twith\nseparators  ";
    app.start_rename("other", existing);
    assert_eq!(name(&app), existing);
    app.key(key(KeyCode::Enter));
    assert!(
        matches!(request(&sent).request, Request::RenameSession {id,name}
        if id == "other" && name == existing.trim())
    );
    assert_eq!(
        (
            app.input.to_string(),
            app.input.cursor_byte(),
            app.input.expanded()
        ),
        before
    );
    assert!(sent.try_recv().is_err());
}

#[test]
fn rename_enter_waits_for_the_approval_to_yield_input_focus() {
    let (mut app, _tx, sent) = recorded_app();
    app.input.set("draft");
    app.start_rename("other", "renamed");
    app.on_event(reconnect_approval("synthetic", "s1"));
    app.key(key(KeyCode::Enter));
    app.paste(" ignored");
    assert_eq!(name(&app), "renamed");
    assert!(sent.try_recv().is_err());

    app.key(key(KeyCode::Tab));
    app.key(key(KeyCode::Enter));
    assert!(
        matches!(request(&sent).request, Request::RenameSession {id,name}
        if id == "other" && name == "renamed")
    );
    assert_eq!(app.approvals.len(), 1);
    assert_eq!(&*app.input, "draft");
    assert!(sent.try_recv().is_err());
}

#[test]
fn rename_cancellation_and_empty_entry_never_submit_or_clear_the_composer() {
    let (mut app, _tx, sent) = recorded_app();
    app.start_rename("s1", "old name");
    app.key(KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT));
    app.paste(" \r\n ");
    app.key(key(KeyCode::Enter));
    assert!(app.renaming.is_some());
    assert!(sent.try_recv().is_err());
    app.key(key(KeyCode::Esc));
    assert!(app.renaming.is_none());
    assert!(app.input.is_empty());

    app.input.set("keep this");
    app.start_rename("s1", "old name");
    app.paste(" newer");
    app.offline = Some((Instant::now(), Instant::now()));
    app.key(key(KeyCode::Enter));
    assert_eq!(name(&app), "old name newer");
    app.key(ctrl('c'));
    assert!(app.renaming.is_none());
    assert!(sent.try_recv().is_err());
    assert_eq!(&*app.input, "keep this");
}

#[test]
fn switching_views_and_sessions_discards_only_the_rename_editor() {
    let (mut app, _tx, sent) = recorded_app();
    app.input.set("original draft");
    app.attachments.push(("s1.txt".into(), vec![1]));
    app.sessions = vec![session("s1", "One"), session("s2", "Two")];
    let mut second = Draft::default();
    second.input.set("second draft");
    second.attachments.push(("s2.txt".into(), vec![2]));
    app.drafts.insert("s2".into(), second);
    app.start_rename("s2", "Two");
    app.paste(" edited");
    app.key(key(KeyCode::Tab));
    assert_eq!(app.view, View::Sessions);
    assert!(app.renaming.is_none());
    assert_eq!(&*app.input, "original draft");
    assert!(matches!(request(&sent).request, Request::ListSessions));

    app.start_rename("s1", "One");
    app.key(ctrl('k'));
    app.paste("Two");
    assert_eq!(name(&app), "One");
    app.key(key(KeyCode::Enter));
    assert_eq!(app.current.as_deref(), Some("s2"));
    assert!(app.renaming.is_none());
    assert_eq!(&*app.input, "second draft");
    assert_eq!(app.attachments, vec![("s2.txt".into(), vec![2])]);
    assert_eq!(app.drafts["s1"].input.expanded(), "original draft");
    app.input.set("new second draft");
    app.switch_session("s1".into());
    assert_eq!(&*app.input, "original draft");
    assert_eq!(app.attachments, vec![("s1.txt".into(), vec![1])]);
    assert_eq!(app.drafts["s2"].input.expanded(), "new second draft");
    assert!(!sent.try_iter().any(|line| matches!(
        serde_json::from_str::<RequestEnvelope>(&line)
            .unwrap()
            .request,
        Request::RenameSession { .. } | Request::SendMessage { .. }
    )));
}

#[test]
fn an_older_search_reply_preserves_the_active_rename_draft() {
    let (mut app, tx, sent) = recorded_app();
    app.slash("search");
    app.input.set("synthetic query");
    app.submit_chat(false);
    let search = request(&sent);
    assert!(
        matches!(search.request, Request::SearchSessions { query, .. }
        if query == "synthetic query")
    );
    app.input.set("new draft");
    app.start_rename("other", "fixture");
    app.paste(" suffix");
    let draft_name = name(&app);
    assert!(draft_name.contains("suffix"));
    reply(&mut app, &tx, search.id, json!([]));
    assert_eq!(app.view, View::Chat);
    assert_eq!(name(&app), draft_name);
    assert_eq!(app.renaming.as_ref().unwrap().target, "other");
    assert!(app.search_context.is_none());
    app.key(key(KeyCode::Esc));
    assert_eq!(app.view, View::Chat);
    assert!(app.renaming.is_none());
    assert_eq!(&*app.input, "new draft");
    assert!(sent.try_recv().is_err());
}

#[test]
fn canceled_or_replaced_suggestions_cannot_reopen_or_retarget_rename() {
    let (mut app, tx, sent) = recorded_app();
    app.input.set("composer");
    app.slash("rename auto");
    let canceled = request(&sent);
    assert!(matches!(canceled.request, Request::SuggestSessionName { id } if id == "s1"));
    assert!(app.renaming.as_ref().unwrap().suggesting());
    app.key(key(KeyCode::Esc));
    assert!(!app.pending_since.contains_key(&canceled.id));
    reply(&mut app, &tx, canceled.id, json!({"name":"canceled"}));
    assert!(app.renaming.is_none());

    app.slash("rename auto");
    let replaced = request(&sent);
    app.start_rename("other", "manual");
    reply(&mut app, &tx, replaced.id, json!({"name":"older"}));
    assert_eq!(app.renaming.as_ref().unwrap().target, "other");
    assert_eq!(name(&app), "manual");

    app.slash("rename auto");
    let old = request(&sent);
    app.slash("rename auto");
    let latest = request(&sent);
    reply(&mut app, &tx, latest.id, json!({"name":"newest"}));
    reply(&mut app, &tx, old.id, json!({"name":"older"}));
    assert_eq!(name(&app), "newest");
    assert_eq!(app.renaming.as_ref().unwrap().target, "s1");
    assert_eq!(&*app.input, "composer");
    app.key(key(KeyCode::Enter));
    assert!(
        matches!(request(&sent).request, Request::RenameSession {id,name}
        if id == "s1" && name == "newest")
    );
    assert!(sent.try_recv().is_err());

    app.slash("rename auto");
    let stale = request(&sent);
    app.switch_session("s2".into());
    app.switch_session("s1".into());
    reply(
        &mut app,
        &tx,
        stale.id,
        json!({"name":"from before switching"}),
    );
    assert!(app.renaming.is_none());
    assert_eq!(&*app.input, "composer");
}

#[test]
fn suggestion_errors_timeouts_and_deleted_targets_close_their_own_editors() {
    let (mut app, tx, sent) = recorded_app();
    app.input.set("keep the draft");
    app.slash("rename auto");
    let failed = request(&sent);
    tx.send(ServerMessage::Response {
        id: failed.id,
        ok: false,
        result: None,
        error: Some("fixture failure".into()),
    })
    .unwrap();
    app.drain_daemon();
    assert!(app
        .notes
        .iter()
        .any(|note| note == "daemon: fixture failure"));
    assert!(app.renaming.is_none());

    app.slash("rename auto");
    let late = request(&sent);
    app.pending_since.insert(
        late.id,
        Instant::now() - REQUEST_DEADLINE - Duration::from_secs(1),
    );
    reply(&mut app, &tx, late.id, json!({"name":"timed out"}));
    assert!(app.renaming.is_none());

    app.start_rename("other", "old");
    app.on_event(Event::SessionDeleted { id: "other".into() });
    assert!(app.renaming.is_none());
    assert_eq!(&*app.input, "keep the draft");

    app.slash("rename auto");
    let _ = request(&sent);
    app.daemon_lost();
    assert!(app.renaming.is_none());
    assert_eq!(&*app.input, "keep the draft");
}

#[test]
fn late_forecast_replies_only_send_the_original_composer() {
    for confirmation in [false, true] {
        let (mut app, tx, sent) = recorded_app();
        let mut cfg = Config::default();
        cfg.limits.forecast_confirm_usd = Some(0.0);
        cfg.save(&app.paths).unwrap();
        app.input.set("original message");
        app.submit_chat(false);
        let forecast = request(&sent);
        assert!(
            matches!(forecast.request, Request::ForecastRun {for_send:true, input}
            if input.draft == "original message")
        );

        app.start_rename("other", "fixture");
        app.paste(" name");
        reply(
            &mut app,
            &tx,
            forecast.id,
            serde_json::to_value(forecast_reply("fixture", confirmation)).unwrap(),
        );
        if confirmation {
            assert!(app.forecast.modal.is_some());
            app.paste("no action");
            assert_eq!(name(&app), "fixture name");
            app.key(key(KeyCode::Enter));
            let confirm = request(&sent);
            assert!(matches!(confirm.request, Request::ConfirmForecast { .. }));
            reply(&mut app, &tx, confirm.id, json!({"confirmed":true}));
        }
        assert!(
            matches!(request(&sent).request, Request::SendMessage {session_id,content,..}
            if session_id == "s1" && content == "original message")
        );
        assert_eq!(name(&app), "fixture name");
        assert!(app.input.is_empty());
        app.key(key(KeyCode::Enter));
        assert!(
            matches!(request(&sent).request, Request::RenameSession {id,name}
            if id == "other" && name == "fixture name")
        );
        assert!(sent.try_recv().is_err());
    }
}

#[test]
fn late_attachment_reply_uses_its_captured_message_and_keeps_the_newer_draft() {
    let (mut app, tx, sent) = recorded_app();
    app.input.set("message waiting for attachment");
    app.attachments.push(("fixture.txt".into(), vec![1, 2, 3]));
    app.submit_chat(false);
    let attachment = request(&sent);
    assert!(
        matches!(attachment.request, Request::AttachFile {session_id,name,..}
        if session_id == "s1" && name == "fixture.txt")
    );
    app.input.set("newer unsent draft");
    app.attachments.push(("held.txt".into(), vec![4]));
    app.start_rename("other", "new name");
    reply(&mut app, &tx, attachment.id, json!({"ok":true}));
    assert!(
        matches!(request(&sent).request, Request::SendMessage {session_id,content,..}
        if session_id == "s1" && content == "message waiting for attachment")
    );
    assert_eq!(name(&app), "new name");
    assert!(app.pending_send.is_none());
    app.key(key(KeyCode::Esc));
    assert_eq!(&*app.input, "newer unsent draft");
    assert_eq!(app.attachments, vec![("held.txt".into(), vec![4])]);
    assert!(sent.try_recv().is_err());
}

#[test]
fn rename_stays_open_when_attachment_timeout_restores_its_own_draft() {
    let (mut app, tx, sent) = recorded_app();
    app.input.set("unsent message");
    app.attachments.push(("fixture.txt".into(), vec![1, 2]));
    app.submit_chat(false);
    let attachment = request(&sent);
    assert!(matches!(attachment.request, Request::AttachFile { .. }));
    app.start_rename("other", "edited name");
    app.pending_since.insert(
        attachment.id,
        Instant::now() - REQUEST_DEADLINE - Duration::from_secs(1),
    );
    reply(&mut app, &tx, attachment.id, json!({"ok":true}));
    assert!(app.pending_send.is_none());
    assert_eq!(name(&app), "edited name");
    assert_eq!(&*app.input, "unsent message");
    assert_eq!(app.attachments, vec![("fixture.txt".into(), vec![1, 2])]);
    app.key(key(KeyCode::Esc));
    assert_eq!(&*app.input, "unsent message");
    assert!(sent.try_recv().is_err());
}

#[test]
fn rename_cursor_wrap_and_higher_overlays_use_their_own_input() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _tx, sent) = recorded_app();
    app.input.set("composer");
    app.input.left();
    let saved_cursor = app.input.cursor_byte();
    let long = format!("start {} end", "ไทย 界 ".repeat(28));
    app.start_rename("s1", &long);
    let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(30, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let rename = app.renaming.as_ref().unwrap();
    assert!(rename.height(30) > 3);
    let last = rename.hits.borrow().last().unwrap().0;
    assert!(last.y < 24);
    app.key(key(KeyCode::Home));
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let (rect, expected) = app.renaming.as_ref().unwrap().hits.borrow()[0];
    app.mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(editor(&app).cursor_byte(), expected);
    assert_eq!(app.input.cursor_byte(), saved_cursor);

    app.pager = Some(code_pager("Read only", "fixture", false));
    app.paste("ignored");
    assert_eq!(name(&app), long);
    app.key(key(KeyCode::Esc));
    app.key(ctrl('f'));
    app.paste("query");
    assert_eq!(app.find.as_ref().unwrap().query, "query");
    assert_eq!(name(&app), long);
    app.key(key(KeyCode::Esc));
    assert!(app.renaming.is_some());
    app.key(key(KeyCode::Esc));
    assert!(app.renaming.is_none());
    assert_eq!(&*app.input, "composer");
    assert!(sent.try_recv().is_err());
}
