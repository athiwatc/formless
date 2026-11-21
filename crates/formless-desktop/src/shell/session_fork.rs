use super::*;
use crate::session_fork::{Action, Outcome, Status};
const FORK_CONTEXT: &str = "FormlessForkReview";
#[derive(Clone)]
pub(super) struct Review {
    ticket: uuid::Uuid,
    connection: u64,
    source: String,
    name: String,
    submitted: Option<Action>,
}
impl Shell {
    pub(super) fn fork_source(&self) -> Option<(String, String)> {
        let id = match self.destination {
            Destination::Sessions => self.directory.chosen_id(&self.directory_rows)?,
            Destination::Chat => self.sidebar.active_id()?,
            _ => return None,
        };
        let mut sources = self
            .client_sessions
            .iter()
            .filter(|session| session.id == id);
        let source = sources.next()?;
        if sources.next().is_some()
            || id.is_empty()
            || id.len() > 256
            || id.chars().any(char::is_control)
        {
            return None;
        }
        let mut name: String = source.name.chars().take(128).collect();
        if source.name.chars().nth(128).is_some() {
            name.push('…');
        }
        Some((id.into(), name))
    }
    pub(super) fn fork_disabled_reason(&self) -> Option<&'static str> {
        let Some(client) = &self.client else {
            return Some("Connect to a Profile first");
        };
        if !self.client_connected || !client.is_ready() {
            return Some("Connect to a Profile first");
        }
        if let Some(reason) = client.fork_status().blocked_reason() {
            return Some(reason);
        }
        if self.fork_source().is_none() {
            return Some("Choose a Session in Chat or Sessions first");
        }
        None
    }
    pub(super) fn sync_fork_status(&mut self, cx: &mut Context<Self>) {
        let status = self
            .client
            .as_ref()
            .map_or(Status::Idle, |client| client.fork_status());
        if self.fork_observed != status {
            self.fork_observed = status;
            cx.notify();
        }
    }
    pub(super) fn review_fork(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.fork_disabled_reason().is_some() || self.background_inert(window) {
            return;
        }
        if self.destination == Destination::Sessions
            && self.finish_directory_composition(window, cx)
        {
            return;
        }
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            return;
        }
        let Some((source, name)) = self.fork_source() else {
            return;
        };
        let connection = self.client.as_ref().unwrap().connection();
        self.set_destination(Destination::ForkReview, window, cx);
        self.focus_region(Region::Conversation, window, cx);
        self.fork_local_error = None;
        self.fork_review = Some(Review {
            ticket: uuid::Uuid::new_v4(),
            connection,
            source,
            name,
            submitted: None,
        });
        self.fork_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }
    pub(super) fn confirm_fork(
        &mut self,
        ticket: uuid::Uuid,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = &self.fork_review else {
            return;
        };
        let Some(client) = &self.client else {
            return;
        };
        if self.destination != Destination::ForkReview
            || review.ticket != ticket
            || review.submitted.is_some()
            || self.background_inert(window)
            || !self.client_connected
            || !client.is_ready()
            || client.connection() != review.connection
            || client.fork_status().blocked_reason().is_some()
            || self
                .client_sessions
                .iter()
                .filter(|session| session.id == review.source)
                .count()
                != 1
        {
            return;
        }
        let action = Action {
            connection: review.connection,
            operation: uuid::Uuid::new_v4(),
            source_id: review.source.clone(),
        };
        let review = self.fork_review.as_mut().unwrap();
        review.ticket = uuid::Uuid::new_v4();
        review.submitted = Some(action.clone());
        if let Err(error) = client.send(ClientCommand::ForkSession(action.clone())) {
            // A failed enqueue cannot have written the request. Keep the review
            // spent and require a new explicit review, never reuse its button.
            self.fork_local_error = Some(error.into());
        }
        self.sync_fork_status(cx);
        cx.notify();
    }
    fn fork_receipt_text(&self) -> Option<String> {
        match &self.fork_observed {
            Status::Idle=>None,
            Status::Reserved(action)=>Some(format!("Fork requested for {}. Awaiting dispatch; leaving this page does not cancel it.",action.source_id)),
            Status::Sent(action)=>Some(format!("Fork submitted for {}. Awaiting a verified receipt; do not submit another. Reconnect never retries it.",action.source_id)),
            Status::Finished{action,outcome:Outcome::Applied(receipt)}=>Some(format!("Fork created. Source: {} · new Session: {} · copied through message sequence {}. Refresh Sessions and choose it explicitly; no Run was started.",action.source_id,receipt.child_id,receipt.fork_point_seq)),
            Status::Finished{action,outcome:Outcome::Rejected(reason)}=>Some(format!("Fork was not sent for {}: {reason}",action.source_id)),
            Status::Finished{action,outcome:Outcome::Unknown(reason)}=>Some(format!("Fork outcome UNKNOWN for {}: {reason} Native Fork is blocked. Check Sessions/TUI; the child may already exist. Nothing is retried.",action.source_id)),
        }
    }
    pub(super) fn render_fork_status(&self) -> Option<String> {
        self.fork_receipt_text()
    }
    pub(super) fn reveal_fork_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.destination != Destination::ForkReview || self.background_inert(window) {
            return;
        }
        let basis = (window.focused(cx), window.viewport_size(), self.text_scale);
        if self.fork_focus.as_ref() != Some(&basis) {
            self.fork_focus = Some(basis);
            self.fork_frames = 2;
        }
        if self.fork_frames > 0 {
            self.fork_frames -= 1;
            if self
                .fork_controls
                .iter()
                .any(|focus| focus.contains_focused(window, cx))
            {
                self.fork_scroll.scroll_to_item(4);
                window.request_animation_frame();
            }
        }
    }
    fn scroll_fork(&mut self, to: WindowMove, page: bool, cx: &mut Context<Self>) {
        let step = if page {
            (self.fork_scroll.bounds().size.height - self.scaled(32.)).max(self.scaled(32.))
        } else {
            self.scaled(32.)
        };
        let offset = self.fork_scroll.offset();
        let end = -self.fork_scroll.max_offset().y;
        let y = match to {
            WindowMove::Earlier => (offset.y + step).min(px(0.)),
            WindowMove::Later => (offset.y - step).max(end),
            WindowMove::Start => px(0.),
            WindowMove::End => end,
        };
        self.fork_scroll.set_offset(point(offset.x, y));
        cx.notify();
    }
    pub(super) fn render_session_fork(
        &self,
        page_height: Option<Pixels>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let review = self.fork_review.as_ref();
        let submitted = review.is_some_and(|review| review.submitted.is_some());
        let can_confirm = review.is_some_and(|review| {
            !submitted
                && self.client_connected
                && self.client.as_ref().is_some_and(|client| {
                    client.is_ready()
                        && client.connection() == review.connection
                        && client.fork_status().blocked_reason().is_none()
                })
                && self
                    .client_sessions
                    .iter()
                    .filter(|session| session.id == review.source)
                    .count()
                    == 1
        });
        let ticket = review.map(|review| review.ticket);
        let source = review.map_or(
            "No source selected. Return to Sessions and review a fork.".into(),
            |review| {
                format!(
                    "Source Session: {}\nName (preview): {}",
                    review.source, review.name
                )
            },
        );
        let receipt = if submitted {
            self.fork_receipt_text()
        } else {
            None
        };
        let notice=self.fork_local_error.as_ref().map(|error|format!("Fork was not queued: {error}. Return to Sessions and review again."))
            .or(receipt).unwrap_or_else(||if can_confirm {"Nothing has been sent. Review the source and scope, then Create fork once.".into()}else{"Source or connection changed, or another fork is unresolved. Nothing is sent from this review; return to Sessions and check its status before reviewing again.".into()});
        v_flex().id("region-conversation").test_support().role(Role::Main).aria_label("Review Session fork").track_focus(&self.conversation_focus)
            .key_context(FORK_CONTEXT).map(|view|inert_while(view,self.background_inert(window)))
            .map(|view|Self::main_size(view,page_height)).when_some(page_height,|view,height|view.h(height))
            .on_action(cx.listener(|this,_:&ScrollViewerUp,_,cx|this.scroll_fork(WindowMove::Earlier,false,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerDown,_,cx|this.scroll_fork(WindowMove::Later,false,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerPageUp,_,cx|this.scroll_fork(WindowMove::Earlier,true,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerPageDown,_,cx|this.scroll_fork(WindowMove::Later,true,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerStart,_,cx|this.scroll_fork(WindowMove::Start,false,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerEnd,_,cx|this.scroll_fork(WindowMove::End,false,cx)))
            .child(v_flex().id("fork-body").size_full().overflow_y_scroll().track_scroll(&self.fork_scroll).p_3().gap_3()
                .child(readable("fork-heading",if submitted {"Session fork receipt"}else{"Review a new Session fork"}))
                .child(readable("fork-source",source))
                .child(readable("fork-scope","Create one new Session from ALL history persisted when the daemon executes the request. That includes private conversation/Tool data and the daemon's applicable plan/checkpoints, model, working directory and standing instructions. Unsent drafts and unpublished live output are excluded. The daemon assigns a fork name; rename it later in Details. Your open Session and its unsent draft stay unchanged. No model call or Run starts. Working-tree files are not duplicated. Once submitted, leaving this page does not cancel the request. A lost or malformed receipt blocks further native forks; check Sessions/TUI before proceeding. This is an in-memory client-lifetime guard, not durable server deduplication."))
                .child(announced(div().id("fork-notice")).test_support().role(Role::Status).aria_value(notice.clone()).child(notice))
                .child(h_flex().flex_wrap().gap_2()
                    .child(Button::new("fork-confirm").label("Create fork").track_focus(&self.fork_controls[0]).disabled(!can_confirm).on_click(cx.listener(move|this,_,window,cx|{if let Some(ticket)=ticket {this.confirm_fork(ticket,window,cx);}})))
                    .child(Button::new("fork-back").label(if submitted {"Back to Sessions"}else{"Cancel"}).track_focus(&self.fork_controls[1]).on_click(cx.listener(|this,_,window,cx|this.set_destination(Destination::Sessions,window,cx))))))
            .into_any_element()
    }
    #[cfg(feature = "test-support")]
    pub fn record_fork_write(&mut self, action: &Action, cx: &mut Context<Self>) {
        self.client.as_ref().unwrap().record_fork_write(action);
        self.sync_fork_status(cx);
    }
    #[cfg(feature = "test-support")]
    pub fn fork_review_ticket(&self) -> Option<uuid::Uuid> {
        self.fork_review.as_ref().map(|review| review.ticket)
    }
    #[cfg(feature = "test-support")]
    pub fn confirm_presented_fork(
        &mut self,
        ticket: uuid::Uuid,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_fork(ticket, window, cx);
    }
    #[cfg(feature = "test-support")]
    pub fn record_fork_receipt(
        &mut self,
        action: &Action,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        self.client
            .as_ref()
            .unwrap()
            .record_fork_outcome(action, outcome);
        self.sync_fork_status(cx);
    }
}
