//! Every setting a client shows or can set is described in SPEC §13, the
//! one source for the help text both Settings views show.

use formless_core::config::{setting_docs, setting_docs_json, SETTABLE};

#[test]
fn every_shown_or_settable_key_is_documented() {
    let docs = setting_docs();
    // The TUI and web Settings rows.
    let shown = [
        "provider.base_url",
        "provider.model",
        "provider.reasoning_effort",
        "provider.api_style",
        "provider.tool_mode",
        "provider.context_tokens",
        "provider.max_retries",
        "provider.stall_notice_secs",
        "provider.tool_use_nudge",
        "provider.fallback_models",
        "tui.multi_pane",
        "tui.theme",
        "tui.inline_images",
        "tui.code_line_numbers",
        "tui.timestamps",
        "tui.notify",
        "tui.notify_min_secs",
        "tui.on_start",
        "tui.screen_reader",
        "limits.daily_usd",
        "limits.run_usd",
        "limits.run_steps",
        "naming.enabled",
        "daemon.idle_grace_secs",
        "relay.enabled",
        "relay.url",
    ];
    let exact = SETTABLE.iter().filter(|k| !k.ends_with('.'));
    let missing: Vec<&str> = shown
        .iter()
        .copied()
        .chain(exact.copied())
        .filter(|k| !docs.contains_key(*k))
        .collect();
    assert!(missing.is_empty(), "add these to SPEC §13: {missing:?}");
    let (meaning, _) = &docs["daemon.log_keep"];
    assert!(
        meaning.contains("rotation") && !meaning.contains('§'),
        "{meaning}"
    );
}

#[test]
fn defaults_come_from_the_config_types() {
    let docs = setting_docs_json();
    assert_eq!(docs["provider.max_retries"]["default"], 5);
    assert_eq!(docs["tui.notify_min_secs"]["default"], 20);
    // Unset by default: SPEC's words.
    assert_eq!(docs["provider.model"]["default"], "— (wizard)");
    assert!(docs["tools.max_tool_depth"]["description"]
        .as_str()
        .unwrap()
        .contains("nest"));
}

/// Every key `Config::default()` has (Option fields included, as nulls)
/// is in SPEC §13, apart from a reviewed list of file-only blocks.
#[test]
fn every_config_key_is_documented() {
    fn flatten(prefix: &str, v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(m)
                if !m.is_empty() && !prefix.is_empty() || prefix.is_empty() =>
            {
                for (k, v) in m {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}.{k}")
                    };
                    flatten(&key, v, out);
                }
            }
            _ => out.push(prefix.to_string()),
        }
    }
    let defaults = serde_json::to_value(formless_core::Config::default()).unwrap();
    let mut keys = Vec::new();
    flatten("", &defaults, &mut keys);
    // File-only blocks: credentials and their mechanics, maps keyed by
    // user-chosen names, and lists of user-written entries.
    let allowed = |k: &str| {
        [
            "provider.auth.",
            "provider.extra_headers",
            "mcp.servers",
            "hooks",
            "providers",
            "roles",
        ]
        .iter()
        .any(|p| k == p.trim_end_matches('.') || k.starts_with(p))
    };
    let docs = setting_docs();
    let missing: Vec<&String> = keys
        .iter()
        .filter(|k| !allowed(k))
        .filter(|k| !docs.contains_key(k.as_str()))
        .collect();
    assert!(missing.is_empty(), "add these to SPEC §13: {missing:?}");
}
