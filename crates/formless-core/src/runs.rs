//! Run-level outcomes (SPEC §8.7): did a Run achieve the user's goal?

use formless_protocol::{RunMetricsInfo, RunSummary};

/// Whether a user message reads like "that's wrong": the previous Run
/// missed the goal. Deliberately narrow: only the message's opening.
pub fn is_correction(text: &str) -> bool {
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
        "no, that",
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
    let t = text.trim().to_lowercase().replace('’', "'");
    OPENINGS.iter().any(|o| t.starts_with(o))
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
        avg_steps: avg(|r| r.steps),
        avg_tool_calls: avg(|r| r.tool_calls),
        avg_tokens: avg(|r| r.prompt_tokens + r.completion_tokens),
        avg_duration_ms: avg(|r| r.duration_ms),
    }
}

/// One line for the optimiser's proposal signals and the Metrics title.
pub fn summary_line(s: &RunSummary) -> String {
    format!(
        "Runs: {} · {:.0}% completed · {:.0}% interrupted · {:.0}% paused · {:.0}% edited · {:.0}% corrected · {:.1} steps · {:.1} tool calls · {:.0} tokens avg",
        s.runs,
        s.completed * 100.0,
        s.interrupted * 100.0,
        s.paused * 100.0,
        s.edited * 100.0,
        s.corrected * 100.0,
        s.avg_steps,
        s.avg_tool_calls,
        s.avg_tokens
    )
}
