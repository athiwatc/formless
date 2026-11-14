//! A bounded Vim editing subset, shared by the composer and Tool editor.
//! Semantics: Vim motion.txt (cw exception/text objects), change.txt and
//! repeat.txt. No Ex commands, Visual mode, macros or system clipboard.
use crate::input::InputLine;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::ops::Range;

pub(crate) const HELP: &str = "Vim editing: i/a/I/A insert; Esc returns to Normal. h/j/k/l, w/b/e, W/B/E, 0/$, gg/G move. d/c/y + motion; dd/cc/yy operate on lines. iw/aw/iW/aW, quotes and ()/[]/{}/<> text objects. x deletes; u undoes; p/P paste the private yank register; . repeats the last change. Counts 1–999; counted inserts repeat on Esc. Normal Esc stops a Run; editor Ctrl+S saves and Ctrl+Q cancels. List-view keys are unchanged.";
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
    Insert(char, usize),
    Put(bool, String, bool, bool, usize),
    Delete(usize),
}
#[derive(Debug, Clone)]
struct Change {
    edited: bool,
    literal: bool,
    command: Command,
    edits: Vec<KeyEvent>,
}
#[derive(Debug, Default, Clone)]
pub(crate) struct Vim {
    pub(crate) mode: Mode,
    pending: String,
    register: String,
    register_linewise: bool,
    register_literal: bool,
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
            self.apply(line, Command::Insert('i', 1));
        }
        for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
            let key = if c == '\n' {
                KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
            } else {
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
            };
            let changed = insert_key(line, key, Surface::Editor).unwrap_or(false);
            self.inserting
                .get_or_insert(Change {
                    edited: false,
                    literal: line.is_literal(),
                    command: Command::Insert('i', 1),
                    edits: vec![],
                })
                .edits
                .push(key);
            self.inserting.as_mut().unwrap().literal = line.is_literal();
            self.inserting.as_mut().unwrap().edited |= changed;
        }
    }

    pub(crate) fn key(&mut self, line: &mut InputLine, key: KeyEvent, surface: Surface) -> Outcome {
        line.modal_expand_pastes();
        if key.code == KeyCode::Esc {
            if self.mode == Mode::Insert {
                self.mode = Mode::Normal;
                if let Some(change) = self.inserting.take() {
                    if change.edited
                        || matches!(
                            change.command,
                            Command::Operator(..) | Command::Insert('o' | 'O', _)
                        )
                    {
                        replay_edits(line, &change, surface, true);
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
            if let Some(changed) = insert_key(line, key, surface) {
                self.inserting
                    .get_or_insert(Change {
                        edited: false,
                        literal: line.is_literal(),
                        command: Command::Insert('i', 1),
                        edits: Vec::new(),
                    })
                    .edits
                    .push(key);
                self.inserting.as_mut().unwrap().literal = line.is_literal();
                self.inserting.as_mut().unwrap().edited |= changed;
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
        if c == 'u' && self.pending.is_empty() {
            line.undo();
            normal_cursor(line);
            return Outcome::Handled;
        }
        if c == '?' && self.pending.is_empty() {
            return Outcome::Help;
        }
        if self.pending.is_empty() && c == '/' && surface == Surface::Composer {
            self.apply(line, Command::Insert('i', 1));
            self.paste(line, "/");
            return Outcome::Handled;
        }
        self.pending.push(c);
        let (count, rest) = count_prefix(&self.pending);
        if rest.is_empty() {
            return Outcome::Handled;
        }
        if rest == "." {
            let override_count = (rest.len() != self.pending.len()).then_some(count);
            self.pending.clear();
            if let Some(mut change) = self.last.clone() {
                if let Some(count) = override_count {
                    match &mut change.command {
                        Command::Operator(_, _, n)
                        | Command::Insert(_, n)
                        | Command::Put(_, _, _, _, n)
                        | Command::Delete(n) => *n = count,
                    }
                }
                if !self.apply(line, change.command.clone()) {
                    return Outcome::Handled;
                }
                if !change.edits.is_empty() {
                    line.set_literal(line.is_literal() || change.literal);
                }
                replay_edits(line, &change, surface, false);
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
        if matches!(rest, "i" | "a" | "I" | "A" | "o" | "O") {
            let kind = rest.chars().next().unwrap();
            self.pending.clear();
            self.apply(line, Command::Insert(kind, count));
            return Outcome::Handled;
        }
        if matches!(rest, "p" | "P") {
            let after = rest == "p";
            self.pending.clear();
            let cmd = Command::Put(
                after,
                self.register.clone(),
                self.register_linewise,
                self.register_literal,
                count,
            );
            if self.apply(line, cmd.clone()) {
                self.last = Some(Change {
                    edited: false,
                    literal: line.is_literal(),
                    command: cmd,
                    edits: vec![],
                });
            }
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
                    edited: false,
                    literal: line.is_literal(),
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
            if self.apply(line, cmd.clone()) {
                self.last = Some(Change {
                    edited: false,
                    literal: line.is_literal(),
                    command: cmd,
                    edits: vec![],
                });
            }
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
            Command::Insert(kind, _) => {
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
                    'o' | 'O' => open_line(line, *kind),
                    _ => {}
                }
                self.mode = Mode::Insert;
                self.inserting = Some(Change {
                    edited: false,
                    literal: line.is_literal(),
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
                if start == end {
                    return false;
                }
                self.register = line[start..end].into();
                self.register_literal = line.is_literal();
                self.register_linewise = false;
                line.modal_replace(start..end, "");
                normal_cursor(line);
            }
            Command::Put(after, text, linewise, literal, count) => {
                if text.is_empty() {
                    return false;
                }
                // EOF linewise registers may lack a terminating separator.
                // Copies still need separators between them.
                let text = if *linewise && *count > 1 && !text.ends_with('\n') {
                    format!("{text}\n").repeat(*count)
                } else {
                    text.repeat(*count)
                };
                if *linewise {
                    let mut text = text;
                    let at = if *after {
                        let end = line_end(line, line.cursor_byte());
                        if end == line.len() {
                            // Remove one structural terminator, retaining
                            // any additional copied blank lines.
                            let body = text.strip_suffix('\n').unwrap_or(&text);
                            text = format!("\n{body}");
                            end
                        } else {
                            // A final-line register can omit its terminal
                            // newline; the existing next line still needs a
                            // separator. Do not rewrite the private register.
                            if !text.ends_with('\n') {
                                text.push('\n');
                            }
                            end + 1
                        }
                    } else {
                        line_start(line, line.cursor_byte())
                    };
                    if !*after && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    line.modal_replace(at..at, &text);
                    line.set_literal(line.is_literal() || *literal);
                    line.set_modal_cursor(if text.starts_with('\n') { at + 1 } else { at });
                    normal_cursor(line);
                    return !text.is_empty();
                }
                let at = if *after && line.cursor_byte() < line_end(line, line.cursor_byte()) {
                    next(line, line.cursor_byte())
                } else {
                    line.cursor_byte()
                };
                line.modal_replace(at..at, &text);
                line.set_literal(line.is_literal() || *literal);
                if !text.is_empty() {
                    line.left()
                }
                normal_cursor(line);
            }
            Command::Operator(op, motion, count) => {
                let Some((mut range, includes_last_line)) =
                    operator_range(line, line.cursor_byte(), *op, motion, *count)
                else {
                    return false;
                };
                let linewise =
                    motion == &op.to_string() || matches!(motion.as_str(), "gg" | "G" | "j" | "k");
                let delete_last_line = *op == 'd' && linewise && includes_last_line;
                if range.is_empty() && *op != 'c' && !(delete_last_line && range.start > 0) {
                    return false;
                }
                // The private register contains line text, never the preceding
                // separator removed only to avoid leaving a ghost final line.
                self.register = line[range.clone()].into();
                self.register_linewise = linewise;
                self.register_literal = line.is_literal();
                if delete_last_line && range.start > 0 {
                    if self.register.is_empty() {
                        self.register.push('\n');
                    }
                    range.start = prev(line, range.start);
                }
                if *op == 'y' {
                    line.set_modal_cursor(range.start);
                    return false;
                }
                line.modal_replace(range, "");
                if *op == 'c' {
                    self.mode = Mode::Insert;
                    self.inserting = Some(Change {
                        edited: false,
                        literal: line.is_literal(),
                        command: command.clone(),
                        edits: vec![],
                    })
                } else {
                    if delete_last_line {
                        let mut at = line_start(line, line.len());
                        while at < line_end(line, at)
                            && cluster(line, at).chars().all(char::is_whitespace)
                        {
                            at = next(line, at);
                        }
                        line.set_modal_cursor(at);
                    }
                    normal_cursor(line)
                }
            }
        }
        true
    }
}

fn open_line(line: &mut InputLine, kind: char) {
    if kind == 'o' {
        line.end();
        line.newline();
    } else {
        line.home();
        let at = line.cursor_byte();
        line.newline();
        line.set_modal_cursor(at);
    }
}

/// Esc has already applied the first insert; dot has only entered Insert.
/// Operator counts select a range and do not multiply replacement text.
fn replay_edits(line: &mut InputLine, change: &Change, surface: Surface, skip_first: bool) {
    let count = match change.command {
        Command::Insert(_, count) => count,
        _ => 1,
    };
    for n in usize::from(skip_first)..count {
        if n > 0 {
            if let Command::Insert(kind @ ('o' | 'O'), _) = change.command {
                open_line(line, kind);
            }
        }
        for key in &change.edits {
            let _ = insert_key(line, *key, surface);
        }
    }
}

/// Some records a handled key and whether it changed text. Motion keys remain
/// in a genuine edit's replay, without making a movement-only visit a change.
fn insert_key(line: &mut InputLine, key: KeyEvent, surface: Surface) -> Option<bool> {
    let before_len = line.len();
    let inserted = match key.code {
        KeyCode::Char(c) => {
            let mut bytes = [0; 4];
            let text = c.encode_utf8(&mut bytes);
            let changed = line.selection().is_none_or(|range| &line[range] != text);
            line.insert_char(c);
            changed
        }
        KeyCode::Backspace => {
            line.backspace();
            false
        }
        KeyCode::Delete => {
            line.delete();
            false
        }
        KeyCode::Left => {
            line.left();
            false
        }
        KeyCode::Right => {
            line.right();
            false
        }
        KeyCode::Up => {
            line.vertical(false);
            false
        }
        KeyCode::Down => {
            line.vertical(true);
            false
        }
        KeyCode::Home => {
            line.home();
            false
        }
        KeyCode::End => {
            line.end();
            false
        }
        KeyCode::Enter
            if surface == Surface::Editor || key.modifiers.contains(KeyModifiers::SHIFT) =>
        {
            let changed = line.selection().is_none_or(|range| &line[range] != "\n");
            line.newline();
            changed
        }
        KeyCode::Tab if surface == Surface::Editor => {
            let changed = line.selection().is_none_or(|range| &line[range] != "  ");
            line.insert_str("  ");
            changed
        }
        _ => return None,
    };
    Some(inserted || line.len() != before_len)
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
/// The range and whether it includes the actual final line, including an
/// empty line after a trailing LF. An exclusive end at len alone is ambiguous.
fn operator_range(
    s: &str,
    at: usize,
    op: char,
    motion: &str,
    count: usize,
) -> Option<(Range<usize>, bool)> {
    if motion == op.to_string() {
        let start = line_start(s, at);
        let mut end = start;
        let mut includes_last_line = false;
        for _ in 0..count {
            end = line_end(s, end);
            if end < s.len() {
                end += 1
            } else {
                includes_last_line = true;
                break;
            }
        }
        if op == 'c' && end > start && s[..end].ends_with('\n') {
            end -= 1;
        }
        return Some((start..end, includes_last_line));
    }
    if motion.starts_with(['i', 'a']) && motion.chars().count() == 2 {
        return object_range(s, at, motion, count).map(|range| (range, false));
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
        return Some((at..end, false));
    }
    let (target, inclusive, linewise) = motion_target(s, at, motion, count)?;
    if linewise {
        let start = line_start(s, at.min(target));
        let end = line_end(s, at.max(target));
        let mut exclusive = if end < s.len() { end + 1 } else { end };
        if op == 'c' && exclusive > start && s[..exclusive].ends_with('\n') {
            exclusive -= 1;
        }
        return Some((start..exclusive, end == s.len()));
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
    Some((start..end, false))
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
        if around {
            let mut left = pair[0];
            let mut right = pair[1] + close.len_utf8();
            let after = right;
            while right < end && kind(cluster(s, right), false) == 0 {
                right = next(s, right);
            }
            if right == after || at < pair[0] {
                right = after;
                while left > start && kind(cluster(s, prev(s, left)), false) == 0 {
                    left = prev(s, left);
                }
            }
            return Some(left..right);
        }
        return Some(if count == 2 {
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
    fn normal_u_undoes_on_both_surfaces_and_preserves_dot_and_register() {
        use unicode_segmentation::UnicodeSegmentation;
        for surface in [Surface::Composer, Surface::Editor] {
            for command in ["x", "dw"] {
                let (mut v, mut l) = normal("👩‍💻e\u{301} one two");
                for c in command.chars() {
                    v.key(&mut l, key(c), surface);
                }
                let edited = l.to_string();
                let register = v.register.clone();
                assert_ne!(edited, "👩‍💻e\u{301} one two");
                v.key(&mut l, key('u'), surface);
                assert_eq!(&*l, "👩‍💻e\u{301} one two");
                assert_eq!(v.register, register);
                assert!(l
                    .grapheme_indices(true)
                    .any(|(at, _)| at == l.cursor_byte()));
                v.key(&mut l, key('.'), surface);
                assert_eq!(&*l, edited);
            }
            let (mut v, mut l) = normal("");
            assert_eq!(v.key(&mut l, key('u'), surface), Outcome::Handled);
            assert_eq!(&*l, "");
            assert_eq!(l.cursor_byte(), 0);
            v.key(&mut l, key('i'), surface);
            v.key(&mut l, key('u'), surface);
            assert_eq!(&*l, "u", "Insert u remains typing");
        }
    }

    #[test]
    fn insert_noops_keep_prior_dot_change_on_both_surfaces() {
        for surface in [Surface::Composer, Surface::Editor] {
            for keys in [
                vec![KeyCode::Backspace],
                vec![KeyCode::Right, KeyCode::Home],
                vec![KeyCode::End, KeyCode::Delete, KeyCode::Home],
            ] {
                let (mut v, mut l) = normal("a b c");
                for c in "dwi".chars() {
                    v.key(&mut l, key(c), surface);
                }
                for code in keys {
                    v.key(&mut l, KeyEvent::new(code, KeyModifiers::NONE), surface);
                }
                v.key(
                    &mut l,
                    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                    surface,
                );
                l.set_modal_cursor(0);
                v.key(&mut l, key('.'), surface);
                assert_eq!(&*l, "c", "{surface:?}");
            }
        }
    }

    #[test]
    fn real_insert_keeps_movements_and_empty_open_changes_remain_repeatable() {
        let (mut v, mut l) = normal("ab");
        commands(&mut v, &mut l, "iX");
        for code in [KeyCode::Left, KeyCode::Right, KeyCode::Esc] {
            v.key(
                &mut l,
                KeyEvent::new(code, KeyModifiers::NONE),
                Surface::Composer,
            );
        }
        commands(&mut v, &mut l, ".");
        assert_eq!(&*l, "XXab");
        for open in ["o", "O", "cw"] {
            let (mut v, mut l) = normal("a b c");
            commands(&mut v, &mut l, open);
            v.key(
                &mut l,
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                Surface::Composer,
            );
            let before = l.to_string();
            commands(&mut v, &mut l, ".");
            assert_ne!(&*l, before, "{open} must still repeat without Insert keys");
        }
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
    #[test]
    fn counted_operators_inner_around_and_nested_blocks_keep_vim_ranges() {
        for (text, keys, expected) in [
            ("one two three", "2dw", "three"),
            ("one two three", "d2w", "three"),
            ("one two three", "d2iw", "two three"),
            ("one two three", "d2aw", "three"),
            ("alpha-beta tail", "diW", " tail"),
            ("a\nb\nc", "2dd", "c"),
            ("one\ntwo\nthree", "cjX", "X\nthree"),
            ("one\ntwo", "jcggX", "X"),
            ("\"one\" tail", "da\"", "tail"),
            ("'one' tail", "da'", "tail"),
            ("`one` tail", "da`", "tail"),
        ] {
            let (mut v, mut l) = normal(text);
            commands(&mut v, &mut l, keys);
            assert_eq!(&*l, expected, "{keys}");
        }
        let (mut v, mut l) = normal("outer [a [b] c] end");
        l.set_modal_cursor(l.find('b').unwrap());
        commands(&mut v, &mut l, "d2i[");
        assert_eq!(&*l, "outer [] end");
    }

    #[test]
    fn backwards_end_line_and_buffer_operators_and_yanks_are_composable() {
        let (mut v, mut l) = normal("one two three");
        commands(&mut v, &mut l, "2wdb");
        assert_eq!(&*l, "one three");
        commands(&mut v, &mut l, "d$");
        assert_eq!(&*l, "one ");
        let (mut v, mut l) = normal("first\nsecond\nthird");
        commands(&mut v, &mut l, "jydk");
        // An invalid operator sequence must not alter the document.
        assert_eq!(&*l, "first\nsecond\nthird");
        commands(&mut v, &mut l, "Gdgg");
        assert!(l.is_empty());
        let (mut v, mut l) = normal("one two");
        commands(&mut v, &mut l, "dw");
        commands(&mut v, &mut l, "yiw.");
        assert!(
            l.is_empty(),
            "yank must not replace the previous delete for dot repeat"
        );
    }

    #[test]
    fn dot_replays_literal_multiline_paste_without_running_commands() {
        let (mut v, mut l) = normal("tail");
        v.paste(&mut l, "dw\n👩‍💻");
        assert_eq!(&*l, "dw\n👩‍💻tail");
        v.key(
            &mut l,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            Surface::Composer,
        );
        commands(&mut v, &mut l, "G$.");
        assert_eq!(&*l, "dw\n👩‍💻taidw\n👩‍💻l");
        assert_eq!(v.mode, Mode::Normal);
    }
    #[test]
    fn dot_with_missing_text_object_never_inserts_a_ghost_replacement() {
        let (mut v, mut l) = normal("\"secret\" plain");
        commands(&mut v, &mut l, "ci\"X");
        v.key(
            &mut l,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            Surface::Composer,
        );
        commands(&mut v, &mut l, "$");
        let before = l.to_string();
        let cursor = l.cursor_byte();
        commands(&mut v, &mut l, ".");
        assert_eq!(&*l, before);
        assert_eq!(l.cursor_byte(), cursor);
        assert_eq!(v.mode, Mode::Normal);
        l.set_modal_cursor(1);
        commands(&mut v, &mut l, ".");
        assert_eq!(
            &*l, "\"X\" plain",
            "the prior valid change is still repeatable at an actual object"
        );
    }
    #[test]
    fn no_op_put_and_delete_do_not_replace_the_last_successful_change() {
        let (mut v, mut l) = normal("tail");
        commands(&mut v, &mut l, "iX");
        v.key(
            &mut l,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            Surface::Composer,
        );
        commands(&mut v, &mut l, "p$.");
        assert_eq!(
            &*l, "XtaiXl",
            "empty private register must not overwrite the last insert"
        );
        let (mut v, mut l) = normal("one two");
        commands(&mut v, &mut l, "dw");
        l.clear();
        commands(&mut v, &mut l, "x");
        l.set("next word");
        l.set_modal_cursor(0);
        commands(&mut v, &mut l, ".");
        assert_eq!(&*l, "word");
    }
}

#[cfg(test)]
#[path = "vim_count_tests.rs"]
mod count_tests;
