//! Fuzzy (subsequence) matching shared by the model picker and file
//! mentions (SPEC §4.2, §5.6).

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// `s` folded for matching, one key per base letter, each with the index
/// of the original char it came from (#789): case folded without
/// changing length (`İ` is one key, `i`), and decomposed with its marks
/// dropped, so an NFC `é` typed by the user matches the NFD `e`+U+0301
/// macOS returns for file names, and `cafe` matches `café`.
pub fn fold(s: &str) -> Vec<(usize, char)> {
    let mut out = Vec::with_capacity(s.len());
    for (i, ch) in s.chars().enumerate() {
        for base in std::iter::once(ch).nfd().filter(|c| !is_combining_mark(*c)) {
            let lower = base.to_lowercase().next().unwrap_or(base);
            out.push((i, lower));
        }
    }
    out
}

/// The char range of `s` where folded `needle` first occurs.
pub fn find_folded(s: &str, needle: &str) -> Option<std::ops::Range<usize>> {
    let hay = fold(s);
    let n: Vec<char> = fold(needle).into_iter().map(|(_, c)| c).collect();
    if n.is_empty() {
        return None;
    }
    let at = hay
        .windows(n.len())
        .position(|w| w.iter().map(|(_, c)| *c).eq(n.iter().copied()))?;
    Some(hay[at].0..hay[at + n.len() - 1].0 + 1)
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
