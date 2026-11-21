//! One search destination, owned by its Session and exact load request.
use super::*;

pub(super) struct Target {
    pub(super) session: String,
    pub(super) message: String,
    seq: i64,
    role: String,
    pub(super) request: Option<u64>,
    pub(super) ready: bool,
    pub(super) call: Option<String>,
}

impl App {
    pub(super) fn start_search_jump(&mut self, hit: SearchHit) {
        // A newer destination supersedes earlier transcript reads for this
        // Session. Retire their identities now, so late replies cannot reset
        // the viewport or replace the transcript after the jump is consumed.
        let superseded: Vec<_> = self
            .run_state_reads
            .iter()
            .filter_map(|(&request, (session, _))| (session == &hit.session_id).then_some(request))
            .collect();
        for request in superseded {
            self.pending.remove(&request);
            self.pending_since.remove(&request);
            self.run_state_reads.remove(&request);
        }
        self.search_jump = Some(Target {
            session: hit.session_id,
            message: hit.message_id,
            seq: hit.seq,
            role: hit.role,
            request: None,
            ready: false,
            call: None,
        });
    }

    pub(super) fn loaded_search_jump(&mut self, request: u64) {
        let Some(target) = self
            .search_jump
            .as_ref()
            .filter(|target| target.request == Some(request) && self.is_current(&target.session))
        else {
            return;
        };
        // Prefer identity. Sequence fallback is only for an absent ID and
        // must uniquely match the same Session and role.
        let message = if target.message.is_empty() {
            let mut matches = self.messages.iter().filter(|message| {
                message.session_id == target.session
                    && message.seq == target.seq
                    && message.role == target.role
            });
            let first = matches.next();
            if matches.next().is_some() {
                None
            } else {
                first
            }
        } else {
            self.messages.iter().find(|message| {
                message.session_id == target.session && message.id == target.message
            })
        };
        if let Some(message) = message {
            let target = self.search_jump.as_mut().unwrap();
            target.message = message.id.clone();
            target.call = (message.role == "tool")
                .then(|| result_call_id(message))
                .flatten();
            target.ready = true;
        } else {
            self.follow_chat_bottom();
            self.notes.push(
                "Search hit is no longer in the loaded transcript; following the Session bottom."
                    .into(),
            );
        }
    }

    pub(super) fn failed_search_jump(&mut self, request: u64) {
        if self
            .search_jump
            .as_ref()
            .is_some_and(|target| target.request == Some(request))
        {
            self.follow_chat_bottom();
            self.notes
                .push("Search hit could not be loaded; following the Session bottom.".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn messages() -> Vec<MessageInfo> {
        (0..100)
            .map(|index| MessageInfo {
                id: format!("m{index}"),
                session_id: "search-session".into(),
                seq: index,
                role: if index % 2 == 0 { "user" } else { "assistant" }.into(),
                content: format!("message-{index:03}\nextra body {index}"),
                created_at: "2026-11-01T00:00:00Z".into(),
                meta: None,
            })
            .collect()
    }
    fn hit(id: &str, seq: i64) -> SearchHit {
        SearchHit {
            session_id: "search-session".into(),
            session_name: "Search Session".into(),
            message_id: id.into(),
            seq,
            role: if seq % 2 == 0 { "user" } else { "assistant" }.into(),
            snippet: "indexed terms do not need to match chat find".into(),
            created_at: String::new(),
        }
    }
    fn open(app: &mut App, sent: &mpsc::Receiver<String>, hit: SearchHit) -> u64 {
        app.view = View::Search;
        app.search_hits = vec![hit];
        app.search_idx = 0;
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let mut request = None;
        while let Ok(line) = sent.try_recv() {
            let envelope: RequestEnvelope = serde_json::from_str(&line).unwrap();
            if matches!(envelope.request, Request::GetSession { .. }) {
                request = Some(envelope.id);
            }
        }
        request.unwrap()
    }
    fn reply(app: &mut App, tx: &mpsc::Sender<ServerMessage>, id: u64, messages: Vec<MessageInfo>) {
        tx.send(ServerMessage::Response {
            id,
            ok: true,
            result: Some(serde_json::json!({"messages":messages})),
            error: None,
        })
        .unwrap();
        app.drain_daemon();
    }
    fn render(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| {
                draw_chat(frame, frame.area(), app);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
    fn app() -> (App, mpsc::Receiver<String>, mpsc::Sender<ServerMessage>) {
        let mut app = crate::tests::test_app();
        let (client, sent) = DaemonClient::recording();
        app.client = Some(client);
        let (tx, rx) = mpsc::channel();
        app.inbox = Some(rx);
        (app, sent, tx)
    }
    #[test]
    fn a_loaded_search_hit_scrolls_to_its_message_once_and_normal_scroll_keys_remain() {
        let (mut app, sent, tx) = app();
        let request = open(&mut app, &sent, hit("m10", 10));
        assert!(!app.search_jump.as_ref().unwrap().ready);
        reply(&mut app, &tx, request, messages());
        let expected = chat_lines(&app, 78, 18).search_line.unwrap();
        let screen = render(&mut app);
        assert!(screen.contains("message-010"), "{screen}");
        assert!(!screen.contains("message-099"));
        let top = app.chat_scroll_top.unwrap();
        assert!(top <= expected && expected < top + 18);
        assert!(top < app.chat_max_top());
        assert!(app.search_jump.is_none());
        app.scroll_chat_down(5);
        assert!(app.chat_scroll_top.unwrap() > top);
        app.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert!(app.chat_scroll_top.is_none());
        app.key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(app.chat_scroll_top.is_some());
    }
    #[test]
    fn request_identity_and_session_changes_prevent_late_jumps() {
        let (mut app, sent, tx) = app();
        let first = open(&mut app, &sent, hit("m10", 10));
        let second = open(&mut app, &sent, hit("m20", 20));
        reply(&mut app, &tx, first, messages());
        assert!(!app.search_jump.as_ref().unwrap().ready);
        assert!(
            app.messages.is_empty(),
            "superseded payload must be ignored"
        );
        assert!(!render(&mut app).contains("message-099"));
        reply(&mut app, &tx, second, messages());
        assert!(render(&mut app).contains("message-020"));
        let stale = open(&mut app, &sent, hit("m10", 10));
        app.switch_session("other-session".into());
        app.messages.clear();
        reply(&mut app, &tx, stale, messages());
        assert!(app.messages.is_empty());
        assert!(app.search_jump.is_none());
        let abandoned = open(&mut app, &sent, hit("m10", 10));
        app.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert!(app.search_jump.is_none());
        reply(&mut app, &tx, abandoned, messages());
        assert!(render(&mut app).contains("message-099"));
    }
    #[test]
    fn superseded_reply_after_the_selected_hit_is_drawn_cannot_reset_its_viewport() {
        let (mut app, sent, tx) = app();
        let first = open(&mut app, &sent, hit("m10", 10));
        let second = open(&mut app, &sent, hit("m20", 20));
        assert!(!app.pending.contains_key(&first));
        assert!(!app.pending_since.contains_key(&first));
        assert!(!app.run_state_reads.contains_key(&first));
        reply(&mut app, &tx, second, messages());
        assert!(render(&mut app).contains("message-020"));
        assert!(app.search_jump.is_none());
        let top = app.chat_scroll_top;
        let mut stale = messages();
        stale[20].content = "obsolete response content".into();
        reply(&mut app, &tx, first, stale);
        assert_eq!(app.chat_scroll_top, top);
        assert!(render(&mut app).contains("message-020"));
        assert_eq!(app.messages[20].content, "message-020\nextra body 20");
    }

    #[test]
    fn every_manual_scroll_key_abandons_a_pending_search_jump() {
        for (code, modifiers) in [
            (KeyCode::Home, KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::CONTROL),
            (KeyCode::Down, KeyModifiers::CONTROL),
            (KeyCode::Char('{'), KeyModifiers::NONE),
            (KeyCode::Char('}'), KeyModifiers::NONE),
        ] {
            let (mut app, sent, tx) = app();
            let request = open(&mut app, &sent, hit("m10", 10));
            // Exercise navigation in a transcript while a replacement load is pending.
            app.messages = messages();
            render(&mut app);
            app.chat_scroll_top = Some(100);
            app.key(KeyEvent::new(code, modifiers));
            assert!(app.search_jump.is_none(), "{code:?} {modifiers:?}");
            reply(&mut app, &tx, request, messages());
            assert!(render(&mut app).contains("message-099"), "{code:?}");
        }
    }

    #[test]
    fn missing_identity_falls_back_and_sequence_fallback_is_checked_within_the_session() {
        let (mut app, sent, tx) = app();
        let request = open(&mut app, &sent, hit("deleted-id", 10));
        reply(&mut app, &tx, request, messages());
        assert!(app.search_jump.is_none());
        assert!(app.chat_scroll_top.is_none());
        assert!(app
            .notes
            .iter()
            .any(|note| note.contains("no longer in the loaded transcript")));
        assert!(render(&mut app).contains("message-099"));
        let request = open(&mut app, &sent, hit("", 10));
        reply(&mut app, &tx, request, messages());
        assert!(render(&mut app).contains("message-010"));
        let request = open(&mut app, &sent, hit("m10", 10));
        let mut wrong_session = messages();
        for message in &mut wrong_session {
            message.session_id = "other-session".into();
        }
        reply(&mut app, &tx, request, wrong_session);
        assert!(app.search_jump.is_none());
    }
    #[test]
    fn grouped_tool_results_show_their_call_context_and_failed_reads_drop_the_target() {
        let (mut app, sent, tx) = app();
        let mut transcript = messages();
        transcript[9].meta=Some(serde_json::json!({"tool_calls":[{"id":"search-call","type":"function","function":{"name":"search_fixture","arguments":"{}"}}]}).to_string());
        transcript[10].role = "tool".into();
        transcript[10].meta = Some(serde_json::json!({"tool_call_id":"search-call"}).to_string());
        let mut target = hit("m10", 10);
        target.role = "tool".into();
        let request = open(&mut app, &sent, target);
        reply(&mut app, &tx, request, transcript);
        let screen = render(&mut app);
        assert!(screen.contains("search_fixture"), "{screen}");
        assert!(app
            .notes
            .iter()
            .any(|note| note.contains("nearest visible context")));
        let request = open(&mut app, &sent, hit("m10", 10));
        tx.send(ServerMessage::Response {
            id: request,
            ok: false,
            result: None,
            error: Some("fixture load failure".into()),
        })
        .unwrap();
        app.drain_daemon();
        assert!(app.search_jump.is_none());
        assert!(app.chat_scroll_top.is_none());
        let timed_out = open(&mut app, &sent, hit("m10", 10));
        *app.pending_since.get_mut(&timed_out).unwrap() =
            Instant::now() - REQUEST_DEADLINE - Duration::from_secs(1);
        app.expire_pending();
        assert!(app.search_jump.is_none());
    }
}
