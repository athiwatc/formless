use super::*;
use crate::topics::{self, Action, Reply};
use formless_protocol::{HarnessKind, TopicFeatureStatus, TopicStatus};
mod outline;
mod parent;
mod policy;

pub(super) struct Ui {
    pub state: topics::State,
    proposing: bool,
    source: Option<String>,
    selected: HashSet<String>,
    name: Entity<InputState>,
    workspace: Entity<InputState>,
    outline: Vec<outline::Step>,
    parent: Option<String>,
    choosing_parent: bool,
    parent_scroll: ScrollHandle,
    scroll: ScrollHandle,
    tree_scroll: ScrollHandle,
    pending_open: Option<String>,
    show_outline_details: bool,
    policy: Option<policy::Editor>,
}

impl Ui {
    pub fn reset_connection(&mut self) {
        self.state = Default::default();
        self.proposing = false;
        self.source = None;
        self.selected.clear();
        self.pending_open = None;
        self.show_outline_details = false;
        self.parent = None;
        self.choosing_parent = false;
        self.policy = None;
    }
    pub fn retain_candidates(&mut self, available: &HashSet<&str>) {
        self.selected.retain(|id| available.contains(id.as_str()));
    }
    pub fn session_changed(&mut self, session: Option<&str>) {
        self.policy = None;
        self.proposing = false;
        self.selected.clear();
        self.state.candidates.clear();
        self.state.candidate_session = None;
        self.state.selected = self
            .state
            .pending_for(session)
            .map(|topic| topic.id.clone())
            .or_else(|| {
                self.state
                    .topics
                    .iter()
                    .find(|t| {
                        t.topic_session_id.as_deref() == session && session.is_some()
                            || t.attempts
                                .iter()
                                .any(|a| Some(a.conversation_session_id.as_str()) == session)
                    })
                    .map(|t| t.id.clone())
            });
    }
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        Self {
            state: Default::default(),
            proposing: false,
            source: None,
            selected: HashSet::new(),
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Topic name, e.g. Formless")),
            workspace: cx
                .new(|cx| InputState::new(window, cx).placeholder("Workspace folder (optional)")),
            outline: Vec::new(),
            parent: None,
            choosing_parent: false,
            parent_scroll: ScrollHandle::new(),
            scroll: ScrollHandle::new(),
            tree_scroll: ScrollHandle::new(),
            pending_open: None,
            show_outline_details: false,
            policy: None,
        }
    }
}

impl Shell {
    fn toggle_topic_policy(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .topic_ui
            .policy
            .as_ref()
            .is_some_and(|editor| editor.topic == id)
        {
            self.topic_ui.policy = None;
        } else if let Some(topic) = self
            .topic_ui
            .state
            .topics
            .iter()
            .find(|topic| topic.id == id)
        {
            self.topic_ui.policy = Some(policy::Editor::new(
                topic.id.clone(),
                topic.limits.max_retries_per_step,
                topic.limits.max_attempts_per_topic,
                window,
                cx,
            ));
        }
        cx.notify();
    }
    fn save_topic_rule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.topic_ui.policy else {
            return;
        };
        if editor.composing(window, cx) {
            self.topic_ui.state.notice =
                Some("Finish composing the controls first; nothing was saved.".into());
            cx.notify();
            return;
        }
        let result = topics::RuleDraft {
            harness: editor.harness,
            acp_agent: editor.acp_agent.read(cx).value().to_string(),
            kind: editor.kind.read(cx).value().to_string(),
            pattern: editor.pattern.read(cx).value().to_string(),
            allow: editor.allow,
        }
        .action(editor.topic.clone());
        match result {
            Ok(action) => self.topic_action(action, cx),
            Err(error) => {
                self.topic_ui.state.notice = Some(error);
                cx.notify();
            }
        }
    }
    fn save_topic_limits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.topic_ui.policy else {
            return;
        };
        if editor.composing(window, cx) {
            self.topic_ui.state.notice =
                Some("Finish composing the controls first; nothing was saved.".into());
            cx.notify();
            return;
        }
        match topics::limits(
            &editor.retries.read(cx).value(),
            &editor.attempts.read(cx).value(),
        ) {
            Ok(limits) => self.topic_action(
                Action::SetLimits {
                    topic: editor.topic.clone(),
                    limits,
                },
                cx,
            ),
            Err(error) => {
                self.topic_ui.state.notice = Some(error);
                cx.notify();
            }
        }
    }
    #[cfg(feature = "test-support")]
    pub fn topic_policy_inputs(&self) -> Option<[Entity<InputState>; 5]> {
        self.topic_ui.policy.as_ref().map(|editor| {
            [
                editor.pattern.clone(),
                editor.kind.clone(),
                editor.acp_agent.clone(),
                editor.retries.clone(),
                editor.attempts.clone(),
            ]
        })
    }
    #[cfg(feature = "test-support")]
    pub fn topic_inputs(&self) -> [Entity<InputState>; 2] {
        [self.topic_ui.name.clone(), self.topic_ui.workspace.clone()]
    }
    #[cfg(feature = "test-support")]
    pub fn topic_step_inputs(&self) -> Vec<(uuid::Uuid, [Entity<InputState>; 3])> {
        self.topic_ui
            .outline
            .iter()
            .map(|step| {
                (
                    step.id,
                    [
                        step.title.clone(),
                        step.instructions.clone(),
                        step.model.clone(),
                    ],
                )
            })
            .collect()
    }

    fn set_topic_outline(
        &mut self,
        preset: topics::Preset,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.topic_ui.outline = topics::preset(preset)
            .into_iter()
            .map(|step| outline::Step::new(step, window, cx))
            .collect();
        cx.notify();
    }
    fn topic_action(&mut self, action: Action, cx: &mut Context<Self>) {
        if !self.client_connected || self.topic_ui.state.pending.is_some() {
            return;
        }
        let Some(client) = &self.client else {
            return;
        };
        let operation = (!matches!(action, Action::Main | Action::List | Action::Harnesses))
            .then(uuid::Uuid::new_v4);
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
                let newly_pending = if let Reply::Inventory(topics) = &reply {
                    topics
                        .iter()
                        .find(|topic| {
                            topic.status == TopicStatus::PendingApproval
                                && Some(topic.source_session_id.as_str())
                                    == self.sidebar.active_id()
                                && !self
                                    .topic_ui
                                    .state
                                    .topics
                                    .iter()
                                    .any(|old| old.id == topic.id)
                        })
                        .map(|topic| topic.id.clone())
                } else {
                    None
                };
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
                    self.topic_ui.state.notice = Some("Updated from Formless.".into());
                }
                self.topic_ui.state.apply(reply);
                if let Some(id) = newly_pending {
                    self.topic_ui.state.selected = Some(id);
                }
                if approved.is_some() {
                    self.topic_ui.proposing = false;
                }
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
        self.topic_ui.parent = None;
        self.topic_ui.choosing_parent = false;
        let cwd = self
            .client_sessions
            .iter()
            .find(|s| Some(s.id.as_str()) == self.sidebar.active_id())
            .and_then(|s| s.cwd.clone())
            .unwrap_or_default();
        self.topic_ui
            .workspace
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
        let workspace = ui.workspace.read(cx).value().to_string().trim().to_string();
        let message_ids = ui
            .state
            .candidates
            .iter()
            .filter(|m| ui.selected.contains(&m.id))
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        if name.is_empty() || message_ids.is_empty() {
            self.topic_ui.state.notice =
                Some("Choose at least one saved request and a topic name.".into());
            cx.notify();
            return;
        }
        if ui.parent.as_ref().is_some_and(|id| {
            !ui.state.topics.iter().any(|topic| {
                &topic.id == id
                    && !matches!(
                        topic.status,
                        TopicStatus::PendingApproval | TopicStatus::Rejected
                    )
            })
        }) {
            self.topic_ui.state.notice =
                Some("Choose an available parent topic or use the source context.".into());
            cx.notify();
            return;
        }
        let outline = ui
            .outline
            .iter()
            .map(|step| step.value(cx))
            .collect::<Vec<_>>();
        if outline.iter().any(|step| step.title.is_empty()) {
            self.topic_ui.state.notice =
                Some("Give each suggested step a title, or remove it.".into());
            cx.notify();
            return;
        }
        if outline
            .iter()
            .any(|step| step.harness == Some(HarnessKind::Acp) && step.acp_agent.is_none())
        {
            self.topic_ui.state.notice =
                Some("Choose the configured ACP agent for each ACP step.".into());
            cx.notify();
            return;
        }
        let proposal = formless_protocol::TopicProposalRequest {
            source_session_id: ui.source.clone().unwrap(),
            parent_topic_id: ui.parent.clone(),
            message_ids,
            features: vec![],
            name,
            workspace: (!workspace.is_empty()).then(|| workspace.into()),
            outline,
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
            Some("Harness transcript is read only. Use Questions / permissions for harness requests, or Topic Chat to steer Formless.")
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
        let displayed = state
            .displayed(self.sidebar.active_id())
            .map(|topic| topic.id.as_str());
        for (depth, topic) in state.tree() {
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
                    .pl(px((depth.min(6) * 12) as f32))
                    .child(
                        Button::new(SharedString::from(format!("topic-{}", topic.id)))
                            .ghost()
                            .label(topic.name.clone())
                            .accessibility_label(format!(
                                "Level {}, {}, {}",
                                depth + 1,
                                topic.name,
                                topics::status(topic)
                            ))
                            .selected(displayed == Some(topic.id.as_str()))
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
                    "{} · {} · {}",
                    attempt.step_title,
                    topics::chosen_harness(Some(attempt.harness), attempt.acp_agent.as_deref()),
                    topics::attempt_status(attempt.status)
                );
                let available = self.sidebar.rows().iter().any(|s| s.id == session);
                tree = tree.child(
                    div()
                        .pl(px(16. + (depth.min(6) * 12) as f32))
                        .min_w_0()
                        .child(
                            Button::new(SharedString::from(format!(
                                "topic-attempt-{}",
                                attempt.id
                            )))
                            .small()
                            .ghost()
                            .label(label)
                            .disabled(!available)
                            .selected(self.sidebar.active_id() == Some(session.as_str()))
                            .tooltip("Read the harness conversation. Formless owns the handoff.")
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_topic_session(&session, window, cx);
                                    this.topic_ui.state.selected = Some(topic_id.clone());
                                    this.topic_ui.proposing = false;
                                    this.prefs.close_overlays();
                                    this.settle(window, cx);
                                },
                            )),
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
        let current_topic = ui.state.displayed(self.sidebar.active_id());
        let awaiting_consent =
            current_topic.is_some_and(|topic| topic.status == TopicStatus::PendingApproval);
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
            .max_h(
                window.viewport_size().height
                    * if ui.proposing {
                        0.65
                    } else if awaiting_consent {
                        0.55
                    } else {
                        0.38
                    },
            )
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
                    )
                    .when_some(current_topic.filter(|_| !ui.proposing), |row, topic| {
                        row.child(policy::toggle(ui, topic, busy, cx))
                    }),
            );
        if ui.proposing {
            panel = panel.child(readable("topic-proposal-help", "Choose saved requests, a topic name and an optional starting outline. Nothing moves or starts until you approve.").text_sm())
                .child(Input::new(&ui.name).small().aria_label("Topic name"))
                .child(parent::editor(ui, busy, cx))
                .child(Input::new(&ui.workspace).small().aria_label("Workspace folder (optional)"))
                .child(readable("topic-workspace-help", "Leave the workspace blank to use a topic scratch folder.").text_xs())
                .child(outline::editor(ui, busy, cx));
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
        } else if let Some(topic) = current_topic {
            if ui
                .policy
                .as_ref()
                .is_some_and(|editor| editor.topic == topic.id)
            {
                panel = panel
                    .child(policy::editor(ui, topic, busy, cx))
                    .child(policy::availability(ui, busy, cx));
            }
            let mut breadcrumbs = h_flex()
                .id("topic-breadcrumbs")
                .test_support()
                .gap_1()
                .flex_wrap();
            if let Some(main) = &ui.state.main {
                let id = main.id.clone();
                breadcrumbs = breadcrumbs.child(
                    Button::new("topic-breadcrumb-main")
                        .small()
                        .ghost()
                        .label("Main Chat")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.topic_ui.state.selected = None;
                            this.topic_ui.proposing = false;
                            this.open_topic_session(&id, window, cx);
                        })),
                );
            }
            for ancestor in ui.state.breadcrumbs(&topic.id) {
                let id = ancestor.id.clone();
                breadcrumbs =
                    breadcrumbs.child(Label::new("/").text_xs()).child(
                        Button::new(SharedString::from(format!("topic-breadcrumb-{id}")))
                            .small()
                            .ghost()
                            .label(ancestor.name.clone())
                            .selected(ancestor.id == topic.id)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_topic(&id, window, cx)
                            })),
                    );
            }
            panel = panel.child(breadcrumbs);
            panel = panel
                .child(
                    readable(
                        "topic-detail-name",
                        format!("{} · {}", topic.name, topics::status(topic)),
                    )
                    .font_semibold(),
                )
                .child(readable("topic-detail-workspace", topic.workspace.as_ref().map(|path| format!("Workspace: {}", path.display())).unwrap_or_else(|| "Workspace: topic scratch folder".into())).text_xs())
                .child(
                    readable(
                        "topic-workflow",
                        "The Formless model decides the next steps. This outline is guidance, not a fixed pipeline.",
                    )
                    .text_sm(),
                );
            for (index, step) in topic.outline.iter().enumerate() {
                panel = panel.child(
                    readable(
                        SharedString::from(format!("topic-outline-{index}")),
                        format!(
                            "{}. {} · {}{} · {}",
                            index + 1,
                            step.title,
                            topics::chosen_harness(step.harness, step.acp_agent.as_deref()),
                            step.model
                                .as_ref()
                                .map(|model| format!(" / {model}"))
                                .unwrap_or_default(),
                            topics::access(step.access),
                        ),
                    )
                    .text_sm(),
                );
                if ui.show_outline_details {
                    panel = panel.child(
                        readable(
                            SharedString::from(format!("topic-outline-instructions-{index}")),
                            step.instructions.clone(),
                        )
                        .text_xs(),
                    );
                }
            }
            if !topic.outline.is_empty() {
                panel = panel.child(
                    Button::new("topic-outline-details")
                        .small()
                        .ghost()
                        .label(if ui.show_outline_details {
                            "Hide step details"
                        } else {
                            "Show step details"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.topic_ui.show_outline_details =
                                !this.topic_ui.show_outline_details;
                            cx.notify();
                        })),
                );
            }
            if !read_only && matches!(topic.status, TopicStatus::NeedsUser | TopicStatus::Running) {
                panel = panel.child(
                    readable(
                        "topic-steering-help",
                        "Reply in Topic Chat to answer Formless or steer the workflow.",
                    )
                    .text_xs(),
                );
            }
            if let Some((_, attempt)) = ui
                .state
                .attempt_for(self.sidebar.active_id().unwrap_or_default())
            {
                panel = panel.child(
                    readable(
                        "topic-active-step",
                        format!(
                            "{} · {}{} · {} · {}",
                            attempt.step_title,
                            topics::chosen_harness(
                                Some(attempt.harness),
                                attempt.acp_agent.as_deref()
                            ),
                            attempt
                                .model
                                .as_ref()
                                .map(|model| format!(" / {model}"))
                                .unwrap_or_default(),
                            topics::access(attempt.access),
                            topics::attempt_status(attempt.status)
                        ),
                    )
                    .text_sm(),
                );
                if let Some(summary) = &attempt.summary {
                    panel =
                        panel.child(readable("topic-attempt-summary", summary.clone()).text_sm());
                }
                for (index, artifact) in attempt.inputs.iter().enumerate() {
                    panel = panel.child(
                        readable(
                            SharedString::from(format!("topic-attempt-input-{index}")),
                            format!("Input · {}", topics::artifact(artifact)),
                        )
                        .text_xs(),
                    );
                }
                for (index, artifact) in attempt.outputs.iter().enumerate() {
                    panel = panel.child(
                        readable(
                            SharedString::from(format!("topic-attempt-output-{index}")),
                            format!("Output · {}", topics::artifact(artifact)),
                        )
                        .text_xs(),
                    );
                }
            }
            let id = topic.id.clone();
            if topic.status == TopicStatus::PendingApproval {
                let reject_id = id.clone();
                panel = panel.child(readable("topic-consent", format!("Move these requests into {} and start this workflow? The Formless model will use this outline as guidance and decide the next steps.", topic.name)).text_sm())
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
                    readable(
                        "topic-recovery-help",
                        "Recovery does not reset or start work. Reset an item for retry below, then choose Start workflow; recovery does not move them again.",
                    )
                    .text_sm(),
                );
                panel = panel.child(
                    Button::new("topic-recover")
                        .small()
                        .outline()
                        .label("Recover topic")
                        .tooltip("Recovery does not reset or start work. Reset an item for retry, then explicitly start the workflow.")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.topic_action(Action::Recover(id.clone()), cx)
                        })),
                );
            }
            if matches!(
                topic.status,
                TopicStatus::Approved | TopicStatus::Running | TopicStatus::NeedsUser
            ) {
                let id = topic.id.clone();
                panel = panel.child(
                    Button::new("topic-cancel")
                        .small()
                        .outline()
                        .label("Cancel workflow")
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.topic_action(Action::Cancel(id.clone()), cx)
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
                for (index, artifact) in feature.outputs.iter().enumerate() {
                    feature_row = feature_row.child(
                        readable(
                            SharedString::from(format!("topic-output-{}-{index}", feature.id)),
                            topics::artifact(artifact),
                        )
                        .text_xs(),
                    );
                }
                if matches!(
                    feature.status,
                    TopicFeatureStatus::Failed
                        | TopicFeatureStatus::NeedsUser
                        | TopicFeatureStatus::Cancelled
                ) {
                    let topic_id = topic.id.clone();
                    let feature = feature.id.clone();
                    feature_row = feature_row.child(
                        Button::new(SharedString::from(format!("topic-retry-{feature}")))
                            .small()
                            .outline()
                            .label("Reset item for retry")
                            .tooltip("Return the workflow to Approved without starting it or moving requests again.")
                            .disabled(busy || topic.status == TopicStatus::Running)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.topic_action(
                                    Action::Retry {
                                        topic: topic_id.clone(),
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
