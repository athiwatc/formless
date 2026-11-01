//! A bounded Vim editing subset, shared by the composer and Tool editor.
//! Semantics: Vim motion.txt (cw exception/text objects), change.txt and
//! repeat.txt. No Ex commands, Visual mode, macros or system clipboard.
use crate::input::InputLine;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::ops::Range;

pub(crate) const HELP: &str = "Vim editing: i/a/I/A insert; Esc returns to Normal. h/j/k/l, w/b/e, W/B/E, 0/$, gg/G move. d/c/y + motion; dd/cc/yy operate on lines. iw/aw/iW/aW, quotes and ()/[]/{}/<> text objects. x deletes; p/P paste the private yank register; . repeats the last change. Counts 1–999. Normal Esc stops a Run; editor Ctrl+S saves and Ctrl+Q cancels. List-view keys are unchanged.";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Mode {
    Normal,
    #[default]
    Insert,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    Composer,
    Editor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Handled,
    Pass,
    Escape,
    Help,
}
#[derive(Debug, Clone)]
enum Command {
    Operator(char, String, usize),
    Insert(char),
    Put(bool, String, bool),
    Delete(usize),
}
#[derive(Debug, Clone)]
struct Change {
    command: Command,
    edits: Vec<KeyEvent>,
}
#[derive(Debug, Default, Clone)]
pub(crate) struct Vim {
    pub(crate) mode: Mode,
    pending: String,
    register: String,
    register_linewise: bool,
    last: Option<Change>,
    inserting: Option<Change>,
}
impl Vim {
    pub(crate) fn label(&self) -> &'static str {
        if self.mode == Mode::Insert {
            "INSERT"
        } else {
            "NORMAL"
        }
    }
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn paste(&mut self, line: &mut InputLine, text: &str) {
        line.modal_expand_pastes();
        if self.mode == Mode::Normal {
            self.apply(line, Command::Insert('i'));
        }
        for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
            let key = if c == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
            } else {
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
            };
            insert_key(line, key, Surface::Editor);
            self.inserting
                .get_or_insert(Change {
                    command: Command::Insert('i'),
                    edits: vec![],
                })
                .edits
                .push(key);
        }
    }

    pub(crate) fn key(&mut self, line: &mut InputLine, key: KeyEvent, surface: Surface) -> Outcome {
        line.modal_expand_pastes();
        if key.code == KeyCode::Esc {
            if self.mode == Mode::Insert {
                self.mode = Mode::Normal;
                if let Some(change) = self.inserting.take() {
                    if !change.edits.is_empty()
                        || matches!(
                            change.command,
                            Command::Operator(..) | Command::Insert('o' | 'O')
                        )
                    {
                        self.last = Some(change);
                    }
                }
                let at = line.cursor_byte();
                if at > line_start(line, at) {
                    line.left();
                }
                normal_cursor(line);
                return Outcome::Handled;
            }
            if !self.pending.is_empty() {
                self.pending.clear();
                return Outcome::Handled;
            }
            return Outcome::Escape;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            if self.mode == Mode::Insert {
                self.inserting = None;
            }
            return Outcome::Pass;
        }
        if self.mode == Mode::Insert {
            let handled = insert_key(line, key, surface);
            if handled {
                self.inserting
                    .get_or_insert(Change {
                        command: Command::Insert('i'),
                        edits: Vec::new(),
                    })
                    .edits
                    .push(key);
                return Outcome::Handled;
            }
            return Outcome::Pass;
        }
        normal_cursor(line);
        let c = match key.code {
            KeyCode::Char(c) => c,
            KeyCode::Left | KeyCode::Backspace => 'h',
            KeyCode::Right => 'l',
            KeyCode::Up => 'k',
            KeyCode::Down => 'j',
            KeyCode::Home => '0',
            KeyCode::End => '$',
            KeyCode::Delete => 'x',
            KeyCode::Enter if surface == Surface::Editor => 'j',
            _ => return Outcome::Pass,
        };
        if c == '?' && self.pending.is_empty() {
            return Outcome::Help;
        }
        if c == '.' && self.pending.is_empty() {
            if let Some(change) = self.last.clone() {
                self.apply(line, change.command.clone());
                for key in &change.edits {
                    insert_key(line, *key, surface);
                }
                if self.mode == Mode::Insert {
                    self.mode = Mode::Normal;
                    if line.cursor_byte() > line_start(line, line.cursor_byte()) {
                        line.left();
                    }
                }
                self.inserting = None;
                self.last = Some(change);
                normal_cursor(line);
            }
            return Outcome::Handled;
        }
        if self.pending.is_empty() {
            if c == '/' && surface == Surface::Composer {
                self.apply(line, Command::Insert('i'));
                self.paste(line, "/");
                return Outcome::Handled;
            }
            if matches!(c, 'i' | 'a' | 'I' | 'A' | 'o' | 'O') {
                self.apply(line, Command::Insert(c));
                return Outcome::Handled;
            }
            if c == 'p' || c == 'P' {
                let cmd = Command::Put(c == 'p', self.register.clone(), self.register_linewise);
                self.apply(line, cmd.clone());
                self.last = Some(Change {
                    command: cmd,
                    edits: vec![],
                });
                return Outcome::Handled;
            }
        }
        self.pending.push(c);
        let (count, rest) = count_prefix(&self.pending);
        if rest.is_empty() {
            return Outcome::Handled;
        }
        if let Some(op @ ('d' | 'c' | 'y')) = rest.chars().next() {
            let tail = &rest[op.len_utf8()..];
            let (motion_count, motion) = count_prefix(tail);
            if motion.is_empty() || motion == "g" || motion == "i" || motion == "a" {
                return Outcome::Handled;
            }
            let cmd = Command::Operator(
                op,
                motion.to_string(),
                count.saturating_mul(motion_count).min(999),
            );
            self.pending.clear();
            let changed = self.apply(line, cmd.clone());
            if changed && op != 'y' && self.mode != Mode::Insert {
                self.last = Some(Change {
                    command: cmd,
                    edits: vec![],
                });
            }
            return Outcome::Handled;
        }
        if rest == "g" {
            return Outcome::Handled;
        }
        if rest == "x" {
            let cmd = Command::Delete(count);
            self.pending.clear();
            self.apply(line, cmd.clone());
            self.last = Some(Change {
                command: cmd,
                edits: vec![],
            });
            return Outcome::Handled;
        }
        let motion = rest.to_string();
        self.pending.clear();
        if let Some((target, _, _)) = motion_target(line, line.cursor_byte(), &motion, count) {
            line.set_modal_cursor(target);
            normal_cursor(line);
        }
        Outcome::Handled
    }
    fn apply(&mut self, line: &mut InputLine, command: Command) -> bool {
        match &command {
            Command::Insert(kind) => {
                match kind {
                    'a' => {
                        if line.cursor_byte() < line_end(line, line.cursor_byte()) {
                            line.right();
                        }
                    }
                    'I' => {
                        let mut at = line_start(line, line.cursor_byte());
                        while at < line_end(line, at)
                            && cluster(line, at).chars().all(char::is_whitespace)
                        {
                            at = next(line, at)
                        }
                        line.set_modal_cursor(at);
                    }
                    'A' => line.end(),
                    'o' => {
                        line.end();
                        line.newline();
                    }
                    'O' => {
                        line.home();
                        let at = line.cursor_byte();
                        line.newline();
                        line.set_modal_cursor(at);
                    }
                    _ => {}
                }
                self.mode = Mode::Insert;
                self.inserting = Some(Change {
                    command: command.clone(),
                    edits: vec![],
                });
            }
            Command::Delete(count) => {
                let start = line.cursor_byte();
                let mut end = start;
                for _ in 0..*count {
                    if end >= line_end(line, start) {
                        break;
                    }
                    end = next(line, end)
                }
                self.register = line[start..end].into();
                self.register_linewise = false;
                line.modal_replace(start..end, "");
                normal_cursor(line);
            }
            Command::Put(after, text, linewise) => {
                if *linewise {
                    let mut text = text.clone();
                    let at = if *after {
                        let end = line_end(line, line.cursor_byte());
                        if end == line.len() {
                            text = format!("\n{}", text.trim_end_matches('\n'));
                            end
                        } else {
                            end + 1
                        }
                    } else {
                        line_start(line, line.cursor_byte())
                    };
                    if !*after && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    line.modal_replace(at..at, &text);
                    line.set_modal_cursor(if text.starts_with('\n') { at + 1 } else { at });
                    normal_cursor(line);
                    return !text.is_empty();
                }
                let at = if *after && line.cursor_byte() < line_end(line, line.cursor_byte()) {
                    next(line, line.cursor_byte())
                } else {
                    line.cursor_byte()
                };
                line.modal_replace(at..at, text);
                if !text.is_empty() {
                    line.left()
                }
                normal_cursor(line);
            }
            Command::Operator(op, motion, count) => {
                let Some(range) = operator_range(line, line.cursor_byte(), *op, motion, *count)
                else {
                    return false;
                };
                self.register = line[range.clone()].into();
                self.register_linewise =
                    motion == &op.to_string() || matches!(motion.as_str(), "gg" | "G" | "j" | "k");
                if *op == 'y' {
                    line.set_modal_cursor(range.start);
                    return false;
                }
                line.modal_replace(range, "");
                if *op == 'c' {
                    self.mode = Mode::Insert;
                    self.inserting = Some(Change {
                        command: command.clone(),
                        edits: vec![],
                    })
                } else {
                    normal_cursor(line)
                }
            }
        }
        true
    }
}
fn insert_key(line: &mut InputLine, key: KeyEvent, surface: Surface) -> bool {
    match key.code {
        KeyCode::Char(c) => line.insert_char(c),
        KeyCode::Backspace => line.backspace(),
        KeyCode::Delete => line.delete(),
        KeyCode::Left => line.left(),
        KeyCode::Right => line.right(),
        KeyCode::Up => line.vertical(false),
        KeyCode::Down => line.vertical(true),
        KeyCode::Home => line.home(),
        KeyCode::End => line.end(),
        KeyCode::Enter
            if surface == Surface::Editor || key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            line.newline()
        }
        KeyCode::Tab if surface == Surface::Editor => line.insert_str("  "),
        _ => return false,
    }
    true
}
fn count_prefix(s: &str) -> (usize, &str) {
    let end = s
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit())
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    if end == 0 || s.starts_with('0') {
        (1, s)
    } else {
        (
            s[..end].parse::<usize>().unwrap_or(999).clamp(1, 999),
            &s[end..],
        )
    }
}
fn prev(s: &str, at: usize) -> usize {
    crate::text::prev_cluster(s, at)
}
fn next(s: &str, at: usize) -> usize {
    crate::text::next_cluster(s, at)
}
fn cluster(s: &str, at: usize) -> &str {
    &s[at..next(s, at)]
}
fn line_start(s: &str, at: usize) -> usize {
    s[..at].rfind('\n').map_or(0, |i| i + 1)
}
fn line_end(s: &str, at: usize) -> usize {
    s[at..].find('\n').map_or(s.len(), |i| at + i)
}
fn normal_cursor(line: &mut InputLine) {
    let at = line.cursor_byte();
    let start = line_start(line, at);
    let end = line_end(line, at);
    if at == end && end > start {
        line.set_modal_cursor(prev(line, end))
    }
}
fn kind(g: &str, big: bool) -> u8 {
    if g.chars().all(char::is_whitespace) {
        0
    } else if big
        || g.chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    {
        1
    } else {
        2
    }
}
fn word_forward(s: &str, mut at: usize, big: bool) -> usize {
    if at >= s.len() {
        return at;
    }
    let k = kind(cluster(s, at), big);
    if k != 0 {
        while at < s.len() && kind(cluster(s, at), big) == k {
            at = next(s, at)
        }
    }
    while at < s.len() && kind(cluster(s, at), big) == 0 {
        at = next(s, at)
    }
    at
}
fn word_back(s: &str, mut at: usize, big: bool) -> usize {
    if at == 0 {
        return at;
    }
    at = prev(s, at);
    while at > 0 && kind(cluster(s, at), big) == 0 {
        at = prev(s, at)
    }
    let k = kind(cluster(s, at), big);
    while at > 0 && kind(cluster(s, prev(s, at)), big) == k {
        at = prev(s, at)
    }
    at
}
fn word_end(s: &str, mut at: usize, big: bool) -> usize {
    if at >= s.len() {
        return at;
    }
    let original = at;
    if next(s, at) < s.len()
        && kind(cluster(s, at), big) != 0
        && kind(cluster(s, next(s, at)), big) == kind(cluster(s, at), big)
    {
        while next(s, at) < s.len()
            && kind(cluster(s, next(s, at)), big) == kind(cluster(s, at), big)
        {
            at = next(s, at)
        }
        return at;
    }
    at = next(s, original);
    while at < s.len() && kind(cluster(s, at), big) == 0 {
        at = next(s, at)
    }
    if at == s.len() {
        return prev(s, at);
    }
    let k = kind(cluster(s, at), big);
    while next(s, at) < s.len() && kind(cluster(s, next(s, at)), big) == k {
        at = next(s, at)
    }
    at
}
fn motion_target(s: &str, at: usize, motion: &str, count: usize) -> Option<(usize, bool, bool)> {
    let mut to = at;
    let mut inclusive = false;
    let mut linewise = false;
    match motion {
        "w" | "W" => {
            for _ in 0..count {
                to = word_forward(s, to, motion == "W")
            }
        }
        "b" | "B" => {
            for _ in 0..count {
                to = word_back(s, to, motion == "B")
            }
        }
        "e" | "E" => {
            for _ in 0..count {
                to = word_end(s, to, motion == "E")
            }
            inclusive = true
        }
        "h" => {
            for _ in 0..count {
                if to > line_start(s, to) {
                    to = prev(s, to)
                }
            }
        }
        "l" => {
            for _ in 0..count {
                if to < line_end(s, to) {
                    to = next(s, to)
                }
            }
        }
        "0" => to = line_start(s, at),
        "$" => {
            to = line_end(s, at);
            inclusive = false
        }
        "gg" => {
            to = 0;
            linewise = true
        }
        "G" => {
            to = line_start(s, s.len());
            linewise = true
        }
        "j" | "k" => {
            let mut line = InputLine::from(s);
            line.set_modal_cursor(at);
            for _ in 0..count {
                line.vertical(motion == "j")
            }
            to = line.cursor_byte();
            linewise = true
        }
        _ => return None,
    }
    Some((to, inclusive, linewise))
}
fn operator_range(
    s: &str,
    at: usize,
    op: char,
    motion: &str,
    count: usize,
) -> Option<Range<usize>> {
    if motion == op.to_string() {
        let start = line_start(s, at);
        let mut end = start;
        for _ in 0..count {
            end = line_end(s, end);
            if end < s.len() {
                end += 1
            } else {
                break;
            }
        }
        if op == 'c' && end > start && s[..end].ends_with('\n') {
            end -= 1;
        }
        return Some(start..end);
    }
    if motion.starts_with(['i', 'a']) && motion.chars().count() == 2 {
        return object_range(s, at, motion, count);
    }
    if op == 'c' && matches!(motion, "w" | "W") && at < s.len() && kind(cluster(s, at), false) != 0
    {
        let big = motion == "W";
        let mut end = at;
        for n in 0..count {
            if n > 0 {
                while end < s.len() && kind(cluster(s, end), big) == 0 {
                    end = next(s, end)
                }
            }
            if end == s.len() {
                break;
            }
            let word = kind(cluster(s, end), big);
            while end < s.len() && kind(cluster(s, end), big) == word {
                end = next(s, end)
            }
        }
        return Some(at..end);
    }
    let (target, inclusive, linewise) = motion_target(s, at, motion, count)?;
    if linewise {
        let start = line_start(s, at.min(target));
        let end = line_end(s, at.max(target));
        return Some(start..if end < s.len() { end + 1 } else { end });
    }
    let start = at.min(target);
    let mut end = at.max(target);
    if inclusive && target >= at && end < s.len() {
        end = next(s, end)
    }
    if op != 'c' && matches!(motion, "w" | "W") && s[start..end].contains('\n') {
        let before = s[..end].trim_end_matches(char::is_whitespace).len();
        if before >= start {
            end = before
        }
    }
    Some(start..end)
}
fn object_range(s: &str, at: usize, motion: &str, count: usize) -> Option<Range<usize>> {
    let mut chars = motion.chars();
    let around = chars.next()? == 'a';
    let object = chars.next()?;
    if matches!(object, 'w' | 'W') {
        if at >= s.len() {
            return None;
        }
        let big = object == 'W';
        let k = kind(cluster(s, at), big);
        let mut start = at;
        while start > 0 && kind(cluster(s, prev(s, start)), big) == k {
            start = prev(s, start)
        }
        let mut end = at;
        while end < s.len() && kind(cluster(s, end), big) == k {
            end = next(s, end)
        }
        if around && k == 0 && end < s.len() {
            let next_kind = kind(cluster(s, end), big);
            while end < s.len() && kind(cluster(s, end), big) == next_kind {
                end = next(s, end)
            }
        }
        for _ in 1..count {
            if around {
                while end < s.len() && kind(cluster(s, end), big) == 0 {
                    end = next(s, end)
                }
            }
            if end == s.len() {
                break;
            }
            let next_kind = kind(cluster(s, end), big);
            while end < s.len() && kind(cluster(s, end), big) == next_kind {
                end = next(s, end)
            }
        }
        if around && k != 0 {
            let initial = end;
            while end < s.len() && cluster(s, end) != "\n" && kind(cluster(s, end), big) == 0 {
                end = next(s, end)
            }
            if end == initial {
                while start > 0
                    && cluster(s, prev(s, start)) != "\n"
                    && kind(cluster(s, prev(s, start)), big) == 0
                {
                    start = prev(s, start)
                }
            }
        }
        return Some(start..end);
    }
    let (open, close) = match object {
        '(' | ')' | 'b' => ('(', ')'),
        '[' | ']' => ('[', ']'),
        '{' | '}' | 'B' => ('{', '}'),
        '<' | '>' => ('<', '>'),
        '\'' | '"' | '`' => (object, object),
        _ => return None,
    };
    if open == close {
        let start = line_start(s, at);
        let end = line_end(s, at);
        let mut quotes = Vec::new();
        for (i, c) in s[start..end].char_indices() {
            if c == open {
                let pos = start + i;
                let escaped = s[..pos].chars().rev().take_while(|c| *c == '\\').count() % 2 == 1;
                if !escaped {
                    quotes.push(pos)
                }
            }
        }
        let pair = quotes
            .chunks_exact(2)
            .find(|p| p[0] <= at && at <= p[1])
            .or_else(|| quotes.chunks_exact(2).find(|p| p[0] >= at))?;
        return Some(if around {
            pair[0]..pair[1] + close.len_utf8()
        } else {
            pair[0] + open.len_utf8()..pair[1]
        });
    }
    let mut stack = Vec::new();
    let mut pairs = Vec::new();
    for (i, c) in s.char_indices() {
        if c == open {
            stack.push(i)
        } else if c == close {
            if let Some(start) = stack.pop() {
                if start <= at && at <= i {
                    pairs.push((start, i))
                }
            }
        }
    }
    pairs.sort_by_key(|(start, end)| end - start);
    let (start, end) = *pairs.get(count.saturating_sub(1))?;
    Some(if around {
        start..end + close.len_utf8()
    } else {
        start + open.len_utf8()..end
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn commands(vim: &mut Vim, line: &mut InputLine, keys: &str) {
        for c in keys.chars() {
            vim.key(line, key(c), Surface::Composer);
        }
    }
    fn normal(text: &str) -> (Vim, InputLine) {
        let mut line = InputLine::from(text);
        line.set_modal_cursor(0);
        let mut vim = Vim::default();
        vim.mode = Mode::Normal;
        (vim, line)
    }
    #[test]
    fn motions_and_operator_ranges_work_on_actual_multiline_input() {
        let (mut v, mut l) = normal("one two\nlast line");
        commands(&mut v, &mut l, "w");
        assert_eq!(l.cursor_byte(), 4);
        commands(&mut v, &mut l, "e");
        assert_eq!(l.cursor_byte(), 6);
        commands(&mut v, &mut l, "b0");
        assert_eq!(l.cursor_byte(), 0);
        commands(&mut v, &mut l, "dw");
        assert_eq!(&*l, "two\nlast line");
        commands(&mut v, &mut l, "G$");
        assert_eq!(l.cursor_byte(), l.len() - 1);
        commands(&mut v, &mut l, "gg");
        assert_eq!(l.cursor_byte(), 0);
        commands(&mut v, &mut l, "dd");
        assert_eq!(&*l, "last line");
    }
    #[test]
    fn change_word_keeps_trailing_space_and_dot_replays_inserted_text() {
        let (mut v, mut l) = normal("one two three");
        commands(&mut v, &mut l, "cwNEW");
        assert_eq!(&*l, "NEW two three");
        assert_eq!(v.mode, Mode::Insert);
        assert_eq!(
            v.key(
                &mut l,
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                Surface::Composer
            ),
            Outcome::Handled
        );
        commands(&mut v, &mut l, "w.");
        assert_eq!(&*l, "NEW NEW three");
        assert_eq!(v.mode, Mode::Normal);
        let (mut v, mut l) = normal("x rest");
        commands(&mut v, &mut l, "cwZ");
        assert_eq!(&*l, "Z rest");
    }
    #[test]
    fn common_objects_and_private_yank_put_preserve_exact_text() {
        let (mut v, mut l) = normal("alpha beta");
        commands(&mut v, &mut l, "yiwwP");
        assert_eq!(&*l, "alpha alphabeta");
        for (text, keys, expected) in [
            ("one two", "daw", "two"),
            ("(a [b] c)", "di(", "()"),
            ("\"quoted\" tail", "di\"", "\"\" tail"),
            ("e\u{301} 👩‍💻 next", "diw", " 👩‍💻 next"),
        ] {
            let (mut v, mut l) = normal(text);
            commands(&mut v, &mut l, keys);
            assert_eq!(&*l, expected);
        }
        let (mut v, mut l) = normal("first\nsecond");
        commands(&mut v, &mut l, "yyp");
        assert_eq!(&*l, "first\nfirst\nsecond");
    }
    #[test]
    fn edits_never_split_graphemes_and_change_line_keeps_line_break() {
        let (mut v, mut l) = normal("👩‍💻e\u{301} tail");
        commands(&mut v, &mut l, "x");
        assert_eq!(&*l, "e\u{301} tail");
        commands(&mut v, &mut l, "x");
        assert_eq!(&*l, " tail");
        let (mut v, mut l) = normal("one\ntwo");
        commands(&mut v, &mut l, "ccX");
        assert_eq!(&*l, "X\ntwo");
    }
    #[test]
    fn first_insert_escape_is_consumed_and_pastes_remain_real_text() {
        let mut v = Vim::default();
        let mut l = InputLine::default();
        let paste = "private pasted line\n".repeat(10);
        l.paste(&paste);
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(v.key(&mut l, escape, Surface::Composer), Outcome::Handled);
        assert_eq!(v.mode, Mode::Normal);
        assert_eq!(l.expanded(), paste);
        assert_eq!(v.key(&mut l, escape, Surface::Composer), Outcome::Escape);
    }
}
