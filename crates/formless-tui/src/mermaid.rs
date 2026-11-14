//! Mermaid diagrams drawn as terminal art.
//!
//! * `flowchart` / `graph` and `stateDiagram` → layered boxes and arrows
//!   (cycle breaking, longest-path layers, dummy nodes for long edges,
//!   barycentre ordering), top-down or left-right.
//! * `sequenceDiagram` → participants, lifelines, messages, notes, blocks.
//! * `pie` → labelled horizontal bars.
//!
//! Anything else, or a diagram wider than the pane, returns `None` and the
//! caller shows the source instead. Parsing is lenient: unknown statements
//! are skipped rather than failing the whole diagram.

use std::collections::HashMap;

use ratatui::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{bold_fg, fg};

pub(crate) struct Diagram {
    pub kind: &'static str,
    pub lines: Vec<Line<'static>>,
}

pub(crate) fn render(src: &str, color: bool, width: usize) -> Option<Diagram> {
    let frontmatter = split_frontmatter(src)?;
    let src = frontmatter.body;
    let header = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    let word = header.split_whitespace().next().unwrap_or("");
    let (kind, mut lines) = match word {
        "graph" | "flowchart" => (
            "flowchart",
            graph_lines(parse_flowchart(src)?, color, width)?,
        ),
        "stateDiagram" | "stateDiagram-v2" => {
            ("state", graph_lines(parse_state(src)?, color, width)?)
        }
        "sequenceDiagram" => ("sequence", sequence(src, color, width)?),
        "pie" => ("pie", pie(src, color, width)?),
        _ => return None,
    };
    let title = frontmatter.title.or_else(|| inline_title(src, kind));
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        let mut heading: Vec<_> = title
            .lines()
            .map(|line| Line::styled(heading_text(line), bold_fg(color, Color::Cyan)))
            .collect();
        if heading.iter().any(|line| line.width() > width) {
            return None;
        }
        heading.append(&mut lines);
        lines = heading;
    }
    Some(Diagram { kind, lines })
}

/// Quoted YAML can decode escaped controls; keep those visible in headings.
fn heading_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    out
}

/// Frontmatter is metadata, never diagram statements. An unclosed block
/// falls back to source instead of guessing where the diagram begins.
struct Frontmatter<'a> {
    body: &'a str,
    title: Option<String>,
}

fn split_frontmatter(src: &str) -> Option<Frontmatter<'_>> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut offset = 0;
    let mut start = None;
    for line in src.split_inclusive('\n') {
        let before = offset;
        offset += line.len();
        let trimmed = line.trim();
        if let Some((start, opening_indent)) = start {
            let indent = line.len() - line.trim_start().len();
            if trimmed == "---" && indent <= opening_indent {
                return Some(Frontmatter {
                    body: &src[offset..],
                    title: frontmatter_title(&src[start..before]),
                });
            }
        } else if !trimmed.is_empty() && !trimmed.starts_with("%%") {
            if trimmed != "---" {
                return Some(Frontmatter {
                    body: src,
                    title: None,
                });
            }
            start = Some((offset, line.len() - line.trim_start().len()));
        }
    }
    if start.is_some() {
        None
    } else {
        Some(Frontmatter {
            body: src,
            title: None,
        })
    }
}

/// Only the root title is displayed; nested config/style keys are ignored.
/// Scalar and literal/folded block titles cover the terminal title subset;
/// this never evaluates YAML tags, aliases or Mermaid configuration.
fn frontmatter_title(metadata: &str) -> Option<String> {
    let lines: Vec<_> = metadata.lines().collect();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let root = lines
        .iter()
        .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
        .map(|line| indent(line))
        .min()?;
    for (i, line) in lines.iter().enumerate() {
        if indent(line) != root {
            continue;
        }
        let Some(value) = line.trim().strip_prefix("title:").map(str::trim) else {
            continue;
        };
        if matches!(
            value.split_whitespace().next(),
            Some("|" | "|-" | "|+" | ">" | ">-" | ">+")
        ) {
            let block: Vec<_> = lines[i + 1..]
                .iter()
                .take_while(|line| line.trim().is_empty() || indent(line) > root)
                .copied()
                .collect();
            let base = block
                .iter()
                .filter(|line| !line.trim().is_empty())
                .map(|line| indent(line))
                .min()?;
            let text = block
                .iter()
                .map(|line| line.get(base..).unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n");
            return Some(
                if value.starts_with('>') {
                    text.split("\n\n")
                        .map(|paragraph| paragraph.lines().collect::<Vec<_>>().join(" "))
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    text
                }
                .trim_end()
                .to_owned(),
            );
        }
        if value.starts_with('"') {
            let mut parser = serde_json::Deserializer::from_str(value).into_iter::<String>();
            let title = parser.next()?.ok()?;
            let rest = value[parser.byte_offset()..].trim();
            return (rest.is_empty() || rest.starts_with('#')).then_some(title);
        }
        if let Some(value) = value.strip_prefix('\'') {
            let mut title = String::new();
            let mut chars = value.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                        title.push(c);
                    } else {
                        let rest: String = chars.collect();
                        return (rest.trim().is_empty() || rest.trim_start().starts_with('#'))
                            .then_some(title);
                    }
                } else {
                    title.push(c);
                }
            }
            return None;
        }
        if value.starts_with(['[', '{', '&', '*', '!', '|', '>']) {
            return None;
        }
        let comment = value
            .char_indices()
            .find(|(i, c)| *c == '#' && (*i == 0 || value[..*i].ends_with(char::is_whitespace)))
            .map_or(value.len(), |(i, _)| i);
        return Some(value[..comment].trim_end().to_owned());
    }
    None
}

fn inline_title(src: &str, kind: &str) -> Option<String> {
    if !matches!(kind, "pie" | "sequence") {
        return None;
    }
    let mut title = if kind == "pie" {
        src.lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("%%"))?
            .split_once("title")
            .map(|(_, t)| t.trim().to_owned())
    } else {
        None
    };
    for stmt in statements(src) {
        let (head, rest) = stmt.split_once(char::is_whitespace).unwrap_or((stmt, ""));
        if head == "title" {
            title = Some(rest.trim().to_owned());
        }
    }
    title
}

// ---------------------------------------------------------------- canvas

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dash {
    Solid,
    Dotted,
    Thick,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ink {
    Border,
    Text,
    Edge,
    Label,
    Title,
}

#[derive(Clone, Copy)]
enum Cell {
    Empty,
    Lines(u8, Dash),
    Glyph(char, Ink),
    /// Right half of a double-width glyph.
    Cont,
}

#[derive(Default)]
struct Canvas {
    rows: Vec<Vec<Cell>>,
}

impl Canvas {
    fn cell(&mut self, x: usize, y: usize) -> &mut Cell {
        if self.rows.len() <= y {
            self.rows.resize_with(y + 1, Vec::new);
        }
        let row = &mut self.rows[y];
        if row.len() <= x {
            row.resize(x + 1, Cell::Empty);
        }
        &mut row[x]
    }

    fn bits(&mut self, x: usize, y: usize, bits: u8, dash: Dash) {
        let c = self.cell(x, y);
        *c = match *c {
            // A cell shared by a solid and a styled edge stays solid.
            Cell::Lines(b, d) => Cell::Lines(b | bits, if d == dash { d } else { Dash::Solid }),
            Cell::Empty => Cell::Lines(bits, dash),
            other => other,
        };
    }

    /// Axis-aligned segment; shared endpoints join into corners/junctions.
    fn seg(&mut self, (x0, y0): (usize, usize), (x1, y1): (usize, usize), dash: Dash) {
        if y0 == y1 {
            let (a, b) = (x0.min(x1), x0.max(x1));
            for x in a..=b {
                let mut bits = 0;
                if x > a {
                    bits |= LEFT;
                }
                if x < b {
                    bits |= RIGHT;
                }
                if bits != 0 {
                    self.bits(x, y0, bits, dash);
                }
            }
        } else if x0 == x1 {
            let (a, b) = (y0.min(y1), y0.max(y1));
            for y in a..=b {
                let mut bits = 0;
                if y > a {
                    bits |= UP;
                }
                if y < b {
                    bits |= DOWN;
                }
                self.bits(x0, y, bits, dash);
            }
        }
    }

    fn glyph(&mut self, x: usize, y: usize, ch: char, ink: Ink) {
        *self.cell(x, y) = Cell::Glyph(ch, ink);
    }

    /// Text left-to-right from `x`; returns the column after it.
    fn text(&mut self, mut x: usize, y: usize, s: &str, ink: Ink) -> usize {
        for ch in s.chars() {
            let w = ch.width().unwrap_or(0);
            if w == 0 {
                continue;
            }
            self.glyph(x, y, ch, ink);
            if w == 2 {
                *self.cell(x + 1, y) = Cell::Cont;
            }
            x += w;
        }
        x
    }

    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, style: BoxStyle) {
        let [tl, tr, bl, br, hz, vt] = style.chars();
        for i in 1..w - 1 {
            self.glyph(x + i, y, hz, Ink::Border);
            self.glyph(x + i, y + h - 1, hz, Ink::Border);
        }
        for j in 1..h - 1 {
            self.glyph(x, y + j, vt, Ink::Border);
            self.glyph(x + w - 1, y + j, vt, Ink::Border);
            for i in 1..w - 1 {
                self.glyph(x + i, y + j, ' ', Ink::Text);
            }
        }
        self.glyph(x, y, tl, Ink::Border);
        self.glyph(x + w - 1, y, tr, Ink::Border);
        self.glyph(x, y + h - 1, bl, Ink::Border);
        self.glyph(x + w - 1, y + h - 1, br, Ink::Border);
    }

    fn width(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }

    fn into_lines(self, color: bool) -> Vec<Line<'static>> {
        let style = |ink: Ink| match ink {
            Ink::Border => fg(color, Color::Cyan),
            Ink::Text => Style::default(),
            Ink::Edge => fg(color, Color::Gray),
            Ink::Label => fg(color, Color::Yellow),
            Ink::Title => bold_fg(color, Color::Cyan),
        };
        self.rows
            .into_iter()
            .map(|row| {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut cur = String::new();
                let mut cur_ink = Ink::Text;
                for cell in row {
                    let (ch, ink) = match cell {
                        Cell::Empty => (' ', Ink::Text),
                        Cell::Cont => continue,
                        Cell::Glyph(c, ink) => (c, ink),
                        Cell::Lines(bits, dash) => (line_glyph(bits, dash), Ink::Edge),
                    };
                    // Spaces join any run so colour changes stay minimal.
                    if ink != cur_ink && ch != ' ' {
                        if !cur.is_empty() {
                            spans.push(Span::styled(std::mem::take(&mut cur), style(cur_ink)));
                        }
                        cur_ink = ink;
                    }
                    cur.push(ch);
                }
                let trimmed = cur.trim_end();
                if !trimmed.is_empty() {
                    spans.push(Span::styled(trimmed.to_string(), style(cur_ink)));
                }
                Line::from(spans)
            })
            .collect()
    }
}

fn line_glyph(bits: u8, dash: Dash) -> char {
    let vertical = bits & (LEFT | RIGHT) == 0;
    let horizontal = bits & (UP | DOWN) == 0;
    match (vertical, horizontal, dash) {
        (true, _, Dash::Dotted) => return '┆',
        (true, _, Dash::Thick) => return '┃',
        (_, true, Dash::Dotted) => return '┄',
        (_, true, Dash::Thick) => return '━',
        _ => {}
    }
    match bits {
        b if b & (LEFT | RIGHT) == 0 => '│',
        b if b & (UP | DOWN) == 0 => '─',
        b if b == DOWN | RIGHT => '╭',
        b if b == DOWN | LEFT => '╮',
        b if b == UP | RIGHT => '╰',
        b if b == UP | LEFT => '╯',
        b if b == UP | DOWN | RIGHT => '├',
        b if b == UP | DOWN | LEFT => '┤',
        b if b == DOWN | LEFT | RIGHT => '┬',
        b if b == UP | LEFT | RIGHT => '┴',
        _ => '┼',
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BoxStyle {
    Square,
    Round,
    Double,
}

impl BoxStyle {
    fn chars(self) -> [char; 6] {
        match self {
            BoxStyle::Square => ['┌', '┐', '└', '┘', '─', '│'],
            BoxStyle::Round => ['╭', '╮', '╰', '╯', '─', '│'],
            BoxStyle::Double => ['╔', '╗', '╚', '╝', '═', '║'],
        }
    }
}

// ----------------------------------------------------------------- graphs

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    Box(BoxStyle),
    /// A single glyph (state diagram start/end).
    Dot(char),
}

#[derive(Debug)]
struct Node {
    label: Vec<String>,
    shape: Shape,
}

#[derive(Debug)]
struct Edge {
    from: usize,
    to: usize,
    label: String,
    dash: Dash,
    arrow: bool,
}

#[derive(Default, Debug)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    left_right: bool,
    index: HashMap<String, usize>,
    /// Something this renderer can't draw faithfully (a two-way edge, a
    /// cross or circle tip): show the source instead of a wrong picture
    /// (#879).
    unsupported: bool,
}

impl Graph {
    fn node(&mut self, id: &str, label: Option<&str>, shape: Option<Shape>) -> usize {
        let i = match self.index.get(id) {
            Some(&i) => i,
            None => {
                self.nodes.push(Node {
                    label: vec![id.to_string()],
                    shape: Shape::Box(BoxStyle::Square),
                });
                self.index.insert(id.to_string(), self.nodes.len() - 1);
                self.nodes.len() - 1
            }
        };
        if let Some(label) = label {
            self.nodes[i].label = split_label(label);
        }
        if let Some(shape) = shape {
            self.nodes[i].shape = shape;
        }
        i
    }
}

fn split_label(s: &str) -> Vec<String> {
    let s = s.trim().trim_matches('"').trim_matches('`');
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        let lower = rest.to_ascii_lowercase();
        let hit = ["<br/>", "<br />", "<br>", "\\n"]
            .iter()
            .filter_map(|p| lower.find(p).map(|i| (i, p.len())))
            .min();
        match hit {
            Some((i, n)) => {
                out.push(rest[..i].trim().to_string());
                rest = &rest[i + n..];
            }
            None => {
                out.push(rest.trim().to_string());
                break;
            }
        }
    }
    out
}

fn direction_is_lr(word: Option<&str>) -> bool {
    matches!(word, Some("LR" | "RL"))
}

/// Statements of a diagram body: header removed, `;` split, comments gone.
fn statements(src: &str) -> impl Iterator<Item = &str> {
    split_frontmatter(src)
        .map_or("", |frontmatter| frontmatter.body)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("%%"))
        .skip(1)
        .flat_map(split_statements)
        .map(str::trim)
        .filter(|l| !l.is_empty())
}

/// A line split at its `;` separators, but not inside a quoted label, a
/// node shape's brackets, an `|edge label|`, or the text after a `:`
/// (a message or label runs to the end of the line) (#878).
fn split_statements(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut depth, mut quote, mut pipe) = (0, 0i32, None::<char>, false);
    for (i, ch) in line.char_indices() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '`') => quote = Some(ch),
            (None, '|') => pipe = !pipe,
            (None, '[' | '(' | '{') => depth += 1,
            (None, ']' | ')' | '}') => depth -= 1,
            (None, ':') if depth <= 0 && !pipe => {
                out.push(&line[start..]);
                return out;
            }
            (None, ';') if depth <= 0 && !pipe => {
                out.push(&line[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&line[start..]);
    out
}

fn parse_flowchart(src: &str) -> Option<Graph> {
    let src = split_frontmatter(src)?.body;
    let header = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    let mut g = Graph {
        left_right: direction_is_lr(header.split_whitespace().nth(1)),
        ..Default::default()
    };
    // Inside a subgraph, `direction` is that subgraph's: it never turns
    // the whole chart (#879).
    let mut depth = 0usize;
    for stmt in statements(src) {
        let first = stmt.split_whitespace().next().unwrap_or("");
        match first {
            "subgraph" => depth += 1,
            "end" => depth = depth.saturating_sub(1),
            _ => {}
        }
        if first == "direction" {
            if depth == 0 {
                g.left_right = direction_is_lr(stmt.split_whitespace().nth(1));
            }
            continue;
        }
        if matches!(
            first,
            "subgraph"
                | "end"
                | "classDef"
                | "class"
                | "style"
                | "linkStyle"
                | "click"
                | "accTitle"
                | "accDescr"
                | "accTitle:"
                | "accDescr:"
        ) {
            continue;
        }
        flow_statement(&mut g, stmt);
    }
    (!g.nodes.is_empty() && !g.unsupported).then_some(g)
}

struct Cursor<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> Cursor<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn ws(&mut self) {
        let r = self.rest();
        self.i += r.len() - r.trim_start().len();
    }
    fn eat(&mut self, p: &str) -> bool {
        if self.rest().starts_with(p) {
            self.i += p.len();
            true
        } else {
            false
        }
    }
    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }
}

/// `A --> B & C -- label --> D`: nodes and edges, lenient.
fn flow_statement(g: &mut Graph, stmt: &str) {
    let mut c = Cursor { s: stmt, i: 0 };
    let Some(mut prev) = node_group(g, &mut c) else {
        return;
    };
    loop {
        c.ws();
        if c.eof() {
            return;
        }
        let Some((label, dash, arrow, visible, exotic)) = flow_edge(&mut c) else {
            return;
        };
        g.unsupported |= exotic;
        c.ws();
        let Some(next) = node_group(g, &mut c) else {
            return;
        };
        if visible {
            for &a in &prev {
                for &b in &next {
                    g.edges.push(Edge {
                        from: a,
                        to: b,
                        label: label.clone(),
                        dash,
                        arrow,
                    });
                }
            }
        }
        prev = next;
    }
}

fn node_group(g: &mut Graph, c: &mut Cursor) -> Option<Vec<usize>> {
    let mut out = vec![flow_node(g, c)?];
    loop {
        let save = c.i;
        c.ws();
        if c.eat("&") {
            c.ws();
            out.push(flow_node(g, c)?);
        } else {
            c.i = save;
            return Some(out);
        }
    }
}

fn flow_node(g: &mut Graph, c: &mut Cursor) -> Option<usize> {
    let r = c.rest();
    let bytes: Vec<(usize, char)> = r.char_indices().collect();
    let mut end = 0;
    for (k, &(i, ch)) in bytes.iter().enumerate() {
        let next = bytes.get(k + 1).map(|p| p.1);
        let ok = ch.is_alphanumeric()
            || ch == '_'
            || (ch == '-' || ch == '.') && next.is_some_and(|n| n.is_alphanumeric());
        if !ok {
            break;
        }
        end = i + ch.len_utf8();
    }
    if end == 0 {
        return None;
    }
    let id = &r[..end];
    c.i += end;
    const SHAPES: [(&str, &str, BoxStyle); 13] = [
        ("(((", ")))", BoxStyle::Round),
        ("([", "])", BoxStyle::Round),
        ("[[", "]]", BoxStyle::Double),
        ("[(", ")]", BoxStyle::Round),
        ("((", "))", BoxStyle::Round),
        ("{{", "}}", BoxStyle::Double),
        ("[/", "/]", BoxStyle::Square),
        ("[\\", "\\]", BoxStyle::Square),
        ("[", "]", BoxStyle::Square),
        ("(", ")", BoxStyle::Round),
        ("{", "}", BoxStyle::Double),
        (">", "]", BoxStyle::Square),
        ("@{", "}", BoxStyle::Square),
    ];
    let mut label = None;
    let mut shape = None;
    for (open, close, style) in SHAPES {
        if c.rest().starts_with(open) {
            let body_start = c.i + open.len();
            let body = &c.s[body_start..];
            // A quoted label may contain the closer.
            let search_from = if body.trim_start().starts_with('"') {
                let q = body.find('"').unwrap_or(0);
                body[q + 1..].find('"').map_or(0, |e| q + 1 + e + 1)
            } else {
                0
            };
            let Some(close_at) = body[search_from..].find(close).map(|k| k + search_from) else {
                break;
            };
            if open != "@{" {
                label = Some(&body[..close_at]);
                shape = Some(Shape::Box(style));
            }
            c.i = body_start + close_at + close.len();
            break;
        }
    }
    if c.eat(":::") {
        while c
            .peek()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '-')
        {
            c.i += c.peek().map_or(1, char::len_utf8);
        }
    }
    Some(g.node(id, label, shape))
}

/// One link operator plus its label: `-->`, `-.->`, `==>`, `---`, `-- x -->`,
/// `-->|x|`, `<-->`, `--x`, `--o`, `~~~` (invisible).
fn flow_edge(c: &mut Cursor) -> Option<(String, Dash, bool, bool, bool)> {
    // A tip at the start (`<-->`) or a cross/circle tip can't be drawn
    // with this renderer's single arrowhead: reported, so the caller falls
    // back to the source (#879).
    let two_way = c.eat("<");
    let mut exotic = two_way;
    let mut arrow = two_way;
    let op_start = c.i;
    while c.peek().is_some_and(|ch| "-=.~".contains(ch)) {
        c.i += 1;
    }
    let mut op = c.s[op_start..c.i].to_string();
    if op.is_empty() {
        return None;
    }
    let mut label = String::new();
    let tip = |c: &mut Cursor| -> bool {
        if c.eat(">") {
            return true;
        }
        let r = c.rest();
        if (r.starts_with('x') || r.starts_with('o'))
            && r[1..].starts_with(|ch: char| ch.is_whitespace())
        {
            c.i += 1;
            return true;
        }
        false
    };
    let crossed = |c: &Cursor| c.s[..c.i].ends_with(['x', 'o']);
    if tip(c) {
        arrow = true;
        exotic |= crossed(c);
    } else if matches!(op.as_str(), "--" | "==" | "-.") && c.peek() == Some(' ') {
        // `-- label -->`: the label runs to the closing operator.
        let r = c.rest();
        let close = ["-->", "==>", ".->", "---", "===", "-.-", "--x", "--o"]
            .iter()
            .filter_map(|p| r.find(p))
            .min()?;
        label = r[..close].trim().to_string();
        c.i += close;
        let s = c.i;
        while c.peek().is_some_and(|ch| "-=.".contains(ch)) {
            c.i += 1;
        }
        op.push_str(&c.s[s..c.i]);
        if tip(c) {
            arrow = true;
            exotic |= crossed(c);
        }
    }
    c.ws();
    if c.eat("|") {
        let r = c.rest();
        let end = r.find('|').unwrap_or(r.len());
        label = r[..end].trim().trim_matches('"').to_string();
        c.i += (end + 1).min(r.len());
    }
    let dash = if op.contains('.') {
        Dash::Dotted
    } else if op.contains('=') {
        Dash::Thick
    } else {
        Dash::Solid
    };
    Some((label, dash, arrow, !op.contains('~'), exotic))
}

fn parse_state(src: &str) -> Option<Graph> {
    let mut g = Graph::default();
    let mut start = None;
    let mut end = None;
    // A multi-line `note … end note` body is text, never states (#879).
    let mut in_note = false;
    for stmt in statements(src) {
        let stmt = stmt.trim_end_matches('{').trim();
        if stmt.is_empty() || stmt == "}" {
            continue;
        }
        if in_note {
            in_note = stmt != "end note";
            continue;
        }
        if stmt.starts_with("note ") && !stmt.contains(':') {
            in_note = true;
            continue;
        }
        let first = stmt.split_whitespace().next().unwrap_or("");
        if first == "direction" {
            g.left_right = direction_is_lr(stmt.split_whitespace().nth(1));
            continue;
        }
        if matches!(
            first,
            "note" | "end" | "classDef" | "class" | "style" | "%%"
        ) {
            continue;
        }
        if let Some(rest) = stmt.strip_prefix("state ") {
            // `state "Long name" as Id` / `state Id`
            let rest = rest.trim();
            if let Some((desc, id)) = rest.split_once(" as ") {
                g.node(id.trim(), Some(desc), Some(Shape::Box(BoxStyle::Round)));
            } else {
                let id = rest.split_whitespace().next().unwrap_or(rest);
                g.node(id, None, Some(Shape::Box(BoxStyle::Round)));
            }
            continue;
        }
        let (body, label) = match stmt.split_once(':') {
            Some((b, l)) => (b.trim(), l.trim()),
            None => (stmt, ""),
        };
        let mut endpoint = |g: &mut Graph, id: &str, is_target: bool| -> usize {
            if id == "[*]" {
                let slot = if is_target { &mut end } else { &mut start };
                *slot.get_or_insert_with(|| {
                    let (key, glyph) = if is_target {
                        ("[*]end", '◉')
                    } else {
                        ("[*]start", '●')
                    };
                    g.node(key, Some(""), Some(Shape::Dot(glyph)))
                })
            } else {
                g.node(id, None, Some(Shape::Box(BoxStyle::Round)))
            }
        };
        if let Some((a, b)) = body.split_once("-->") {
            let from = endpoint(&mut g, a.trim(), false);
            let to = endpoint(&mut g, b.trim(), true);
            g.edges.push(Edge {
                from,
                to,
                label: label.to_string(),
                dash: Dash::Solid,
                arrow: true,
            });
        } else if !label.is_empty() && !body.contains(' ') {
            g.node(body, Some(label), Some(Shape::Box(BoxStyle::Round)));
        }
    }
    (!g.nodes.is_empty()).then_some(g)
}

// ----------------------------------------------------------- graph layout

struct LNode {
    real: Option<usize>,
    layer: usize,
    /// Position along the rank axis (start) and its extent.
    r0: usize,
    rlen: usize,
    /// Position along the order axis (start, may go negative mid-layout).
    o0: i64,
    olen: usize,
}

struct LEdge {
    a: usize,
    b: usize,
    edge: usize,
    first: bool,
    last: bool,
    /// Drawn against the layout direction (a cycle-breaking back edge).
    reversed: bool,
}

fn graph_lines(g: Graph, color: bool, width: usize) -> Option<Vec<Line<'static>>> {
    let preferred = g.left_right;
    let canvas = layout(&g, preferred)
        .filter(|c| c.width() <= width)
        .or_else(|| layout(&g, !preferred).filter(|c| c.width() <= width))?;
    Some(canvas.into_lines(color))
}

fn layout(g: &Graph, lr: bool) -> Option<Canvas> {
    let n = g.nodes.len();
    let edges: Vec<(usize, usize, usize)> = g
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.from != e.to)
        .map(|(i, e)| (e.from, e.to, i))
        .collect();
    let self_loops: Vec<(usize, &str)> = g
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| (e.from, e.label.as_str()))
        .collect();

    // 1. Break cycles: DFS back edges are reversed for layering.
    let mut out: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
    for (k, &(a, b, _)) in edges.iter().enumerate() {
        out[a].push((b, k));
    }
    let mut reversed = vec![false; edges.len()];
    let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
    for root in 0..n {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        state[root] = 1;
        while let Some(&mut (v, ref mut idx)) = stack.last_mut() {
            if let Some(&(w, k)) = out[v].get(*idx) {
                *idx += 1;
                match state[w] {
                    0 => {
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    1 => reversed[k] = true,
                    _ => {}
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }
    let dag: Vec<(usize, usize)> = edges
        .iter()
        .zip(&reversed)
        .map(|(&(a, b, _), &r)| if r { (b, a) } else { (a, b) })
        .collect();

    // 2. Longest-path layers, then pull sources down next to their children.
    let mut indeg = vec![0usize; n];
    for &(_, b) in &dag {
        indeg[b] += 1;
    }
    let mut queue: Vec<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
    let mut layer = vec![0usize; n];
    let mut qi = 0;
    while qi < queue.len() {
        let v = queue[qi];
        qi += 1;
        for &(a, b) in &dag {
            if a == v {
                layer[b] = layer[b].max(layer[a] + 1);
                indeg[b] -= 1;
                if indeg[b] == 0 {
                    queue.push(b);
                }
            }
        }
    }
    for v in 0..n {
        let has_in = dag.iter().any(|&(_, b)| b == v);
        let min_child = dag
            .iter()
            .filter(|&&(a, _)| a == v)
            .map(|&(_, b)| layer[b])
            .min();
        if let (false, Some(m)) = (has_in, min_child) {
            layer[v] = m.saturating_sub(1).max(layer[v]);
        }
    }

    // 3. Layered graph with dummy nodes on long edges.
    let label_of = |v: usize| {
        let mut l = g.nodes[v].label.clone();
        // A self-loop is a ↺ after the label, with its own label (#879).
        for (_, label) in self_loops.iter().filter(|(n, _)| *n == v) {
            if let Some(last) = l.last_mut() {
                last.push_str(" ↺");
                if !label.is_empty() {
                    last.push(' ');
                    last.push_str(label);
                }
            }
        }
        l
    };
    let mut ln: Vec<LNode> = (0..n)
        .map(|v| {
            let (w, h) = match g.nodes[v].shape {
                Shape::Dot(_) => (1, 1),
                Shape::Box(_) => {
                    let l = label_of(v);
                    (
                        l.iter().map(|s| s.width()).max().unwrap_or(0) + 4,
                        l.len() + 2,
                    )
                }
            };
            let (rlen, olen) = if lr { (w, h) } else { (h, w) };
            LNode {
                real: Some(v),
                layer: layer[v],
                r0: 0,
                rlen,
                o0: 0,
                olen,
            }
        })
        .collect();
    let mut le: Vec<LEdge> = Vec::new();
    for (k, &(a, b)) in dag.iter().enumerate() {
        let mut prev = a;
        let span = layer[b] - layer[a];
        for step in 1..span {
            ln.push(LNode {
                real: None,
                layer: layer[a] + step,
                r0: 0,
                rlen: 0,
                o0: 0,
                olen: 1,
            });
            let d = ln.len() - 1;
            le.push(LEdge {
                a: prev,
                b: d,
                edge: edges[k].2,
                first: step == 1,
                last: false,
                reversed: reversed[k],
            });
            prev = d;
        }
        le.push(LEdge {
            a: prev,
            b,
            edge: edges[k].2,
            first: span == 1,
            last: true,
            reversed: reversed[k],
        });
    }
    let layers_n = ln.iter().map(|v| v.layer).max().unwrap_or(0) + 1;
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layers_n];
    for (i, v) in ln.iter().enumerate() {
        layers[v.layer].push(i);
    }

    // 4. Barycentre ordering sweeps.
    let mut pos = vec![0f64; ln.len()];
    let renumber = |layers: &Vec<Vec<usize>>, pos: &mut Vec<f64>| {
        for l in layers {
            for (i, &v) in l.iter().enumerate() {
                pos[v] = i as f64;
            }
        }
    };
    renumber(&layers, &mut pos);
    for sweep in 0..6 {
        let down = sweep % 2 == 0;
        let order: Vec<usize> = if down {
            (1..layers_n).collect()
        } else {
            (0..layers_n.saturating_sub(1)).rev().collect()
        };
        for k in order {
            let mut keyed: Vec<(f64, usize)> = layers[k]
                .iter()
                .map(|&v| {
                    let nb: Vec<f64> = le
                        .iter()
                        .filter_map(|e| match down {
                            true if e.b == v => Some(pos[e.a]),
                            false if e.a == v => Some(pos[e.b]),
                            _ => None,
                        })
                        .collect();
                    let key = if nb.is_empty() {
                        pos[v]
                    } else {
                        nb.iter().sum::<f64>() / nb.len() as f64
                    };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
            layers[k] = keyed.into_iter().map(|(_, v)| v).collect();
            renumber(&layers, &mut pos);
        }
    }

    // 5. Order coordinates: align with neighbours, never overlap.
    let gap_o: i64 = if lr { 1 } else { 3 };
    let center = |v: &LNode| v.o0 + v.olen as i64 / 2;
    for layer in &layers {
        let mut x = 0i64;
        for &v in layer {
            ln[v].o0 = x;
            x += ln[v].olen as i64 + gap_o;
        }
    }
    for sweep in 0..4 {
        let down = sweep % 2 == 0;
        let order: Vec<usize> = if down {
            (1..layers_n).collect()
        } else {
            (0..layers_n.saturating_sub(1)).rev().collect()
        };
        for k in order {
            let mut prev_end: Option<i64> = None;
            for &v in &layers[k] {
                let nb: Vec<i64> = le
                    .iter()
                    .filter_map(|e| match down {
                        true if e.b == v => Some(center(&ln[e.a])),
                        false if e.a == v => Some(center(&ln[e.b])),
                        _ => None,
                    })
                    .collect();
                let mut o = if nb.is_empty() {
                    ln[v].o0
                } else {
                    nb.iter().sum::<i64>() / nb.len() as i64 - ln[v].olen as i64 / 2
                };
                if let Some(p) = prev_end {
                    o = o.max(p + gap_o);
                }
                ln[v].o0 = o;
                prev_end = Some(o + ln[v].olen as i64);
            }
        }
    }
    let min_o = ln.iter().map(|v| v.o0).min().unwrap_or(0);
    for v in &mut ln {
        v.o0 -= min_o;
    }

    // 6. Rank coordinates. Left-right gaps widen to fit edge labels; a
    //    top-down gap shrinks to two rows when every edge runs straight.
    let mut label_w = vec![0usize; layers_n];
    for e in le.iter().filter(|e| e.first) {
        let k = ln[e.a].layer;
        label_w[k] = label_w[k].max(g.edges[e.edge].label.width());
    }
    let mut starts = Vec::with_capacity(layers_n);
    let mut ends = Vec::with_capacity(layers_n);
    let mut r = 0usize;
    for k in 0..layers_n {
        let extent = layers[k]
            .iter()
            .filter(|&&v| ln[v].real.is_some())
            .map(|&v| ln[v].rlen)
            .max()
            .unwrap_or(1);
        starts.push(r);
        ends.push(r + extent - 1);
        for &v in &layers[k] {
            if ln[v].real.is_some() {
                ln[v].r0 = r + (extent - ln[v].rlen) / 2;
            } else {
                ln[v].r0 = r;
                ln[v].rlen = extent;
            }
        }
        let lw = label_w[k];
        let straight = le.iter().filter(|e| ln[e.a].layer == k).all(|e| {
            center(&ln[e.a]) == center(&ln[e.b]) && (!e.first || g.edges[e.edge].label.is_empty())
        });
        let gap = if lr && lw > 0 {
            5 + lw
        } else if !lr && straight {
            2
        } else {
            4
        };
        r += extent + gap;
    }

    // 7. Draw: edges, labels, then boxes on top.
    let at = |r: usize, o: usize| if lr { (r, o) } else { (o, r) };
    let mut cv = Canvas::default();
    let (arrow_fwd, arrow_back) = if lr { ('▶', '◀') } else { ('▼', '▲') };
    let mut labels: Vec<(usize, usize, &str)> = Vec::new();
    let mut junctions: Vec<(usize, usize, char)> = Vec::new();
    for e in &le {
        let (a, b) = (&ln[e.a], &ln[e.b]);
        let edge = &g.edges[e.edge];
        let (ao, bo) = (center(a) as usize, center(b) as usize);
        let exit = if a.real.is_some() {
            a.r0 + a.rlen
        } else {
            ends[a.layer]
        };
        let bus = ends[a.layer] + 2;
        let entry = if b.real.is_some() {
            b.r0 - 1
        } else {
            starts[b.layer]
        };
        cv.seg(at(exit, ao), at(bus, ao), edge.dash);
        cv.seg(at(bus, ao), at(bus, bo), edge.dash);
        cv.seg(at(bus, bo), at(entry, bo), edge.dash);
        if b.real.is_none() {
            cv.seg(at(b.r0, bo), at(b.r0 + b.rlen - 1, bo), edge.dash);
        }
        let real_box = |v: &LNode| {
            v.real
                .is_some_and(|i| matches!(g.nodes[i].shape, Shape::Box(_)))
        };
        if real_box(a) {
            let (x, y) = at(a.r0 + a.rlen - 1, ao);
            junctions.push((x, y, if lr { '├' } else { '┬' }));
        }
        let head_at_b = e.last && edge.arrow && !e.reversed;
        if head_at_b {
            let (x, y) = at(entry, bo);
            cv.glyph(x, y, arrow_fwd, Ink::Edge);
        } else if real_box(b) {
            let (x, y) = at(b.r0, bo);
            junctions.push((x, y, if lr { '┤' } else { '┴' }));
        }
        if e.first && edge.arrow && e.reversed {
            let (x, y) = at(exit, ao);
            cv.glyph(x, y, arrow_back, Ink::Edge);
        }
        if e.first && !edge.label.is_empty() {
            let (x, y) = if lr { (bus + 2, bo) } else { (bo + 2, bus + 1) };
            labels.push((x, y, &edge.label));
        }
    }
    for (x, y, text) in labels {
        cv.text(x, y, text, Ink::Label);
    }
    for v in ln.iter().filter(|v| v.real.is_some()) {
        let node = &g.nodes[v.real.unwrap_or(0)];
        let (x, y) = at(v.r0, v.o0 as usize);
        let (w, h) = if lr {
            (v.rlen, v.olen)
        } else {
            (v.olen, v.rlen)
        };
        match node.shape {
            Shape::Dot(ch) => cv.glyph(x, y, ch, Ink::Border),
            Shape::Box(style) => {
                cv.rect(x, y, w, h, style);
                for (i, line) in label_of(v.real.unwrap_or(0)).iter().enumerate() {
                    let lx = x + (w - line.width()) / 2;
                    cv.text(lx, y + 1 + i, line, Ink::Text);
                }
            }
        }
    }
    for (x, y, ch) in junctions {
        cv.glyph(x, y, ch, Ink::Border);
    }
    Some(cv)
}

// ---------------------------------------------------------------- sequence

enum SeqEv {
    Msg {
        from: usize,
        to: usize,
        text: String,
        dash: Dash,
        head: char,
    },
    Note {
        lo: usize,
        hi: usize,
        side: i8,
        text: String,
    },
    Block(String),
}

fn sequence(src: &str, color: bool, width: usize) -> Option<Vec<Line<'static>>> {
    let mut names: Vec<String> = Vec::new();
    let mut ids: HashMap<String, usize> = HashMap::new();
    let mut part = |names: &mut Vec<String>, id: &str, label: Option<&str>| -> usize {
        let id = id.trim();
        let i = *ids.entry(id.to_string()).or_insert_with(|| {
            names.push(id.to_string());
            names.len() - 1
        });
        if let Some(l) = label {
            names[i] = l.trim().to_string();
        }
        i
    };
    let mut evs = Vec::new();
    let mut blocks = Vec::new(); // true: visible control block; false: participant box
    let mut numbered = false;
    let mut n = 0;
    for stmt in statements(src) {
        let (head, rest) = stmt.split_once(char::is_whitespace).unwrap_or((stmt, ""));
        match head {
            "participant" | "actor" => {
                match rest.split_once(" as ") {
                    Some((id, label)) => part(&mut names, id, Some(label)),
                    None => part(&mut names, rest, None),
                };
            }
            "autonumber" => numbered = true,
            "loop" | "alt" | "opt" | "par" | "critical" | "break" | "rect" => {
                blocks.push(true);
                evs.push(SeqEv::Block(format!("{head} {rest}").trim().to_string()));
            }
            "else" | "and" => evs.push(SeqEv::Block(format!("{head} {rest}").trim().to_string())),
            "box" => blocks.push(false),
            "end" => {
                if blocks.pop() == Some(true) {
                    evs.push(SeqEv::Block(String::new()));
                }
            }
            "activate" | "deactivate" | "create" | "destroy" | "title" | "links" | "link" => {}
            _ if head.eq_ignore_ascii_case("note") => {
                let Some((pos, text)) = rest.split_once(':') else {
                    continue;
                };
                let pos = pos.trim();
                let (side, who) = if let Some(w) = pos.strip_prefix("left of") {
                    (-1, w)
                } else if let Some(w) = pos.strip_prefix("right of") {
                    (1, w)
                } else {
                    (0, pos.strip_prefix("over").unwrap_or(pos))
                };
                let ps: Vec<usize> = who.split(',').map(|w| part(&mut names, w, None)).collect();
                let lo = *ps.iter().min()?;
                let hi = *ps.iter().max()?;
                evs.push(SeqEv::Note {
                    lo,
                    hi,
                    side,
                    text: text.trim().to_string(),
                });
            }
            _ => {
                let ops = [
                    ("-->>", '▶', Dash::Dotted),
                    ("->>", '▶', Dash::Solid),
                    ("-->", '›', Dash::Dotted),
                    ("->", '›', Dash::Solid),
                    ("--x", '×', Dash::Dotted),
                    ("-x", '×', Dash::Solid),
                    ("--)", '▷', Dash::Dotted),
                    ("-)", '▷', Dash::Solid),
                ];
                // The first `-` that starts an arrow: participant names
                // may hold hyphens themselves (`my-svc->>db`) (#879).
                let Some((dash_at, &(op, head, dash))) =
                    stmt.match_indices('-').find_map(|(i, _)| {
                        ops.iter()
                            .find(|(op, ..)| stmt[i..].starts_with(op))
                            .map(|o| (i, o))
                    })
                else {
                    continue;
                };
                let from = &stmt[..dash_at];
                let r = &stmt[dash_at..];
                let r = r[op.len()..].trim_start_matches(['+', '-']);
                let (to, text) = r.split_once(':').unwrap_or((r, ""));
                if from.trim().is_empty() || to.trim().is_empty() {
                    continue;
                }
                let from = part(&mut names, from, None);
                let to = part(&mut names, to, None);
                n += 1;
                let text = if numbered {
                    format!("{n}. {}", text.trim())
                } else {
                    text.trim().to_string()
                };
                evs.push(SeqEv::Msg {
                    from,
                    to,
                    text,
                    dash,
                    head,
                });
            }
        }
    }
    if names.is_empty() {
        return None;
    }

    // Column centres: boxes fit, then every message label fits its span.
    let bw: Vec<usize> = names.iter().map(|s| s.width() + 4).collect();
    let mut c = vec![bw[0] / 2];
    for i in 1..names.len() {
        c.push(c[i - 1] + (bw[i - 1] + bw[i]) / 2 + 2);
    }
    let mut right = c[c.len() - 1] + bw[bw.len() - 1] / 2 + 1;
    let widen = |c: &mut Vec<usize>, from: usize, by: usize| {
        for x in &mut c[from..] {
            *x += by;
        }
    };
    for ev in &evs {
        match ev {
            SeqEv::Msg { from, to, text, .. } if from != to => {
                let (lo, hi) = ((*from).min(*to), (*from).max(*to));
                let need = text.width() + 4;
                if c[hi] - c[lo] < need {
                    let d = need - (c[hi] - c[lo]);
                    widen(&mut c, hi, d);
                }
            }
            SeqEv::Msg { from, text, .. } => {
                let need = text.width().max(4) + 4;
                if *from + 1 < c.len() && c[*from + 1] - c[*from] < need {
                    let d = need - (c[*from + 1] - c[*from]);
                    widen(&mut c, *from + 1, d);
                }
                right = right.max(c[*from] + need);
            }
            SeqEv::Note { lo, side, text, .. } if *side != 0 => {
                if *side > 0 {
                    right = right.max(c[*lo] + text.width() + 6);
                } else if c[*lo] < text.width() + 6 {
                    let d = text.width() + 6 - c[*lo];
                    widen(&mut c, 0, d);
                }
            }
            _ => {}
        }
    }
    let last = c.len() - 1;
    right = right.max(c[last] + bw[last] / 2 + 1);
    if right > width {
        return None;
    }

    let mut cv = Canvas::default();
    let header = |cv: &mut Canvas, y: usize, bottom: bool| {
        for (i, name) in names.iter().enumerate() {
            let x = c[i] - bw[i] / 2;
            cv.rect(x, y, bw[i], 3, BoxStyle::Square);
            cv.text(x + 2, y + 1, name, Ink::Title);
            let (jy, ch) = if bottom { (y, '┴') } else { (y + 2, '┬') };
            cv.glyph(c[i], jy, ch, Ink::Border);
        }
    };
    let rows: usize = evs
        .iter()
        .map(|e| match e {
            SeqEv::Msg { from, to, .. } if from == to => 3,
            SeqEv::Msg { .. } => 2,
            SeqEv::Note { .. } => 3,
            SeqEv::Block(_) => 1,
        })
        .sum::<usize>()
        + 1;
    let bottom = 3 + rows;
    for &x in &c {
        cv.seg((x, 3), (x, bottom), Dash::Solid);
    }
    let mut y = 4;
    for ev in &evs {
        match ev {
            SeqEv::Msg {
                from,
                to,
                text,
                dash,
                head,
            } if from == to => {
                let x = c[*from];
                cv.text(x + 2, y, text, Ink::Label);
                cv.seg((x, y + 1), (x + 3, y + 1), *dash);
                cv.seg((x + 3, y + 1), (x + 3, y + 2), *dash);
                cv.seg((x + 3, y + 2), (x + 1, y + 2), *dash);
                cv.glyph(
                    x + 1,
                    y + 2,
                    if *head == '›' { '‹' } else { '◀' },
                    Ink::Edge,
                );
                y += 3;
            }
            SeqEv::Msg {
                from,
                to,
                text,
                dash,
                head,
            } => {
                let (a, b) = (c[*from], c[*to]);
                let (lo, hi) = (a.min(b), a.max(b));
                cv.text(lo + (hi - lo - text.width()) / 2, y, text, Ink::Label);
                let tip = if b > a { b - 1 } else { b + 1 };
                cv.seg((a, y + 1), (tip, y + 1), *dash);
                let glyph = match (*head, b > a) {
                    (h, true) => h,
                    ('▶', false) => '◀',
                    ('›', false) => '‹',
                    ('▷', false) => '◁',
                    (h, false) => h,
                };
                cv.glyph(tip, y + 1, glyph, Ink::Edge);
                y += 2;
            }
            SeqEv::Note { lo, hi, side, text } => {
                let w = text.width() + 4;
                let x = match side {
                    -1 => c[*lo] - 1 - w,
                    1 => c[*lo] + 2,
                    _ => {
                        let mid = (c[*lo] + c[*hi]) / 2;
                        let span = c[*hi] - c[*lo] + 4;
                        let w2 = w.max(span);
                        let x = mid.saturating_sub(w2 / 2);
                        cv.rect(x, y, w2, 3, BoxStyle::Round);
                        cv.text(x + (w2 - text.width()) / 2, y + 1, text, Ink::Text);
                        y += 3;
                        continue;
                    }
                };
                cv.rect(x, y, w, 3, BoxStyle::Round);
                cv.text(x + 2, y + 1, text, Ink::Text);
                y += 3;
            }
            SeqEv::Block(text) => {
                cv.seg((0, y), (right.saturating_sub(1), y), Dash::Dotted);
                if !text.is_empty() {
                    cv.text(1, y, &format!(" {text} "), Ink::Label);
                }
                y += 1;
            }
        }
    }
    header(&mut cv, 0, false);
    header(&mut cv, bottom, true);
    if cv.width() > width {
        return None;
    }
    Some(cv.into_lines(color))
}

// --------------------------------------------------------------------- pie

fn pie(src: &str, color: bool, width: usize) -> Option<Vec<Line<'static>>> {
    let src = split_frontmatter(src)?.body;
    let mut slices: Vec<(String, f64)> = Vec::new();
    for stmt in statements(src) {
        if stmt.starts_with("title") {
            continue;
        }
        if let Some((label, value)) = stmt.rsplit_once(':') {
            if let Ok(v) = value.trim().parse::<f64>() {
                slices.push((label.trim().trim_matches('"').to_string(), v.max(0.0)));
            }
        }
    }
    let total: f64 = slices.iter().map(|s| s.1).sum();
    if slices.is_empty() || total <= 0.0 {
        return None;
    }
    let label_w = slices
        .iter()
        .map(|s| s.0.width())
        .max()
        .unwrap_or(0)
        .min(28);
    // Reserve the label, separating space and widest formatted percentage
    // before allocating bar cells. Refuse a width that cannot show a readable
    // four-cell bar; the caller can use its wider preview/pager fallback.
    let percentages: Vec<_> = slices
        .iter()
        .map(|(_, value)| format!(" {:.1}%", value / total * 100.0))
        .collect();
    let percent_w = percentages.iter().map(|value| value.width()).max()?;
    let bar_w = width.checked_sub(label_w + 1 + percent_w)?.min(40);
    if bar_w < 4 {
        return None;
    }
    let max = slices.iter().map(|s| s.1).fold(0.0, f64::max);
    const PALETTE: [Color; 6] = [
        Color::Cyan,
        Color::Green,
        Color::Yellow,
        Color::Magenta,
        Color::Blue,
        Color::Red,
    ];
    let mut lines = Vec::new();
    for (i, (label, v)) in slices.iter().enumerate() {
        let cells = if *v == 0.0 {
            0
        } else {
            (((v / max) * bar_w as f64).round() as usize).max(1)
        };
        let shown = super::ui::pad(label, label_w);
        lines.push(Line::from(vec![
            Span::raw(format!("{shown} ")),
            Span::styled("█".repeat(cells), fg(color, PALETTE[i % PALETTE.len()])),
            Span::styled(percentages[i].clone(), super::ui::dim(color)),
        ]));
    }
    if lines.iter().any(|line| line.width() > width) {
        return None;
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(src: &str, width: usize) -> Vec<String> {
        render(src, false, width)
            .expect("renders")
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn flowchart_top_down_boxes_and_arrows() {
        let out = text(
            "graph TD\n  A[Start] --> B{Ready?}\n  B -->|yes| C(Go)\n  B -- no --> A",
            80,
        );
        let all = out.join("\n");
        assert!(all.contains("│ Start │"), "{all}");
        assert!(all.contains("║ Ready? ║"), "{all}");
        assert!(all.contains("│ Go │"), "{all}");
        assert!(
            all.contains('▼') && all.contains('▲'),
            "back edge gets an up arrow:\n{all}"
        );
        assert!(all.contains("yes") && all.contains("no"), "{all}");
        // Start sits above Ready? above Go.
        let row = |s: &str| out.iter().position(|l| l.contains(s)).unwrap();
        assert!(
            row("Start") < row("Ready?") && row("Ready?") < row("Go"),
            "{all}"
        );
    }

    #[test]
    fn flowchart_left_right_and_chains() {
        let out = text("flowchart LR\nA --> B --> C\nA -.-> C", 80);
        let all = out.join("\n");
        assert!(all.contains('▶'), "{all}");
        assert!(all.contains('┄'), "dotted edge:\n{all}");
        let col = |s: &str| {
            out.iter()
                .find_map(|l| l.find(s).map(|b| l[..b].chars().count()))
                .unwrap()
        };
        assert!(col("│ A") < col("│ B") && col("│ B") < col("│ C"), "{all}");
    }

    #[test]
    fn too_wide_falls_back_to_none() {
        let src = "graph TD\nA[aaaaaaaaaaaa] --> B[bbbbbbbbbbbbbbbb] & C[cccccccccccccccc] & D[dddddddddddddd]";
        assert!(render(src, false, 30).is_none());
        assert!(
            render("gantt\n title x", false, 80).is_none(),
            "unsupported kinds fall back"
        );
    }

    #[test]
    fn state_diagram_has_start_and_end() {
        let all = text(
            "stateDiagram-v2\n[*] --> Idle\nIdle --> Busy : job\nBusy --> Idle\nBusy --> [*]",
            80,
        )
        .join("\n");
        assert!(all.contains('●') && all.contains('◉'), "{all}");
        assert!(
            all.contains("╭") && all.contains("Idle") && all.contains("job"),
            "{all}"
        );
    }

    #[test]
    fn sequence_lifelines_and_messages() {
        let out = text(
            "sequenceDiagram\nautonumber\nparticipant A as Alice\nA->>Bob: Hello\nBob-->>A: Hi\nloop retry\nA->>A: think\nend\nNote over A,Bob: done",
            80,
        );
        let all = out.join("\n");
        assert!(all.contains("Alice") && all.contains("Bob"), "{all}");
        assert!(all.contains("1. Hello") && all.contains("2. Hi"), "{all}");
        assert!(all.contains('▶') && all.contains('◀'), "{all}");
        assert!(all.contains("loop retry") && all.contains("done"), "{all}");
        assert_eq!(
            out.iter().filter(|l| l.contains("Alice")).count(),
            2,
            "header and footer:\n{all}"
        );
    }

    #[test]
    fn pie_bars() {
        let out = text("pie title Pets\n\"Dogs\" : 3\n\"Cats\" : 1", 60);
        assert_eq!(out[0], "Pets");
        assert!(
            out[1].starts_with("Dogs ") && out[1].ends_with("75.0%"),
            "{out:?}"
        );
        assert!(out[2].contains("25.0%"));
    }

    #[test]
    fn pie_zero_and_clamped_negative_slices_have_no_positive_bar() {
        let source = "pie\n\"Zero\":0\n\"Negative\":-1\n\"Tiny\":0.000001\n\"Positive\":1";
        for color in [false, true] {
            let diagram = render(source, color, 60).unwrap();
            for row in &diagram.lines[..2] {
                let text: String = row.spans.iter().map(|span| span.content.as_ref()).collect();
                assert!(text.ends_with("0.0%"), "{text}");
                assert!(!text.contains('█'), "{text}");
            }
            assert_eq!(diagram.lines[2].spans[1].content, "█");
            assert!(diagram.lines[3].spans[1].content.chars().count() > 1);
            assert!(render("pie\n\"A\":0\n\"B\":-1", color, 60).is_none());
        }
    }

    #[test]
    fn pie_lines_and_titles_fit_requested_display_width_or_refuse() {
        for source in [
            "pie\n\"abcdefghijklmnopqrstuvwxyzab\":1".to_string(),
            format!("pie title {}\n\"A\":1", "Long title ".repeat(8)),
            format!("pie\n\"{}\":1\n\"ไทย👩🏽‍💻\":2", "表".repeat(30)),
            "pie\n\"\":1".to_string(),
        ] {
            for color in [false, true] {
                for width in 0..=120 {
                    if let Some(diagram) = render(&source, color, width) {
                        assert!(
                            diagram.lines.iter().all(|line| line.width() <= width),
                            "width {width}: {source}"
                        );
                    }
                }
                assert!(render(&source, color, 200).is_some());
            }
        }
        assert!(render("pie\n\"abcdefghijklmnopqrstuvwxyzab\":1", false, 30).is_none());
        assert!(render("pie\n\"A\":1", false, 3).is_none());
    }

    /// `;` inside a label or after a message's `:` is text (#878).
    #[test]
    fn semicolons_in_labels_and_messages_are_text() {
        let all = text("flowchart LR\n  A[\"wait; retry\"] --> B", 80).join("\n");
        assert!(all.contains("wait; retry"), "{all}");
        assert!(
            all.contains("│ B ") && all.contains('▶'),
            "the edge stays:\n{all}"
        );
        assert!(!all.contains("│ retry"), "{all}");
        let all = text("sequenceDiagram\n  A->>B: hello; world", 80).join("\n");
        assert!(all.contains("hello; world"), "{all}");
        // A plain `;` still separates statements.
        let all = text("graph LR\n  A --> B; B --> C", 80).join("\n");
        assert!(all.contains("│ A ") && all.contains("│ C │"), "{all}");
    }

    #[test]
    fn frontmatter_is_metadata_for_every_supported_diagram() {
        for body in [
            "flowchart LR\nA --> B",
            "stateDiagram-v2\n[*] --> Idle",
            "sequenceDiagram\nA->>B: Hello",
            "pie\n\"Dogs\" : 3\n\"Cats\" : 1",
        ] {
            let source = format!("\u{feff}\r\n%% comment\r\n---\r\ntitle: Checkout flow\r\nconfig:\r\n  title: Not a heading\r\n  theme: dark\r\n---\r\n{body}");
            let out = text(&source, 80);
            assert_eq!(out[0], "Checkout flow");
            assert_eq!(&out[1..], text(body, 80));
            assert_eq!(
                statements(&source).collect::<Vec<_>>(),
                statements(body).collect::<Vec<_>>()
            );
            assert!(render(&source, true, 80).is_some());
        }
        assert!(
            parse_flowchart("---\ntitle: LR\n---\nflowchart LR\nA --> B")
                .unwrap()
                .left_right
        );
        assert_eq!(
            pie("---\nconfig:\n  theme: dark\n---\npie\n\"A\":1", false, 80)
                .unwrap()
                .len(),
            1
        );
        assert!(render("---\ntitle: Unclosed\nflowchart LR\nA-->B", false, 80).is_none());
        assert!(render("---\ntitle: No diagram\n---", false, 80).is_none());
        assert!(render("---\ntitle: Unsupported\n---\ngantt", false, 80).is_none());
    }

    #[test]
    fn diagram_titles_are_unquoted_once_and_respect_the_pane() {
        let body = "flowchart LR\nA --> B";
        for (metadata, expected) in [
            ("title: Plain # comment", "Plain"),
            ("title: \"Quoted # title\" # comment", "Quoted # title"),
            ("title: 'Checkout''s flow'", "Checkout's flow"),
            ("title: |-\n  Checkout\n  flow", "Checkout\nflow"),
            ("title: |-\n  ---\n  Checkout", "---\nCheckout"),
            ("title: >-\n  Checkout\n  flow", "Checkout flow"),
        ] {
            let out = text(&format!("---\n{metadata}\n---\n{body}"), 80);
            assert!(out.join("\n").starts_with(expected), "{out:?}");
            assert_eq!(out.len(), text(body, 80).len() + expected.lines().count());
        }
        let source = "---\ntitle: Preferred\n---\npie title Old\n\"A\":1";
        let out = text(source, 80);
        assert_eq!(out[0], "Preferred");
        assert!(!out.iter().any(|line| line == "Old"));
        assert_eq!(
            text("sequenceDiagram\ntitle Request flow\nA->>B: hi", 80)[0],
            "Request flow"
        );
        assert!(render(
            &format!("---\ntitle: {}\n---\n{body}", "long".repeat(30)),
            false,
            80
        )
        .is_none());
        let nested = format!("---\nconfig:\n  title: Not root\n---\n{body}");
        assert_eq!(text(&nested, 80), text(body, 80));
        let tagged = format!("---\ntitle: !unsupported value\n---\n{body}");
        assert_eq!(text(&tagged, 80), text(body, 80));
        let controls = format!(
            r#"---
title: "\u001b[31mHi\u202e"
---
{body}"#
        );
        let out = text(&controls, 80);
        assert_eq!(out[0], r"\u{1b}[31mHi\u{202e}");
        assert!(!out[0].contains('\x1b') && !out[0].contains('\u{202e}'));
        let wide_controls = format!("---\ntitle: \"{}\"\n---\n{body}", "\\u0000".repeat(20));
        assert!(
            render(&wide_controls, false, 80).is_none(),
            "measure visible escapes before fitting"
        );
    }

    #[test]
    fn participant_boxes_close_silently_without_consuming_control_blocks() {
        let plain = "sequenceDiagram\nparticipant UI\nparticipant API\nUI->>API: GET /cart";
        let boxed = "sequenceDiagram\nbox Frontend\nparticipant UI\nend\nparticipant API\nUI->>API: GET /cart";
        assert_eq!(text(boxed, 80), text(plain, 80));
        for block in [
            "loop retry",
            "alt success",
            "opt optional",
            "par work",
            "critical access",
            "break failure",
            "rect rgb(0,0,0)",
        ] {
            let plain = format!("sequenceDiagram\n{block}\nparticipant UI\nUI->>API: hi\nend");
            let inner_box = format!(
                "sequenceDiagram\n{block}\nbox Frontend\nparticipant UI\nend\nUI->>API: hi\nend"
            );
            let outer_box = format!(
                "sequenceDiagram\nbox Frontend\n{block}\nparticipant UI\nUI->>API: hi\nend\nend"
            );
            assert_eq!(text(&inner_box, 80), text(&plain, 80), "{block}");
            assert_eq!(text(&outer_box, 80), text(&plain, 80), "{block}");
        }
        let branches = "sequenceDiagram\nalt ok\nA->>B: yes\nelse failed\nA->>B: no\nend\npar work\nA->>B: one\nand more\nA->>B: two\nend";
        let boxed = branches.replace("A->>B: no", "box Group\nA->>B: no\nend");
        assert_eq!(text(&boxed, 80), text(branches, 80));
        assert_eq!(
            text(&format!("{plain}\nend"), 80),
            text(plain, 80),
            "unmatched end is not a divider"
        );
    }

    /// Constructs that used to draw a wrong picture now draw the right one
    /// or fall back to the source (#879).
    #[test]
    fn no_wrong_pictures() {
        // Hyphenated participants.
        let all = text(
            "sequenceDiagram\n  my-svc->>db: query\n  db-->>my-svc: rows",
            80,
        )
        .join("\n");
        assert!(
            all.contains("my-svc") && all.contains("query") && all.contains("rows"),
            "{all}"
        );
        // A note's body is not a state.
        let all = text("stateDiagram-v2\n  [*] --> Idle\n  note right of Idle\n    Status: ok\n  end note\n  Idle --> Busy", 80).join("\n");
        assert!(!all.contains("│ ok │"), "{all}");
        assert!(all.contains("Idle") && all.contains("Busy"), "{all}");
        // A subgraph's direction stays inside it.
        let td = text(
            "flowchart TD\n  A --> B\n  subgraph S\n    direction LR\n    C --> D\n  end",
            80,
        );
        let row_of = |name: &str| {
            td.iter()
                .position(|l| l.contains(&format!("│ {name} ")))
                .unwrap()
        };
        assert!(
            row_of("A") < row_of("B"),
            "top-down kept:\n{}",
            td.join("\n")
        );
        // Two-way and cross/circle tips: the source, not a one-way arrow.
        assert!(render("flowchart LR\n  A <--> B", false, 80).is_none());
        assert!(render("flowchart LR\n  A --x B", false, 80).is_none());
        assert!(render("flowchart LR\n  A --o B", false, 80).is_none());
        // A labelled self-loop keeps its label.
        let all = text("flowchart LR\n  A -->|retry| A\n  A --> B", 80).join("\n");
        assert!(all.contains("↺ retry"), "{all}");
    }
}
