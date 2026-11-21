//! Grouped full-range usage from the existing daemon report.
use super::*;
use std::io::IsTerminal;

#[derive(Clone, Copy, clap::ValueEnum)]
pub(super) enum Group {
    Day,
    Model,
    Provider,
    Purpose,
    Role,
    Session,
}
impl Group {
    fn key(self) -> Option<String> {
        match self {
            Self::Day => None,
            Self::Model => Some("model"),
            Self::Provider => Some("provider"),
            Self::Purpose => Some("purpose"),
            Self::Role => Some("role"),
            Self::Session => Some("session"),
        }
        .map(str::to_owned)
    }
}

fn partial_utc_day(since: Option<&str>) -> Option<String> {
    let cutoff = chrono::DateTime::parse_from_rfc3339(since?).ok()?.to_utc();
    (cutoff.time() != chrono::NaiveTime::MIN).then(|| cutoff.date_naive().to_string())
}

fn human(
    value: &Value,
    by: Group,
    since: Option<&str>,
    terminal: bool,
    color: bool,
    width: Option<usize>,
) -> Result<String> {
    let rows = value
        .as_array()
        .context("grouped usage response was not an array")?;
    if rows.is_empty() {
        return Ok("No usage in range.\n".into());
    }
    let daily = matches!(by, Group::Day);
    let partial = if daily {
        partial_utc_day(since).filter(|day| rows.iter().any(|row| row["key"].as_str() == Some(day)))
    } else {
        None
    };
    let mut table = Table::new(&[
        if daily { "DAY (UTC)" } else { "KEY" },
        "PROMPT TOKENS",
        "COMPLETION TOKENS",
        "CACHED TOKENS",
        "COST USD",
    ]);
    for row in rows {
        let key = row["key"].as_str().context("usage key was not text")?;
        let mut cells = vec![c(if partial.as_deref() == Some(key) {
            format!("{key} (partial)")
        } else {
            key.into()
        })];
        for key in ["prompt_tokens", "completion_tokens", "cached_tokens"] {
            cells.push(c(row[key]
                .as_i64()
                .with_context(|| format!("usage {key} was not an integer"))?
                .to_string()));
        }
        cells.push(c(if row["cost"].is_null() {
            "-".into()
        } else {
            let cost = row["cost"]
                .as_f64()
                .context("usage cost was not numeric or null")?;
            format!("${cost:.6}")
        }));
        table.row(cells);
    }
    let mut report = table.render(terminal, color, width);
    if terminal {
        if let Some(day) = partial {
            // A fitted table can clip the marker at ordinary terminal widths.
            report.push_str(&format!("Partial UTC day {day}\n"));
        }
    }
    Ok(report)
}

pub(super) fn write_report(
    out: &mut impl Write,
    by: Group,
    since: Option<String>,
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
) -> Result<()> {
    let value = request(Request::UsageByDay {
        since: since.clone(),
        by: by.key(),
    })?;
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&value)?)?;
    } else {
        let terminal = std::io::stdout().is_terminal();
        let width = terminal
            .then(|| {
                crossterm::terminal::size()
                    .ok()
                    .map(|(width, _)| width as usize)
            })
            .flatten();
        write!(
            out,
            "{}",
            human(
                &value,
                by,
                since.as_deref(),
                terminal,
                terminal && table::color_allowed(),
                width
            )?
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn human_uses_utc_days_and_marks_only_a_present_truncated_cutoff_day() {
        let rows = serde_json::json!([
            {"key":"2026-11-16","prompt_tokens":1,"completion_tokens":0,"cached_tokens":0,"cost":null},
            {"key":"2026-11-17","prompt_tokens":2,"completion_tokens":0,"cached_tokens":0,"cost":null}
        ]);
        let render = |group, since| human(&rows, group, since, false, false, None).unwrap();
        let truncated = render(Group::Day, Some("2026-11-17T03:00:00+07:00"));
        assert!(truncated.contains("DAY (UTC)"));
        assert!(truncated.contains("2026-11-16 (partial)"));
        assert!(!truncated.contains("2026-11-17 (partial)"));
        let next = render(Group::Day, Some("2026-11-16T20:00:00-05:00"));
        assert!(next.contains("2026-11-17 (partial)"));
        for since in [
            None,
            Some("2026-11-16T07:00:00+07:00"),
            Some("2026-11-16T19:00:00-05:00"),
            Some("2026-11-15T20:00:00Z"),
        ] {
            let whole = render(Group::Day, since);
            assert!(whole.contains("DAY (UTC)"));
            assert!(!whole.contains("(partial)"));
        }
        let grouped = render(Group::Model, Some("2026-11-16T20:00:00Z"));
        assert!(grouped.contains("KEY"));
        assert!(!grouped.contains("DAY (UTC)") && !grouped.contains("(partial)"));
    }

    #[test]
    fn partial_day_note_survives_terminal_column_fitting() {
        let rows = serde_json::json!([
            {"key":"2026-11-16","prompt_tokens":1,"completion_tokens":0,"cached_tokens":0,"cost":0.12345678901234567},
            {"key":"2026-11-17","prompt_tokens":2,"completion_tokens":0,"cached_tokens":0,"cost":null}
        ]);
        for width in [65, 72, 80] {
            let rendered = human(
                &rows,
                Group::Day,
                Some("2026-11-16T20:00:00Z"),
                true,
                false,
                Some(width),
            )
            .unwrap();
            assert!(
                rendered.ends_with("Partial UTC day 2026-11-16\n"),
                "{rendered}"
            );
            let missing = human(
                &rows,
                Group::Day,
                Some("2026-11-15T20:00:00Z"),
                true,
                false,
                Some(width),
            )
            .unwrap();
            assert!(!missing.contains("Partial UTC day"), "{missing}");
        }
    }

    #[test]
    fn grouping_parser_maps_default_and_day_to_none_and_checks_since_before_dispatch() {
        for (flag, expected) in [
            (None, None),
            (Some("day"), None),
            (Some("model"), Some("model")),
            (Some("provider"), Some("provider")),
            (Some("purpose"), Some("purpose")),
            (Some("role"), Some("role")),
            (Some("session"), Some("session")),
        ] {
            let mut args = vec!["formless", "usage", "--json"];
            if let Some(flag) = flag {
                args.extend(["--by", flag]);
            }
            let cli = Cli::try_parse_from(args).unwrap();
            let Some(Cmd::Usage { by, since, json }) = cli.cmd else {
                panic!("wrong command")
            };
            assert_eq!(by.key().as_deref(), expected);
            assert!(since.is_none() && json);
        }
        assert!(Cli::try_parse_from(["formless", "usage", "--by", "typo"]).is_err());
        for value in [
            "bad",
            "18446744073709551615d",
            "18446744073709551615s1s",
            "18446744073709551615",
            "9223372036854775s",
        ] {
            assert!(
                Cli::try_parse_from(["formless", "usage", "--since", value]).is_err(),
                "{value}"
            );
        }
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-13T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(cutoff_at("30d", now).unwrap(), "2026-10-14T12:00:00+00:00");
    }

    #[test]
    fn one_request_preserves_cutoff_order_values_unknown_cost_and_json_fields() {
        let value = serde_json::json!([
            {"key":"日本語","prompt_tokens":1,"completion_tokens":2,"cached_tokens":0,"cost":null,"future":7},
            {"key":"ไทย","prompt_tokens":100,"completion_tokens":200,"cached_tokens":3,"cost":0.125},
            {"key":"sum","prompt_tokens":30,"completion_tokens":20,"cached_tokens":0,"cost":0.1 + 0.2},
            {"key":"tiny","prompt_tokens":1,"completion_tokens":1,"cached_tokens":0,"cost":0.000123456},
            {"key":"zero","prompt_tokens":1,"completion_tokens":0,"cached_tokens":0,"cost":0.0}
        ]);
        for json in [false, true] {
            let mut out = Vec::new();
            let mut calls = Vec::new();
            write_report(
                &mut out,
                Group::Day,
                Some("2026-10-14T12:00:00+00:00".into()),
                json,
                |request| {
                    calls.push(request);
                    Ok(value.clone())
                },
            )
            .unwrap();
            assert!(
                matches!(&calls[..],[Request::UsageByDay{since:Some(since),by:None}] if since=="2026-10-14T12:00:00+00:00")
            );
            if json {
                let unformatted = serde_json::to_string_pretty(&value).unwrap() + "\n";
                assert_eq!(out, unformatted.into_bytes());
            } else {
                let text = String::from_utf8(out).unwrap();
                assert!(text.find("日本語").unwrap() < text.find("ไทย").unwrap());
                for (key, cost) in [
                    ("日本語", "-"),
                    ("ไทย", "$0.125000"),
                    ("sum", "$0.300000"),
                    ("tiny", "$0.000123"),
                    ("zero", "$0.000000"),
                ] {
                    let row = text.lines().find(|line| line.starts_with(key)).unwrap();
                    assert_eq!(row.split_whitespace().last(), Some(cost), "{row}");
                }
                assert!(!text.contains("0.30000000000000004"));
            }
        }
        let narrow = human(&value, Group::Day, None, true, false, Some(30)).unwrap();
        assert!(!narrow.is_empty());
        assert!(human(
            &serde_json::json!([]),
            Group::Day,
            Some("2026-11-16T20:00:00Z"),
            false,
            false,
            None
        )
        .unwrap()
        .contains("No usage in range"));
        let mut out = Vec::new();
        assert!(write_report(&mut out, Group::Model, None, false, |_| Err(
            anyhow::anyhow!("fixture failure")
        ))
        .is_err());
        assert!(out.is_empty());
    }
}
