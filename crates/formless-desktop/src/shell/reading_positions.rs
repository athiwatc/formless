use super::*;

impl Shell {
    /// Only a decoded snapshot of the Session being left can own a bookmark.
    /// Do not overwrite it when the user leaves another still-loading view.
    pub(super) fn park_reading_position(&mut self, session: &str, cx: &App) {
        if self.transcript_source != TranscriptSource::Snapshot
            || self
                .transcript_state
                .as_ref()
                .is_none_or(|state| state.session_id() != session)
        {
            return;
        }
        let state = self.timeline.read(cx);
        if !state.is_scrolled_up() {
            self.reading_positions.save(session, None);
        } else {
            self.reading_positions
                .save(session, self.current_reading_message(cx));
        }
    }
    pub(super) fn current_reading_message(&self, cx: &App) -> Option<u64> {
        if self.transcript_source != TranscriptSource::Snapshot
            || !self.timeline.read(cx).is_scrolled_up()
        {
            return None;
        }
        let index = self.timeline.read(cx).logical_scroll_top().item_ix;
        let blocks = self.blocks.borrow();
        if blocks.is_streaming() && index + 1 == blocks.len() {
            return None;
        }
        Some(blocks.blocks().get(index)?.id)
    }
    pub(super) fn scroll_to_reading_message(&mut self, message: u64, cx: &mut Context<Self>) {
        let row = self
            .blocks
            .borrow()
            .blocks()
            .iter()
            .position(|block| block.id == message);
        if let Some(row) = row {
            self.timeline.update(cx, |timeline, cx| {
                timeline.scroll_to_item(row, cx);
            });
        } else {
            if let Some(session) = self.sidebar.active_id() {
                self.reading_positions.save(session, None);
            }
            self.timeline
                .update(cx, |timeline, cx| timeline.scroll_to_end(cx));
            self.reading_notice=Some("The saved reading message is absent from this snapshot. Showing latest; the old position was forgotten.");
        }
    }

    /// Invoked only after a snapshot is accepted by the existing Session and
    /// selection-generation boundary. A normal refresh never replays a position.
    pub(super) fn restore_reading_position(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.reading_restore.as_ref() else {
            return;
        };
        if self.sidebar.active_id() != Some(session.as_str())
            || self.transcript_source != TranscriptSource::Snapshot
            || self
                .transcript_state
                .as_ref()
                .is_none_or(|state| state.session_id() != session)
        {
            return;
        }
        let session = self.reading_restore.take().unwrap();
        let Some(message) = self.reading_positions.get(&session) else {
            return;
        };
        self.scroll_to_reading_message(message, cx);
    }

    #[cfg(feature = "test-support")]
    pub fn following_latest(&self, cx: &App) -> bool {
        self.timeline.read(cx).is_following_tail()
    }
    #[cfg(feature = "test-support")]
    pub fn reading_message(&self, cx: &App) -> Option<u64> {
        self.blocks
            .borrow()
            .blocks()
            .get(self.timeline.read(cx).logical_scroll_top().item_ix)
            .map(|block| block.id)
    }
    #[cfg(feature = "test-support")]
    pub fn saved_reading_positions(&self) -> usize {
        self.reading_positions.len()
    }
}
