//! Provisional legacy deltas, kept apart from canonical history and action state.
use formless_protocol::Event;

use crate::memory::limits;

pub const MAX_BYTES: usize = limits::COMPOSER_MAX_BYTES;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub answer: String,
    pub reasoning: String,
    pub unavailable: bool,
}

impl View {
    pub fn visible(&self) -> bool {
        self.unavailable || !self.answer.is_empty() || !self.reasoning.is_empty()
    }

    pub fn preview(text: &str) -> &str {
        &text[crate::timeline::window_start(
            text,
            text.len(),
            limits::BLOCK_PREVIEW_MAX_BYTES,
            limits::BLOCK_PREVIEW_MAX_LINES,
        )..]
    }
}

#[derive(Default)]
pub struct Overlay {
    run: Option<String>,
    ended: bool,
    blocked: bool,
    view: View,
    dirty: bool,
}

impl Overlay {
    pub fn take(&mut self) -> Option<View> {
        std::mem::take(&mut self.dirty).then(|| self.view.clone())
    }

    fn clear(&mut self) {
        self.dirty |= self.view.visible();
        self.view = View::default();
    }

    pub fn snapshot(&mut self, run: Option<&str>, running: bool) {
        self.clear();
        if let Some(run) = run {
            if self.run.as_deref() != Some(run) {
                self.blocked = false;
            }
            self.run = Some(run.to_owned());
        }
        self.ended = !running;
    }

    /// Returns whether canonical history should be fetched again.
    pub fn event(&mut self, session: &str, event: &Event) -> bool {
        if event.session_id().is_some_and(|id| id != session)
            || (event.session_id().is_none() && !matches!(event, Event::DeltasDropped { .. }))
        {
            return false;
        }
        match event {
            Event::RunStarted { run_id, .. } => {
                self.clear();
                self.run = Some(run_id.clone());
                self.ended = false;
                self.blocked = false;
            }
            Event::MessageDelta { run_id, delta, .. }
            | Event::ReasoningDelta { run_id, delta, .. } => {
                if self.ended || self.blocked || self.run.as_deref().is_some_and(|id| id != run_id)
                {
                    return false;
                }
                self.run = Some(run_id.clone());
                if self
                    .view
                    .answer
                    .len()
                    .saturating_add(self.view.reasoning.len())
                    .saturating_add(delta.len())
                    > MAX_BYTES
                {
                    self.clear();
                    self.blocked = true;
                    self.view.unavailable = true;
                    self.dirty = true;
                    return true;
                }
                if delta.is_empty() {
                    return false;
                }
                if matches!(event, Event::ReasoningDelta { .. }) {
                    self.view.reasoning.push_str(delta);
                } else {
                    self.view.answer.push_str(delta);
                }
                self.dirty = true;
            }
            Event::MessageComplete { run_id, .. } => {
                if self.run.as_deref().is_none_or(|id| id == run_id) {
                    self.clear();
                    self.blocked = false;
                    return true;
                }
            }
            Event::MessageAppended { message, .. } if message.role == "assistant" => {
                self.clear();
                self.blocked = false;
                return true;
            }
            Event::DeltasDropped { run_id, .. } => {
                if run_id.is_empty() || self.run.as_deref().is_none_or(|id| id == run_id) {
                    self.clear();
                    // A suffix after a gap is not a complete provisional answer.
                    self.blocked = true;
                    self.view.unavailable = true;
                    self.dirty = true;
                    return true;
                }
            }
            Event::StreamRestarted { run_id, .. } => {
                if self.run.as_deref().is_none_or(|id| id == run_id) {
                    self.clear();
                    self.blocked = false;
                    return true;
                }
            }
            Event::RunFinished { run_id, .. } | Event::RunPaused { run_id, .. }
                if self.run.as_deref().is_none_or(|id| id == run_id) =>
            {
                self.clear();
                self.ended = true;
                return true;
            }
            _ => {}
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(session: &str, run: &str, text: &str) -> Event {
        Event::MessageDelta {
            session_id: session.into(),
            run_id: run.into(),
            delta: text.into(),
        }
    }

    #[test]
    fn distinct_text_coalesces_without_changing_canonical_history() {
        let mut overlay = Overlay::default();
        assert_eq!(overlay.take(), None);
        overlay.event("s", &delta("s", "r", "Hello "));
        overlay.event("s", &delta("s", "r", "ไทย 😀"));
        overlay.event(
            "s",
            &Event::ReasoningDelta {
                session_id: "s".into(),
                run_id: "r".into(),
                delta: "thinking".into(),
            },
        );
        overlay.event("s", &delta("other", "r", "foreign"));
        overlay.event("s", &delta("s", "old", "stale"));
        assert_eq!(
            overlay.take(),
            Some(View {
                answer: "Hello ไทย 😀".into(),
                reasoning: "thinking".into(),
                unavailable: false
            })
        );
        assert_eq!(overlay.take(), None);
        overlay.event("s", &delta("s", "r", ""));
        assert_eq!(overlay.take(), None);
    }

    #[test]
    fn snapshots_and_restarts_discard_provisional_text() {
        let mut overlay = Overlay::default();
        overlay.event("s", &delta("s", "r", "discard"));
        overlay.snapshot(Some("r"), true);
        assert_eq!(overlay.take(), Some(View::default()));
        overlay.event("s", &delta("s", "r", "retry"));
        assert!(overlay.event(
            "s",
            &Event::StreamRestarted {
                session_id: "s".into(),
                run_id: "r".into()
            }
        ));
        assert_eq!(overlay.take(), Some(View::default()));
        overlay.event("s", &delta("s", "r", "replacement"));
        assert_eq!(overlay.take().unwrap().answer, "replacement");
        overlay.snapshot(Some("r"), false);
        overlay.event("s", &delta("s", "r", "late"));
        assert_eq!(overlay.take(), Some(View::default()));
    }

    #[test]
    fn gap_and_size_limit_fail_closed_until_a_boundary() {
        for gap in [false, true] {
            let mut overlay = Overlay::default();
            overlay.event("s", &delta("s", "r", "prefix"));
            if gap {
                assert!(overlay.event(
                    "s",
                    &Event::DeltasDropped {
                        session_id: "".into(),
                        run_id: "".into(),
                        kinds: vec![],
                        handle: None,
                        model: None
                    }
                ));
            } else {
                assert!(overlay.event("s", &delta("s", "r", &"x".repeat(MAX_BYTES))));
            }
            overlay.event("s", &delta("s", "r", "suffix"));
            let view = overlay.take().unwrap();
            assert!(view.unavailable);
            assert!(view.answer.is_empty());
            overlay.snapshot(Some("r"), true);
            overlay.event("s", &delta("s", "r", "still skipped"));
            assert_eq!(overlay.take(), Some(View::default()));
            overlay.event(
                "s",
                &Event::RunStarted {
                    session_id: "s".into(),
                    run_id: "next".into(),
                },
            );
            overlay.event("s", &delta("s", "next", "new"));
            assert_eq!(overlay.take().unwrap().answer, "new");
        }
    }

    #[test]
    fn completed_messages_and_finished_runs_retire_both_channels() {
        let mut overlay = Overlay::default();
        overlay.event("s", &delta("s", "r", "live"));
        let message = serde_json::from_value(serde_json::json!({
            "id":"m", "session_id":"s", "seq":1, "role":"assistant", "content":"canonical", "created_at":"now"
        })).unwrap();
        assert!(overlay.event(
            "s",
            &Event::MessageComplete {
                session_id: "s".into(),
                run_id: "r".into(),
                message
            }
        ));
        assert_eq!(overlay.take(), Some(View::default()));
        overlay.event("s", &delta("s", "r", "next step"));
        assert!(overlay.event(
            "s",
            &Event::RunFinished {
                session_id: "s".into(),
                run_id: "r".into(),
                status: "completed".into()
            }
        ));
        overlay.event("s", &delta("s", "r", "late"));
        assert_eq!(overlay.take(), Some(View::default()));
    }

    #[test]
    fn preview_is_bounded_and_utf8_safe() {
        let text = "ไทย😀\n".repeat(20_000);
        let preview = View::preview(&text);
        assert!(preview.len() <= limits::BLOCK_PREVIEW_MAX_BYTES);
        assert!(preview.lines().count() <= limits::BLOCK_PREVIEW_MAX_LINES);
        assert!(text.ends_with(preview));
    }
}
