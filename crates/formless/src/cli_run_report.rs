//! Human presentation of stored observations and cancellation acknowledgments.
use super::*;

fn field(value: &Value, key: &str) -> String {
    match value.get(key) {
        None | Some(Value::Null) => "not reported".into(),
        Some(Value::String(text)) => comparison_terminal_text(text),
        Some(other) => comparison_terminal_text(&other.to_string()),
    }
}

fn elapsed(value: &Value) -> Option<String> {
    let created = chrono::DateTime::parse_from_rfc3339(value["created_at"].as_str()?).ok()?;
    let updated = chrono::DateTime::parse_from_rfc3339(value["updated_at"].as_str()?).ok()?;
    let duration = (updated - created).to_std().ok()?;
    Some(if duration.as_secs() == 0 {
        format!("{}ms", duration.as_millis())
    } else {
        format_duration(duration)
    })
}

fn spend_field(spend: &Value, key: &str) -> String {
    match key {
        "known_usd" | "total_usd" => spend[key]
            .as_f64()
            .map_or_else(|| "-".into(), |amount| format!("${amount:.6}")),
        "usage_complete" | "spend_complete" | "uses_local_prices" => match spend[key].as_bool() {
            Some(true) => "yes".into(),
            Some(false) => "no".into(),
            None => "not reported".into(),
        },
        "unpriced" => {
            let models = spend[key]
                .as_array()
                .and_then(|values| values.iter().map(Value::as_str).collect::<Option<Vec<_>>>());
            match models {
                Some(models) if models.is_empty() => "none".into(),
                Some(models) => comparison_terminal_text(&models.join(", ")),
                None => "not reported".into(),
            }
        }
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
        ("Recorded cached tokens", "cached_tokens"),
        ("Recorded cache-write tokens", "cache_write_tokens"),
        ("Recorded reasoning tokens", "reasoning_tokens"),
    ] {
        writeln!(out, "{label}: {}", field(value, key)).unwrap();
    }
    writeln!(
        out,
        "Duration: {}",
        elapsed(value).as_deref().unwrap_or("not reported")
    )
    .unwrap();
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
        match value["spend_error"].as_str() {
            Some(reason) => writeln!(
                out,
                "Spend report: unavailable ({})",
                comparison_terminal_text(reason)
            )
            .unwrap(),
            None => out.push_str("Spend report: unavailable\n"),
        }
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
            &serde_json::json!({"created_at":"2026-11-20T12:00:00Z","updated_at":"2026-11-20T12:01:02.500Z","cached_tokens":2,"cache_write_tokens":3,"reasoning_tokens":4,"spend":{"known_usd":0.0123456789,"total_usd":null,"usage_complete":false,"spend_complete":false,"uses_local_prices":true,"unpriced":["モデル","local/foo"]},"error":"first\n\u{1b}[31mไทย"}),
        );
        for expected in [
            "Known USD subtotal: $0.012346",
            "Total USD: -",
            "Usage complete: no",
            "Spend complete: no",
            "Uses local prices: yes",
            "Unpriced models: モデル, local/foo",
            "Duration: 1m2s",
            "Recorded cached tokens: 2",
            "Recorded cache-write tokens: 3",
            "Recorded reasoning tokens: 4",
            "first\\u{a}\\u{1b}[31mไทย",
        ] {
            assert!(text.contains(expected), "{text}");
        }
        assert!(!text.contains('\u{1b}'));
        assert!(show(&serde_json::json!({})).contains("Stored status: not reported"));
        assert!(
            show(&serde_json::json!({"spend":{"unpriced":[]}})).contains("Unpriced models: none")
        );
        assert!(show(&serde_json::json!({"created_at":"2026-11-20T12:01:00Z","updated_at":"2026-11-20T12:00:00Z","spend_error":"configuration could not be loaded"})).contains("Duration: not reported\nSpend report: unavailable (configuration could not be loaded)"));
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
