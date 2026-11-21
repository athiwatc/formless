use super::*;
use crate::step_decision::{self as model, Choice, Status};
use formless_protocol::{PendingStep, STEP_PREVIEW_BYTES};

pub(super) struct Panel {
    instance: uuid::Uuid,
    connection: u64,
    generation: u64,
    prompt: PendingStep,
    input: Entity<TextareaState>,
    _subscription: Subscription,
    accepted: String,
    observed: Status,
    operation: Option<uuid::Uuid>,
    notice: Option<String>,
    pub close: FocusHandle,
    focus: FocusHandle,
    scroll: ScrollHandle,
    controls: Vec<(FocusHandle, gpui_kit::ScrollAnchor)>,
    input_anchor: gpui_kit::ScrollAnchor,
    revealed: Option<FocusHandle>,
    viewport: Option<Size<Pixels>>,
    scale: TextScale,
    frames: u8,
}
impl Shell {
    pub(super) fn step_prompt(&self) -> Option<PendingStep> {
        if !self.client_connected || !self.client_snapshot_ready {
            return None;
        }
        let crate::steps::Inventory::Snapshot(snapshot) = &self.client_steps else {
            return None;
        };
        if snapshot.more || snapshot.prompts.len() != 1 {
            return None;
        }
        let prompt = &snapshot.prompts[0];
        if !model::complete(prompt) {
            return None;
        }
        let client = self.client.as_ref()?;
        if !client.is_ready() || client.step_status().blocked() {
            return None;
        }
        if matches!(client.step_status(),Status::Finished{action,outcome:model::Outcome::Accepted} if action.prompt.step_id==prompt.step_id)
        {
            return None;
        }
        Some(prompt.clone())
    }
    pub(super) fn open_step_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_modal(self.layout(window)).is_some() {
            return;
        }
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            self.composer_notice = Some("Finish composing before Step review.".into());
            cx.notify();
            return;
        }
        let Some(prompt) = self.step_prompt() else {
            return;
        };
        let client = self.client.as_ref().unwrap();
        let instance = client.instance();
        let connection = client.connection();
        let text = self
            .step_draft
            .as_ref()
            .filter(|(owner, p, _)| *owner == instance && p == &prompt)
            .map(|(_, _, s)| s.clone())
            .unwrap_or_else(|| prompt.arguments_preview.clone());
        self.invalidate_forecast(cx);
        self.palette = None;
        self.viewer = None;
        self.prefs.close_overlays();
        let (input, subscription) = Self::step_input(&text, window, cx);
        let scroll = ScrollHandle::new();
        self.step_panel = Some(Panel {
            instance,
            connection,
            generation: self.client_generation,
            prompt,
            input,
            _subscription: subscription,
            accepted: text,
            observed: Status::Idle,
            operation: None,
            notice: None,
            close: cx.focus_handle(),
            focus: cx.focus_handle(),
            input_anchor: gpui_kit::ScrollAnchor::for_handle(scroll.clone()),
            controls: (0..6)
                .map(|_| {
                    (
                        cx.focus_handle(),
                        gpui_kit::ScrollAnchor::for_handle(scroll.clone()),
                    )
                })
                .collect(),
            scroll,
            revealed: None,
            viewport: None,
            scale: self.text_scale,
            frames: 2,
        });
        self.settle(window, cx);
    }
    fn step_input(
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TextareaState>, Subscription) {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 3)
                .submit_on_enter(false)
                .default_value(text.to_owned())
        });
        let subscription = cx.observe_in(&input, window, |this, _, window, cx| {
            this.step_edit_changed(window, cx)
        });
        (input, subscription)
    }
    fn step_edit_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.step_panel.as_mut() else {
            return;
        };
        panel.frames = 2;
        let text = panel.input.read(cx).value().to_string();
        if text.len() <= STEP_PREVIEW_BYTES {
            panel.accepted = text;
            cx.notify();
            return;
        }
        if panel.input.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            return;
        }
        let input = panel.input.clone();
        let accepted = panel.accepted.clone();
        cx.defer_in(window,move|this,window,cx|{
            let Some(panel)=&this.step_panel else{return;};
            if panel.input!=input || input.read(cx).value().len()<=STEP_PREVIEW_BYTES || input.update(cx,|input,cx|input.marked_text_range(window,cx).is_some()){return;}
            let focused=input.focus_handle(cx).contains_focused(window,cx);let (fresh,sub)=Self::step_input(&accepted,window,cx);
            if focused{fresh.update(cx,|input,cx|input.focus(window,cx));}
            let panel=this.step_panel.as_mut().unwrap();panel.input=fresh;panel._subscription=sub;panel.notice=Some("Edit not added: native Step arguments are limited to 4 KiB. Earlier text kept; edit undo history cleared.".into());cx.notify();
        });
        cx.notify();
    }
    fn step_review_current(&self, panel: &Panel) -> bool {
        self.client_connected
            && self.client_snapshot_ready
            && self.client_generation == panel.generation
            && self.sidebar.active_id() == Some(panel.prompt.session_id.as_str())
            && self.client.as_ref().is_some_and(|client| {
                client.is_ready()
                    && client.instance() == panel.instance
                    && client.connection() == panel.connection
            })
            && matches!(&self.client_steps,crate::steps::Inventory::Snapshot(s) if s.prompts.iter().any(|p|p==&panel.prompt))
    }
    pub fn submit_step_review(
        &mut self,
        choice: Choice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Some(Modal::Step) {
            return;
        }
        let Some(panel) = self.step_panel.as_ref() else {
            return;
        };
        if !self.step_review_current(panel) {
            self.step_panel.as_mut().unwrap().notice=Some("This Step review is stale or disconnected. No decision was queued. Close and review a fresh prompt.".into());
            cx.notify();
            return;
        }
        let status = self.client.as_ref().unwrap().step_status();
        if status.blocked()
            || matches!(&status,Status::Finished{action,outcome:model::Outcome::Accepted} if action.prompt.step_id==panel.prompt.step_id)
        {
            self.step_panel.as_mut().unwrap().notice = Some(
                status
                    .notice()
                    .unwrap_or("This Step decision was already accepted; no retry was queued.")
                    .into(),
            );
            cx.notify();
            return;
        }
        if panel.input.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            self.step_panel.as_mut().unwrap().notice =
                Some("Finish composing edited arguments first; no decision was sent.".into());
            cx.notify();
            return;
        }
        if let Err(error) = choice.validate() {
            self.step_panel.as_mut().unwrap().notice = Some(error.into());
            cx.notify();
            return;
        }
        let client = self.client.as_ref().unwrap();
        let action = model::Action {
            client: client.instance(),
            connection: client.connection(),
            generation: self.client_generation,
            epoch: client.epoch(),
            operation: uuid::Uuid::new_v4(),
            prompt: panel.prompt.clone(),
            choice,
        };
        let operation = action.operation;
        let result = client.decide_step(action);
        let queued = result.is_ok();
        let recorded = matches!(client.step_status(),Status::Reserved(a)|Status::Sent(a)|Status::Finished{action:a,..} if a.operation==operation);
        if recorded {
            self.step_panel.as_mut().unwrap().operation = Some(operation);
        }
        self.step_panel.as_mut().unwrap().notice = Some(match result {
            Ok(()) => {
                "Revalidating the exact Step before sending. No automatic retry will occur.".into()
            }
            Err(e) => e.into(),
        });
        if queued {
            window.focus(&self.step_panel.as_ref().unwrap().close, cx);
        }
        self.sync_step_status(cx);
    }
    fn submit_step_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.step_panel else {
            return;
        };
        let text = panel.input.read(cx).value().to_string();
        self.submit_step_review(Choice::Edit(text), window, cx);
    }
    pub(super) fn close_step_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = &self.step_panel else {
            return;
        };
        if panel.input.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            panel
                .input
                .update(cx, |input, cx| input.unmark_text(window, cx));
            self.step_edit_changed(window, cx);
            cx.notify();
            return;
        }
        if panel.input.read(cx).value().len() > STEP_PREVIEW_BYTES {
            self.step_edit_changed(window, cx);
            return;
        }
        self.step_draft = Some((
            panel.instance,
            panel.prompt.clone(),
            panel.input.read(cx).value().to_string(),
        ));
        if let Some(client) = self
            .client
            .as_ref()
            .filter(|c| c.instance() == panel.instance && c.connection() == panel.connection)
        {
            client.cancel_unsent_step();
        }
        self.step_panel = None;
        self.settle(window, cx);
        if self.step_prompt().is_some() {
            window.focus(&self.step_open_focus, cx);
        } else {
            self.composer
                .update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }
    pub(super) fn sync_step_status(&mut self, cx: &mut Context<Self>) {
        let status = self
            .client
            .as_ref()
            .map(Client::step_status)
            .unwrap_or_default();
        if let Some(panel) = self.step_panel.as_mut() {
            let status = match &status {
                Status::Reserved(a) | Status::Sent(a) | Status::Finished { action: a, .. }
                    if Some(a.operation) == panel.operation =>
                {
                    status
                }
                _ => Status::Idle,
            };
            if panel.observed != status {
                panel.observed = status;
                cx.notify();
            }
        }
    }
    pub(super) fn reveal_step_focus(&mut self, window: &mut Window, cx: &mut App) {
        let Some(panel) = self.step_panel.as_mut() else {
            return;
        };
        let focused = window.focused(cx);
        if panel.revealed != focused
            || panel.viewport != Some(window.viewport_size())
            || panel.scale != self.text_scale
        {
            panel.revealed = focused;
            panel.viewport = Some(window.viewport_size());
            panel.scale = self.text_scale;
            panel.frames = 2;
        }
        if panel.frames == 0 {
            return;
        }
        panel.frames -= 1;
        let anchor = if panel.input.focus_handle(cx).contains_focused(window, cx) {
            Some(&panel.input_anchor)
        } else {
            panel
                .controls
                .iter()
                .find(|(focus, _)| focus.contains_focused(window, cx))
                .map(|(_, a)| a)
        };
        if let Some(anchor) = anchor {
            anchor.scroll_to(window, cx);
            window.request_animation_frame();
        }
    }
    fn scroll_step(&mut self, lines: f32, page: bool, cx: &mut Context<Self>) {
        if let Some(panel) = &self.step_panel {
            let amount = if page {
                panel.scroll.bounds().size.height
            } else {
                self.scaled(40.)
            };
            let offset = panel.scroll.offset();
            panel.scroll.set_offset(point(
                offset.x,
                (offset.y + amount * lines).clamp(-panel.scroll.max_offset().y, px(0.)),
            ));
            cx.notify();
        }
    }
    pub(super) fn render_step_review(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = self.step_panel.as_ref().unwrap();
        let viewport = window.viewport_size();
        let margin = px(16.);
        let weak = cx.entity().downgrade();
        let enabled = self.step_review_current(panel)
            && !self.client.as_ref().unwrap().step_status().blocked()
            && !matches!(self.client.as_ref().unwrap().step_status(),Status::Finished{action,outcome:model::Outcome::Accepted} if action.prompt.step_id==panel.prompt.step_id);
        let status=match &panel.observed {Status::Idle=>"No decision has been queued.",Status::Reserved(_)=>"Revalidating this exact prompt; decision not yet written.",Status::Sent(_)=>"Decision may have been written; waiting for its receipt. Do not retry.",Status::Finished{outcome:model::Outcome::Accepted,..}=>"Decision accepted by the daemon. This is not proof of Tool execution or success; normal security approvals still apply.",Status::Finished{outcome:model::Outcome::Rejected(e),..}=>e,Status::Finished{outcome:model::Outcome::Unknown,..}=>"Decision outcome unknown. It may already have applied. Native Step decisions are blocked for this client; inspect Formless TUI. No retry was sent."};
        let mut body=v_flex().id("step-review-body").test_support().flex_1().min_h_0().overflow_y_scroll().track_scroll(&panel.scroll).gap_2()
            .child(readable("step-review-identity",format!("Session {}\nRun {}\nStep {}\nTool {} · call {} of {}",panel.prompt.session_id,panel.prompt.run_id,panel.prompt.step_id,panel.prompt.tool_name,panel.prompt.index+1,panel.prompt.count)))
            .child(readable("step-review-scope","Review the complete original arguments below. Step decisions do not grant security approval. Continue uses the original arguments; Edit and continue uses your JSON object. Step off continues this call and disables Step pauses for this Session until enabled again. No decision is retried after connection loss."))
            .child(readable("step-original-arguments",panel.prompt.arguments_preview.clone()).text_sm())
            .child(div().id("step-copy-anchor").test_support().anchor_scroll(Some(panel.controls[0].1.clone())).child(Button::new("step-copy-original").track_focus(&panel.controls[0].0).label("Copy original").disabled(!self.step_review_current(panel)).on_click(cx.listener(|this,_,_,cx|{if let Some(panel)=&this.step_panel{if this.step_review_current(panel){cx.write_to_clipboard(ClipboardItem::new_string(panel.prompt.arguments_preview.clone()));}}}))))
            .child(div().id("step-copy-edited-anchor").test_support().anchor_scroll(Some(panel.controls[1].1.clone())).child(Button::new("step-copy-edited").track_focus(&panel.controls[1].0).label("Copy edited").on_click(cx.listener(|this,_,_,cx|{if let Some(panel)=&this.step_panel{if this.client.as_ref().is_some_and(|c|c.instance()==panel.instance){cx.write_to_clipboard(ClipboardItem::new_string(panel.input.read(cx).value().to_string()));}}}))))
            .child(readable("step-edit-label","Edited arguments · JSON object · 4 KiB limit"))
            .child(div().id("step-input-anchor").test_support().anchor_scroll(Some(panel.input_anchor.clone())).child(Textarea::new(&panel.input).aria_label("Edited Step arguments").disabled(!enabled).w_full().on_paste(move|item,_,cx|{if item.text().is_some_and(|s|s.len()>STEP_PREVIEW_BYTES){if let Some(shell)=weak.upgrade(){shell.update(cx,|this,cx|{if let Some(panel)=&mut this.step_panel{panel.notice=Some("Paste not added: edited arguments are limited to 4 KiB.".into());cx.notify();}});}true}else{false}})));
        for (index, (id, label, choice)) in [
            ("step-continue", "Continue original", Some(Choice::Continue)),
            ("step-skip", "Skip this call", Some(Choice::Skip)),
            ("step-edit", "Edit and continue", None),
            ("step-all", "Continue; Step off", Some(Choice::All)),
        ]
        .into_iter()
        .enumerate()
        {
            let button = Button::new(id)
                .track_focus(&panel.controls[index + 2].0)
                .label(label)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(choice) = &choice {
                        this.submit_step_review(choice.clone(), window, cx);
                    } else {
                        this.submit_step_edit(window, cx);
                    }
                }));
            body = body.child(
                div()
                    .id(("step-action-anchor", index))
                    .test_support()
                    .anchor_scroll(Some(panel.controls[index + 2].1.clone()))
                    .child(button),
            );
        }
        body = body
            .child(
                announced(div().id("step-review-status"))
                    .test_support()
                    .role(Role::Status)
                    .aria_value(status.to_string())
                    .child(status.to_string()),
            )
            .when_some(panel.notice.clone(), |body, text| {
                body.child(
                    announced(div().id("step-review-notice"))
                        .test_support()
                        .role(Role::Status)
                        .aria_value(text.clone())
                        .child(text),
                )
            });
        if !self.step_review_current(panel) {
            body=body.child(readable("step-review-stale","This is an earlier review. Its connection or pending prompt changed; decision controls are disabled. Edits are kept for this window when closing."));
        }
        let dialog = v_flex()
            .id("step-review-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label("Review Step decision")
            .map(modal_dialog)
            .key_context("FormlessStepReview")
            .on_action(cx.listener(|this, _: &CloseStepReview, window, cx| {
                this.close_step_review(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ScrollViewerUp, _, cx| this.scroll_step(1., false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ScrollViewerDown, _, cx| this.scroll_step(-1., false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ScrollViewerPageUp, _, cx| this.scroll_step(1., true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ScrollViewerPageDown, _, cx| {
                    this.scroll_step(-1., true, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ScrollViewerStart, _, cx| {
                if let Some(panel) = &this.step_panel {
                    panel.scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerEnd, _, cx| {
                if let Some(panel) = &this.step_panel {
                    panel.scroll.scroll_to_bottom();
                    cx.notify();
                }
            }))
            .w(self.scaled(680.).min(viewport.width - margin * 2.))
            .max_h(viewport.height - margin * 2.)
            .p_3()
            .gap_2()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .child(
                readable("step-review-heading", "Review Step decision")
                    .text_lg()
                    .flex_shrink_0(),
            )
            .child(body)
            .child(
                Button::new("step-review-close")
                    .track_focus(&panel.close)
                    .label("Close · keep edits")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.close_step_review(window, cx)),
                    ),
            )
            .focus_trap("step-review-dialog", &panel.focus);
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
                    .pt(margin)
                    .child(dialog),
            )
    }
    #[cfg(feature = "test-support")]
    pub fn record_step_write(&self, dispatch: &model::Dispatch) -> bool {
        self.client.as_ref().unwrap().record_step_write(dispatch)
    }
    #[cfg(feature = "test-support")]
    pub fn record_step_outcome(
        &mut self,
        dispatch: &model::Dispatch,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        self.client
            .as_ref()
            .unwrap()
            .record_step_outcome(dispatch, value, true);
        self.sync_step_status(cx);
    }
    #[cfg(feature = "test-support")]
    pub fn step_edit_input(&self) -> Option<Entity<TextareaState>> {
        self.step_panel.as_ref().map(|p| p.input.clone())
    }
}
