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
    let header = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    let word = header.split_whitespace().next().unwrap_or("");
    let (kind, lines) = match word {
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
    Some(Diagram { kind, lines })
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
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("%%"))
        .skip(1)
        .flat_map(|l| l.split(';'))
        .map(str::trim)
        .filter(|l| !l.is_empty())
}

fn parse_flowchart(src: &str) -> Option<Graph> {
    let header = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    let mut g = Graph {
        left_right: direction_is_lr(header.split_whitespace().nth(1)),
        ..Default::default()
    };
    for stmt in statements(src) {
        let first = stmt.split_whitespace().next().unwrap_or("");
        if first == "direction" {
            g.left_right = direction_is_lr(stmt.split_whitespace().nth(1));
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
    (!g.nodes.is_empty()).then_some(g)
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
        let Some((label, dash, arrow, visible)) = flow_edge(&mut c) else {
            return;
        };
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
fn flow_edge(c: &mut Cursor) -> Option<(String, Dash, bool, bool)> {
    let mut arrow = c.eat("<");
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
    if tip(c) {
        arrow = true;
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
    Some((label, dash, arrow, !op.contains('~')))
}

fn parse_state(src: &str) -> Option<Graph> {
    let mut g = Graph::default();
    let mut start = None;
    let mut end = None;
    for stmt in statements(src) {
        let stmt = stmt.trim_end_matches('{').trim();
        if stmt.is_empty() || stmt == "}" {
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
    let self_loops: Vec<usize> = g
        .edges
        .iter()
        .filter(|e| e.from == e.to)
        .map(|e| e.from)
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
        if self_loops.contains(&v) {
            if let Some(last) = l.last_mut() {
                last.push_str(" ↺");
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
            "loop" | "alt" | "opt" | "par" | "critical" | "break" | "rect" | "else" | "and" => {
                evs.push(SeqEv::Block(format!("{head} {rest}").trim().to_string()))
            }
            "end" => evs.push(SeqEv::Block(String::new())),
            "activate" | "deactivate" | "box" | "create" | "destroy" | "title" | "links"
            | "link" => {}
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
                let Some(dash_at) = stmt.find('-') else {
                    continue;
                };
                let from = &stmt[..dash_at];
                let r = &stmt[dash_at..];
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
                let Some(&(op, head, dash)) = ops.iter().find(|(op, ..)| r.starts_with(op)) else {
                    continue;
                };
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
    let mut title = String::new();
    let mut slices: Vec<(String, f64)> = Vec::new();
    let header = src
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    if let Some(t) = header.split_once("title").map(|(_, t)| t.trim()) {
        title = t.to_string();
    }
    for stmt in statements(src) {
        if let Some(t) = stmt.strip_prefix("title") {
            title = t.trim().to_string();
        } else if let Some((label, value)) = stmt.rsplit_once(':') {
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
    let bar_w = width.saturating_sub(label_w + 12).clamp(4, 40);
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
    if !title.is_empty() {
        lines.push(Line::styled(title, bold_fg(color, Color::Cyan)));
    }
    for (i, (label, v)) in slices.iter().enumerate() {
        let cells = ((v / max) * bar_w as f64).round() as usize;
        let shown = super::ui::pad(label, label_w);
        lines.push(Line::from(vec![
            Span::raw(format!("{shown} ")),
            Span::styled(
                "█".repeat(cells.max(1)),
                fg(color, PALETTE[i % PALETTE.len()]),
            ),
            Span::styled(format!(" {:.1}%", v / total * 100.0), super::ui::dim(color)),
        ]));
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
}
