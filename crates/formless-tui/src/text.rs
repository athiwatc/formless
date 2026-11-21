//! Text measured the way the terminal draws it: by grapheme cluster (what
//! a reader sees as one character, UAX #29) and display columns, never by
//! `char` (#784–#786).

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Columns `s` takes on screen.
pub(crate) fn width(s: &str) -> usize {
    s.width()
}

/// The byte offset of the cluster boundary before `i` (0 at the start).
pub(crate) fn prev_cluster(s: &str, i: usize) -> usize {
    s[..i]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(j, _)| j)
}

/// The byte offset of the cluster boundary after `i` (the end at the end).
pub(crate) fn next_cluster(s: &str, i: usize) -> usize {
    s[i..]
        .graphemes(true)
        .next()
        .map_or(s.len(), |g| i + g.len())
}

/// Script blocks that use cluster boundaries for soft wrapping and word steps.
pub(crate) fn unspaced(c: char) -> bool {
    unspaced_symbol(c)
        || matches!(c as u32,
        0x0E00..=0x0EFF // Thai, Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x1980..=0x19DF // New Tai Lue
        | 0x1A20..=0x1AAF // Tai Tham
        | 0x3000..=0x312F // CJK punctuation, kana, Bopomofo
        | 0x31A0..=0x31BF // Bopomofo Extended
        | 0x31F0..=0x31FF // Katakana Phonetic Extensions
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF // ideographs
        | 0xA9E0..=0xA9FF | 0xAA60..=0xAA7F // Myanmar Extended-B and A
        | 0xAA80..=0xAADF // Tai Viet
        | 0xFF00..=0xFFEF // fullwidth forms
        | 0x20000..=0x323AF) // Supplementary ideographs, including Extensions G and H
}

/// Khmer symbols, Kanbun marks and CJK strokes take a word step of their own.
pub(crate) fn unspaced_symbol(c: char) -> bool {
    matches!(c as u32, 0x19E0..=0x19FF | 0x3190..=0x319F | 0x31C0..=0x31EF)
}

/// The longest prefix of `s` that fits `max` columns, cut between
/// clusters.
pub(crate) fn take_width(s: &str, max: usize) -> &str {
    let mut w = 0;
    for (i, g) in s.grapheme_indices(true) {
        w += g.width();
        if w > max {
            return &s[..i];
        }
    }
    s
}

/// `s` in at most `max` columns, ending in `…` when cut.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    format!("{}…", take_width(s, max.saturating_sub(1)))
}

/// Bidirectional formatting controls (embeddings, overrides, isolates and
/// marks): invisible, yet able to reorder what is shown around them.
pub(crate) fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `s` with every bidi control shown as `⟨U+202E⟩`, so a name like
/// `report\u{202E}fdp.exe` in an approval can't display reversed (#791).
pub(crate) fn escape_bidi(s: &str) -> std::borrow::Cow<'_, str> {
    escape_controls(s, is_bidi_control)
}

/// Make invisible formatting visible in an identifier or other exact value
/// the user is deciding about. Ordinary prose uses `escape_bidi`.
pub(crate) fn escape_approval_controls(s: &str) -> std::borrow::Cow<'_, str> {
    escape_controls(s, |c| {
        is_bidi_control(c)
            || matches!(
                c,
                '\u{00AD}' | '\u{180E}' | '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2064}'
                    | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}'
            )
    })
}

fn escape_controls(s: &str, escape: fn(char) -> bool) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(escape) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut shown = String::with_capacity(s.len());
    for c in s.chars() {
        if escape(c) {
            shown.push_str(&format!("⟨U+{:04X}⟩", u32::from(c)));
        } else {
            shown.push(c);
        }
    }
    std::borrow::Cow::Owned(shown)
}

/// [`escape_bidi`] over every span of a line.
pub(crate) fn escape_bidi_line(line: ratatui::text::Line<'static>) -> ratatui::text::Line<'static> {
    let spans = line
        .spans
        .into_iter()
        .map(|s| ratatui::text::Span::styled(escape_bidi(&s.content).into_owned(), s.style))
        .collect::<Vec<_>>();
    ratatui::text::Line { spans, ..line }
}

/// Display a stored line as text, including visible escapes for terminal
/// controls, line separators and directional formatting.
pub(crate) fn escape_terminal_line(s: &str) -> String {
    let visible = escape_bidi(s);
    let escape = |c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}');
    if !visible.chars().any(escape) {
        return visible.into_owned();
    }
    let mut out = String::with_capacity(visible.len());
    for c in visible.chars() {
        if escape(c) {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bidi_controls_are_shown_not_obeyed() {
        assert_eq!(
            escape_bidi("report\u{202E}fdp.exe"),
            "report⟨U+202E⟩fdp.exe"
        );
        assert_eq!(escape_bidi("a\u{2066}b\u{2069}"), "a⟨U+2066⟩b⟨U+2069⟩");
        assert!(matches!(
            escape_bidi("plain"),
            std::borrow::Cow::Borrowed(_)
        ));
        // Arabic text itself is untouched.
        assert_eq!(escape_bidi("مرحبا"), "مرحبا");
    }

    #[test]
    fn approval_controls_are_shown_in_exact_values() {
        for code in [
            0xad, 0x180e, 0x200b, 0x200c, 0x200d, 0x2060, 0x2061, 0x2062, 0x2063, 0x2064, 0x202e,
            0xfeff, 0xe0000, 0xe0061, 0xe007f,
        ] {
            let c = char::from_u32(code).unwrap();
            assert_eq!(
                escape_approval_controls(&format!("a{c}z")),
                format!("a⟨U+{code:04X}⟩z")
            );
        }
        let prose = "می\u{200c}خواهم 👩\u{200d}💻";
        assert_eq!(escape_bidi(prose), prose);
        assert!(matches!(
            escape_approval_controls("path ✈\u{fe0f}"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
