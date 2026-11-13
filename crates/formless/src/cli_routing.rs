//! Read-only projection of the existing daemon routing report, without ranking.
use super::*;

fn field(value: &Value, key: &str) -> String {
    match value.get(key) {
        None => "unknown (not reported)".into(),
        Some(Value::String(text)) => comparison_terminal_text(text),
        Some(other) => comparison_terminal_text(&other.to_string()),
    }
}

fn render(value: &Value, json: bool) -> Result<String> {
    if json {
        return Ok(serde_json::to_string_pretty(value)?);
    }
    let decisions = value.as_array().context("routing report is not an array")?;
    if decisions.is_empty() {
        return Ok("No decisions are currently available in this daemon report.".into());
    }
    let mut lines = Vec::new();
    for decision in decisions {
        lines.push(format!(
            "{} · {} → {}",
            field(decision, "at"),
            field(decision, "kind"),
            field(decision, "picked")
        ));
        lines.push(format!("  reason: {}", field(decision, "reason")));
        if let Some(candidates) = decision.get("candidates").and_then(Value::as_array) {
            for candidate in candidates {
                let picked = decision
                    .get("picked")
                    .and_then(Value::as_str)
                    .zip(candidate.get("model").and_then(Value::as_str))
                    .is_some_and(|(picked, model)| picked == model);
                let figures = [
                    "samples",
                    "good_rate",
                    "avg_ms",
                    "score",
                    "cost",
                    "qualified",
                ]
                .map(|key| format!("{key}: {}", field(candidate, key)))
                .join(" · ");
                lines.push(format!(
                    "  {}{} · {figures}",
                    if picked { "[picked] " } else { "" },
                    field(candidate, "model")
                ));
            }
        } else {
            lines.push(format!("  candidates: {}", field(decision, "candidates")));
        }
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

pub(super) fn write_report(
    out: &mut impl Write,
    json: bool,
    request: impl FnOnce(Request) -> Result<Value>,
) -> Result<()> {
    let value = request(Request::RoutingDecisions)?;
    let report = render(&value, json)?;
    writeln!(out, "{report}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn human_report_preserves_order_figures_unknowns_and_picked_marker() {
        let reply = json!([
            {"at":"newest","kind":"role:ไทย","picked":"日本語","reason":"เหตุผล ไทย",
             "candidates":[{"model":"日本語","samples":0,"avg_ms":null,"qualified":false},
                           {"model":"other","samples":5,"good_rate":0.75,"score":1.2345,"cost":0,"qualified":true}]},
            {"at":"oldest","reason":null,"candidates":[{}]}
        ]);
        let report = render(&reply, false).unwrap();
        assert!(report.find("newest").unwrap() < report.find("oldest").unwrap());
        for text in [
            "[picked] 日本語",
            "เหตุผล ไทย",
            "samples: 0",
            "avg_ms: null",
            "score: unknown (not reported)",
            "good_rate: 0.75",
            "score: 1.2345",
            "cost: 0",
            "qualified: false",
            "reason: null",
        ] {
            assert!(report.contains(text), "{text}: {report}");
        }
        assert_eq!(
            report.matches("[picked]").count(),
            1,
            "missing model/pick never marks a candidate"
        );
        assert!(!report.contains("Session"));
        assert_eq!(
            render(&json!([]), false).unwrap(),
            "No decisions are currently available in this daemon report."
        );
    }

    #[test]
    fn json_keeps_the_complete_value_and_request_errors_leave_stdout_empty() {
        let reply = json!([{"picked":null,"future":{"list":[null,"ไทย",0,false]},"candidates":[]}]);
        let mut out = Vec::new();
        let mut calls = 0;
        write_report(&mut out, true, |request| {
            calls += 1;
            assert!(matches!(request, Request::RoutingDecisions));
            Ok(reply.clone())
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), reply);
        out.clear();
        let error = write_report(&mut out, false, |request| {
            assert!(matches!(request, Request::RoutingDecisions));
            bail!("fixture request refused")
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "fixture request refused");
        assert!(out.is_empty());
        assert!(write_report(&mut out, false, |_| Ok(json!({"unexpected":true}))).is_err());
        assert!(
            out.is_empty(),
            "malformed reports are rejected before printing"
        );
    }

    #[test]
    fn routing_parser_and_help_expose_json_without_dispatch() {
        assert!(matches!(
            Cli::try_parse_from(["formless", "routing", "--json"])
                .unwrap()
                .cmd,
            Some(Cmd::Routing { json: true })
        ));
        let help = Cli::try_parse_from(["formless", "routing", "--help"])
            .err()
            .unwrap();
        assert_eq!(help.kind(), clap::error::ErrorKind::DisplayHelp);
        assert!(help.to_string().contains("--json"));
    }
}
