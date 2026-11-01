//! Pure rendering helpers: panels, colour scales and compact formatting.
//! Markdown lives in `markdown`. Colours always go through `fg` so `NO_COLOR` holds.

use chrono::{DateTime, Local, Utc};
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders};

use super::{bold_fg, fg};

/// Rounded, titled panel used by every view.
pub(crate) fn panel<'a>(title: impl Into<Line<'a>>, color: bool, border: Color) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(fg(color, border))
        .title(title)
}

/// Bottom-right key hint for a panel.
pub(crate) fn hint(text: &str, color: bool) -> Line<'static> {
    Line::styled(format!(" {text} "), dim(color)).right_aligned()
}

/// Secondary text: grey with colour, terminal-dim without.
pub(crate) fn dim(color: bool) -> Style {
    if color {
        Style::default().fg(super::theme::remap(Color::DarkGray))
    } else {
        Style::default().add_modifier(Modifier::DIM)
    }
}

pub(crate) fn rate_text(rate: Option<f64>) -> String {
    rate.map(|r| format!("{:.0}%", r * 100.0))
        .unwrap_or_else(|| "-".into())
}

/// Green at or above the optimiser trigger (90%), yellow above 70%, red below.
pub(crate) fn rate_style(color: bool, rate: Option<f64>) -> Style {
    match rate {
        None => dim(color),
        Some(r) if r >= 0.9 => fg(color, Color::Green),
        Some(r) if r >= 0.7 => fg(color, Color::Yellow),
        Some(_) => bold_fg(color, Color::Red),
    }
}

pub(crate) fn bar(rate: f64, width: usize) -> String {
    let filled = ((rate.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// Fit `s` into exactly `width` screen columns, with an ellipsis when
/// cut (#785).
pub(crate) fn pad(s: &str, width: usize) -> String {
    let out = crate::text::truncate(s, width);
    let w = crate::text::width(&out);
    format!("{out}{}", " ".repeat(width.saturating_sub(w)))
}

pub(crate) fn relative_time(ts: &str, now: DateTime<Utc>) -> String {
    let Ok(t) = DateTime::parse_from_rfc3339(ts) else {
        return ts.to_string();
    };
    let secs = (now - t.with_timezone(&Utc)).num_seconds().max(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        86_400..=604_799 => format!("{}d ago", secs / 86_400),
        _ => t.with_timezone(&Local).format("%Y-%m-%d").to_string(),
    }
}

/// A message's time (SPEC §10.2): `now`, `2m`, `3h` for today's recent
/// ones, else the local `Tue 14:02` within a week or `3 Mar 14:02`.
pub(crate) fn message_stamp(ts: &str, now: DateTime<Utc>) -> Option<String> {
    let t = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    let secs = (now - t).num_seconds().max(0);
    let local = t.with_timezone(&Local);
    use crate::i18n::{stamp, Stamp};
    Some(match secs {
        0..=59 => stamp(Stamp::Now, local),
        60..=3599 => stamp(Stamp::Minutes(secs / 60), local),
        3600..=43_199 => stamp(Stamp::Hours(secs / 3600), local),
        43_200..=518_399 => stamp(Stamp::Weekday, local),
        _ => stamp(Stamp::Date, local),
    })
}

/// The local calendar day of an RFC 3339 timestamp, for day separators.
pub(crate) fn local_day(ts: &str) -> Option<chrono::NaiveDate> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.with_timezone(&Local).date_naive())
}

/// Local `HH:MM` for an RFC 3339 timestamp.
pub(crate) fn clock_time(ts: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.with_timezone(&Local).format("%H:%M").to_string())
}

/// One-line `key=value` summary of a JSON tool-call argument object.
pub(crate) fn summarize_args(args: &str, max: usize) -> String {
    let flat = |s: &str| super::truncate_chars(s, max);
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(args) else {
        return flat(args);
    };
    // Keys arrive sorted; `name` is the most telling, so it leads.
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by_key(|(k, _)| *k != "name");
    let parts: Vec<String> = entries
        .into_iter()
        .map(|(k, v)| match v {
            serde_json::Value::String(s) => format!("{k}={:?}", super::truncate_chars(s, 40)),
            serde_json::Value::Array(a) => format!("{k}=[{}]", a.len()),
            serde_json::Value::Object(_) => format!("{k}={{…}}"),
            other => format!("{k}={other}"),
        })
        .collect();
    flat(&parts.join("  "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_helpers() {
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(pad("abcdef", 4), "abc…");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(1.7, 3), "███");
        assert_eq!(rate_text(Some(0.904)), "90%");
        assert_eq!(rate_style(true, Some(0.95)).fg, Some(Color::Green));
        assert_eq!(rate_style(true, Some(0.5)).fg, Some(Color::Red));
        assert_eq!(rate_style(false, Some(0.5)).fg, None);
        let now = DateTime::parse_from_rfc3339("2026-10-21T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(relative_time("2026-10-21T11:59:30Z", now), "just now");
        assert_eq!(relative_time("2026-10-21T11:15:00Z", now), "45m ago");
        assert_eq!(relative_time("2026-10-21T09:00:00Z", now), "3h ago");
        assert_eq!(relative_time("2026-10-19T12:00:00Z", now), "2d ago");
        assert_eq!(relative_time("garbage", now), "garbage");
        assert_eq!(
            summarize_args(r#"{"name":"page_title","code":"x","tags":[1,2]}"#, 200),
            r#"name="page_title"  code="x"  tags=[2]"#
        );
        assert_eq!(summarize_args("not json", 200), "not json");
    }
}

/// What a Capability spec lets a Tool do, in plain words (SPEC §7.2), for
/// the approval dialog's `?`.
pub(crate) fn explain_capability(spec: &str) -> String {
    let (kind, scope) = spec.split_once(':').unwrap_or((spec, ""));
    let input = |s: &str| s.strip_prefix("$input.").map(str::to_string);
    match (kind, scope) {
        ("net", "") => "can reach any host on the network".into(),
        ("net", s) => match input(s) {
            Some(f) => format!("can reach the host named in its `{f}` input, per call"),
            None => format!("can reach {s} over the network"),
        },
        ("fs", s) => {
            let (mode, path) = s.split_once(':').unwrap_or((s, ""));
            if mode == "read-sensitive" {
                return format!(
                    "can read credential data at exactly {path}. {}",
                    formless_protocol::sensitive_read_warning(spec).unwrap_or_default()
                );
            }
            let what = if mode == "write" {
                "can create, overwrite or delete files"
            } else {
                "can read files"
            };
            match input(path) {
                Some(f) => format!("{what} at the path in its `{f}` input, per call"),
                None if path.is_empty() => format!("{what} anywhere"),
                None => format!("{what} under {path}"),
            }
        }
        ("env", k) => format!("can read the environment variable {k}"),
        ("process", "") => "can run any program on this machine".into(),
        ("process", cmd) => format!("can run the program `{cmd}`"),
        ("clipboard", "read") => "can read your clipboard".into(),
        ("clipboard", _) => "can write to your clipboard".into(),
        ("notify", _) => "can show desktop notifications".into(),
        ("store", "") => "can keep its own small saved state".into(),
        ("store", ns) => format!("can read and change the shared store `{ns}`"),
        ("harness", "read") => {
            "can see your setup (providers, MCP servers, skills, settings; never keys)".into()
        }
        ("harness", area) => {
            format!("can propose changes to your {area} setup (each one asks you)")
        }
        ("llm", "") => "can make model calls (costs tokens)".into(),
        ("llm", m) => format!("can make model calls to {m} (costs tokens)"),
        ("agent", "") => "can start sub-agents with the same Tools (costs tokens)".into(),
        ("agent", m) => format!("can start sub-agents on {m} (costs tokens)"),
        _ => "a capability this client doesn't know how to describe".into(),
    }
}

#[cfg(test)]
mod capability_tests {
    use super::explain_capability as e;

    #[test]
    fn explains_capabilities() {
        assert_eq!(
            e("fs:write:/Users/x/out"),
            "can create, overwrite or delete files under /Users/x/out"
        );
        assert_eq!(
            e("fs:read:$input.path"),
            "can read files at the path in its `path` input, per call"
        );
        assert_eq!(e("net"), "can reach any host on the network");
        assert_eq!(
            e("net:example.com"),
            "can reach example.com over the network"
        );
        assert_eq!(e("process:git"), "can run the program `git`");
        assert!(e("harness:mcp").contains("each one asks you"));
        assert!(e("weird:thing").contains("doesn't know"));
    }

    #[test]
    fn credential_reads_are_exact_instead_of_directory_grants() {
        let explanation = e("fs:read-sensitive:/private/project/token.json");
        assert!(explanation.contains("at exactly /private/project/token.json"));
        assert!(explanation.contains("not descendants"));
        assert!(explanation.contains("may flow to the model provider"));
        assert!(!explanation.contains("under "));
    }
}
