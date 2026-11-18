//! Typed, read-only decoding of Daemon Core replies into the presentation
//! model. No transport, no GPUI, no I/O.
//!
//! `ServerMessage::Response` carries its result as a `serde_json::Value`,
//! and what each reply holds is known today only to the TUI, which reads it
//! field by field (findings §5.2). This module states that knowledge for the
//! two replies the shell needs first, `ListSessions` and `GetSession`, and
//! turns them into what the shell already shows: `SessionInfo` for the
//! sidebar and [`TimelineBlock`]s for the timeline. Anything missing or of
//! the wrong shape is an error that says what, never a guess or an empty
//! default, because an empty list would be shown as "no Sessions" or "no
//! messages" when it is really "could not read".
//!
//! Role mapping follows the TUI: `user`, `tool` and `system-note` are their
//! own kinds and any other role is the assistant's. An assistant message
//! with no text (a turn that only called tools) is left out of the
//! timeline, as the TUI leaves it out of its transcript; it is counted.
//!
//! Only what the timeline and sidebar use is read. Pending events, queued
//! items and plans in a `GetSession` reply are not decoded here.

use std::fmt;

use formless_protocol::{MessageInfo, SessionInfo};

use crate::model::BlockKind;
use crate::timeline::TimelineBlock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// A field is absent or null.
    Missing(&'static str),
    /// A field is present with the wrong shape.
    Malformed { field: &'static str, detail: String },
    /// The reply is for another Session than the one asked for.
    WrongSession { asked: String, got: String },
    /// A message belongs to another Session than its envelope or snapshot.
    WrongMessageSession { asked: String, got: String },
    /// A message's sequence number cannot be a block id.
    NegativeSeq(i64),
    /// Two messages share a sequence number.
    DuplicateSeq(i64),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Missing(field) => write!(f, "the reply has no {field}"),
            DecodeError::Malformed { field, detail } => {
                write!(f, "the reply's {field} cannot be read: {detail}")
            }
            DecodeError::WrongSession { asked, got } => {
                write!(f, "asked for Session {asked}, the reply is for {got}")
            }
            DecodeError::WrongMessageSession { asked, got } => {
                write!(f, "message in Session {asked} belongs to Session {got}")
            }
            DecodeError::NegativeSeq(seq) => write!(f, "a message has sequence number {seq}"),
            DecodeError::DuplicateSeq(seq) => {
                write!(f, "two messages have sequence number {seq}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// A `GetSession` reply as the shell uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub session_id: String,
    /// Read-only Step identities sampled with this reply; absent on older daemons.
    pub steps: crate::steps::Inventory,
    /// Sampled Step-mode state; absent/malformed on older daemons remains unknown.
    pub step_mode: Option<bool>,
    /// Whether a Run is in progress, as the daemon says at reply time.
    pub running: bool,
    /// Live target sampled with the running flag. Older peers may omit it.
    pub steerable_run_id: Option<String>,
    /// Recovery state sampled by the daemon, never inferred from an old badge.
    pub unfinished_run: Option<UnfinishedRun>,
    /// The timeline, in sequence order; each block's id is its message's
    /// sequence number.
    pub blocks: Vec<TimelineBlock>,
    /// Assistant messages left out because they hold no text.
    pub hidden: usize,
    /// Highest persisted sequence, including assistant turns hidden above.
    /// Event reconciliation must not mistake those turns for new messages.
    pub last_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct UnfinishedRun {
    pub run_id: String,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
}

impl UnfinishedRun {
    pub fn recoverable(&self) -> bool {
        matches!(self.status.as_str(), "paused" | "interrupted")
    }
}

/// The block kind a message role is shown as. Matches the TUI's mapping.
pub fn block_kind(role: &str) -> BlockKind {
    match role {
        "user" => BlockKind::User,
        "tool" => BlockKind::ToolSummary,
        "system-note" => BlockKind::Note,
        _ => BlockKind::Assistant,
    }
}

/// The value under `key`, read as `T`; `name` is what an error calls it.
fn field<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    key: &str,
    name: &'static str,
) -> Result<T, DecodeError> {
    match value.get(key) {
        None | Some(serde_json::Value::Null) => Err(DecodeError::Missing(name)),
        Some(found) => {
            serde_json::from_value(found.clone()).map_err(|error| DecodeError::Malformed {
                field: name,
                detail: error.to_string(),
            })
        }
    }
}

/// A `ListSessions` result: every Session the daemon lists.
pub fn session_list(result: &serde_json::Value) -> Result<Vec<SessionInfo>, DecodeError> {
    if result.is_null() {
        return Err(DecodeError::Missing("session list"));
    }
    serde_json::from_value(result.clone()).map_err(|error| DecodeError::Malformed {
        field: "session list",
        detail: error.to_string(),
    })
}

/// A `GetSession` result for the Session `asked`. A reply for any other
/// Session is refused, so one Session's transcript is never shown as
/// another's.
pub fn session_snapshot(
    asked: &str,
    result: &serde_json::Value,
) -> Result<SessionSnapshot, DecodeError> {
    if !result.is_object() {
        return Err(DecodeError::Malformed {
            field: "result",
            detail: "not an object".into(),
        });
    }
    let session = result
        .get("session")
        .ok_or(DecodeError::Missing("session"))?;
    let session_id: String = field(session, "id", "session id")?;
    if session_id != asked {
        return Err(DecodeError::WrongSession {
            asked: asked.to_string(),
            got: session_id,
        });
    }
    let running: bool = field(result, "running", "running")?;
    let steerable_run_id = result["steerable_run_id"]
        .as_str()
        .filter(|id| running && formless_protocol::valid_reviewed_run_id(id))
        .map(str::to_string);
    let unfinished_run: Option<UnfinishedRun> = result
        .get("unfinished_run")
        .filter(|value| !value.is_null())
        .map(|_| field(result, "unfinished_run", "unfinished Run"))
        .transpose()?;
    if unfinished_run
        .as_ref()
        .is_some_and(|run| running || run.run_id.is_empty() || run.status.is_empty())
    {
        return Err(DecodeError::Malformed {
            field: "unfinished Run",
            detail: "missing identity/status or conflicts with a running Run".into(),
        });
    }
    let mut messages: Vec<MessageInfo> = field(result, "messages", "messages")?;
    messages.sort_by_key(|message| message.seq);
    let mut blocks = Vec::with_capacity(messages.len());
    let mut hidden = 0;
    let mut previous = None;
    for message in messages {
        if previous == Some(message.seq) {
            return Err(DecodeError::DuplicateSeq(message.seq));
        }
        previous = Some(message.seq);
        match message_block(asked, message)? {
            Some(block) => blocks.push(block),
            None => hidden += 1,
        }
    }
    Ok(SessionSnapshot {
        steps: crate::steps::Inventory::decode(asked, result.get("pending_steps")),
        step_mode: result.get("step_mode").and_then(serde_json::Value::as_bool),
        session_id,
        running,
        steerable_run_id,
        unfinished_run,
        blocks,
        hidden,
        last_seq: previous.map(|seq| seq as u64),
    })
}

/// Validate a persisted message's Session and sequence before allocating
/// its timeline block. Shared by snapshots and events.
pub(crate) fn message_sequence(asked: &str, message: &MessageInfo) -> Result<u64, DecodeError> {
    if message.session_id != asked {
        return Err(DecodeError::WrongMessageSession {
            asked: asked.to_string(),
            got: message.session_id.clone(),
        });
    }
    u64::try_from(message.seq).map_err(|_| DecodeError::NegativeSeq(message.seq))
}

fn message_block(asked: &str, message: MessageInfo) -> Result<Option<TimelineBlock>, DecodeError> {
    let id = message_sequence(asked, &message)?;
    let kind = block_kind(&message.role);
    if kind == BlockKind::Assistant && message.content.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(TimelineBlock::new(id, kind, message.content)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recovery_requires_a_valid_idle_snapshot_and_does_not_guess_unknown_statuses() {
        let mut value = json!({"session":{"id":"s1"}, "running":false, "messages":[],
            "unfinished_run":{"run_id":"r1","status":"interrupted","error":"stopped"}});
        let snapshot = session_snapshot("s1", &value).unwrap();
        assert!(snapshot.unfinished_run.unwrap().recoverable());
        value["running"] = json!(true);
        assert!(session_snapshot("s1", &value).is_err());
        value["running"] = json!(false);
        value["unfinished_run"]["status"] = json!("future-state");
        assert!(!session_snapshot("s1", &value)
            .unwrap()
            .unfinished_run
            .unwrap()
            .recoverable());
        value["unfinished_run"]["run_id"] = json!("");
        assert!(session_snapshot("s1", &value).is_err());
        value["unfinished_run"] = serde_json::Value::Null;
        assert!(session_snapshot("s1", &value)
            .unwrap()
            .unfinished_run
            .is_none());
    }

    fn message(seq: i64, role: &str, content: &str) -> MessageInfo {
        MessageInfo {
            id: format!("m{seq}"),
            session_id: "s1".into(),
            seq,
            role: role.into(),
            content: content.into(),
            created_at: "2026-11-14T00:00:00Z".into(),
            meta: None,
        }
    }

    /// A reply shaped like the daemon's, built from the protocol's own
    /// types, with the fields this module does not read present too.
    fn reply(session: &str, running: bool, messages: &[MessageInfo]) -> serde_json::Value {
        json!({
            "session": {"id": session, "name": "First"},
            "messages": messages,
            "running": running,
            "pending_events": [],
            "queued_items": [],
            "plan": [],
        })
    }

    #[test]
    fn a_transcript_becomes_blocks_in_sequence_order_with_the_tui_role_mapping() {
        let messages = [
            message(3, "assistant", "**Answer** ข้อความไทย"),
            message(1, "user", "Question 日本語"),
            message(2, "assistant", "  \n"),
            message(4, "tool", "exit 0"),
            message(5, "system-note", "Model changed"),
            message(6, "something-new", "shown as the assistant's"),
        ];
        let snapshot = session_snapshot("s1", &reply("s1", true, &messages)).unwrap();
        assert_eq!(snapshot.session_id, "s1");
        assert!(snapshot.running);
        assert_eq!(snapshot.hidden, 1);
        let shown: Vec<(u64, BlockKind, &str)> = snapshot
            .blocks
            .iter()
            .map(|block| (block.id, block.kind, block.markdown()))
            .collect();
        assert_eq!(
            shown,
            [
                (1, BlockKind::User, "Question 日本語"),
                (3, BlockKind::Assistant, "**Answer** ข้อความไทย"),
                (4, BlockKind::ToolSummary, "exit 0"),
                (5, BlockKind::Note, "Model changed"),
                (6, BlockKind::Assistant, "shown as the assistant's"),
            ]
        );
    }

    #[test]
    fn a_reply_for_another_session_is_refused() {
        let error = session_snapshot("s1", &reply("s2", false, &[])).unwrap_err();
        assert_eq!(
            error,
            DecodeError::WrongSession {
                asked: "s1".into(),
                got: "s2".into()
            }
        );
        assert_eq!(
            error.to_string(),
            "asked for Session s1, the reply is for s2"
        );
    }

    #[test]
    fn missing_or_malformed_fields_are_errors_not_empty_state() {
        let mut no_messages = reply("s1", false, &[]);
        no_messages.as_object_mut().unwrap().remove("messages");
        assert_eq!(
            session_snapshot("s1", &no_messages),
            Err(DecodeError::Missing("messages"))
        );
        let mut null_running = reply("s1", false, &[]);
        null_running["running"] = serde_json::Value::Null;
        assert_eq!(
            session_snapshot("s1", &null_running),
            Err(DecodeError::Missing("running"))
        );
        let mut bad_messages = reply("s1", false, &[]);
        bad_messages["messages"] = json!([{"seq": 1}]);
        assert!(matches!(
            session_snapshot("s1", &bad_messages),
            Err(DecodeError::Malformed {
                field: "messages",
                ..
            })
        ));
        assert_eq!(
            session_snapshot("s1", &json!({"messages": [], "running": false})),
            Err(DecodeError::Missing("session"))
        );
        assert_eq!(
            session_snapshot(
                "s1",
                &json!({"session": {}, "messages": [], "running": false})
            ),
            Err(DecodeError::Missing("session id"))
        );
        assert!(matches!(
            session_snapshot("s1", &json!([])),
            Err(DecodeError::Malformed {
                field: "result",
                ..
            })
        ));
    }

    #[test]
    fn sequence_numbers_must_be_usable_block_ids() {
        let negative = reply("s1", false, &[message(-1, "user", "x")]);
        assert_eq!(
            session_snapshot("s1", &negative),
            Err(DecodeError::NegativeSeq(-1))
        );
        let twice = reply(
            "s1",
            false,
            &[message(2, "user", "a"), message(2, "assistant", "b")],
        );
        assert_eq!(
            session_snapshot("s1", &twice),
            Err(DecodeError::DuplicateSeq(2))
        );
    }

    #[test]
    fn hidden_turns_still_advance_the_snapshot_sequence() {
        let snapshot =
            session_snapshot("s1", &reply("s1", false, &[message(7, "assistant", "")])).unwrap();
        assert_eq!(snapshot.last_seq, Some(7));
        assert!(snapshot.blocks.is_empty());
        assert_eq!(
            session_snapshot("s1", &reply("s1", false, &[]))
                .unwrap()
                .last_seq,
            None
        );
    }

    #[test]
    fn a_snapshot_cannot_carry_another_sessions_message() {
        let mut foreign = message(1, "user", "private to s2");
        foreign.session_id = "s2".into();
        assert_eq!(
            session_snapshot("s1", &reply("s1", false, &[foreign])),
            Err(DecodeError::WrongMessageSession {
                asked: "s1".into(),
                got: "s2".into()
            })
        );
    }

    #[test]
    fn an_empty_transcript_is_empty_not_an_error() {
        let snapshot = session_snapshot("s1", &reply("s1", false, &[])).unwrap();
        assert!(snapshot.blocks.is_empty());
        assert_eq!(snapshot.hidden, 0);
        assert!(!snapshot.running);
    }

    #[test]
    fn a_session_list_reads_the_protocol_type_and_refuses_anything_else() {
        let sessions = crate::fixture::sessions(3);
        let listed = session_list(&serde_json::to_value(&sessions).unwrap()).unwrap();
        assert_eq!(listed, sessions);
        assert_eq!(session_list(&json!([])).unwrap(), Vec::new());
        assert_eq!(
            session_list(&serde_json::Value::Null),
            Err(DecodeError::Missing("session list"))
        );
        assert!(matches!(
            session_list(&json!({"sessions": []})),
            Err(DecodeError::Malformed {
                field: "session list",
                ..
            })
        ));
        assert!(matches!(
            session_list(&json!([{"id": "s1"}])),
            Err(DecodeError::Malformed { .. })
        ));
    }
}
