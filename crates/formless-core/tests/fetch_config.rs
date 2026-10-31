//! Tool HTTP deadline settings survive config round trips and old configs.
use formless_core::Config;

#[test]
fn tool_fetch_deadlines_have_finite_defaults_and_can_be_configured() {
    let defaults: Config = toml::from_str("").unwrap();
    assert_eq!(defaults.tools.fetch_connect_timeout_secs, 10);
    assert_eq!(defaults.tools.fetch_timeout_secs, 60);
    let configured: Config =
        toml::from_str("[tools]\nfetch_connect_timeout_secs = 2\nfetch_timeout_secs = 5\n")
            .unwrap();
    let roundtrip: Config = toml::from_str(&toml::to_string(&configured).unwrap()).unwrap();
    assert_eq!(roundtrip.tools.fetch_connect_timeout_secs, 2);
    assert_eq!(roundtrip.tools.fetch_timeout_secs, 5);
}
