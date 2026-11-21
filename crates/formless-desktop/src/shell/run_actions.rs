use super::*;
use crate::run_actions::{Action, Context as ContextUsage, Kind, Ledger, Owner, Status, Target};
use std::sync::Arc;

enum Stage {
    Loading,
    Editing,
    Preparing,
    Review(Option<Box<formless_protocol::RunForecast>>),
    Confirming,
    Submitted,
}
pub(super) struct Panel {
    owner: Owner,
    operation: uuid::Uuid,
    kind: Kind,
    target: Option<Target>,
    stage: Stage,
    frozen: String,
    typed: Option<String>,
    initial_text: Option<String>,
    input: Entity<TextareaState>,
    notice: String,
    focus: FocusHandle,
    pub close: FocusHandle,
    scroll: ScrollHandle,
}
#[derive(Default)]
pub(super) struct State {
    pub queue: Option<(Owner, Vec<crate::message_queue::Item>)>,
    pub queue_open: Option<bool>,
    pub delete_undo: Option<crate::run_actions::DeleteUndo>,
    pub panel: Option<Panel>,
    pub ledger: Arc<Ledger>,
    pub context: Option<(Owner, ContextUsage)>,
    pub activity: Option<(Owner, String, String)>,
    pub notice: Option<(Owner, String)>,
    observed: Status,
}
impl Shell {
    pub(super) fn prefill_run_action(&mut self, text: String) {
        if let Some(panel) = &mut self.run_actions.panel {
            panel.initial_text = Some(text);
        }
    }
    pub(super) fn render_message_queue(
        &self,
        inert: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        use crate::message_queue::Operation;
        let (_, items) = self
            .run_actions
            .queue
            .as_ref()
            .filter(|(o, _)| self.telemetry_current(o) && self.client_snapshot_ready)?;
        let expanded = self
            .run_actions
            .queue_open
            .unwrap_or(self.effective_width(window) >= crate::model::tokens::THREE_PANE_MIN);
        let blocked = inert || self.run_actions.ledger.status().blocked();
        let mut panel = v_flex()
            .id("message-queue")
            .test_support()
            .role(Role::Group)
            .aria_label("Queued follow-ups")
            .gap_2()
            .p_2()
            .rounded(self.scaled(crate::theme::SURFACE_RADIUS))
            .border_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("queue-toggle")
                    .small()
                    .outline()
                    .label(format!(
                        "{} Queue ({})",
                        if expanded { "Hide" } else { "Show" },
                        items.len()
                    ))
                    .disabled(inert)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_actions.queue_open = Some(!expanded);
                        cx.notify();
                    })),
            );
        if expanded {
            let mut list = v_flex()
                .id("queue-items")
                .max_h(px(150.))
                .overflow_y_scroll()
                .gap_2();
            if items.is_empty() {
                list = list.child(
                    readable(
                        "queue-empty",
                        "No queued follow-ups. Queue is available during an active Run.",
                    )
                    .text_sm(),
                );
            }
            for (index, item) in items.iter().enumerate() {
                let id = item.id;
                let mut row = h_flex().flex_wrap().gap_2().items_center().child(
                    readable(
                        ("queue-text", index),
                        format!(
                            "{}. {}",
                            index + 1,
                            crate::questions::preview(&item.content, 180)
                        ),
                    )
                    .text_sm()
                    .flex_1()
                    .min_w_0(),
                );
                for (name, label, op, unavailable) in [
                    (
                        "queue-edit",
                        "Edit",
                        Operation::Edit(id),
                        item.revision.is_none(),
                    ),
                    ("queue-up", "Up", Operation::Up(id), index == 0),
                    (
                        "queue-down",
                        "Down",
                        Operation::Down(id),
                        index + 1 == items.len(),
                    ),
                    ("queue-remove", "Remove", Operation::Remove(id), false),
                ] {
                    row = row.child(
                        Button::new((name, index))
                            .small()
                            .outline()
                            .label(label)
                            .accessibility_label(format!("{label} queued message {}", index + 1))
                            .disabled(blocked || unavailable)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_run_action(Kind::Queue(op), window, cx)
                            })),
                    );
                }
                if item.revision.is_none() {
                    row = row.child(
                        readable(
                            ("queue-unsupported-edit", index),
                            "Update the daemon to edit safely.",
                        )
                        .text_xs(),
                    );
                }
                list = list.child(row);
            }
            panel = panel.child(list);
        }
        Some(panel.into_any_element())
    }
    pub(super) fn undo_deleted_session(&mut self, operation: uuid::Uuid, cx: &mut Context<Self>) {
        if let Some(ticket) = self
            .run_actions
            .delete_undo
            .take()
            .filter(|t| t.operation == operation && t.available())
        {
            if self.client_identity() == Some((ticket.owner.client, ticket.owner.connection))
                && self.client_connected
            {
                self.client_request(ClientCommand::UndoDelete(operation), cx);
            }
        }
        cx.notify();
    }
    pub(super) fn run_action_owner(&self) -> Option<Owner> {
        let client = self.client.as_ref()?;
        (self.client_connected
            && client.is_ready()
            && !self.harness_read_only()
            && self.client_snapshot_ready
            && self.destination == Destination::Chat)
            .then(|| Owner {
                client: client.instance(),
                connection: client.connection(),
                epoch: client.epoch(),
                generation: self.client_generation,
                session: self.sidebar.active_id().unwrap_or_default().into(),
            })
            .filter(|o| !o.session.is_empty())
    }
    pub(super) fn open_run_action(
        &mut self,
        kind: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.background_inert(window) || self.run_actions.ledger.status().blocked() {
            return;
        }
        let Some(owner) = self.run_action_owner() else {
            return;
        };
        if matches!(kind, Kind::Queue(_))
            && self
                .run_actions
                .queue
                .as_ref()
                .is_none_or(|(o, _)| !self.telemetry_current(o))
        {
            self.composer_notice = Some("Queue inventory is unavailable. Refresh the Session or update the daemon; nothing was sent.".into());
            cx.notify();
            return;
        }
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            return;
        }
        let operation = uuid::Uuid::new_v4();
        let close = cx.focus_handle();
        let input = cx.new(|cx| TextareaState::new(window, cx));
        self.run_actions.panel = Some(Panel {
            owner: owner.clone(),
            operation,
            kind,
            target: None,
            stage: Stage::Loading,
            frozen: String::new(),
            typed: None,
            initial_text: None,
            input,
            notice: "Reading the current canonical action target…".into(),
            focus: cx.focus_handle(),
            close: close.clone(),
            scroll: ScrollHandle::new(),
        });
        self.client_request(ClientCommand::ReadRunAction { owner, operation }, cx);
        self.settle(window, cx);
        window.focus(&close, cx);
        cx.notify();
    }
    pub(super) fn close_run_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.run_actions.ledger.cancel();
        self.run_actions.panel = None;
        self.settle(window, cx);
        self.focus_region(Region::Composer, window, cx);
        cx.notify();
    }
    pub(super) fn settle_run_actions(&mut self, cx: &mut Context<Self>) {
        let owner = self.run_action_owner();
        if self
            .run_actions
            .panel
            .as_ref()
            .is_some_and(|p| owner.as_ref() != Some(&p.owner))
        {
            self.run_actions.ledger.cancel();
            if let Some(p) = &mut self.run_actions.panel {
                p.notice = "Selection or connection changed. This review is retired. Close it and reopen the action; nothing is replayed.".into();
            }
        }
        let status = self.run_actions.ledger.status();
        if self.run_actions.observed != status {
            if matches!(
                status,
                Status::Applied(_) | Status::Reconciled | Status::Rejected(_) | Status::Unknown
            ) {
                self.composer_notice = Some(status.notice().into());
            }
            self.run_actions.observed = status;
            cx.notify();
        }
    }
    pub(super) fn run_action_target(
        &mut self,
        owner: Owner,
        operation: uuid::Uuid,
        result: Result<Target, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.run_action_owner().as_ref() != Some(&owner) {
            return;
        }
        let Some(p) = self.run_actions.panel.as_mut().filter(|p| {
            p.owner == owner && p.operation == operation && matches!(p.stage, Stage::Loading)
        }) else {
            return;
        };
        match result {
            Ok(target) => {
                let text = p.initial_text.take().unwrap_or_else(|| match p.kind {
                    Kind::Queue(op) if op.id().is_some() => target
                        .queue
                        .as_ref()
                        .and_then(|items| items.iter().find(|i| Some(i.id) == op.id()))
                        .map(|i| i.content.clone())
                        .unwrap_or_default(),
                    Kind::Queue(crate::message_queue::Operation::Enqueue) => {
                        crate::slash::message_text(&self.composer.read(cx).value()).to_string()
                    }
                    Kind::Edit => target
                        .user
                        .as_ref()
                        .map(|u| u.1.clone())
                        .unwrap_or_default(),
                    Kind::Cwd => target.cwd.clone().unwrap_or_default(),
                    _ => String::new(),
                });
                p.input
                    .update(cx, |input, cx| input.set_value(text, window, cx));
                p.notice = if target.allowed(p.kind) {
                    "Canonical target loaded. Review the exact action before confirming."
                } else {
                    target.unavailable_reason(p.kind)
                }
                .into();
                p.target = Some(target);
                p.stage = Stage::Editing;
            }
            Err(error) => {
                p.notice = error;
            }
        }
        cx.notify();
    }
    fn prepare_run_action(
        &mut self,
        operation: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = self.run_action_owner();
        let Some(p) = self.run_actions.panel.as_ref().filter(|p| {
            p.operation == operation
                && Some(&p.owner) == owner.as_ref()
                && matches!(p.stage, Stage::Editing)
        }) else {
            return;
        };
        if p.input
            .update(cx, |i, cx| i.marked_text_range(window, cx).is_some())
        {
            return;
        }
        let Some(target) = p.target.clone() else {
            return;
        };
        let text = p.input.read(cx).value().to_string();
        let action = Action {
            operation,
            owner: p.owner.clone(),
            target,
            kind: p.kind,
            text: text.clone(),
        };
        match self.run_actions.ledger.reserve(action) {
            Ok(d) => {
                let p = self.run_actions.panel.as_mut().unwrap();
                p.frozen = text;
                p.stage = Stage::Preparing;
                if let Err(e) = self
                    .client
                    .as_ref()
                    .unwrap()
                    .send(ClientCommand::PrepareRunAction(d))
                {
                    self.run_actions.ledger.cancel();
                    self.run_actions.panel.as_mut().unwrap().notice = e.into();
                }
            }
            Err(e) => self.run_actions.panel.as_mut().unwrap().notice = e,
        }
        cx.notify();
    }
    pub(super) fn run_action_review(
        &mut self,
        owner: Owner,
        operation: uuid::Uuid,
        quote: Option<formless_protocol::RunForecast>,
        reviewed_text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.run_action_owner().as_ref() != Some(&owner) {
            return;
        }
        if let Some(p) = self.run_actions.panel.as_mut().filter(|p| {
            p.owner == owner && p.operation == operation && matches!(p.stage, Stage::Preparing)
        }) {
            if let Some(text) = reviewed_text.filter(|_| p.kind == Kind::Cwd) {
                p.typed = (p.frozen != text).then(|| p.frozen.clone());
                p.frozen = text;
            }
            p.stage = Stage::Review(quote.map(Box::new));
        }
        cx.notify();
    }
    fn confirm_run_action(&mut self, operation: uuid::Uuid, cx: &mut Context<Self>) {
        let owner = self.run_action_owner();
        let Some(p) = self.run_actions.panel.as_mut().filter(|p| {
            p.operation == operation
                && Some(&p.owner) == owner.as_ref()
                && matches!(p.stage, Stage::Review(_))
        }) else {
            return;
        };
        p.stage = Stage::Confirming;
        self.client_request(ClientCommand::ConfirmRunAction(operation), cx);
    }
    pub(super) fn run_action_ready(
        &mut self,
        owner: Owner,
        operation: uuid::Uuid,
        cx: &mut Context<Self>,
    ) {
        if self.run_action_owner().as_ref() != Some(&owner) {
            self.run_actions.ledger.cancel_operation(operation);
            return;
        }
        let Some(p) = self.run_actions.panel.as_mut().filter(|p| {
            p.owner == owner && p.operation == operation && matches!(p.stage, Stage::Confirming)
        }) else {
            self.run_actions.ledger.cancel_operation(operation);
            return;
        };
        p.stage = Stage::Submitted;
        self.client_request(ClientCommand::CommitRunAction(operation), cx);
    }
    pub(super) fn live_context_label(&self) -> Option<String> {
        self.run_actions
            .context
            .as_ref()
            .filter(|(o, _)| self.telemetry_current(o))
            .map(|(_, c)| c.label())
    }
    pub(super) fn telemetry_current(&self, owner: &Owner) -> bool {
        self.client_connected
            && self
                .client
                .as_ref()
                .is_some_and(|c| c.instance() == owner.client && c.connection() == owner.connection)
            && self.client_generation == owner.generation
            && self.sidebar.active_id() == Some(owner.session.as_str())
    }
    pub(super) fn run_activity_label(&self) -> Option<String> {
        self.run_actions
            .activity
            .as_ref()
            .filter(|(o, _, _)| self.telemetry_current(o))
            .map(|(_, _, text)| text.clone())
    }
    pub(super) fn run_notice_label(&self) -> Option<String> {
        self.run_actions
            .notice
            .as_ref()
            .filter(|(o, _)| self.telemetry_current(o))
            .map(|(_, text)| text.clone())
    }
    pub(super) fn run_activity(&mut self, owner: Owner, event: Event, cx: &mut Context<Self>) {
        if !self.telemetry_current(&owner) {
            return;
        }
        match event {
            Event::ProviderStalled { run_id, idle_secs, .. } => self.run_actions.activity = Some((owner, run_id, format!("Provider stalled · silent for {idle_secs}s. Retry provider request continues this Run."))),
            Event::ProviderRetrying { run_id, attempt, max_attempts, wait_secs, reason, .. } => self.run_actions.activity = Some((owner, run_id, format!("Provider retrying · {attempt}/{max_attempts} · wait {wait_secs}s · {}", crate::questions::preview(&reason, 300)))),
            Event::CompactionNotice { summary_preview, .. } => {
                self.run_actions.context = None;
                self.run_actions.notice = Some((owner, format!("Compaction complete · {}", crate::questions::preview(&summary_preview, 500))));
            },
            Event::Note { text, .. } if text.starts_with("compaction failed:") => self.run_actions.notice = Some((owner, crate::questions::preview(&text, 500))),
            Event::RunUsage { run_id, usage, .. } => {
                if usage.context_used >= 0 && usage.context_tokens > 0 {
                    self.run_actions.context = Some((owner.clone(), ContextUsage { used: usage.context_used as u64, limit: usage.context_tokens as u64, measured: true }));
                }
                self.run_actions.notice = Some((owner, format!("Run usage · {} prompt / {} completion tokens · {} steps · {}", usage.prompt_tokens, usage.completion_tokens, usage.steps, usage.cost.map_or_else(|| "cost unavailable".into(), |v| format!("${v:.4}")))));
                if self.run_actions.activity.as_ref().is_some_and(|(_, r, _)| *r == run_id) { self.run_actions.activity = None; }
            }
            Event::RunStarted { .. } => {
                self.run_actions.context = None;
                self.run_actions.activity = None;
            },
            Event::RunFinished { .. } | Event::RunPaused { .. } | Event::MessageComplete { .. } | Event::StreamRestarted { .. } => self.run_actions.activity = None,
            _ => {},
        }
        cx.notify();
    }
    pub(super) fn render_run_actions(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.run_actions.panel.as_ref().unwrap();
        let operation = p.operation;
        let current = self.run_action_owner().as_ref() == Some(&p.owner);
        let status = self.run_actions.ledger.status();
        let viewport = window.viewport_size();
        let mut body = v_flex()
            .id("run-action-body")
            .test_support()
            .min_h_0()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&p.scroll)
            .gap_3()
            .child(readable("run-action-help", p.kind.help()).text_sm())
            .children(
                self.run_activity_label()
                    .map(|text| readable("run-action-activity", text).text_sm()),
            )
            .child(
                readable(
                    "run-action-session",
                    format!("Session: {}", p.owner.session),
                )
                .text_sm(),
            )
            .child(
                readable(
                    "run-action-target",
                    p.target.as_ref().map_or_else(
                        || "Loading target…".into(),
                        |t| {
                            format!(
                                "Run: {} · last message: {}",
                                t.run.as_deref().unwrap_or("none"),
                                t.user.as_ref().map_or("none", |u| u.0.as_str())
                            )
                        },
                    ),
                )
                .text_xs(),
            )
            .child(readable("run-action-notice", p.notice.clone()).text_sm())
            .child(readable("run-action-status", status.notice()).text_sm());
        if matches!(p.stage, Stage::Editing) && !matches!(p.kind, Kind::Queue(_)) {
            body = body.child(h_flex().flex_wrap().gap_2().children(
                Kind::ALL.into_iter().enumerate().map(|(i, kind)| {
                    Button::new(("run-action-kind", i))
                        .small()
                        .outline()
                        .label(kind.label())
                        .selected(kind == p.kind)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.choose_run_action(kind, window, cx)
                        }))
                }),
            ));
        }
        if matches!(p.stage, Stage::Editing)
            && !matches!(
                p.kind,
                Kind::Delete
                    | Kind::ProviderRetry
                    | Kind::Queue(
                        crate::message_queue::Operation::Up(_)
                            | crate::message_queue::Operation::Down(_)
                            | crate::message_queue::Operation::Remove(_)
                    )
            )
        {
            body = body
                .child(readable("run-action-input-label", p.kind.input_label()).text_sm())
                .child(Textarea::new(&p.input).aria_label(p.kind.input_label()));
        } else if !p.frozen.is_empty() {
            body = body.child(
                readable(
                    "run-action-frozen",
                    format!(
                        "Exact reviewed input (JSON): {}",
                        serde_json::to_string(&p.frozen).unwrap()
                    ),
                )
                .text_sm(),
            );
        }
        if let Stage::Review(Some(q)) = &p.stage {
            body = body.children(
                forecast::summary(q)
                    .into_iter()
                    .enumerate()
                    .map(|(i, text)| div().id(("run-action-quote", i)).text_sm().child(text)),
            );
        }
        if let Some(typed) = &p.typed {
            body = body.child(
                readable(
                    "run-action-typed",
                    format!(
                        "Typed input (JSON): {}",
                        serde_json::to_string(typed).unwrap()
                    ),
                )
                .text_sm(),
            );
        }
        let dialog = v_flex().id("run-action-dialog").test_support().role(Role::Dialog).aria_label(p.kind.label()).aria_description("Modal action review. Escape cancels. Review the exact Session, Run and input before confirming. Writes are acknowledged and never replayed.").map(modal_dialog)
            .key_context("FormlessRunActions")
            .on_action(cx.listener(|this, _: &CloseRunActions, window, cx| this.close_run_actions(window, cx)))
            .w(self.scaled(640.).min(viewport.width - px(24.))).max_h(viewport.height - px(24.)).p_3().gap_3()
            .bg(cx.theme().popover).text_color(cx.theme().popover_foreground).border_1().border_color(cx.theme().border).rounded(px(crate::theme::SURFACE_RADIUS)).shadow_lg().occlude()
            .child(readable("run-action-heading", p.kind.label()).text_lg().font_semibold().flex_shrink_0())
            .child(body)
            .child(h_flex().flex_wrap().flex_shrink_0().gap_2()
                .child(Button::new("run-action-close").label("Cancel / Close").track_focus(&p.close).on_click(cx.listener(|this, _, window, cx| this.close_run_actions(window, cx))))
                .child(Button::new("run-action-prepare").label("Review exact action").outline().disabled(!current || !matches!(p.stage, Stage::Editing) || !p.target.as_ref().is_some_and(|t| t.allowed(p.kind)) || status.blocked()).on_click(cx.listener(move |this, _, window, cx| this.prepare_run_action(operation, window, cx))))
                .child(Button::new("run-action-confirm").label(format!("Confirm: {}", p.kind.label())).primary().disabled(!current || !matches!(p.stage, Stage::Review(_)) || !matches!(status, Status::Reviewing(_))).on_click(cx.listener(move |this, _, _, cx| this.confirm_run_action(operation, cx)))))
            .focus_trap("run-action-dialog", &p.focus);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(cx.theme().overlay)
                    .occlude(),
            )
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .justify_center()
                    .items_start()
                    .pt_3()
                    .child(dialog),
            )
    }
    fn choose_run_action(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let owner = self.run_action_owner();
        let Some(p) = self
            .run_actions
            .panel
            .as_mut()
            .filter(|p| Some(&p.owner) == owner.as_ref() && matches!(p.stage, Stage::Editing))
        else {
            return;
        };
        if p.input
            .update(cx, |i, cx| i.marked_text_range(window, cx).is_some())
        {
            return;
        }
        let text = match kind {
            Kind::Edit => p
                .target
                .as_ref()
                .and_then(|t| t.user.as_ref())
                .map(|u| u.1.clone())
                .unwrap_or_default(),
            Kind::Cwd => p
                .target
                .as_ref()
                .and_then(|t| t.cwd.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        p.input.update(cx, |i, cx| i.set_value(text, window, cx));
        p.kind = kind;
        p.operation = uuid::Uuid::new_v4();
        p.notice = p
            .target
            .as_ref()
            .filter(|t| !t.allowed(kind))
            .map_or("Review the exact action before confirming.", |t| {
                t.unavailable_reason(kind)
            })
            .into();
        cx.notify();
    }
}
