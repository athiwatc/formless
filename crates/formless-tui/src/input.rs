//! The chat input (SPEC §10.2): a small multi-line editor with readline
//! keys, collapsed large pastes and per-Session history.

/// Pastes longer than this many lines (or `PASTE_COLLAPSE_CHARS` chars)
/// show as `[pasted N lines]` in the input; the full text is sent.
const PASTE_COLLAPSE_LINES: usize = 8;
const PASTE_COLLAPSE_CHARS: usize = 1500;

#[derive(Debug, Default, Clone)]
pub struct InputLine {
    text: String,
    /// Byte offset, always on a char boundary.
    cursor: usize,
    /// Placeholder shown in `text` -> the pasted text it stands for.
    pastes: Vec<(String, String)>,
    /// Browsing history: (index into the history, the draft to restore).
    browsing: Option<(usize, String)>,
    /// Ctrl+R: the query and the history index it matched.
    search: Option<(String, Option<usize>)>,
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
        i
    }
}

use crate::text;
use unicode_segmentation::UnicodeSegmentation;

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
    /// Replace the text, cursor at the end.
    pub fn set(&mut self, s: &str) {
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
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.browsing = None;
    }

    pub fn insert_str(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.browsing = None;
    }

    /// A bracketed paste: inserted as one block, never as Enter presses.
    /// Large ones collapse to a placeholder.
    pub fn paste(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let lines = s.lines().count();
        if lines > PASTE_COLLAPSE_LINES || s.chars().count() > PASTE_COLLAPSE_CHARS {
            let placeholder = format!("[pasted {lines} lines #{}]", self.pastes.len() + 1);
            self.insert_str(&placeholder);
            self.pastes.push((placeholder, s));
        } else {
            self.insert_str(&s);
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
        self.cursor = self.prev_boundary(self.cursor);
    }

    pub fn right(&mut self) {
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
        self.cursor = self.word_start_before(self.cursor);
    }

    pub fn word_right(&mut self) {
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
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Remove `start..end`, dropping any collapsed paste it cut into.
    fn remove(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.text.replace_range(start..end, "");
        self.cursor = start;
        let text = &self.text;
        self.pastes.retain(|(p, _)| text.contains(p.as_str()));
        self.browsing = None;
    }

    pub fn backspace(&mut self) {
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
        let end = self.next_boundary(self.cursor);
        self.remove(self.cursor, end);
    }

    /// Ctrl+W / Alt+Backspace.
    pub fn delete_word_back(&mut self) {
        let start = self.word_start_before(self.cursor);
        self.remove(start, self.cursor);
    }

    /// Alt+D.
    pub fn delete_word_forward(&mut self) {
        let end = self.word_end_after(self.cursor);
        self.remove(self.cursor, end);
    }

    /// Ctrl+U: to the start of the line.
    pub fn kill_to_start(&mut self) {
        let start = self.line_start();
        self.remove(start, self.cursor);
    }

    /// Ctrl+K: to the end of the line.
    pub fn kill_to_end(&mut self) {
        let end = self.line_end();
        self.remove(self.cursor, end);
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
        let col = text::width(&self.text[self.line_start()..self.cursor]);
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
    pub fn history_prev(&mut self, history: &[String]) {
        if history.is_empty() {
            return;
        }
        let (idx, draft) = match self.browsing.take() {
            Some((0, d)) => (0, d),
            Some((i, d)) => (i - 1, d),
            None => (history.len() - 1, self.expanded()),
        };
        self.set(&history[idx]);
        self.browsing = Some((idx, draft));
    }

    /// Down back towards the draft.
    pub fn history_next(&mut self, history: &[String]) {
        let Some((idx, draft)) = self.browsing.take() else {
            return;
        };
        if idx + 1 < history.len() {
            self.set(&history[idx + 1]);
            self.browsing = Some((idx + 1, draft));
        } else {
            self.set(&draft);
        }
    }

    // --- Ctrl+R history search -------------------------------------------

    pub fn searching(&self) -> Option<(&str, Option<usize>)> {
        self.search.as_ref().map(|(q, m)| (q.as_str(), *m))
    }

    /// Start a search, or (when one is open) look further back.
    pub fn search_older(&mut self, history: &[String]) {
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

    pub fn search_type(&mut self, c: Option<char>, history: &[String]) {
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
    pub fn search_accept(&mut self, history: &[String]) {
        if let Some((_, Some(i))) = self.search.take() {
            self.set(&history[i]);
        }
    }

    pub fn search_cancel(&mut self) {
        self.search = None;
    }

    /// The rows to draw for `width` columns, and the cursor's (row, col),
    /// both in screen columns: a wide CJK character takes two, a Thai mark
    /// none (#785).
    pub fn layout(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut cursor = (0, 0);
        let mut offset = 0;
        for line in self.text.split('\n') {
            let mut row = String::new();
            let mut col = 0;
            for (i, g) in line.grapheme_indices(true) {
                let w = text::width(g);
                if col + w > width && col > 0 {
                    rows.push(std::mem::take(&mut row));
                    col = 0;
                }
                if offset + i == self.cursor {
                    cursor = (rows.len(), col);
                }
                row.push_str(g);
                col += w;
            }
            if offset + line.len() == self.cursor {
                if col == width {
                    rows.push(std::mem::take(&mut row));
                    col = 0;
                }
                cursor = (rows.len(), col);
            }
            rows.push(row);
            offset += line.len() + 1;
        }
        (rows, cursor)
    }
}

fn find_back(history: &[String], query: &str, before: usize) -> Option<usize> {
    let q = query.to_lowercase();
    history[..before.min(history.len())]
        .iter()
        .rposition(|h| h.to_lowercase().contains(&q))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
