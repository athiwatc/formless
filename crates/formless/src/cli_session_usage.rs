//! Two independent, existing reports; buffer both before emitting stdout.
use super::*;

pub(super) fn write_report(
    out: &mut impl Write,
    session_id: &str,
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
) -> Result<()> {
    let usage = request(Request::SessionUsage {
        session_id: session_id.into(),
    })?;
    let context = request(Request::ContextUsage {
        session_id: session_id.into(),
    })?;
    let report = serde_json::json!({"session_id":session_id,"usage":usage,"context":context});
    let text = if json {
        format!("{}\n", serde_json::to_string_pretty(&report)?)
    } else {
        human(&report)?
    };
    out.write_all(text.as_bytes())?;
    Ok(())
}

fn human(report: &Value) -> Result<String> {
    use std::fmt::Write;
    let number = |value: &Value, key: &str| {
        value[key]
            .as_u64()
            .with_context(|| format!("{key} was not a nonnegative integer"))
    };
    let text = |value: &Value, key: &str| {
        value[key]
            .as_str()
            .map(comparison_terminal_text)
            .with_context(|| format!("{key} was not text"))
    };
    let usage = &report["usage"];
    let context = &report["context"];
    let mut out = format!(
        "Session: {}\nReported Run count: {}\n",
        text(report, "session_id")?,
        number(usage, "runs")?
    );
    for (label, value) in [
        ("Recorded total", &usage["total"]),
        ("Latest Run", &usage["last_run"]),
    ] {
        if value.is_null() {
            writeln!(out, "latest Run usage unavailable")?;
            continue;
        }
        writeln!(out, "{label}:")?;
        for (label, key) in [
            ("Steps", "steps"),
            ("Prompt tokens", "prompt_tokens"),
            ("Completion tokens", "completion_tokens"),
            ("Cached tokens", "cached_tokens"),
            ("Cache-write tokens", "cache_write_tokens"),
        ] {
            writeln!(out, "  {label}: {}", number(value, key)?)?;
        }
        let cost = if value["cost"].is_null() {
            "-".into()
        } else {
            anyhow::ensure!(
                value["cost"].is_number(),
                "usage cost was not numeric or null"
            );
            value["cost"].to_string()
        };
        writeln!(out, "  Estimated cost (USD): {cost}")?;
        let unpriced = match value.get("unpriced") {
            None => "not reported".into(),
            Some(Value::Array(models)) if models.is_empty() => "none reported".into(),
            Some(Value::Array(models)) => models
                .iter()
                .map(|model| {
                    model
                        .as_str()
                        .map(comparison_terminal_text)
                        .context("unpriced model was not text")
                })
                .collect::<Result<Vec<_>>>()?
                .join(", "),
            _ => bail!("unpriced models was not an array"),
        };
        writeln!(out, "  Unpriced models: {unpriced}")?;
    }
    writeln!(out,"Current context estimate:\n  Model: {}\n  Used tokens (estimated): {}\n  Context tokens: {}\n  Context source: {}",text(context,"model")?,number(context,"used_tokens")?,number(context,"context_tokens")?,text(context,"context_source")?)?;
    let breakdown = &context["breakdown"];
    for (label, key) in [
        ("System prompt tokens (estimated)", "system_prompt"),
        ("Tool schema tokens (estimated)", "tool_schemas"),
        ("History tokens (estimated)", "history"),
        (
            "Attachment entries in stored message metadata",
            "attachments",
        ),
    ] {
        writeln!(out, "  {label}: {}", number(breakdown, key)?)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn usage() -> Value {
        serde_json::json!({"runs":12,"total":{"steps":0,"prompt_tokens":10,"completion_tokens":2,"cached_tokens":0,"cache_write_tokens":1,"cost":null,"unpriced":["モデル ไทย"],"future":true},"last_run":null,"future":7})
    }
    fn context() -> Value {
        serde_json::json!({"model":"日本語 model","used_tokens":99,"context_tokens":1000,"context_source":"configured","measured":false,"breakdown":{"system_prompt":10,"tool_schemas":11,"history":12,"attachments":3},"future":"retained"})
    }
    #[test]
    fn replies_are_buffered_and_preserved_without_projecting_future_fields() {
        for json in [false, true] {
            let mut calls = Vec::new();
            let mut out = Vec::new();
            write_report(&mut out, "full-id", json, |request| {
                calls.push(request);
                Ok(if calls.len() == 1 { usage() } else { context() })
            })
            .unwrap();
            assert!(
                matches!(&calls[..],[Request::SessionUsage{session_id:a},Request::ContextUsage{session_id:b}] if a=="full-id"&&b=="full-id")
            );
            if json {
                assert_eq!(
                    serde_json::from_slice::<Value>(&out).unwrap(),
                    serde_json::json!({"session_id":"full-id","usage":usage(),"context":context()})
                );
            } else {
                let text = String::from_utf8(out).unwrap();
                for expected in [
                    "Reported Run count: 12",
                    "Steps: 0",
                    "Estimated cost (USD): -",
                    "Unpriced models: モデル ไทย",
                    "latest Run usage unavailable",
                    "Used tokens (estimated): 99",
                    "System prompt tokens (estimated): 10",
                    "Attachment entries in stored message metadata: 3",
                ] {
                    assert!(text.contains(expected), "{text}");
                }
                assert!(!text.contains("no Runs") && !text.contains("completed"));
            }
        }
        for fail_at in [1, 2] {
            let mut calls = 0;
            let mut out = Vec::new();
            assert!(write_report(&mut out, "id", false, |_| {
                calls += 1;
                if calls == fail_at {
                    bail!("fixture failure");
                }
                Ok(usage())
            })
            .is_err());
            assert!(out.is_empty());
        }
    }
    #[test]
    fn missing_empty_and_zero_values_are_not_replaced_with_invented_data() {
        let mut usage = usage();
        let mut latest = usage["total"].clone();
        latest["unpriced"] = serde_json::json!([]);
        latest["cost"] = serde_json::json!(0);
        usage["last_run"] = latest;
        usage["total"].as_object_mut().unwrap().remove("unpriced");
        let text = human(&serde_json::json!({"session_id":"id","usage":usage,"context":context()}))
            .unwrap();
        assert!(
            text.contains("Unpriced models: not reported")
                && text.contains("Unpriced models: none reported")
        );
        assert!(text.contains("Estimated cost (USD): 0"));
        assert!(!text.contains("usage unavailable"));
        let cli =
            Cli::try_parse_from(["formless", "sessions", "usage", "日本語", "--json"]).unwrap();
        assert!(
            matches!(cli.cmd,Some(Cmd::Sessions{action:SessionsAction::Usage{id,json:true}}) if id=="日本語")
        );
    }
}
