//! One table renderer for the CLI's lists (SPEC §12): aligned columns,
//! secondary fields dimmed, colour only on a terminal and never with
//! `NO_COLOR`, and the widest column cut to fit the terminal. When readable
//! aligned columns no longer fit, labelled records wrap every full field.
//! Zero/one-column requests preserve whole graphemes as best effort.

use formless_tui::{colorblind_rgb, tone_marker, SemanticTone};
use std::cell::Cell;
use unicode_segmentation::UnicodeSegmentation;
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
        if n == 0 {
            return String::new();
        }
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
        if let Some(max) =
            width.filter(|max| widths.iter().sum::<usize>() + 2 * n.saturating_sub(1) > *max)
        {
            // Below the readable aligned minimum, repeat labelled fields and
            // wrap their full values. IDs are never silently shortened here.
            // At zero/one columns, a full wide grapheme may exceed the budget;
            // preserve it as best effort instead of dropping text or looping.
            let max = max.max(1);
            let mut out = String::new();
            let labels = |out: &mut String, header: &str| {
                for line in record_lines(&format!("{header}:"), max) {
                    out.push_str(&paint(&line, Tone::Dim));
                    out.push('\n');
                }
            };
            if rows.is_empty() {
                for header in &headers {
                    labels(&mut out, header);
                }
            }
            for (index, row) in rows.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                for (column, header) in headers.iter().enumerate() {
                    labels(&mut out, header);
                    let (text, tone) = row
                        .get(column)
                        .map(|(text, tone)| (text.as_str(), *tone))
                        .unwrap_or(("", Tone::Plain));
                    let indent = if max >= 4
                        && text
                            .graphemes(true)
                            .all(|cluster| cluster.width() <= max - 2)
                    {
                        "  "
                    } else {
                        ""
                    };
                    for line in record_lines(text, max - indent.len()) {
                        if !line.is_empty() {
                            out.push_str(indent);
                            out.push_str(&paint(&line, tone));
                        }
                        out.push('\n');
                    }
                }
            }
            return out;
        }
        let line = |cells: Vec<(String, Tone)>| -> String {
            let last = n - 1;
            let mut out = String::new();
            for (i, w) in widths.iter().enumerate() {
                let (text, tone) = cells
                    .get(i)
                    .cloned()
                    .unwrap_or((String::new(), Tone::Plain));
                let text = fit(&text, *w);
                let pad = if i == last {
                    0
                } else {
                    w.saturating_sub(text.width())
                };
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

/// Full field contents for the narrow record layout. Measure whole strings
/// so presentation sequences and cross-grapheme ligatures use display width.
fn record_lines(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for cluster in text.graphemes(true) {
        let mut candidate = row.clone();
        candidate.push_str(cluster);
        if !row.is_empty() && candidate.width() > width {
            rows.push(std::mem::take(&mut row));
        }
        row.push_str(cluster);
    }
    rows.push(row);
    rows
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

/// Colour is allowed unless `NO_COLOR` is nonempty (no-color.org).
pub fn color_allowed() -> bool {
    std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
}

/// `text` cut to `w` columns with an ellipsis.
pub(super) fn fit(text: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    let measured_width = text.width();
    if measured_width <= w {
        return text.to_string();
    }
    // Most cells have additive grapheme widths. Measure each cluster once so
    // joined emoji, modifiers and variation selectors keep their string width.
    let mut used = 0;
    let mut end = 0;
    let mut sum = 0;
    for (start, cluster) in text.grapheme_indices(true) {
        let width = cluster.width();
        sum += width;
        if end == start && used + width < w {
            used += width;
            end = start + cluster.len();
        }
    }
    if sum != measured_width {
        // Script ligatures can cross grapheme boundaries (e.g. Arabic Lam-Alef).
        // Retain full-prefix measurement for those exceptional cells. Prefix
        // widths can decrease when a ligature completes, so binary search is
        // not valid here. Ordinary cells still use the linear path above.
        end = 0;
        for (start, cluster) in text.grapheme_indices(true) {
            let next = start + cluster.len();
            if text[..next].width() >= w {
                break;
            }
            end = next;
        }
    }
    format!("{}…", &text[..end])
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
    fn long_mixed_width_cells_clip_within_the_requested_columns() {
        let input = "ascii ไทย 中文 ".repeat(10_000);
        for width in [1, 2, 7, 40, 200] {
            let clipped = fit(&input, width);
            assert!(clipped.ends_with('…'));
            assert!(clipped.width() <= width, "{width}: {clipped:?}");
        }
        assert_eq!(fit("a中ไทยtail", 4), "a中…");
        assert_eq!(fit("ไทย", 10), "ไทย");
    }

    #[test]
    fn clipping_preserves_sequence_aware_widths() {
        for (input, width, expected) in [
            ("👩‍💻abcdefg", 8, "👩‍💻abcde…"),
            ("👩🏽‍💻abcdefg", 8, "👩🏽‍💻abcde…"),
            ("❤\u{fe0f}abcdefg", 8, "❤\u{fe0f}abcde…"),
            ("🇹🇭abcdefg", 8, "🇹🇭abcde…"),
            ("لاabcdefghi", 8, "لاabcdef…"),
            ("e\u{301}abcdefghi", 8, "e\u{301}abcdef…"),
        ] {
            let clipped = fit(input, width);
            assert_eq!(clipped, expected);
            assert_eq!(clipped.width(), width);
        }
    }

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
    fn narrow_sessions_table_uses_full_labelled_records() {
        let mut table = Table::new(&["ID", "UPDATED", "MSGS", "NAME", "LAST MESSAGE"]);
        table.row(vec![
            c("12345678"),
            c("2026-11-14 12:34"),
            c("1234"),
            c("abcdefgh"),
            c("message with full details"),
        ]);
        let out = table.render(true, false, Some(40));
        assert!(out.lines().all(|line| line.width() <= 40), "{out}");
        for text in [
            "ID:",
            "UPDATED:",
            "MSGS:",
            "NAME:",
            "LAST MESSAGE:",
            "12345678",
            "2026-11-14 12:34",
            "message with full details",
        ] {
            assert!(out.contains(text), "missing {text}: {out}");
        }
        assert!(!out.contains('…'), "record values stay complete: {out}");
    }

    #[test]
    fn narrow_records_preserve_unicode_status_and_all_fields() {
        let mut table = Table::new(&["ID", "STATE", "DESCRIPTION"]);
        table.row(vec![
            c("12345678"),
            t("ready", Tone::Good),
            c("❤\u{fe0f} 👩🏽‍💻 e\u{301} لا 中文"),
        ]);
        table.row(vec![
            c("abcdef01"),
            t("failed", Tone::Bad),
            c("second record"),
        ]);
        for width in 0..=27 {
            for color in [false, true] {
                let out = table.render_theme(true, color, Some(width), true);
                let plain = regex::Regex::new(r"\x1b\[[0-9;]*m")
                    .unwrap()
                    .replace_all(&out, "")
                    .into_owned();
                assert!(
                    plain.lines().all(|line| line.width() <= width.max(2)),
                    "width {width}: {plain}"
                );
                for value in [
                    "12345678",
                    "ok ready",
                    "fail failed",
                    "❤\u{fe0f} 👩🏽‍💻 e\u{301} لا 中文",
                    "abcdef01",
                    "second record",
                ] {
                    // Remove only the known indentation and inserted line breaks.
                    let joined = plain
                        .lines()
                        .map(|line| line.strip_prefix("  ").unwrap_or(line))
                        .collect::<String>();
                    assert!(
                        joined.contains(value),
                        "missing {value}, width {width}: {plain}"
                    );
                }
                assert!(!plain.contains('…'));
            }
        }
        assert_eq!(fit("nonempty", 0), "");
        assert_eq!(fit("❤\u{fe0f}tail", 1), "…");
        assert_eq!(Table::new(&[]).render(true, false, Some(0)), "");
        assert!(!Table::new(&["EMPTY"])
            .render(true, false, Some(1))
            .is_empty());
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
