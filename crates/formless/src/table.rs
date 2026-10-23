//! One table renderer for the CLI's lists (SPEC §12): aligned columns,
//! secondary fields dimmed, colour only on a terminal and never with
//! `NO_COLOR`, and the widest column cut to fit the terminal.

use unicode_width::UnicodeWidthStr;

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
    fn code(self) -> Option<&'static str> {
        Some(match self {
            Tone::Plain => return None,
            Tone::Strong => "1",
            Tone::Dim => "2",
            Tone::Good => "32",
            Tone::Warn => "33",
            Tone::Bad => "31",
        })
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
        print!("{}", self.render(tty && color_allowed(), width));
    }

    /// The table as text: `color` adds ANSI styles, `width` cuts the
    /// widest column (down to 8) so rows fit.
    pub fn render(&self, color: bool, width: Option<usize>) -> String {
        let n = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.width()).collect();
        for r in &self.rows {
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
        let paint = |text: &str, tone: Tone| match (color, tone.code()) {
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
        let header = self
            .headers
            .iter()
            .map(|h| (h.to_string(), Tone::Dim))
            .collect();
        let mut out = line(header);
        for r in &self.rows {
            out.push_str(&line(r.clone()));
        }
        out
    }
}

/// Colour is allowed unless `NO_COLOR` is set to anything (no-color.org).
pub fn color_allowed() -> bool {
    std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

/// `text` cut to `w` columns with an ellipsis.
fn fit(text: &str, w: usize) -> String {
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
        let out = sample().render(false, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "NAME        CALLS  DESCRIPTION");
        assert_eq!(lines[1], "shout       2      Upper-case some text");
        // Narrow: the widest column is cut with an ellipsis.
        let narrow = sample().render(false, Some(40));
        assert!(narrow.lines().all(|l| l.width() <= 40), "{narrow}");
        assert!(narrow.contains('…'));
    }

    #[test]
    fn colour_only_when_asked() {
        assert!(!sample().render(false, None).contains('\x1b'));
        let coloured = sample().render(true, None);
        assert!(
            coloured.contains("\x1b[2mUpper-case some text\x1b[0m"),
            "{coloured}"
        );
    }
}
