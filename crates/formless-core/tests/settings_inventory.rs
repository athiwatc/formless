//! Safe current-value projection, shared by real browser fixtures and the TUI parity gate.
use formless_core::{
    config::{setting_docs_json, McpServerCfg, ModelPrice, SETTABLE},
    Config, Paths,
};
use serde_json::{json, Value};

fn paths() -> Paths {
    Paths {
        data_dir: "/fixture/data".into(),
        config_dir: "/fixture/config".into(),
        profile: "fixture".into(),
    }
}

#[test]
fn documented_settable_values_choices_and_optional_keys_are_complete() {
    let mut cfg = Config::default();
    cfg.provider.max_retries = 7;
    cfg.provider.fallback_models = vec!["small,with-comma".into(), "large".into()];
    cfg.provider.prices.insert(
        "fixture".into(),
        ModelPrice {
            input: 1.5,
            output: 3.0,
            ..Default::default()
        },
    );
    cfg.provider
        .extra_headers
        .insert("X-Credential".into(), "HeaderSentinel822".into());
    cfg.provider
        .query_params
        .insert("token".into(), "QuerySentinel822".into());
    cfg.security
        .secret_patterns
        .push("PatternSentinel822".into());
    let mut server = McpServerCfg::default();
    server
        .env
        .insert("API_KEY".into(), "McpEnvSentinel822".into());
    cfg.mcp.servers.insert("fixture".into(), server);
    let summary = cfg.settings_summary(&paths());
    let values = summary["values"].as_object().unwrap();
    let docs = setting_docs_json();
    let serialized = serde_json::to_value(&cfg).unwrap();
    let mut expected = 0;
    for key in docs.as_object().unwrap().keys().filter(|key| {
        SETTABLE.iter().any(|allowed| {
            key.as_str() == *allowed || (allowed.ends_with('.') && key.starts_with(allowed))
        })
    }) {
        expected += 1;
        assert!(values.contains_key(key), "missing {key}");
        assert_eq!(
            &values[key],
            serialized
                .pointer(&format!("/{}", key.replace('.', "/")))
                .unwrap_or(&Value::Null),
            "{key}"
        );
    }
    assert!(expected > 100);
    assert_eq!(
        values.len(),
        expected,
        "projection has no non-settable keys"
    );
    assert_eq!(values["provider.max_retries"], 7);
    assert_eq!(
        values["provider.fallback_models"],
        json!(["small,with-comma", "large"])
    );
    assert_eq!(values["provider.prices"]["fixture"]["input"], 1.5);
    assert!(values.contains_key("limits.daily_usd") && values["limits.daily_usd"].is_null());
    assert_eq!(
        summary["choices"]["tui.theme"],
        json!(["auto", "dark", "light", "high-contrast", "colorblind"])
    );
    for key in [
        "provider.base_url",
        "provider.extra_headers",
        "provider.query_params",
        "provider.auth",
        "security.secure",
        "hooks",
        "mcp.servers",
    ] {
        assert!(!values.contains_key(key), "{key} must not become editable");
    }
    assert_eq!(summary["read_only"]["mcp.servers"], "1 configured");
    let text = summary.to_string();
    for secret in [
        "HeaderSentinel822",
        "QuerySentinel822",
        "PatternSentinel822",
        "McpEnvSentinel822",
    ] {
        assert!(!text.contains(secret), "private value leaked: {secret}");
    }
    // Optional fixture export lets the browser exercise the exact Rust payload,
    // including every current key, rather than a hand-maintained JS inventory.
    if let Some(file) = std::env::var_os("FORMLESS_SETTINGS_FIXTURE") {
        std::fs::write(file, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
    }
}
