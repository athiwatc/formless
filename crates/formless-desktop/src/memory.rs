//! The scaffold's memory policy for text it holds on the user's behalf.
//!
//! Every place the shell keeps user text has a stated bound here:
//!
//! | Text | Bound | Enforced by |
//! |---|---|---|
//! | Active composer | [`limits::COMPOSER_MAX_BYTES`] | [`admit_paste`] before a paste, [`clamp_to_limit`] after any other edit |
//! | Stored drafts | [`limits::DRAFT_MAX_ENTRIES`] and [`limits::DRAFT_MAX_BYTES`] | `model::Drafts` |
//! | Composer undo history | the kit's 1,000 transactions, plus [`UndoBudget`] | the shell rebuilds the composer, which drops its history |
//! | Recorded intent | no text at all | [`IntentRecord`] keeps sizes only |
//! | A timeline row | [`limits::BLOCK_PREVIEW_MAX_BYTES`] and [`limits::BLOCK_PREVIEW_MAX_LINES`] laid out | `timeline::TimelineBlock` |
//!
//! Not covered: the transcript itself. How much history is loaded is a
//! protocol question (there is no pagination), not a view policy.

use formless_protocol::SendMode;

use crate::model::Intent;

pub mod limits {
    /// The most text the composer will hold. Larger content belongs in an
    /// attachment, not in a message typed or pasted into a text field.
    pub const COMPOSER_MAX_BYTES: usize = 1024 * 1024;
    pub const DRAFT_MAX_ENTRIES: usize = 64;
    pub const DRAFT_MAX_BYTES: usize = 4 * 1024 * 1024;
    /// Pasted text accepted into one composer before its undo history is
    /// dropped. The kit bounds undo by transaction count, not by size, so a
    /// few large pastes could otherwise pin many megabytes.
    pub const UNDO_PASTE_BUDGET_BYTES: usize = 4 * 1024 * 1024;
    /// How much of one timeline block is handed to the text layout.
    pub const BLOCK_PREVIEW_MAX_BYTES: usize = 32 * 1024;
    pub const BLOCK_PREVIEW_MAX_LINES: usize = 400;
}

// A draft that fits the composer always fits the draft store, so parking a
// draft on a Session switch can never be the thing that evicts it.
const _: () = assert!(limits::COMPOSER_MAX_BYTES <= limits::DRAFT_MAX_BYTES);

/// Whether a paste may go into the composer. A paste that would cross the
/// limit is refused whole: inserting part of a clipboard silently is worse
/// than inserting none of it and saying why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteAdmission {
    Accept,
    Reject { would_be_bytes: usize },
}

pub fn admit_paste(draft_bytes: usize, paste_bytes: usize) -> PasteAdmission {
    let would_be_bytes = draft_bytes.saturating_add(paste_bytes);
    if would_be_bytes > limits::COMPOSER_MAX_BYTES {
        PasteAdmission::Reject { would_be_bytes }
    } else {
        PasteAdmission::Accept
    }
}

/// The longest prefix of `text` that is at most `max_bytes` long and ends on
/// a character boundary.
pub fn clamp_to_limit(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Tracks pasted bytes accepted since the composer's undo history was last
/// dropped, and says when it is time to drop it again.
#[derive(Debug, Default)]
pub struct UndoBudget {
    accepted_paste_bytes: usize,
}

impl UndoBudget {
    pub fn record_paste(&mut self, bytes: usize) {
        self.accepted_paste_bytes = self.accepted_paste_bytes.saturating_add(bytes);
    }

    pub fn is_spent(&self) -> bool {
        self.accepted_paste_bytes > limits::UNDO_PASTE_BUDGET_BYTES
    }

    pub fn reset(&mut self) {
        self.accepted_paste_bytes = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    Send(SendMode),
    Stop,
}

/// What the shell keeps after handing an [`Intent`] on: enough to show what
/// happened, and none of the message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRecord {
    pub kind: IntentKind,
    pub session_id: String,
    pub content_bytes: usize,
    pub content_chars: usize,
}

impl From<&Intent> for IntentRecord {
    fn from(intent: &Intent) -> Self {
        match intent {
            Intent::SendMessage {
                session_id,
                content,
                mode,
            } => Self {
                kind: IntentKind::Send(*mode),
                session_id: session_id.clone(),
                content_bytes: content.len(),
                content_chars: content.chars().count(),
            },
            Intent::InterruptRun { session_id } => Self {
                kind: IntentKind::Stop,
                session_id: session_id.clone(),
                content_bytes: 0,
                content_chars: 0,
            },
        }
    }
}

/// A byte count for people: whole bytes, then KiB and MiB to one decimal.
pub fn format_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KIB {
        format!("{bytes} bytes")
    } else if value < KIB * KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{:.1} MiB", value / (KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_is_accepted_up_to_the_limit_and_refused_whole_beyond_it() {
        let max = limits::COMPOSER_MAX_BYTES;
        assert_eq!(admit_paste(0, max), PasteAdmission::Accept);
        assert_eq!(admit_paste(max - 10, 10), PasteAdmission::Accept);
        assert_eq!(
            admit_paste(max - 10, 11),
            PasteAdmission::Reject {
                would_be_bytes: max + 1
            }
        );
        assert_eq!(
            admit_paste(0, 5 * 1024 * 1024),
            PasteAdmission::Reject {
                would_be_bytes: 5 * 1024 * 1024
            }
        );
        // No overflow on absurd sizes.
        assert!(matches!(
            admit_paste(usize::MAX, usize::MAX),
            PasteAdmission::Reject { .. }
        ));
    }

    #[test]
    fn clamping_never_splits_a_character() {
        assert_eq!(clamp_to_limit("hello", 10), "hello");
        assert_eq!(clamp_to_limit("hello", 3), "hel");
        // Thai letters are three bytes each; a ZWJ emoji is many.
        assert_eq!(clamp_to_limit("กขค", 7), "กข");
        assert_eq!(clamp_to_limit("กขค", 2), "");
        let emoji = "a👩🏽‍💻";
        for max in 0..emoji.len() {
            let clamped = clamp_to_limit(emoji, max);
            assert!(clamped.len() <= max);
            assert!(emoji.starts_with(clamped));
        }
    }

    #[test]
    fn the_undo_budget_is_spent_only_after_enough_pasted_bytes() {
        let mut budget = UndoBudget::default();
        budget.record_paste(limits::UNDO_PASTE_BUDGET_BYTES);
        assert!(!budget.is_spent());
        budget.record_paste(1);
        assert!(budget.is_spent());
        budget.reset();
        assert!(!budget.is_spent());
    }

    #[test]
    fn an_intent_record_keeps_sizes_and_no_text() {
        let content = "สวัสดี 👩🏽‍💻".to_string();
        let record = IntentRecord::from(&Intent::SendMessage {
            session_id: "s1".to_string(),
            content: content.clone(),
            mode: SendMode::Queue,
        });
        assert_eq!(record.kind, IntentKind::Send(SendMode::Queue));
        assert_eq!(record.content_bytes, content.len());
        assert_eq!(record.content_chars, content.chars().count());
        // The record is a fixed-size summary plus the Session id.
        assert!(!format!("{record:?}").contains("สวัสดี"));

        let stop = IntentRecord::from(&Intent::InterruptRun {
            session_id: "s1".to_string(),
        });
        assert_eq!(stop.kind, IntentKind::Stop);
        assert_eq!(stop.content_bytes, 0);
    }

    #[test]
    fn byte_counts_read_naturally() {
        assert_eq!(format_bytes(812), "812 bytes");
        assert_eq!(format_bytes(32 * 1024), "32.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
