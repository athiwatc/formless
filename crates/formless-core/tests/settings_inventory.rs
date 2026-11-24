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
    assert_eq!(values["naming.auto_download"], false);
    assert_eq!(values["naming.model"], "qwen2.5-0.5b");
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
    assert!(summary["read_only"].get("naming.model").is_none());
    assert_eq!(summary["read_only"]["naming.model_size"], "409 MiB");
    assert_eq!(summary["read_only"]["naming.cache_size"], "0 B");
    assert_eq!(summary["read_only"]["naming.status"], "not installed");
    assert_eq!(summary["naming"]["model"], "qwen2.5-0.5b");
    assert!(!summary["docs"]["naming.auto_download"]["description"]
        .as_str()
        .unwrap()
        .contains("409 MiB"));
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

#[test]
fn legacy_naming_download_default_is_migrated_before_explicit_opt_in() {
    let base =
        std::env::temp_dir().join(format!("formless-naming-consent-{}", uuid::Uuid::new_v4()));
    let paths = Paths {
        data_dir: base.join("data"),
        config_dir: base.join("config"),
        profile: "fixture".into(),
    };
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    assert!(!Config::default().naming.auto_download);
    Config::default().save(&paths).unwrap();
    let saved_default = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(saved_default.contains("auto_download = false"));
    assert!(saved_default.contains("auto_download_migration_done = true"));

    std::fs::write(paths.config_file(), "[naming]\nauto_download = true\n").unwrap();

    let legacy_text = std::fs::read_to_string(paths.config_file()).unwrap();
    let file_only = Config::load_file_only(&paths).unwrap();
    assert!(!file_only.naming.auto_download);
    assert!(file_only.naming.auto_download_migration_done);
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        legacy_text
    );

    let mut cfg = Config::load(&paths).unwrap();
    assert!(!cfg.naming.auto_download);
    assert!(cfg.naming.auto_download_migration_done);
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        legacy_text
    );

    assert!(Config::persist_naming_download_migration(&paths).unwrap());
    let saved = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(saved.contains("auto_download = false"));
    assert!(saved.contains("auto_download_migration_done = true"));
    assert!(!Config::persist_naming_download_migration(&paths).unwrap());

    cfg.set_key("naming.auto_download", json!(true)).unwrap();
    assert!(cfg.naming.auto_download_migration_done);
    cfg.save(&paths).unwrap();
    assert!(Config::load(&paths).unwrap().naming.auto_download);

    let hand_authored_opt_in =
        "[naming]\nauto_download = true\nauto_download_migration_done = true\n";
    std::fs::write(paths.config_file(), hand_authored_opt_in).unwrap();
    let opted_in = Config::load_file_only(&paths).unwrap();
    assert!(opted_in.naming.auto_download);
    assert!(!Config::persist_naming_download_migration(&paths).unwrap());
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        hand_authored_opt_in
    );

    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn naming_download_text_load_migrates_legacy_default_and_preserves_opt_in() {
    let file = std::path::Path::new("fixture-config.toml");
    let legacy = Config::load_from_text("[naming]\nauto_download = true\n", file).unwrap();
    assert!(!legacy.naming.auto_download);
    assert!(legacy.naming.auto_download_migration_done);

    let opted_in = Config::load_from_text(
        "[naming]\nauto_download = true\nauto_download_migration_done = true\n",
        file,
    )
    .unwrap();
    assert!(opted_in.naming.auto_download);
}
