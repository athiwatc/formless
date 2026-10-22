//! Clickable links in the chat (SPEC §10.2): after each draw, URLs in the
//! chat area are repainted wrapped in OSC 8 hyperlinks, so terminals that
//! support them make them clickable. Ratatui's cells can't hold the escape
//! sequences (they'd count toward the width), so the repaint goes straight
//! to the terminal, and only when the visible links change.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use std::io::Write;

/// One visible link: row, first and last column, and its target.
#[derive(Debug, Clone, PartialEq)]
pub struct Painted {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    pub url: String,
}

/// URLs in a line of text, as (start char, end char exclusive, url).
pub fn find_urls(text: &str) -> Vec<(usize, usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let s: String = chars.iter().collect();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = ["https://", "http://"]
        .iter()
        .filter_map(|p| s[byte_of(&chars, from)..].find(p))
        .min()
    {
        let start = from + s[byte_of(&chars, from)..][..rel].chars().count();
        let mut end = start;
        while end < chars.len() && !chars[end].is_whitespace() && !"<>\"`│".contains(chars[end]) {
            end += 1;
        }
        // Trailing punctuation belongs to the sentence, not the URL.
        while end > start && ".,;:!?'\")]}".contains(chars[end - 1]) {
            // Keep a `)` that closes a `(` inside the URL.
            if chars[end - 1] == ')' && chars[start..end].contains(&'(') {
                break;
            }
            end -= 1;
        }
        let url: String = chars[start..end].iter().collect();
        if url.len() > "https://".len() {
            out.push((start, end, url));
        }
        from = end.max(start + 1);
    }
    out
}

fn byte_of(chars: &[char], idx: usize) -> usize {
    chars[..idx.min(chars.len())]
        .iter()
        .map(|c| c.len_utf8())
        .sum()
}

/// The links visible in `area` of a drawn buffer.
pub fn scan(buf: &Buffer, area: Rect) -> Vec<Painted> {
    let mut out = Vec::new();
    for y in area.top()..area.bottom() {
        // One char per cell; wide chars leave an empty continuation cell.
        let mut xs = Vec::new();
        let mut text = String::new();
        for x in area.left()..area.right() {
            let sym = buf[(x, y)].symbol();
            for c in sym.chars().take(1) {
                xs.push(x);
                text.push(c);
            }
        }
        for (a, b, url) in find_urls(&text) {
            out.push(Painted {
                y,
                x0: xs[a],
                x1: xs[b - 1],
                url,
            });
        }
    }
    out
}

/// Repaint `links` with OSC 8 around their cells, keeping each cell's
/// colours and modifiers.
pub fn paint(out: &mut impl Write, buf: &Buffer, links: &[Painted]) -> std::io::Result<()> {
    use crossterm::style::{
        Attribute, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    };
    use crossterm::{cursor, QueueableCommand};
    if links.is_empty() {
        return Ok(());
    }
    out.queue(cursor::SavePosition)?;
    for l in links {
        out.queue(cursor::MoveTo(l.x0, l.y))?;
        out.queue(Print(format!("\x1b]8;;{}\x1b\\", l.url)))?;
        for x in l.x0..=l.x1 {
            let cell = &buf[(x, l.y)];
            out.queue(SetAttribute(Attribute::Reset))?;
            out.queue(SetForegroundColor(to_ct(cell.fg)))?;
            out.queue(SetBackgroundColor(to_ct(cell.bg)))?;
            if cell.modifier.contains(Modifier::BOLD) {
                out.queue(SetAttribute(Attribute::Bold))?;
            }
            if cell.modifier.contains(Modifier::DIM) {
                out.queue(SetAttribute(Attribute::Dim))?;
            }
            if cell.modifier.contains(Modifier::ITALIC) {
                out.queue(SetAttribute(Attribute::Italic))?;
            }
            if cell.modifier.contains(Modifier::UNDERLINED) {
                out.queue(SetAttribute(Attribute::Underlined))?;
            }
            if cell.modifier.contains(Modifier::REVERSED) {
                out.queue(SetAttribute(Attribute::Reverse))?;
            }
            out.queue(Print(cell.symbol()))?;
        }
        out.queue(Print("\x1b]8;;\x1b\\"))?;
    }
    out.queue(SetAttribute(Attribute::Reset))?;
    out.queue(ResetColor)?;
    out.queue(cursor::RestorePosition)?;
    out.flush()
}

fn to_ct(c: Color) -> crossterm::style::Color {
    use crossterm::style::Color as C;
    match c {
        Color::Reset => C::Reset,
        Color::Black => C::Black,
        Color::Red => C::DarkRed,
        Color::Green => C::DarkGreen,
        Color::Yellow => C::DarkYellow,
        Color::Blue => C::DarkBlue,
        Color::Magenta => C::DarkMagenta,
        Color::Cyan => C::DarkCyan,
        Color::Gray => C::Grey,
        Color::DarkGray => C::DarkGrey,
        Color::LightRed => C::Red,
        Color::LightGreen => C::Green,
        Color::LightYellow => C::Yellow,
        Color::LightBlue => C::Blue,
        Color::LightMagenta => C::Magenta,
        Color::LightCyan => C::Cyan,
        Color::White => C::White,
        Color::Rgb(r, g, b) => C::Rgb { r, g, b },
        Color::Indexed(i) => C::AnsiValue(i),
    }
}

/// Whether to emit OSC 8: `tui.hyperlinks` is "on", "off" or "auto"
/// (on, except terminals known to show the sequences).
pub fn enabled(setting: &str) -> bool {
    match setting {
        "on" => true,
        "off" => false,
        _ => {
            let term = std::env::var("TERM").unwrap_or_default();
            std::env::var("TERM_PROGRAM").ok().as_deref() != Some("Apple_Terminal")
                && term != "linux"
                && term != "dumb"
        }
    }
}

/// Open a URL or file with the system's opener.
pub fn open_external(target: &str) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_urls_without_trailing_punctuation() {
        let got = find_urls("see https://example.com/a?b=1, and (http://x.dev/p).");
        let urls: Vec<&str> = got.iter().map(|(_, _, u)| u.as_str()).collect();
        assert_eq!(urls, vec!["https://example.com/a?b=1", "http://x.dev/p"]);
        assert_eq!((got[0].0, got[0].1), (4, 29));
        let wiki = find_urls("https://en.wikipedia.org/wiki/Rust_(language) ok");
        assert_eq!(wiki[0].2, "https://en.wikipedia.org/wiki/Rust_(language)");
        assert!(find_urls("no links here, just https:// alone").is_empty());
        let wide = find_urls("日本 https://a.jp");
        assert_eq!((wide[0].0, wide[0].2.as_str()), (3, "https://a.jp"));
    }

    #[test]
    fn scans_a_buffer_and_paints_osc8() {
        let area = Rect::new(0, 0, 40, 2);
        let mut buf = Buffer::empty(area);
        buf.set_string(
            2,
            1,
            "go to https://x.dev now",
            ratatui::style::Style::default(),
        );
        let links = scan(&buf, area);
        assert_eq!(
            links,
            vec![Painted {
                y: 1,
                x0: 8,
                x1: 20,
                url: "https://x.dev".into()
            }]
        );
        let mut out = Vec::new();
        paint(&mut out, &buf, &links).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\x1b]8;;https://x.dev\x1b\\"), "{s:?}");
        assert!(s.ends_with("\x1b8") || s.contains("\x1b]8;;\x1b\\"));
    }
}
