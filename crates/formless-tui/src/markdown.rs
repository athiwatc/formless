//! Markdown → styled, pre-wrapped terminal lines.
//!
//! CommonMark plus the GitHub extensions models actually emit: tables, task
//! lists, strikethrough, alerts (`> [!NOTE]`), footnote refs and `$math$`.
//! Paragraphs wrap at word boundaries to the pane width and keep their list
//! or quote prefix on continuation rows. Fenced code is syntax-highlighted;
//! ```` ```mermaid ```` blocks are drawn as diagrams when the kind is
//! supported and they fit, otherwise shown as source.

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use ratatui::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::highlight::{diff_line, Highlighter};
use super::ui::dim;
use super::{bold_fg, fg, mermaid};

pub(crate) fn render(text: &str, color: bool, indent: &str, width: usize) -> Vec<Line<'static>> {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH;
    let mut r = Renderer {
        color,
        width: width.max(indent.width() + 16),
        indent: indent.to_string(),
        ..Default::default()
    };
    for ev in Parser::new_ext(text, opts) {
        r.event(ev);
    }
    r.flush();
    r.out
}

/// A line prefix contributed by an enclosing block.
struct Prefix {
    /// Used once, on the block's first row (a list marker).
    first: Option<Vec<Span<'static>>>,
    rest: Vec<Span<'static>>,
}

struct Table {
    aligns: Vec<Alignment>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    row: Vec<Vec<Span<'static>>>,
    cell: Vec<Span<'static>>,
    header_rows: usize,
}

#[derive(Default)]
struct Renderer {
    color: bool,
    width: usize,
    indent: String,
    out: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    prefixes: Vec<Prefix>,
    lists: Vec<Option<u64>>,
    code: Option<(String, String)>,
    table: Option<Table>,
    link: Option<(String, usize)>,
    need_gap: bool,
}

impl Renderer {
    fn style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::default(), |a, s| a.patch(*s))
    }

    fn push_text(&mut self, text: impl Into<String>, extra: Style) {
        let span = Span::styled(text.into(), self.style().patch(extra));
        match &mut self.table {
            Some(t) => t.cell.push(span),
            None => self.spans.push(span),
        }
    }

    /// Prefix spans for the next row; list markers are consumed once.
    fn prefix(&mut self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        if !self.indent.is_empty() {
            spans.push(Span::raw(self.indent.clone()));
        }
        for p in &mut self.prefixes {
            match p.first.take() {
                Some(first) => spans.extend(first),
                None => spans.extend(p.rest.iter().cloned()),
            }
        }
        spans
    }

    fn prefix_width(&self) -> usize {
        self.indent.width()
            + self
                .prefixes
                .iter()
                .flat_map(|p| p.rest.iter())
                .map(|s| s.content.width())
                .sum::<usize>()
    }

    fn emit(&mut self, body: Vec<Span<'static>>) {
        let mut spans = self.prefix();
        spans.extend(body);
        self.out.push(Line::from(spans));
    }

    /// Blank separator between blocks (quote bars continue through it;
    /// pending list markers are kept for the block's real first row).
    fn gap(&mut self) {
        if self.need_gap && !self.out.is_empty() {
            let mut spans: Vec<Span<'static>> = self
                .prefixes
                .iter()
                .flat_map(|p| p.rest.iter().cloned())
                .collect();
            while spans.last().is_some_and(|s| s.content.trim().is_empty()) {
                spans.pop();
            }
            if let Some(last) = spans.last_mut() {
                last.content = last.content.trim_end().to_string().into();
                spans.insert(0, Span::raw(self.indent.clone()));
            }
            self.out.push(Line::from(spans));
        }
        self.need_gap = false;
    }

    /// Word-wrap the pending inline spans into rows.
    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        let avail = self.width.saturating_sub(self.prefix_width()).max(8);
        for row in wrap(spans, avail) {
            self.emit(row);
        }
    }

    fn event(&mut self, ev: Event) {
        let color = self.color;
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => {
                if let Some((_, body)) = &mut self.code {
                    body.push_str(&t);
                } else {
                    self.push_text(t.to_string(), Style::default());
                }
            }
            Event::Code(t) => self.push_text(t.to_string(), fg(color, Color::Yellow)),
            Event::InlineMath(t) | Event::DisplayMath(t) => self.push_text(
                t.to_string(),
                fg(color, Color::LightMagenta).add_modifier(Modifier::ITALIC),
            ),
            Event::Html(t) | Event::InlineHtml(t) => {
                let tag = t.trim().to_ascii_lowercase();
                if matches!(tag.as_str(), "<br>" | "<br/>" | "<br />") {
                    self.push_text("\n", Style::default());
                } else if !tag.starts_with("<!--") {
                    self.push_text(t.trim_end_matches('\n').to_string(), dim(color));
                }
            }
            Event::SoftBreak => self.push_text(" ", Style::default()),
            Event::HardBreak => self.push_text("\n", Style::default()),
            Event::Rule => {
                self.flush();
                self.gap();
                let w = self.width.saturating_sub(self.prefix_width()).min(60);
                self.emit(vec![Span::styled("─".repeat(w), dim(color))]);
                self.need_gap = true;
            }
            Event::TaskListMarker(done) => {
                let (mark, style) = if done {
                    ("☑ ", fg(color, Color::Green))
                } else {
                    ("☐ ", dim(color))
                };
                // GitHub shows the checkbox instead of the bullet.
                match self.prefixes.last_mut() {
                    Some(p) if p.first.is_some() => {
                        p.first = Some(vec![Span::styled(mark, style)]);
                    }
                    _ => self.push_text(mark, style),
                }
            }
            Event::FootnoteReference(name) => self.push_text(format!("[^{name}]"), dim(color)),
        }
    }

    fn start(&mut self, tag: Tag) {
        let color = self.color;
        match tag {
            Tag::Paragraph => self.gap(),
            Tag::Heading { level, .. } => {
                self.gap();
                let style = match level {
                    HeadingLevel::H1 => {
                        bold_fg(color, Color::Cyan).add_modifier(Modifier::UNDERLINED)
                    }
                    HeadingLevel::H2 => bold_fg(color, Color::Cyan),
                    HeadingLevel::H3 => bold_fg(color, Color::Blue),
                    _ => Style::default().add_modifier(Modifier::BOLD),
                };
                self.styles.push(style);
            }
            Tag::BlockQuote(kind) => {
                self.flush();
                self.gap();
                let (bar, title) = match kind {
                    None => (dim(color), None),
                    Some(k) => {
                        let (c, t) = match k {
                            BlockQuoteKind::Note => (Color::Blue, "ℹ Note"),
                            BlockQuoteKind::Tip => (Color::Green, "★ Tip"),
                            BlockQuoteKind::Important => (Color::Magenta, "‼ Important"),
                            BlockQuoteKind::Warning => (Color::Yellow, "⚠ Warning"),
                            BlockQuoteKind::Caution => (Color::Red, "✖ Caution"),
                        };
                        (fg(color, c), Some((t, bold_fg(color, c))))
                    }
                };
                self.prefixes.push(Prefix {
                    first: None,
                    rest: vec![Span::styled("│ ", bar)],
                });
                if let Some((t, style)) = title {
                    self.emit(vec![Span::styled(t, style)]);
                }
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.gap();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split([' ', ',', '{']).next().unwrap_or("").to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.gap();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                self.gap();
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        Span::styled(m, fg(color, Color::Cyan))
                    }
                    _ => Span::styled(
                        format!("{} ", ["•", "◦", "▪"][depth % 3]),
                        fg(color, Color::Cyan),
                    ),
                };
                let w = marker.content.width();
                self.prefixes.push(Prefix {
                    first: Some(vec![marker]),
                    rest: vec![Span::raw(" ".repeat(w))],
                });
            }
            Tag::Table(aligns) => {
                self.flush();
                self.gap();
                self.table = Some(Table {
                    aligns,
                    rows: Vec::new(),
                    row: Vec::new(),
                    cell: Vec::new(),
                    header_rows: 0,
                });
            }
            Tag::TableHead => self
                .styles
                .push(Style::default().add_modifier(Modifier::BOLD)),
            Tag::TableRow | Tag::TableCell => {}
            Tag::Emphasis => self
                .styles
                .push(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self
                .styles
                .push(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self
                .styles
                .push(Style::default().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                let at = self
                    .table
                    .as_ref()
                    .map_or(self.spans.len(), |t| t.cell.len());
                self.link = Some((dest_url.to_string(), at));
                self.styles
                    .push(fg(color, Color::Blue).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { dest_url, .. } => {
                self.push_text("[image: ", dim(color));
                self.link = Some((dest_url.to_string(), usize::MAX));
                self.styles.push(dim(color));
            }
            Tag::FootnoteDefinition(name) => {
                self.flush();
                self.gap();
                self.push_text(format!("[^{name}]: "), dim(color));
            }
            Tag::HtmlBlock => self.gap(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        let color = self.color;
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock | TagEnd::FootnoteDefinition => {
                self.flush();
                self.need_gap = true;
            }
            TagEnd::Heading(_) => {
                self.flush();
                self.styles.pop();
                self.need_gap = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.prefixes.pop();
                self.need_gap = true;
            }
            TagEnd::CodeBlock => {
                if let Some((lang, body)) = self.code.take() {
                    self.code_block(&lang, &body);
                }
                self.need_gap = true;
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.need_gap = true;
                }
            }
            TagEnd::Item => {
                self.flush();
                // An item with no text still shows its marker.
                if self.prefixes.last().is_some_and(|p| p.first.is_some()) {
                    self.emit(Vec::new());
                }
                self.prefixes.pop();
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.table_lines(t);
                }
                self.need_gap = true;
            }
            TagEnd::TableHead => {
                self.styles.pop();
                if let Some(t) = &mut self.table {
                    t.rows.push(std::mem::take(&mut t.row));
                    t.header_rows = 1;
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = &mut self.table {
                    t.rows.push(std::mem::take(&mut t.row));
                }
            }
            TagEnd::TableCell => {
                if let Some(t) = &mut self.table {
                    t.row.push(std::mem::take(&mut t.cell));
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some((url, at)) = self.link.take() {
                    let spans = self.table.as_ref().map_or(&self.spans, |t| &t.cell);
                    let text: String = spans[at.min(spans.len())..]
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect();
                    let bare = url.trim_start_matches("mailto:");
                    if !url.is_empty() && text != url && text != bare && !url.starts_with('#') {
                        self.push_text(format!(" ({url})"), dim(color));
                    }
                }
            }
            TagEnd::Image => {
                self.styles.pop();
                if let Some((url, _)) = self.link.take() {
                    self.push_text(format!("]({url})"), dim(color));
                }
            }
            _ => {}
        }
    }

    fn code_block(&mut self, lang: &str, body: &str) {
        let color = self.color;
        let avail = self.width.saturating_sub(self.prefix_width());
        let body = body.strip_suffix('\n').unwrap_or(body);
        if lang.eq_ignore_ascii_case("mermaid") {
            if let Some(d) = mermaid::render(body, color, avail) {
                self.emit(vec![Span::styled(
                    format!("◇ mermaid · {}", d.kind),
                    dim(color),
                )]);
                for line in d.lines {
                    self.emit(line.spans);
                }
                return;
            }
        }
        let label = if lang.eq_ignore_ascii_case("mermaid") {
            "╭─ mermaid (source — diagram too wide or not drawable here)".to_string()
        } else if lang.is_empty() {
            "╭─".to_string()
        } else {
            format!("╭─ {lang}")
        };
        self.emit(vec![Span::styled(label, dim(color))]);
        let diff = matches!(lang, "diff" | "patch");
        let mut hl = Highlighter::new(lang, color);
        let code_w = avail.saturating_sub(2).max(8);
        for raw in body.split('\n') {
            let raw = raw.replace('\t', "    ");
            let spans = if diff {
                vec![diff_line(&raw, color)]
            } else if let Some(h) = &mut hl {
                h.line(&raw)
            } else {
                vec![Span::styled(raw.clone(), fg(color, Color::Yellow))]
            };
            for row in hard_wrap(spans, code_w) {
                let mut line = vec![Span::styled("│ ", dim(color))];
                line.extend(row);
                self.emit(line);
            }
        }
        self.emit(vec![Span::styled("╰─", dim(color))]);
    }

    fn table_lines(&mut self, t: Table) {
        let color = self.color;
        let cols = t.rows.iter().map(Vec::len).max().unwrap_or(0);
        if cols == 0 {
            return;
        }
        let cell_w = |c: &Vec<Span>| c.iter().map(|s| s.content.width()).sum::<usize>();
        let mut natural = vec![1usize; cols];
        for row in &t.rows {
            for (i, c) in row.iter().enumerate() {
                natural[i] = natural[i].max(cell_w(c));
            }
        }
        let avail = self.width.saturating_sub(self.prefix_width());
        let frame = 3 * cols + 1;
        // Too narrow to keep every column legible: one record per row.
        if avail < frame + cols * MIN_COL {
            self.table_records(t);
            return;
        }
        let widths = fit_widths(&natural, avail - frame);
        let border = |l: &str, m: &str, r: &str| {
            let mut s = l.to_string();
            for (i, w) in widths.iter().enumerate() {
                s.push_str(&"─".repeat(w + 2));
                s.push_str(if i + 1 == cols { r } else { m });
            }
            Span::styled(s, dim(color))
        };
        self.emit(vec![border("┌", "┬", "┐")]);
        for (ri, row) in t.rows.iter().enumerate() {
            if ri == t.header_rows && t.header_rows > 0 {
                self.emit(vec![border("├", "┼", "┤")]);
            }
            // Each cell wraps within its column; the row is as tall as
            // its tallest cell.
            let cells: Vec<Vec<Vec<Span<'static>>>> = widths
                .iter()
                .enumerate()
                .map(|(i, w)| wrap(row.get(i).cloned().unwrap_or_default(), *w))
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for k in 0..height {
                let mut line = vec![Span::styled("│", dim(color))];
                for (i, w) in widths.iter().enumerate() {
                    let cell = truncate_spans(cells[i].get(k).cloned().unwrap_or_default(), *w);
                    let pad = w - cell_w(&cell);
                    let (l, r) = match t.aligns.get(i) {
                        Some(Alignment::Right) => (pad, 0),
                        Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                        _ => (0, pad),
                    };
                    line.push(Span::raw(" ".repeat(l + 1)));
                    line.extend(cell);
                    line.push(Span::raw(" ".repeat(r + 1)));
                    line.push(Span::styled("│", dim(color)));
                }
                self.emit(line);
            }
        }
        self.emit(vec![border("└", "┴", "┘")]);
    }

    /// A table too wide for the pane, as `key: value` records.
    fn table_records(&mut self, t: Table) {
        let color = self.color;
        let headers: Vec<Vec<Span<'static>>> = if t.header_rows > 0 {
            t.rows[0].clone()
        } else {
            Vec::new()
        };
        for (n, row) in t.rows.iter().skip(t.header_rows).enumerate() {
            self.emit(vec![Span::styled(
                format!("▸ row {}", n + 1),
                dim(color).add_modifier(Modifier::BOLD),
            )]);
            for (i, cell) in row.iter().enumerate() {
                let mut line = vec![Span::raw("  ")];
                match headers.get(i).filter(|h| !h.is_empty()) {
                    Some(h) => line.extend(h.iter().map(|s| {
                        Span::styled(s.content.clone(), s.style.add_modifier(Modifier::BOLD))
                    })),
                    None => line.push(Span::styled(
                        format!("col {}", i + 1),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                }
                line.push(Span::raw(": "));
                line.extend(cell.iter().cloned());
                self.emit(line);
            }
        }
    }
}

/// The narrowest a table column gets before the table becomes records.
const MIN_COL: usize = 4;

/// Column widths for `room` characters: columns narrower than an equal
/// share keep their natural width, and the rest share what's left in
/// proportion to their natural widths (never below `MIN_COL`).
fn fit_widths(natural: &[usize], room: usize) -> Vec<usize> {
    if natural.iter().sum::<usize>() <= room {
        return natural.to_vec();
    }
    let mut widths = natural.to_vec();
    let mut flex: Vec<usize> = (0..natural.len()).collect();
    let mut left = room;
    loop {
        let share = left / flex.len().max(1);
        let (small, big): (Vec<usize>, Vec<usize>) =
            flex.iter().partition(|&&i| natural[i] <= share);
        if small.is_empty() {
            let total: usize = big.iter().map(|&i| natural[i]).sum();
            for &i in &big {
                widths[i] = (natural[i] * left / total.max(1)).max(MIN_COL);
            }
            break;
        }
        left -= small.iter().map(|&i| natural[i]).sum::<usize>();
        flex = big;
        if flex.is_empty() {
            break;
        }
    }
    // The minimum can overshoot: take back from the widest.
    while widths.iter().sum::<usize>() > room {
        let i = (0..widths.len()).max_by_key(|&i| widths[i]).unwrap_or(0);
        if widths[i] <= MIN_COL {
            break;
        }
        widths[i] -= 1;
    }
    widths
}

/// Greedy word wrap of styled spans; `\n` inside a span forces a break.
fn wrap(spans: Vec<Span<'static>>, avail: usize) -> Vec<Vec<Span<'static>>> {
    // Split into words (non-space runs, possibly multi-style) and spaces.
    enum Piece {
        Word(Vec<Span<'static>>, usize),
        Space(Style),
        Break,
    }
    let mut pieces: Vec<Piece> = Vec::new();
    for span in spans {
        let style = span.style;
        let mut word = String::new();
        let flush_word = |word: &mut String, pieces: &mut Vec<Piece>| {
            if word.is_empty() {
                return;
            }
            let w = word.width();
            let s = Span::styled(std::mem::take(word), style);
            match pieces.last_mut() {
                Some(Piece::Word(parts, pw)) => {
                    parts.push(s);
                    *pw += w;
                }
                _ => pieces.push(Piece::Word(vec![s], w)),
            }
        };
        for ch in span.content.chars() {
            if ch == '\n' {
                flush_word(&mut word, &mut pieces);
                pieces.push(Piece::Break);
            } else if ch.is_whitespace() {
                flush_word(&mut word, &mut pieces);
                if !matches!(pieces.last(), Some(Piece::Space(_))) {
                    pieces.push(Piece::Space(style));
                }
            } else {
                word.push(ch);
            }
        }
        flush_word(&mut word, &mut pieces);
    }
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut row_w = 0;
    let mut space: Option<Style> = None;
    for p in pieces {
        match p {
            Piece::Break => {
                rows.push(std::mem::take(&mut row));
                row_w = 0;
                space = None;
            }
            Piece::Space(s) => {
                if row_w > 0 {
                    space = Some(s);
                }
            }
            Piece::Word(parts, w) => {
                let sp = usize::from(space.is_some());
                if row_w > 0 && row_w + sp + w > avail {
                    rows.push(std::mem::take(&mut row));
                    row_w = 0;
                } else if let Some(s) = space {
                    row.push(Span::styled(" ", s));
                    row_w += 1;
                }
                space = None;
                if w > avail {
                    // A word longer than the row: hard-split it.
                    let mut chunks = hard_wrap(parts, avail);
                    let last = chunks.pop().unwrap_or_default();
                    for c in chunks {
                        row.extend(c);
                        rows.push(std::mem::take(&mut row));
                    }
                    row_w = last.iter().map(|s| s.content.width()).sum();
                    row.extend(last);
                } else {
                    row.extend(parts);
                    row_w += w;
                }
            }
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// Split spans into rows of at most `avail` columns, ignoring words.
fn hard_wrap(spans: Vec<Span<'static>>, avail: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows = vec![Vec::new()];
    let mut w = 0;
    for span in spans {
        let mut buf = String::new();
        for ch in span.content.chars() {
            let cw = ch.width().unwrap_or(0);
            if w + cw > avail && w > 0 {
                if !buf.is_empty() {
                    rows.last_mut()
                        .unwrap()
                        .push(Span::styled(std::mem::take(&mut buf), span.style));
                }
                rows.push(Vec::new());
                w = 0;
            }
            buf.push(ch);
            w += cw;
        }
        if !buf.is_empty() {
            rows.last_mut().unwrap().push(Span::styled(buf, span.style));
        }
    }
    rows
}

fn truncate_spans(spans: Vec<Span<'static>>, max: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.width()).sum();
    if total <= max {
        return spans;
    }
    let mut out = Vec::new();
    let mut w = 0;
    for span in spans {
        let mut buf = String::new();
        for ch in span.content.chars() {
            let cw = ch.width().unwrap_or(0);
            if w + cw + 1 > max {
                buf.push('…');
                out.push(Span::styled(buf, span.style));
                return out;
            }
            buf.push(ch);
            w += cw;
        }
        out.push(Span::styled(buf, span.style));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(md: &str, width: usize) -> Vec<String> {
        render(md, false, "  ", width)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn strips_markers_and_styles_spans() {
        let out = render(
            "# Title\nThe **bold** and `code` and ~~gone~~.",
            true,
            "  ",
            80,
        );
        let text: Vec<String> = out
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text, ["  Title", "", "  The bold and code and gone."]);
        let find = |s: &str| out[2].spans.iter().find(|x| x.content == s).unwrap().style;
        assert!(find("bold").add_modifier.contains(Modifier::BOLD));
        assert_eq!(find("code").fg, Some(Color::Yellow));
        assert!(find("gone").add_modifier.contains(Modifier::CROSSED_OUT));
        assert!(
            lines("not **closed", 80)[0].contains("**closed"),
            "unclosed stays literal"
        );
    }

    #[test]
    fn wraps_with_list_and_quote_prefixes() {
        let out = lines("- one two three four five six seven eight\n  - nested\n1. first\n2. [ ] todo\n- [x] done\n\n> quoted words that wrap around", 24);
        assert_eq!(out[0], "  • one two three four");
        assert_eq!(
            out[1], "    five six seven eight",
            "continuation keeps the hanging indent"
        );
        assert_eq!(out[2], "    ◦ nested", "{out:?}");
        assert!(out.iter().any(|l| l == "  1. first"), "{out:?}");
        assert!(out.iter().any(|l| l.contains("☐ todo")), "{out:?}");
        assert!(out.iter().any(|l| l == "  ☑ done"), "{out:?}");
        let quoted: Vec<_> = out.iter().filter(|l| l.starts_with("  │ ")).collect();
        assert!(quoted.len() >= 2, "{out:?}");
        assert!(out.iter().all(|l| l.width() <= 24), "{out:?}");
    }

    #[test]
    fn tables_alerts_rules_and_links() {
        let out = lines(
            "| Name | Qty |\n|:-----|----:|\n| apple | 3 |\n| kiwi | 12 |\n\n---\n\n> [!WARNING]\n> careful\n\nSee [docs](https://x.dev) or <https://y.dev>.",
            60,
        );
        assert_eq!(out[0], "  ┌───────┬─────┐");
        assert_eq!(out[1], "  │ Name  │ Qty │");
        assert_eq!(out[2], "  ├───────┼─────┤");
        assert_eq!(out[3], "  │ apple │   3 │", "right-aligned column");
        assert!(out.iter().any(|l| l.starts_with("  ────")), "{out:?}");
        assert!(out.iter().any(|l| l == "  │ ⚠ Warning"), "{out:?}");
        let last = out.last().unwrap();
        assert_eq!(last, "  See docs (https://x.dev) or https://y.dev.");
    }

    #[test]
    fn narrow_tables_wrap_cells_then_become_records() {
        let md = "| Name | Notes |\n|---|---|\n| apple | a crisp red fruit that keeps well in a cool place |\n| kiwi | small and green |";
        // Narrower than the table: the long cell wraps, nothing is cut.
        let out = lines(md, 36);
        assert!(out.iter().all(|l| l.chars().count() <= 36), "{out:#?}");
        let text = out.join(" ");
        for word in ["crisp", "keeps", "place", "green"] {
            assert!(text.contains(word), "{word} lost: {out:#?}");
        }
        assert!(out.iter().any(|l| l.contains("│ apple │")), "{out:#?}");
        // Too narrow for every column: one record per row.
        let wide = "| a | b | c | d | e |\n|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 |";
        let out = lines(wide, 20);
        assert!(out.iter().any(|l| l.trim() == "▸ row 1"), "{out:#?}");
        assert!(out.iter().any(|l| l.trim() == "c: 3"), "{out:#?}");
    }

    #[test]
    fn column_widths_shrink_proportionally() {
        assert_eq!(fit_widths(&[5, 10], 20), vec![5, 10], "fits as is");
        let w = fit_widths(&[5, 40, 20], 35);
        assert_eq!(w[0], 5, "a small column keeps its width");
        assert!(w[1] > w[2] && w.iter().sum::<usize>() <= 35, "{w:?}");
        let tiny = fit_widths(&[30, 30], 8);
        assert!(tiny.iter().all(|&x| x >= MIN_COL), "{tiny:?}");
    }

    #[test]
    fn code_blocks_highlight_and_mermaid_draws() {
        let out = render("```rust\nfn main() {}\n```", true, "", 60);
        assert_eq!(out[0].spans[0].content, "╭─ rust");
        let kw = out[1].spans.iter().find(|s| s.content == "fn").unwrap();
        assert_eq!(kw.style.fg, Some(Color::Magenta));
        let out = lines("```mermaid\ngraph LR\nA --> B\n```", 60);
        assert_eq!(out[0], "  ◇ mermaid · flowchart");
        assert!(out.iter().any(|l| l.contains("│ A ├───▶│ B │")), "{out:?}");
        let out = lines("```mermaid\ngantt\ntitle x\n```", 60);
        assert!(out[0].contains("mermaid (source"), "{out:?}");
        assert!(out.iter().any(|l| l.contains("gantt")));
    }
}
