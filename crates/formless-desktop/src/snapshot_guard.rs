//! Ownership and ordering guard for future read-only `GetSession` requests.
//!
//! This is not a transport, reconnect loop, event buffer or replay protocol.
//! A consumer creates a new context whenever the selected Session or event
//! stream/connection changes, and tags both requests and events at receipt.
//! A request ticket belongs to this exact guard, context and latest request.
//! Retired replies are refused before decoding, even on A -> B -> A.
//!
//! The current protocol has no snapshot/event watermark. Any transcript event
//! observed while a request is outstanding makes its reply ambiguous: it may
//! predate or already contain the event. We refuse that reply and require a
//! new request, rather than erasing newer text or replaying deltas twice. This
//! conservative policy can need repeated retries during an active stream. A
//! caller must expose the retry state and choose a retry cadence; this guard
//! neither retries nor claims that the client has recovered.
//!
//! Success proves local ownership and no *observed* overlap only. It does not
//! provide a protocol cutoff: a delayed event may already be in the snapshot.
//! Future transport integration must characterize that ordering, serialize
//! event observation/application with resolve + snapshot installation, and
//! discard or revalidate queued work. Do not use this guard alone as proof
//! that reconnect recovery is complete or a transcript gap can be cleared.

use std::sync::Arc;

use formless_protocol::Event;

use crate::replies::{self, DecodeError, SessionSnapshot};
use crate::transcript::affects_transcript;

/// Local lifetime identity, not a protocol request id or authentication token.
/// Clone it into the request/reader work for this Session attachment. Creating
/// a new context, including for the same Session, retires all older work.
#[derive(Clone, Debug)]
pub struct SnapshotContext {
    identity: Arc<()>,
    session_id: Arc<str>,
}

impl SnapshotContext {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Opaque, non-serializable ownership tag. Keep it beside the transport's
/// request id, not in the wire payload. It is valid at most once.
#[derive(Clone, Debug)]
pub struct SnapshotTicket {
    context: SnapshotContext,
    serial: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotError {
    /// Wrong guard, retired attachment, superseded/completed/cancelled request.
    Stale,
    /// No reply cutoff exists with which to order these events safely.
    EventsDuringRequest,
    Decode(DecodeError),
    /// Never wrap and allow an old ticket to match a new one.
    RequestCounterExhausted,
}

impl SnapshotError {
    pub fn notice(&self) -> &'static str {
        match self {
            Self::Stale => "This Session load was superseded; its reply was ignored.",
            Self::EventsDuringRequest => "The transcript changed while loading; reload this Session to get a current snapshot.",
            Self::Decode(_) => "The Session reply could not be read; reload this Session.",
            Self::RequestCounterExhausted => "This Session attachment cannot start another load; reattach before reloading.",
        }
    }
}

#[derive(Debug)]
struct Pending {
    serial: u64,
    saw_transcript_event: bool,
}

/// At most one current context and pending request; no transcript or event
/// copies. Old tickets retain only their own small context identity/Session id.
#[derive(Debug, Default)]
pub struct SnapshotGuard {
    context: Option<SnapshotContext>,
    serial: u64,
    pending: Option<Pending>,
}

impl SnapshotGuard {
    /// A new Session or a new attachment/connection, even for the same id.
    /// The caller must also discard any pending old event-application work.
    pub fn attach(&mut self, session_id: impl Into<String>) -> SnapshotContext {
        let context = SnapshotContext {
            identity: Arc::new(()),
            session_id: Arc::from(session_id.into()),
        };
        self.context = Some(context.clone());
        self.pending = None;
        self.serial = 0;
        context
    }

    pub fn detach(&mut self) {
        self.context = None;
        self.pending = None;
    }

    pub fn is_current(&self, context: &SnapshotContext) -> bool {
        self.context
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(&current.identity, &context.identity))
    }

    /// Supersedes the previous outstanding request without buffering it.
    pub fn begin(&mut self, context: &SnapshotContext) -> Result<SnapshotTicket, SnapshotError> {
        if !self.is_current(context) {
            return Err(SnapshotError::Stale);
        }
        let serial = self
            .serial
            .checked_add(1)
            .ok_or(SnapshotError::RequestCounterExhausted)?;
        self.serial = serial;
        self.pending = Some(Pending {
            serial,
            saw_transcript_event: false,
        });
        Ok(SnapshotTicket {
            context: context.clone(),
            serial,
        })
    }

    /// Observe every event from its original attachment, before the caller
    /// applies or queues it. Returns whether it can affect this transcript;
    /// it does not authorize applying it to a missing or outdated snapshot.
    /// Foreign Sessions/attachments and non-transcript events do not dirty
    /// the pending load. Even an exact duplicate conservatively dirties it.
    pub fn observe(&mut self, context: &SnapshotContext, event: &Event) -> bool {
        if !self.is_current(context) || !affects_transcript(event, context.session_id()) {
            return false;
        }
        if let Some(pending) = &mut self.pending {
            pending.saw_transcript_event = true;
        }
        true
    }

    fn owns(&self, ticket: &SnapshotTicket) -> bool {
        self.is_current(&ticket.context)
            && self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.serial == ticket.serial)
    }

    /// Cancellation/failure for an older request cannot cancel a newer one.
    pub fn cancel(&mut self, ticket: &SnapshotTicket) -> bool {
        if !self.owns(ticket) {
            return false;
        }
        self.pending = None;
        true
    }

    /// Decode only a current reply with no observed overlap. Resolve and
    /// installation must happen in the same serialized state transition as
    /// event handling, not an asynchronous display task. This is not proof
    /// of a protocol cutoff or recovery; see the module ordering contract.
    /// Failure changes no transcript. Every current reply, successful or
    /// failed, consumes its pending request.
    pub fn resolve(
        &mut self,
        ticket: &SnapshotTicket,
        result: &serde_json::Value,
    ) -> Result<SessionSnapshot, SnapshotError> {
        if !self.owns(ticket) {
            return Err(SnapshotError::Stale);
        }
        let pending = self.pending.take().expect("owns checked the pending load");
        if pending.saw_transcript_event {
            return Err(SnapshotError::EventsDuringRequest);
        }
        replies::session_snapshot(ticket.context.session_id(), result)
            .map_err(SnapshotError::Decode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use formless_protocol::DroppedEventKind;
    use serde_json::json;

    fn reply(id: &str, text: &str) -> serde_json::Value {
        json!({"session":{"id":id},"running":false,"messages":[{
            "id":"m1","session_id":id,"seq":1,"role":"user","content":text,"created_at":"now"
        }]})
    }

    fn delta(id: &str) -> Event {
        Event::MessageDelta {
            session_id: id.into(),
            run_id: "r".into(),
            delta: "new text".into(),
        }
    }

    fn dropped(id: &str, kinds: Vec<DroppedEventKind>) -> Event {
        Event::DeltasDropped {
            session_id: id.into(),
            run_id: String::new(),
            kinds,
            handle: None,
            model: None,
        }
    }

    #[test]
    fn only_the_latest_request_can_decode_and_each_ticket_is_consumed_once() {
        let mut guard = SnapshotGuard::default();
        let context = guard.attach("s1");
        let old = guard.begin(&context).unwrap();
        let current = guard.begin(&context).unwrap();
        assert_eq!(guard.resolve(&old, &json!(null)), Err(SnapshotError::Stale));
        assert!(!guard.cancel(&old));
        let snapshot = guard.resolve(&current, &reply("s1", "new")).unwrap();
        assert_eq!(snapshot.blocks[0].markdown(), "new");
        assert_eq!(
            guard.resolve(&current, &reply("s1", "old")),
            Err(SnapshotError::Stale)
        );
    }

    #[test]
    fn a_b_a_and_same_session_reattachment_cannot_revive_old_requests_or_events() {
        let mut guard = SnapshotGuard::default();
        let first = guard.attach("a");
        let old = guard.begin(&first).unwrap();
        guard.attach("b");
        let returned = guard.attach("a");
        let current = guard.begin(&returned).unwrap();
        assert!(!guard.observe(&first, &delta("a")));
        assert_eq!(guard.begin(&first).unwrap_err(), SnapshotError::Stale);
        assert_eq!(
            guard.resolve(&old, &reply("a", "old")),
            Err(SnapshotError::Stale)
        );
        assert_eq!(
            guard
                .resolve(&current, &reply("a", "current"))
                .unwrap()
                .session_id,
            "a"
        );
        let before_reconnect = guard.begin(&returned).unwrap();
        let reconnected = guard.attach("a");
        let after_reconnect = guard.begin(&reconnected).unwrap();
        assert_eq!(
            guard.resolve(&before_reconnect, &reply("a", "old")),
            Err(SnapshotError::Stale)
        );
        assert!(!guard.observe(&returned, &delta("a")));
        assert!(guard
            .resolve(&after_reconnect, &reply("a", "current"))
            .is_ok());
    }

    #[test]
    fn another_guard_cannot_accept_a_same_session_same_serial_ticket() {
        let mut a = SnapshotGuard::default();
        let mut b = SnapshotGuard::default();
        let ca = a.attach("same");
        let cb = b.attach("same");
        let ta = a.begin(&ca).unwrap();
        let tb = b.begin(&cb).unwrap();
        assert!(!b.is_current(&ca));
        assert_eq!(
            b.resolve(&ta, &reply("same", "wrong")),
            Err(SnapshotError::Stale)
        );
        assert!(b.resolve(&tb, &reply("same", "right")).is_ok());
    }

    #[test]
    fn interleaved_events_require_a_new_request_instead_of_speculative_replay() {
        let mut guard = SnapshotGuard::default();
        let context = guard.attach("s1");
        let ticket = guard.begin(&context).unwrap();
        assert!(guard.observe(&context, &delta("s1")));
        assert_eq!(
            guard.resolve(&ticket, &reply("s1", "might include delta")),
            Err(SnapshotError::EventsDuringRequest)
        );
        assert_eq!(
            guard.resolve(&ticket, &reply("s1", "retry same ticket")),
            Err(SnapshotError::Stale)
        );
        let retry = guard.begin(&context).unwrap();
        assert!(guard.resolve(&retry, &reply("s1", "authoritative")).is_ok());
    }

    #[test]
    fn foreign_or_non_transcript_events_do_not_invalidate_the_load() {
        let mut guard = SnapshotGuard::default();
        let context = guard.attach("s1");
        let ticket = guard.begin(&context).unwrap();
        assert!(!guard.observe(&context, &delta("other")));
        assert!(!guard.observe(
            &context,
            &Event::RunStarted {
                session_id: "s1".into(),
                run_id: "r".into()
            }
        ));
        assert!(!guard.observe(
            &context,
            &Event::ReasoningDelta {
                session_id: "s1".into(),
                run_id: "r".into(),
                delta: "reasoning".into()
            }
        ));
        assert!(!guard.observe(
            &context,
            &dropped("s1", vec![DroppedEventKind::ToolProgress])
        ));
        assert!(guard.resolve(&ticket, &reply("s1", "current")).is_ok());
    }

    #[test]
    fn legacy_unknown_and_transcript_gap_warnings_are_conservatively_observed() {
        for event in [
            dropped("", vec![]),
            dropped("", vec![DroppedEventKind::Unknown]),
            dropped("s1", vec![DroppedEventKind::MessageDelta]),
            Event::MessagesEdited {
                session_id: "s1".into(),
            },
            Event::StreamRestarted {
                session_id: "s1".into(),
                run_id: "r".into(),
            },
        ] {
            let mut guard = SnapshotGuard::default();
            let context = guard.attach("s1");
            let ticket = guard.begin(&context).unwrap();
            assert!(guard.observe(&context, &event));
            assert_eq!(
                guard.resolve(&ticket, &reply("s1", "old")),
                Err(SnapshotError::EventsDuringRequest)
            );
        }
    }

    #[test]
    fn decode_failure_is_named_consumes_the_ticket_and_does_not_accept_another_session() {
        let mut guard = SnapshotGuard::default();
        let context = guard.attach("s1");
        let ticket = guard.begin(&context).unwrap();
        assert_eq!(
            guard.resolve(&ticket, &json!(null)),
            Err(SnapshotError::Decode(DecodeError::Malformed {
                field: "result",
                detail: "not an object".into()
            }))
        );
        assert_eq!(
            guard.resolve(&ticket, &reply("s1", "later")),
            Err(SnapshotError::Stale)
        );
        let retry = guard.begin(&context).unwrap();
        assert!(matches!(
            guard.resolve(&retry, &reply("other", "wrong")),
            Err(SnapshotError::Decode(DecodeError::WrongSession { .. }))
        ));
    }

    #[test]
    fn detach_and_cancel_reject_delayed_work_but_do_not_dirty_a_new_load() {
        let mut guard = SnapshotGuard::default();
        let context = guard.attach("s1");
        let cancelled = guard.begin(&context).unwrap();
        assert!(guard.cancel(&cancelled));
        assert!(!guard.cancel(&cancelled));
        assert_eq!(
            guard.resolve(&cancelled, &reply("s1", "old")),
            Err(SnapshotError::Stale)
        );
        let detached = guard.begin(&context).unwrap();
        guard.detach();
        assert!(!guard.observe(&context, &delta("s1")));
        assert_eq!(
            guard.resolve(&detached, &reply("s1", "old")),
            Err(SnapshotError::Stale)
        );
        let new_context = guard.attach("s1");
        let ticket = guard.begin(&new_context).unwrap();
        assert!(guard.resolve(&ticket, &reply("s1", "current")).is_ok());
    }

    #[test]
    fn rejecting_an_overlapping_reply_does_not_erase_live_text_or_clear_a_gap() {
        use crate::transcript::{ReloadReason, Transcript};
        let mut guard = SnapshotGuard::default();
        let context = guard.attach("s1");
        let initial = guard.begin(&context).unwrap();
        let mut transcript =
            Transcript::new(guard.resolve(&initial, &reply("s1", "question")).unwrap());
        let pending = guard.begin(&context).unwrap();
        let live = delta("s1");
        assert!(guard.observe(&context, &live));
        transcript.apply(&live);
        transcript.settle();
        assert_eq!(
            guard.resolve(&pending, &reply("s1", "older")),
            Err(SnapshotError::EventsDuringRequest)
        );
        assert_eq!(
            transcript.timeline().blocks().last().unwrap().markdown(),
            "new text"
        );
        let gap = dropped("s1", vec![]);
        assert!(guard.observe(&context, &gap)); // no outstanding request
        transcript.apply(&gap);
        assert_eq!(
            transcript.reload_needed(),
            Some(&ReloadReason::DeltasDropped)
        );
        let retry = guard.begin(&context).unwrap();
        let snapshot = guard.resolve(&retry, &reply("s1", "fresh")).unwrap();
        transcript.replace_snapshot(snapshot).unwrap();
        assert_eq!(transcript.reload_needed(), None);
        assert_eq!(transcript.timeline().blocks()[0].markdown(), "fresh");
    }

    #[test]
    fn request_counter_never_wraps_and_a_new_context_has_a_new_identity() {
        let mut guard = SnapshotGuard::default();
        let context = guard.attach("s1");
        let first = guard.begin(&context).unwrap();
        guard.serial = u64::MAX;
        assert_eq!(
            guard.begin(&context).unwrap_err(),
            SnapshotError::RequestCounterExhausted
        );
        let new_context = guard.attach("s1");
        let new_ticket = guard.begin(&new_context).unwrap();
        assert_eq!(
            guard.resolve(&first, &reply("s1", "old")),
            Err(SnapshotError::Stale)
        );
        assert!(guard.resolve(&new_ticket, &reply("s1", "current")).is_ok());
    }
}
