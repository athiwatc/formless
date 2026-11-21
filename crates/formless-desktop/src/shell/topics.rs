use super::*;
use crate::topics::{self, Action, Reply};
use formless_protocol::{HarnessKind, TopicFeatureStatus, TopicStatus};

pub(super) struct Ui {
    pub state: topics::State,
    proposing: bool,
    source: Option<String>,
    selected: HashSet<String>,
    name: Entity<InputState>,
    repo: Entity<InputState>,
    builder_model: Entity<InputState>,
    reviewer_model: Entity<InputState>,
    builder: HarnessKind,
    reviewer: HarnessKind,
    scroll: ScrollHandle,
    tree_scroll: ScrollHandle,
    pending_open: Option<String>,
}

impl Ui {
    pub fn reset_connection(&mut self) {
        self.state = Default::default();
        self.proposing = false;
        self.source = None;
        self.selected.clear();
        self.pending_open = None;
    }
    pub fn retain_candidates(&mut self, available: &HashSet<&str>) {
        self.selected.retain(|id| available.contains(id.as_str()));
    }
    pub fn session_changed(&mut self, session: Option<&str>) {
        self.proposing = false;
        self.selected.clear();
        self.state.candidates.clear();
        self.state.candidate_session = None;
        self.state.selected = self
            .state
            .topics
            .iter()
            .find(|t| {
                t.topic_session_id.as_deref() == session && session.is_some()
                    || t.attempts
                        .iter()
                        .any(|a| Some(a.conversation_session_id.as_str()) == session)
            })
            .map(|t| t.id.clone());
    }
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        Self {
            state: Default::default(),
            proposing: false,
            source: None,
            selected: HashSet::new(),
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Topic name, e.g. Formless")),
            repo: cx.new(|cx| InputState::new(window, cx).placeholder("Absolute repository path")),
            builder_model: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Build model (optional, e.g. opus)")
            }),
            reviewer_model: cx
                .new(|cx| InputState::new(window, cx).placeholder("Review model (optional)")),
            builder: HarnessKind::Claude,
            reviewer: HarnessKind::Codex,
            scroll: ScrollHandle::new(),
            tree_scroll: ScrollHandle::new(),
            pending_open: None,
        }
    }
}

impl Shell {
    #[cfg(feature = "test-support")]
    pub fn topic_inputs(&self) -> [Entity<InputState>; 4] {
        [
            self.topic_ui.name.clone(),
            self.topic_ui.repo.clone(),
            self.topic_ui.builder_model.clone(),
            self.topic_ui.reviewer_model.clone(),
        ]
    }
    fn topic_action(&mut self, action: Action, cx: &mut Context<Self>) {
        if !self.client_connected || self.topic_ui.state.pending.is_some() {
            return;
        }
        let Some(client) = &self.client else {
            return;
        };
        let operation = (!matches!(action, Action::Main | Action::List)).then(uuid::Uuid::new_v4);
        let command = ClientCommand::Topic {
            connection: client.connection(),
            operation,
            action,
        };
        match client.send(command) {
            Ok(()) => {
                if let Some(operation) = operation {
                    self.topic_ui.state.pending = Some(operation);
                    self.topic_ui.state.notice = Some("Waiting for Formless…".into());
                }
            }
            Err(error) => self.topic_ui.state.notice = Some(error.to_string()),
        }
        cx.notify();
    }

    pub(super) fn topic_result(
        &mut self,
        connection: u64,
        operation: Option<uuid::Uuid>,
        result: Result<Reply, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .client
            .as_ref()
            .is_none_or(|c| c.connection() != connection)
        {
            return;
        }
        if operation.is_some() && operation != self.topic_ui.state.pending {
            return;
        }
        if operation.is_some() {
            self.topic_ui.state.pending = None;
        }
        match result {
            Ok(reply) => {
                let main = match &reply {
                    Reply::Main(s) => Some(s.clone()),
                    _ => None,
                };
                let approved = match &reply {
                    Reply::Topic(t) if t.status != TopicStatus::PendingApproval => {
                        t.topic_session_id.clone()
                    }
                    _ => None,
                };
                if operation.is_some() {
                    self.topic_ui.proposing = false;
                    self.topic_ui.state.notice =
                        Some("Updated from Formless. Nothing is merged automatically.".into());
                }
                self.topic_ui.state.apply(reply);
                if let Some(main) = main {
                    if !self.client_sessions.iter().any(|s| s.id == main.id) {
                        self.client_sessions.push(main.clone());
                        self.sidebar
                            .replace(&self.client_sessions, &HashSet::new(), false);
                        self.session_sizes =
                            Self::row_sizes(self.sidebar.rows().len(), self.text_scale);
                    }
                    if self.sidebar.active_id().is_none() {
                        self.open_topic_session(&main.id, window, cx);
                    }
                }
                if let Some(id) = approved {
                    // ListSessions may arrive after the approval receipt. Never
                    // invent session metadata or show the old transcript as it.
                    if self.sidebar.rows().iter().any(|s| s.id == id) {
                        self.open_topic_session(&id, window, cx);
                    } else {
                        self.topic_ui.pending_open = Some(id);
                    }
                }
            }
            Err(error) => self.topic_ui.state.notice = Some(error),
        }
        cx.notify();
    }

    pub(super) fn topic_sessions_loaded(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.topic_ui.pending_open.take() {
            if self.sidebar.rows().iter().any(|s| s.id == id) {
                self.open_topic_session(&id, window, cx);
            } else {
                self.topic_ui.pending_open = Some(id);
            }
        }
    }

    fn open_topic_session(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.sidebar.rows().iter().any(|session| session.id == id) {
            self.topic_ui.state.notice = Some("This conversation is unavailable or archived. Restore archived conversations from Sessions first.".into());
            cx.notify();
            return false;
        }
        self.sidebar.select_id(id);
        self.activate_selected(window, cx);
        true
    }

    fn open_topic(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let session = self
            .topic_ui
            .state
            .topics
            .iter()
            .find(|t| t.id == id)
            // Pending proposals have no topic transcript yet. Review them in
            // their source chat, including when opened from another page.
            .map(|t| {
                t.topic_session_id
                    .clone()
                    .unwrap_or_else(|| t.source_session_id.clone())
            });
        if let Some(session) = session {
            if !self.open_topic_session(&session, window, cx) {
                return;
            }
        }
        self.topic_ui.state.selected = Some(id.to_string());
        self.topic_ui.proposing = false;
        // A collapsed sidebar is an overlay: return to the conversation.
        self.prefs.close_overlays();
        self.settle(window, cx);
        cx.notify();
    }

    fn begin_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.topic_ui.proposing = !self.topic_ui.proposing;
        self.topic_ui.source = self.sidebar.active_id().map(str::to_string);
        self.topic_ui.selected.clear();
        self.topic_ui.state.notice = None;
        let cwd = self
            .client_sessions
            .iter()
            .find(|s| Some(s.id.as_str()) == self.sidebar.active_id())
            .and_then(|s| s.cwd.clone())
            .unwrap_or_default();
        self.topic_ui
            .repo
            .update(cx, |input, cx| input.set_value(cwd, window, cx));
        cx.notify();
    }

    fn propose_topic(&mut self, cx: &mut Context<Self>) {
        let ui = &self.topic_ui;
        if !ui.proposing
            || ui.source.as_deref() != self.sidebar.active_id()
            || ui.state.candidate_session != ui.source
        {
            return;
        }
        let name = ui.name.read(cx).value().to_string().trim().to_string();
        let repo = ui.repo.read(cx).value().to_string().trim().to_string();
        let message_ids = ui
            .state
            .candidates
            .iter()
            .filter(|m| ui.selected.contains(&m.id))
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        if name.is_empty() || repo.is_empty() || message_ids.is_empty() {
            self.topic_ui.state.notice =
                Some("Choose at least one request, a topic name and a repository path.".into());
            cx.notify();
            return;
        }
        let optional = |input: &Entity<InputState>| {
            let value = input.read(cx).value().to_string().trim().to_string();
            (!value.is_empty()).then_some(value)
        };
        let proposal = formless_protocol::TopicProposalRequest {
            source_session_id: ui.source.clone().unwrap(),
            message_ids,
            features: vec![],
            name,
            repo: repo.into(),
            builder: ui.builder,
            reviewer: ui.reviewer,
            builder_model: optional(&ui.builder_model),
            reviewer_model: optional(&ui.reviewer_model),
        };
        self.topic_action(Action::Propose(proposal), cx);
    }

    pub(super) fn harness_read_only(&self) -> bool {
        self.sidebar
            .active_id()
            .is_some_and(|id| self.topic_ui.state.attempt_for(id).is_some())
    }

    pub(super) fn topic_write_notice(&self) -> Option<&'static str> {
        if self.topic_ui.state.supported && !self.topic_ui.state.inventory_loaded {
            Some("Waiting for topic conversations to load")
        } else if self.harness_read_only() {
            Some("Harness conversation is read only; Formless manages the handoff")
        } else {
            None
        }
    }

    pub(super) fn topic_session_edit_allowed(&self) -> bool {
        (!self.topic_ui.state.supported || self.topic_ui.state.inventory_loaded)
            && self
                .session_edit_target()
                .is_some_and(|id| self.topic_ui.state.attempt_for(id).is_none())
    }

    pub(super) fn render_topic_tree(&self, _window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.topic_ui.state;
        let mut tree = v_flex()
            .id("topic-navigation")
            .test_support()
            .gap_1()
            .p_2()
            .min_w_0()
            .min_h_0()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.topic_ui.tree_scroll);
        if let Some(main) = &state.main {
            let id = main.id.clone();
            tree = tree.child(
                Button::new("topic-main-chat")
                    .ghost()
                    .label("Main Chat")
                    .selected(self.sidebar.active_id() == Some(id.as_str()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.topic_ui.state.selected = None;
                        this.topic_ui.proposing = false;
                        this.open_topic_session(&id, window, cx);
                    })),
            );
        }
        tree = tree.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(
                    readable("topic-chat-heading", "Topic Chat")
                        .text_xs()
                        .font_semibold()
                        .flex_1(),
                )
                .child(
                    Button::new("topics-refresh")
                        .small()
                        .ghost()
                        .label("Refresh")
                        .disabled(!self.client_connected || state.pending.is_some())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.topic_action(Action::List, cx)),
                        ),
                ),
        );
        if state.topics.is_empty() {
            tree = tree.child(
                readable(
                    "topic-empty",
                    "Keep everyday chat in Main. Move work into a topic when you’re ready.",
                )
                .text_xs()
                .text_color(cx.theme().muted_foreground),
            );
        }
        for topic in &state.topics {
            if topic.status == TopicStatus::Rejected {
                continue;
            }
            let id = topic.id.clone();
            let ready = topic
                .features
                .iter()
                .filter(|f| f.status == TopicFeatureStatus::Ready)
                .count();
            tree = tree.child(
                v_flex()
                    .gap_1()
                    .min_w_0()
                    .child(
                        Button::new(SharedString::from(format!("topic-{}", topic.id)))
                            .ghost()
                            .label(topic.name.clone())
                            .accessibility_label(format!(
                                "{}, {}",
                                topic.name,
                                topics::status(topic)
                            ))
                            .selected(state.selected.as_deref() == Some(topic.id.as_str()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_topic(&id, window, cx)
                            })),
                    )
                    .child(
                        readable(
                            SharedString::from(format!("topic-state-{}", topic.id)),
                            format!(
                                "{} · {ready}/{} ready",
                                topics::status(topic),
                                topic.features.len()
                            ),
                        )
                        .text_xs()
                        .pl_3()
                        .text_color(cx.theme().muted_foreground),
                    ),
            );
            for attempt in &topic.attempts {
                let session = attempt.conversation_session_id.clone();
                let topic_id = topic.id.clone();
                let label = format!(
                    "{} · {} {} · {:?}",
                    topics::harness(attempt.harness),
                    topics::role(attempt.role),
                    attempt.round,
                    attempt.status
                );
                let available = self.sidebar.rows().iter().any(|s| s.id == session);
                tree = tree.child(
                    div().pl_4().min_w_0().child(
                        Button::new(SharedString::from(format!("topic-attempt-{}", attempt.id)))
                            .small()
                            .ghost()
                            .label(label)
                            .disabled(!available)
                            .selected(self.sidebar.active_id() == Some(session.as_str()))
                            .tooltip("Read the harness conversation. Formless owns the handoff.")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_topic_session(&session, window, cx);
                                this.topic_ui.state.selected = Some(topic_id.clone());
                                this.topic_ui.proposing = false;
                                this.prefs.close_overlays();
                                this.settle(window, cx);
                            })),
                    ),
                );
            }
        }
        tree.into_any_element()
    }

    pub(super) fn render_topic_workbench(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = &self.topic_ui;
        let busy = ui.state.pending.is_some() || !self.client_connected;
        let read_only = self.harness_read_only();
        let title = if ui
            .state
            .main
            .as_ref()
            .is_some_and(|s| Some(s.id.as_str()) == self.sidebar.active_id())
        {
            "Main Chat · everyday conversation"
        } else if read_only {
            "Harness conversation · read only"
        } else {
            "Topic workspace"
        };
        let mut panel = v_flex()
            .id("topic-workbench")
            .test_support()
            .flex_shrink_0()
            .min_w_0()
            .w_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted)
            .px_3()
            .py_2()
            .gap_2()
            .max_h(window.viewport_size().height * if ui.proposing { 0.65 } else { 0.38 })
            .overflow_y_scroll()
            .track_scroll(&ui.scroll)
            .child(
                h_flex()
                    .min_w_0()
                    .flex_wrap()
                    .gap_2()
                    .items_center()
                    .child(
                        readable("topic-workspace-label", title)
                            .text_sm()
                            .font_semibold()
                            .flex_1(),
                    )
                    .child(
                        Button::new("topic-move-open")
                            .small()
                            .outline()
                            .label(if ui.proposing {
                                "Cancel move"
                            } else {
                                "Move requests to topic"
                            })
                            .disabled(
                                busy || self.topic_write_notice().is_some()
                                    || !self.client_snapshot_ready
                                    || self.sidebar.active_id().is_none(),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.begin_topic(window, cx)),
                            ),
                    ),
            );
        if ui.proposing {
            panel = panel.child(readable("topic-proposal-help", "Choose the requests to move and your build/review workflow. Nothing moves or starts until you approve the review.").text_sm())
                .child(Input::new(&ui.name).small().aria_label("Topic name"))
                .child(Input::new(&ui.repo).small().aria_label("Repository path"));
            for build in [true, false] {
                let chosen = if build { ui.builder } else { ui.reviewer };
                let mut row = h_flex().min_w_0().flex_wrap().gap_2().items_center().child(
                    readable(
                        if build {
                            "topic-build-label"
                        } else {
                            "topic-review-label"
                        },
                        if build { "Build with" } else { "Review with" },
                    )
                    .text_xs(),
                );
                for kind in [HarnessKind::Claude, HarnessKind::Codex] {
                    row = row.child(
                        Button::new(SharedString::from(format!(
                            "topic-harness-{build}-{kind:?}"
                        )))
                        .small()
                        .outline()
                        .label(topics::harness(kind))
                        .selected(kind == chosen)
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if build {
                                this.topic_ui.builder = kind;
                            } else {
                                this.topic_ui.reviewer = kind;
                            }
                            cx.notify();
                        })),
                    );
                }
                panel = panel.child(row).child(
                    Input::new(if build {
                        &ui.builder_model
                    } else {
                        &ui.reviewer_model
                    })
                    .small()
                    .aria_label(if build {
                        "Build model"
                    } else {
                        "Review model"
                    }),
                );
            }
            if ui.state.candidates.is_empty() {
                panel = panel.child(
                    readable(
                        "topic-no-requests",
                        "Send a request first, then select it here once it is saved.",
                    )
                    .text_sm(),
                );
            }
            for message in &ui.state.candidates {
                let id = message.id.clone();
                panel = panel.child(
                    Button::new(SharedString::from(format!("topic-message-{}", message.id)))
                        .small()
                        .outline()
                        .label(format!(
                            "{} #{} · {}",
                            if ui.selected.contains(&message.id) {
                                "Selected"
                            } else {
                                "Select"
                            },
                            message.seq,
                            message.preview.replace('\n', " ")
                        ))
                        .selected(ui.selected.contains(&message.id))
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.topic_ui.selected.remove(&id) {
                                this.topic_ui.selected.insert(id.clone());
                            }
                            cx.notify();
                        })),
                );
            }
            panel = panel.child(
                Button::new("topic-propose")
                    .small()
                    .primary()
                    .label("Review topic move")
                    .disabled(busy || ui.selected.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.propose_topic(cx))),
            );
        } else if let Some(topic) = ui
            .state
            .selected()
            .or_else(|| ui.state.pending_for(self.sidebar.active_id()))
        {
            panel = panel
                .child(
                    readable(
                        "topic-detail-name",
                        format!("{} · {}", topic.name, topics::status(topic)),
                    )
                    .font_semibold(),
                )
                .child(readable("topic-detail-repo", topic.repo.display().to_string()).text_xs())
                .child(
                    readable(
                        "topic-workflow",
                        format!(
                            "{}{} builds → {}{} reviews · Formless manages handoffs",
                            topics::harness(topic.builder),
                            topic
                                .builder_model
                                .as_ref()
                                .map(|m| format!(" / {m}"))
                                .unwrap_or_default(),
                            topics::harness(topic.reviewer),
                            topic
                                .reviewer_model
                                .as_ref()
                                .map(|m| format!(" / {m}"))
                                .unwrap_or_default()
                        ),
                    )
                    .text_sm(),
                );
            let id = topic.id.clone();
            if topic.status == TopicStatus::PendingApproval {
                let reject_id = id.clone();
                panel = panel.child(readable("topic-consent", format!("Move these requests into {} and start this workflow? Formless will hand each feature from builder to reviewer. Nothing auto-merges.", topic.name)).text_sm())
                    .child(h_flex().flex_wrap().gap_2()
                        .child(Button::new("topic-approve").small().primary().label("Move and start").disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| this.topic_action(Action::MoveAndStart(id.clone()), cx))))
                        .child(Button::new("topic-reject").small().outline().label("Keep in original chat").disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| this.topic_action(Action::Reject(reject_id.clone()), cx)))));
            } else if topic.status == TopicStatus::Approved {
                panel = panel.child(
                    Button::new("topic-start")
                        .small()
                        .primary()
                        .label("Start workflow")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.topic_action(Action::Start(id.clone()), cx)
                        })),
                );
            } else if topic.status == TopicStatus::Failed {
                panel = panel.child(
                    Button::new("topic-recover")
                        .small()
                        .outline()
                        .label("Recover topic")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.topic_action(Action::Recover(id.clone()), cx)
                        })),
                );
            }
            for feature in &topic.features {
                let mut feature_row = v_flex()
                    .gap_1()
                    .min_w_0()
                    .border_l_2()
                    .border_color(cx.theme().border)
                    .pl_2()
                    .child(
                        readable(
                            SharedString::from(format!("topic-feature-{}", feature.id)),
                            format!(
                                "{} · {}",
                                topics::feature_status(feature.status),
                                feature.request
                            ),
                        )
                        .text_sm(),
                    );
                if let Some(detail) = &feature.detail {
                    feature_row = feature_row.child(Label::new(detail.clone()).text_xs());
                }
                if let Some(revision) = &feature.ready_revision {
                    feature_row = feature_row.child(
                        Label::new(format!("Reviewed revision: {revision} · not merged")).text_xs(),
                    );
                }
                if matches!(
                    feature.status,
                    TopicFeatureStatus::Failed | TopicFeatureStatus::NeedsUser
                ) {
                    let topic = topic.id.clone();
                    let feature = feature.id.clone();
                    feature_row = feature_row.child(
                        Button::new(SharedString::from(format!("topic-retry-{feature}")))
                            .small()
                            .outline()
                            .label("Retry feature")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.topic_action(
                                    Action::Retry {
                                        topic: topic.clone(),
                                        feature: feature.clone(),
                                    },
                                    cx,
                                )
                            })),
                    );
                }
                panel = panel.child(feature_row);
            }
        }
        if let Some(notice) = &ui.state.notice {
            panel = panel.child(
                announced(div().id("topic-notice"))
                    .test_support()
                    .role(Role::Status)
                    .aria_value(notice.clone())
                    .child(Label::new(notice.clone()).text_sm()),
            );
        }
        panel.into_any_element()
    }
}
