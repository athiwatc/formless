//! Pin a headless command to the Run acknowledged by its own send request.
use super::*;
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct Target {
    pub(super) run_id: Option<String>,
    acknowledged: bool,
    pending: VecDeque<Event>,
    bytes: usize,
}
impl Target {
    pub(super) fn buffer(&mut self, event: Event) -> Result<()> {
        let bytes = serde_json::to_vec(&event)?.len();
        anyhow::ensure!(self.pending.len() < 4096 && self.bytes.saturating_add(bytes) <= 4 * 1024 * 1024,
            "headless event backlog exceeded 4096 messages or 4 MiB while awaiting Run identity; reconnect to resynchronize");
        self.bytes += bytes;
        self.pending.push_back(event);
        Ok(())
    }
    pub(super) fn waiting(&self) -> bool {
        !self.acknowledged
    }
    pub(super) fn acknowledge(&mut self, result: &Value) -> Result<()> {
        if let Some(run) = result["run_id"].as_str().filter(|id| !id.is_empty()) {
            self.run_id = Some(run.into());
        } else {
            // Older daemons acknowledged only `started`. A single buffered
            // start remains usable; competing starts cannot be attributed.
            let mut starts = self.pending.iter().filter_map(|event| match event {
                Event::RunStarted { run_id, .. } => Some(run_id.as_str()),
                _ => None,
            });
            let first = starts.next();
            anyhow::ensure!(result["started"] == true && first.is_some() && starts.all(|id| Some(id) == first),
                "the daemon did not identify the accepted Run; restart it with the current Formless binary before retrying");
            self.run_id = first.map(str::to_owned);
        }
        self.acknowledged = true;
        Ok(())
    }
    pub(super) fn next(&mut self) -> Option<Event> {
        if !self.acknowledged {
            return None;
        }
        let event = self.pending.pop_front()?;
        self.bytes = self
            .bytes
            .saturating_sub(serde_json::to_vec(&event).map_or(0, |v| v.len()));
        Some(event)
    }
    pub(super) fn accepts(&self, event: &Event) -> Result<bool> {
        let identity = serde_json::to_value(event)?;
        let event_run = identity["run_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .or_else(|| identity["plan_state"]["run_id"].as_str());
        Ok(event_run.is_none_or(|id| self.run_id.as_deref() == Some(id)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn started(id: &str) -> Event {
        Event::RunStarted {
            session_id: "s".into(),
            run_id: id.into(),
        }
    }
    #[test]
    fn acknowledgment_pins_target_and_replays_buffer_without_old_tail() {
        let mut target = Target::default();
        target.buffer(started("old")).unwrap();
        target.buffer(started("new")).unwrap();
        target
            .buffer(Event::RunPaused {
                session_id: "s".into(),
                run_id: "old".into(),
                error: "obsolete".into(),
                explained: None,
            })
            .unwrap();
        assert!(target.next().is_none());
        target
            .acknowledge(&serde_json::json!({"run_id":"new"}))
            .unwrap();
        let mut accepted = Vec::new();
        while let Some(event) = target.next() {
            if target.accepts(&event).unwrap() {
                accepted.push(event);
            }
        }
        assert_eq!(accepted.len(), 1);
        assert!(matches!(&accepted[0], Event::RunStarted { run_id, .. } if run_id == "new"));
        for event in [
            started("old"),
            Event::RunFinished {
                session_id: "s".into(),
                run_id: "old".into(),
                status: "failed".into(),
            },
            Event::RunRecap {
                session_id: "s".into(),
                run_id: "old".into(),
                recap: Default::default(),
            },
        ] {
            assert!(!target.accepts(&event).unwrap());
        }
        assert_eq!(target.run_id.as_deref(), Some("new"));
        assert_eq!(target.bytes, 0);
    }
    #[test]
    fn legacy_acknowledgment_refuses_ambiguous_starts_and_queue_overflow() {
        let mut target = Target::default();
        target.buffer(started("old")).unwrap();
        target.buffer(started("new")).unwrap();
        assert!(target
            .acknowledge(&serde_json::json!({"started":true}))
            .is_err());
        let mut legacy = Target::default();
        legacy.buffer(started("only")).unwrap();
        legacy
            .acknowledge(&serde_json::json!({"started":true}))
            .unwrap();
        assert_eq!(legacy.run_id.as_deref(), Some("only"));
        target.bytes = 4 * 1024 * 1024;
        assert!(target.buffer(started("overflow")).is_err());
    }
}
