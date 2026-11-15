//! Run-level outcomes (SPEC §8.7): did a Run achieve the user's goal?

use formless_protocol::{RunMetricsInfo, RunSummary};

const OPENINGS: &[&str] = &[
    "that's wrong",
    "thats wrong",
    "that is wrong",
    "that's not right",
    "that is not right",
    "that's not what",
    "that is not what",
    "not what i asked",
    "not what i meant",
    "no that",
    "no that's",
    "wrong",
    "it didn't work",
    "it doesn't work",
    "that didn't work",
    "that doesn't work",
    "you didn't",
    "still broken",
    "still not working",
];
// Praise and reassurance that open like a correction (#831).
const NOT_CORRECTIONS: &[&str] = &[
    "no that was perfect",
    "no that's perfect",
    "no that was great",
    "no that's great",
    "no that was right",
    "no that's right",
    "no that works",
    "no that's fine",
    "no that was fine",
    "no that's good",
    "no that was good",
    "no that's exactly right",
    "no that was exactly right",
    "no that's exactly what i wanted",
    "no that was exactly what i wanted",
    "you didn't need to",
    "you didn't have to",
];

const CORRECTION_SOURCE_SCALARS: usize = 40;
const _: () = {
    let lists = [OPENINGS, NOT_CORRECTIONS];
    let mut list = 0;
    while list < lists.len() {
        let mut index = 0;
        while index < lists[list].len() {
            let phrase = lists[list][index];
            assert!(
                phrase.is_ascii(),
                "reconsider bounded normalization for non-ASCII phrases"
            );
            assert!(
                phrase.len() + 2 <= CORRECTION_SOURCE_SCALARS,
                "phrase, optional comma and boundary must fit the source prefix"
            );
            index += 1;
        }
        list += 1;
    }
};

/// Whether a user message reads like "that's wrong": the previous Run
/// missed the goal. Deliberately narrow: only the message's opening.
pub fn is_correction(text: &str) -> bool {
    let trimmed = text.trim();
    let end = trimmed
        .char_indices()
        .nth(CORRECTION_SOURCE_SCALARS)
        .map_or(trimmed.len(), |(index, _)| index);
    // Trimming still scans whitespace. Only normalization allocation/work is
    // bounded. Contextual lowercase can differ beyond the ASCII comparisons;
    // boundary letters remain alphanumeric in either form.
    let t = trimmed[..end].to_lowercase().replace('’', "'");
    // Canonicalize only the leading no-comma variant, within the bounded text.
    let t = if t.starts_with("no, ") {
        t.replacen("no, ", "no ", 1)
    } else {
        t
    };
    if NOT_CORRECTIONS.iter().any(|p| t.starts_with(p)) {
        return false;
    }
    // Whole words only: "wrongdoing" doesn't open with "wrong".
    OPENINGS.iter().any(|o| {
        t.starts_with(o)
            && t[o.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric())
    })
}

/// Roll finished Runs up (Runs still going are left out).
pub fn summarize(runs: &[RunMetricsInfo]) -> RunSummary {
    let done: Vec<&RunMetricsInfo> = runs.iter().filter(|r| r.status != "running").collect();
    let n = done.len() as f64;
    if done.is_empty() {
        return RunSummary::default();
    }
    let share = |o: &str| done.iter().filter(|r| r.outcome == o).count() as f64 / n;
    let avg = |f: fn(&RunMetricsInfo) -> i64| done.iter().map(|r| f(r) as f64).sum::<f64>() / n;
    RunSummary {
        runs: done.len() as i64,
        completed: share("completed"),
        interrupted: share("interrupted"),
        paused: share("paused"),
        edited: share("edited"),
        corrected: share("corrected"),
        rated_good: done.iter().filter(|r| r.rating == "good").count() as f64 / n,
        rated_bad: share("rated_bad"),
        avg_steps: avg(|r| r.steps),
        avg_tool_calls: avg(|r| r.tool_calls),
        avg_tokens: avg(|r| r.prompt_tokens + r.completion_tokens),
        avg_duration_ms: avg(|r| r.duration_ms),
    }
}

/// One line for the optimiser's proposal signals and the Metrics title.
pub fn summary_line(s: &RunSummary) -> String {
    format!(
        "Runs: {} · {:.0}% completed · {:.0}% interrupted · {:.0}% paused · {:.0}% edited · {:.0}% corrected · {:.0}% 👍 · {:.0}% 👎 · {:.1} steps · {:.1} tool calls · {:.0} tokens avg",
        s.runs,
        s.completed * 100.0,
        s.interrupted * 100.0,
        s.paused * 100.0,
        s.edited * 100.0,
        s.corrected * 100.0,
        s.rated_good * 100.0,
        s.rated_bad * 100.0,
        s.avg_steps,
        s.avg_tool_calls,
        s.avg_tokens
    )
}

#[cfg(test)]
mod correction_prefix_tests {
    use super::*;

    fn full_normalization_reference(text: &str) -> bool {
        let text = text.trim().to_lowercase().replace('’', "'");
        let text = if text.starts_with("no, ") {
            text.replacen("no, ", "no ", 1)
        } else {
            text
        };
        !NOT_CORRECTIONS
            .iter()
            .any(|prefix| text.starts_with(prefix))
            && OPENINGS.iter().any(|prefix| {
                text.starts_with(prefix)
                    && text[prefix.len()..]
                        .chars()
                        .next()
                        .is_none_or(|c| !c.is_alphanumeric())
            })
    }

    #[test]
    fn praise_with_optional_no_comma_is_not_a_correction() {
        for praise in [
            "no that's perfect",
            "no that was great",
            "no that's right",
            "no that's fine",
            "no that works",
            "no that's good",
            "no that's exactly right",
            "no that's exactly what i wanted",
            "no that was exactly what i wanted",
        ] {
            for text in [
                praise.to_string(),
                praise.replacen("no ", "no, ", 1),
                praise.to_uppercase().replace('\'', "’"),
            ] {
                assert!(
                    !is_correction(&format!("{text}, thanks {}", "long tail ".repeat(100))),
                    "{text}"
                );
            }
        }
        for text in [
            "no that's wrong",
            "no, that's wrong",
            "no, that was broken",
            "no, that's exactly the wrong thing",
        ] {
            assert!(is_correction(text), "{text}");
        }
    }

    #[test]
    fn bounded_correction_normalization_preserves_all_phrase_and_unicode_boundaries() {
        for phrase in OPENINGS.iter().chain(NOT_CORRECTIONS) {
            for prefix in [
                phrase.to_string(),
                phrase.replacen("no ", "no, ", 1),
                phrase.to_uppercase(),
                phrase.to_uppercase().replace('\'', "’"),
            ] {
                for suffix in [
                    String::new(),
                    "!".into(),
                    "doing".into(),
                    "ไทย".into(),
                    "İ".into(),
                    format!("Σ{}A", "\u{301}".repeat(100)),
                    format!(" {}", "long tail ไทย ".repeat(1000)),
                ] {
                    let text = format!(
                        "{}{}{}{}",
                        " \t\n".repeat(100),
                        prefix,
                        suffix,
                        " \n".repeat(100)
                    );
                    assert_eq!(
                        is_correction(&text),
                        full_normalization_reference(&text),
                        "phrase {phrase:?}"
                    );
                }
            }
        }
        for text in [
            "",
            " \n\t",
            "Wrongdoing",
            "No that's great!",
            "No that was perfect",
            "İT DIDN'T WORK",
        ] {
            assert_eq!(
                is_correction(text),
                full_normalization_reference(text),
                "{text}"
            );
        }
        // A contextual mapping really can change when the source is cut;
        // both sigma forms still reject the same ASCII-opening boundary.
        let text = format!("WRONGΣ{}A", "\u{301}".repeat(100));
        let prefix: String = text.chars().take(CORRECTION_SOURCE_SCALARS).collect();
        assert!(prefix.to_lowercase().contains('ς'));
        assert!(text.to_lowercase().contains('σ'));
        assert_eq!(is_correction(&text), full_normalization_reference(&text));
        assert!(!is_correction(&text));
    }
}
