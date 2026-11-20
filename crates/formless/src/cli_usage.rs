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
        c(cost.map_or_else(|| "-".into(), |amount| amount.to_string())),
    ]
}

fn human(value: &Value, terminal: bool, color: bool, width: Option<usize>) -> Result<String> {
    let rows = value
        .as_array()
        .context("grouped usage response was not an array")?;
    if rows.is_empty() {
        return Ok("No usage in range.\n".into());
    }
    let mut table = Table::new(&[
        "KEY",
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
        table.row(usage_cells(key, counts, cache_write, cost));
    }
    table.row(usage_cells("TOTAL", totals, total_cache_write, total_cost));
    Ok(table.render(terminal, color, width))
}

pub(super) fn write_report(
    out: &mut impl Write,
    by: Group,
    since: Option<String>,
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
) -> Result<()> {
    let value = request(Request::UsageByDay {
        since,
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
            human(&value, terminal, terminal && table::color_allowed(), width)?
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            {"key":"日本語","prompt_tokens":1,"completion_tokens":2,"cached_tokens":0,"cache_write_tokens":1,"cost":null,"future":7},
            {"key":"ไทย","prompt_tokens":100,"completion_tokens":200,"cached_tokens":3,"cache_write_tokens":4,"cost":0.125}
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
                assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), value);
            } else {
                let text = String::from_utf8(out).unwrap();
                assert!(text.find("日本語").unwrap() < text.find("ไทย").unwrap());
                assert!(text.contains('-') && text.contains("0.125"));
                assert_eq!(
                    text.lines()
                        .last()
                        .unwrap()
                        .split_whitespace()
                        .collect::<Vec<_>>(),
                    ["TOTAL", "101", "202", "3", "5", "303", "-"]
                );
            }
        }
        let narrow = human(&value, true, false, Some(30)).unwrap();
        assert!(!narrow.is_empty());
        assert!(human(&serde_json::json!([]), false, false, None)
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
        let rendered = human(&priced, false, false, None).unwrap();
        assert_eq!(
            rendered.lines().last().unwrap().split_whitespace().last(),
            Some("0.375")
        );
    }
}
