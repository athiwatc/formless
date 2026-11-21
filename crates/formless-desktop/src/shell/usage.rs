use super::*;
use crate::session_usage::{Report, Target};

impl Shell {
    fn usage_target(&self) -> Option<Target> {
        let client = self.client.as_ref()?;
        (self.client_connected && client.is_ready() && self.client_snapshot_ready)
            .then(|| Target {
                session_id: self.sidebar.active_id().unwrap_or_default().into(),
                connection: client.connection(),
                generation: self.client_generation,
            })
            .filter(|target| !target.session_id.is_empty())
    }
    fn read_session_usage(
        &mut self,
        target: Target,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.usage_target().as_ref() != Some(&target)
            || self.session_usage.revision != revision
            || self.session_usage.pending()
        {
            return;
        }
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            self.session_usage.notice = "Finish composing before reading usage. Draft kept.".into();
            cx.notify();
            return;
        }
        let operation = uuid::Uuid::new_v4();
        self.session_usage.start(target.clone(), operation);
        if let Err(error) = self
            .client
            .as_ref()
            .unwrap()
            .send(ClientCommand::ReadSessionUsage {
                target: target.clone(),
                operation,
            })
        {
            self.session_usage
                .receive(&target, operation, Err(error.into()));
        }
        self.usage_frames = 2;
        cx.notify();
    }
    pub(super) fn receive_session_usage(
        &mut self,
        target: Target,
        operation: uuid::Uuid,
        result: Result<Report, String>,
        cx: &mut Context<Self>,
    ) {
        if self.usage_target().as_ref() == Some(&target)
            && self.session_usage.receive(&target, operation, result)
        {
            self.usage_frames = 2;
            cx.notify();
        }
    }
    fn copy_session_usage(&self, target: &Target, revision: uuid::Uuid, cx: &mut Context<Self>) {
        if self.usage_target().as_ref() == Some(target)
            && self.session_usage.target.as_ref() == Some(target)
            && self.session_usage.revision == revision
        {
            if let Some(report) = &self.session_usage.report {
                cx.write_to_clipboard(ClipboardItem::new_string(report.copy()));
            }
        }
    }
    #[cfg(feature = "test-support")]
    pub fn read_presented_session_usage(
        &mut self,
        target: Target,
        revision: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.read_session_usage(target, revision, window, cx);
    }
    #[cfg(feature = "test-support")]
    pub fn copy_presented_session_usage(
        &self,
        target: &Target,
        revision: uuid::Uuid,
        cx: &mut Context<Self>,
    ) {
        self.copy_session_usage(target, revision, cx);
    }
    #[cfg(feature = "test-support")]
    pub fn session_usage_revision(&self) -> uuid::Uuid {
        self.session_usage.revision
    }

    pub(super) fn reveal_session_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.usage_target();
        if self.session_usage.target.is_some() && self.session_usage.target != current {
            self.session_usage.retire(
                "Session, snapshot or connection changed. Read usage again; nothing was replayed.",
            );
        }
        let focused = window.focused(cx);
        let basis = (focused.clone(), window.viewport_size(), self.text_scale);
        if self.usage_focus.as_ref() != Some(&basis) {
            self.usage_focus = Some(basis);
            self.usage_frames = 2;
        }
        if self.usage_frames > 0 {
            self.usage_frames -= 1;
            for (focus, anchor) in &self.usage_controls {
                if focus.contains_focused(window, cx) {
                    anchor.scroll_to(window, cx);
                    window.request_animation_frame();
                    break;
                }
            }
        }
    }

    pub(super) fn render_session_usage(&self, cx: &mut Context<Self>) -> AnyElement {
        let target = self.usage_target();
        let revision = self.session_usage.revision;
        let read_target = target.clone();
        let copy_target = target.clone();
        let can_copy = self.session_usage.report.is_some()
            && self.session_usage.target == target
            && target.is_some();
        v_flex()
            .gap_2()
            .child(readable("session-usage-heading", "Recorded Session usage").text_sm())
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .id("session-usage-read-anchor")
                            .pt_1()
                            .anchor_scroll(Some(self.usage_controls[0].1.clone()))
                            .child(
                                Button::new("session-usage-read")
                                    .label("Read usage")
                                    .accessibility_label("Read recorded usage for the open Session")
                                    .track_focus(&self.usage_controls[0].0)
                                    .disabled(target.is_none() || self.session_usage.pending())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(target) = &read_target {
                                            this.read_session_usage(
                                                target.clone(),
                                                revision,
                                                window,
                                                cx,
                                            );
                                        }
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id("session-usage-copy-anchor")
                            .pt_1()
                            .anchor_scroll(Some(self.usage_controls[1].1.clone()))
                            .child(
                                Button::new("session-usage-copy")
                                    .label("Copy usage")
                                    .accessibility_label(
                                        "Copy displayed recorded Session usage as JSON",
                                    )
                                    .track_focus(&self.usage_controls[1].0)
                                    .disabled(!can_copy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(target) = &copy_target {
                                            this.copy_session_usage(target, revision, cx);
                                        }
                                    })),
                            ),
                    ),
            )
            .child(readable(
                "session-usage-notice",
                self.session_usage.notice.clone(),
            ))
            .when_some(self.session_usage.target.as_ref(), |view, target| {
                view.child(readable(
                    "session-usage-id",
                    format!("Recorded read for Session: {}", target.session_id),
                ))
            })
            .when_some(self.session_usage.report.as_ref(), |view, report| {
                view.child(readable("session-usage-values", report.text()))
            })
            .into_any_element()
    }
}
