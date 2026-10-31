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

/// Scripts written without spaces between words (Thai, Lao, Khmer,
/// Myanmar, Chinese, Japanese): any cluster boundary may break a line or
/// end a word step.
pub(crate) fn unspaced(c: char) -> bool {
    matches!(c as u32,
        0x0E00..=0x0EFF // Thai, Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x3000..=0x30FF // CJK punctuation, kana
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF // ideographs
        | 0xFF00..=0xFFEF // fullwidth forms
        | 0x20000..=0x2FFFF)
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
