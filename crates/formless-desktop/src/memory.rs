//! The scaffold's memory policy for text it holds on the user's behalf.
//!
//! | Text | Bound | How it is kept |
//! |---|---|---|
//! | Active composer | [`limits::COMPOSER_MAX_BYTES`] for accepted text | An edit whose result is over the limit is taken back whole: the full previous draft and its selection are restored. Nothing is ever cut to fit. The kit cannot refuse a multi-line edit beforehand, so the text is over the limit for the instant between the edit and its rejection. |
//! | Stored drafts | [`limits::DRAFT_MAX_ENTRIES`] and [`limits::DRAFT_MAX_BYTES`], hard limits | `model::Drafts` evicts the oldest stored drafts and reports which, and the shell tells the user. |
//! | Composer undo history | An accounting limit, not a measured one: [`limits::UNDO_EDIT_BUDGET_BYTES`] of edited text, and the kit's own 1,000 transactions | [`UndoBudget`] adds up what every edit removed and inserted; when the sum passes the budget the shell rebuilds the composer, which is the only way to drop the kit's history. What the kit really retains per transaction was not measured. |
//! | Recorded intent | No text at all | [`IntentRecord`] keeps sizes only. |
//! | A timeline row | [`limits::BLOCK_PREVIEW_MAX_BYTES`] and [`limits::BLOCK_PREVIEW_MAX_LINES`] laid out | `timeline::TimelineBlock`. |
//!
//! Two limits on the composer bound are known and accepted:
//!
//! - Text an input method is still composing is not judged until the
//!   composition ends, so the composer can exceed the limit by the length of
//!   the composition.
//! - When a composition ends in place the kit gives no signal. The excess is
//!   then judged at the composer's next change of any kind, and until then
//!   it can be neither sent nor stored as a draft.
//!
//! Rejecting an edit normally keeps the undo history, because it is done
//! with the kit's own undo. When that does not restore exactly the previous
//! draft (the kit merges quick typing into one undo step; the composer may
//! not have focus), the previous draft is restored by rebuilding the
//! composer, and the history is lost. The user is told which happened.
//!
//! Not covered: the transcript itself (how much history is loaded is a
//! protocol question; there is no pagination), and whatever the kit keeps
//! internally beyond its undo history (layout caches, the redo stack).

use std::ops::Range;

use unicode_segmentation::GraphemeCursor;

use formless_protocol::SendMode;

use crate::model::Intent;

pub mod limits {
    /// The most text the composer will hold. Larger content belongs in an
    /// attachment, not in a message typed or pasted into a text field.
    pub const COMPOSER_MAX_BYTES: usize = 1024 * 1024;
    pub const DRAFT_MAX_ENTRIES: usize = 64;
    pub const DRAFT_MAX_BYTES: usize = 4 * 1024 * 1024;
    /// Bytes removed plus bytes inserted, summed over every edit to one
    /// composer, before its undo history is dropped. The kit bounds undo by
    /// transaction count, not by size, so a few large edits could otherwise
    /// pin many megabytes.
    pub const UNDO_EDIT_BUDGET_BYTES: usize = 4 * 1024 * 1024;
    /// How much of one timeline block is handed to the text layout.
    pub const BLOCK_PREVIEW_MAX_BYTES: usize = 32 * 1024;
    pub const BLOCK_PREVIEW_MAX_LINES: usize = 400;
}

// A draft that fits the composer always fits the draft store, so parking a
// draft on a Session switch can never be the thing that evicts it.
const _: () = assert!(limits::COMPOSER_MAX_BYTES <= limits::DRAFT_MAX_BYTES);

/// Whether a clipboard can be refused before it is inserted. Only a paste
/// that could not fit even if it replaced the entire draft is refused here;
/// anything smaller is inserted and then judged on the text that results, so
/// replacing a selection is accounted for exactly.
pub fn paste_can_never_fit(paste_bytes: usize) -> bool {
    paste_bytes > limits::COMPOSER_MAX_BYTES
}

/// Whether composer text is within the limit.
pub fn within_composer_limit(text: &str) -> bool {
    text.len() <= limits::COMPOSER_MAX_BYTES
}

/// Where one edit changed a text: `old[start..old_end]` became
/// `new[start..new_end]`. All three offsets are on character boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditSpan {
    pub start: usize,
    pub old_end: usize,
    pub new_end: usize,
}

impl EditSpan {
    pub fn removed_bytes(&self) -> usize {
        self.old_end - self.start
    }

    pub fn inserted_bytes(&self) -> usize {
        self.new_end - self.start
    }
}

/// The smallest single replacement that turns `old` into `new`: what is left
/// after their common prefix and common suffix. Where characters repeat the
/// replacement could sit at more than one place; this picks the last.
pub fn edit_span(old: &str, new: &str) -> EditSpan {
    let (old_bytes, new_bytes) = (old.as_bytes(), new.as_bytes());
    let mut start = old_bytes
        .iter()
        .zip(new_bytes)
        .take_while(|(a, b)| a == b)
        .count();
    // Two different characters can share leading bytes.
    while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
        start -= 1;
    }
    let mut suffix = old_bytes[start..]
        .iter()
        .rev()
        .zip(new_bytes[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    EditSpan {
        start,
        old_end: old.len() - suffix,
        new_end: new.len() - suffix,
    }
}

/// Widen `range` so both ends sit on grapheme boundaries of `text`: a
/// selection or caret restored from it never lands inside a character the
/// user sees as one (an emoji sequence, a letter with combining marks).
pub fn snap_to_graphemes(text: &str, range: Range<usize>) -> Range<usize> {
    let boundary = |offset: usize, forward: bool| -> usize {
        let offset = offset.min(text.len());
        let mut cursor = GraphemeCursor::new(offset, text.len(), true);
        if cursor.is_boundary(text, 0).unwrap_or(true) {
            return offset;
        }
        let found = if forward {
            cursor.next_boundary(text, 0)
        } else {
            cursor.prev_boundary(text, 0)
        };
        found.ok().flatten().unwrap_or(offset)
    };
    let start = boundary(range.start, false);
    let end = boundary(range.end.max(range.start), true);
    start..end.max(start)
}

/// Adds up the text every edit to one composer removed and inserted since
/// its undo history was last dropped, and says when to drop it again. This
/// is accounting, not measurement: it assumes a transaction holds what it
/// removed and what it inserted, and it counts an undone or redone edit
/// again, so it errs towards dropping the history early.
#[derive(Debug, Default)]
pub struct UndoBudget {
    edited_bytes: usize,
}

impl UndoBudget {
    pub fn record_edit(&mut self, span: EditSpan) {
        self.edited_bytes = self
            .edited_bytes
            .saturating_add(span.removed_bytes())
            .saturating_add(span.inserted_bytes());
    }

    pub fn is_spent(&self) -> bool {
        self.edited_bytes > limits::UNDO_EDIT_BUDGET_BYTES
    }

    pub fn reset(&mut self) {
        self.edited_bytes = 0;
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
    fn only_a_paste_that_can_never_fit_is_refused_before_insertion() {
        let max = limits::COMPOSER_MAX_BYTES;
        assert!(!paste_can_never_fit(0));
        // Exactly the limit could replace a fully selected draft.
        assert!(!paste_can_never_fit(max));
        assert!(paste_can_never_fit(max + 1));
        assert!(paste_can_never_fit(5 * 1024 * 1024));
        assert!(within_composer_limit(&"a".repeat(max)));
        assert!(!within_composer_limit(&"a".repeat(max + 1)));
    }

    #[test]
    fn an_edit_span_finds_an_insertion_in_the_middle_and_leaves_the_tail_alone() {
        let old = "head TAIL";
        let new = "head inserted TAIL";
        let span = edit_span(old, new);
        assert_eq!(&old[..span.start], "head ");
        assert_eq!(&old[span.old_end..], "TAIL");
        assert_eq!(&new[span.start..span.new_end], "inserted ");
        assert_eq!((span.removed_bytes(), span.inserted_bytes()), (0, 9));
    }

    #[test]
    fn an_edit_span_covers_replacement_deletion_and_no_change() {
        let replaced = edit_span("one two three", "one 2 three");
        assert_eq!(
            (replaced.start, replaced.old_end, replaced.new_end),
            (4, 7, 5)
        );
        let deleted = edit_span("abcdef", "abef");
        assert_eq!((deleted.removed_bytes(), deleted.inserted_bytes()), (2, 0));
        let same = edit_span("same", "same");
        assert_eq!((same.removed_bytes(), same.inserted_bytes()), (0, 0));
        let all = edit_span("", "new");
        assert_eq!((all.start, all.old_end, all.new_end), (0, 0, 3));
        // Repeated characters: any placement is a valid description.
        let repeated = edit_span("aaa", "aaaa");
        assert_eq!(
            (repeated.removed_bytes(), repeated.inserted_bytes()),
            (0, 1)
        );
    }

    #[test]
    fn an_edit_span_never_splits_a_character() {
        // ก and ข differ only in their last byte.
        let span = edit_span("xกy", "xขy");
        assert_eq!((span.start, span.old_end, span.new_end), (1, 4, 4));
        // Same trailing bytes, different leading byte.
        let span = edit_span("é", "ũ");
        assert_eq!((span.start, span.old_end, span.new_end), (0, 2, 2));
        for (old, new) in [("กขค", "กคค"), ("a👩b", "a👨b"), ("", "ก"), ("ก", "")]
        {
            let span = edit_span(old, new);
            assert!(old.is_char_boundary(span.start) && old.is_char_boundary(span.old_end));
            assert!(new.is_char_boundary(span.start) && new.is_char_boundary(span.new_end));
            assert_eq!(
                format!(
                    "{}{}{}",
                    &old[..span.start],
                    &new[span.start..span.new_end],
                    &old[span.old_end..]
                ),
                new
            );
        }
    }

    #[test]
    fn a_range_snaps_outwards_to_whole_graphemes() {
        // One grapheme of several code points: woman, skin tone, ZWJ, laptop.
        let emoji = "👩🏽‍💻";
        let text = format!("a{emoji}b");
        let end = 1 + emoji.len();
        for inside in 2..end {
            if text.is_char_boundary(inside) {
                assert_eq!(
                    snap_to_graphemes(&text, inside..inside),
                    1..end,
                    "caret at {inside}"
                );
            }
        }
        // Already on boundaries: unchanged.
        assert_eq!(snap_to_graphemes(&text, 1..end), 1..end);
        assert_eq!(snap_to_graphemes(&text, 0..0), 0..0);
        assert_eq!(
            snap_to_graphemes(&text, text.len()..text.len()),
            text.len()..text.len()
        );
        // A Thai consonant with a vowel and a tone mark is one grapheme.
        let thai = "ที่";
        assert_eq!(snap_to_graphemes(thai, 3..3), 0..thai.len());
        // Out of range is clamped, not a panic.
        assert_eq!(snap_to_graphemes("ab", 5..9), 2..2);
    }

    #[test]
    fn the_undo_budget_counts_every_edit_both_ways() {
        let mut budget = UndoBudget::default();
        let half = limits::UNDO_EDIT_BUDGET_BYTES / 2;
        // Replacing `half` bytes with `half` bytes costs the whole budget.
        budget.record_edit(EditSpan {
            start: 0,
            old_end: half,
            new_end: half,
        });
        assert!(!budget.is_spent());
        // One typed character more tips it.
        budget.record_edit(EditSpan {
            start: 0,
            old_end: 0,
            new_end: 1,
        });
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
