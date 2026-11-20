//! Human presentation of stored observations and cancellation acknowledgments.
use super::*;

fn field(value: &Value, key: &str) -> String {
    match value.get(key) {
        None | Some(Value::Null) => "not reported".into(),
        Some(Value::String(text)) => comparison_terminal_text(text),
        Some(other) => comparison_terminal_text(&other.to_string()),
    }
}

fn stored_duration(value: &Value) -> Option<String> {
    let timestamp = |key: &str| chrono::DateTime::parse_from_rfc3339(value[key].as_str()?).ok();
    let elapsed = (timestamp("updated_at")? - timestamp("created_at")?)
        .to_std()
        .ok()?;
    Some(format_duration(elapsed))
}

fn spend_field(spend: &Value, key: &str) -> String {
    match key {
        "known_usd" | "total_usd" => spend[key]
            .as_f64()
            .map_or_else(|| "-".into(), |usd| format!("${usd:.4}")),
        "usage_complete" | "spend_complete" | "uses_local_prices" => match spend[key].as_bool() {
            Some(true) => "yes".into(),
            Some(false) => "no".into(),
            None => "not reported".into(),
        },
        "unpriced" => match spend[key].as_array() {
            Some(names) if names.is_empty() => "none".into(),
            Some(names) if names.iter().all(Value::is_string) => names
                .iter()
                .filter_map(Value::as_str)
                .map(comparison_terminal_text)
                .collect::<Vec<_>>()
                .join(", "),
            _ => "not reported".into(),
        },
        _ => field(spend, key),
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
    writeln!(
        out,
        "Duration (stored timestamp span): {}",
        stored_duration(value).as_deref().unwrap_or("not reported")
    )
    .unwrap();
    for (label, key) in [
        ("Recorded cached tokens", "cached_tokens"),
        ("Recorded cache write tokens", "cache_write_tokens"),
        ("Recorded reasoning tokens", "reasoning_tokens"),
    ] {
        let count = value["token_details"][key]
            .as_u64()
            .map_or_else(|| "not reported".into(), |count| count.to_string());
        writeln!(out, "{label}: {count}").unwrap();
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
            let display = spend_field(spend, key);
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
            &serde_json::json!({"spend":{"known_usd":0.0123456789,"total_usd":null,"usage_complete":false,"spend_complete":false,"uses_local_prices":true,"unpriced":["モデル","local/foo"]},"error":"first\n\u{1b}[31mไทย"}),
        );
        for expected in [
            "Known USD subtotal: $0.0123",
            "Total USD: -",
            "Usage complete: no",
            "Spend complete: no",
            "Uses local prices: yes",
            "Unpriced models: モデル, local/foo",
            "first\\u{a}\\u{1b}[31mไทย",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains('\u{1b}'));
        assert!(show(&serde_json::json!({})).contains("Stored status: not reported"));
    }

    #[test]
    fn show_keeps_timestamp_span_and_token_details_evidence_limited() {
        let mut reply = serde_json::json!({
            "created_at":"2026-11-20T08:00:00+07:00",
            "updated_at":"2026-11-20T02:02:05Z",
            "token_details":{"cached_tokens":0,"cache_write_tokens":null,"reasoning_tokens":3},
            "spend":{"known_usd":0.0,"total_usd":0.0456789,"unpriced":[]}
        });
        let raw = serde_json::to_string_pretty(&reply).unwrap();
        let text = show(&reply);
        for expected in [
            "Duration (stored timestamp span): 1h2m5s",
            "Recorded cached tokens: 0",
            "Recorded cache write tokens: not reported",
            "Recorded reasoning tokens: 3",
            "Known USD subtotal: $0.0000",
            "Total USD: $0.0457",
            "Usage complete: not reported",
            "Unpriced models: none",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert_eq!(serde_json::to_string_pretty(&reply).unwrap(), raw);
        for updated in ["bad timestamp", "2026-11-20T00:59:59Z"] {
            reply["updated_at"] = serde_json::json!(updated);
            assert!(show(&reply).contains("Duration (stored timestamp span): not reported"));
        }
        reply["spend"]["unpriced"] = serde_json::json!(["model\u{1b}[31m"]);
        let text = show(&reply);
        assert!(text.contains("Unpriced models: model\\u{1b}[31m"));
        assert!(!text.contains('\u{1b}'));
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
