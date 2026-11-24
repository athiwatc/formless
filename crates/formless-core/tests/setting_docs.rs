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
        "naming.auto_download",
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
    assert_eq!(docs["naming.auto_download"]["default"], false);
    // Unset by default: SPEC's words.
    assert_eq!(docs["provider.model"]["default"], "— (wizard)");
    assert!(docs["tools.max_tool_depth"]["description"]
        .as_str()
        .unwrap()
        .contains("nest"));
}

/// File-only blocks: credentials and their mechanics, maps keyed by
/// user-chosen names, and lists of user-written entries. SPEC §13's prose
/// names the same list (checked below).
const FILE_ONLY: &[&str] = &[
    "provider.auth",
    "provider.extra_headers",
    "mcp.servers",
    "hooks",
    "providers",
    "roles",
];

/// On a key boundary: `hooks` covers `hooks[].event`, not `hooks_extra`.
fn file_only(key: &str) -> bool {
    FILE_ONLY.iter().any(|p| {
        key == *p
            || key
                .strip_prefix(p)
                .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
    })
}

/// Every key of a configuration with every option set and one entry in
/// every map and list (`*` for a user-chosen map key, `[]` for a list
/// entry), so keys nested inside them are seen too (#851).
fn shape_keys() -> Vec<String> {
    fn flatten(prefix: &str, v: &serde_json::Value, out: &mut Vec<String>) {
        let join = |k: &str| {
            if prefix.is_empty() {
                k.to_string()
            } else {
                format!("{prefix}.{k}")
            }
        };
        match v {
            serde_json::Value::Object(m) => {
                assert!(
                    !m.is_empty(),
                    "{prefix} is empty in config::documented_shape(): give it an entry"
                );
                for (k, v) in m {
                    flatten(&join(k), v, out);
                }
            }
            serde_json::Value::Array(a) => {
                let first = a.first().unwrap_or_else(|| {
                    panic!("{prefix} is empty in config::documented_shape(): give it an entry")
                });
                if first.is_object() {
                    flatten(&format!("{prefix}[]"), first, out);
                } else {
                    out.push(prefix.to_string());
                }
            }
            serde_json::Value::Null => {
                panic!("{prefix} is unset in config::documented_shape(): set it")
            }
            _ => out.push(prefix.to_string()),
        }
    }
    let shape = serde_json::to_value(formless_core::config::documented_shape()).unwrap();
    let mut keys = Vec::new();
    flatten("", &shape, &mut keys);
    keys
}

/// Every key the configuration can hold is in SPEC §13, apart from the
/// file-only blocks; a map of structs is documented by its row naming
/// each field.
#[test]
fn every_config_key_is_documented() {
    let docs = setting_docs();
    let keys = shape_keys();
    let mut missing = Vec::new();
    for key in keys.iter().filter(|k| !file_only(k)) {
        if docs.contains_key(key.as_str()) {
            continue;
        }
        // `provider.aliases.*`: a map of plain values, documented as a whole.
        if let Some(map) = key.strip_suffix(".*") {
            if docs.contains_key(map) {
                continue;
            }
        }
        // `provider.prices.*.input`: the map's row names the field.
        if let Some((map, field)) = key.split_once(".*.") {
            if docs
                .get(map)
                .is_some_and(|(meaning, _)| meaning.contains(field))
            {
                continue;
            }
        }
        missing.push(key.clone());
    }
    assert!(missing.is_empty(), "add these to SPEC §13: {missing:?}");

    // The other way: no row for a key that no longer exists.
    let exists = |doc: &str| {
        keys.iter().any(|k| {
            k == doc
                || k.strip_prefix(doc)
                    .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
        })
    };
    let stale: Vec<&String> = docs
        .keys()
        .filter(|d| !exists(d) && !file_only(d))
        .collect();
    assert!(
        stale.is_empty(),
        "SPEC §13 documents keys the config does not have: {stale:?}"
    );

    // The prose lists the same exceptions as this test.
    let spec = include_str!("../../../SPEC.md");
    let prose = spec
        .split("the only exceptions are file-only blocks it lists (")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .expect("SPEC §13 lists the file-only blocks");
    let listed: Vec<&str> = prose.split('`').skip(1).step_by(2).collect();
    assert_eq!(
        listed, FILE_ONLY,
        "SPEC §13's file-only list and FILE_ONLY differ"
    );
}

#[test]
fn file_only_matches_on_key_boundaries() {
    assert!(file_only("hooks") && file_only("hooks[].event"));
    assert!(file_only("provider.auth.client_id") && file_only("providers.*.model"));
    assert!(!file_only("hooks_extra") && !file_only("providers_v2") && !file_only("roles2.x"));
}
