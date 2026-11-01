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

/// Encode the native absolute file path, including Windows drive/UNC/verbatim
/// prefixes, as a standards-compliant terminal hyperlink.
pub fn file_url(path: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

/// Parse only numeric suffixes, preserving drive colons and colons in paths.
pub fn path_and_line(word: &str) -> (&str, Option<u32>) {
    let Some((head, last)) = word.rsplit_once(':') else {
        return (word, None);
    };
    let Ok(number) = last.parse::<u32>() else {
        return (word, None);
    };
    if let Some((path, line)) = head.rsplit_once(':') {
        if let Ok(line) = line.parse::<u32>() {
            return (path, Some(line));
        }
    }
    (head, Some(number))
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
        let end = url_end(&chars, start, run_end(&chars, start));
        let url: String = chars[start..end].iter().collect();
        if url.len() > "https://".len() {
            out.push((start, end, url));
        }
        from = end.max(start + 1);
    }
    out
}

/// Path-looking words in a line, as (start char, end char exclusive,
/// path, line): `src/main.rs`, `./a/b.txt:42`, `Cargo.toml`. Whether they
/// exist is checked by the caller.
pub fn find_paths(text: &str) -> Vec<(usize, usize, String, Option<u32>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let boundary = |c: char| c.is_whitespace() || "()[]{}<>\"'`,;│".contains(c);
        if boundary(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !boundary(chars[i]) {
            i += 1;
        }
        let mut word: String = chars[start..i].iter().collect();
        let mut end = i;
        while word.ends_with(['.', ':', '!', '?']) {
            word.pop();
            end -= 1;
        }
        if word.contains("://") || word.starts_with('@') {
            continue;
        }
        // `path:line` or `path:line:col`.
        let (path, line) = path_and_line(&word);
        let path = path.to_string();
        let ext = path.rsplit_once('.').is_some_and(|(stem, e)| {
            !stem.is_empty()
                && (1..=6).contains(&e.len())
                && e.chars().all(|c| c.is_ascii_alphanumeric())
        });
        let looks = (path.contains(['/', '\\']) && path.len() > 1) || ext;
        if looks && !path.chars().all(|c| c.is_ascii_digit() || c == '.') {
            out.push((start, end, path, line));
        }
    }
    out
}

/// Where a URL starting at `start` runs to: up to whitespace or a
/// character that can't be in one.
fn run_end(chars: &[char], start: usize) -> usize {
    let mut end = start;
    while end < chars.len() && !chars[end].is_whitespace() && !"<>\"`│".contains(chars[end]) {
        end += 1;
    }
    end
}

/// The end of the URL in `chars[start..end]` once trailing punctuation
/// that belongs to the sentence is dropped. A closing bracket stays only
/// while it closes one opened inside the URL (#880): `f_(x))` keeps one.
fn url_end(chars: &[char], start: usize, mut end: usize) -> usize {
    while end > start {
        let c = chars[end - 1];
        let opener = match c {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            _ => None,
        };
        if let Some(open) = opener {
            let url = &chars[start..end];
            let opened = url.iter().filter(|&&x| x == open).count();
            let closed = url.iter().filter(|&&x| x == c).count();
            if closed <= opened {
                break;
            }
        } else if !".,;:!?'\"".contains(c) {
            break;
        }
        end -= 1;
    }
    end
}

fn byte_of(chars: &[char], idx: usize) -> usize {
    chars[..idx.min(chars.len())]
        .iter()
        .map(|c| c.len_utf8())
        .sum()
}

/// The links visible in `area` of a drawn buffer: URLs, and paths that
/// `resolve` turns into a `file://` target (it says whether one exists).
pub fn scan(
    buf: &Buffer,
    area: Rect,
    resolve: &mut dyn FnMut(&str) -> Option<String>,
) -> Vec<Painted> {
    // One char per cell; wide chars leave an empty continuation cell.
    let rows: Vec<(u16, Vec<u16>, Vec<char>)> = (area.top()..area.bottom())
        .map(|y| {
            let mut xs = Vec::new();
            let mut text = Vec::new();
            for x in area.left()..area.right() {
                for c in buf[(x, y)].symbol().chars().take(1) {
                    xs.push(x);
                    text.push(c);
                }
            }
            (y, xs, text)
        })
        .collect();
    let first_ink = |t: &[char]| t.iter().position(|c| !c.is_whitespace());
    let last_ink = |t: &[char]| t.iter().rposition(|c| !c.is_whitespace());
    // Char ranges per row already covered by a URL (a fragment of one
    // wrapped from the row above included).
    let mut covered: Vec<Vec<(usize, usize)>> = vec![Vec::new(); rows.len()];
    let mut out = Vec::new();
    for (i, (y, xs, chars)) in rows.iter().enumerate() {
        let text: String = chars.iter().collect();
        for (a, b, url) in find_urls(&text) {
            if covered[i].iter().any(|&(ca, cb)| a < cb && b > ca) {
                continue;
            }
            // A long URL hard-split across rows (#880): it is alone on its
            // row, fills it, and the next row carries on at the same
            // column. Join the pieces into one target for every fragment.
            let mut pieces = vec![(i, a, run_end(chars, a))];
            loop {
                let &(r, pa, pb) = pieces.last().expect("one piece");
                let (_, _, rc) = &rows[r];
                let alone = first_ink(rc) == Some(pa) && last_ink(rc).map(|l| l + 1) == Some(pb);
                let Some((_, _, next)) = rows.get(r + 1) else {
                    break;
                };
                if !alone || pb - pa < 16 || first_ink(next) != Some(a) {
                    break;
                }
                pieces.push((r + 1, a, run_end(next, a)));
            }
            let target = if pieces.len() == 1 {
                url
            } else {
                let joined: Vec<char> = pieces
                    .iter()
                    .flat_map(|&(r, pa, pb)| rows[r].2[pa..pb].iter().copied())
                    .collect();
                let end = url_end(&joined, 0, joined.len());
                joined[..end].iter().collect()
            };
            let mut left = target.chars().count();
            for &(r, pa, pb) in &pieces {
                let n = (pb - pa).min(left);
                if n == 0 {
                    break;
                }
                left -= n;
                let (ry, rxs, _) = &rows[r];
                out.push(Painted {
                    y: *ry,
                    x0: rxs[pa],
                    x1: rxs[pa + n - 1],
                    url: target.clone(),
                });
                covered[r].push((pa, pa + n));
            }
        }
        for (a, b, path, _) in find_paths(&text) {
            if covered[i].iter().any(|&(ua, ub)| a < ub && b > ua) {
                continue;
            }
            if let Some(url) = resolve(&path) {
                out.push(Painted {
                    y: *y,
                    x0: xs[a],
                    x1: xs[b - 1],
                    url,
                });
            }
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
    #[test]
    fn native_file_links_round_trip_reserved_and_unicode_path_names() {
        let dir = std::env::temp_dir().join(format!("fl link {}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("report # % ไทย.txt");
        std::fs::write(&file, b"fixture").unwrap();
        let canonical = file.canonicalize().unwrap();
        let target = super::file_url(&canonical).unwrap();
        assert!(
            target.contains("%20") && target.contains("%23") && target.contains("%25"),
            "{target}"
        );
        let parsed = url::Url::parse(&target).unwrap();
        assert_eq!(parsed.scheme(), "file");
        assert!(parsed.query().is_none() && parsed.fragment().is_none());
        assert_eq!(
            parsed.to_file_path().unwrap().canonicalize().unwrap(),
            canonical
        );
        assert_eq!(
            crate::clipimg::dropped_file(&target)
                .unwrap()
                .canonicalize()
                .unwrap(),
            canonical
        );
        assert!(super::file_url(std::path::Path::new("relative.txt")).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_drive_and_unc_colons_survive_line_suffix_parsing() {
        assert_eq!(
            super::path_and_line(r"C:\work\src\lib.rs:10:4"),
            (r"C:\work\src\lib.rs", Some(10))
        );
        assert_eq!(
            super::path_and_line(r"C:\work\src\lib.rs"),
            (r"C:\work\src\lib.rs", None)
        );
        assert_eq!(
            super::path_and_line(r"\\server\share\file.txt:7"),
            (r"\\server\share\file.txt", Some(7))
        );
        let paths = super::find_paths(r"see C:\work\src\lib.rs:10 and \\server\share\file.txt:7");
        assert_eq!(
            paths
                .iter()
                .map(|(_, _, path, line)| (path.as_str(), *line))
                .collect::<Vec<_>>(),
            vec![
                (r"C:\work\src\lib.rs", Some(10)),
                (r"\\server\share\file.txt", Some(7))
            ]
        );
    }

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
        // Brackets are balanced, not just looked for (#880).
        let paren = find_urls("(see https://example.com/f_(x))");
        assert_eq!(paren[0].2, "https://example.com/f_(x)");
        let square = find_urls("(https://x.dev/a[1])");
        assert_eq!(square[0].2, "https://x.dev/a[1]");
        let wide = find_urls("日本 https://a.jp");
        assert_eq!((wide[0].0, wide[0].2.as_str()), (3, "https://a.jp"));
    }

    #[test]
    fn finds_path_like_words() {
        let got = find_paths("edit src/main.rs:42, then Cargo.toml (see ./a/b.txt). v1.2 and 3.14");
        let paths: Vec<(&str, Option<u32>)> =
            got.iter().map(|(_, _, p, l)| (p.as_str(), *l)).collect();
        assert_eq!(
            paths,
            vec![
                ("src/main.rs", Some(42)),
                ("Cargo.toml", None),
                ("./a/b.txt", None),
                ("v1.2", None)
            ]
        );
        assert_eq!(
            (got[0].0, got[0].1),
            (5, 19),
            "the :line is part of the link"
        );
        assert!(find_paths("https://x.dev/a.html and @user").is_empty());
    }

    /// A URL hard-wrapped across rows links every fragment to the whole
    /// URL; a URL that merely ends a row is left alone (#880).
    #[test]
    fn a_url_wrapped_across_rows_is_one_link() {
        let url = "https://example.com/very/long/path/to/a/page";
        let lines = crate::markdown::render(&format!("see {url} now"), false, "", 24);
        let area = Rect::new(0, 0, 24, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        for (y, l) in lines.iter().enumerate() {
            buf.set_line(0, y as u16, l, 24);
        }
        let links = scan(&buf, area, &mut |_| None);
        assert!(links.len() >= 2, "{links:?}");
        assert!(links.iter().all(|l| l.url == url), "{links:?}");
        let painted: usize = links.iter().map(|l| (l.x1 - l.x0 + 1) as usize).sum();
        assert_eq!(painted, url.len());

        // A short URL that ends a soft-wrapped row is not joined to the
        // words below it.
        let lines =
            crate::markdown::render("read https://x.dev/a then continue reading", false, "", 20);
        let area = Rect::new(0, 0, 20, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        for (y, l) in lines.iter().enumerate() {
            buf.set_line(0, y as u16, l, 20);
        }
        let links = scan(&buf, area, &mut |_| None);
        assert_eq!(
            links.iter().map(|l| l.url.as_str()).collect::<Vec<_>>(),
            ["https://x.dev/a"]
        );
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
        let links = scan(&buf, area, &mut |_| None);
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
