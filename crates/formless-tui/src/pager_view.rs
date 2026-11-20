//! Paging and horizontal clipping use the geometry of the frame being drawn.
use super::{highlight_matches, query, single_line_query_paste, text, ui, Pager};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    Find(Option<Hit>),
}

#[derive(Clone, Copy)]
struct Hit {
    row: usize,
    column: usize,
    first_grapheme_end: usize,
}

impl Hit {
    fn reveal_left(self, left: usize, width: usize, right: usize) -> usize {
        if self.column < left || self.first_grapheme_end > left.saturating_add(width) {
            self.column.min(right)
        } else {
            left
        }
    }
}

#[derive(Clone, Default)]
struct Search {
    draft: Option<String>,
    query: String,
    hits: Vec<Hit>,
    current: usize,
}

impl Search {
    fn status(&self) -> String {
        if self.query.is_empty() {
            String::new()
        } else if self.hits.is_empty() {
            "no match · ".into()
        } else {
            format!("match {}/{} · ", self.current + 1, self.hits.len())
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct View {
    gutter: usize,
    moves: Vec<Move>,
    // Pager contents belong to this instance and stay fixed; bounds are
    // recomputed from this width and the actual viewport on each draw.
    content_width: Option<usize>,
    search: Search,
    follow_hit: Option<Hit>,
}

impl View {
    pub(super) fn with_gutter(width: usize) -> Self {
        Self {
            gutter: width,
            ..Self::default()
        }
    }

    pub(super) fn search_paste(&mut self, text: &str) {
        if let Some(draft) = &mut self.search.draft {
            draft.push_str(&single_line_query_paste(text));
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

/// Only the pager owns these keys. The application keeps its existing
/// special pager actions and dismissal keys when no query is being edited.
pub(super) fn search_key(pager: &mut Pager, key: &KeyEvent) -> bool {
    if let Some(draft) = &mut pager.viewport.search.draft {
        match key.code {
            KeyCode::Esc => pager.viewport.search.draft = None,
            KeyCode::Enter if query::plain(key) => {
                let text = std::mem::take(draft);
                pager.viewport.search.draft = None;
                let hits = find_lines(pager, &text);
                pager.viewport.moves.push(Move::Find(hits.first().copied()));
                let search = &mut pager.viewport.search;
                search.query = text;
                search.hits = hits;
                search.current = 0;
            }
            KeyCode::Backspace if query::plain(key) => {
                if let Some((start, _)) = draft.grapheme_indices(true).next_back() {
                    draft.truncate(start);
                }
            }
            KeyCode::Char(ch) if ch.is_control() => {}
            _ => {
                let _ = query::edit(draft, key);
            }
        }
        return true;
    }

    let search = &mut pager.viewport.search;
    if (key.code == KeyCode::Char('/') && query::plain(key))
        || (key.code == KeyCode::Char('f') && key.modifiers == KeyModifiers::CONTROL)
    {
        search.draft = Some(search.query.clone());
        return true;
    }
    if !query::plain(key) {
        return false;
    }
    let forward = match key.code {
        KeyCode::Char('n') => true,
        KeyCode::Char('N') => false,
        KeyCode::Enter if !search.query.is_empty() => key.modifiers != KeyModifiers::SHIFT,
        _ => return false,
    };
    let count = search.hits.len();
    if count > 0 {
        search.current = if forward {
            (search.current + 1) % count
        } else {
            (search.current + count - 1) % count
        };
        pager
            .viewport
            .moves
            .push(Move::Find(Some(search.hits[search.current])));
    }
    true
}

fn find_lines(pager: &Pager, query: &str) -> Vec<Hit> {
    if formless_core::fuzzy::fold_chars(query).next().is_none() {
        return Vec::new();
    }
    pager
        .lines
        .iter()
        .enumerate()
        .filter_map(|(row, raw)| {
            let raw_line;
            let line = match pager.styled.get(row) {
                Some(line) => line,
                None => {
                    raw_line = Line::raw(raw.as_str());
                    &raw_line
                }
            };
            let gutter = usize::from(pager.viewport.gutter > 0 && pager.styled.get(row).is_some())
                .min(line.spans.len());
            let content = joined(&line.spans[gutter..]);
            let first = formless_core::fuzzy::find_folded(&content, query)?;
            let mut remaining = first.start;
            let mut column = 0;
            for grapheme in content.graphemes(true) {
                let count = grapheme.chars().count();
                let width = text::width(grapheme);
                if remaining < count {
                    return Some(Hit {
                        row,
                        column,
                        first_grapheme_end: column + width,
                    });
                }
                remaining -= count;
                column += width;
            }
            None
        })
        .collect()
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
        // Explicit scrolling resumes the normal viewport behavior.
        if !matches!(movement, Move::Find(_)) {
            pager.viewport.follow_hit = None;
        }
        match movement {
            Move::Rows(rows) => pager.top = pager.top.saturating_add_signed(rows).min(last),
            Move::Page(true) => pager.top = pager.top.saturating_add(page).min(last),
            Move::Page(false) => pager.top = pager.top.saturating_sub(page),
            Move::Columns(columns) => {
                pager.left = pager.left.saturating_add_signed(columns).min(right)
            }
            Move::Home => pager.top = 0,
            Move::End => pager.top = last,
            Move::Find(hit) => {
                pager.viewport.follow_hit = hit;
                if let Some(hit) = hit {
                    pager.top = hit.row.min(last);
                    pager.left = hit.reveal_left(pager.left, width, right);
                }
            }
        }
    }
    if let Some(hit) = pager.viewport.follow_hit {
        pager.top = hit.row.min(last);
        pager.left = hit.reveal_left(pager.left, width, right);
    }

    let search = &pager.viewport.search;
    let selected = search.hits.get(search.current).map(|hit| hit.row);
    let body: Vec<_> = (pager.top..pager.lines.len())
        .take(height)
        .map(|index| {
            let mut line = pager
                .styled
                .get(index)
                .cloned()
                .unwrap_or_else(|| Line::raw(pager.lines[index].clone()));
            let gutter = pager.viewport.gutter > 0 && pager.styled.get(index).is_some();
            if search
                .hits
                .binary_search_by_key(&index, |hit| hit.row)
                .is_ok()
            {
                // Line numbering stays fixed and outside search. Patching only
                // modifiers retains foreground and background syntax colors.
                let prefix = (gutter && !line.spans.is_empty()).then(|| line.spans.remove(0));
                (line, _) = highlight_matches(line, &search.query, |_| {
                    let emphasis = if selected == Some(index) {
                        Modifier::UNDERLINED | Modifier::REVERSED
                    } else {
                        Modifier::UNDERLINED
                    };
                    Style::default().add_modifier(emphasis)
                });
                if let Some(prefix) = prefix {
                    line.spans.insert(0, prefix);
                }
            }
            clipped_line(line, gutter, pager.left, width)
        })
        .collect();
    let title = if search.draft.is_some() {
        format!(" Enter find · Esc cancel · {} ", pager.title)
    } else {
        format!(
            " {}{} · line {}/{} · ↑↓ ←→ PgUp PgDn g G · Esc close ",
            search.status(),
            pager.title,
            (pager.top + 1).min(pager.lines.len()),
            pager.lines.len()
        )
    };
    let footer = match &search.draft {
        Some(draft) => format!(
            " / {}▏ ",
            text::truncate(
                &text::escape_bidi(draft),
                usize::from(inner.width).saturating_sub(5)
            )
        ),
        None if !search.query.is_empty() => {
            let width = usize::from(inner.width);
            let keys = if width >= 40 {
                "Enter/Shift+Enter · n/N"
            } else {
                "↵/⇧↵ n/N"
            };
            let budget = width.saturating_sub(text::width(keys) + 7);
            let query = if budget > 0 {
                text::truncate(&text::escape_bidi(&search.query), budget)
            } else {
                String::new()
            };
            format!(" / {query} · {keys} ")
        }
        None => " / find · n/N next/previous ".into(),
    };
    f.render_widget(Clear, area);
    let panel = ui::panel(title, color, Color::Cyan).title_bottom(ui::hint(&footer, color));
    f.render_widget(Paragraph::new(body).block(panel), area);
}
