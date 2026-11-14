//! Three independent read-only reports sharing one already checked cutoff.
use super::*;
use std::io::IsTerminal;

fn value(row: &Value, key: &str) -> String {
    match row.get(key) {
        None => "-".into(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn human(report: &Value, terminal: bool, color: bool, width: Option<usize>) -> Result<String> {
    let mut sections = Vec::new();
    for (key, title, headers) in [
        (
            "models",
            "Models",
            vec![
                "MODEL",
                "SUCCESSES",
                "FAILURES",
                "TOTAL MS",
                "PROMPT TOKENS",
                "COMPLETION TOKENS",
                "CACHED TOKENS",
            ],
        ),
        (
            "tools",
            "Tools",
            vec!["TOOL ID", "INVOCATIONS", "SUCCESS RATE"],
        ),
        (
            "builtins",
            "Built-ins",
            vec!["NAME", "CALLS", "FAILURES", "AVG MS", "KINDS"],
        ),
    ] {
        let rows = report[key]
            .as_array()
            .with_context(|| format!("{key} report is not an array"))?;
        if rows.is_empty() {
            sections.push(format!("{title}\nnone in range\n"));
            continue;
        }
        let mut table = Table::new(&headers);
        for row in rows {
            let cells = match key {
                "models" => [
                    "model",
                    "successes",
                    "failures",
                    "total_ms",
                    "prompt_tokens",
                    "completion_tokens",
                    "cached_tokens",
                ]
                .map(|key| value(row, key))
                .to_vec(),
                "tools" => vec![
                    value(row, "tool_id"),
                    value(row, "invocations"),
                    row["success_rate"]
                        .as_f64()
                        .map(|rate| format!("{}%", rate * 100.0))
                        .unwrap_or_else(|| "-".into()),
                ],
                _ => {
                    let kinds = if let Some(kinds) = row.get("kinds").and_then(Value::as_object) {
                        let mut pairs: Vec<_> = kinds.iter().collect();
                        pairs.sort_by_key(|(key, _)| *key);
                        pairs
                            .into_iter()
                            .map(|(key, value)| format!("{key}={value}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    } else {
                        value(row, "kinds")
                    };
                    vec![
                        value(row, "name"),
                        value(row, "calls"),
                        value(row, "failures"),
                        value(row, "avg_ms"),
                        kinds,
                    ]
                }
            };
            table.row(cells.into_iter().map(c).collect());
        }
        sections.push(format!("{title}\n{}", table.render(terminal, color, width)));
    }
    Ok(sections.join("\n"))
}

pub(super) fn write_report(
    out: &mut impl Write,
    since: Option<String>,
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
) -> Result<()> {
    let builtins = request(Request::ListBuiltinStats {
        since: since.clone(),
    })?;
    let tools = request(Request::ListToolMetrics {
        since: since.clone(),
    })?;
    let models = request(Request::ListModelStats { since })?;
    let report = serde_json::json!({"models":models,"tools":tools,"builtins":builtins});
    let text = if json {
        serde_json::to_string_pretty(&report)?
    } else {
        let terminal = std::io::stdout().is_terminal();
        let width = terminal
            .then(|| {
                crossterm::terminal::size()
                    .ok()
                    .map(|(width, _)| width as usize)
            })
            .flatten();
        human(&report, terminal, terminal && table::color_allowed(), width)?
    };
    writeln!(out, "{text}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn replies() -> Value {
        json!({
            "models":[{"model":"β-日本語","successes":2,"failures":1,"total_ms":1000,"prompt_tokens":0,"completion_tokens":4,"cached_tokens":0,"future":null},{"model":"α-ไทย","successes":0,"failures":0,"total_ms":0,"prompt_tokens":0,"completion_tokens":0,"cached_tokens":0}],
            "tools":[{"tool_id":"ไทย-null","invocations":2,"success_rate":null,"future":{"key":false}},{"tool_id":"zero","invocations":3,"success_rate":0.0}],
            "builtins":[{"name":"zeta","calls":4,"failures":1,"avg_ms":0.5,"kinds":{"z":2,"a":1}},{"name":"alpha","calls":0,"failures":0,"avg_ms":0,"kinds":{}}]
        })
    }

    #[test]
    fn metrics_requests_share_one_cutoff_and_json_retains_every_reply_value() {
        for since in [None, Some("2026-01-01T00:00:00+00:00".to_owned())] {
            let expected = replies();
            let mut calls = Vec::new();
            let mut out = Vec::new();
            write_report(&mut out, since.clone(), true, |request| {
                let (key, cutoff) = match request {
                    Request::ListBuiltinStats { since } => ("builtins", since),
                    Request::ListToolMetrics { since } => ("tools", since),
                    Request::ListModelStats { since } => ("models", since),
                    other => panic!("unexpected request {other:?}"),
                };
                assert_eq!(cutoff, since);
                calls.push(key);
                Ok(expected[key].clone())
            })
            .unwrap();
            assert_eq!(calls, ["builtins", "tools", "models"]);
            assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), expected);
        }
    }

    #[test]
    fn human_metrics_preserve_row_order_null_rate_and_returned_figures() {
        let text = human(&replies(), false, false, None).unwrap();
        assert!(text.find("Models").unwrap() < text.find("Tools").unwrap());
        assert!(text.find("Tools").unwrap() < text.find("Built-ins").unwrap());
        assert!(text.find("β-日本語").unwrap() < text.find("α-ไทย").unwrap());
        assert!(text.find("zeta").unwrap() < text.find("alpha").unwrap());
        let null = text
            .lines()
            .find(|line| line.starts_with("ไทย-null"))
            .unwrap();
        let zero = text.lines().find(|line| line.starts_with("zero")).unwrap();
        assert!(null.ends_with('-') && !null.contains("0%"), "{null}");
        assert!(zero.ends_with("0%"), "{zero}");
        assert!(text.contains("1000") && text.contains("0.5") && text.contains("a=1 z=2"));
    }

    #[test]
    fn empty_metrics_and_request_failures_never_print_a_partial_report() {
        let empty = json!({"models":[],"tools":[],"builtins":[]});
        assert_eq!(
            human(&empty, false, false, None)
                .unwrap()
                .matches("none in range")
                .count(),
            3
        );
        for stop in 1..=3 {
            let mut calls = 0;
            let mut out = Vec::new();
            assert!(write_report(&mut out, None, false, |_| {
                calls += 1;
                if calls == stop {
                    bail!("private fixture failure")
                }
                Ok(json!([]))
            })
            .is_err());
            assert_eq!(calls, stop);
            assert!(out.is_empty());
        }
        let mut out = Vec::new();
        write_report(&mut out, None, true, |_| Ok(json!([]))).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), empty);
    }

    #[test]
    fn metrics_since_is_checked_at_parse_time_with_unchanged_duration_spelling() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(cutoff_at("7d", now).unwrap(), "2026-01-01T12:00:00+00:00");
        assert_eq!(
            cutoff_at("1h30m", now).unwrap(),
            "2026-01-08T10:30:00+00:00"
        );
        assert!(matches!(
            Cli::try_parse_from(["formless", "metrics", "--since", "7d", "--json"])
                .unwrap()
                .cmd,
            Some(Cmd::Metrics {
                since: Some(_),
                json: true
            })
        ));
        for raw in [
            "bad",
            "1w",
            "18446744073709551615d",
            "18446744073709551615s1s",
            "18446744073709551615",
            "1000000000d",
        ] {
            assert!(cutoff_at(raw, now).is_err(), "{raw}");
            let error = Cli::try_parse_from(["formless", "metrics", "--since", raw])
                .err()
                .unwrap();
            assert_eq!(error.exit_code(), 2);
        }
        let help = Cli::try_parse_from(["formless", "metrics", "--help"])
            .err()
            .unwrap()
            .to_string();
        assert!(
            help.contains("--json")
                && help.contains("--since")
                && help.contains("independent reads")
        );
    }
}
