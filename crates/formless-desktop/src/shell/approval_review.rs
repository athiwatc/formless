use super::*;
use crate::approvals::{Action, Inventory, Prompt, Status};

pub(super) struct Panel {
    instance: uuid::Uuid,
    connection: u64,
    revision: uuid::Uuid,
    minimum_revision: u64,
    reading: bool,
    submitted: bool,
    retired: bool,
    prompt: Option<Prompt>,
    inventory: Inventory,
    selected: usize,
    notice: String,
    observed: Status,
    details: String,
    starts: Box<[u32]>,
    layout: Option<ViewerLayout>,
    error: Option<full_view::RowError>,
    pub close: FocusHandle,
    focus: FocusHandle,
    scroll: ScrollHandle,
    rows: gpui_kit::UniformListScrollHandle,
    rows_focus: FocusHandle,
    rows_anchor: gpui_kit::ScrollAnchor,
    controls: Vec<(FocusHandle, gpui_kit::ScrollAnchor)>,
    revealed: Option<FocusHandle>,
    viewport: Option<Size<Pixels>>,
    frames: u8,
}
impl Shell {
    fn approval_owner_current(&self, p: &Panel) -> bool {
        self.client_connected
            && self.client.as_ref().is_some_and(|c| {
                c.is_ready() && c.instance() == p.instance && c.connection() == p.connection
            })
    }
    fn approval_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.palette_composing(window, cx)
            || self
                .composer
                .update(cx, |i, cx| i.marked_text_range(window, cx).is_some())
            || self
                .browse_query(self.destination)
                .is_some_and(|q| q.update(cx, |i, cx| i.marked_text_range(window, cx).is_some()))
    }
    pub(super) fn open_approvals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .current_modal(self.layout(window))
            .is_some_and(|m| m != Modal::Palette)
        {
            return;
        }
        if self.approval_composing(window, cx) {
            return;
        }
        let Some(c) = self
            .client
            .as_ref()
            .filter(|c| self.client_connected && c.is_ready())
        else {
            return;
        };
        let (instance, connection) = (c.instance(), c.connection());
        self.invalidate_forecast(cx);
        self.palette = None;
        self.viewer = None;
        self.prefs.close_overlays();
        let scroll = ScrollHandle::new();
        let revision = uuid::Uuid::new_v4();
        self.approval_panel = Some(Panel {
            instance,
            connection,
            revision,
            minimum_revision: 0,
            reading: false,
            submitted: false,
            retired: true,
            prompt: None,
            inventory: Inventory::default(),
            selected: 0,
            notice: String::new(),
            observed: Status::Idle,
            details: String::new(),
            starts: Box::default(),
            layout: None,
            error: None,
            close: cx.focus_handle(),
            focus: cx.focus_handle(),
            rows: gpui_kit::UniformListScrollHandle::new(),
            rows_focus: cx.focus_handle().tab_stop(true),
            rows_anchor: gpui_kit::ScrollAnchor::for_handle(scroll.clone()),
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
            frames: 2,
        });
        self.settle(window, cx);
        self.read_approvals(revision, window, cx);
    }
    fn read_approvals(
        &mut self,
        revision: uuid::Uuid,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(c) = self
            .client
            .as_ref()
            .filter(|c| self.client_connected && c.is_ready())
        else {
            return;
        };
        let Some(p) = &mut self.approval_panel else {
            return;
        };
        if p.revision != revision
            || p.instance != c.instance()
            || (p.reading && p.connection == c.connection())
        {
            return;
        }
        if p.connection != c.connection() {
            p.minimum_revision = 0;
        }
        p.connection = c.connection();
        p.revision = uuid::Uuid::new_v4();
        p.reading = true;
        p.submitted = false;
        p.retired = true;
        p.prompt = None;
        p.inventory = Inventory::default();
        p.selected = 0;
        p.details.clear();
        p.starts = Box::default();
        p.rows = gpui_kit::UniformListScrollHandle::new();
        p.layout = None;
        p.error = None;
        p.notice = "Reading current Profile Tool security approvals…".into();
        if let Err(e) = c.send(ClientCommand::ReadApprovals {
            connection: p.connection,
            operation: p.revision,
        }) {
            p.reading = false;
            p.notice = e.into();
        }
        cx.notify();
    }
    pub(super) fn approval_inventory(
        &mut self,
        instance: uuid::Uuid,
        connection: u64,
        operation: uuid::Uuid,
        revision: u64,
        result: Result<Inventory, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &self.approval_panel else {
            return;
        };
        if !self.approval_owner_current(p)
            || p.instance != instance
            || p.connection != connection
            || p.revision != operation
            || !p.reading
        {
            return;
        }
        let p = self.approval_panel.as_mut().unwrap();
        p.reading = false;
        if revision < p.minimum_revision {
            p.notice = "Approvals changed during this read. Reload before reviewing.".into();
            cx.notify();
            return;
        }
        p.minimum_revision = revision;
        match result {
            Ok(inventory) if !inventory.is_empty()=>{let prompt=inventory.prompts()[0].clone();p.details=prompt.display();p.prompt=Some(prompt);p.inventory=inventory;p.selected=0;p.retired=false;p.layout=None;p.notice="Review the complete requested authority below. No decision has been sent.".into();},
            Ok(_)=>p.notice="No pending Tool security approval in this read. Other decisions may still require Formless TUI.".into(),
            Err(e)=>p.notice=crate::questions::preview(&e,500),
        }
        p.frames = 2;
        cx.notify();
    }
    pub(super) fn approval_invalidated(
        &mut self,
        instance: uuid::Uuid,
        connection: u64,
        revision: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = &mut self.approval_panel else {
            return;
        };
        if p.instance != instance || p.connection != connection {
            return;
        }
        p.minimum_revision = p.minimum_revision.max(revision);
        p.retired = true;
        if !p.submitted {
            p.notice="Approval inventory changed. Reload before deciding; earlier evidence is read-only.".into();
        }
        cx.notify();
    }
    fn select_approval(
        &mut self,
        revision: uuid::Uuid,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Some(Modal::Approvals) || self.approval_composing(window, cx) {
            return;
        }
        let Some(p) = &self.approval_panel else {
            return;
        };
        if p.revision != revision
            || !self.approval_owner_current(p)
            || p.reading
            || p.submitted
            || p.retired
            || index == p.selected
            || self.client.as_ref().unwrap().approval_status().blocked()
        {
            return;
        }
        let Some(prompt) = p.inventory.prompts().get(index).cloned() else {
            return;
        };
        let p = self.approval_panel.as_mut().unwrap();
        p.revision = uuid::Uuid::new_v4();
        p.selected = index;
        p.scroll.set_offset(point(px(0.), px(0.)));
        p.details = prompt.display();
        p.prompt = Some(prompt);
        p.layout = None;
        p.error = None;
        p.starts = Box::default();
        p.rows = gpui_kit::UniformListScrollHandle::new();
        p.notice="Review this selected request's complete authority. Selection grants nothing; Allow once or Deny is a separate explicit action.".into();
        p.frames = 2;
        p.close.focus(window, cx);
        cx.notify();
    }
    pub fn submit_approval(
        &mut self,
        revision: uuid::Uuid,
        prompt: Prompt,
        allow: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Some(Modal::Approvals) || self.approval_composing(window, cx) {
            return;
        }
        let Some(p) = &self.approval_panel else {
            return;
        };
        if p.revision != revision
            || !self.approval_owner_current(p)
            || p.reading
            || p.submitted
            || p.retired
            || p.layout.is_none()
            || p.error.is_some()
            || p.prompt.as_ref() != Some(&prompt)
            || self.client.as_ref().unwrap().approval_status().blocked()
        {
            return;
        }
        self.invalidate_forecast(cx);
        let c = self.client.as_ref().unwrap();
        let action = Action {
            client: c.instance(),
            connection: c.connection(),
            epoch: c.epoch(),
            operation: revision,
            prompt,
            allow,
        };
        let result = c.decide_approval(action);
        let p = self.approval_panel.as_mut().unwrap();
        p.submitted = true;
        if let Err(e) = result {
            p.notice = e.into();
        }
        self.sync_approval_status(cx);
        cx.notify();
    }
    fn copy_approval(&self, revision: uuid::Uuid, cx: &mut Context<Self>) {
        if let Some(p) = &self.approval_panel {
            if p.revision == revision
                && self
                    .client
                    .as_ref()
                    .is_some_and(|c| c.instance() == p.instance)
            {
                if let Some(prompt) = &p.prompt {
                    cx.write_to_clipboard(ClipboardItem::new_string(prompt.display()));
                }
            }
        }
    }
    fn close_approval_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.approval_panel.is_none() {
            return;
        }
        if let Some(c) = &self.client {
            c.cancel_unsent_approval();
        }
        self.approval_panel = None;
        self.settle(window, cx);
        cx.notify();
    }
    pub(super) fn sync_approval_status(&mut self, cx: &mut Context<Self>) {
        let Some(p) = &self.approval_panel else {
            return;
        };
        let current = self.approval_owner_current(p);
        let status = self
            .client
            .as_ref()
            .map(Client::approval_status)
            .unwrap_or_default();
        let p = self.approval_panel.as_mut().unwrap();
        if !current {
            let notice =
                "Connection changed. Reload explicitly after reconnecting; nothing is replayed.";
            if p.reading || !p.retired || p.notice != notice {
                p.reading = false;
                p.retired = true;
                p.notice = notice.into();
                cx.notify();
            }
        }
        if p.observed != status {
            p.observed = status;
            cx.notify();
        }
    }
    pub(super) fn reveal_approval_focus(&mut self, window: &mut Window, cx: &mut App) {
        let width = self
            .scaled(680.)
            .min(window.viewport_size().width - px(32.));
        let mut layout = self.viewer_layout(window, cx);
        layout.width = (width - px(48.)).max(px(1.));
        let Some(p) = &mut self.approval_panel else {
            return;
        };
        if p.layout.as_ref() != Some(&layout) {
            let (starts, error) = Self::measure_viewer(&p.details, &layout, cx);
            p.starts = starts;
            p.error = error;
            p.layout = Some(layout);
            p.frames = 2;
        }
        let focused = window.focused(cx);
        if p.revealed != focused || p.viewport != Some(window.viewport_size()) {
            p.revealed = focused;
            p.viewport = Some(window.viewport_size());
            p.frames = 2;
        }
        if p.frames == 0 {
            return;
        }
        p.frames -= 1;
        let anchor = if p.rows_focus.contains_focused(window, cx) {
            Some(&p.rows_anchor)
        } else {
            p.controls
                .iter()
                .find(|(f, _)| f.contains_focused(window, cx))
                .map(|(_, a)| a)
        };
        if let Some(a) = anchor {
            a.scroll_to(window, cx);
            window.request_animation_frame();
        }
    }
    fn scroll_approval(
        &mut self,
        to: WindowMove,
        rows: isize,
        cx: &mut Context<Self>,
        window: &Window,
    ) {
        let Some(p) = &self.approval_panel else {
            return;
        };
        if p.rows_focus.is_focused(window) && !p.starts.is_empty() {
            let state = p.rows.0.borrow();
            let (top, page) = state
                .last_item_size
                .and_then(|size| {
                    let height = size.contents.height / p.starts.len().max(1) as f32;
                    (height > px(0.)).then(|| {
                        (
                            (-state.base_handle.offset().y / height).floor().max(0.) as usize,
                            (size.item.height / height).floor().max(1.) as usize,
                        )
                    })
                })
                .unwrap_or((0, 1));
            let top = state.deferred_scroll_to_item.map_or(top, |d| d.item_index);
            drop(state);
            let step = if rows == 0 {
                page.saturating_sub(1).max(1)
            } else {
                rows.unsigned_abs()
            };
            let last = p.starts.len() - 1;
            let target = match to {
                WindowMove::Start => 0,
                WindowMove::End => last,
                WindowMove::Earlier => top.saturating_sub(step),
                WindowMove::Later => (top + step).min(last),
            };
            p.rows.scroll_to_item_strict(target, ScrollStrategy::Top);
        } else {
            let amount = if rows == 0 {
                p.scroll.bounds().size.height
            } else {
                self.scaled(40.) * rows.unsigned_abs() as f32
            };
            let max = p.scroll.max_offset().y;
            let y = match to {
                WindowMove::Start => px(0.),
                WindowMove::End => -max,
                WindowMove::Earlier => (p.scroll.offset().y + amount).min(px(0.)),
                WindowMove::Later => (p.scroll.offset().y - amount).max(-max),
            };
            p.scroll.set_offset(point(px(0.), y));
        }
        cx.notify();
    }
    pub(super) fn render_approval_review(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = self.approval_panel.as_ref().unwrap();
        let revision = p.revision;
        let viewport = window.viewport_size();
        let enabled = self.approval_owner_current(p)
            && !p.reading
            && !p.submitted
            && !p.retired
            && p.layout.is_some()
            && p.error.is_none()
            && p.prompt.is_some()
            && !self.client.as_ref().unwrap().approval_status().blocked();
        let rows = gpui_kit::uniform_list(
            "approval-evidence-rows",
            p.starts.len(),
            cx.processor(|this, range: Range<usize>, _, _| {
                let Some(p) = &this.approval_panel else {
                    return Vec::new();
                };
                range
                    .filter(|i| *i < p.starts.len())
                    .map(|i| {
                        let text = SharedString::from(
                            full_view::row(&p.details, &p.starts, i).to_string(),
                        );
                        div()
                            .id(("approval-evidence-row", i))
                            .test_support()
                            .role(Role::Label)
                            .aria_value(text.clone())
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(if text.is_empty() { " ".into() } else { text })
                    })
                    .collect()
            }),
        )
        .track_scroll(&p.rows)
        .flex_1()
        .min_h_0()
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(cx.theme().mono_font_size);
        let mut body=v_flex().id("approval-review-body").test_support().flex_1().min_h_0().overflow_y_scroll().track_scroll(&p.scroll).gap_2()
            .child(readable("approval-review-scope","Profile-wide Tool call review. Allow once grants only this call's exact requested authority. Deny is not remembered. No persistent grant or path widening."))
            .child(readable("approval-review-limits","Arguments are daemon-redacted evidence; code_preview is not full code. Review one selected request at a time. Wider scopes, batches, unsupported/mixed inventories and other decision types require Formless TUI."))
            .child(readable("approval-review-notice",p.notice.clone()));
        if p.inventory.prompts().len() > 1 {
            let prompt = p.prompt.as_ref().unwrap();
            body = body.child(readable(
                "approval-selection",
                format!(
                    "Request {} of {} · Session {} · Tool {} version {} · approval {}",
                    p.selected + 1,
                    p.inventory.prompts().len(),
                    prompt.session(),
                    prompt.tool(),
                    prompt.version(),
                    prompt.id()
                ),
            ));
            let mut navigation = h_flex().flex_wrap().gap_2();
            for (slot, id, label, index, edge) in [
                (
                    4,
                    "approval-previous",
                    "Previous request",
                    p.selected.saturating_sub(1),
                    p.selected == 0,
                ),
                (
                    5,
                    "approval-next",
                    "Next request",
                    p.selected + 1,
                    p.selected + 1 >= p.inventory.prompts().len(),
                ),
            ] {
                navigation = navigation.child(
                    div()
                        .id(SharedString::from(format!("{id}-anchor")))
                        .anchor_scroll(Some(p.controls[slot].1.clone()))
                        .child(
                            Button::new(id)
                                .label(label)
                                .track_focus(&p.controls[slot].0)
                                .disabled(
                                    edge || !self.approval_owner_current(p)
                                        || p.reading
                                        || p.submitted
                                        || p.retired
                                        || self
                                            .client
                                            .as_ref()
                                            .unwrap()
                                            .approval_status()
                                            .blocked(),
                                )
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_approval(revision, index, window, cx)
                                })),
                        ),
                );
            }
            body = body.child(navigation);
        }
        if let Some(notice) = p.observed.notice() {
            let target = p
                .observed
                .action()
                .map(|a| {
                    format!(
                        "{} decision for approval {} in Session {}. ",
                        if a.allow { "Allow once" } else { "Deny" },
                        a.prompt.id(),
                        a.prompt.session()
                    )
                })
                .unwrap_or_default();
            body = body.child(
                announced(div().id("approval-receipt"))
                    .test_support()
                    .role(Role::Status)
                    .aria_value(format!("{target}{notice}"))
                    .child(format!("{target}{notice}")),
            );
        }
        if let Some(error) = p.error {
            body = body.child(readable("approval-evidence-error", error.notice()));
        }
        body = body.child(
            div()
                .id("approval-evidence")
                .test_support()
                .role(Role::Group)
                .aria_label("Complete approval JSON; use arrow and page keys to read")
                .track_focus(&p.rows_focus)
                .anchor_scroll(Some(p.rows_anchor.clone()))
                .h(self.scaled(160.).min(viewport.height * 0.4))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .border_1()
                .border_color(if p.rows_focus.is_focused(window) {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .child(rows),
        );
        for (i, (id, label)) in [
            ("approval-reload", "Reload approvals"),
            ("approval-copy", "Copy reviewed JSON"),
        ]
        .into_iter()
        .enumerate()
        {
            body = body.child(
                div()
                    .id(SharedString::from(format!("{id}-anchor")))
                    .anchor_scroll(Some(p.controls[i].1.clone()))
                    .child(
                        Button::new(id)
                            .label(label)
                            .track_focus(&p.controls[i].0)
                            .disabled(if i == 0 {
                                p.reading
                            } else {
                                p.prompt.is_none()
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if i == 0 {
                                    this.read_approvals(revision, window, cx)
                                } else {
                                    this.copy_approval(revision, cx)
                                }
                            })),
                    ),
            );
        }
        for (i, (id, label, allow)) in [
            ("approval-deny", "Deny this call", false),
            ("approval-allow-once", "Allow this call once", true),
        ]
        .into_iter()
        .enumerate()
        {
            let prompt = p.prompt.clone();
            body = body.child(
                div()
                    .id(SharedString::from(format!("{id}-anchor")))
                    .anchor_scroll(Some(p.controls[i + 2].1.clone()))
                    .child(
                        Button::new(id)
                            .label(label)
                            .track_focus(&p.controls[i + 2].0)
                            .disabled(!enabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(prompt) = &prompt {
                                    this.submit_approval(
                                        revision,
                                        prompt.clone(),
                                        allow,
                                        window,
                                        cx,
                                    )
                                }
                            })),
                    ),
            );
        }
        let dialog = v_flex()
            .id("approval-review-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label("Review security approval")
            .map(modal_dialog)
            .key_context("FormlessApprovalReview")
            .on_action(cx.listener(|this, _: &CloseApprovalReview, window, cx| {
                this.close_approval_review(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerUp, window, cx| {
                this.scroll_approval(WindowMove::Earlier, 1, cx, window)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerDown, window, cx| {
                this.scroll_approval(WindowMove::Later, 1, cx, window)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerPageUp, window, cx| {
                this.scroll_approval(WindowMove::Earlier, 0, cx, window)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerPageDown, window, cx| {
                this.scroll_approval(WindowMove::Later, 0, cx, window)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerStart, window, cx| {
                this.scroll_approval(WindowMove::Start, 0, cx, window)
            }))
            .on_action(cx.listener(|this, _: &ScrollViewerEnd, window, cx| {
                this.scroll_approval(WindowMove::End, 0, cx, window)
            }))
            .w(self.scaled(680.).min(viewport.width - px(32.)))
            .max_h(viewport.height - px(32.))
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
                readable("approval-review-heading", "Review security approval")
                    .text_lg()
                    .flex_shrink_0(),
            )
            .child(body)
            .child(
                Button::new("approval-review-close")
                    .track_focus(&p.close)
                    .label("Close without another decision")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.close_approval_review(window, cx)),
                    ),
            )
            .focus_trap("approval-review-dialog", &p.focus);
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
                    .pt(px(16.))
                    .child(dialog),
            )
    }
    #[cfg(feature = "test-support")]
    pub fn select_presented_approval(
        &mut self,
        revision: uuid::Uuid,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_approval(revision, index, window, cx);
    }
    #[cfg(feature = "test-support")]
    pub fn approval_target(&self) -> Option<(uuid::Uuid, Prompt)> {
        let p = self.approval_panel.as_ref()?;
        Some((p.revision, p.prompt.clone()?))
    }
}

#[cfg(feature = "test-support")]
impl Shell {
    pub fn approval_review_diagnostic(&self) -> String {
        self.approval_panel
            .as_ref()
            .map(|p| {
                format!(
                    "reading={} retired={} owner={} notice={}",
                    p.reading,
                    p.retired,
                    self.approval_owner_current(p),
                    p.notice
                )
            })
            .unwrap_or_else(|| "no panel".into())
    }
    pub fn approval_review_status(&self) -> Status {
        self.client
            .as_ref()
            .map(Client::approval_status)
            .unwrap_or_default()
    }
    pub fn finish_recorded_approval(
        &mut self,
        d: &crate::approvals::Dispatch,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        self.client
            .as_ref()
            .unwrap()
            .finish_recorded_approval(d, value);
        self.sync_approval_status(cx);
    }
}
