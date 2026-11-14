//! The chat input (SPEC §10.2): a small multi-line editor with readline
//! keys, collapsed large pastes and per-Session history.

/// Pastes longer than this many lines (or `PASTE_COLLAPSE_CHARS` chars)
/// show as `[pasted N lines]` in the input; the full text is sent.
const PASTE_COLLAPSE_LINES: usize = 8;
const PASTE_COLLAPSE_CHARS: usize = 1500;

#[derive(Debug, Default, Clone)]
pub struct InputLine {
    /// Prompt-template/recalled message data has no slash-command authority.
    literal: bool,
    text: String,
    history: crate::text_history::History<EditSnapshot>,
    ring: crate::text_history::KillRing,
    anchor: Option<usize>,
    yank: Option<(usize, usize, usize, bool)>,
    last_kill: bool,
    /// Retained older cuts are available only by explicit Alt+Y after a
    /// failed retention, so ordinary Yank cannot impersonate the latest cut.
    cut_unavailable: bool,
    history_notice: Option<&'static str>,
    /// Byte offset, always on a char boundary.
    cursor: usize,
    /// Preferred display column for consecutive wrapped-row moves, by width.
    visual_goal: Option<(usize, usize)>,
    /// Preferred display column across logical-line vertical moves.
    logical_goal: Option<usize>,
    /// Placeholder shown in `text` -> the pasted text it stands for.
    pastes: Vec<(String, String)>,
    /// Browsing history: (index into the history, the draft to restore).
    browsing: Option<(usize, String, bool)>,
    /// Ctrl+R: the query and the history index it matched.
    search: Option<(String, Option<usize>)>,
}

#[derive(Debug, Clone)]
struct EditSnapshot {
    literal: bool,
    text: String,
    cursor: usize,
    pastes: Vec<(String, String)>,
    anchor: Option<usize>,
}
impl crate::text_history::Snapshot for EditSnapshot {
    fn retained_bytes(&self) -> usize {
        self.text
            .len()
            .saturating_add(self.pastes.iter().fold(0usize, |n, (p, t)| {
                n.saturating_add(p.len()).saturating_add(t.len())
            }))
    }
}

impl std::ops::Deref for InputLine {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

impl PartialEq<&str> for InputLine {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl From<&str> for InputLine {
    fn from(s: &str) -> Self {
        let mut i = Self::default();
        i.set(s);
        i.history = Default::default();
        i
    }
}

use crate::text;
use unicode_segmentation::UnicodeSegmentation;

#[path = "history_match.rs"]
mod history_match;

/// A cluster that is part of a word. Clusters of scripts written without
/// spaces (Thai, CJK) are words of their own, so a word step moves one
/// cluster there instead of across the whole sentence (#786).
fn is_word(g: &str) -> bool {
    g.chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn unspaced(g: &str) -> bool {
    g.chars().next().is_some_and(text::unspaced)
}

impl InputLine {
    /// Rendering must not clone retained undo or kill-ring payloads.
    pub(crate) fn projection(&self) -> Self {
        Self {
            literal: self.literal,
            text: self.text.clone(),
            cursor: self.cursor,
            pastes: self.pastes.clone(),
            anchor: self.anchor,
            cut_unavailable: self.cut_unavailable,
            history_notice: self.history_notice,
            ..Self::default()
        }
    }
    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            literal: self.literal,
            text: self.text.clone(),
            cursor: self.cursor,
            pastes: self.pastes.clone(),
            anchor: self.anchor,
        }
    }
    fn restore(&mut self, s: EditSnapshot) {
        self.visual_goal = None;
        self.logical_goal = None;
        self.literal = s.literal;
        self.text = s.text;
        self.cursor = s.cursor;
        self.pastes = s.pastes;
        self.anchor = s.anchor;
        self.browsing = None;
        self.search = None;
        self.yank = None;
        self.last_kill = false;
    }
    fn before_edit(&mut self, typing: bool) {
        self.visual_goal = None;
        self.logical_goal = None;
        self.history_notice = None;
        if !self.history.coalesces(typing) {
            let current = self.snapshot();
            self.history.before_edit(&current, typing);
        }
        self.yank = None;
        self.last_kill = false;
    }
    pub(crate) fn break_edit_group(&mut self) {
        self.visual_goal = None;
        self.logical_goal = None;
        self.history.break_group();
        self.yank = None;
        self.last_kill = false;
    }
    pub(crate) fn undo(&mut self) -> bool {
        let current = self.snapshot();
        match self.history.undo(&current) {
            crate::text_history::Restore::Value(s) => {
                self.restore(s);
                self.history_notice = None;
                true
            }
            crate::text_history::Restore::Empty => {
                self.history_notice = None;
                false
            }
            crate::text_history::Restore::BudgetRefused => {
                self.history_notice =
                    Some("Undo refused: current draft exceeds the 16 MiB history limit");
                false
            }
        }
    }
    pub(crate) fn redo(&mut self) -> bool {
        let current = self.snapshot();
        match self.history.redo(&current) {
            crate::text_history::Restore::Value(s) => {
                self.restore(s);
                self.history_notice = None;
                true
            }
            crate::text_history::Restore::Empty => {
                self.history_notice = None;
                false
            }
            crate::text_history::Restore::BudgetRefused => {
                self.history_notice =
                    Some("Redo refused: current draft exceeds the 16 MiB history limit");
                false
            }
        }
    }

    pub(crate) fn prepare_selection(&mut self, extend: bool) {
        self.modal_expand_pastes();
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
    }

    pub(crate) fn mouse_cursor(&mut self, at: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.set_modal_cursor(at);
        self.modal_expand_pastes();
        self.break_edit_group();
    }

    pub(crate) fn selection(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        self.selection()
            .map(|r| self.expanded_range(r.start, r.end))
    }

    pub(crate) fn cut_selection(&mut self) -> Option<String> {
        let r = self.selection()?;
        let text = self.expanded_range(r.start, r.end);
        self.kill_range(r.start, r.end);
        self.anchor = None;
        Some(text)
    }

    fn remove_selection(&mut self) -> bool {
        let Some(r) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.remove(r.start, r.end);
        self.anchor = None;
        true
    }

    /// Display-only selected spans, retaining the existing wrapped layout.
    pub(crate) fn styled_rows(
        &self,
        width: usize,
        top: usize,
        height: usize,
    ) -> Vec<Vec<(String, bool)>> {
        let (rows, _) = self.layout(width);
        let selection = self.selection();
        let mut offset = 0;
        rows.into_iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let at = offset;
                offset += row.len();
                if self.text.as_bytes().get(offset) == Some(&b'\n') {
                    offset += 1;
                }
                if i < top || i >= top.saturating_add(height) {
                    return None;
                }
                Some(selection_spans(&row, at, selection.as_ref()))
            })
            .collect()
    }

    pub(crate) fn cursor_byte(&self) -> usize {
        self.cursor
    }

    /// Modal editors use whole grapheme boundaries, not byte/char fragments.
    pub(crate) fn set_modal_cursor(&mut self, at: usize) {
        self.visual_goal = None;
        self.logical_goal = None;
        self.clamp_modal_cursor(at);
    }

    /// Normal-mode end clamping is normalization, not a new horizontal goal.
    pub(crate) fn clamp_modal_cursor(&mut self, at: usize) {
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(self.text.len()))
            .take_while(|&i| i <= at.min(self.text.len()))
            .last()
            .unwrap_or(0);
    }

    pub(crate) fn modal_replace(
        &mut self,
        range: std::ops::Range<usize>,
        replacement: &str,
    ) -> bool {
        let boundary =
            |at| at == self.text.len() || self.text.grapheme_indices(true).any(|(i, _)| i == at);
        if range.start > range.end
            || range.end > self.text.len()
            || !boundary(range.start)
            || !boundary(range.end)
        {
            return false;
        }
        self.before_edit(false);
        self.remove(range.start, range.end);
        self.cursor = range.start;
        self.insert_raw(replacement);
        self.search = None;
        true
    }

    /// Vim works on actual text. Expand paste placeholders before modal
    /// editing so a partial operator cannot strand or silently discard a paste.
    pub(crate) fn modal_expand_pastes(&mut self) {
        let mut expanded = false;
        for (placeholder, full) in std::mem::take(&mut self.pastes) {
            if let Some(start) = self.text.find(&placeholder) {
                let end = start + placeholder.len();
                self.anchor = self.anchor.map(|at| {
                    if at >= end {
                        at - placeholder.len() + full.len()
                    } else if at > start {
                        start + full.len()
                    } else {
                        at
                    }
                });
                self.cursor = if self.cursor >= end {
                    self.cursor - placeholder.len() + full.len()
                } else if self.cursor > start {
                    start + full.len()
                } else {
                    self.cursor
                };
                self.text.replace_range(start..end, &full);
                expanded = true;
            }
        }
        if expanded {
            self.visual_goal = None;
            self.logical_goal = None;
            self.snap_after_insert();
            self.anchor = self.anchor.map(|at| self.boundary_at_or_after(at));
        } else {
            self.clamp_modal_cursor(self.cursor);
        }
    }

    pub(crate) fn is_literal(&self) -> bool {
        self.literal
    }
    /// Intent is part of the next editing snapshot; marking an expansion does
    /// not add a separate text edit or split one undo operation.
    pub(crate) fn set_literal(&mut self, literal: bool) {
        self.literal = literal;
    }

    /// Replace the text, cursor at the end.
    pub fn set(&mut self, s: &str) {
        self.before_edit(false);
        self.literal = false;
        self.anchor = None;
        self.text = s.to_string();
        self.cursor = self.text.len();
        self.pastes.clear();
        self.browsing = None;
    }

    pub fn clear(&mut self) {
        self.set("");
    }

    /// The text to send (collapsed pastes expanded); clears the input.
    pub fn take(&mut self) -> String {
        let out = self.expanded();
        self.clear();
        self.history = Default::default();
        self.search = None;
        out
    }

    pub fn expanded(&self) -> String {
        let mut out = self.text.clone();
        for (placeholder, full) in &self.pastes {
            out = out.replacen(placeholder.as_str(), full, 1);
        }
        out
    }

    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    pub fn insert_char(&mut self, c: char) {
        self.before_edit(c != '\n' && self.selection().is_none());
        self.remove_selection();
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.snap_after_insert();
        self.browsing = None;
    }

    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.before_edit(false);
        self.remove_selection();
        self.insert_raw(s);
    }

    /// One Vim bracketed-paste operation, coalescing with insert keys and
    /// counted/dot replay while snapping only after the complete payload.
    pub(crate) fn insert_modal_paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.selection().is_some() {
            self.break_edit_group();
        }
        self.before_edit(true);
        self.remove_selection();
        self.insert_raw(text);
    }

    fn insert_raw(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.snap_after_insert();
        self.browsing = None;
    }

    /// Insertion can join neighbors on both sides. Keep the cursor after the
    /// completed full-text cluster, without altering the inserted text.
    fn snap_after_insert(&mut self) {
        self.cursor = self.boundary_at_or_after(self.cursor);
    }

    fn boundary_at_or_after(&self, at: usize) -> usize {
        let mut cursor = unicode_segmentation::GraphemeCursor::new(at, self.text.len(), true);
        match cursor.is_boundary(&self.text, 0) {
            Ok(true) => return at,
            Ok(false) => {
                if let Ok(Some(next)) = cursor.next_boundary(&self.text, 0) {
                    return next;
                }
            }
            Err(_) => {}
        }
        // Full text is normally sufficient for GraphemeCursor; retain a
        // boundary-safe fallback for any unexpected context request.
        self.text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(self.text.len()))
            .find(|&i| i >= at)
            .unwrap_or(self.text.len())
    }

    /// A bracketed paste: inserted as one block, never as Enter presses.
    /// Large ones collapse to a placeholder.
    pub fn paste(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.before_edit(false);
        self.remove_selection();
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let lines = s.lines().count();
        if lines > PASTE_COLLAPSE_LINES || s.chars().count() > PASTE_COLLAPSE_CHARS {
            let placeholder = format!("[pasted {lines} lines #{}]", self.pastes.len() + 1);
            self.insert_raw(&placeholder);
            self.pastes.push((placeholder, s));
        } else {
            self.insert_raw(&s);
        }
    }

    /// Cursor steps are whole grapheme clusters: a letter with its marks,
    /// an emoji sequence, a flag (#784).
    fn prev_boundary(&self, i: usize) -> usize {
        text::prev_cluster(&self.text, i)
    }

    fn next_boundary(&self, i: usize) -> usize {
        text::next_cluster(&self.text, i)
    }

    fn cluster_before(&self, i: usize) -> &str {
        &self.text[self.prev_boundary(i)..i]
    }

    fn cluster_after(&self, i: usize) -> &str {
        &self.text[i..self.next_boundary(i)]
    }

    pub fn left(&mut self) {
        self.break_edit_group();
        self.cursor = self.prev_boundary(self.cursor);
    }

    pub fn right(&mut self) {
        self.break_edit_group();
        self.cursor = self.next_boundary(self.cursor);
    }

    fn word_start_before(&self, mut i: usize) -> usize {
        while i > 0 && !is_word(self.cluster_before(i)) {
            i = self.prev_boundary(i);
        }
        if i > 0 && unspaced(self.cluster_before(i)) {
            return self.prev_boundary(i);
        }
        while i > 0 && is_word(self.cluster_before(i)) && !unspaced(self.cluster_before(i)) {
            i = self.prev_boundary(i);
        }
        i
    }

    fn word_end_after(&self, mut i: usize) -> usize {
        while i < self.text.len() && !is_word(self.cluster_after(i)) {
            i = self.next_boundary(i);
        }
        if i < self.text.len() && unspaced(self.cluster_after(i)) {
            return self.next_boundary(i);
        }
        while i < self.text.len()
            && is_word(self.cluster_after(i))
            && !unspaced(self.cluster_after(i))
        {
            i = self.next_boundary(i);
        }
        i
    }

    pub fn word_left(&mut self) {
        self.break_edit_group();
        self.cursor = self.word_start_before(self.cursor);
    }

    pub fn word_right(&mut self) {
        self.break_edit_group();
        self.cursor = self.word_end_after(self.cursor);
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    pub fn home(&mut self) {
        self.break_edit_group();
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.break_edit_group();
        self.cursor = self.line_end();
    }

    /// Remove `start..end`, dropping any collapsed paste it cut into.
    fn remove(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let mut start = start;
        let mut end = end;
        for (placeholder, _) in &self.pastes {
            if let Some(at) = self.text.find(placeholder) {
                if start < at + placeholder.len() && end > at {
                    start = start.min(at);
                    end = end.max(at + placeholder.len());
                }
            }
        }
        self.text.replace_range(start..end, "");
        self.cursor = start;
        let text = &self.text;
        self.pastes.retain(|(p, _)| text.contains(p.as_str()));
        self.browsing = None;
    }

    pub fn backspace(&mut self) {
        if self.selection().is_some() {
            self.before_edit(false);
            self.remove_selection();
            return;
        }
        if self.cursor == 0 {
            self.break_edit_group();
            return;
        }
        self.before_edit(false);
        // A collapsed paste goes as one unit.
        if let Some((p, _)) = self
            .pastes
            .iter()
            .find(|(p, _)| self.text[..self.cursor].ends_with(p.as_str()))
        {
            let start = self.cursor - p.len();
            self.remove(start, self.cursor);
            return;
        }
        let start = self.prev_boundary(self.cursor);
        self.remove(start, self.cursor);
    }

    pub fn delete(&mut self) {
        if self.selection().is_some() {
            self.before_edit(false);
            self.remove_selection();
            return;
        }
        if self.cursor == self.text.len() {
            self.break_edit_group();
            return;
        }
        self.before_edit(false);
        let end = self.next_boundary(self.cursor);
        self.remove(self.cursor, end);
    }

    /// Ctrl+W / Alt+Backspace.
    pub fn delete_word_back(&mut self) {
        let start = self.word_start_before(self.cursor);
        let end = self.cursor;
        self.kill_range(start, end);
    }

    /// Alt+D.
    pub fn delete_word_forward(&mut self) {
        let start = self.cursor;
        let end = self.word_end_after(self.cursor);
        self.kill_range(start, end);
    }

    /// Ctrl+U: to the start of the line.
    pub fn kill_to_start(&mut self) {
        let start = self.line_start();
        let end = self.cursor;
        self.kill_range(start, end);
    }

    /// Ctrl+K: to the end of the line.
    pub fn kill_to_end(&mut self) {
        let start = self.cursor;
        let end = self.line_end();
        self.kill_range(start, end);
    }

    fn expanded_range(&self, start: usize, end: usize) -> String {
        // Partial selection of a placeholder expands to that entire paste,
        // rather than copying an unusable fragment of its display label.
        let mut start = start;
        let mut end = end;
        for (p, _) in &self.pastes {
            if let Some(at) = self.text.find(p) {
                if start < at + p.len() && end > at {
                    start = start.min(at);
                    end = end.max(at + p.len());
                }
            }
        }
        let mut text = self.text[start..end].to_string();
        for (p, full) in &self.pastes {
            text = text.replacen(p, full, 1);
        }
        text
    }

    fn kill_range(&mut self, start: usize, end: usize) {
        if start == end {
            self.break_edit_group();
            return;
        }
        let consecutive = self.last_kill;
        let prepend = start < self.cursor;
        self.before_edit(false);
        let retained = self.ring.kill(
            self.expanded_range(start, end),
            prepend,
            consecutive,
            self.literal,
        );
        self.remove(start, end);
        self.last_kill = retained;
        self.cut_unavailable = !retained;
    }

    pub(crate) fn kill_notice(&self) -> Option<&'static str> {
        self.cut_unavailable
            .then_some("Cut unavailable for Yank (1 MiB limit) · Alt+Y older cuts")
    }

    pub(crate) fn edit_notice(&self) -> Option<&'static str> {
        self.history_notice.or_else(|| self.kill_notice())
    }

    pub(crate) fn yank(&mut self) -> bool {
        if self.cut_unavailable {
            return false;
        }
        let Some(text) = self.ring.get(0).map(str::to_owned) else {
            return false;
        };
        let base_literal = self.literal;
        self.before_edit(false);
        self.remove_selection();
        let start = self.cursor;
        self.insert_raw(&text);
        self.literal = base_literal || self.ring.literal(0);
        // Track inserted bytes, not a cursor snapped across a neighboring cluster.
        self.yank = Some((start, start + text.len(), 0, base_literal));
        true
    }

    pub(crate) fn cycle_yank(&mut self) -> bool {
        if self.cut_unavailable {
            if self.ring.get(0).is_none() {
                return false;
            }
            self.cut_unavailable = false;
            return self.yank();
        }
        let Some((start, end, index, base_literal)) = self.yank else {
            return false;
        };
        let Some(text) = self.ring.get(index + 1).map(str::to_owned) else {
            return false;
        };
        self.before_edit(false);
        self.remove(start, end);
        self.insert_raw(&text);
        self.literal = base_literal || self.ring.literal(index + 1);
        self.yank = Some((start, start + text.len(), index + 1, base_literal));
        true
    }

    pub fn newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn on_first_line(&self) -> bool {
        !self.text[..self.cursor].contains('\n')
    }

    pub fn on_last_line(&self) -> bool {
        !self.text[self.cursor..].contains('\n')
    }

    /// Up/Down inside a multi-line input, keeping the screen column.
    pub fn vertical(&mut self, down: bool) {
        let col = self
            .logical_goal
            .unwrap_or_else(|| text::width(&self.text[self.line_start()..self.cursor]));
        self.break_edit_group();
        self.logical_goal = Some(col);
        let target = if down {
            if self.on_last_line() {
                return;
            }
            self.line_end() + 1
        } else {
            if self.on_first_line() {
                return;
            }
            let prev_end = self.line_start() - 1;
            self.text[..prev_end].rfind('\n').map_or(0, |i| i + 1)
        };
        let line_end = self.text[target..]
            .find('\n')
            .map_or(self.text.len(), |i| target + i);
        let mut c = target;
        while c < line_end {
            let next = self.next_boundary(c);
            if text::width(&self.text[target..next]) > col {
                break;
            }
            c = next;
        }
        self.cursor = c;
    }

    /// Up through `history` (oldest first), keeping the draft.
    pub fn history_prev(&mut self, history: &[impl AsRef<str>]) {
        if history.is_empty() {
            self.browsing = None;
            return;
        }
        let (idx, draft, literal) = match self.browsing.take() {
            Some((i, _, _)) if history.get(i).is_none() => return,
            Some((0, d, literal)) => (0, d, literal),
            Some((i, d, literal)) => (i - 1, d, literal),
            None => (history.len() - 1, self.expanded(), self.literal),
        };
        let Some(entry) = history.get(idx) else {
            return;
        };
        self.set(entry.as_ref());
        self.literal = true;
        self.browsing = Some((idx, draft, literal));
    }

    /// Down back towards the draft.
    pub fn history_next(&mut self, history: &[impl AsRef<str>]) {
        let Some((idx, draft, literal)) = self.browsing.take() else {
            return;
        };
        if history.get(idx).is_none() {
            return;
        }
        if let Some(next) = idx.checked_add(1).and_then(|i| history.get(i)) {
            self.set(next.as_ref());
            self.literal = true;
            self.browsing = Some((idx + 1, draft, literal));
        } else {
            self.set(&draft);
            self.literal = literal;
        }
    }

    // --- Ctrl+R history search -------------------------------------------

    pub fn searching(&self) -> Option<(&str, Option<usize>)> {
        self.search.as_ref().map(|(q, m)| (q.as_str(), *m))
    }

    /// Start a search, or (when one is open) look further back.
    pub fn search_older(&mut self, history: &[impl AsRef<str>]) {
        match &mut self.search {
            None => self.search = Some((String::new(), None)),
            Some((q, m)) => {
                let before = m.unwrap_or(history.len());
                if let Some(i) = find_back(history, q, before) {
                    *m = Some(i);
                }
            }
        }
    }

    pub fn search_type(&mut self, c: Option<char>, history: &[impl AsRef<str>]) {
        if let Some((q, m)) = &mut self.search {
            match c {
                Some(c) => q.push(c),
                None => {
                    q.pop();
                }
            }
            *m = find_back(history, q, history.len());
        }
    }

    /// Enter: take the match into the input.
    pub fn search_accept(&mut self, history: &[impl AsRef<str>]) {
        if let Some((_, Some(i))) = self.search.take() {
            if let Some(entry) = history.get(i) {
                self.set(entry.as_ref());
                self.literal = true;
            }
        }
    }

    pub fn search_cancel(&mut self) {
        self.search = None;
    }

    /// The history bank changed. Keep the draft/query, but never accept an
    /// index from its previous ordering. Enter cancels this invalidated search
    /// instead of accidentally submitting the draft as a new message.
    pub(crate) fn invalidate_history(&mut self) {
        self.browsing = None;
        if let Some((_, found)) = &mut self.search {
            *found = None;
        }
    }

    /// The rows to draw for `width` columns, and the cursor's (row, col),
    /// both in screen columns: a wide CJK character takes two, a Thai mark
    /// none (#785).
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let mut rows = Vec::new();
        let cursor = self.walk_layout(width, |row| rows.push(row.to_owned()), |_, _| {});
        (rows, cursor)
    }

    /// Move through exactly the grapheme rows used by rendering. False means
    /// there is no adjacent visual row, so ordinary arrows may browse history.
    pub(crate) fn vertical_wrapped(&mut self, down: bool, width: usize) -> bool {
        let width = width.max(1);
        let mut points = Vec::new();
        let cursor = self.walk_layout(
            width,
            |_| {},
            |byte, position| points.push((byte, position)),
        );
        let goal = self
            .visual_goal
            .filter(|(previous, _)| *previous == width)
            .map_or(cursor.1, |(_, column)| column);
        self.break_edit_group();
        self.visual_goal = Some((width, goal));
        let target = if down {
            cursor.0.checked_add(1)
        } else {
            cursor.0.checked_sub(1)
        };
        let Some(target) = target else {
            return false;
        };
        let Some((byte, _)) = points
            .into_iter()
            .rev()
            .find(|(_, (row, column))| *row == target && *column <= goal)
        else {
            return false;
        };
        self.cursor = byte;
        true
    }

    fn walk_layout(
        &self,
        width: usize,
        mut emit_row: impl FnMut(&str),
        mut boundary: impl FnMut(usize, (usize, usize)),
    ) -> (usize, usize) {
        let width = width.max(1);
        let mut row_index = 0;
        let mut cursor = (0, 0);
        let mut offset = 0;
        for line in self.text.split('\n') {
            let mut row_start = 0;
            let mut col = 0;
            for (i, g) in line.grapheme_indices(true) {
                let w = text::width(g);
                if col + w > width && col > 0 {
                    emit_row(&line[row_start..i]);
                    row_index += 1;
                    row_start = i;
                    col = 0;
                }
                boundary(offset + i, (row_index, col));
                if offset + i == self.cursor {
                    cursor = (row_index, col);
                }
                col += w;
            }
            if offset + line.len() == self.cursor {
                if col == width {
                    emit_row(&line[row_start..]);
                    row_index += 1;
                    row_start = line.len();
                    col = 0;
                }
                cursor = (row_index, col);
            }
            // A full row's trailing boundary belongs to the next wrapped row;
            // only a drawn end-of-input caret row may own it here.
            if col < width || offset + line.len() == self.cursor {
                boundary(offset + line.len(), (row_index, col));
            }
            emit_row(&line[row_start..]);
            row_index += 1;
            offset += line.len() + 1;
        }
        cursor
    }
}

/// A single contiguous selection needs at most three spans per visible row,
/// rather than one heap allocation per grapheme of a very long line.
pub(crate) fn selection_spans(
    row: &str,
    offset: usize,
    selection: Option<&std::ops::Range<usize>>,
) -> Vec<(String, bool)> {
    let mut spans: Vec<(String, bool)> = Vec::new();
    for (at, g) in row.grapheme_indices(true) {
        let selected = selection.is_some_and(|r| r.contains(&(offset + at)));
        if let Some((text, _)) = spans
            .last_mut()
            .filter(|(_, previous)| *previous == selected)
        {
            text.push_str(g);
        } else {
            spans.push((g.to_owned(), selected));
        }
    }
    spans
}

fn find_back(history: &[impl AsRef<str>], query: &str, before: usize) -> Option<usize> {
    let matcher = history_match::Matcher::new(query);
    history[..before.min(history.len())]
        .iter()
        .rposition(|h| matcher.contains(h.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_joining_insertions_keep_full_grapheme_boundaries() {
        for (before, at_byte, inserted, after, cursor) in [
            ("👩💻x", "👩".len(), '\u{200d}', "👩‍💻x", "👩‍💻".len()),
            ("🇦🇧🇨x", "🇦🇧".len(), '🇩', "🇦🇧🇩🇨x", "🇦🇧🇩🇨".len()),
            ("abx", 1, '\u{301}', "a\u{301}bx", "a\u{301}".len()),
            ("abx", 1, '\u{600}', "a\u{600}bx", "a\u{600}b".len()),
        ] {
            for mode in 0..4 {
                let mut line = at(before, at_byte);
                let text = inserted.to_string();
                match mode {
                    0 => line.insert_char(inserted),
                    1 => line.insert_str(&text),
                    2 => line.paste(&text),
                    _ => {
                        assert!(line.modal_replace(at_byte..at_byte, &text));
                    }
                }
                assert_eq!(line.expanded(), after);
                assert_eq!(line.cursor_byte(), cursor, "{before:?} mode={mode}");
                let mut back = line.clone();
                back.backspace();
                assert!(
                    back.cursor_byte() == back.len()
                        || back
                            .grapheme_indices(true)
                            .any(|(at, _)| at == back.cursor_byte())
                );
                let mut forward = line.clone();
                forward.delete();
                assert!(
                    forward.cursor_byte() == forward.len()
                        || forward
                            .grapheme_indices(true)
                            .any(|(at, _)| at == forward.cursor_byte())
                );
                assert!(line.undo());
                assert_eq!(line.expanded(), before);
                assert_eq!(line.cursor_byte(), at_byte);
                assert!(line.redo());
                assert_eq!(line.expanded(), after);
                assert_eq!(line.cursor_byte(), cursor);
            }
        }
        let mut line = at("👩💻x", "👩".len());
        line.insert_char('\u{200d}');
        let mut forward = line.clone();
        forward.delete();
        assert_eq!(forward.expanded(), "👩‍💻");
        line.backspace();
        assert_eq!(line.expanded(), "x");
    }

    #[test]
    fn composer_logical_vertical_goal_survives_short_wide_and_empty_lines() {
        for (text, goal) in [
            ("abcd\nx\nabcd", 3),
            ("ab\n界界\nab", 1),
            ("abcd\n\nabcd", 3),
        ] {
            let last = text.rfind('\n').unwrap() + 1;
            let mut line = at(text, goal);
            for _ in 0..2 {
                line.prepare_selection(false);
                line.vertical(true);
            }
            assert_eq!(line.cursor_byte(), last + goal, "{text:?}");
            for _ in 0..2 {
                line.prepare_selection(false);
                line.vertical(false);
            }
            assert_eq!(line.cursor_byte(), goal, "{text:?}");
        }
        let mut line = at("abcd\nx\nabcd", 3);
        line.vertical(true);
        line.left();
        line.vertical(true);
        assert_eq!(line.cursor_byte(), 7);
        line.set("abcd\nx\nabcd");
        line.set_modal_cursor(3);
        line.vertical(true);
        line.insert_char('y');
        line.vertical(true);
        assert_eq!(line.cursor_byte(), 10);
    }

    #[test]
    fn composer_joining_yank_cycle_preserves_neighboring_text() {
        let mut line = at("👩💻", "👩".len());
        line.ring.kill("X".into(), false, false, false);
        line.ring.kill("\u{200d}".into(), false, false, false);
        assert!(line.yank());
        assert_eq!(line.expanded(), "👩‍💻");
        assert_eq!(line.cursor_byte(), "👩‍💻".len());
        assert!(line.cycle_yank());
        assert_eq!(line.expanded(), "👩X💻");
        assert_eq!(line.cursor_byte(), "👩X".len());
    }

    #[test]
    fn history_search_accept_after_shrink_preserves_the_draft() {
        let mut line = InputLine::from("private unsent draft");
        let old = vec!["first".to_string(), "last match".to_string()];
        line.search_older(&old);
        line.search_type(Some('m'), &old);
        assert_eq!(line.searching(), Some(("m", Some(1))));
        line.search_accept(&["shorter".to_string()]);
        assert_eq!(line.expanded(), "private unsent draft");
    }
    #[test]
    fn history_browse_after_shrink_never_indexes_a_stale_entry() {
        let mut line = InputLine::from("draft");
        let old = vec![
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ];
        line.history_prev(&old);
        assert_eq!(line.expanded(), "third");
        line.history_prev(&["shorter".to_string()]);
        assert_eq!(line.expanded(), "third");
    }
    #[test]
    fn history_next_after_shrink_cancels_the_stale_browse_without_replacing_text() {
        let mut line = InputLine::from("draft");
        let old = ["first", "second", "third"];
        line.history_prev(&old);
        line.history_next(&["shorter"]);
        assert_eq!(line.expanded(), "third");
        assert!(line.browsing.is_none());
    }

    #[test]
    fn undo_restores_collapsed_paste_backing_and_cursor() {
        let mut i = InputLine::from("前");
        let paste = "long line\n".repeat(20);
        i.paste(&paste);
        let shown = i.to_string();
        let cursor = i.cursor;
        i.backspace();
        assert_eq!(i.expanded(), "前");
        assert!(i.undo());
        assert_eq!(i.to_string(), shown);
        assert_eq!(i.cursor, cursor);
        assert_eq!(i.expanded(), format!("前{paste}"));
        assert!(i.undo());
        assert_eq!(i.expanded(), "前");
        assert!(i.redo());
        assert_eq!(i.expanded(), format!("前{paste}"));
    }

    #[test]
    fn killed_unicode_can_be_yanked_cycled_and_undone() {
        let mut i = InputLine::from("ไทย hello");
        i.delete_word_back();
        i.left();
        i.right();
        i.kill_to_start();
        assert_eq!(i.expanded(), "");
        assert!(i.yank());
        assert_eq!(i.expanded(), "ไทย ");
        assert!(i.cycle_yank());
        assert_eq!(i.expanded(), "hello");
        assert!(i.undo());
        assert_eq!(i.expanded(), "ไทย ");
        i.left();
        assert!(!i.cycle_yank(), "navigation ends the yank replacement");
    }

    #[test]
    fn history_browsing_is_undoable_and_send_ends_the_draft_history() {
        let mut i = InputLine::from("draft");
        i.left();
        let cursor = i.cursor;
        i.history_prev(&["old"]);
        assert!(i.undo());
        assert_eq!(i.expanded(), "draft");
        assert_eq!(i.cursor, cursor);
        assert_eq!(i.take(), "draft");
        assert!(!i.undo(), "a new message cannot restore the sent draft");
    }

    #[test]
    fn selection_expands_pastes_and_replaces_whole_graphemes_undoably() {
        let mut i = InputLine::from("前");
        let paste = "ไทย\n".repeat(20);
        i.paste(&paste);
        i.prepare_selection(true);
        i.left();
        i.home();
        assert_eq!(i.selected_text(), Some("ไทย\n".into()));
        let selected = i.selected_text().unwrap();
        assert_eq!(i.cut_selection(), Some(selected));
        assert!(i.undo());
        assert_eq!(i.expanded(), format!("前{paste}"));
        let mut i = InputLine::from("a👩🏽‍💻文");
        i.prepare_selection(true);
        i.left();
        i.left();
        assert_eq!(i.selected_text(), Some("👩🏽‍💻文".into()));
        i.insert_char('é');
        assert_eq!(i.expanded(), "aé");
        assert!(i.undo());
        assert_eq!(i.selected_text(), Some("👩🏽‍💻文".into()));
    }

    #[test]
    fn consecutive_backward_kills_prepend_in_original_text_order() {
        let mut i = InputLine::from("one two three");
        i.delete_word_back();
        i.delete_word_back();
        assert_eq!(i.expanded(), "one ");
        assert!(i.yank());
        assert_eq!(i.expanded(), "one two three");
    }

    #[test]
    fn display_projection_does_not_copy_or_consume_undo_ownership() {
        let mut i = InputLine::from("base");
        i.insert_char('é');
        i.insert_char('文');
        let mut display = i.projection();
        assert_eq!(display.expanded(), "baseé文");
        assert!(!display.undo(), "render projection owns no history");
        assert!(i.undo());
        assert_eq!(i.expanded(), "base");
    }

    #[test]
    fn killing_inside_a_collapsed_label_keeps_no_orphan_and_is_recoverable() {
        let mut i = InputLine::from("before ");
        let paste = "private long text\n".repeat(20);
        i.paste(&paste);
        i.delete_word_back();
        assert_eq!(i.expanded(), "before ");
        assert!(i.yank());
        assert_eq!(i.expanded(), format!("before {paste}"));
        assert!(i.undo());
        assert!(i.undo());
        assert_eq!(i.expanded(), format!("before {paste}"));
        assert_eq!(i.pastes.len(), 1);
    }

    #[test]
    fn selection_rendering_groups_long_lines_and_only_styles_visible_rows() {
        let row = "é".repeat(20_000);
        let selected = 200..400;
        let spans = selection_spans(&row, 0, Some(&selected));
        assert_eq!(
            spans.len(),
            3,
            "long lines do not allocate one span per grapheme"
        );
        assert_eq!(
            spans.iter().map(|(s, _)| s.as_str()).collect::<String>(),
            row
        );
        assert_eq!(
            spans
                .iter()
                .filter(|(_, selected)| *selected)
                .map(|(s, _)| s.len())
                .sum::<usize>(),
            200
        );
        let i = InputLine::from("abcdefghij\n".repeat(1000).as_str());
        let visible = i.styled_rows(80, 500, 3);
        assert_eq!(visible.len(), 3);
        assert!(visible
            .iter()
            .all(|row| row.len() == 1 && row[0].0 == "abcdefghij"));
    }

    fn at(s: &str, cursor: usize) -> InputLine {
        let mut i = InputLine::from(s);
        i.cursor = cursor;
        i
    }

    #[test]
    fn moves_and_edits_at_the_cursor() {
        let mut i = InputLine::from("hello world");
        i.home();
        i.insert_str(">> ");
        assert_eq!(&*i, ">> hello world");
        i.end();
        i.word_left();
        assert_eq!(i.cursor, ">> hello ".len());
        i.delete_word_back();
        assert_eq!(&*i, ">> world");
        i.kill_to_end();
        assert_eq!(&*i, ">> ");
        i.kill_to_start();
        assert!(i.is_empty());

        let mut u = InputLine::from("héllo");
        u.left();
        u.left();
        u.backspace();
        assert_eq!(&*u, "hélo");
        u.word_right();
        u.delete();
        assert_eq!(u.cursor, u.len());
    }

    #[test]
    fn multi_line_navigation() {
        let mut i = at("ab\ncdef\ng", 5); // after "cd"
        assert!(!i.on_first_line() && !i.on_last_line());
        i.vertical(false);
        assert_eq!(i.cursor, 2, "column clamps to the shorter line");
        i.vertical(true);
        assert_eq!(i.cursor, 5);
        i.vertical(true);
        assert_eq!(i.cursor, "ab\ncdef\ng".len());
        i.home();
        assert_eq!(i.before_cursor(), "ab\ncdef\n");
    }

    #[test]
    fn large_pastes_collapse_and_expand() {
        let big = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut i = InputLine::from("see ");
        i.paste(&big);
        assert_eq!(&*i, "see [pasted 20 lines #1]");
        i.insert_str(" thanks");
        assert_eq!(i.expanded(), format!("see {big} thanks"));
        // Backspacing into the placeholder removes the whole paste.
        i.end();
        for _ in 0.." thanks".len() {
            i.backspace();
        }
        i.backspace();
        assert_eq!(&*i, "see ");
        i.paste("a\r\nb");
        assert_eq!(i.take(), "see a\nb");
    }

    #[test]
    fn history_keeps_the_draft() {
        let h = vec!["first".to_string(), "second".to_string()];
        let mut i = InputLine::from("draft");
        i.history_prev(&h);
        assert_eq!(&*i, "second");
        i.history_prev(&h);
        i.history_prev(&h);
        assert_eq!(&*i, "first");
        i.history_next(&h);
        assert_eq!(&*i, "second");
        i.history_next(&h);
        assert_eq!(&*i, "draft");
        assert!(i.browsing.is_none());
    }

    #[test]
    fn reverse_search() {
        let h: Vec<String> = ["fix the build", "deploy it", "fix tests"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut i = InputLine::default();
        i.search_older(&h);
        for c in "fix".chars() {
            i.search_type(Some(c), &h);
        }
        assert_eq!(i.searching(), Some(("fix", Some(2))));
        i.search_older(&h);
        assert_eq!(i.searching(), Some(("fix", Some(0))));
        i.search_accept(&h);
        assert_eq!(&*i, "fix the build");
        assert!(i.searching().is_none());
    }

    #[test]
    fn layout_wraps_and_places_the_cursor() {
        let i = at("abcdef\ngh", 4);
        let (rows, cur) = i.layout(4);
        assert_eq!(rows, vec!["abcd", "ef", "gh"]);
        assert_eq!(cur, (1, 0));
        let end = InputLine::from("abcd");
        assert_eq!(end.layout(4).1, (1, 0));
        assert_eq!(
            InputLine::default().layout(4),
            (vec![String::new()], (0, 0))
        );
    }

    #[test]
    fn wrapped_vertical_moves_preserve_text_column_and_explicit_newline_rows() {
        let mut input = InputLine::from("abcdefghijklmn");
        assert_eq!(input.layout(5).1, (2, 4));
        assert!(input.vertical_wrapped(false, 5));
        assert_eq!(input.layout(5).1, (1, 4));
        assert!(input.vertical_wrapped(false, 5));
        assert_eq!(input.layout(5).1, (0, 4));
        assert!(!input.vertical_wrapped(false, 5));
        assert_eq!(&*input, "abcdefghijklmn");

        let mut input = at("abcdef\nx\nuvwxyz", 5);
        assert_eq!(input.layout(4).1, (1, 1));
        assert!(input.vertical_wrapped(true, 4));
        assert_eq!(input.layout(4).1, (2, 1));
        assert!(input.vertical_wrapped(true, 4));
        assert_eq!(input.layout(4).1, (3, 1));
        assert!(input.vertical_wrapped(true, 4));
        assert_eq!(input.layout(4).1, (4, 1));
        assert_eq!(&*input, "abcdef\nx\nuvwxyz");
    }

    #[test]
    fn wrapped_vertical_goal_survives_short_rows_and_resets_for_edit_move_and_resize() {
        let mut input = at("abcdef\nx\nuvwxyz", 3);
        assert!(input.vertical_wrapped(true, 6));
        assert_eq!(input.layout(6).1, (1, 1));
        assert!(input.vertical_wrapped(true, 6));
        assert_eq!(input.layout(6).1, (2, 3));
        input.left();
        assert!(input.vertical_wrapped(false, 6));
        assert!(input.vertical_wrapped(false, 6));
        assert_eq!(input.layout(6).1, (0, 2));
        input.insert_char('!');
        assert!(input.visual_goal.is_none());
        input.vertical_wrapped(true, 6);
        input.vertical_wrapped(true, 4);
        assert_eq!(input.visual_goal.unwrap().0, 4);
        input.set("fresh");
        assert!(input.visual_goal.is_none());
    }

    #[test]
    fn wrapped_vertical_keeps_wide_and_joining_clusters_whole_including_exact_width_ends() {
        for text in [
            "表a表a表a",
            "สวัสดีครับสวัสดีครับ",
            "a👩🏽‍💻b👨‍👩‍👧‍👦c",
            "abcd\n表e\u{301}z\n",
        ] {
            let boundaries: Vec<_> = text
                .grapheme_indices(true)
                .map(|(byte, _)| byte)
                .chain(std::iter::once(text.len()))
                .collect();
            for width in [1, 2, 3, 4, 7] {
                for &byte in &boundaries {
                    for down in [false, true] {
                        let mut input = at(text, byte);
                        input.vertical_wrapped(down, width);
                        assert!(boundaries.contains(&input.cursor), "{text:?} width={width}");
                        assert_eq!(&*input, text);
                    }
                }
            }
        }
        let mut input = InputLine::from("abcdefgh");
        assert_eq!(input.layout(4).1, (2, 0));
        assert!(input.vertical_wrapped(false, 4));
        assert_eq!(input.layout(4).1, (1, 0));
    }

    /// Steps and deletions take whole clusters (#784).
    #[test]
    fn edits_whole_grapheme_clusters() {
        let mut i = InputLine::from("สวัสดี");
        for _ in 0..4 {
            i.left();
        }
        assert_eq!(i.cursor, 0, "four clusters: ส วั ส ดี");
        i.end();
        i.backspace();
        assert_eq!(&*i, "สวัส", "ดี goes as one");

        let family = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
        let mut i = InputLine::from(format!("a{family}b").as_str());
        i.left();
        i.left();
        assert_eq!(i.cursor, 1, "one step over the whole family");
        i.right();
        i.backspace();
        assert_eq!(&*i, "ab");

        for s in ["🇹🇭x", "e\u{301}x"] {
            let mut i = InputLine::from(s);
            i.home();
            i.right();
            assert_eq!(&i.text[i.cursor..], "x", "{s}: never inside the cluster");
            i.home();
            i.delete();
            assert_eq!(&*i, "x");
        }
    }

    /// Layout counts screen columns (#785).
    #[test]
    fn layout_counts_columns() {
        let i = InputLine::from("你好世界你好");
        let (rows, cursor) = i.layout(6);
        assert_eq!(rows, ["你好世", "界你好", ""]);
        assert_eq!(cursor, (2, 0));
        let (rows, cursor) = i.layout(7);
        assert_eq!(rows, ["你好世", "界你好"]);
        assert_eq!(cursor, (1, 6));
        let i = InputLine::from("สวัสดีครับ");
        let (rows, cursor) = i.layout(10);
        assert_eq!(rows.len(), 1);
        assert_eq!(cursor, (0, 7), "display width, not 10 chars");
        // Up/Down keep the screen column.
        let mut i = at("表a\nabc", "表".len());
        i.vertical(true);
        assert_eq!(i.cursor, "表a\nab".len());
    }

    /// Ctrl+W in Thai takes one cluster, not the whole sentence (#786).
    #[test]
    fn word_steps_in_unspaced_scripts() {
        let mut i = InputLine::from("ช่วยสรุปไฟล์นี้");
        i.delete_word_back();
        assert!(!i.is_empty());
        assert_eq!(&*i, "ช่วยสรุปไฟล์", "the last cluster, นี้");
        let mut i = InputLine::from("hello 世界");
        i.delete_word_back();
        assert_eq!(&*i, "hello 世");
    }
    #[test]
    fn command_intent_is_part_of_undo_redo_and_transcript_history_snapshots() {
        let mut line = InputLine::from("/quit");
        line.set_literal(true);
        line.clear();
        assert!(!line.is_literal());
        assert!(line.undo());
        assert_eq!(line, "/quit");
        assert!(line.is_literal());
        assert!(line.redo());
        assert!(line.is_empty());
        assert!(!line.is_literal());
        line.set("/sessions");
        assert!(!line.is_literal());
        line.clear();
        assert!(line.undo());
        assert_eq!(line, "/sessions");
        assert!(!line.is_literal());
        let history = vec!["/quit".to_string()];
        line.history_prev(&history);
        assert_eq!(line, "/quit");
        assert!(line.is_literal());
        line.history_next(&history);
        assert_eq!(line, "/sessions");
        assert!(
            !line.is_literal(),
            "returning to a typed draft preserves its original command intent"
        );
    }
    #[test]
    fn oversized_visible_and_backing_drafts_refuse_history_without_losing_metadata() {
        for backing in [false, true] {
            let mut line = InputLine::from("first");
            line.insert_str(" second");
            line.insert_str(" third");
            assert!(line.undo());
            if backing {
                line.text = "[pasted]".into();
                line.pastes = vec![("[pasted]".into(), "x".repeat(16 * 1024 * 1024 + 1))];
            } else {
                line.text = "x".repeat(16 * 1024 * 1024 + 1);
            }
            line.cursor = 3;
            line.anchor = Some(1);
            line.literal = true;
            let current = line.snapshot();
            let history = format!("{:?}", line.history);
            assert!(!line.undo());
            assert!(line.edit_notice().unwrap().starts_with("Undo refused:"));
            assert_eq!(&*line, current.text);
            assert_eq!(line.cursor, current.cursor);
            assert_eq!(line.anchor, current.anchor);
            assert_eq!(line.pastes, current.pastes);
            assert_eq!(line.literal, current.literal);
            assert_eq!(format!("{:?}", line.history), history);
            assert!(!line.redo());
            assert!(line.edit_notice().unwrap().starts_with("Redo refused:"));
            assert_eq!(&*line, current.text);
            assert_eq!(line.cursor, current.cursor);
            assert_eq!(line.anchor, current.anchor);
            assert_eq!(line.pastes, current.pastes);
            assert_eq!(line.literal, current.literal);
            assert_eq!(format!("{:?}", line.history), history);
            line.anchor = None;
            line.end();
            line.insert_char('!');
            assert!(line.edit_notice().is_none());
            let copied = line.expanded();
            assert_eq!(copied.len(), 16 * 1024 * 1024 + 2);
            assert!(copied.ends_with('!'));
        }
    }

    #[test]
    fn rejected_cuts_keep_older_yanks_explicit_and_break_the_coalescing_chain() {
        let mut line = InputLine::from("/quit");
        line.set_literal(true);
        line.kill_to_start();
        line.set(&"x".repeat(1024 * 1024 + 1));
        line.kill_to_start();
        assert!(line.is_empty());
        assert!(line.kill_notice().is_some());
        assert!(line.projection().kill_notice().is_some());
        assert!(!line.yank());
        assert!(line.is_empty());
        assert!(line.cycle_yank());
        assert_eq!(line, "/quit");
        assert!(line.is_literal());
        assert!(line.kill_notice().is_none());
        let mut line = InputLine::from("z");
        line.kill_to_start();
        line.set(&("a".repeat(1024 * 1024 - 4) + "bbbbbbbbEND"));
        line.home();
        line.kill_range(0, 1024 * 1024 - 4);
        let old = line.ring.get(0).unwrap().to_string();
        line.kill_range(0, 8);
        assert!(line.cut_unavailable);
        assert!(!line.last_kill);
        assert_eq!(line.ring.get(0), Some(old.as_str()));
        assert_eq!(line.ring.get(1), Some("z"));
        line.kill_range(0, 3);
        assert!(!line.cut_unavailable);
        assert_eq!(line.ring.get(0), Some("END"));
        assert_eq!(line.ring.get(1), Some(old.as_str()));
        assert_eq!(line.ring.get(2), Some("z"));
        assert!(line.yank());
        assert_eq!(line, "END");
        assert!(line.cycle_yank());
        assert_eq!(&*line, old.as_str());
    }

    #[test]
    fn private_kill_ring_keeps_distinct_intents_and_cycles_without_reclassifying_the_draft() {
        let mut line = InputLine::from("/quit");
        line.set_literal(true);
        line.kill_to_start();
        line.set("/quit");
        line.kill_to_start();
        line.clear();
        assert!(line.yank());
        assert_eq!(line, "/quit");
        assert!(!line.is_literal());
        assert!(line.cycle_yank());
        assert_eq!(line, "/quit");
        assert!(line.is_literal());
        assert!(line.cycle_yank());
        assert_eq!(line, "/quit");
        assert!(!line.is_literal());
    }
}
