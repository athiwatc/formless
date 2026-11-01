//! Bounded, editor-independent undo and kill-ring storage.
//!
//! Snapshots belong to the editor: in particular a collapsed paste's backing
//! text must be included, not just its visible placeholder. Limits cover
//! retained payload; the live draft itself is never truncated to fit history.
use std::collections::VecDeque;

const HISTORY_ENTRIES: usize = 128;
const HISTORY_BYTES: usize = 16 * 1024 * 1024;
const RING_ENTRIES: usize = 16;
const RING_BYTES: usize = 1024 * 1024;

pub(crate) trait Snapshot: Clone {
    fn retained_bytes(&self) -> usize;
}

#[derive(Debug, Clone)]
struct Entry<T> {
    value: T,
    bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct History<T> {
    undo: VecDeque<Entry<T>>,
    redo: VecDeque<Entry<T>>,
    bytes: usize,
    typing: bool,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            bytes: 0,
            typing: false,
        }
    }
}

impl<T: Snapshot> History<T> {
    pub(crate) fn coalesces(&self, typing: bool) -> bool {
        typing && self.typing
    }

    /// Call BEFORE a real mutation. Adjacent typing calls share one undo
    /// point; navigation, commands and programmatic replacements break it.
    pub(crate) fn before_edit(&mut self, current: &T, typing: bool) {
        for entry in self.redo.drain(..) {
            self.bytes -= entry.bytes;
        }
        if !typing || !self.typing {
            let bytes = current.retained_bytes();
            if bytes <= HISTORY_BYTES {
                self.undo.push_back(Entry {
                    value: current.clone(),
                    bytes,
                });
                self.bytes += bytes;
            } else {
                // Keeping older snapshots would jump across an unrecordable
                // edit, so make this an explicit history boundary instead.
                self.undo.clear();
                self.bytes = 0;
            }
        }
        self.typing = typing;
        self.trim();
    }

    pub(crate) fn break_group(&mut self) {
        self.typing = false;
    }

    pub(crate) fn undo(&mut self, current: &T) -> Option<T> {
        self.break_group();
        let entry = self.undo.pop_back()?;
        self.bytes -= entry.bytes;
        self.push_current(current, false);
        Some(entry.value)
    }

    pub(crate) fn redo(&mut self, current: &T) -> Option<T> {
        self.break_group();
        let entry = self.redo.pop_back()?;
        self.bytes -= entry.bytes;
        self.push_current(current, true);
        Some(entry.value)
    }

    fn push_current(&mut self, current: &T, undo: bool) {
        let bytes = current.retained_bytes();
        if bytes <= HISTORY_BYTES {
            let entry = Entry {
                value: current.clone(),
                bytes,
            };
            if undo {
                self.undo.push_back(entry);
            } else {
                self.redo.push_back(entry);
            }
            self.bytes += bytes;
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.bytes > HISTORY_BYTES || self.undo.len() + self.redo.len() > HISTORY_ENTRIES {
            // Preserve the next redo, rather than its furthest future state.
            let oldest = self.undo.pop_front().or_else(|| self.redo.pop_front());
            let Some(entry) = oldest else {
                break;
            };
            self.bytes -= entry.bytes;
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct KillRing {
    entries: VecDeque<String>,
    bytes: usize,
}

impl KillRing {
    /// An oversized kill still edits the draft, but must not leave a stale
    /// previous entry looking like the recoverable contents of that kill.
    pub(crate) fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if text.len() > RING_BYTES {
            self.entries.clear();
            self.bytes = 0;
            return;
        }
        if self.entries.front() == Some(&text) {
            return;
        }
        self.bytes += text.len();
        self.entries.push_front(text);
        while self.bytes > RING_BYTES || self.entries.len() > RING_ENTRIES {
            self.bytes -= self
                .entries
                .pop_back()
                .expect("nonempty over-budget ring")
                .len();
        }
    }

    pub(crate) fn kill(&mut self, text: String, prepend: bool, consecutive: bool) {
        if !consecutive || self.entries.is_empty() {
            self.push(text);
            return;
        }
        let previous = self.entries.pop_front().expect("nonempty ring");
        self.bytes -= previous.len();
        if previous.len().saturating_add(text.len()) > RING_BYTES {
            self.entries.clear();
            self.bytes = 0;
            return;
        }
        self.push(if prepend {
            text + &previous
        } else {
            previous + &text
        });
    }

    pub(crate) fn get(&self, index: usize) -> Option<&str> {
        if self.entries.is_empty() {
            None
        } else {
            Some(&self.entries[index % self.entries.len()])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone, Debug, PartialEq)]
    struct Draft(String, usize);
    impl Snapshot for Draft {
        fn retained_bytes(&self) -> usize {
            self.0.len()
        }
    }
    fn draft(s: &str, cursor: usize) -> Draft {
        Draft(s.into(), cursor)
    }

    #[test]
    fn typing_groups_restore_cursor_and_branching_discards_redo() {
        let mut h = History::default();
        h.before_edit(&draft("", 0), true);
        h.before_edit(&draft("é", 2), true);
        h.break_group();
        h.before_edit(&draft("é文", 2), true);
        assert_eq!(h.undo(&draft("é!文", 3)), Some(draft("é文", 2)));
        assert_eq!(h.undo(&draft("é文", 2)), Some(draft("", 0)));
        assert_eq!(h.redo(&draft("", 0)), Some(draft("é文", 2)));
        h.before_edit(&draft("é文", 2), false);
        assert_eq!(h.redo(&draft("new", 3)), None);
    }

    #[test]
    fn retained_count_and_payload_are_bounded_in_both_directions() {
        let mut h = History::default();
        for n in 0..300 {
            h.before_edit(&draft(&n.to_string(), 0), false);
        }
        assert_eq!(h.undo.len(), HISTORY_ENTRIES);
        for _ in 0..64 {
            h.undo(&draft("current", 0));
        }
        assert!(h.undo.len() + h.redo.len() <= HISTORY_ENTRIES);
        let huge = Draft("x".repeat(HISTORY_BYTES + 1), 0);
        h.before_edit(&huge, false);
        assert!(h.undo.is_empty() && h.redo.is_empty());
        assert_eq!(h.bytes, 0);
    }

    #[test]
    fn kill_ring_cycles_unicode_and_bounds_without_splitting_text() {
        let mut ring = KillRing::default();
        ring.push("ไทย👩🏽‍💻".into());
        ring.push("中文".into());
        assert_eq!(ring.get(0), Some("中文"));
        assert_eq!(ring.get(1), Some("ไทย👩🏽‍💻"));
        assert_eq!(ring.get(2), Some("中文"));
        for n in 0..30 {
            ring.push(n.to_string());
        }
        assert_eq!(ring.entries.len(), RING_ENTRIES);
        ring.push("x".repeat(RING_BYTES + 1));
        assert_eq!(ring.get(0), None);
    }
}
