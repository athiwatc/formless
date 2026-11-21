//! Wires Topic Chats into the App: daemon requests, replies, keys and layout.

use super::*;
use crate::topic::{self, Action, ConfirmedAction, Node};

/// Terminals at least this wide show the conversation rail beside the chat.
pub(crate) const WIDE_RAIL: u16 = 110;
const RAIL_WIDTH: u16 = 34;

macro_rules! parse {
    ($ty:ty, $ok:expr, $result:expr) => {
        $result
            .filter(|_| $ok)
            .and_then(|value| serde_json::from_value::<$ty>(value).ok())
    };
}

impl App {
    pub(crate) fn topic_connected(&mut self) {
        self.topic.disconnected();
        self.topic_epoch = self.topic_epoch.wrapping_add(1);
        self.pending_topic_actions.clear();
        self.send_request(
            Pending::TopicCapability {
                epoch: self.topic_epoch,
            },
            Request::Ping,
        );
    }

    pub(crate) fn topic_tick(&mut self) {
        if self.client.is_some() && self.offline.is_none() && self.topic.needs_refresh() {
            self.refresh_topics();
        }
    }

    pub(crate) fn refresh_topics(&mut self) {
        if !self.topic.enabled() {
            return;
        }
        self.topic.mark_refreshed();
        self.topic_list_generation = self.topic_list_generation.wrapping_add(1);
        self.request(
            Pending::TopicList {
                generation: self.topic_list_generation,
            },
            Request::ListTopics,
        );
        if !self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::TopicMain { .. }))
        {
            self.request_main_chat(false);
        }
    }

    fn request_main_chat(&mut self, open: bool) {
        self.topic_main_generation = self.topic_main_generation.wrapping_add(1);
        self.request(
            Pending::TopicMain {
                generation: self.topic_main_generation,
                open,
            },
            Request::GetMainChat,
        );
    }

    fn topic_unavailable(&mut self) -> bool {
        if self.topic.enabled() {
            return false;
        }
        self.notes.push(
            if self.topic.capability_known() {
                "Topic Chats need a daemon with topic_orchestration_v1; /sessions still lists every session."
            } else {
                "Checking Topic Chat support with the daemon; try again in a moment."
            }
            .into(),
        );
        true
    }

    /// `/main`, `/topics` and `/topic …`.
    pub(crate) fn topic_slash(&mut self, name: &str, args: &str) {
        if self.topic_unavailable() {
            return;
        }
        match name {
            "main" => self.open_main_chat(),
            "topics" => {
                self.refresh_topics();
                self.topic.open_picker();
            }
            _ => {
                let args = args.trim();
                if args.is_empty() || args == "new" {
                    self.open_topic_proposal();
                } else if let Err(error) = self.topic.open_topic_command(args) {
                    self.notes.push(error);
                }
            }
        }
    }

    fn open_topic_proposal(&mut self) {
        let Some(current) = self.current.clone() else {
            self.notes
                .push("Open Main Chat (/main) before proposing a topic.".into());
            return;
        };
        if self.topic.is_harness_session(&current) {
            self.notes
                .push("Harness conversations are read-only; propose topics from Main Chat.".into());
            return;
        }
        let source = self
            .sessions
            .iter()
            .find(|session| session.id == current)
            .or_else(|| self.topic.main().filter(|main| main.id == current))
            .cloned();
        let Some(source) = source else {
            self.notes
                .push("This session is still loading; try /topic again.".into());
            return;
        };
        let repo = source.cwd.clone().unwrap_or_default();
        let name = std::path::Path::new(&repo)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let messages = self.messages.clone();
        self.topic.open_proposal(&source, &messages, &repo, &name);
    }

    /// Always asks the daemon: a purged Main Chat is replaced there, so a
    /// cached id may no longer exist.
    fn open_main_chat(&mut self) {
        self.request_main_chat(true);
    }

    fn open_topic_session(&mut self, id: String) {
        self.topic.close_overlay();
        if let Some(node) = self.topic.topic_for_session(&id) {
            self.topic.select(node);
        }
        self.view = View::Chat;
        if self.current.as_deref() != Some(id.as_str()) {
            self.switch_session(id.clone());
            self.request(Pending::GetSession, Request::GetSession { id });
        }
    }

    /// Topic overlays own the keyboard while open, below approvals.
    pub(crate) fn topic_key(&mut self, key: KeyEvent) -> bool {
        if !self.topic.overlay_open() || !self.approvals.is_empty() || !self.promotions.is_empty() {
            return false;
        }
        let action = self.topic.key(key);
        self.topic_action(action);
        true
    }

    fn topic_action(&mut self, action: Action) {
        match action {
            Action::None | Action::Close => {}
            Action::Refresh => self.refresh_topics(),
            Action::OpenMain => self.open_main_chat(),
            Action::OpenTopic(topic_id) => self.open_node(Node::Topic(topic_id)),
            Action::OpenAttempt {
                topic_id,
                attempt_id,
            } => self.open_node(Node::Attempt {
                topic_id,
                attempt_id,
            }),
            Action::Propose(proposal) => {
                self.topic.proposal_pending();
                self.request(Pending::TopicPropose, Request::ProposeTopic { proposal });
            }
            Action::Confirm { topic_id, action } => self.confirm_topic_action(topic_id, action),
            Action::Reject { topic_id, reason } => {
                if self.claim_topic_action(format!("reject:{topic_id}")) {
                    self.request(
                        Pending::TopicReject {
                            topic_id: topic_id.clone(),
                        },
                        Request::RejectTopic {
                            rejection: TopicRejectionRequest { topic_id, reason },
                        },
                    );
                }
            }
        }
    }

    fn open_node(&mut self, node: Node) {
        match self.topic.session_id(&node).map(str::to_string) {
            Some(id) => self.open_topic_session(id),
            None => self
                .notes
                .push("This conversation has not started yet.".into()),
        }
    }

    fn claim_topic_action(&mut self, key: String) -> bool {
        if self.pending_topic_actions.insert(key) {
            return true;
        }
        self.notes
            .push("That topic action is already in progress.".into());
        false
    }

    fn confirm_topic_action(&mut self, topic_id: String, action: ConfirmedAction) {
        match action {
            ConfirmedAction::Approve | ConfirmedAction::ApproveAndStart => {
                let Some(source_session_id) = self
                    .topic
                    .topic(&topic_id)
                    .map(|topic| topic.source_session_id.clone())
                else {
                    return;
                };
                if !self.claim_topic_action(format!("approve:{topic_id}")) {
                    return;
                }
                self.request(
                    Pending::TopicApprove {
                        topic_id: topic_id.clone(),
                        source_session_id,
                        start_after_approval: action == ConfirmedAction::ApproveAndStart,
                    },
                    Request::ApproveTopic {
                        approval: TopicApprovalRequest { topic_id },
                    },
                );
            }
            ConfirmedAction::Start => self.start_topic(topic_id),
            ConfirmedAction::Recover => {
                if self.claim_topic_action(format!("recover:{topic_id}")) {
                    self.request(
                        Pending::TopicRecover {
                            topic_id: topic_id.clone(),
                        },
                        Request::RecoverTopic {
                            recovery: TopicRecoveryRequest { topic_id },
                        },
                    );
                }
            }
            ConfirmedAction::Retry { feature_id } => {
                if self.claim_topic_action(format!("retry:{topic_id}:{feature_id}")) {
                    self.request(
                        Pending::TopicRetry {
                            topic_id: topic_id.clone(),
                            feature_id: feature_id.clone(),
                        },
                        Request::RetryTopicFeature {
                            retry: TopicFeatureRetryRequest {
                                topic_id,
                                feature_id,
                            },
                        },
                    );
                }
            }
        }
    }

    fn start_topic(&mut self, topic_id: String) {
        if !self.topic.claim_start(&topic_id) {
            self.notes
                .push("The workflow start was already requested for this topic.".into());
            return;
        }
        self.request(
            Pending::TopicStart {
                topic_id: topic_id.clone(),
            },
            Request::StartTopicWorkflow { topic_id },
        );
    }

    pub(crate) fn topic_changed(&mut self, topic_id: String) {
        if self.topic.enabled() && self.offline.is_none() {
            self.request(
                Pending::TopicGet {
                    topic_id: topic_id.clone(),
                    epoch: self.topic_epoch,
                },
                Request::GetTopic { topic_id },
            );
        }
    }

    fn topic_name(&self, topic_id: &str) -> String {
        self.topic
            .topic(topic_id)
            .map(|topic| topic.name.clone())
            .unwrap_or_else(|| topic_id.to_string())
    }

    pub(crate) fn topic_reply(
        &mut self,
        kind: Pending,
        ok: bool,
        result: Option<serde_json::Value>,
        error: Option<String>,
    ) {
        let reason = error.unwrap_or_else(|| "unexpected daemon reply".into());
        match kind {
            Pending::TopicCapability { epoch } => {
                if epoch != self.topic_epoch {
                    return;
                }
                let enabled = ok
                    && result
                        .as_ref()
                        .and_then(|result| result["features"].as_array())
                        .is_some_and(|features| {
                            features
                                .iter()
                                .any(|feature| feature.as_str() == Some(topic::CAPABILITY))
                        });
                self.topic.set_capability(enabled);
                if enabled {
                    self.refresh_topics();
                }
            }
            Pending::TopicMain { generation, open } => {
                if generation != self.topic_main_generation {
                    return;
                }
                match parse!(SessionInfo, ok, result) {
                    Some(main) => {
                        let id = main.id.clone();
                        self.topic.set_main(main);
                        if open {
                            self.open_topic_session(id);
                        }
                    }
                    None if open => self
                        .notes
                        .push(format!("Main Chat is unavailable: {reason}")),
                    None => {}
                }
            }
            Pending::TopicList { generation } => {
                if generation != self.topic_list_generation {
                    return;
                }
                // Periodic refreshes stay quiet on failure.
                let Some(topics) = parse!(Vec<TopicInfo>, ok, result) else {
                    return;
                };
                if let Some(topic_id) = self.topic.update_topics(topics) {
                    self.topic.mark_pending_reviewed(&topic_id);
                    self.notes.push(format!(
                        "Topic proposal “{}” needs approval: /topic {topic_id} to review. Nothing moves until you approve.",
                        self.topic_name(&topic_id)
                    ));
                }
            }
            Pending::TopicGet { topic_id, epoch } => {
                if epoch != self.topic_epoch {
                    return;
                }
                if let Some(topic) =
                    parse!(TopicInfo, ok, result).filter(|topic| topic.id == topic_id)
                {
                    self.topic.upsert_topic(topic, false);
                }
            }
            Pending::TopicPropose => match parse!(TopicInfo, ok, result) {
                Some(topic) => {
                    let topic_id = topic.id.clone();
                    let review = self.topic.proposal_is_sending();
                    self.topic.upsert_topic(topic, review);
                    if review {
                        self.topic.close_overlay();
                        self.topic.open_review(&topic_id);
                    }
                    self.notes.push(
                        "Topic proposed. Review it to approve or reject; nothing has moved yet."
                            .into(),
                    );
                }
                None => {
                    self.topic.proposal_failed();
                    self.notes.push(format!("Topic proposal failed: {reason}"));
                }
            },
            Pending::TopicApprove {
                topic_id,
                source_session_id,
                start_after_approval,
            } => {
                self.pending_topic_actions
                    .remove(&format!("approve:{topic_id}"));
                match parse!(TopicInfo, ok, result) {
                    Some(topic) => {
                        let name = topic.name.clone();
                        self.topic.upsert_topic(topic, false);
                        if self.current.as_deref() == Some(source_session_id.as_str()) {
                            self.request(
                                Pending::GetSession,
                                Request::GetSession {
                                    id: source_session_id,
                                },
                            );
                        }
                        self.notes.push(format!(
                            "Approved “{name}”: the selected requests moved into the topic."
                        ));
                        if start_after_approval {
                            self.start_topic(topic_id);
                        }
                    }
                    None => self.notes.push(format!(
                        "Topic approval failed: {reason}. Nothing moved or started."
                    )),
                }
            }
            Pending::TopicStart { topic_id } => match parse!(TopicInfo, ok, result) {
                Some(topic) => {
                    let name = topic.name.clone();
                    self.topic.upsert_topic(topic, false);
                    self.notes.push(format!(
                        "Workflow started for “{name}”. Ready features wait for you; nothing merges automatically."
                    ));
                }
                None => {
                    self.topic.release_start(&topic_id);
                    self.notes.push(format!("Workflow start failed: {reason}"));
                }
            },
            Pending::TopicReject { topic_id } => {
                self.pending_topic_actions
                    .remove(&format!("reject:{topic_id}"));
                self.topic_updated(ok, result, &reason, "Proposal rejected; no requests moved.");
            }
            Pending::TopicRecover { topic_id } => {
                self.pending_topic_actions
                    .remove(&format!("recover:{topic_id}"));
                self.topic_updated(ok, result, &reason, "Topic recovery requested.");
            }
            Pending::TopicRetry {
                topic_id,
                feature_id,
            } => {
                self.pending_topic_actions
                    .remove(&format!("retry:{topic_id}:{feature_id}"));
                self.topic_updated(ok, result, &reason, "Feature retry requested.");
            }
            _ => {}
        }
    }

    fn topic_updated(
        &mut self,
        ok: bool,
        result: Option<serde_json::Value>,
        reason: &str,
        done: &str,
    ) {
        match parse!(TopicInfo, ok, result) {
            Some(topic) => {
                self.topic.upsert_topic(topic, false);
                self.notes.push(done.into());
            }
            None => self.notes.push(format!("Topic action failed: {reason}")),
        }
    }

    /// Nested harness transcripts are read-only; slash commands still work.
    pub(crate) fn topic_blocks_submit(&mut self) -> bool {
        if self.search_mode || !self.topic.current_is_harness(self.current.as_deref()) {
            return false;
        }
        let text = self.input.expanded();
        if text.starts_with('/')
            && !self.input.is_literal()
            && self.editing_id.is_none()
            && self.editing_queued.is_none()
        {
            return false;
        }
        self.notes.push(
            "Harness conversations are read-only. Use /topics or /main to return to a chat you can send to."
                .into(),
        );
        true
    }
}

impl Pending {
    pub(crate) fn is_topic(&self) -> bool {
        matches!(
            self,
            Pending::TopicCapability { .. }
                | Pending::TopicMain { .. }
                | Pending::TopicList { .. }
                | Pending::TopicGet { .. }
                | Pending::TopicPropose
                | Pending::TopicApprove { .. }
                | Pending::TopicStart { .. }
                | Pending::TopicReject { .. }
                | Pending::TopicRecover { .. }
                | Pending::TopicRetry { .. }
        )
    }
}

/// Chat with the conversation rail (wide) or a breadcrumb (narrow).
pub(crate) fn draw_chat_with_topics(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
) -> Option<keys::ChatFocus> {
    if !app.topic.enabled() {
        return draw_chat(f, area, app);
    }
    let mut chat = area;
    if area.width >= WIDE_RAIL {
        let parts =
            Layout::horizontal([Constraint::Length(RAIL_WIDTH), Constraint::Min(1)]).split(area);
        app.topic
            .draw_rail(f, parts[0], app.current.as_deref(), app.color);
        chat = parts[1];
    }
    if chat.height > 4 {
        let parts = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(chat);
        let crumb = app.topic.breadcrumb(app.current.as_deref());
        let hint = if area.width >= WIDE_RAIL {
            ""
        } else {
            "  ·  /topics"
        };
        f.render_widget(
            Paragraph::new(Line::styled(
                format!(" {crumb}{hint}"),
                crate::ui::dim(app.color),
            )),
            parts[0],
        );
        chat = parts[1];
    }
    draw_chat(f, chat, app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::test_app;

    fn feature(id: &str, request: &str, status: TopicFeatureStatus) -> TopicFeatureInfo {
        TopicFeatureInfo {
            id: id.into(),
            source_message_id: "m1".into(),
            source_seq: 1,
            request: request.into(),
            status,
            active_attempt_id: None,
            ready_revision: None,
            review_round: 0,
            detail: None,
        }
    }

    fn topic(id: &str, status: TopicStatus) -> TopicInfo {
        TopicInfo {
            id: id.into(),
            name: format!("Topic {id}"),
            repo: "/repo".into(),
            base_revision: "base".into(),
            source_session_id: "main".into(),
            topic_session_id: Some(format!("session-{id}")),
            status,
            builder: HarnessKind::Claude,
            builder_model: Some("opus".into()),
            reviewer: HarnessKind::Codex,
            reviewer_model: Some("sol".into()),
            features: vec![
                feature("x", "Add X", TopicFeatureStatus::Pending),
                feature("y", "Add Y", TopicFeatureStatus::Pending),
            ],
            attempts: vec![],
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn attempt(topic_id: &str, session: &str, role: HarnessRole) -> TopicAttemptInfo {
        TopicAttemptInfo {
            id: format!("attempt-{session}"),
            topic_id: topic_id.into(),
            feature_id: "x".into(),
            round: 1,
            role,
            harness: if role == HarnessRole::Build {
                HarnessKind::Claude
            } else {
                HarnessKind::Codex
            },
            status: TopicAttemptStatus::Building,
            conversation_session_id: session.into(),
            revision: None,
            harness_session_id: None,
            detail: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    struct Wire {
        sent: mpsc::Receiver<String>,
        reply: mpsc::Sender<ServerMessage>,
    }

    impl Wire {
        fn requests(&self) -> Vec<RequestEnvelope> {
            self.sent
                .try_iter()
                .map(|line| serde_json::from_str(&line).unwrap())
                .collect()
        }

        fn topic_requests(&self) -> Vec<RequestEnvelope> {
            self.requests()
                .into_iter()
                .filter(|envelope| {
                    matches!(
                        envelope.request,
                        Request::ProposeTopic { .. }
                            | Request::ApproveTopic { .. }
                            | Request::RejectTopic { .. }
                            | Request::StartTopicWorkflow { .. }
                            | Request::RecoverTopic { .. }
                            | Request::RetryTopicFeature { .. }
                    )
                })
                .collect()
        }

        fn respond(&self, app: &mut App, id: u64, value: serde_json::Value) {
            self.reply
                .send(ServerMessage::Response {
                    id,
                    ok: true,
                    result: Some(value),
                    error: None,
                })
                .unwrap();
            app.drain_daemon();
        }
    }

    fn connected(topics: Vec<TopicInfo>) -> (App, Wire) {
        let mut app = test_app();
        let (client, sent) = DaemonClient::recording();
        let (reply, inbox) = mpsc::channel();
        app.client = Some(client);
        app.inbox = Some(inbox);
        app.topic.set_capability(true);
        app.topic.mark_refreshed();
        app.topic.update_topics(topics);
        (app, Wire { sent, reply })
    }

    fn press(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn approve_and_start_needs_consent_then_moves_refreshes_and_starts_once() {
        let (mut app, wire) = connected(vec![topic("t1", TopicStatus::PendingApproval)]);
        app.current = Some("main".into());
        app.topic_slash("topic", "t1");
        press(&mut app, KeyCode::Char('s'));
        assert!(
            wire.topic_requests().is_empty(),
            "review alone sends nothing"
        );
        press(&mut app, KeyCode::Enter);
        let approve = wire.topic_requests();
        assert_eq!(approve.len(), 1);
        assert!(matches!(
            &approve[0].request,
            Request::ApproveTopic { approval } if approval.topic_id == "t1"
        ));
        wire.respond(
            &mut app,
            approve[0].id,
            serde_json::to_value(topic("t1", TopicStatus::Approved)).unwrap(),
        );
        let after = wire.requests();
        assert!(after
            .iter()
            .any(|e| matches!(&e.request, Request::GetSession { id } if id == "main")));
        let starts: Vec<_> = after
            .iter()
            .filter(|e| matches!(&e.request, Request::StartTopicWorkflow { topic_id } if topic_id == "t1"))
            .collect();
        assert_eq!(starts.len(), 1);
        wire.respond(
            &mut app,
            starts[0].id,
            serde_json::to_value(topic("t1", TopicStatus::Running)).unwrap(),
        );
        app.confirm_topic_action("t1".into(), ConfirmedAction::Start);
        assert!(wire.topic_requests().is_empty(), "start is requested once");
    }

    #[test]
    fn approve_only_never_starts_and_cancel_sends_nothing() {
        let (mut app, wire) = connected(vec![topic("t1", TopicStatus::PendingApproval)]);
        app.topic_slash("topic", "t1");
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(wire.topic_requests().is_empty());
        app.topic_slash("topic", "t1");
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Char('y'));
        let approve = wire.topic_requests();
        assert_eq!(approve.len(), 1);
        wire.respond(
            &mut app,
            approve[0].id,
            serde_json::to_value(topic("t1", TopicStatus::Approved)).unwrap(),
        );
        assert!(
            wire.topic_requests().is_empty(),
            "approval alone does not start"
        );
    }

    #[test]
    fn reject_sends_reason_without_approving() {
        let (mut app, wire) = connected(vec![topic("t1", TopicStatus::PendingApproval)]);
        app.topic_slash("topic", "t1");
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Enter);
        let requests = wire.topic_requests();
        assert_eq!(requests.len(), 1);
        assert!(matches!(
            &requests[0].request,
            Request::RejectTopic { rejection } if rejection.topic_id == "t1" && !rejection.reason.is_empty()
        ));
    }

    fn session(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            forked_from: None,
            fork_point_seq: None,
            name: "Main Chat".into(),
            model: None,
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: Some("/work/formless".into()),
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: Vec::new(),
            instructions: None,
        }
    }

    #[test]
    fn manual_proposal_is_reviewed_before_it_is_sent_and_cancel_sends_nothing() {
        let (mut app, wire) = connected(vec![]);
        app.current = Some("main".into());
        app.topic.set_main(session("main"));
        app.messages = vec![MessageInfo {
            id: "m1".into(),
            session_id: "main".into(),
            seq: 1,
            role: "user".into(),
            content: "Let's add X and Y to Formless".into(),
            created_at: String::new(),
            meta: None,
        }];
        app.topic_slash("topic", "");
        assert!(app.topic.overlay_open());
        press(&mut app, KeyCode::Esc);
        assert!(!app.topic.overlay_open());
        assert!(wire.topic_requests().is_empty());

        app.topic_slash("topic", "");
        for _ in 0..7 {
            press(&mut app, KeyCode::Enter);
        }
        assert!(
            wire.topic_requests().is_empty(),
            "the review step sends nothing"
        );
        press(&mut app, KeyCode::Enter);
        let requests = wire.topic_requests();
        assert_eq!(requests.len(), 1);
        let Request::ProposeTopic { proposal } = &requests[0].request else {
            panic!("expected a proposal");
        };
        assert_eq!(proposal.message_ids, ["m1"]);
        assert_eq!(proposal.name, "formless");
        assert_eq!(proposal.repo, std::path::PathBuf::from("/work/formless"));
        assert_eq!(proposal.builder, HarnessKind::Claude);
        assert_eq!(proposal.reviewer, HarnessKind::Codex);
        wire.respond(
            &mut app,
            requests[0].id,
            serde_json::to_value(topic("t1", TopicStatus::PendingApproval)).unwrap(),
        );
        assert!(app.topic.overlay_open());
        assert!(wire.topic_requests().is_empty(), "proposing never approves");
    }

    #[test]
    fn stale_topic_lists_are_ignored() {
        let (mut app, wire) = connected(vec![]);
        app.refresh_topics();
        app.refresh_topics();
        let lists: Vec<_> = wire
            .requests()
            .into_iter()
            .filter(|e| matches!(e.request, Request::ListTopics))
            .collect();
        assert_eq!(lists.len(), 2);
        wire.respond(
            &mut app,
            lists[0].id,
            serde_json::to_value(vec![topic("old", TopicStatus::Running)]).unwrap(),
        );
        assert!(app.topic.topic("old").is_none());
        wire.respond(
            &mut app,
            lists[1].id,
            serde_json::to_value(vec![topic("new", TopicStatus::Running)]).unwrap(),
        );
        assert!(app.topic.topic("new").is_some());
    }

    #[test]
    fn pending_suggestion_is_announced_once_without_approving() {
        let (mut app, wire) = connected(vec![]);
        for _ in 0..2 {
            app.refresh_topics();
            let list = wire
                .requests()
                .into_iter()
                .find(|e| matches!(e.request, Request::ListTopics))
                .unwrap();
            wire.respond(
                &mut app,
                list.id,
                serde_json::to_value(vec![topic("s1", TopicStatus::PendingApproval)]).unwrap(),
            );
        }
        let announced = app
            .notes
            .iter()
            .filter(|note| note.contains("needs approval"))
            .count();
        assert_eq!(announced, 1);
        assert!(wire.topic_requests().is_empty());
    }

    #[test]
    fn harness_conversations_are_read_only_but_accept_commands() {
        let mut running = topic("t1", TopicStatus::Running);
        running
            .attempts
            .push(attempt("t1", "claude-1", HarnessRole::Build));
        let (mut app, wire) = connected(vec![running]);
        app.current = Some("claude-1".into());
        app.input.set("please change this");
        app.submit_chat(false);
        assert!(wire.requests().is_empty());
        assert!(app.notes.last().unwrap().contains("read-only"));
        assert_eq!(app.input.expanded(), "please change this");
        app.input.set("/topics");
        app.submit_chat(false);
        assert!(app.topic.overlay_open());
    }

    #[test]
    fn drafts_and_ids_survive_switching_between_conversations() {
        let mut running = topic("t1", TopicStatus::Running);
        running
            .attempts
            .push(attempt("t1", "codex-1", HarnessRole::Review));
        let (mut app, _wire) = connected(vec![running.clone()]);
        app.topic.set_main(session("main"));
        app.current = Some("main".into());
        app.input.set("half-written");
        app.open_node(Node::Topic("t1".into()));
        assert_eq!(app.current.as_deref(), Some("session-t1"));
        assert!(app.input.is_empty());
        app.topic.update_topics(vec![running]);
        assert_eq!(app.topic.selected(), &Node::Topic("t1".into()));
        app.open_main_chat();
        assert!(app
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::TopicMain { open: true, .. })));
        app.open_topic_session("main".into());
        assert_eq!(app.current.as_deref(), Some("main"));
        assert_eq!(app.input.expanded(), "half-written");
    }

    #[test]
    fn older_daemons_hide_topic_chats_quietly() {
        let mut app = test_app();
        let (client, sent) = DaemonClient::recording();
        let (reply, inbox) = mpsc::channel();
        app.client = Some(client);
        app.inbox = Some(inbox);
        let wire = Wire { sent, reply };
        app.topic_connected();
        let ping = wire.requests().pop().unwrap();
        assert!(matches!(ping.request, Request::Ping));
        wire.respond(&mut app, ping.id, serde_json::json!({ "features": [] }));
        assert!(!app.topic.enabled());
        app.topic_tick();
        assert!(wire.requests().is_empty());
        app.topic_slash("topics", "");
        assert!(!app.topic.overlay_open());
        assert!(app.notes.last().unwrap().contains("topic_orchestration_v1"));
    }

    fn render(app: &mut App, width: u16) -> String {
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(width, 30)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn wide_terminals_show_the_hierarchy_and_narrow_ones_a_breadcrumb() {
        let _env = credential_env::EnvGuard::isolated();
        let mut running = topic("t1", TopicStatus::Running);
        running.features[0].status = TopicFeatureStatus::Building;
        running
            .attempts
            .push(attempt("t1", "claude-1", HarnessRole::Build));
        running
            .attempts
            .push(attempt("t1", "codex-1", HarnessRole::Review));
        let (mut app, _wire) = connected(vec![topic("t0", TopicStatus::PendingApproval), running]);
        app.current = Some("claude-1".into());
        let wide = render(&mut app, 140);
        for expected in [
            "Main Chat",
            "Topic Chats",
            "Topic t0",
            "Needs approval",
            "Topic t1",
            "Building",
            "Claude Build",
            "Codex Review",
            "read-only",
        ] {
            assert!(wide.contains(expected), "missing {expected}:\n{wide}");
        }
        let narrow = render(&mut app, 80);
        assert!(!narrow.contains("Topic Chats\n"));
        assert!(narrow.contains("Topic t1 / Claude Build"), "{narrow}");
        assert!(narrow.contains("/topics"));
        app.topic.open_picker();
        let picker = render(&mut app, 80);
        assert!(picker.contains("Codex Review"), "{picker}");
    }
}
