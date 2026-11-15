//! Human presentation of stored observations and cancellation acknowledgments.
use super::*;

fn field(value: &Value, key: &str) -> String {
    match value.get(key) {
        None | Some(Value::Null) => "not reported".into(),
        Some(Value::String(text)) => comparison_terminal_text(text),
        Some(other) => comparison_terminal_text(&other.to_string()),
    }
}

pub(super) fn show(value: &Value) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    for (label, key) in [
        ("Run ID", "run_id"),
        ("Session ID", "session_id"),
        ("Stored status", "status"),
        ("Created", "created_at"),
        ("Updated", "updated_at"),
        ("Start message sequence", "start_message_seq"),
        ("End message sequence", "end_message_seq"),
        ("Steps", "steps"),
        ("Tool calls", "tool_calls"),
        ("Recorded prompt tokens", "prompt_tokens"),
        ("Recorded completion tokens", "completion_tokens"),
    ] {
        writeln!(out, "{label}: {}", field(value, key)).unwrap();
    }
    if value.get("error").is_some_and(|error| !error.is_null()) {
        writeln!(out, "Stored error: {}", field(value, "error")).unwrap();
    }
    if let Some(spend) = value.get("spend").filter(|value| value.is_object()) {
        out.push_str("Spend report (available estimates):\n");
        for (label, key) in [
            ("Known USD subtotal", "known_usd"),
            ("Total USD", "total_usd"),
            ("Usage complete", "usage_complete"),
            ("Spend complete", "spend_complete"),
            ("Admitted attempts", "admitted_attempts"),
            ("Usage records", "usage_records"),
            ("Uses local prices", "uses_local_prices"),
            ("Unpriced models", "unpriced"),
        ] {
            let display = if matches!(key, "known_usd" | "total_usd")
                && spend.get(key).is_none_or(Value::is_null)
            {
                "-".into()
            } else {
                field(spend, key)
            };
            writeln!(out, "  {label}: {display}").unwrap();
        }
    } else {
        out.push_str("Spend report: unavailable\n");
    }
    out
}

pub(super) fn cancel(value: &Value) -> String {
    let id = field(value, "run_id");
    match value["interrupted"].as_bool() {
        Some(true) => format!("Cancellation requested for Run {id}.\n"),
        Some(false) => format!("No active accepting Run was found to interrupt for Run {id}.\n"),
        None => format!(
            "Cancellation reply did not report whether a request was accepted for Run {id}.\n"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stored_statuses_missing_fields_and_partial_spend_stay_evidence_limited() {
        for status in [
            "running",
            "completed",
            "paused",
            "failed",
            "abandoned",
            "interrupted",
            "unknown",
        ] {
            let text = show(
                &serde_json::json!({"run_id":"run-日本語","status":status,"prompt_tokens":0,"completion_tokens":0}),
            );
            assert!(text.contains(&format!("Stored status: {status}")));
            assert!(
                text.contains("Created: not reported")
                    && text.contains("Recorded prompt tokens: 0")
            );
            assert!(text.contains("Spend report: unavailable"));
        }
        let text = show(
            &serde_json::json!({"spend":{"known_usd":0,"total_usd":null,"usage_complete":false,"spend_complete":false,"unpriced":["モデル"]},"error":"first\n\u{1b}[31mไทย"}),
        );
        for expected in [
            "Known USD subtotal: 0",
            "Total USD: -",
            "Usage complete: false",
            "Spend complete: false",
            "モデル",
            "first\\u{a}\\u{1b}[31mไทย",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains('\u{1b}'));
        assert!(show(&serde_json::json!({})).contains("Stored status: not reported"));
    }
    #[test]
    fn cancellation_acknowledgments_never_claim_a_terminal_outcome() {
        assert_eq!(
            cancel(&serde_json::json!({"run_id":"id","interrupted":true})),
            "Cancellation requested for Run id.\n"
        );
        let text = cancel(&serde_json::json!({"run_id":"id","interrupted":false}));
        assert!(text.contains("No active accepting Run"));
        assert!(!text.contains("stopped") && !text.contains("finished"));
        assert!(cancel(&serde_json::json!({})).contains("did not report"));
    }
}
