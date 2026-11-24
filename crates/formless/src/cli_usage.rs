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

fn usage_cells(
    key: &str,
    [prompt, completion, cached]: [i128; 3],
    cache_write: Option<i128>,
    cost: Option<f64>,
) -> Vec<(String, Tone)> {
    vec![
        c(key),
        c(prompt.to_string()),
        c(completion.to_string()),
        c(cached.to_string()),
        c(cache_write.map_or_else(|| "-".into(), |count| count.to_string())),
        c((prompt + completion).to_string()),
        c(cost.map_or_else(|| "-".into(), |amount| format!("${amount:.6}"))),
    ]
}

fn local_utc_offset_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

fn offset_label(minutes: i32) -> String {
    let sign = if minutes >= 0 { '+' } else { '-' };
    let magnitude = minutes.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", magnitude / 60, magnitude % 60)
}

fn partial_local_day(since: Option<&str>, utc_offset_minutes: i32) -> Option<String> {
    let cutoff = chrono::DateTime::parse_from_rfc3339(since?).ok()?.to_utc();
    let local_cutoff = cutoff.checked_add_signed(chrono::Duration::minutes(i64::from(utc_offset_minutes)))?;
    (local_cutoff.time() != chrono::NaiveTime::MIN)
        .then(|| local_cutoff.date_naive().to_string())
}

fn human(
    value: &Value,
    by: Group,
    since: Option<&str>,
    utc_offset_minutes: Option<i32>,
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
    let offset = utc_offset_minutes.unwrap_or_default();
    let partial = if daily {
        partial_local_day(since, offset)
            .filter(|day| rows.iter().any(|row| row["key"].as_str() == Some(day)))
    } else {
        None
    };
    let day_header = format!("DAY (LOCAL, {})", offset_label(offset));
    let mut table = Table::new(&[
        if daily { &day_header } else { "KEY" },
        "PROMPT TOKENS",
        "COMPLETION TOKENS",
        "CACHED TOKENS",
        "CACHE WRITE",
        "TOTAL TOKENS",
        "COST USD",
    ]);
    let mut totals = [0; 3];
    let mut total_cache_write = Some(0);
    let mut total_cost = Some(0.0);
    for row in rows {
        let key = row["key"].as_str().context("usage key was not text")?;
        let key = if partial.as_deref() == Some(key) {
            format!("{key} (partial)")
        } else {
            key.into()
        };
        let mut counts = [0; 3];
        for (count, field) in
            counts
                .iter_mut()
                .zip(["prompt_tokens", "completion_tokens", "cached_tokens"])
        {
            *count = i128::from(
                row[field]
                    .as_i64()
                    .with_context(|| format!("usage {field} was not an integer"))?,
            );
        }
        let cache_write = if row["cache_write_tokens"].is_null() {
            None
        } else {
            Some(i128::from(
                row["cache_write_tokens"]
                    .as_i64()
                    .context("usage cache_write_tokens was not an integer")?,
            ))
        };
        let cost = if row["cost"].is_null() {
            None
        } else {
            Some(
                row["cost"]
                    .as_f64()
                    .context("usage cost was not numeric or null")?,
            )
        };
        for (total, count) in totals.iter_mut().zip(counts) {
            *total += count;
        }
        total_cache_write = total_cache_write
            .zip(cache_write)
            .map(|(sum, value)| sum + value);
        total_cost = total_cost.zip(cost).map(|(sum, value)| sum + value);
        table.row(usage_cells(&key, counts, cache_write, cost));
    }
    table.row(usage_cells("TOTAL", totals, total_cache_write, total_cost));
    let mut report = table.render(terminal, color, width);
    if terminal {
        if let Some(day) = partial {
            // A fitted table can clip the marker at ordinary terminal widths.
            report.push_str(&format!("Partial local day {day} ({})\n", offset_label(offset)));
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
    let utc_offset_minutes = matches!(by, Group::Day).then(local_utc_offset_minutes);
    let value = request(Request::UsageByDay {
        since: since.clone(),
        by: by.key(),
        utc_offset_minutes,
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
                utc_offset_minutes,
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
    fn human_uses_local_days_and_marks_only_a_present_truncated_cutoff_day() {
        let rows = serde_json::json!([
            {"key":"2026-11-16","prompt_tokens":1,"completion_tokens":0,"cached_tokens":0,"cost":null},
            {"key":"2026-11-17","prompt_tokens":2,"completion_tokens":0,"cached_tokens":0,"cost":null}
        ]);
        let render = |group, since| {
            human(&rows, group, since, Some(420), false, false, None).unwrap()
        };
        let truncated = render(Group::Day, Some("2026-11-17T03:00:00+07:00"));
        assert!(truncated.contains("DAY (LOCAL, UTC+07:00)"));
        assert!(truncated.contains("2026-11-17 (partial)"));
        assert!(!truncated.contains("2026-11-16 (partial)"));
        let next = render(Group::Day, Some("2026-11-16T20:00:00-05:00"));
        assert!(next.contains("2026-11-17 (partial)"));
        for since in [
            None,
            Some("2026-11-15T17:00:00Z"),
            Some("2026-11-16T17:00:00Z"),
            Some("2026-11-18T17:00:00Z"),
        ] {
            let whole = render(Group::Day, since);
            assert!(whole.contains("DAY (LOCAL, UTC+07:00)"));
            assert!(!whole.contains("(partial)"));
        }
        let grouped = render(Group::Model, Some("2026-11-16T20:00:00Z"));
        assert!(grouped.contains("KEY"));
        assert!(!grouped.contains("DAY (LOCAL") && !grouped.contains("(partial)"));
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
                Some(420),
                true,
                false,
                Some(width),
            )
            .unwrap();
            assert!(
                rendered.ends_with("Partial local day 2026-11-17 (UTC+07:00)\n"),
                "{rendered}"
            );
            let missing = human(
                &rows,
                Group::Day,
                Some("2026-11-17T20:00:00Z"),
                Some(420),
                true,
                false,
                Some(width),
            )
            .unwrap();
            assert!(!missing.contains("Partial local day"), "{missing}");
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
        let expected_offset = local_utc_offset_minutes();
        let value = serde_json::json!([
            {"key":"日本語","prompt_tokens":1,"completion_tokens":2,"cached_tokens":0,"cache_write_tokens":1,"cost":null,"future":7},
            {"key":"ไทย","prompt_tokens":100,"completion_tokens":200,"cached_tokens":3,"cache_write_tokens":4,"cost":0.125},
            {"key":"sum","prompt_tokens":30,"completion_tokens":20,"cached_tokens":0,"cache_write_tokens":0,"cost":0.1 + 0.2},
            {"key":"tiny","prompt_tokens":1,"completion_tokens":1,"cached_tokens":0,"cache_write_tokens":0,"cost":0.000123456},
            {"key":"zero","prompt_tokens":1,"completion_tokens":0,"cached_tokens":0,"cache_write_tokens":0,"cost":0.0}
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
                matches!(&calls[..],[Request::UsageByDay{since:Some(since),by:None,utc_offset_minutes:Some(offset)}] if since=="2026-10-14T12:00:00+00:00" && *offset==expected_offset)
            );
            if json {
                let unformatted = serde_json::to_string_pretty(&value).unwrap() + "\n";
                assert_eq!(out, unformatted.into_bytes());
            } else {
                let text = String::from_utf8(out).unwrap();
                assert!(text.find("日本語").unwrap() < text.find("ไทย").unwrap());
                assert_eq!(
                    text.lines()
                        .last()
                        .unwrap()
                        .split_whitespace()
                        .collect::<Vec<_>>(),
                    ["TOTAL", "133", "223", "3", "5", "356", "-"]
                );
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
        let narrow = human(&value, Group::Day, None, Some(420), true, false, Some(30)).unwrap();
        assert!(!narrow.is_empty());
        assert!(human(
            &serde_json::json!([]),
            Group::Day,
            Some("2026-11-16T20:00:00Z"),
            Some(420),
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

        let mut priced = value;
        priced[0]["cost"] = serde_json::json!(0.25);
        let rendered = human(&priced, Group::Day, None, Some(420), false, false, None).unwrap();
        assert_eq!(
            rendered.lines().last().unwrap().split_whitespace().last(),
            Some("$0.675123")
        );
    }
}
