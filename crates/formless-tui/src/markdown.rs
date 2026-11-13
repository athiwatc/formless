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
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::highlight::Highlighter;
use super::ui::dim;
use super::{bold_fg, fg, mermaid};

thread_local! {
    /// `tui.code_line_numbers`: dim line numbers in code blocks.
    static LINE_NUMBERS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn set_line_numbers(on: bool) {
    LINE_NUMBERS.with(|l| l.set(on));
}

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
    /// An image's source, kept apart from `link` so an image inside a link
    /// leaves the link's destination intact (#875).
    image: Option<String>,
    /// Styles opened by inline HTML tags (`<kbd>`, `<b>`…), innermost last.
    html: Vec<(String, Style)>,
    need_gap: bool,
    /// Code blocks so far, so each is labelled `[n]` like `/copy code n`.
    code_index: usize,
}

impl Renderer {
    fn style(&self) -> Style {
        self.styles
            .iter()
            .chain(self.html.iter().map(|(_, s)| s))
            .fold(Style::default(), |a, s| a.patch(*s))
    }

    /// Where the next pushed span lands (for a link's text).
    fn mark(&self) -> usize {
        self.table
            .as_ref()
            .map_or(self.spans.len(), |t| t.cell.len())
    }

    /// Close a link: its URL follows its text unless the text is the URL.
    fn finish_link(&mut self) {
        if let Some((url, at)) = self.link.take() {
            let spans = self.table.as_ref().map_or(&self.spans, |t| &t.cell);
            let text: String = spans[at.min(spans.len())..]
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            let bare = url.trim_start_matches("mailto:");
            if !url.is_empty() && text != url && text != bare && !url.starts_with('#') {
                self.push_text(format!(" ({url})"), dim(self.color));
            }
        }
    }

    /// Inline HTML as its text content (#875): common formatting tags
    /// style what they wrap, `<a href>` is a link, `<br>` breaks the line,
    /// and any other tag (and every comment) is dropped, not shown.
    fn html(&mut self, raw: &str) {
        let color = self.color;
        let mut rest = raw;
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix("<!--") {
                rest = after.split_once("-->").map_or("", |(_, r)| r);
                continue;
            }
            let Some(lt) = rest.find('<') else {
                self.push_text(rest.trim_end_matches('\n').to_string(), Style::default());
                break;
            };
            if lt > 0 {
                self.push_text(rest[..lt].to_string(), Style::default());
            }
            let Some(gt) = rest[lt..].find('>') else {
                self.push_text(rest[lt..].to_string(), Style::default());
                break;
            };
            let tag = &rest[lt + 1..lt + gt];
            rest = &rest[lt + gt + 1..];
            let closing = tag.starts_with('/');
            let body = tag.trim_start_matches('/').trim_end_matches('/').trim();
            let name: String = body
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();
            if name == "br" {
                self.push_text("\n", Style::default());
                continue;
            }
            if closing {
                if let Some(i) = self.html.iter().rposition(|(n, _)| *n == name) {
                    self.html.truncate(i);
                    if name == "a" {
                        self.finish_link();
                    }
                }
                continue;
            }
            let style = match name.as_str() {
                "b" | "strong" => Style::default().add_modifier(Modifier::BOLD),
                "i" | "em" => Style::default().add_modifier(Modifier::ITALIC),
                "s" | "del" | "strike" => Style::default().add_modifier(Modifier::CROSSED_OUT),
                "u" | "ins" => Style::default().add_modifier(Modifier::UNDERLINED),
                "code" | "kbd" | "samp" => fg(color, Color::Yellow),
                "sub" | "sup" | "span" | "small" | "mark" => Style::default(),
                "a" => {
                    let href = body
                        .split("href=")
                        .nth(1)
                        .map(|v| {
                            let q = v.chars().next().filter(|c| *c == '"' || *c == '\'');
                            match q {
                                Some(q) => v[1..].split(q).next().unwrap_or_default(),
                                None => v.split_whitespace().next().unwrap_or_default(),
                            }
                        })
                        .unwrap_or_default()
                        .to_string();
                    if self.link.is_none() && !href.is_empty() {
                        self.link = Some((href, self.mark()));
                    }
                    fg(color, Color::Blue).add_modifier(Modifier::UNDERLINED)
                }
                _ => continue,
            };
            self.html.push((name, style));
        }
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
            Event::Html(t) | Event::InlineHtml(t) => self.html(&t),
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
                // Ordered tasks keep their sequence number; unordered tasks
                // use the checkbox in place of the bullet. The final marker
                // determines both wrapping and the hanging indent.
                let ordered = matches!(self.lists.last(), Some(Some(_)));
                match self.prefixes.last_mut() {
                    Some(p) if p.first.is_some() => {
                        let first = p.first.as_mut().unwrap();
                        if !ordered {
                            first.clear();
                        }
                        first.push(Span::styled(mark, style));
                        let width = first.iter().map(|span| span.content.width()).sum();
                        p.rest = vec![Span::raw(" ".repeat(width))];
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
                let at = self.mark();
                self.link = Some((dest_url.to_string(), at));
                self.styles
                    .push(fg(color, Color::Blue).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { dest_url, .. } => {
                self.push_text("[image: ", dim(color));
                self.image = Some(dest_url.to_string());
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
                self.finish_link();
            }
            TagEnd::Image => {
                self.styles.pop();
                if let Some(url) = self.image.take() {
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
        self.code_index += 1;
        let index = self.code_index;
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
            // Wider than the pane: a cut-down preview, with the full
            // diagram in the pager.
            if let Some(d) = mermaid::render(body, color, 10_000) {
                for row in wide_banner(index, d.kind, avail) {
                    self.emit(vec![Span::styled(row, dim(color))]);
                }
                let total = d.lines.len();
                for line in d.lines.into_iter().take(PREVIEW_ROWS) {
                    self.emit(truncate_spans(line.spans, avail));
                }
                if total > PREVIEW_ROWS {
                    self.emit(vec![Span::styled(
                        format!("  … {} more rows", total - PREVIEW_ROWS),
                        dim(color),
                    )]);
                }
                return;
            }
        }
        if lang.eq_ignore_ascii_case("mermaid") {
            // Word-wrapped to the pane, so the label is never clipped (#1742).
            let label =
                format!("╭─ mermaid [{index}] (source — diagram too wide or not drawable here)");
            let words = label.split(' ').map(str::to_string).collect::<Vec<_>>();
            for row in fit_groups(&[words], " ", avail) {
                self.emit(vec![Span::styled(row, dim(color))]);
            }
        } else {
            let label = if lang.is_empty() {
                format!("╭─ [{index}]")
            } else {
                format!("╭─ {lang} [{index}]")
            };
            self.emit(vec![Span::styled(label, dim(color))]);
        }
        let diff = ["diff", "patch"]
            .iter()
            .any(|d| lang.eq_ignore_ascii_case(d));
        if diff {
            // Coloured, with long unchanged context collapsed.
            let code_w = avail.saturating_sub(2).max(8);
            for row in super::highlight::diff_lines(body, color) {
                let mut line = vec![Span::styled("│ ", dim(color))];
                line.extend(truncate_spans(row, code_w));
                self.emit(line);
            }
            self.emit(vec![Span::styled("╰─", dim(color))]);
            return;
        }
        let mut hl = Highlighter::new(lang, color);
        let numbers = LINE_NUMBERS.with(|l| l.get());
        let num_w = body.split('\n').count().to_string().len();
        let code_w = avail
            .saturating_sub(2 + if numbers { num_w + 1 } else { 0 })
            .max(8);
        for (n, raw) in body.split('\n').enumerate() {
            let raw = raw.replace('\t', "    ");
            let spans = if let Some(h) = &mut hl {
                h.line(&raw)
            } else {
                // A language the highlighter doesn't know: plain text in
                // the default colour, not solid yellow (#877).
                vec![Span::raw(raw.clone())]
            };
            // Code never wraps (it would break indentation): long lines
            // end in `…`, and `/pager code N` shows them in full.
            let mut line = vec![Span::styled("│ ", dim(color))];
            if numbers {
                line.push(Span::styled(format!("{:>num_w$} ", n + 1), dim(color)));
            }
            line.extend(truncate_spans(spans, code_w));
            self.emit(line);
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
                // The value wraps under itself with a hanging indent, so no
                // row is wider than the pane (#874); a key too wide to leave
                // room puts the value on the rows below it.
                let avail = self.width.saturating_sub(self.prefix_width()).max(1);
                let key_w: usize = line.iter().map(|s| s.content.width()).sum();
                let (first, hang) = if avail >= key_w + 8 {
                    (line, key_w)
                } else {
                    self.emit(truncate_spans(line, avail));
                    (vec![Span::raw("    ")], 4.min(avail.saturating_sub(1)))
                };
                let mut rows = wrap(cell.clone(), avail - hang).into_iter();
                let mut head = first;
                head.extend(rows.next().unwrap_or_default());
                self.emit(head);
                for r in rows {
                    let mut l = vec![Span::raw(" ".repeat(hang))];
                    l.extend(r);
                    self.emit(l);
                }
            }
        }
    }
}

/// Rows of a too-wide diagram shown in the chat before `/pager diagram`.
const PREVIEW_ROWS: usize = 12;

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
        /// A place to break with no space: between clusters of a script
        /// written without spaces (Thai, CJK) (#786).
        Soft,
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
        for g in span.content.graphemes(true) {
            let ch = g.chars().next().unwrap_or(' ');
            if g == "\n" || g == "\r\n" {
                flush_word(&mut word, &mut pieces);
                pieces.push(Piece::Break);
            } else if ch.is_whitespace() {
                flush_word(&mut word, &mut pieces);
                if !matches!(pieces.last(), Some(Piece::Space(_))) {
                    pieces.push(Piece::Space(style));
                }
            } else if crate::text::unspaced(ch) {
                // Each cluster is its own word, with break chances around it.
                flush_word(&mut word, &mut pieces);
                if matches!(pieces.last(), Some(Piece::Word(..))) {
                    pieces.push(Piece::Soft);
                }
                word.push_str(g);
                flush_word(&mut word, &mut pieces);
                pieces.push(Piece::Soft);
            } else {
                word.push_str(g);
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
            Piece::Soft => {}
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
        // By cluster, so a mark never starts a row and an emoji sequence
        // is never cut (#786).
        for g in span.content.graphemes(true) {
            let cw = g.width();
            if w + cw > avail && w > 0 {
                if !buf.is_empty() {
                    rows.last_mut()
                        .unwrap()
                        .push(Span::styled(std::mem::take(&mut buf), span.style));
                }
                rows.push(Vec::new());
                w = 0;
            }
            buf.push_str(g);
            w += cw;
        }
        if !buf.is_empty() {
            rows.last_mut().unwrap().push(Span::styled(buf, span.style));
        }
    }
    rows
}

/// The hint above a too-wide diagram's preview, in rows of at most `avail`
/// columns (#1742). Its phrases stay whole where a row holds them, so
/// `/pager diagram N` and `/open diagram N` are never cut; a pane narrower
/// than a command drops the prose and keeps the label and both commands,
/// broken at their spaces first. No room at all shows no hint.
fn wide_banner(index: usize, kind: &str, avail: usize) -> Vec<String> {
    let words = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
    let label = words(&format!("◇ mermaid [{index}]"));
    let pager = format!("/pager diagram {index}");
    let open = format!("/open diagram {index}");
    if avail < pager.width().max(open.width()) {
        return fit_groups(&[label, words(&pager), words(&open)], " ", avail);
    }
    let mut hint = words("wider than the chat:");
    hint.push(pager);
    let kind: Vec<String> = words(kind).into_iter().filter(|w| !w.is_empty()).collect();
    fit_groups(&[label, kind, hint, vec![open]], " · ", avail)
}

/// Pack groups of words into non-empty rows of at most `avail` columns:
/// groups joined by `sep` and words by a space. A group moves to a new
/// row whole when that row holds it; one wider than a row starts a row
/// and wraps word by word, and a word wider than a row is split between
/// grapheme clusters. A cluster wider than a row shows as `…`; with no
/// room at all (`avail == 0`) there are no rows.
fn fit_groups(groups: &[Vec<String>], sep: &str, avail: usize) -> Vec<String> {
    let mut rows = Vec::new();
    if avail == 0 {
        return rows;
    }
    let mut row = String::new();
    for group in groups.iter().filter(|g| !g.is_empty()) {
        let whole = group.join(" ");
        let gap = if row.is_empty() { 0 } else { sep.width() };
        if row.width() + gap + whole.width() <= avail {
            if gap > 0 {
                row.push_str(sep);
            }
            row.push_str(&whole);
            continue;
        }
        if whole.width() <= avail {
            rows.push(std::mem::replace(&mut row, whole));
            continue;
        }
        // Too wide for any row: it starts its own, wrapped by word.
        if !row.is_empty() {
            rows.push(std::mem::take(&mut row));
        }
        for word in group {
            let gap = usize::from(!row.is_empty());
            if row.width() + gap + word.width() <= avail {
                if gap > 0 {
                    row.push(' ');
                }
                row.push_str(word);
                continue;
            }
            if !row.is_empty() {
                rows.push(std::mem::take(&mut row));
            }
            for g in word.graphemes(true) {
                let g = if g.width() > avail { "…" } else { g };
                if !row.is_empty() && row.width() + g.width() > avail {
                    rows.push(std::mem::take(&mut row));
                }
                row.push_str(g);
            }
        }
    }
    if !row.is_empty() {
        rows.push(row);
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
        for g in span.content.graphemes(true) {
            let cw = g.width();
            if w + cw + 1 > max {
                buf.push('…');
                out.push(Span::styled(buf, span.style));
                return out;
            }
            buf.push_str(g);
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
    fn ordered_tasks_keep_numbers_check_state_and_final_hanging_indent() {
        for start in [1, 10, 100] {
            let source = format!("{start}. [ ] Task _italic_ 日本語 with enough text to wrap\n{}. [x] Finished `code` with more wrapped content", start + 1);
            for color in [false, true] {
                let out = render(&source, color, "", 24);
                let text = out
                    .iter()
                    .map(|line| {
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>();
                let first = format!("{start}. ☐ ");
                let next = format!("{}. ☑ ", start + 1);
                let split = text
                    .iter()
                    .position(|line| line.starts_with(&next))
                    .unwrap();
                for (rows, marker, expected) in [
                    (
                        &text[..split],
                        first.as_str(),
                        "Taskitalic日本語withenoughtexttowrap",
                    ),
                    (
                        &text[split..],
                        next.as_str(),
                        "Finishedcodewithmorewrappedcontent",
                    ),
                ] {
                    assert!(rows[0].starts_with(marker), "{text:?}");
                    let indent = " ".repeat(marker.width());
                    let content = rows
                        .iter()
                        .enumerate()
                        .filter(|(_, line)| !line.is_empty())
                        .map(|(index, line)| {
                            if index == 0 {
                                line.strip_prefix(marker).unwrap()
                            } else {
                                line.strip_prefix(&indent).unwrap()
                            }
                        })
                        .collect::<String>();
                    assert_eq!(content.split_whitespace().collect::<String>(), expected);
                }
                assert!(text.iter().all(|line| line.width() <= 24), "{text:?}");
                assert!(out
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.content == "italic"
                        && span.style.add_modifier.contains(Modifier::ITALIC)));
            }
        }
    }

    #[test]
    fn task_prefixes_align_inside_quotes_and_nested_lists() {
        for (source, first, rest) in [
            (
                "> 10. [x] Long task with Unicode 日本語 and wrapped content",
                "  │ 10. ☑ ",
                "  │       ",
            ),
            (
                "- [ ] Long task with Unicode 日本語 and wrapped content",
                "  ☐ ",
                "    ",
            ),
            (
                "- parent\n\n  10. [ ] Long task with Unicode 日本語 and wrapped content",
                "    10. ☐ ",
                "          ",
            ),
        ] {
            let out = lines(source, 24);
            let start = out
                .iter()
                .position(|line| line.starts_with(first))
                .unwrap_or_else(|| panic!("missing {first:?}: {out:?}"));
            let rows = out[start..]
                .iter()
                .filter(|line| !line.trim().is_empty() && line.trim() != "│")
                .collect::<Vec<_>>();
            assert!(rows.len() > 1, "{out:?}");
            for row in &rows[1..] {
                assert!(row.starts_with(rest), "{out:?}");
            }
            assert!(out.iter().all(|line| line.width() <= 24), "{out:?}");
            let content = rows
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    line.strip_prefix(if index == 0 { first } else { rest })
                        .unwrap()
                })
                .collect::<String>();
            assert_eq!(
                content.split_whitespace().collect::<String>(),
                "LongtaskwithUnicode日本語andwrappedcontent"
            );
        }
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
        // A long value in records mode wraps under its key (#874).
        let long = "| a | b | c | d | e |\n|---|---|---|---|---|\n| 1 | a very long cell value that keeps going and going | 3 | 4 | 5 |";
        let out = lines(long, 20);
        assert!(out.iter().all(|l| l.width() <= 20), "{out:#?}");
        let text = out.join(" ");
        for word in ["very", "long", "cell", "value", "keeps", "going"] {
            assert!(text.contains(word), "{word} lost: {out:#?}");
        }
        let b = out
            .iter()
            .position(|l| l.trim_start().starts_with("b: "))
            .unwrap();
        assert!(out[b + 1].starts_with("     "), "hanging indent: {out:#?}");
    }

    /// Thai and Chinese wrap within the width, never starting a row with a
    /// mark or cutting an emoji sequence (#786).
    #[test]
    fn unspaced_scripts_wrap_between_clusters() {
        let marks = |c: char| matches!(c as u32, 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E);
        let out = lines("สวัสดีครับวันนี้อากาศดีมากเลยนะครับ", 20);
        assert!(out.len() > 1, "{out:?}");
        for row in &out {
            assert!(row.width() <= 20, "{row}");
            let first = row.trim_start().chars().next().unwrap();
            assert!(!marks(first), "row starts with a mark: {row}");
        }
        let out = lines("我喜欢用中文写很长的句子来测试换行", 20);
        assert!(
            out.len() > 1 && out.iter().all(|r| r.width() <= 20),
            "{out:?}"
        );
        assert_eq!(
            out.join("").replace(' ', ""),
            "我喜欢用中文写很长的句子来测试换行"
        );
        let family = "👨\u{200d}👩\u{200d}👧\u{200d}👦";
        let rows = hard_wrap(vec![Span::raw(family.repeat(3))], 3);
        for r in &rows {
            let t: String = r.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(t, family, "{rows:?}");
        }
        let cut = truncate_spans(vec![Span::raw(format!("{family}{family}x"))], 4);
        let t: String = cut.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(t, format!("{family}…"));
    }

    /// Unknown languages are plain text; `Diff` is a diff whatever its
    /// case (#877).
    #[test]
    fn unknown_languages_are_plain_and_diff_ignores_case() {
        let out = render("```brainfuck\n+[-]\n```", true, "", 40);
        let code = out
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == "+[-]")
            .unwrap();
        assert_eq!(code.style.fg, None, "default colour, not yellow");
        let out = render("```Diff\n+added\n```", true, "", 40);
        let added = out
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content == "+added")
            .unwrap();
        assert_eq!(added.style.fg, Some(Color::Green));
    }

    /// An image inside a link keeps both addresses, and inline HTML shows
    /// its text, not its tags (#875).
    #[test]
    fn image_links_and_inline_html() {
        let out = lines(
            "[![alt](https://i.example/x.png)](https://dest.example/)",
            80,
        )
        .join("\n");
        assert!(out.contains("https://i.example/x.png"), "{out}");
        assert!(out.contains("(https://dest.example/)"), "{out}");
        let out = lines(
            "Press <kbd>Esc</kbd>, H<sub>2</sub>O, a<sup>2</sup> <!-- hidden --> <foo>x</foo>",
            80,
        )
        .join("\n");
        assert_eq!(out.trim(), "Press Esc, H2O, a2 x");
        let out = lines(
            "see <a href=\"https://x.dev\">the docs</a> and <b>bold</b>",
            80,
        )
        .join("\n");
        assert_eq!(out.trim(), "see the docs (https://x.dev) and bold");
        let styled = render("<b>bold</b>", false, "", 40);
        let span = styled[0]
            .spans
            .iter()
            .find(|s| s.content == "bold")
            .unwrap();
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
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
    fn code_blocks_are_numbered_and_can_show_line_numbers() {
        let md = "```\na\n```\n\ntext\n\n```py\nx = 1\ny = 2\n```";
        let out = lines(md, 40);
        assert!(out.contains(&"  ╭─ [1]".to_string()), "{out:#?}");
        assert!(
            out.contains(&"  ╭─ py [2]".to_string()),
            "matches /copy code 2"
        );
        set_line_numbers(true);
        let out = lines(md, 40);
        set_line_numbers(false);
        assert!(
            out.iter().any(|l| l.trim_end() == "  │ 2 y = 2"),
            "{out:#?}"
        );
    }

    #[test]
    fn long_code_lines_are_cut_not_wrapped() {
        let md = format!("```\n    indented {}\nshort\n```", "x".repeat(80));
        let out = lines(&md, 30);
        let body: Vec<&String> = out.iter().filter(|l| l.contains('│')).collect();
        assert_eq!(body.len(), 2, "one row per source line: {out:#?}");
        assert!(body[0].ends_with('…') && body[0].contains("│     indented"));
        assert!(out.iter().all(|l| l.chars().count() <= 30), "{out:#?}");
    }

    #[test]
    fn code_blocks_highlight_and_mermaid_draws() {
        let out = render("```rust\nfn main() {}\n```", true, "", 60);
        assert_eq!(out[0].spans[0].content, "╭─ rust [1]");
        let kw = out[1].spans.iter().find(|s| s.content == "fn").unwrap();
        assert_eq!(kw.style.fg, Some(Color::Magenta));
        let out = lines("```mermaid\ngraph LR\nA --> B\n```", 60);
        assert_eq!(out[0], "  ◇ mermaid · flowchart");
        assert!(out.iter().any(|l| l.contains("│ A ├───▶│ B │")), "{out:?}");
        let out = lines("```mermaid\ngantt\ntitle x\n```", 60);
        assert!(out[0].contains("mermaid [1] (source"), "{out:?}");
        assert!(out.iter().any(|l| l.contains("gantt")));
    }

    /// A too-wide flowchart: one label of about `22 * n` columns (#1742).
    fn wide_mermaid(n: usize) -> String {
        format!(
            "graph TD\nA[{}] --> B[done]",
            "a very long step name ".repeat(n).trim_end()
        )
    }

    /// `md` rendered with the `lines` indent, checked against `width`.
    fn fitted(md: &str, width: usize) -> Vec<String> {
        let out = lines(md, width);
        for l in &out {
            assert!(
                l.width() <= width,
                "{} > {width}: {l:?}\n{out:#?}",
                l.width()
            );
        }
        out
    }

    /// Words of the rendered rows, without quote bars, bullets and `·`.
    fn words_of(rows: &[String]) -> Vec<String> {
        rows.iter()
            .flat_map(|l| l.split_whitespace())
            .filter(|w| !["·", "│", "•", "◦", "▪"].contains(w))
            .map(str::to_string)
            .collect()
    }

    fn banner_words(index: usize) -> Vec<String> {
        format!("◇ mermaid [{index}] flowchart wider than the chat: /pager diagram {index} /open diagram {index}")
            .split(' ')
            .map(str::to_string)
            .collect()
    }

    /// Both commands whole, each on one row.
    fn assert_commands(rows: &[String], index: usize) {
        for cmd in [
            format!("/pager diagram {index}"),
            format!("/open diagram {index}"),
        ] {
            assert!(rows.iter().any(|l| l.contains(&cmd)), "{cmd}: {rows:#?}");
        }
    }

    #[test]
    fn wide_mermaid_banner_wraps_at_40_and_80() {
        for (width, ns) in [(40, 2..=6), (80, 4..=6)] {
            for n in ns {
                let src = wide_mermaid(n);
                let out = fitted(&format!("```mermaid\n{src}\n```"), width);
                // The preview rows are the existing cut, ellipsis and all.
                let d = mermaid::render(&src, false, 10_000).unwrap();
                let preview: Vec<String> = d
                    .lines
                    .into_iter()
                    .take(PREVIEW_ROWS)
                    .map(|l| {
                        let cut = truncate_spans(l.spans, width - 2);
                        format!(
                            "  {}",
                            cut.iter().map(|s| s.content.as_ref()).collect::<String>()
                        )
                    })
                    .collect();
                assert!(out.ends_with(&preview), "{out:#?}\n{preview:#?}");
                assert!(preview.iter().any(|l| l.ends_with('…')), "{preview:#?}");
                let banner = &out[..out.len() - preview.len()];
                assert_eq!(words_of(banner), banner_words(1), "{banner:#?}");
                assert_commands(banner, 1);
                if width == 80 {
                    assert_eq!(
                        banner,
                        [
                            "  ◇ mermaid [1] · flowchart · wider than the chat: /pager diagram 1",
                            "  /open diagram 1",
                        ],
                        "n = {n}"
                    );
                } else {
                    assert_eq!(
                        banner,
                        [
                            "  ◇ mermaid [1] · flowchart",
                            "  wider than the chat: /pager diagram 1",
                            "  /open diagram 1",
                        ],
                        "n = {n}"
                    );
                }
            }
        }
    }

    #[test]
    fn wide_mermaid_banner_keeps_its_index_under_nested_prefixes() {
        // Eleven blocks first, so the diagram is [12].
        let before = "```\nx\n```\n\n".repeat(11);
        let src = wide_mermaid(4);
        let fence = |bar: &str| {
            format!("```mermaid\n{src}\n```")
                .lines()
                .map(|l| format!("{bar}{l}\n"))
                .collect::<String>()
        };
        let nested = [
            format!("{before}- intro\n\n{}", fence("  > ")),
            format!("{before}> > intro\n> >\n{}", fence("> > ")),
            format!("{before}> - intro\n>\n{}", fence(">   ")),
        ];
        for md in &nested {
            for width in [40, 52, 80] {
                let out = fitted(md, width);
                let at = out
                    .iter()
                    .position(|l| l.contains("◇ mermaid [12]"))
                    .unwrap();
                let rest = &out[at..];
                let expect = banner_words(12);
                assert_eq!(words_of(rest)[..expect.len()], expect[..], "{out:#?}");
                assert_commands(rest, 12);
                assert!(
                    rest.iter().any(|l| l.ends_with('…')),
                    "preview cut: {out:#?}"
                );
                assert!(rest.iter().all(|l| l.starts_with("  ")), "{out:#?}");
            }
        }
    }

    #[test]
    fn wide_mermaid_banner_has_a_compact_form_for_tiny_panes() {
        for index in [1, 12, 100] {
            let pager = format!("/pager diagram {index}");
            let open = format!("/open diagram {index}");
            let full = banner_words(index);
            let compact: Vec<String> = format!("◇ mermaid [{index}] {pager} {open}")
                .split(' ')
                .map(str::to_string)
                .collect();
            assert!(
                wide_banner(index, "flowchart", 0).is_empty(),
                "no room: no rows"
            );
            for avail in 1..=120 {
                let rows = wide_banner(index, "flowchart", avail);
                assert!(
                    rows.iter().all(|r| r.width() <= avail),
                    "{avail}: {rows:#?}"
                );
                assert!(rows.iter().all(|r| !r.is_empty()), "{avail}: {rows:#?}");
                let squashed: String = rows.concat().split([' ', '·']).collect();
                if avail >= pager.width() {
                    assert_eq!(words_of(&rows), full, "{avail}: {rows:#?}");
                    assert_commands(&rows, index);
                } else {
                    let want: String = compact.concat();
                    assert_eq!(squashed, want, "{avail}: {rows:#?}");
                    if avail >= "mermaid".len() {
                        assert_eq!(words_of(&rows), compact, "words kept: {rows:#?}");
                    }
                }
            }
        }
        let quoted = |depth: usize| {
            format!("```mermaid\n{}\n```", wide_mermaid(2))
                .lines()
                .map(|l| format!("{}{l}\n", "> ".repeat(depth)))
                .collect::<String>()
        };
        // Prefixes as wide as the pane (the renderer's minimum of 16
        // columns) leave no room: no panic, no loop, and no hint rows.
        for width in [0, 1, 16] {
            let out = render(&quoted(8), false, "", width);
            let text: Vec<String> = out
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert!(!text.iter().any(|l| l.contains("mermaid")), "{text:#?}");
        }
        // Room for exactly the commands (16 columns): both whole.
        let out = fitted(&quoted(6), 30);
        assert_commands(&out, 1);
        // Room for less (10 columns): the compact hint, within the pane.
        let out = fitted(&quoted(4), 20);
        let squashed: String = out.concat().split([' ', '│']).collect();
        assert!(
            squashed.contains("◇mermaid[1]/pagerdiagram1/opendiagram1"),
            "{out:#?}"
        );
    }

    #[test]
    fn fit_groups_bounds_wide_and_joined_clusters() {
        let family = "👩\u{200d}👩\u{200d}👧";
        let words = ["長", "e\u{301}x", family, "長長", "ab"];
        let groups = [
            words.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
            vec!["e\u{301}".to_string()],
        ];
        // Clusters the rows may hold: the input's, the joins and `…`.
        let ok = ["長", "e\u{301}", "x", family, "a", "b", " ", "·", "…"];
        for avail in 0..=6 {
            let rows = fit_groups(&groups, " · ", avail);
            assert_eq!(rows.is_empty(), avail == 0, "{avail}: {rows:?}");
            for r in &rows {
                assert!(!r.is_empty() && r.width() <= avail, "{avail}: {rows:?}");
                assert!(
                    r.graphemes(true).all(|g| ok.contains(&g)),
                    "{avail}: split cluster in {r:?}"
                );
            }
            if avail == 1 {
                assert!(rows.iter().all(|r| !r.contains('長')), "{rows:?}");
                assert!(rows.iter().any(|r| r == "…"), "{rows:?}");
            }
            if avail >= 2 {
                assert!(rows.iter().any(|r| r.contains('長')), "{rows:?}");
            }
            if avail >= 1 {
                assert!(rows.iter().any(|r| r.contains("e\u{301}")), "{rows:?}");
            }
        }
    }

    #[test]
    fn wide_mermaid_banner_fits_beside_unicode_prose_and_labels() {
        let family = "👩\u{200d}👩\u{200d}👧";
        let label = format!("長いステップ名 {family} ขั้นตอนที่ยาวมาก ").repeat(4);
        let src = format!("graph TD\nA[{}] --> B[完了]", label.trim_end());
        let md = format!(
            "> ข้อความภาษาไทย 中文说明 {family} prose\n>\n{}",
            format!("```mermaid\n{src}\n```")
                .lines()
                .map(|l| format!("> {l}\n"))
                .collect::<String>()
        );
        for width in [40, 80] {
            let out = fitted(&md, width);
            let at = out
                .iter()
                .position(|l| l.contains("◇ mermaid [1]"))
                .unwrap();
            let expect = banner_words(1);
            assert_eq!(words_of(&out[at..])[..expect.len()], expect[..], "{out:#?}");
            assert_commands(&out[at..], 1);
            for l in &out {
                assert!(
                    !l.replace(family, "").contains('\u{200d}'),
                    "cluster cut: {l:?}"
                );
            }
        }
    }

    #[test]
    fn mermaid_source_label_wraps_to_the_pane() {
        let md = "```mermaid\ngantt\ntitle x\n```";
        let label = "╭─ mermaid [1] (source — diagram too wide or not drawable here)";
        assert_eq!(fitted(md, 80)[0], format!("  {label}"), "fits: unchanged");
        let words: Vec<String> = label.split(' ').map(str::to_string).collect();
        for (md, width) in [
            (md.to_string(), 40),
            (md.to_string(), 24),
            (format!("- item\n\n  > {}", md.replace('\n', "\n  > ")), 40),
            (format!("> > {}", md.replace('\n', "\n> > ")), 40),
        ] {
            let out = fitted(&md, width);
            let at = out
                .iter()
                .position(|l| l.contains("╭─ mermaid [1]"))
                .unwrap();
            let rows: Vec<String> = out[at..]
                .iter()
                .take_while(|l| !l.contains("gantt"))
                .cloned()
                .collect();
            assert!(rows.len() > 1, "{out:#?}");
            assert_eq!(words_of(&rows), words, "{out:#?}");
            assert!(out.iter().any(|l| l.contains("│ gantt")), "{out:#?}");
        }
        for avail in 0..=70 {
            let rows = fit_groups(&[words.clone()], " ", avail);
            assert_eq!(rows.is_empty(), avail == 0, "{rows:?}");
            assert!(
                rows.iter().all(|r| !r.is_empty() && r.width() <= avail),
                "{avail}: {rows:?}"
            );
        }
    }
}
