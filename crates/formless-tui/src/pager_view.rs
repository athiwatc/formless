//! Paging and horizontal clipping use the geometry of the frame being drawn.
use super::{text, ui, Pager};
use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy)]
enum Move {
    Rows(isize),
    Page(bool),
    Columns(isize),
    Home,
    End,
}

#[derive(Clone, Default)]
pub(super) struct View {
    gutter: usize,
    moves: Vec<Move>,
    // Pager contents belong to this instance and stay fixed; bounds are
    // recomputed from this width and the actual viewport on each draw.
    content_width: Option<usize>,
}

impl View {
    pub(super) fn with_gutter(width: usize) -> Self {
        Self {
            gutter: width,
            ..Self::default()
        }
    }

    pub(super) fn key(&mut self, code: KeyCode) {
        self.moves.push(match code {
            KeyCode::Down | KeyCode::Char('j') => Move::Rows(1),
            KeyCode::Up | KeyCode::Char('k') => Move::Rows(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => Move::Page(true),
            KeyCode::PageUp => Move::Page(false),
            KeyCode::Home | KeyCode::Char('g') => Move::Home,
            KeyCode::End | KeyCode::Char('G') => Move::End,
            KeyCode::Right | KeyCode::Char('l') => Move::Columns(8),
            KeyCode::Left | KeyCode::Char('h') => Move::Columns(-8),
            _ => return,
        });
    }
}

fn joined<'a>(spans: &'a [Span<'_>]) -> Cow<'a, str> {
    match spans {
        [] => Cow::Borrowed(""),
        [span] => Cow::Borrowed(span.content.as_ref()),
        spans => Cow::Owned(spans.iter().map(|span| span.content.as_ref()).collect()),
    }
}

#[cfg(test)]
#[test]
fn pager_clipping_preserves_clusters_crossing_style_spans() {
    let red = Style::default().fg(Color::Red);
    let green = Style::default().fg(Color::Green);
    let line = Line::from(vec![
        Span::raw("12345678"),
        Span::styled("e", red),
        Span::styled("\u{301}👩", green),
        Span::styled("\u{200d}💻", red),
    ]);
    let clipped = clipped_line(line, false, 8, 3);
    assert_eq!(
        clipped.spans,
        [
            Span::styled("e\u{301}", red),
            Span::styled("👩\u{200d}💻", green),
        ]
    );
}

fn line_width(content: &str) -> usize {
    content.graphemes(true).map(text::width).sum()
}

fn widest_line(pager: &Pager) -> usize {
    pager
        .lines
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            if let Some(line) = pager.styled.get(index) {
                let start = usize::from(pager.viewport.gutter > 0).min(line.spans.len());
                line_width(&joined(&line.spans[start..]))
            } else {
                let line = Line::raw(raw.as_str());
                line_width(&joined(&line.spans))
            }
        })
        .max()
        .unwrap_or_default()
}

fn append(span_list: &mut Vec<Span<'static>>, content: &str, style: Style) {
    if let Some(span) = span_list.last_mut().filter(|span| span.style == style) {
        span.content.to_mut().push_str(content);
    } else {
        span_list.push(Span::styled(content.to_owned(), style));
    }
}

fn clipped_line(mut line: Line<'static>, gutter: bool, left: usize, width: usize) -> Line<'static> {
    let original = std::mem::take(&mut line.spans);
    let fixed = usize::from(gutter).min(original.len());
    line.spans.extend(original[..fixed].iter().cloned());
    let spans = &original[fixed..];
    let content = joined(spans);
    let mut index = 0;
    let mut span_end = spans.first().map_or(0, |span| span.content.len());
    let right = left.saturating_add(width);
    let mut column = 0_usize;
    for (start, grapheme) in content.grapheme_indices(true) {
        let first = column;
        column = column.saturating_add(text::width(grapheme));
        if column <= left {
            continue;
        }
        if first >= right {
            break;
        }
        while start >= span_end {
            index += 1;
            span_end += spans[index].content.len();
        }
        // A cluster spanning style runs uses the leading span's style. Never
        // split its combining marks or joiners into other terminal cells.
        let style = spans[index].style;
        if first >= left && column <= right {
            append(&mut line.spans, grapheme, style);
        } else {
            // A partially visible wide grapheme becomes blank cells. Keep
            // subsequent glyphs aligned with the same source columns.
            let visible = column.min(right).saturating_sub(first.max(left));
            append(&mut line.spans, &" ".repeat(visible), style);
        }
    }
    line
}

#[cfg(test)]
#[test]
fn pager_clipping_keeps_adjacent_rows_in_source_column_alignment() {
    let wide = clipped_line(Line::raw("1234567界X"), false, 8, 2);
    let narrow = clipped_line(Line::raw("123456789X"), false, 8, 2);
    assert_eq!(wide.to_string(), " X");
    assert_eq!(narrow.to_string(), "9X");
}

#[cfg(test)]
#[test]
fn pager_width_ignores_raw_line_terminators() {
    let pager = Pager {
        title: "notes fixture".into(),
        lines: vec!["a\n\n\n".into()],
        top: 0,
        left: 0,
        styled: Vec::new(),
        viewport: View::default(),
    };
    assert_eq!(widest_line(&pager), 1);
}

pub(super) fn draw(f: &mut Frame, pager: &mut Pager, color: bool) {
    let area = f.area();
    let inner = ui::panel("", color, Color::Cyan).inner(area);
    let height = usize::from(inner.height);
    let page = height.max(1);
    let last = pager.lines.len().saturating_sub(page);
    let width = usize::from(inner.width).saturating_sub(pager.viewport.gutter);
    let content_width = pager
        .viewport
        .content_width
        .unwrap_or_else(|| widest_line(pager));
    pager.viewport.content_width = Some(content_width);
    let right = if width == 0 {
        0
    } else {
        content_width.saturating_sub(width)
    };
    pager.top = pager.top.min(last);
    pager.left = pager.left.min(right);
    for movement in pager.viewport.moves.drain(..) {
        match movement {
            Move::Rows(rows) => pager.top = pager.top.saturating_add_signed(rows).min(last),
            Move::Page(true) => pager.top = pager.top.saturating_add(page).min(last),
            Move::Page(false) => pager.top = pager.top.saturating_sub(page),
            Move::Columns(columns) => {
                pager.left = pager.left.saturating_add_signed(columns).min(right)
            }
            Move::Home => pager.top = 0,
            Move::End => pager.top = last,
        }
    }

    let body: Vec<_> = (pager.top..pager.lines.len())
        .take(height)
        .map(|index| {
            let line = pager
                .styled
                .get(index)
                .cloned()
                .unwrap_or_else(|| Line::raw(pager.lines[index].clone()));
            clipped_line(line, pager.viewport.gutter > 0, pager.left, width)
        })
        .collect();
    let title = format!(
        " {} · line {}/{} · ↑↓ ←→ PgUp PgDn g G · Esc close ",
        pager.title,
        (pager.top + 1).min(pager.lines.len()),
        pager.lines.len()
    );
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(body).block(ui::panel(title, color, Color::Cyan)),
        area,
    );
}
