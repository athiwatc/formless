//! One table renderer for the CLI's lists (SPEC §12): aligned columns,
//! secondary fields dimmed, colour only on a terminal and never with
//! `NO_COLOR`, and the widest column cut to fit the terminal.

use formless_tui::{colorblind_rgb, tone_marker, SemanticTone};
use std::cell::Cell;
use unicode_width::UnicodeWidthStr;

thread_local! { static COLORBLIND: Cell<bool> = const { Cell::new(false) }; }

/// Ordinary CLI commands set this from their own profile. Read-only discovery
/// paths need no config parse and use the default palette with explicit labels.
pub fn configure_theme(name: &str) {
    COLORBLIND.with(|enabled| enabled.set(name == "colorblind"));
}

/// How a cell is drawn when colour is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Strong,
    Dim,
    Good,
    Warn,
    Bad,
}

impl Tone {
    fn semantic(self) -> Option<SemanticTone> {
        match self {
            Self::Good => Some(SemanticTone::Good),
            Self::Warn => Some(SemanticTone::Warn),
            Self::Bad => Some(SemanticTone::Bad),
            _ => None,
        }
    }
    fn code(self, colorblind: bool) -> Option<String> {
        if let Some(tone) = self.semantic().filter(|_| colorblind) {
            let (r, g, b) = colorblind_rgb(tone);
            return Some(format!("38;2;{r};{g};{b}"));
        }
        Some(
            match self {
                Tone::Plain => return None,
                Tone::Strong => "1",
                Tone::Dim => "2",
                Tone::Good => "32",
                Tone::Warn => "33",
                Tone::Bad => "31",
            }
            .to_string(),
        )
    }
    fn label(self, text: &str, markers: bool) -> String {
        if !text.is_empty() {
            if let Some(tone) = self.semantic().filter(|_| markers) {
                return format!("{} {text}", tone_marker(tone));
            }
        }
        text.to_owned()
    }
}

pub struct Table {
    headers: Vec<&'static str>,
    rows: Vec<Vec<(String, Tone)>>,
}

impl Table {
    pub fn new(headers: &[&'static str]) -> Self {
        Self {
            headers: headers.to_vec(),
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: Vec<(String, Tone)>) {
        self.rows.push(cells);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Print to stdout, coloured and fitted when it's a terminal.
    pub fn print(&self) {
        use std::io::IsTerminal;
        let tty = std::io::stdout().is_terminal();
        let width = tty
            .then(|| crossterm::terminal::size().ok().map(|(w, _)| w as usize))
            .flatten();
        print!("{}", self.render(tty, tty && color_allowed(), width));
    }

    /// Terminal readers get accessible status markers when colour alone is
    /// insufficient. Pipes omit semantic markers, regardless of theme;
    /// both modes normalize cell whitespace and show input controls visibly.
    pub fn render(&self, terminal: bool, color: bool, width: Option<usize>) -> String {
        self.render_theme(terminal, color, width, COLORBLIND.with(Cell::get))
    }

    fn render_theme(
        &self,
        terminal: bool,
        color: bool,
        width: Option<usize>,
        colorblind: bool,
    ) -> String {
        // Normalize before measuring, labeling or clipping. Input cells never
        // contribute terminal escapes; only paint() below emits trusted styles.
        let headers: Vec<_> = self.headers.iter().map(|h| cell_text(h)).collect();
        let rows: Vec<Vec<_>> = self
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|(text, tone)| {
                        (
                            tone.label(&cell_text(text), terminal && (!color || colorblind)),
                            *tone,
                        )
                    })
                    .collect()
            })
            .collect();
        let n = self.headers.len();
        let mut widths: Vec<usize> = headers.iter().map(|h| h.width()).collect();
        for r in &rows {
            for (i, (text, _)) in r.iter().enumerate().take(n) {
                widths[i] = widths[i].max(text.width());
            }
        }
        if let Some(max) = width {
            let gaps = 2 * n.saturating_sub(1);
            while widths.iter().sum::<usize>() + gaps > max {
                let (i, w) = widths
                    .iter()
                    .copied()
                    .enumerate()
                    .max_by_key(|(_, w)| *w)
                    .unwrap();
                if w <= 8 {
                    break;
                }
                widths[i] = w - 1;
            }
        }
        let paint = |text: &str, tone: Tone| match (color, tone.code(colorblind)) {
            (true, Some(code)) if !text.is_empty() => format!("\x1b[{code}m{text}\x1b[0m"),
            _ => text.to_string(),
        };
        let line = |cells: Vec<(String, Tone)>| -> String {
            let last = n - 1;
            let mut out = String::new();
            for (i, w) in widths.iter().enumerate() {
                let (text, tone) = cells
                    .get(i)
                    .cloned()
                    .unwrap_or((String::new(), Tone::Plain));
                let text = fit(&text, *w);
                let pad = if i == last { 0 } else { w - text.width() };
                out.push_str(&paint(&text, tone));
                out.push_str(&" ".repeat(pad));
                if i != last {
                    out.push_str("  ");
                }
            }
            out.trim_end().to_string() + "\n"
        };
        let header = headers.into_iter().map(|h| (h, Tone::Dim)).collect();
        let mut out = line(header);
        for r in &rows {
            out.push_str(&line(r.clone()));
        }
        out
    }
}

/// Controls already made visible in CLI comparison output. Keep the same
/// policy for table cells: C0/C1, direction marks/overrides and isolates.
pub(super) fn terminal_control(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// One visible line, with whitespace collapsed and input controls escaped.
/// Apply this to piped text too: redirected tables may be displayed later.
pub(super) fn cell_text(text: &str) -> String {
    let mut visible = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = !visible.is_empty();
            continue;
        }
        if space {
            visible.push(' ');
            space = false;
        }
        if terminal_control(c) {
            visible.extend(c.escape_unicode());
        } else {
            visible.push(c);
        }
    }
    visible
}

/// Colour is allowed unless `NO_COLOR` is set to anything (no-color.org).
pub fn color_allowed() -> bool {
    std::env::var_os("NO_COLOR").is_none()
}

/// `text` cut to `w` columns with an ellipsis.
pub(super) fn fit(text: &str, w: usize) -> String {
    if text.width() <= w {
        return text.to_string();
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) + 1 > w {
            break;
        }
        out.push(c);
    }
    out + "…"
}

/// A plain cell.
pub fn c(text: impl Into<String>) -> (String, Tone) {
    (text.into(), Tone::Plain)
}

/// A styled cell.
pub fn t(text: impl Into<String>, tone: Tone) -> (String, Tone) {
    (text.into(), tone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piped_status_cells_preserve_the_original_whitespace_fields() {
        let out = statuses().render(false, false, None);
        assert_eq!(
            out.lines()
                .nth(1)
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["ไทย-ok", "100%", "active"]
        );
    }

    fn sample() -> Table {
        let mut tbl = Table::new(&["NAME", "CALLS", "DESCRIPTION"]);
        tbl.row(vec![
            c("shout"),
            c("2"),
            t("Upper-case some text", Tone::Dim),
        ]);
        tbl.row(vec![
            c("fetch_page"),
            c("140"),
            t("Fetch a web page and return its readable text", Tone::Dim),
        ]);
        tbl
    }

    #[test]
    fn columns_align_and_fit_the_terminal() {
        let out = sample().render(false, false, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "NAME        CALLS  DESCRIPTION");
        assert_eq!(lines[1], "shout       2      Upper-case some text");
        // Narrow: the widest column is cut with an ellipsis.
        let narrow = sample().render(true, false, Some(40));
        assert!(narrow.lines().all(|l| l.width() <= 40), "{narrow}");
        assert!(narrow.contains('…'));
    }

    #[test]
    fn colour_only_when_asked() {
        assert!(!sample().render(false, false, None).contains('\x1b'));
        let coloured = sample().render(true, true, None);
        assert!(
            coloured.contains("\x1b[2mUpper-case some text\x1b[0m"),
            "{coloured}"
        );
    }

    #[test]
    fn cells_are_single_line_and_controls_are_visible_before_layout() {
        let mut table = Table::new(&["NAME", "TASK"]);
        table.row(vec![c("first"), c("  a\nb\tc\r\n\x1b[31m\u{202e}  ")]);
        table.row(vec![c("second"), c("ไทย 👩‍💻 e\u{301}  text")]);
        let plain = table.render(false, false, None);
        assert_eq!(plain.lines().count(), 3, "{plain:?}");
        assert!(plain.contains(r"a b c \u{1b}[31m\u{202e}"), "{plain:?}");
        assert!(!plain.contains('\x1b') && !plain.contains('\u{202e}'));
        assert!(plain.contains("ไทย 👩‍💻 e\u{301} text"), "{plain:?}");
        for color in [false, true] {
            let out = table.render(true, color, Some(45));
            let stripped = regex::Regex::new(r"\x1b\[[0-9;]*m")
                .unwrap()
                .replace_all(&out, "")
                .into_owned();
            assert_eq!(stripped.lines().count(), 3, "{out:?}");
            assert!(stripped.lines().all(|line| line.width() <= 45), "{out:?}");
            assert!(
                !out.contains("\x1b[31m"),
                "cell must not inject style: {out:?}"
            );
            assert!(
                !stripped.chars().any(|c| c != '\n' && terminal_control(c)),
                "{stripped:?}"
            );
        }
    }

    #[test]
    fn normalization_preserves_text_and_escapes_all_existing_control_classes() {
        assert_eq!(
            cell_text("\t  alpha\r\n\u{2028}\u{2029} beta\u{a0}\t"),
            "alpha beta"
        );
        assert_eq!(cell_text("\n\t"), "");
        let mut input = String::from("start");
        for c in [
            '\0', '\x08', '\x1b', '\x7f', '\u{80}', '\u{9f}', '\u{61c}', '\u{200e}', '\u{200f}',
            '\u{202a}', '\u{202e}', '\u{2066}', '\u{2069}',
        ] {
            input.push(c);
        }
        let out = cell_text(&input);
        assert!(out.starts_with("start\\u{0}\\u{8}\\u{1b}"), "{out:?}");
        assert!(!out.chars().any(terminal_control), "{out:?}");
        let mut table = Table::new(&[" HEAD\nER ", "EMPTY"]);
        table.row(vec![t("\t\n ", Tone::Good), c("value")]);
        let out = table.render(true, false, None);
        assert_eq!(out.lines().count(), 2);
        assert!(out.starts_with("HEAD ER"));
        assert!(
            !out.contains("ok"),
            "empty normalized cells have no status label"
        );
    }

    fn statuses() -> Table {
        let mut table = Table::new(&["TOOL", "RATE", "STATE"]);
        for (name, rate, state, tone) in [
            ("ไทย-ok", "100%", "active", Tone::Good),
            ("warn", "80%", "off", Tone::Warn),
            ("failed", "20%", "needs attention", Tone::Bad),
        ] {
            table.row(vec![c(name), t(rate, tone), t(state, tone)]);
        }
        table
    }

    #[test]
    fn semantic_labels_survive_no_color_and_use_the_canonical_colorblind_palette() {
        let table = statuses();
        let plain = table.render_theme(true, false, None, false);
        for marker in ["ok 100%", "warn 80%", "fail 20%", "fail needs attention"] {
            assert!(plain.contains(marker), "{plain}");
        }
        assert!(!plain.contains('\x1b'));
        let accessible = table.render_theme(true, true, None, true);
        for tone in [SemanticTone::Good, SemanticTone::Warn, SemanticTone::Bad] {
            let (r, g, b) = colorblind_rgb(tone);
            assert!(accessible.contains(&format!("\x1b[38;2;{r};{g};{b}m")));
        }
        let stripped = regex::Regex::new(r"\x1b\[[0-9;]*m")
            .unwrap()
            .replace_all(&accessible, "");
        assert_eq!(
            stripped, plain,
            "colorblind and no-color contain identical status information/layout"
        );
        let ordinary = table.render_theme(true, true, None, false);
        assert!(ordinary.contains("\x1b[32m100%"));
        assert!(!ordinary.contains("ok 100%"));
    }

    #[test]
    fn marker_widths_are_counted_before_unicode_alignment_and_clipping() {
        let table = statuses();
        for colorblind in [false, true] {
            let out = table.render_theme(true, false, Some(34), colorblind);
            assert!(out.lines().all(|line| line.width() <= 34), "{out}");
            for marker in ["ok ", "warn ", "fail "] {
                assert!(out.contains(marker), "marker clipped: {out}");
            }
            assert!(out.contains("ไทย"), "printable Unicode is retained");
        }
        let mut empty = Table::new(&["STATE"]);
        empty.row(vec![t("", Tone::Good)]);
        assert!(
            !empty.render_theme(true, false, None, false).contains("ok"),
            "missing values are not announced as success"
        );
    }
}
