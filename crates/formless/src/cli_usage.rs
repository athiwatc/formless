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
        "COST USD",
    ]);
    for row in rows {
        let mut cells = vec![c(row["key"].as_str().context("usage key was not text")?)];
        for key in ["prompt_tokens", "completion_tokens", "cached_tokens"] {
            cells.push(c(row[key]
                .as_i64()
                .with_context(|| format!("usage {key} was not an integer"))?
                .to_string()));
        }
        cells.push(c(if row["cost"].is_null() {
            "-".into()
        } else {
            row["cost"]
                .as_f64()
                .context("usage cost was not numeric or null")?
                .to_string()
        }));
        table.row(cells);
    }
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
            {"key":"日本語","prompt_tokens":1,"completion_tokens":2,"cached_tokens":0,"cost":null,"future":7},
            {"key":"ไทย","prompt_tokens":100,"completion_tokens":200,"cached_tokens":3,"cost":0.125}
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
    }
}
