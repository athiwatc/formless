//! Clickable links in the chat (SPEC §10.2): after each draw, URLs in the
//! chat area are repainted wrapped in OSC 8 hyperlinks, so terminals that
//! support them make them clickable. Ratatui's cells can't hold the escape
//! sequences (they'd count toward the width), so the repaint goes straight
//! to the terminal when the visible links or their rendered cells change.
//! Cells leaving a link are repainted with no hyperlink active.

use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use std::collections::{BTreeSet, HashSet};
use std::io::Write;
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

const CLOSE_LINK: &str = "\x1b]8;;\x1b\\";
// Terminate a partial control string before closing the current hyperlink.
const RECOVER_LINK: &str = "\x1b\\\x1b]8;;\x1b\\";

/// One visible link with inclusive leading-cell bounds and its target.
#[derive(Debug, Clone, PartialEq)]
pub struct Painted {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    pub url: String,
}

#[derive(Default)]
pub struct PaintCache {
    area: Rect,
    links: Vec<Painted>,
    cells: Vec<Cell>,
    covered: HashSet<(u16, u16)>,
    retry: bool,
}

impl PaintCache {
    pub fn clear(&mut self) {
        self.links.clear();
        self.cells.clear();
        self.covered.clear();
        self.retry = false;
    }

    pub fn recover_terminal(&self, out: &mut impl Write) -> std::io::Result<()> {
        if self.retry {
            use crossterm::style::{Attribute, Print, ResetColor, SetAttribute};
            use crossterm::QueueableCommand;
            // A previous write error can also block best-effort closure.
            out.queue(Print(RECOVER_LINK))?;
            out.queue(SetAttribute(Attribute::Reset))?;
            out.queue(ResetColor)?;
            out.flush()?;
        }
        Ok(())
    }

    pub fn update(
        &mut self,
        out: &mut impl Write,
        buf: &Buffer,
        links: Vec<Painted>,
    ) -> std::io::Result<()> {
        if self.area != buf.area {
            // Ratatui clears the terminal before drawing into a resized buffer.
            self.clear();
            self.area = buf.area;
        }
        let cells = links
            .iter()
            .flat_map(|link| (link.x0..=link.x1).map(move |x| &buf[(x, link.y)]));
        if !self.retry && self.links == links && self.cells.iter().eq(cells.clone()) {
            return Ok(());
        }
        let covered = links
            .iter()
            .flat_map(|link| {
                link_cells(buf, link).flat_map(move |(cols, _)| cols.map(move |x| (x, link.y)))
            })
            .collect::<HashSet<_>>();
        let removed = self
            .covered
            .difference(&covered)
            .copied()
            .collect::<HashSet<_>>();
        let rows = removed.iter().map(|&(_, y)| y).collect::<BTreeSet<_>>();
        let mut plain = Vec::new();
        for y in rows {
            // A replacement glyph can begin before the old span and still
            // cover it. Repaint the entire glyph from its leading column.
            for (cols, _) in row_cells(buf, y, buf.area.left()..buf.area.right()) {
                if cols.clone().any(|x| removed.contains(&(x, y))) {
                    plain.push((cols.start, y));
                }
            }
        }
        if let Err(error) = paint(out, buf, &links, &plain) {
            // Writes can fail partway through either pass. Retry and account
            // for every cell that could still be linked on the terminal.
            self.covered.extend(covered);
            self.retry = true;
            return Err(error);
        }
        self.retry = false;
        self.covered = covered;
        self.cells.clear();
        self.cells.extend(cells.cloned());
        self.links = links;
        Ok(())
    }
}

fn row_cells(
    buf: &Buffer,
    y: u16,
    columns: Range<u16>,
) -> impl Iterator<Item = (Range<u16>, &Cell)> {
    let mut next = columns.start;
    columns.filter_map(move |x| {
        if x < next {
            return None;
        }
        let cell = &buf[(x, y)];
        let width = u16::try_from(cell.symbol().width())
            .unwrap_or(u16::MAX)
            .max(1);
        next = x.saturating_add(width).min(buf.area.right());
        (!cell.skip).then_some((x..next, cell))
    })
}

fn link_cells<'a>(buf: &'a Buffer, link: &Painted) -> impl Iterator<Item = (Range<u16>, &'a Cell)> {
    row_cells(buf, link.y, link.x0..link.x1.saturating_add(1))
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
    let search = s.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some((rel, prefix_len)) = ["https://", "http://"]
        .iter()
        .filter_map(|p| {
            search[byte_of(&chars, from)..]
                .find(p)
                .map(|rel| (rel, p.len()))
        })
        .min_by_key(|(rel, _)| *rel)
    {
        let start = from + s[byte_of(&chars, from)..][..rel].chars().count();
        let end = url_end(&chars, start, run_end(&chars, start));
        let url: String = chars[start..end].iter().collect();
        if url.len() > prefix_len {
            out.push((start, end, http_target(url)));
        }
        from = end.max(start + 1);
    }
    out
}

/// Normalize only the HTTP(S) scheme for terminal targets; visible text and
/// the case-sensitive path, query, fragment and escapes stay unchanged.
fn http_target(mut url: String) -> String {
    if let Some(colon) = url.find(':') {
        url[..colon].make_ascii_lowercase();
    }
    url
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
    let rows: Vec<(u16, Vec<u16>, Vec<char>)> = (area.top()..area.bottom())
        .map(|y| {
            let mut xs = Vec::new();
            let mut text = Vec::new();
            // Walk from the buffer's left edge so a glyph crossing into the
            // chat area can never contribute a stray continuation space.
            for (cols, cell) in row_cells(buf, y, buf.area.left()..area.right()) {
                if cols.start < area.left() || cols.end > area.right() {
                    continue;
                }
                for c in cell.symbol().chars() {
                    xs.push(cols.start);
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
                let (ry, rxs, rc) = &rows[r];
                let alone = first_ink(rc) == Some(pa) && last_ink(rc).map(|l| l + 1) == Some(pb);
                if !alone {
                    break;
                }
                let last_x = rxs[pb - 1];
                let width =
                    usize::from(last_x - rxs[pa]) + buf[(last_x, *ry)].symbol().width().max(1);
                let Some((_, next_xs, next)) = rows.get(r + 1) else {
                    break;
                };
                let Some(next_start) = first_ink(next) else {
                    break;
                };
                if width < 16 || next_xs[next_start] != xs[a] {
                    break;
                }
                pieces.push((r + 1, next_start, run_end(next, next_start)));
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
            let target = http_target(target);
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

fn paint_cell(out: &mut impl Write, cell: &Cell) -> std::io::Result<()> {
    use crossterm::style::{
        Attribute, Print, SetAttribute, SetBackgroundColor, SetForegroundColor, SetUnderlineColor,
    };
    use crossterm::QueueableCommand;
    const ATTRIBUTES: [(Modifier, Attribute); 9] = [
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::DIM, Attribute::Dim),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::REVERSED, Attribute::Reverse),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
        (Modifier::HIDDEN, Attribute::Hidden),
        (Modifier::SLOW_BLINK, Attribute::SlowBlink),
        (Modifier::RAPID_BLINK, Attribute::RapidBlink),
    ];
    out.queue(SetAttribute(Attribute::Reset))?;
    out.queue(SetForegroundColor(cell.fg.into()))?;
    out.queue(SetBackgroundColor(cell.bg.into()))?;
    if cell.underline_color != Color::Reset {
        out.queue(SetUnderlineColor(cell.underline_color.into()))?;
    }
    for (modifier, attribute) in ATTRIBUTES {
        if cell.modifier.contains(modifier) {
            out.queue(SetAttribute(attribute))?;
        }
    }
    out.queue(Print(cell.symbol()))?;
    Ok(())
}

/// Restore the current glyphs at expired positions before drawing the links.
fn paint(
    out: &mut impl Write,
    buf: &Buffer,
    links: &[Painted],
    plain: &[(u16, u16)],
) -> std::io::Result<()> {
    use crossterm::style::{Attribute, Print, ResetColor, SetAttribute};
    use crossterm::{cursor, QueueableCommand};
    if links.is_empty() && plain.is_empty() {
        return Ok(());
    }
    out.queue(cursor::SavePosition)?;
    let result = (|| {
        if !plain.is_empty() {
            out.queue(Print(CLOSE_LINK))?;
            for &(x, y) in plain {
                out.queue(cursor::MoveTo(x, y))?;
                paint_cell(out, &buf[(x, y)])?;
            }
        }
        for link in links {
            out.queue(cursor::MoveTo(link.x0, link.y))?;
            out.queue(Print(format!("\x1b]8;;{}\x1b\\", link.url)))?;
            let mut x = link.x0;
            for (cols, cell) in link_cells(buf, link) {
                if x != cols.start {
                    out.queue(cursor::MoveTo(cols.start, link.y))?;
                }
                paint_cell(out, cell)?;
                x = cols.end;
            }
            out.queue(Print(CLOSE_LINK))?;
        }
        out.queue(SetAttribute(Attribute::Reset))?;
        out.queue(ResetColor)?;
        out.queue(cursor::RestorePosition)?;
        out.flush()
    })();
    if result.is_err() {
        // Best effort before the next frame writes unrelated cells.
        let _ = out.queue(Print(RECOVER_LINK));
        let _ = out.queue(SetAttribute(Attribute::Reset));
        let _ = out.queue(ResetColor);
        let _ = out.queue(cursor::RestorePosition);
        let _ = out.flush();
    }
    result
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
    fn urls_use_the_matched_scheme_minimum() {
        for url in [
            "http://a",
            "http://a/",
            "https://a",
            "https://a/",
            "http://é",
        ] {
            let text = format!("日本 ({url}). next");
            assert_eq!(
                find_urls(&text),
                vec![(4, 4 + url.chars().count(), url.into())]
            );
        }
        for bare in ["http://", "https://", "http://.", "https://."] {
            assert!(find_urls(bare).is_empty(), "{bare}");
        }
        assert_eq!(
            find_urls("http://a https://b"),
            vec![(0, 8, "http://a".into()), (9, 18, "https://b".into())]
        );
    }

    #[test]
    fn mixed_case_schemes_keep_text_offsets_and_target_remainders() {
        let text = "日本 (HTTPS://Case.Example/A%2Fb?Token=AbC#Frag), HTTP://a HtTp://é.";
        let got = find_urls(text);
        let targets: Vec<_> = got.iter().map(|(_, _, url)| url.as_str()).collect();
        assert_eq!(
            targets,
            [
                "https://Case.Example/A%2Fb?Token=AbC#Frag",
                "http://a",
                "http://é"
            ]
        );
        for ((start, end, _), original) in got.iter().zip([
            "HTTPS://Case.Example/A%2Fb?Token=AbC#Frag",
            "HTTP://a",
            "HtTp://é",
        ]) {
            assert_eq!(
                text.chars()
                    .skip(*start)
                    .take(end - start)
                    .collect::<String>(),
                original
            );
        }
        for bare in [
            "HTTP://",
            "hTtPs://.",
            "FTP://example.test",
            "https:example.test",
        ] {
            assert!(find_urls(bare).is_empty(), "{bare}");
        }
        assert!(find_paths(text).is_empty());
    }

    #[test]
    fn mixed_case_wrapped_links_paint_original_text_with_normalized_targets() {
        let original = "HtTpS://Case.Example/Long/Path%2FCase?Token=AbC#Frag";
        let target = "https://Case.Example/Long/Path%2FCase?Token=AbC#Frag";
        let lines = crate::markdown::render(original, false, "", 24);
        let area = Rect::new(0, 0, 24, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        for (y, line) in lines.iter().enumerate() {
            buf.set_line(0, y as u16, line, 24);
        }
        let links = scan(&buf, area, &mut |_| panic!("URL is not a file path"));
        assert!(links.len() >= 2, "{links:?}");
        assert!(links.iter().all(|link| link.url == target), "{links:?}");
        assert_eq!(
            links
                .iter()
                .map(|link| (link.x1 - link.x0 + 1) as usize)
                .sum::<usize>(),
            original.len()
        );
        let mut out = Vec::new();
        paint(&mut out, &buf, &links, &[]).unwrap();
        let painted = String::from_utf8(out).unwrap();
        assert!(
            painted.contains(&format!("\x1b]8;;{target}\x1b\\")),
            "{painted:?}"
        );
        let visible: String = links
            .iter()
            .flat_map(|link| (link.x0..=link.x1).map(|x| buf[(x, link.y)].symbol()))
            .collect();
        assert_eq!(visible, original);
    }

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
        paint(&mut out, &buf, &links, &[]).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("\x1b]8;;https://x.dev\x1b\\"), "{s:?}");
        assert!(s.ends_with("\x1b8") || s.contains("\x1b]8;;\x1b\\"));
    }

    #[test]
    fn scan_and_paint_preserve_wide_url_glyphs() {
        let area = Rect::new(0, 0, 42, 1);
        let mut buf = Buffer::empty(area);
        let url = "https://ja.wikipedia.org/wiki/日本";
        buf.set_string(
            0,
            0,
            format!("see {url} now"),
            ratatui::style::Style::default(),
        );

        let links = scan(&buf, area, &mut |_| None);
        let x1 = 4 + url.width() as u16 - 2;
        assert_eq!(buf[(x1, 0)].symbol(), "本");
        assert_eq!(
            links,
            [Painted {
                y: 0,
                x0: 4,
                x1,
                url: url.to_owned(),
            }]
        );

        let mut out = Vec::new();
        paint(&mut out, &buf, &links, &[]).unwrap();
        let painted = String::from_utf8(out).unwrap();
        let (_, after_target) = painted.split_once(&format!("\x1b]8;;{url}\x1b\\")).unwrap();
        let (visible, _) = after_target.split_once(CLOSE_LINK).unwrap();
        assert_eq!(visible.matches('日').count(), 1);
        assert_eq!(visible.matches('本').count(), 1);
        assert!(visible.find('日').unwrap() < visible.find('本').unwrap());
        assert!(!visible.contains(' '));
    }

    #[test]
    fn scan_resolves_a_wide_unicode_file_path_with_a_line_suffix() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let name = "設計📄.md";
        let file = dir.join(name);
        std::fs::write(&file, b"fixture").unwrap();
        let canonical = file.canonicalize().unwrap();
        let area = Rect::new(0, 0, 32, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(
            0,
            0,
            format!("see {name}:42"),
            ratatui::style::Style::default(),
        );

        let links = scan(&buf, area, &mut |word| {
            let path = dir.join(word).canonicalize().ok()?;
            file_url(&path)
        });
        let resolved = links.first().and_then(|link| {
            let target = url::Url::parse(&link.url).ok()?;
            target.to_file_path().ok()?.canonicalize().ok()
        });
        std::fs::remove_file(file).unwrap();
        std::fs::remove_dir(dir).unwrap();

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].x0, 4);
        assert_eq!(links[0].x1, 4 + name.width() as u16 + 2);
        assert_eq!(resolved, Some(canonical));
    }

    #[test]
    fn wrapped_urls_use_terminal_columns_and_keep_complete_graphemes() {
        let area = Rect::new(2, 1, 20, 2);
        let mut buf = Buffer::empty(area);
        let first = "https://x/日本語";
        let second = "文e\u{301}❤️👩‍💻";
        let x0 = 6;
        assert_eq!(first.width(), 16);
        buf.set_string(x0, 1, first, ratatui::style::Style::default());
        buf.set_string(x0, 2, second, ratatui::style::Style::default());
        let target = format!("{first}{second}");

        let links = scan(&buf, area, &mut |_| None);
        assert_eq!(
            links,
            [
                Painted {
                    y: 1,
                    x0,
                    x1: area.right() - 2,
                    url: target.clone(),
                },
                Painted {
                    y: 2,
                    x0,
                    x1: x0 + "文e\u{301}❤️".width() as u16,
                    url: target,
                },
            ]
        );
        for (link, text) in links.iter().zip([first, second]) {
            let visible: String = link_cells(&buf, link)
                .map(|(_, cell)| cell.symbol())
                .collect();
            assert_eq!(visible, text);
        }
    }

    #[test]
    fn link_repaints_when_find_restylings_leave_it_in_place() {
        use ratatui::{style::Style, text::Line};

        let area = Rect::new(0, 0, 36, 1);
        let mut buf = Buffer::empty(area);
        let source = Line::raw("https://example.test/repo");
        buf.set_line(0, 0, &source, area.width);
        let found = scan(&buf, area, &mut |_| None);
        assert_eq!(found.len(), 1);
        let mut cache = PaintCache::default();
        let mut out = Vec::new();
        cache.update(&mut out, &buf, found.clone()).unwrap();
        assert!(!out.is_empty());

        out.clear();
        buf[(35, 0)].set_fg(Color::Yellow);
        cache.update(&mut out, &buf, found.clone()).unwrap();
        assert!(
            out.is_empty(),
            "a cell outside the link cannot invalidate it"
        );

        let (highlighted, count) = crate::highlight_matches(source.clone(), "repo", |_| {
            Style::default().bg(Color::Yellow)
        });
        assert_eq!(count, 1);
        buf.set_line(0, 0, &highlighted, area.width);
        assert_eq!(scan(&buf, area, &mut |_| None), found);
        cache.update(&mut out, &buf, found.clone()).unwrap();
        let painted = std::str::from_utf8(&out).unwrap();
        assert!(painted.contains("\x1b]8;;https://example.test/repo\x1b\\"));
        assert_eq!(painted.contains("\x1b[48;5;3m"), crate::colors_enabled());

        out.clear();
        buf.reset();
        buf.set_line(0, 0, &source, area.width);
        cache.update(&mut out, &buf, found.clone()).unwrap();
        assert!(
            !out.is_empty(),
            "dismissing find restores the unstyled link"
        );

        out.clear();
        cache.clear();
        cache.update(&mut out, &buf, found.clone()).unwrap();
        assert!(!out.is_empty(), "full redraw invalidates painted links");

        out.clear();
        buf.resize(Rect::new(0, 0, 38, 1));
        cache.update(&mut out, &buf, found).unwrap();
        assert!(!out.is_empty(), "resize invalidates painted links");
    }

    #[test]
    fn paint_preserves_strike_hidden_blink_and_underline_color() {
        use ratatui::style::Style;

        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "https://x.test", Style::default());
        buf[(0, 0)].set_style(
            Style::default()
                .underline_color(Color::Indexed(37))
                .add_modifier(
                    Modifier::UNDERLINED
                        | Modifier::CROSSED_OUT
                        | Modifier::HIDDEN
                        | Modifier::SLOW_BLINK,
                ),
        );
        buf[(1, 0)].set_style(Style::default().add_modifier(Modifier::RAPID_BLINK));
        let found = scan(&buf, area, &mut |_| None);
        let mut out = Vec::new();
        paint(&mut out, &buf, &found, &[]).unwrap();
        let painted = std::str::from_utf8(&out).unwrap();
        for attribute in ["\x1b[4m", "\x1b[9m", "\x1b[8m", "\x1b[5m", "\x1b[6m"] {
            assert!(
                painted.contains(attribute),
                "missing {attribute:?} in {painted:?}"
            );
        }
        assert_eq!(painted.contains("\x1b[58;5;37m"), crate::colors_enabled());
    }

    #[test]
    fn repaints_unchanged_suffix_after_the_url_prefix_is_removed() {
        use ratatui::style::Style;

        let area = Rect::new(0, 0, 36, 2);
        let text = "https://example.test/repo";
        let mut before = Buffer::empty(area);
        before.set_string(0, 1, text, Style::default());
        let mut cache = PaintCache::default();
        let mut out = Vec::new();
        cache
            .update(&mut out, &before, scan(&before, area, &mut |_| None))
            .unwrap();

        let mut after = before.clone();
        after.set_string(0, 1, "--------", Style::default());
        assert!(before.diff(&after).iter().all(|(x, _, _)| *x < 8));
        let found = scan(&after, area, &mut |_| None);
        assert!(found.is_empty());
        out.clear();
        cache.update(&mut out, &after, found).unwrap();
        let painted = std::str::from_utf8(&out).unwrap();
        assert!(painted.starts_with("\x1b7\x1b]8;;\x1b\\"));
        assert!(painted.contains(&format!("\x1b[2;{}H", text.len())));
        assert!(!painted.contains("\x1b]8;;https://"));
        assert!(!painted.contains(&format!("\x1b[2;{}H", text.len() + 1)));
        assert!(!painted.contains("\x1b[1;"), "unrelated row was repainted");
    }

    #[test]
    fn disabling_repaints_present_links_plain_and_reenable_restores_them() {
        use ratatui::style::Style;

        let area = Rect::new(0, 0, 30, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(
            0,
            0,
            "https://example.test",
            Style::default().add_modifier(Modifier::CROSSED_OUT),
        );
        let found = scan(&buf, area, &mut |_| None);
        assert_eq!(found.len(), 1);
        let mut cache = PaintCache::default();
        let mut out = Vec::new();
        cache.update(&mut out, &buf, found.clone()).unwrap();

        out.clear();
        cache.update(&mut out, &buf, Vec::new()).unwrap();
        let painted = std::str::from_utf8(&out).unwrap();
        assert!(painted.starts_with("\x1b7\x1b]8;;\x1b\\"));
        assert!(
            painted.contains("\x1b[9mh"),
            "plain repaint must preserve the style"
        );
        assert!(!painted.contains("\x1b]8;;https://"));

        out.clear();
        cache.update(&mut out, &buf, Vec::new()).unwrap();
        assert!(out.is_empty());
        cache.update(&mut out, &buf, found).unwrap();
        assert!(std::str::from_utf8(&out)
            .unwrap()
            .contains("\x1b]8;;https://example.test\x1b\\"));
    }

    #[test]
    fn removed_span_covers_old_and_new_wide_glyphs_at_its_edges() {
        use ratatui::style::Style;

        let area = Rect::new(0, 0, 9, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(2, 0, "x界", Style::default());
        let mut cache = PaintCache::default();
        let mut out = Vec::new();
        // The end of the scanned link is the leading column of 界.
        let found = vec![Painted {
            y: 0,
            x0: 2,
            x1: 3,
            url: "https://example.test".into(),
        }];
        cache.update(&mut out, &buf, found).unwrap();

        buf.reset();
        buf.set_string(1, 0, "界xy", Style::default());
        out.clear();
        cache.update(&mut out, &buf, Vec::new()).unwrap();
        let painted = std::str::from_utf8(&out).unwrap();
        assert!(painted.contains("\x1b[1;2H") && painted.contains('界'));
        assert!(
            !painted.contains("\x1b[1;3H"),
            "do not overwrite the wide continuation"
        );
        assert!(
            painted.contains("\x1b[1;5H") && painted.contains('y'),
            "clear the old wide tail"
        );
    }

    #[test]
    fn moving_a_link_repaints_only_the_exposed_columns_plain() {
        let area = Rect::new(0, 0, 8, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "abcdefgh", ratatui::style::Style::default());
        let mut cache = PaintCache::default();
        let mut out = Vec::new();
        let mut link = Painted {
            y: 0,
            x0: 1,
            x1: 3,
            url: "https://old.test".into(),
        };
        cache.update(&mut out, &buf, vec![link.clone()]).unwrap();
        out.clear();
        link.x0 = 2;
        link.x1 = 4;
        link.url = "https://new.test".into();
        cache.update(&mut out, &buf, vec![link]).unwrap();
        let painted = std::str::from_utf8(&out).unwrap();
        let (plain, linked) = painted
            .split_once("\x1b[1;3H\x1b]8;;https://new.test\x1b\\")
            .unwrap();
        assert!(plain.starts_with("\x1b7\x1b]8;;\x1b\\\x1b[1;2H"));
        assert_eq!(plain.matches('H').count(), 1);
        assert!(plain.ends_with('b') && linked.contains('e'));
    }

    struct FailAfter {
        bytes: Vec<u8>,
        suffix: &'static [u8],
        failures_left: usize,
    }

    impl FailAfter {
        fn new(suffix: &'static [u8]) -> Self {
            Self {
                bytes: Vec::new(),
                suffix,
                failures_left: 1,
            }
        }
    }

    impl Write for FailAfter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.failures_left > 0 && self.bytes.ends_with(self.suffix) {
                self.failures_left -= 1;
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.bytes.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn interrupted_paint_closes_the_link_and_can_be_retried() {
        const OPEN: &[u8] = b"\x1b]8;;https://x.test\x1b\\";
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "https://x.test", ratatui::style::Style::default());
        let found = scan(&buf, area, &mut |_| None);
        let mut cache = PaintCache::default();
        let mut out = FailAfter::new(OPEN);
        let error = cache.update(&mut out, &buf, found.clone()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        assert!(out.bytes.ends_with(b"\x1b]8;;\x1b\\\x1b[0m\x1b[0m\x1b8"));

        out.bytes.clear();
        cache.update(&mut out, &buf, found).unwrap();
        assert!(out.bytes.windows(OPEN.len()).any(|bytes| bytes == OPEN));
    }

    #[test]
    fn interrupted_writes_preserve_cleanup_even_if_the_link_is_gone() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "https://x.test", ratatui::style::Style::default());
        let found = scan(&buf, area, &mut |_| None);
        let mut cache = PaintCache::default();

        let mut partial_link = FailAfter::new(b"h");
        assert!(cache.update(&mut partial_link, &buf, found).is_err());
        let mut partial_cleanup = FailAfter::new(b"\x1b[1;1H");
        assert!(cache
            .update(&mut partial_cleanup, &buf, Vec::new())
            .is_err());

        let mut out = Vec::new();
        cache.update(&mut out, &buf, Vec::new()).unwrap();
        let painted = std::str::from_utf8(&out).unwrap();
        assert!(painted.starts_with("\x1b7\x1b]8;;\x1b\\\x1b[1;1H"));
        assert!(!painted.contains("\x1b]8;;https://"));
        out.clear();
        cache.update(&mut out, &buf, Vec::new()).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn recovers_before_any_new_frame_when_the_emergency_close_also_fails() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "https://x.test", ratatui::style::Style::default());
        let found = scan(&buf, area, &mut |_| None);
        let mut cache = PaintCache::default();
        let mut out = FailAfter::new(b"h");
        out.failures_left = 2;
        assert!(cache.update(&mut out, &buf, found).is_err());
        assert_eq!(out.failures_left, 0);
        assert!(!std::str::from_utf8(&out.bytes)
            .unwrap()
            .contains(CLOSE_LINK));

        let mut recovering = FailAfter::new(b"");
        assert!(cache.recover_terminal(&mut recovering).is_err());
        assert!(recovering.bytes.is_empty());
        cache.recover_terminal(&mut recovering).unwrap();
        assert!(recovering.bytes.starts_with(RECOVER_LINK.as_bytes()));

        let mut plain = Vec::new();
        cache.update(&mut plain, &buf, Vec::new()).unwrap();
        assert!(plain.starts_with(b"\x1b7\x1b]8;;\x1b\\"));
        plain.clear();
        cache.recover_terminal(&mut plain).unwrap();
        assert!(plain.is_empty());
    }
}
