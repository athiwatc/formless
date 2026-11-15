//! The existing default-provider picker report, including in-band listing errors.
use super::*;

#[derive(Subcommand)]
pub(super) enum Action {
    /// Fetch the default provider's model catalog and existing picker metadata
    #[command(
        after_help = "Examples:\n  formless models list\n  formless models list --json\n\nUses the default provider endpoint and refreshes its existing context-window cache. Current means configured default model/effort. Recent IDs come from Sessions; tests are remembered results, not new probes. A non-null listing error is printed with the partial report, then exits 1; other RPC errors follow normal CLI handling. The handler's 15-second timeout covers the listing call, not all command work."
    )]
    List {
        /// Print the complete existing reply, including unknown fields and error
        #[arg(long)]
        json: bool,
    },
}

pub(super) fn run(paths: &Paths, action: Action) -> Result<i32> {
    let Action::List { json } = action;
    formless_core::daemon::ensure_daemon(paths)?;
    let value = formless_tui::client::request_once(paths, Request::ListModels)?;
    let (text, code) = report(&value, json)?;
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())?;
    out.flush()?;
    Ok(code)
}
fn shown(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "-".into(),
        Some(value) => comparison_terminal_text(&value.to_string()),
    }
}
fn string(value: &Value) -> Result<&str> {
    value
        .as_str()
        .context("model catalog contains a non-string id")
}
fn report(value: &Value, json: bool) -> Result<(String, i32)> {
    use std::fmt::Write as _;
    let failed = value.get("error").is_some_and(|error| !error.is_null());
    let code = if failed { 1 } else { 0 };
    if json {
        return Ok((format!("{}\n", serde_json::to_string_pretty(value)?), code));
    }
    let models = value["models"]
        .as_array()
        .context("model catalog response has no models array")?;
    let mut out=format!("Default-provider model catalog\nConfigured default model: {}\nConfigured default effort: {}\n",shown(value["current"].get("model")),shown(value["current"].get("reasoning_effort")));
    if failed {
        writeln!(
            out,
            "Endpoint listing error (partial report): {}",
            shown(value.get("error"))
        )?;
    }
    out.push_str(
        "Endpoint models (reply order; metadata is returned data, tests are historical):\n",
    );
    if models.is_empty() {
        if failed {
            out.push_str("  (no endpoint models available in this partial report)\n");
        } else {
            out.push_str("  (endpoint listed no models)\n");
        }
    }
    for model in models {
        let id = string(model)?;
        writeln!(out, "  {}", comparison_terminal_text(id))?;
        writeln!(
            out,
            "    Returned metadata: {}",
            shown(value["meta"].get(id))
        )?;
        writeln!(
            out,
            "    Remembered test: {}",
            shown(value["tests"].get(id))
        )?;
    }
    for (key, label) in [
        ("recent", "Recent Session-model IDs (reply order)"),
        ("missing", "Missing IDs (daemon report)"),
    ] {
        writeln!(out, "{label}:")?;
        if let Some(ids) = value.get(key).and_then(Value::as_array) {
            if ids.is_empty() {
                out.push_str("  (none)\n");
            }
            for id in ids {
                writeln!(out, "  {}", comparison_terminal_text(string(id)?))?;
            }
        } else {
            out.push_str("  -\n");
        }
    }
    out.push_str("Alias mappings (as returned):\n");
    if let Some(aliases) = value.get("aliases").and_then(Value::as_object) {
        if aliases.is_empty() {
            out.push_str("  (none)\n");
        }
        for (name, target) in aliases {
            writeln!(
                out,
                "  {} → {}",
                comparison_terminal_text(name),
                shown(Some(target))
            )?;
        }
    } else {
        out.push_str("  -\n");
    }
    out.push_str("Roles (as returned):\n");
    if let Some(roles) = value.get("roles").and_then(Value::as_array) {
        if roles.is_empty() {
            out.push_str("  (none)\n");
        }
        for role in roles {
            writeln!(
                out,
                "  Name: {} · Provider: {} · Model: {}",
                shown(role.get("name")),
                shown(role.get("provider")),
                shown(role.get("model"))
            )?;
        }
    } else {
        out.push_str("  -\n");
    }
    Ok((out, code))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        json!({"models":["z-ไทย","a"],"meta":{"z-ไทย":{"context_window":123,"future":"returned"}},"tests":{"a":{"ok":false,"error":"historical"}},"recent":["not-on-endpoint","z-ไทย"],"missing":["missing-only"],"aliases":{"friendly":"not-on-endpoint"},"roles":[{"name":"บทบาท","provider":"named","model":"friendly"}],"current":{"model":"friendly","reasoning_effort":null},"error":null,"future":[true,null]})
    }
    #[test]
    fn model_report_preserves_all_sections_order_and_only_reported_metadata() {
        let value = fixture();
        let (text, code) = report(&value, false).unwrap();
        assert_eq!(code, 0);
        assert!(text.find("  z-ไทย\n").unwrap() < text.find("  a\n").unwrap());
        for term in [
            "not-on-endpoint",
            "missing-only",
            "friendly",
            "บทบาท",
            "named",
            "historical",
            "123",
            "future",
        ] {
            assert!(text.contains(term), "{term}: {text}");
        }
        assert!(text.contains("Configured default effort: -"));
        assert!(text.contains("Returned metadata: -") && text.contains("Remembered test: -"));
        assert!(!text.contains("verified"));
        let (text, code) = report(&value, true).unwrap();
        assert_eq!(code, 0);
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), value);
    }
    #[test]
    fn model_in_band_error_prints_one_partial_report_and_exits_one_in_both_modes() {
        let mut value = fixture();
        value["models"] = json!([]);
        value["error"] = json!("fixture listing failure");
        let (text, code) = report(&value, false).unwrap();
        assert_eq!(code, 1);
        assert_eq!(text.matches("fixture listing failure").count(), 1);
        assert!(text.contains("partial report") && text.contains("not-on-endpoint"));
        let (text, code) = report(&value, true).unwrap();
        assert_eq!(code, 1);
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), value);
        value["error"] = json!(false);
        assert_eq!(report(&value, true).unwrap().1, 1);
    }
    #[test]
    fn empty_endpoint_without_error_is_success_and_controls_are_visible() {
        let mut value = fixture();
        value["models"] = json!([]);
        let (text, code) = report(&value, false).unwrap();
        assert_eq!(code, 0);
        assert!(text.contains("endpoint listed no models"));
        value["models"] = json!(["bad\u{1b}[2Jไทย"]);
        let (text, _) = report(&value, false).unwrap();
        assert!(!text.contains('\u{1b}'));
        assert!(text.contains("\\u{1b}") && text.contains("ไทย"));
    }
}
