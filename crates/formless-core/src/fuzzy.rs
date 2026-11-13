//! Fuzzy (subsequence) matching shared by the model picker and file
//! mentions (SPEC §4.2, §5.6).

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// Accent-insensitive matching keys tagged with their original char indices.
/// Lowercase expansions retain every key; German sharp S explicitly matches
/// `ss` (lowercasing alone does not perform that case-fold expansion). NFC/NFD
/// spellings share keys, without shifting highlights into a folded copy.
pub fn fold(s: &str) -> Vec<(usize, char)> {
    let mut out = Vec::with_capacity(s.len());
    for (i, ch) in s.chars().enumerate() {
        out.extend(fold_char(ch).map(|c| (i, c)));
    }
    out
}

/// Streaming search keys with the same folding as [`fold`], without
/// allocating a folded copy of each candidate string.
pub fn fold_chars(s: &str) -> impl Iterator<Item = char> + '_ {
    s.chars().flat_map(fold_char)
}

fn fold_char(ch: char) -> impl Iterator<Item = char> {
    std::iter::once(ch)
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .flat_map(|lower| {
            match lower {
                'ß' => [Some('s'), Some('s')],
                // Both lowercase sigmas refer to the same uppercase letter.
                'ς' => [Some('σ'), None],
                c => [Some(c), None],
            }
            .into_iter()
            .flatten()
        })
}

/// The char range of `s` where folded `needle` first occurs.
pub fn find_folded(s: &str, needle: &str) -> Option<std::ops::Range<usize>> {
    let hay = fold(s);
    let n: Vec<char> = fold(needle).into_iter().map(|(_, c)| c).collect();
    if n.is_empty() {
        return None;
    }
    let window = hay
        .windows(n.len())
        .find(|w| w.iter().map(|(_, c)| *c).eq(n.iter().copied()))?;
    Some(window[0].0..window[n.len() - 1].0 + 1)
}

/// Non-overlapping matches, as original character ranges. A folded expansion
/// can match once per source glyph, never twice inside a single `ß`.
pub fn folded_match_ranges(s: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    let hay = fold(s);
    let n: Vec<char> = fold(needle).into_iter().map(|(_, c)| c).collect();
    if n.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut end = 0;
    for window in hay.windows(n.len()) {
        if window[0].0 >= end && window.iter().map(|(_, c)| *c).eq(n.iter().copied()) {
            let range = window[0].0..window[n.len() - 1].0 + 1;
            end = range.end;
            ranges.push(range);
        }
    }
    ranges
}

/// Score `candidate` against `query` as a case-insensitive subsequence;
/// higher is better. Returns the matched char positions for highlighting.
/// Each place the first query char occurs is tried as a start, so a
/// contiguous match later in the id beats a scattered one earlier.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<(i64, Vec<usize>)> {
    let q: Vec<char> = fold(query)
        .into_iter()
        .map(|(_, c)| c)
        .filter(|c| !c.is_whitespace())
        .collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let folded = fold(candidate);
    let c: Vec<char> = folded.iter().map(|(_, c)| *c).collect();
    let mut best: Option<(i64, Vec<usize>)> = None;
    for start in (0..c.len()).filter(|&i| c[i] == q[0]) {
        let Some(found) = match_from(&q, &c, start) else {
            // No later start can finish the match either.
            break;
        };
        if best.as_ref().is_none_or(|b| found.0 > b.0) {
            best = Some(found);
        }
    }
    // Prefer shorter ids among equal matches. Positions are the original
    // chars', so highlighting lands on the right glyphs.
    best.map(|(score, pos)| {
        let mut orig: Vec<usize> = pos.into_iter().map(|p| folded[p].0).collect();
        orig.dedup();
        (score - (c.len() / 8) as i64, orig)
    })
}

/// Greedy left-most match of `q` in `c` from `start`.
fn match_from(q: &[char], c: &[char], start: usize) -> Option<(i64, Vec<usize>)> {
    let mut positions = Vec::with_capacity(q.len());
    let mut score = 0i64;
    let mut prev: Option<usize> = None;
    for (i, ch) in c.iter().enumerate().skip(start) {
        if positions.len() == q.len() {
            break;
        }
        if *ch != q[positions.len()] {
            continue;
        }
        score += 1;
        // Runs of consecutive matches and matches at word starts count most.
        if prev.is_some_and(|p| p + 1 == i) {
            score += 5;
        }
        if i == 0 || !c[i - 1].is_alphanumeric() {
            score += 8;
        }
        if let Some(p) = prev {
            score -= (i - p - 1).min(10) as i64;
        }
        positions.push(i);
        prev = Some(i);
    }
    (positions.len() == q.len()).then_some((score, positions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_fold_agrees_with_positioned_keys() {
        for text in [
            "café",
            "cafe\u{301}",
            "Straße",
            "STRAẞE",
            "ΟΣ ος",
            "İ",
            "ไทย",
            "前文",
        ] {
            assert_eq!(
                fold_chars(text).collect::<Vec<_>>(),
                fold(text).into_iter().map(|(_, c)| c).collect::<Vec<_>>()
            );
        }
        assert_eq!(
            fold_chars("café STRAẞE ος").collect::<String>(),
            "cafe strasse οσ"
        );
        assert_eq!(find_folded("ΟΣ ος", "οσ"), Some(0..2));
    }

    #[test]
    fn sharp_s_matching_preserves_original_glyphs() {
        for spelling in ["straße", "STRAẞE", "strasse"] {
            for query in ["straße", "STRAẞE", "strasse"] {
                let (_, positions) = fuzzy_match(query, spelling).unwrap();
                assert_eq!(positions, (0..spelling.chars().count()).collect::<Vec<_>>());
                assert_eq!(
                    find_folded(&format!("see {spelling}!"), query),
                    Some(4..4 + spelling.chars().count())
                );
            }
        }
        assert_eq!(fold("ßẞ"), vec![(0, 's'), (0, 's'), (1, 's'), (1, 's')]);
        assert_eq!(fuzzy_match("rs", "straße.rs").unwrap().1, vec![7, 8]);
        assert_eq!(
            folded_match_ranges("ß ss ẞ", "s"),
            vec![0..1, 2..3, 3..4, 5..6]
        );
        assert_eq!(folded_match_ranges("ßß", "sss"), vec![0..2]);
        assert!(folded_match_ranges("abc", "").is_empty());
        assert!(folded_match_ranges("abc", "xyz").is_empty());
        assert_eq!(
            folded_match_ranges("café cafe\u{301} İ", "CAFE"),
            vec![0..4, 5..9]
        );
    }

    #[test]
    fn subsequence_matching() {
        assert!(fuzzy_match("son4", "anthropic/claude-sonnet-4.5").is_some());
        assert!(fuzzy_match("SON4", "claude-sonnet-4").is_some());
        assert!(fuzzy_match("4son", "claude-sonnet-4").is_none());
        assert_eq!(fuzzy_match("", "x").unwrap().0, 0);
        let (_, pos) = fuzzy_match("gpt", "openai/gpt-4o").unwrap();
        assert_eq!(pos, vec![7, 8, 9]);
        // A contiguous later match beats a scattered earlier one.
        let opus = fuzzy_match("opus", "anthropic/claude-opus-4").unwrap();
        let sonnet = fuzzy_match("opus", "anthropic/claude-sonnet-4").unwrap();
        assert!(opus.0 > sonnet.0, "{opus:?} vs {sonnet:?}");
        assert_eq!(opus.1, vec![17, 18, 19, 20]);
    }

    #[test]
    fn lowercase_expansion_does_not_shift_suffix_highlights() {
        let candidate = "İstanbul-gpt";
        let (score, positions) = fuzzy_match("gpt", candidate).unwrap();
        assert_eq!(positions, vec![9, 10, 11]);
        let highlighted: String = candidate
            .chars()
            .enumerate()
            .filter_map(|(i, ch)| positions.contains(&i).then_some(ch))
            .collect();
        assert_eq!(highlighted, "gpt");
        // Expanding lowercase must not add a character to the length penalty.
        assert_eq!(score, fuzzy_match("gpt", "Istanbul-gpt").unwrap().0);
    }

    #[test]
    fn ascii_scores_remain_stable() {
        assert_eq!(fuzzy_match("gpt", "gpt"), Some((21, vec![0, 1, 2])));
        assert_eq!(
            fuzzy_match("gpt", "openai/gpt-4o"),
            Some((20, vec![7, 8, 9]))
        );
    }

    /// Positions index the original string; NFC and NFD match each other
    /// (#789).
    #[test]
    fn folding_keeps_original_positions_and_ignores_normal_form() {
        let (_, pos) = fuzzy_match("istanbul", "İstanbul-guide").unwrap();
        assert_eq!(pos, (0..8).collect::<Vec<_>>());
        let nfd = "cafe\u{301}.rs";
        assert!(fuzzy_match("café.rs", nfd).is_some());
        assert!(fuzzy_match("cafe\u{301}", "café.rs").is_some());
        let (_, pos) = fuzzy_match("rs", nfd).unwrap();
        assert_eq!(pos, vec![6, 7], "indexes count the NFD mark");
        assert_eq!(
            find_folded("see İstanbul now", "i\u{307}stanbul"),
            Some(4..12)
        );
        assert_eq!(find_folded("see İstanbul now", "istanbul"), Some(4..12));
    }
}
