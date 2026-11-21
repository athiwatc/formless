use crate::{hunk_view, input::InputLine, text};
use std::{borrow::Cow, collections::VecDeque};
use unicode_segmentation::UnicodeSegmentation;

fn display(grapheme: &str, masked: bool) -> Cow<'_, str> {
    if masked {
        Cow::Borrowed("•")
    } else if grapheme.len() > 256 {
        Cow::Borrowed("…")
    } else if grapheme
        .chars()
        .any(|c| c.is_control() || text::is_bidi_control(c))
    {
        Cow::Owned(hunk_view::display_text(grapheme))
    } else {
        Cow::Borrowed(grapheme)
    }
}

fn clipped_field_grapheme(grapheme: &str, masked: bool, width: usize) -> Cow<'_, str> {
    let visible = display(grapheme, masked);
    if text::width(&visible) > width {
        Cow::Borrowed("…")
    } else {
        visible
    }
}

/// One field, clipped by display columns. Keep one cell available at the
/// insertion point. A masked grapheme produces one bullet at every position.
pub(super) fn field(
    input: &InputLine,
    width: usize,
    masked: bool,
    active: bool,
) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let (before, after) = input.split_at(if active { input.cursor_byte() } else { 0 });
    let mut left = VecDeque::new();
    let mut col = 0;
    for grapheme in before.graphemes(true).rev() {
        let visible = clipped_field_grapheme(grapheme, masked, width);
        let cells = text::width(&visible);
        if col + cells >= width {
            break;
        }
        if cells > 0 {
            col += cells;
            left.push_front(visible);
        }
    }
    let mut text = left.into_iter().collect::<String>();
    let mut end = col;
    for grapheme in after.graphemes(true) {
        let visible = clipped_field_grapheme(grapheme, masked, width);
        let cells = text::width(&visible);
        if end + cells > width {
            break;
        }
        if cells > 0 {
            text.push_str(&visible);
            end += cells;
        }
    }
    (text, col)
}

struct Rows {
    lines: VecDeque<String>,
    column: usize,
    width: usize,
    height: usize,
}

impl Rows {
    fn new_line(&mut self, scroll: bool) -> bool {
        if self.lines.len() == self.height {
            if !scroll {
                return false;
            }
            self.lines.pop_front();
        }
        self.column = 0;
        self.lines.push_back(String::new());
        true
    }

    fn fill(&mut self, source: &str, scroll: bool) -> bool {
        for grapheme in source.graphemes(true) {
            if grapheme == "\n" {
                if !self.new_line(scroll) {
                    return false;
                }
                continue;
            }
            let visible = display(grapheme, false);
            // An expanded escape can span rows in a narrow comment pane.
            for rendered in visible.graphemes(true) {
                let cells = text::width(rendered);
                if cells == 0 {
                    continue;
                }
                let (rendered, cells) = if cells > self.width {
                    ("…", 1)
                } else {
                    (rendered, cells)
                };
                if self.column + cells > self.width && !self.new_line(scroll) {
                    return false;
                }
                self.lines.back_mut().unwrap().push_str(rendered);
                self.column += cells;
            }
        }
        true
    }

    fn cursor(&mut self) -> (usize, usize) {
        if self.column == self.width {
            self.new_line(true);
        }
        (self.lines.len() - 1, self.column)
    }
}

/// Wrap the displayed comment, retaining only rows in its viewport.
pub(super) fn comment(
    input: &InputLine,
    width: usize,
    height: usize,
) -> (VecDeque<String>, (usize, usize)) {
    if width == 0 || height == 0 {
        return (VecDeque::new(), (0, 0));
    }
    let mut rows = Rows {
        lines: VecDeque::from([String::new()]),
        column: 0,
        width,
        height,
    };
    let mut cursor = None;
    let mut fits = true;
    input.visit_expanded_editor(|source, before| {
        if !before && cursor.is_none() {
            cursor = Some(rows.cursor());
        }
        if fits {
            fits = rows.fill(source, before);
        }
    });
    let cursor = cursor.unwrap_or_else(|| rows.cursor());
    (rows.lines, cursor)
}
