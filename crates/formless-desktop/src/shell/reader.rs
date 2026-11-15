use super::*;
use crate::client::{StreamBatch, StreamOwner, StreamStatus};
impl Shell {
    fn stream_owner_current(&self, owner: &StreamOwner) -> bool {
        self.client_connected
            && self.client_generation == owner.generation
            && self.sidebar.active_id() == Some(owner.session_id.as_str())
            && self.client.as_ref().is_some_and(|client| {
                client.future_attempt_validation()
                    && client.is_ready()
                    && client.instance() == owner.client
                    && client.connection() == owner.connection
                    && client.stream_epoch() == owner.view_epoch
            })
    }
    pub(super) fn validate_stream_view(&mut self, cx: &mut Context<Self>) {
        if self
            .stream_view_owner
            .as_ref()
            .is_some_and(|owner| !self.stream_owner_current(owner))
        {
            self.retire_stream_view(cx);
        }
    }
    pub(super) fn retire_stream_view(&mut self, cx: &mut Context<Self>) {
        let Some(owner) = self.stream_view_owner.take() else {
            return;
        };
        if self.sidebar.active_id() != Some(owner.session_id.as_str())
            || self.client_generation != owner.generation
        {
            return;
        }
        self.client_steps = Default::default();
        if let Some(state) = self.transcript_state.as_mut() {
            state.retire_live(&mut self.blocks.borrow_mut());
        }
        self.invalidate_source_view();
        let count = self.blocks.borrow().len();
        self.timeline.update(cx, |timeline, cx| {
            let known = timeline.item_count();
            if count <= known {
                timeline.splice(count..known, 0, cx);
            } else {
                timeline.reset(count, cx);
            }
        });
        self.invalidate_search(cx);
        cx.notify();
    }
    fn stream_status_view(
        &mut self,
        owner: &StreamOwner,
        status: StreamStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = if status.running {
            RunState::Running
        } else if status
            .unfinished_run
            .as_ref()
            .is_some_and(|run| run.recoverable())
        {
            RunState::Paused
        } else {
            self.client_sessions
                .iter()
                .find(|session| session.id == owner.session_id)
                .map_or(RunState::Idle, |session| {
                    RunState::of(session, &HashSet::new(), true)
                })
        };
        self.sidebar.set_run_state(&owner.session_id, state);
        if let Some(row) = self
            .directory_rows
            .iter_mut()
            .find(|row| row.id == owner.session_id)
        {
            row.run_state = state;
        }
        if status.unfinished_run.is_none()
            && (self.resume_focus.is_focused(window) || self.abandon_focus.is_focused(window))
        {
            self.composer
                .update(cx, |composer, cx| composer.focus(window, cx));
        }
        self.client_steps = status.steps;
        self.client_recovery = status.unfinished_run;
        if !self.client_snapshot_ready {
            self.client_status =
                "Connected · live reader validation · stored-message fallback".into();
        }
        self.client_snapshot_ready = true;
        cx.notify();
    }
    pub(super) fn apply_stream_batch(
        &mut self,
        mut batch: StreamBatch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.stream_owner_current(&batch.owner) {
            return;
        }
        if let Some(snapshot) = batch.baseline.take() {
            if self.stream_view_latest.as_ref().is_some_and(|last| {
                last.same_context(&batch.owner) && batch.owner.load <= last.load
            }) {
                return;
            }
            if snapshot.session_id != batch.owner.session_id {
                return;
            }
            self.install_client_snapshot(batch.owner.generation, snapshot, window, cx);
            self.stream_view_owner = Some(batch.owner.clone());
            self.stream_view_latest = Some(batch.owner.clone());
        } else if self.stream_view_owner.as_ref() != Some(&batch.owner) {
            return;
        }
        if let Some(status) = batch.status {
            self.stream_status_view(&batch.owner, status, window, cx);
        }
        for event in batch.events {
            let result = self.apply_transcript_event(&event, cx);
            if result.is_err() || self.transcript_reload_needed().is_some() {
                self.retire_stream_view(cx);
                self.client_request(ClientCommand::Refresh, cx);
                return;
            }
        }
        if batch.reload {
            self.retire_stream_view(cx);
        }
    }
    #[cfg(feature = "test-support")]
    pub fn live_stream_owner(&self) -> Option<StreamOwner> {
        self.stream_view_owner.clone()
    }
    #[cfg(feature = "test-support")]
    pub fn live_stream_instance(&self) -> Option<uuid::Uuid> {
        self.client.as_ref().map(Client::instance)
    }
    #[cfg(feature = "test-support")]
    pub fn live_stream_epoch(&self) -> Option<u64> {
        self.client.as_ref().map(Client::stream_epoch)
    }
}
