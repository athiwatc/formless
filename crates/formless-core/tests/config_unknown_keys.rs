use formless_core::config::{unknown_keys_from_text, UnknownConfigKey};
use formless_core::Config;

#[test]
fn reports_unknown_keys_and_suggests_close_known_keys() {
    let text = r#"
[provider]
preset = "openai"
modle = "gpt-5"

[provider.auth]
revocation_url = "https://oauth.example.invalid/revoke"

[provider.prices."gpt-5"]
input = 1.0
output = 2.0

[provider.extra_headers]
"X-Request-ID" = "fixture"

[providers.local]
base_url = "http://localhost/v1"
model = "llama"
modle = "llama-3"

[roles.reviewer]
description = "review"
provider = "local"
model = "llama"

[mcp.servers.local]
command = "server"
args = ["--stdio"]

[mcp.servers.local.env]
API_TOKEN = "fixture"

[tui.colors]
"warning-text" = "yellow"

[future_section]
enabled = true
"#;

    assert_eq!(
        unknown_keys_from_text(text).unwrap(),
        vec![
            UnknownConfigKey {
                key: "future_section".into(),
                suggestion: None,
            },
            UnknownConfigKey {
                key: "provider.modle".into(),
                suggestion: Some("provider.model".into()),
            },
            UnknownConfigKey {
                key: "providers.local.modle".into(),
                suggestion: Some("providers.local.model".into()),
            },
        ]
    );
}

#[test]
fn catches_unknown_keys_inside_named_provider_role_and_server_entries() {
    let text = r#"
[providers.local]
bas_url = "http://localhost/v1"

[roles.reviewer]
descrption = "review"

[mcp.servers.local]
command = "server"
commnad = "typo"
"#;

    assert_eq!(
        unknown_keys_from_text(text).unwrap(),
        vec![
            UnknownConfigKey {
                key: "mcp.servers.local.commnad".into(),
                suggestion: Some("mcp.servers.local.command".into()),
            },
            UnknownConfigKey {
                key: "providers.local.bas_url".into(),
                suggestion: Some("providers.local.base_url".into()),
            },
            UnknownConfigKey {
                key: "roles.reviewer.descrption".into(),
                suggestion: Some("roles.reviewer.description".into()),
            },
        ]
    );
}

#[test]
fn quotes_dynamic_map_key_segments_in_paths_and_suggestions() {
    let text = r#"
[provider.prices."gpt-4.1"]
inpt = 1.0
"#;

    assert_eq!(
        unknown_keys_from_text(text).unwrap(),
        vec![UnknownConfigKey {
            key: "provider.prices.\"gpt-4.1\".inpt".into(),
            suggestion: Some("provider.prices.\"gpt-4.1\".input".into()),
        }]
    );
}

#[test]
fn default_config_round_trip_has_no_unknown_keys() {
    let mut config = Config::default();
    config.naming.auto_download_migration_done = true;
    let text = toml::to_string_pretty(&config).unwrap();
    assert!(unknown_keys_from_text(&text).unwrap().is_empty());
}
