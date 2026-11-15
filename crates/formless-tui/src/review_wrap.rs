//! A physical-row window for the rare preview offset that exceeds u16.
//!
//! Adapted from Ratatui 0.29's WordWrapper with trim=false. Keep the parity
//! test when updating Ratatui: ordinary preview wrapping remains Paragraph's.
//
// The MIT License (MIT)
// Copyright (c) 2016-2022 Florian Dehau
// Copyright (c) 2023-2024 The Ratatui Developers
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, Span, StyledGrapheme},
    widgets::Widget,
};
use std::collections::VecDeque;
use unicode_width::UnicodeWidthStr;

/// Render rows already emitted by the word wrapper. Paragraph's unwrapped
/// truncator differs at an overhanging wide glyph, so retain its wrapped-row
/// cell-writing behavior as well as its wrapping boundaries.
pub(super) struct Rows(pub Vec<Line<'static>>);
impl Widget for Rows {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        for (y, line) in self.0.iter().take(area.height as usize).enumerate() {
            let spare = (area.width as usize).saturating_sub(line.width());
            let mut x = match line.alignment.unwrap_or(Alignment::Left) {
                Alignment::Left => 0,
                Alignment::Center => spare / 2,
                Alignment::Right => spare,
            };
            for grapheme in line.styled_graphemes(Style::default()) {
                let width = grapheme.symbol.width();
                if width == 0 {
                    continue;
                }
                if x >= area.width as usize {
                    break;
                }
                buffer[(area.x + x as u16, area.y + y as u16)]
                    .set_symbol(grapheme.symbol)
                    .set_style(grapheme.style);
                x += width;
            }
        }
    }
}

pub(super) fn window(
    line: &Line<'_>,
    width: u16,
    first: usize,
    height: usize,
) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    fn emit(
        symbols: &[StyledGrapheme<'_>],
        alignment: Option<Alignment>,
        first: usize,
        height: usize,
        index: &mut usize,
        rows: &mut Vec<Line<'static>>,
    ) {
        if *index >= first && rows.len() < height {
            let mut row = Line::from(
                symbols
                    .iter()
                    .map(|g| Span::styled(g.symbol.to_owned(), g.style))
                    .collect::<Vec<_>>(),
            );
            row.alignment = alignment;
            rows.push(row);
        }
        *index += 1;
    }
    let width = usize::from(width);
    let mut rows = Vec::new();
    let mut index = 0;
    let mut pending_line = Vec::new();
    let mut pending_word = Vec::new();
    let mut pending_whitespace: VecDeque<StyledGrapheme<'_>> = VecDeque::new();
    let (mut line_width, mut word_width, mut whitespace_width) = (0usize, 0usize, 0usize);
    let mut non_whitespace_previous = false;
    for grapheme in line.styled_graphemes(Style::default()) {
        let symbol_width = grapheme.symbol.width();
        // This is Ratatui's whitespace definition (NBSP stays in a word).
        let whitespace = grapheme.symbol == "\u{200b}"
            || grapheme.symbol.chars().all(char::is_whitespace) && grapheme.symbol != "\u{a0}";
        if symbol_width > width {
            continue;
        }
        let word_found = non_whitespace_previous && whitespace;
        let overflow =
            pending_line.is_empty() && word_width + whitespace_width + symbol_width > width;
        if word_found || overflow {
            pending_line.extend(pending_whitespace.drain(..));
            line_width += whitespace_width;
            pending_line.append(&mut pending_word);
            line_width += word_width;
            whitespace_width = 0;
            word_width = 0;
        }
        let line_full = line_width >= width;
        let word_overflow = symbol_width > 0 && line_width + whitespace_width + word_width >= width;
        if line_full || word_overflow {
            let mut remaining_width = width.saturating_sub(line_width);
            emit(
                &pending_line,
                line.alignment,
                first,
                height,
                &mut index,
                &mut rows,
            );
            if rows.len() == height {
                return rows;
            }
            pending_line.clear();
            line_width = 0;
            while let Some(front) = pending_whitespace.front() {
                let columns = front.symbol.width();
                if columns > remaining_width {
                    break;
                }
                whitespace_width -= columns;
                remaining_width -= columns;
                pending_whitespace.pop_front();
            }
            if whitespace && pending_whitespace.is_empty() {
                continue;
            }
        }
        if whitespace {
            whitespace_width += symbol_width;
            pending_whitespace.push_back(grapheme);
        } else {
            word_width += symbol_width;
            pending_word.push(grapheme);
        }
        non_whitespace_previous = !whitespace;
    }
    if pending_line.is_empty() && pending_word.is_empty() && !pending_whitespace.is_empty() {
        emit(&[], line.alignment, first, height, &mut index, &mut rows);
    }
    pending_line.extend(pending_whitespace);
    pending_line.append(&mut pending_word);
    if !pending_line.is_empty() || index == 0 {
        emit(
            &pending_line,
            line.alignment,
            first,
            height,
            &mut index,
            &mut rows,
        );
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Color, Modifier},
        widgets::{Paragraph, Widget, Wrap},
    };

    #[test]
    fn physical_windows_match_paragraph_symbols_styles_and_whitespace() {
        for text in [
            "",
            "   ",
            "a b  c ",
            "longunbrokenword tail",
            "表a ไทย e\u{301} 👩🏽‍💻",
            "a\u{a0}b c\u{200b}d\te",
            "line\ninside",
        ] {
            let split = text
                .char_indices()
                .nth(text.chars().count() / 2)
                .map_or(text.len(), |(byte, _)| byte);
            let line = Line::from(vec![
                Span::styled(&text[..split], Style::default().fg(Color::Green)),
                Span::styled(
                    &text[split..],
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]);
            for width in 1..=20 {
                let count = Paragraph::new(line.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(width);
                for first in 0..=count {
                    let area = Rect::new(0, 0, width, 3);
                    let mut expected = Buffer::empty(area);
                    Paragraph::new(line.clone())
                        .wrap(Wrap { trim: false })
                        .scroll((first as u16, 0))
                        .render(area, &mut expected);
                    let mut actual = Buffer::empty(area);
                    Rows(window(&line, width, first, 3)).render(area, &mut actual);
                    assert_eq!(actual, expected, "{text:?}, width={width}, top={first}");
                }
            }
        }
    }
}
