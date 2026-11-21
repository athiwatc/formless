use super::*;
use crate::step_mode::Action;
impl Shell {
    pub(super) fn mode_enable_action(&self) -> Option<Action> {
        self.mode_change_action(true)
    }
    pub(super) fn mode_disable_action(&self) -> Option<Action> {
        self.mode_change_action(false)
    }
    fn mode_change_action(&self, on: bool) -> Option<Action> {
        if self.destination != Destination::Chat
            || !self.client_connected
            || !self.client_snapshot_ready
            || self.client_step_mode != Some(!on)
        {
            return None;
        }
        if !on
            && (self.run_state() != RunState::Idle
                || self.client_recovery.is_some()
                || !matches!(&self.client_steps, crate::steps::Inventory::Snapshot(s) if s.prompts.is_empty() && !s.more))
        {
            return None;
        }
        let client = self.client.as_ref()?;
        if !client.is_ready() || client.step_mode_status().blocked() {
            return None;
        }
        Some(Action {
            client: client.instance(),
            connection: client.connection(),
            generation: self.client_generation,
            view_epoch: client.stream_epoch(),
            operation: uuid::Uuid::new_v4(),
            session_id: self.sidebar.active_id()?.into(),
            on,
        })
    }
    pub(super) fn mode_notice(&self) -> Option<String> {
        let client = self.client.as_ref()?;
        let status = &self.mode_observed;
        let action = status.action()?;
        if action.client != client.instance()
            || (self.sidebar.active_id() != Some(&action.session_id) && !status.blocked())
        {
            return None;
        }
        Some(format!(
            "Step-mode change for Session {}: {}",
            action.session_id,
            status.notice()?
        ))
    }
    pub(super) fn sync_mode_status(&mut self, cx: &mut Context<Self>) {
        let status = self
            .client
            .as_ref()
            .map(|c| c.step_mode_status())
            .unwrap_or_default();
        if status != self.mode_observed {
            self.mode_observed = status;
            cx.notify();
        }
    }
    pub fn enable_mode(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if action.on {
            self.change_mode(action, window, cx)
        }
    }
    pub fn disable_mode(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if !action.on {
            self.change_mode(action, window, cx)
        }
    }
    fn change_mode(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if self.background_inert(window) || self.current_modal(self.layout(window)).is_some() {
            return;
        }
        let Some(current) = self.mode_change_action(action.on) else {
            return;
        };
        if !action.valid()
            || action.client != current.client
            || action.connection != current.connection
            || action.generation != current.generation
            || action.view_epoch != current.view_epoch
            || action.session_id != current.session_id
        {
            return;
        }
        if self.composer.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            self.composer_notice =
                Some("Finish composing first; Step mode was not changed.".into());
            cx.notify();
            return;
        }
        self.invalidate_forecast(cx);
        let client = self.client.as_ref().unwrap();
        let result = if action.on {
            client.enable_step_mode(action)
        } else {
            client.disable_step_mode(action)
        };
        match result {
            Ok(()) => {
                self.client_step_mode = None;
                self.composer
                    .update(cx, |input, cx| input.focus(window, cx));
            }
            Err(e) => self.composer_notice = Some(e.into()),
        }
        self.sync_mode_status(cx);
        cx.notify();
    }
    #[cfg(feature = "test-support")]
    pub fn step_mode_enable_target(&self) -> Option<Action> {
        self.mode_enable_action()
    }
    #[cfg(feature = "test-support")]
    pub fn step_mode_disable_target(&self) -> Option<Action> {
        self.mode_disable_action()
    }
    #[cfg(feature = "test-support")]
    pub fn sampled_step_mode(&self) -> Option<bool> {
        self.client_step_mode
    }
    #[cfg(feature = "test-support")]
    pub fn mode_enable_status(&self) -> crate::step_mode::Status {
        self.client
            .as_ref()
            .map(|c| c.step_mode_status())
            .unwrap_or_default()
    }
}

#[cfg(feature = "test-support")]
impl Shell {
    pub fn finish_recorded_mode(
        &mut self,
        d: &crate::step_mode::Dispatch,
        outcome: crate::step_mode::Outcome,
        cx: &mut Context<Self>,
    ) {
        self.client
            .as_ref()
            .unwrap()
            .finish_recorded_mode(d, outcome);
        self.sync_mode_status(cx);
    }
}
