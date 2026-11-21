//! Admission for the opt-in future-attempt protocol, not enabled by the shell.
//! The caller must install a fresh gate only from its owned barrier reply and
//! tag events at original receipt. Canonical invalidations are never replayed
//! or discarded using the producer barrier. No transcript/text is retained.
use crate::replies::SessionSnapshot;
use formless_protocol::{Event, TranscriptAttemptChange as Phase, TranscriptBarrier};
use std::sync::Arc;

pub const MAX_LIVE_BYTES: u64 = 1024 * 1024;
#[derive(Clone, Debug, Default)]
pub struct Attachment(Arc<()>);
#[derive(Debug, PartialEq)]
pub enum Change<'a> {
    Ignore,
    Started,
    Append(&'a str),
    Canonical(&'a formless_protocol::MessageInfo),
    Reload,
}
#[derive(Debug)]
struct Active {
    run: String,
    turn: String,
    attempt: String,
    bytes: u64,
}
#[derive(Debug)]
pub struct Gate {
    attachment: Attachment,
    session: String,
    base_seq: i64,
    barrier: TranscriptBarrier,
    observed_cursor: u64,
    active: Option<Active>,
    retired: bool,
}
fn identity(text: &str) -> bool {
    !text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control)
}
impl Gate {
    pub fn new(
        attachment: Attachment,
        snapshot: &SessionSnapshot,
        barrier: TranscriptBarrier,
    ) -> Result<Self, &'static str> {
        if !identity(&snapshot.session_id) || uuid::Uuid::parse_str(&barrier.epoch).is_err() {
            return Err("Invalid future-attempt barrier identity");
        }
        let base_seq = i64::try_from(snapshot.last_seq.unwrap_or(0))
            .map_err(|_| "Invalid canonical sequence")?;
        Ok(Self {
            attachment,
            session: snapshot.session_id.clone(),
            base_seq,
            observed_cursor: barrier.cursor,
            barrier,
            active: None,
            retired: false,
        })
    }
    pub fn canonical_seq(&self) -> u64 {
        self.base_seq as u64
    }
    pub fn retired(&self) -> bool {
        self.retired
    }
    fn fail(&mut self) -> Change<'static> {
        self.active = None;
        self.retired = true;
        Change::Reload
    }
    pub fn event<'a>(&mut self, attachment: &Attachment, event: &'a Event) -> Change<'a> {
        if !Arc::ptr_eq(&attachment.0, &self.attachment.0) || self.retired {
            return Change::Ignore;
        }
        let Event::TranscriptAttempt { update } = event else {
            if let Event::MessageAppended {
                session_id,
                message,
            }
            | Event::MessageComplete {
                session_id,
                message,
                ..
            } = event
            {
                if session_id != &self.session {
                    return Change::Ignore;
                }
                // Only an exact next canonical record can advance an idle
                // baseline. Duplicates/overlap or an unbound boundary during
                // live display require a fresh snapshot instead of guessing.
                if self.active.is_none()
                    && crate::replies::message_sequence(&self.session, message).is_ok()
                    && self.base_seq.checked_add(1) == Some(message.seq)
                {
                    self.base_seq = message.seq;
                    return Change::Canonical(message);
                }
                return self.fail();
            }
            if crate::transcript::affects_transcript(event, &self.session)
                || matches!(event, Event::RunStarted { session_id, .. } | Event::RunFinished { session_id, .. } | Event::RunPaused { session_id, .. } if session_id == &self.session && self.active.is_some())
                || matches!(event, Event::SessionDeleted { id } if id == &self.session)
            {
                return self.fail();
            }
            return Change::Ignore;
        };
        if update.session_id != self.session {
            return Change::Ignore;
        }
        if update.epoch != self.barrier.epoch
            || !identity(&update.run_id)
            || !identity(&update.turn_id)
            || !identity(&update.attempt_id)
        {
            return self.fail();
        }
        // Old attempts are never resumed, including their delayed prefix.
        if self.active.is_none() && update.cursor <= self.barrier.cursor {
            return Change::Ignore;
        }
        // Even an ignored future Delta/Closed proves a later scoped cursor
        // was received. It must not permit a delayed Start to revive a prefix.
        // Gaps are valid because other Sessions consume global cursor values.
        if update.cursor <= self.observed_cursor {
            return self.fail();
        }
        self.observed_cursor = update.cursor;
        match &update.change {
            Phase::Started { base_seq } => {
                if self.active.is_some() || *base_seq != self.base_seq {
                    return self.fail();
                }
                self.active = Some(Active {
                    run: update.run_id.clone(),
                    turn: update.turn_id.clone(),
                    attempt: update.attempt_id.clone(),
                    bytes: 0,
                });
                Change::Started
            }
            Phase::Delta { offset, text } => {
                let Some(active) = self.active.as_mut() else {
                    return Change::Ignore;
                };
                if active.run != update.run_id
                    || active.turn != update.turn_id
                    || active.attempt != update.attempt_id
                    || *offset != active.bytes
                {
                    return self.fail();
                }
                let Some(bytes) = active
                    .bytes
                    .checked_add(text.len() as u64)
                    .filter(|bytes| *bytes <= MAX_LIVE_BYTES)
                else {
                    return self.fail();
                };
                active.bytes = bytes;
                Change::Append(text)
            }
            Phase::Closed { .. } => {
                if self.active.is_none() {
                    return Change::Ignore;
                }
                // Neither a successful byte count nor "returned" proves
                // persistence/acceptance. Close always requires a new snapshot.
                self.fail()
            }
        }
    }
}
/// Bounded handshake staging for events received before the barrier reply.
/// The caller must install the returned canonical snapshot and replay these
/// owned frames through Gate in one serialized transition, before later frames.
pub struct Pending {
    attachment: Attachment,
    session: String,
    events: Vec<Event>,
    bytes: usize,
    overflow: bool,
}
pub struct Loaded {
    pub gate: Gate,
    pub snapshot: SessionSnapshot,
    pub buffered: Vec<Event>,
}
struct Count {
    bytes: usize,
    remaining: usize,
}
impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::other("barrier event bound"));
        }
        self.bytes += bytes.len();
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn fits_event(event: &Event) -> bool {
    let mut count = Count {
        bytes: 0,
        remaining: MAX_LIVE_BYTES as usize,
    };
    serde_json::to_writer(&mut count, event).is_ok()
}
impl Pending {
    pub fn new(attachment: Attachment, session: &str) -> Result<Self, &'static str> {
        if !identity(session) {
            return Err("Invalid Session identity");
        }
        Ok(Self {
            attachment,
            session: session.into(),
            events: Vec::new(),
            bytes: 0,
            overflow: false,
        })
    }
    pub fn observe(&mut self, attachment: &Attachment, event: &Event) -> Result<(), &'static str> {
        if !Arc::ptr_eq(&attachment.0, &self.attachment.0) {
            return Ok(());
        }
        if self.overflow {
            return Err("Barrier staging overflow; use canonical mode");
        }
        if event.session_id() != Some(self.session.as_str())
            && !crate::transcript::affects_transcript(event, &self.session)
            && !matches!(event, Event::SessionDeleted { id } if id == &self.session)
        {
            return Ok(());
        }
        let mut count = Count {
            bytes: 0,
            remaining: (MAX_LIVE_BYTES as usize).saturating_sub(self.bytes),
        };
        if self.events.len() == 256 || serde_json::to_writer(&mut count, event).is_err() {
            self.events.clear();
            self.bytes = 0;
            self.overflow = true;
            return Err("Barrier staging overflow; use canonical mode");
        }
        self.bytes += count.bytes;
        self.events.push(event.clone());
        Ok(())
    }
    pub fn finish(
        self,
        current: &Attachment,
        reply: &serde_json::Value,
    ) -> Result<Loaded, &'static str> {
        if !Arc::ptr_eq(&self.attachment.0, &current.0) {
            return Err("Retired barrier attachment");
        }
        if self.overflow {
            return Err("Barrier staging overflow; use canonical mode");
        }
        if reply["mode"] != "future_attempts_only_v1" {
            return Err("Unsupported barrier mode");
        }
        let snapshot = crate::replies::session_snapshot(&self.session, &reply["snapshot"])
            .map_err(|_| "Invalid barrier snapshot")?;
        let mut metadata_size = Count {
            bytes: 0,
            remaining: 2048,
        };
        serde_json::to_writer(&mut metadata_size, &reply["barrier"])
            .map_err(|_| "Oversized producer barrier")?;
        let barrier = serde_json::from_value::<TranscriptBarrier>(reply["barrier"].clone())
            .map_err(|_| "Invalid producer barrier")?;
        let gate = Gate::new(self.attachment, &snapshot, barrier)?;
        Ok(Loaded {
            gate,
            snapshot,
            buffered: self.events,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use formless_protocol::{TranscriptAttemptClose, TranscriptAttemptEvent};
    use serde_json::json;
    const EPOCH: &str = "4f0d6106-0902-4401-942e-6ff75ddd1565";
    fn gate() -> (Attachment, Gate) {
        let attachment = Attachment::default();
        let snapshot = crate::replies::session_snapshot("s", &json!({"session":{"id":"s"},"running":true,"messages":[{"id":"m","session_id":"s","seq":4,"role":"user","content":"Question","created_at":"now"}]})).unwrap();
        let gate = Gate::new(
            attachment.clone(),
            &snapshot,
            TranscriptBarrier {
                epoch: EPOCH.into(),
                cursor: 10,
            },
        )
        .unwrap();
        (attachment, gate)
    }
    fn event(cursor: u64, change: Phase) -> Event {
        Event::TranscriptAttempt {
            update: TranscriptAttemptEvent {
                epoch: EPOCH.into(),
                cursor,
                session_id: "s".into(),
                run_id: "r".into(),
                turn_id: "turn".into(),
                attempt_id: "attempt".into(),
                change,
            },
        }
    }
    fn start(cursor: u64) -> Event {
        event(cursor, Phase::Started { base_seq: 4 })
    }
    fn delta(cursor: u64, offset: u64, text: &str) -> Event {
        event(
            cursor,
            Phase::Delta {
                offset,
                text: text.into(),
            },
        )
    }
    #[test]
    fn ignored_future_frames_cannot_be_followed_by_a_lower_cursor_start() {
        for prior in [
            delta(12, 0, "missing prefix"),
            event(
                12,
                Phase::Closed {
                    bytes: 0,
                    reason: TranscriptAttemptClose::Returned,
                },
            ),
        ] {
            let (tag, mut gate) = gate();
            assert_eq!(gate.event(&tag, &prior), Change::Ignore);
            assert_eq!(gate.event(&tag, &start(11)), Change::Reload);
            assert!(gate.retired());
            assert_eq!(
                gate.event(&tag, &delta(13, 0, "must not appear")),
                Change::Ignore
            );
        }
    }
    #[test]
    fn ignored_future_frames_advance_floor_without_requiring_contiguous_cursors() {
        let (tag, mut gate) = gate();
        assert_eq!(
            gate.event(&tag, &delta(12, 0, "not replayed")),
            Change::Ignore
        );
        assert_eq!(gate.event(&tag, &start(20)), Change::Started);
        assert_eq!(
            gate.event(&tag, &delta(99, 0, "new prefix")),
            Change::Append("new prefix")
        );
    }
    #[test]
    fn duplicate_scoped_future_frames_retire_even_without_an_active_attempt() {
        let (tag, mut gate) = gate();
        assert_eq!(gate.event(&tag, &delta(12, 0, "ignored")), Change::Ignore);
        assert_eq!(gate.event(&tag, &delta(12, 0, "duplicate")), Change::Reload);
    }
    #[test]
    fn old_barrier_frames_and_foreign_owners_do_not_poison_future_cursor_floor() {
        let (tag, mut gate) = gate();
        assert_eq!(gate.event(&tag, &delta(12, 0, "ignored")), Change::Ignore);
        let mut foreign = start(999);
        if let Event::TranscriptAttempt { update } = &mut foreign {
            update.session_id = "other".into();
        }
        assert_eq!(gate.event(&tag, &foreign), Change::Ignore);
        assert_eq!(
            gate.event(&Attachment::default(), &start(999)),
            Change::Ignore
        );
        assert_eq!(gate.event(&tag, &start(10)), Change::Ignore);
        assert_eq!(gate.event(&tag, &start(13)), Change::Started);
        assert_eq!(
            gate.event(&tag, &delta(14, 0, "current")),
            Change::Append("current")
        );
    }
    #[test]
    fn only_a_future_start_can_admit_text_and_offsets_count_utf8_bytes() {
        let (tag, mut gate) = gate();
        for e in [
            start(10),
            delta(9, 0, "old"),
            delta(12, 0, "missing prefix"),
        ] {
            assert_eq!(gate.event(&tag, &e), Change::Ignore);
        }
        assert_eq!(gate.event(&tag, &start(13)), Change::Started);
        assert_eq!(
            gate.event(&tag, &delta(14, 0, "ไทย😀")),
            Change::Append("ไทย😀")
        );
        assert_eq!(
            gate.event(&tag, &delta(20, 13, "x")),
            Change::Append("x"),
            "other Sessions can consume intervening global cursors"
        );
        assert_eq!(
            gate.event(
                &tag,
                &event(
                    21,
                    Phase::Closed {
                        bytes: 14,
                        reason: TranscriptAttemptClose::Returned
                    }
                )
            ),
            Change::Reload
        );
        assert!(gate.retired());
        assert_eq!(gate.event(&tag, &start(22)), Change::Ignore);
    }
    #[test]
    fn missing_duplicate_foreign_or_oversized_chunks_retire_without_append() {
        for mode in 0..8 {
            let (tag, mut gate) = gate();
            gate.event(&tag, &start(11));
            let mut bad = delta(12, 0, "x");
            if let Event::TranscriptAttempt { update } = &mut bad {
                match mode {
                    0 => {
                        update.change = Phase::Delta {
                            offset: 1,
                            text: "missing prefix".into(),
                        }
                    }
                    1 => update.cursor = 11,
                    2 => update.attempt_id = "other".into(),
                    3 => update.turn_id = "other".into(),
                    4 => update.run_id = "other".into(),
                    5 => update.epoch = uuid::Uuid::new_v4().to_string(),
                    6 => {
                        update.change = Phase::Delta {
                            offset: 0,
                            text: "x".repeat(MAX_LIVE_BYTES as usize + 1),
                        }
                    }
                    _ => update.attempt_id.clear(),
                }
            }
            assert_eq!(gate.event(&tag, &bad), Change::Reload, "mode {mode}");
            assert_eq!(gate.event(&tag, &delta(99, 0, "later")), Change::Ignore);
        }
    }
    #[test]
    fn attachment_and_session_ownership_precede_all_admission() {
        let (tag, mut gate) = gate();
        let retired = Attachment::default();
        assert_eq!(gate.event(&retired, &start(99)), Change::Ignore);
        let mut foreign = start(12);
        if let Event::TranscriptAttempt { update } = &mut foreign {
            update.session_id = "other".into();
        }
        assert_eq!(gate.event(&tag, &foreign), Change::Ignore);
        assert_eq!(gate.event(&tag, &start(11)), Change::Started);
        assert_eq!(
            gate.event(&retired, &delta(99, 0, "old A → B → A")),
            Change::Ignore
        );
        assert_eq!(
            gate.event(&tag, &delta(12, 0, "current")),
            Change::Append("current")
        );
    }
    #[test]
    fn canonical_invalidations_are_never_discarded_by_a_producer_barrier() {
        for active in [false, true] {
            let (tag, mut gate) = gate();
            if active {
                gate.event(&tag, &start(11));
            }
            let snapshot=crate::replies::session_snapshot("s",&json!({"session":{"id":"s"},"running":true,"messages":[{"id":"m","session_id":"s","seq":4,"role":"user","content":"Question","created_at":"now"}]})).unwrap();
            let event = Event::MessageAppended {
                session_id: "s".into(),
                message: formless_protocol::MessageInfo {
                    generated_by: None,
                    id: "m".into(),
                    session_id: "s".into(),
                    seq: 4,
                    role: "user".into(),
                    content: "Question".into(),
                    created_at: "now".into(),
                    meta: None,
                },
            };
            assert_eq!(snapshot.last_seq, Some(4));
            assert_eq!(
                gate.event(&tag, &event),
                Change::Reload,
                "even an exact already-in-snapshot canonical event retires the barrier"
            );
        }
        let (tag, mut gate) = gate();
        assert_eq!(
            gate.event(&tag, &event(11, Phase::Started { base_seq: 5 })),
            Change::Reload
        );
    }
    #[test]
    fn a_missing_final_chunk_or_restart_never_proves_canonical_completion() {
        for reason in [
            TranscriptAttemptClose::Returned,
            TranscriptAttemptClose::Restarted,
            TranscriptAttemptClose::Dropped,
        ] {
            let (tag, mut gate) = gate();
            gate.event(&tag, &start(11));
            gate.event(&tag, &delta(12, 0, "partial"));
            assert_eq!(
                gate.event(&tag, &event(13, Phase::Closed { bytes: 100, reason })),
                Change::Reload
            );
        }
    }
    fn reply() -> serde_json::Value {
        json!({"mode":"future_attempts_only_v1","barrier":{"epoch":EPOCH,"cursor":10},"snapshot":{"session":{"id":"s"},"running":true,"messages":[{"id":"m","session_id":"s","seq":4,"role":"user","content":"Question","created_at":"now"}]}})
    }
    #[test]
    fn staging_preserves_a_complete_future_prefix_received_before_its_reply() {
        let tag = Attachment::default();
        let mut pending = Pending::new(tag.clone(), "s").unwrap();
        pending.observe(&tag, &start(11)).unwrap();
        pending
            .observe(&tag, &delta(12, 0, "complete prefix ไทย"))
            .unwrap();
        let mut loaded = pending.finish(&tag, &reply()).unwrap();
        assert_eq!(loaded.snapshot.last_seq, Some(4));
        assert_eq!(
            loaded.gate.event(&tag, &loaded.buffered[0]),
            Change::Started
        );
        assert_eq!(
            loaded.gate.event(&tag, &loaded.buffered[1]),
            Change::Append("complete prefix ไทย")
        );
    }
    #[test]
    fn staging_excludes_an_old_attempt_and_never_discards_canonical_overlap() {
        let tag = Attachment::default();
        let mut pending = Pending::new(tag.clone(), "s").unwrap();
        pending.observe(&tag, &start(9)).unwrap();
        pending.observe(&tag, &delta(11, 0, "old prefix")).unwrap();
        pending
            .observe(
                &tag,
                &Event::MessagesEdited {
                    session_id: "s".into(),
                },
            )
            .unwrap();
        let mut loaded = pending.finish(&tag, &reply()).unwrap();
        assert_eq!(loaded.gate.event(&tag, &loaded.buffered[0]), Change::Ignore);
        assert_eq!(loaded.gate.event(&tag, &loaded.buffered[1]), Change::Ignore);
        assert_eq!(loaded.gate.event(&tag, &loaded.buffered[2]), Change::Reload);
    }
    #[test]
    fn staging_has_both_count_and_byte_bounds_and_keeps_retired_attachments_inert() {
        let tag = Attachment::default();
        let old = Attachment::default();
        let mut pending = Pending::new(tag.clone(), "s").unwrap();
        pending
            .observe(
                &old,
                &delta(99, 0, &"x".repeat(MAX_LIVE_BYTES as usize + 1)),
            )
            .unwrap();
        assert!(pending.events.is_empty());
        assert!(pending
            .observe(
                &tag,
                &delta(12, 0, &"x".repeat(MAX_LIVE_BYTES as usize + 1))
            )
            .is_err());
        assert!(pending.finish(&tag, &reply()).is_err());
        let mut pending = Pending::new(tag.clone(), "s").unwrap();
        for n in 0..256 {
            pending.observe(&tag, &delta(n + 11, 0, "")).unwrap();
        }
        assert!(pending.observe(&tag, &delta(999, 0, "")).is_err());
        assert!(pending.events.is_empty());
        assert!(pending.finish(&tag, &reply()).is_err());
        assert!(Pending::new(tag.clone(), "s")
            .unwrap()
            .finish(&old, &serde_json::Value::Null)
            .is_err());
    }
    #[test]
    fn staging_refuses_wrong_session_mode_and_invalid_epoch_replies() {
        for bad in ["session", "mode", "epoch", "oversized"] {
            let tag = Attachment::default();
            let pending = Pending::new(tag.clone(), "s").unwrap();
            let mut value = reply();
            match bad {
                "session" => value["snapshot"]["session"]["id"] = "other".into(),
                "mode" => value["mode"] = "general_replay".into(),
                "oversized" => value["barrier"]["future"] = "x".repeat(2049).into(),
                _ => value["barrier"]["epoch"] = "invalid".into(),
            }
            assert!(pending.finish(&tag, &value).is_err());
        }
    }
}

#[cfg(test)]
#[path = "attempt_admission_schedules.rs"]
mod schedules;
