//! Read-only transcript event reconciliation. No transport, GPUI or I/O.
//!
//! Start from a decoded `GetSession` snapshot, then apply the protocol's
//! ordered events. There is no MessageStarted event: the first nonempty
//! delta opens a provisional assistant row. A persisted assistant turn
//! (MessageAppended or MessageComplete) replaces it with canonical text,
//! never concatenates the final answer onto the deltas. Tools and notes
//! can arrive meanwhile without becoming part of that provisional row.
//!
//! Gaps are explicit and sticky until a new snapshot replaces the state.
//! This layer does not request that snapshot or alter Run/composer state.
//! `snapshot_guard` separately rejects retired local request/context tags
//! and observed snapshot/event overlap. Protocol ordering and recovery still
//! belong to the future client/transport; neither module is a replay protocol.

use formless_protocol::{DroppedEventKind, Event, MessageInfo};

use crate::model::BlockKind;
use crate::replies::{block_kind, message_sequence, DecodeError, SessionSnapshot};
use crate::timeline::{Timeline, TimelineBlock};

/// Shared relevance classification for transcript application and snapshot
/// ordering. This does not decode messages or prove that an event is valid.
/// An unscoped transcript/legacy/unknown drop warning affects every Session.
pub(crate) fn affects_transcript(event: &Event, session_id: &str) -> bool {
    if event.session_id().is_some_and(|id| id != session_id) {
        return false;
    }
    match event {
        Event::MessageAppended { .. }
        | Event::MessageDelta { .. }
        | Event::MessageComplete { .. }
        | Event::MessagesEdited { .. }
        | Event::StreamRestarted { .. } => true,
        Event::DeltasDropped { kinds, .. } => {
            kinds.is_empty()
                || kinds.iter().any(|kind| {
                    matches!(
                        kind,
                        DroppedEventKind::MessageDelta | DroppedEventKind::Unknown
                    )
                })
        }
        _ => false,
    }
}

/// Persisted sequences are nonnegative i64s, so this id cannot collide
/// with one. It lives only until the partial answer is replaced/discarded.
const LIVE_BLOCK_ID: u64 = u64::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadReason {
    CanonicalRefresh,
    MessagesEdited,
    DeltasDropped,
    StreamRestarted,
    InvalidMessage(DecodeError),
    /// A new sequence arrived behind the known transcript, or a known
    /// sequence was sent again with different text/kind.
    ConflictingSequence(u64),
    /// An event would join/finish the partial answer of a different Run.
    DifferentRun,
}

impl ReloadReason {
    /// User-facing state for a future view/client, not a silent gap.
    pub fn notice(&self) -> &'static str {
        match self {
            Self::CanonicalRefresh => "Live text paused; waiting for fresh stored messages.",
            Self::MessagesEdited => "The transcript was edited; reload this Session.",
            Self::DeltasDropped => "Some live text was missed; this transcript may be incomplete. Reload this Session.",
            Self::StreamRestarted => "The provider stream restarted; the partial answer was discarded. Reload this Session.",
            Self::InvalidMessage(_) | Self::ConflictingSequence(_) | Self::DifferentRun =>
                "Transcript updates could not be reconciled; reload this Session.",
        }
    }
}

/// What applying one event did. The client can coalesce view changes and
/// arrange a snapshot reload without mistaking an ignored event for one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Change {
    pub timeline_changed: bool,
    /// True only when the reload state first becomes set.
    pub reload_changed: bool,
}

#[derive(Debug)]
pub(crate) struct TranscriptState {
    session_id: String,
    last_seq: Option<u64>,
    stream_run: Option<String>,
    reload: Option<ReloadReason>,
}

impl TranscriptState {
    pub(crate) fn new(session_id: String, last_seq: Option<u64>) -> Self {
        Self {
            session_id,
            last_seq,
            stream_run: None,
            reload: None,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn reload_needed(&self) -> Option<&ReloadReason> {
        self.reload.as_ref()
    }

    fn need_reload(&mut self, reason: ReloadReason) -> Change {
        let reload_changed = self.reload.is_none();
        // Keep the first cause until the snapshot, instead of concealing
        // one gap when a later event has an apparently complete answer.
        self.reload.get_or_insert(reason);
        Change {
            reload_changed,
            ..Change::default()
        }
    }

    fn discard_stream(&mut self, timeline: &mut Timeline) -> bool {
        let had_stream = timeline.is_streaming();
        timeline.discard_stream();
        self.stream_run = None;
        had_stream
    }

    fn different_run(&self, run_id: &str) -> bool {
        self.stream_run.as_deref().is_some_and(|run| run != run_id)
    }

    fn persisted(
        &mut self,
        timeline: &mut Timeline,
        message: &MessageInfo,
        run: Option<&str>,
    ) -> Change {
        // Validate before changing the partial answer, sequence or timeline.
        let seq = match message_sequence(&self.session_id, message) {
            Ok(seq) => seq,
            Err(error) => return self.need_reload(ReloadReason::InvalidMessage(error)),
        };
        let kind = block_kind(&message.role);
        let attachments = if kind == BlockKind::User {
            crate::transcript_attachments::decode(message.meta.as_deref())
        } else {
            Vec::new()
        };
        let hidden = kind == BlockKind::Assistant && message.content.trim().is_empty();
        if self.last_seq.is_some_and(|last| seq <= last) {
            let old = timeline.block(seq);
            // Exact duplicate events are idempotent, including an old
            // completion delivered while a newer step is streaming.
            let same = match old {
                Some(old) => {
                    !hidden
                        && old.kind == kind
                        && old.markdown() == message.content
                        && old.attachments() == attachments
                }
                None => hidden, // empty assistant turns stay hidden
            };
            return if same {
                Change::default()
            } else {
                self.need_reload(ReloadReason::ConflictingSequence(seq))
            };
        }
        if run.is_some_and(|run| self.different_run(run)) {
            return self.need_reload(ReloadReason::DifferentRun);
        }
        let assistant = kind == BlockKind::Assistant;
        let mut changed = if assistant || run.is_some() {
            self.discard_stream(timeline)
        } else {
            false
        };
        self.last_seq = Some(seq);
        if !hidden {
            timeline.append_persisted(
                TimelineBlock::new(seq, kind, message.content.clone())
                    .with_attachments(attachments),
            );
            changed = true;
        }
        Change {
            timeline_changed: changed,
            ..Change::default()
        }
    }

    #[cfg(feature = "gui")]
    pub(crate) fn retire_live(&mut self, timeline: &mut Timeline) -> Change {
        let mut change = self.need_reload(ReloadReason::CanonicalRefresh);
        change.timeline_changed = self.discard_stream(timeline);
        change
    }

    pub(crate) fn apply(&mut self, timeline: &mut Timeline, event: &Event) -> Change {
        // Only an unscoped DeltasDropped warning can affect this Session
        // without naming it. Unrelated global/Run/security events do not.
        if !affects_transcript(event, &self.session_id) {
            return Change::default();
        }
        // A failed boundary is not a baseline for a later suffix. Once
        // reconciliation is lost, only an owned, trusted snapshot can resume
        // application; later completions cannot repair missing history.
        if self.reload.is_some() {
            return Change::default();
        }
        match event {
            Event::MessageAppended { message, .. } => self.persisted(timeline, message, None),
            Event::MessageComplete {
                run_id, message, ..
            } => self.persisted(timeline, message, Some(run_id)),
            Event::MessageDelta { run_id, delta, .. } => {
                if self.different_run(run_id) {
                    return self.need_reload(ReloadReason::DifferentRun);
                }
                if delta.is_empty() {
                    return Change::default();
                }
                if !timeline.is_streaming() {
                    timeline.begin_stream_with_id(LIVE_BLOCK_ID, BlockKind::Assistant);
                    self.stream_run = Some(run_id.clone());
                }
                timeline.push_delta(delta);
                Change {
                    timeline_changed: true,
                    ..Change::default()
                }
            }
            Event::MessagesEdited { .. } => {
                let mut change = self.need_reload(ReloadReason::MessagesEdited);
                change.timeline_changed = self.discard_stream(timeline);
                change
            }
            Event::StreamRestarted { run_id, .. } => {
                if self.different_run(run_id) {
                    return self.need_reload(ReloadReason::DifferentRun);
                }
                let mut change = self.need_reload(ReloadReason::StreamRestarted);
                change.timeline_changed = self.discard_stream(timeline);
                change
            }
            Event::DeltasDropped { run_id, .. } => {
                // An old Run's warning cannot erase this Run's partial
                // text. It still marks a gap in this Session's history.
                let discard = run_id.is_empty() || !self.different_run(run_id);
                let mut change = self.need_reload(ReloadReason::DeltasDropped);
                if discard {
                    change.timeline_changed = self.discard_stream(timeline);
                }
                change
            }
            _ => Change::default(),
        }
    }
}

/// Owned model for GUI-free use. The shell moves it into state + its
/// existing shared timeline without cloning the transcript. Only the reducer
/// mutates message content; preview-window moves remain view-only.
#[derive(Debug)]
pub struct Transcript {
    state: TranscriptState,
    timeline: Timeline,
}

impl Transcript {
    pub fn new(snapshot: SessionSnapshot) -> Self {
        Self {
            state: TranscriptState::new(snapshot.session_id, snapshot.last_seq),
            timeline: Timeline::new(snapshot.blocks),
        }
    }

    pub fn session_id(&self) -> &str {
        self.state.session_id()
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// Rebuild the bounded preview at a view's cadence, not on every delta.
    pub fn settle(&mut self) -> bool {
        self.timeline.settle()
    }

    pub fn reload_needed(&self) -> Option<&ReloadReason> {
        self.state.reload_needed()
    }

    /// A trusted replacement snapshot clears partial text and any gap. A
    /// future client must establish freshness and event ordering first.
    /// A delayed reply for another Session changes nothing.
    pub fn replace_snapshot(&mut self, snapshot: SessionSnapshot) -> Result<(), DecodeError> {
        if snapshot.session_id != self.session_id() {
            return Err(DecodeError::WrongSession {
                asked: self.session_id().to_string(),
                got: snapshot.session_id,
            });
        }
        *self = Self::new(snapshot);
        Ok(())
    }

    pub fn apply(&mut self, event: &Event) -> Change {
        self.state.apply(&mut self.timeline, event)
    }

    #[cfg(feature = "gui")]
    pub(crate) fn into_parts(self) -> (TranscriptState, Timeline) {
        (self.state, self.timeline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replies::session_snapshot;
    use serde_json::json;

    fn message(seq: i64, role: &str, content: &str) -> MessageInfo {
        MessageInfo {
            id: format!("m{seq}"),
            session_id: "s1".into(),
            seq,
            role: role.into(),
            content: content.into(),
            created_at: "now".into(),
            meta: None,
        }
    }

    fn snapshot(messages: &[MessageInfo]) -> SessionSnapshot {
        session_snapshot(
            "s1",
            &json!({"session": {"id": "s1"}, "running": true, "messages": messages}),
        )
        .unwrap()
    }

    fn transcript() -> Transcript {
        Transcript::new(snapshot(&[message(1, "user", "Question")]))
    }

    fn delta(run: &str, text: &str) -> Event {
        Event::MessageDelta {
            session_id: "s1".into(),
            run_id: run.into(),
            delta: text.into(),
        }
    }

    fn appended(seq: i64, role: &str, text: &str) -> Event {
        Event::MessageAppended {
            session_id: "s1".into(),
            message: message(seq, role, text),
        }
    }

    #[test]
    fn live_user_attachments_match_snapshot_projection_and_duplicates_check_metadata() {
        let mut transcript = transcript();
        let mut message = message(2, "user", "");
        message.meta = Some(
            json!({"attachments":[{"name":"sent.png","mime":"image/png","bytes":8},{"name":null}]})
                .to_string(),
        );
        let event = Event::MessageAppended {
            session_id: "s1".into(),
            message: message.clone(),
        };
        assert!(transcript.apply(&event).timeline_changed);
        let snapshot = snapshot(&[message.clone()]);
        assert_eq!(
            transcript.timeline().block(2).unwrap().attachments(),
            snapshot.blocks[0].attachments()
        );
        assert!(!transcript.apply(&event).timeline_changed);
        message.meta = Some(json!({"attachments":[{"name":"sent.png","mime":"image/png","bytes":8,"expired":true}]}).to_string());
        assert!(
            transcript
                .apply(&Event::MessageAppended {
                    session_id: "s1".into(),
                    message
                })
                .reload_changed
        );
    }

    fn complete(run: &str, seq: i64, text: &str) -> Event {
        Event::MessageComplete {
            session_id: "s1".into(),
            run_id: run.into(),
            message: message(seq, "assistant", text),
        }
    }

    fn shown(transcript: &Transcript) -> Vec<(u64, BlockKind, &str)> {
        transcript
            .timeline()
            .blocks()
            .iter()
            .map(|b| (b.id, b.kind, b.markdown()))
            .collect()
    }

    fn gap(session: &str, run: &str, kinds: Vec<DroppedEventKind>) -> Event {
        Event::DeltasDropped {
            session_id: session.into(),
            run_id: run.into(),
            kinds,
            handle: None,
            model: None,
        }
    }

    #[test]
    fn first_delta_starts_a_provisional_row_and_completion_replaces_it() {
        let mut t = transcript();
        assert_eq!(t.apply(&delta("r1", "")), Change::default());
        assert!(!t.timeline().is_streaming());
        for chunk in ["ข้อ", "ความไทย", " 日本語 🐈"] {
            assert!(t.apply(&delta("r1", chunk)).timeline_changed);
        }
        assert_eq!(t.timeline().stream_work().rebuilds, 0);
        t.settle();
        assert_eq!(t.timeline().stream_work().rebuilds, 1);
        assert_eq!(
            shown(&t)[1],
            (LIVE_BLOCK_ID, BlockKind::Assistant, "ข้อความไทย 日本語 🐈")
        );
        assert!(
            t.apply(&complete("r1", 2, "Canonical answer, not the prefix"))
                .timeline_changed
        );
        assert_eq!(
            shown(&t)[1],
            (2, BlockKind::Assistant, "Canonical answer, not the prefix")
        );
        assert!(!t.timeline().is_streaming());
        assert!(t.reload_needed().is_none());
    }

    #[test]
    fn a_persisted_assistant_step_replaces_live_text_before_the_next_tool_step() {
        let mut t = transcript();
        t.apply(&delta("r1", "partial"));
        t.apply(&appended(2, "assistant", "Before tool"));
        assert!(!t.timeline().is_streaming());
        t.apply(&appended(3, "tool", "exit 0"));
        t.apply(&delta("r1", "next partial"));
        t.apply(&complete("r1", 4, "After tool"));
        assert_eq!(
            shown(&t),
            [
                (1, BlockKind::User, "Question"),
                (2, BlockKind::Assistant, "Before tool"),
                (3, BlockKind::ToolSummary, "exit 0"),
                (4, BlockKind::Assistant, "After tool")
            ]
        );
    }

    #[test]
    fn interleaved_tools_and_notes_do_not_steal_the_stream() {
        let mut t = transcript();
        t.apply(&delta("r1", "A"));
        t.apply(&appended(2, "tool", "background tool"));
        t.apply(&appended(3, "system-note", "note"));
        t.apply(&delta("r1", "B"));
        assert_eq!(shown(&t)[3], (LIVE_BLOCK_ID, BlockKind::Assistant, "AB"));
        t.apply(&complete("r1", 4, "AB final"));
        assert_eq!(shown(&t)[3], (4, BlockKind::Assistant, "AB final"));
    }

    #[test]
    fn empty_assistant_turns_are_hidden_but_end_the_partial_answer() {
        let mut t = transcript();
        t.apply(&delta("r1", "discard me"));
        t.apply(&appended(2, "assistant", " \n"));
        assert_eq!(t.timeline().len(), 1);
        assert!(!t.timeline().is_streaming());
        assert_eq!(t.state.last_seq, Some(2));
        t.apply(&complete("r1", 3, "without any deltas"));
        assert_eq!(t.timeline().len(), 2);
    }

    #[test]
    fn exact_duplicates_do_not_duplicate_messages_or_erase_a_newer_partial() {
        let mut t = transcript();
        t.apply(&complete("r1", 2, "first answer"));
        t.apply(&delta("r1", "next step"));
        assert_eq!(
            t.apply(&complete("r1", 2, "first answer")),
            Change::default()
        );
        assert_eq!(t.apply(&appended(1, "user", "Question")), Change::default());
        assert_eq!(shown(&t)[2].2, "next step");
        assert!(t.reload_needed().is_none());
    }

    #[test]
    fn conflicting_and_out_of_order_messages_request_reload_without_mutation() {
        for event in [appended(1, "user", "edited"), appended(0, "user", "older")] {
            let mut t = transcript();
            t.apply(&delta("r1", "keep"));
            let before = shown(&t)
                .into_iter()
                .map(|(id, kind, text)| (id, kind, text.to_string()))
                .collect::<Vec<_>>();
            assert!(t.apply(&event).reload_changed);
            assert!(matches!(
                t.reload_needed(),
                Some(ReloadReason::ConflictingSequence(_))
            ));
            assert_eq!(
                shown(&t)
                    .into_iter()
                    .map(|(id, kind, text)| (id, kind, text.to_string()))
                    .collect::<Vec<_>>(),
                before
            );
        }
    }

    #[test]
    fn malformed_persisted_messages_do_not_replace_the_partial_answer() {
        let mut foreign = message(2, "assistant", "wrong Session");
        foreign.session_id = "s2".into();
        for message in [foreign, message(-1, "assistant", "negative")] {
            let mut t = transcript();
            t.apply(&delta("r1", "keep"));
            assert!(
                t.apply(&Event::MessageComplete {
                    session_id: "s1".into(),
                    run_id: "r1".into(),
                    message
                })
                .reload_changed
            );
            assert!(matches!(
                t.reload_needed(),
                Some(ReloadReason::InvalidMessage(_))
            ));
            assert_eq!(shown(&t)[1].2, "keep");
            assert_eq!(t.state.last_seq, Some(1));
        }
    }

    #[test]
    fn every_transcript_event_for_another_session_is_ignored() {
        let mut t = transcript();
        t.apply(&delta("r1", "keep"));
        let events = [
            delta("r1", "foreign"),
            complete("r1", 2, "foreign"),
            appended(3, "user", "foreign"),
            Event::MessagesEdited {
                session_id: "s1".into(),
            },
            Event::StreamRestarted {
                session_id: "s1".into(),
                run_id: "r1".into(),
            },
            gap("s1", "r1", vec![]),
        ];
        // Round trip through the protocol wire shape, not a second event model.
        for event in events {
            let mut value = serde_json::to_value(event).unwrap();
            value["session_id"] = json!("s2");
            let event = serde_json::from_value(value).unwrap();
            assert_eq!(t.apply(&event), Change::default());
        }
        assert_eq!(shown(&t)[1].2, "keep");
        assert!(t.reload_needed().is_none());
    }

    #[test]
    fn edits_and_restart_discard_partial_text_and_latch_reload_until_a_snapshot() {
        for (event, reason) in [
            (
                Event::MessagesEdited {
                    session_id: "s1".into(),
                },
                ReloadReason::MessagesEdited,
            ),
            (
                Event::StreamRestarted {
                    session_id: "s1".into(),
                    run_id: "r1".into(),
                },
                ReloadReason::StreamRestarted,
            ),
        ] {
            let mut t = transcript();
            t.apply(&delta("r1", "bad prefix"));
            assert_eq!(
                t.apply(&event),
                Change {
                    timeline_changed: true,
                    reload_changed: true
                }
            );
            assert_eq!(t.reload_needed(), Some(&reason));
            assert_eq!(t.timeline().len(), 1);
            assert!(!t.apply(&event).reload_changed);
            assert_eq!(t.apply(&delta("r1", "fresh")), Change::default());
            assert_eq!(t.timeline().len(), 1);
            assert_eq!(
                t.apply(&complete("r1", 2, "fresh final")),
                Change::default()
            );
            assert_eq!(t.reload_needed(), Some(&reason));
            t.replace_snapshot(snapshot(&[message(1, "user", "edited question")]))
                .unwrap();
            assert!(t.reload_needed().is_none());
            assert!(!t.timeline().is_streaming());
            assert_eq!(shown(&t), [(1, BlockKind::User, "edited question")]);
        }
    }

    #[test]
    fn legacy_global_unknown_and_message_delta_gaps_need_reload() {
        for event in [
            gap("s1", "r1", vec![]),
            gap("", "", vec![]),
            gap("s1", "r1", vec![DroppedEventKind::MessageDelta]),
            gap("", "", vec![DroppedEventKind::Unknown]),
        ] {
            let mut t = transcript();
            t.apply(&delta("r1", "incomplete"));
            assert_eq!(
                t.apply(&event),
                Change {
                    timeline_changed: true,
                    reload_changed: true
                }
            );
            assert_eq!(t.reload_needed(), Some(&ReloadReason::DeltasDropped));
            assert!(t.reload_needed().unwrap().notice().contains("incomplete"));
            assert_eq!(t.timeline().len(), 1);
            t.apply(&complete("r1", 2, "complete text"));
            assert!(t.reload_needed().is_some());
        }
    }

    #[test]
    fn a_latched_reconciliation_failure_freezes_all_later_events_until_snapshot() {
        for failure in [
            Event::MessagesEdited {
                session_id: "s1".into(),
            },
            Event::StreamRestarted {
                session_id: "s1".into(),
                run_id: "r1".into(),
            },
            gap("s1", "r1", vec![DroppedEventKind::MessageDelta]),
            complete("other-run", 2, "wrong Run"),
            appended(-1, "assistant", "invalid sequence"),
            appended(1, "user", "conflicting question"),
        ] {
            let mut t = transcript();
            t.apply(&delta("r1", "partial before failure"));
            assert!(t.apply(&failure).reload_changed);
            let reason = t.reload_needed().cloned().unwrap();
            let before = shown(&t)
                .into_iter()
                .map(|(id, kind, text)| (id, kind, text.to_owned()))
                .collect::<Vec<_>>();
            let sequence = t.state.last_seq;
            for event in [
                delta("r1", &"unsafe suffix".repeat(100_000)),
                appended(2, "tool", "untrusted boundary"),
                complete("r1", 3, "untrusted complete"),
                Event::MessagesEdited {
                    session_id: "s1".into(),
                },
                Event::StreamRestarted {
                    session_id: "s1".into(),
                    run_id: "r1".into(),
                },
                gap("", "", vec![]),
            ] {
                assert_eq!(
                    t.apply(&event),
                    Change::default(),
                    "a gap is not a new baseline"
                );
            }
            assert_eq!(t.reload_needed(), Some(&reason));
            assert_eq!(t.state.last_seq, sequence);
            assert_eq!(
                shown(&t)
                    .into_iter()
                    .map(|(id, kind, text)| (id, kind, text.to_owned()))
                    .collect::<Vec<_>>(),
                before
            );
            t.replace_snapshot(snapshot(&[message(1, "user", "trusted question")]))
                .unwrap();
            assert!(t.reload_needed().is_none());
            assert!(t.apply(&delta("r2", "fresh suffix")).timeline_changed);
            assert!(
                t.apply(&complete("r2", 2, "fresh canonical"))
                    .timeline_changed
            );
            assert_eq!(shown(&t)[1].2, "fresh canonical");
        }
    }

    #[test]
    fn nontranscript_drops_and_events_do_not_mark_a_transcript_gap() {
        let mut t = transcript();
        for event in [
            gap(
                "s1",
                "r1",
                vec![
                    DroppedEventKind::ToolProgress,
                    DroppedEventKind::RunUsage,
                    DroppedEventKind::InvocationActivity,
                    DroppedEventKind::ModelPullProgress,
                    DroppedEventKind::ReasoningDelta,
                ],
            ),
            Event::SettingsChanged,
            Event::RunStarted {
                session_id: "s1".into(),
                run_id: "r1".into(),
            },
            Event::ReasoningDelta {
                session_id: "s1".into(),
                run_id: "r1".into(),
                delta: "not answer text".into(),
            },
        ] {
            assert_eq!(t.apply(&event), Change::default());
        }
        assert_eq!(t.timeline().len(), 1);
        assert!(t.reload_needed().is_none());
    }

    #[test]
    fn different_runs_cannot_mix_or_finish_each_others_live_text() {
        for event in [
            delta("r2", "not r1"),
            complete("r2", 2, "not r1"),
            Event::StreamRestarted {
                session_id: "s1".into(),
                run_id: "r2".into(),
            },
        ] {
            let mut t = transcript();
            t.apply(&delta("r1", "keep r1"));
            assert!(t.apply(&event).reload_changed);
            assert_eq!(t.reload_needed(), Some(&ReloadReason::DifferentRun));
            assert_eq!(shown(&t)[1].2, "keep r1");
        }
        let mut t = transcript();
        t.apply(&delta("r1", "keep r1"));
        t.apply(&gap("s1", "r2", vec![]));
        assert_eq!(shown(&t)[1].2, "keep r1");
        assert!(t.reload_needed().is_some());
    }

    #[test]
    fn a_foreign_snapshot_is_refused_without_clearing_a_gap_or_partial() {
        let mut t = transcript();
        t.apply(&delta("r1", "keep"));
        // A different Run's gap latches reload without discarding this partial.
        t.apply(&gap("s1", "other-run", vec![]));
        assert_eq!(t.apply(&delta("r1", "ignored suffix")), Change::default());
        let mut snapshot = snapshot(&[]);
        snapshot.session_id = "s2".into();
        assert!(matches!(
            t.replace_snapshot(snapshot),
            Err(DecodeError::WrongSession { .. })
        ));
        assert_eq!(t.session_id(), "s1");
        assert!(t.reload_needed().is_some());
        assert_eq!(shown(&t)[1].2, "keep");
    }

    #[test]
    fn hidden_snapshot_turns_and_max_persisted_sequence_never_collide_with_live_id() {
        let mut t = Transcript::new(snapshot(&[message(i64::MAX, "assistant", "")]));
        assert_eq!(t.state.last_seq, Some(i64::MAX as u64));
        assert_eq!(
            t.apply(&appended(i64::MAX, "assistant", "")),
            Change::default()
        );
        t.apply(&delta("r1", "live"));
        assert_eq!(shown(&t), [(LIVE_BLOCK_ID, BlockKind::Assistant, "live")]);
    }
}
