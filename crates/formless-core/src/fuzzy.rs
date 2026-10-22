//! Fuzzy (subsequence) matching shared by the model picker and file
//! mentions (SPEC §4.2, §5.6).

/// Score `candidate` against `query` as a case-insensitive subsequence;
/// higher is better. Returns the matched char positions for highlighting.
/// Each place the first query char occurs is tried as a start, so a
/// contiguous match later in the id beats a scattered one earlier.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<(i64, Vec<usize>)> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let c: Vec<char> = candidate.chars().flat_map(char::to_lowercase).collect();
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
    // Prefer shorter ids among equal matches.
    best.map(|(score, pos)| (score - (c.len() / 8) as i64, pos))
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
}
