//! Relay crypto/URL contract (SPEC §17 v2): the pairing secret is the
//! AES-256-GCM key; the room is a one-way hash of it.

use formless_core::config::{generate_relay_secret, RelayCfg};
use formless_core::relay::{
    browser_url, decrypt_line, encrypt_line, key_from_secret, relay_ws_url, room_id,
};

#[test]
fn relay_crypto_round_trips_and_room_is_one_way() {
    let secret = generate_relay_secret();
    let key = key_from_secret(&secret).unwrap();
    let frame = encrypt_line(&key, "{\"hello\":\"world\"}").unwrap();
    assert!(
        !frame.contains("world"),
        "ciphertext must not leak plaintext"
    );
    assert_eq!(decrypt_line(&key, &frame).unwrap(), "{\"hello\":\"world\"}");

    let other = key_from_secret(&generate_relay_secret()).unwrap();
    assert!(decrypt_line(&other, &frame).is_err(), "wrong key must fail");
    assert!(decrypt_line(&key, "not json").is_err());

    let room = room_id(&key);
    assert_eq!(room.len(), 32);
    assert_eq!(room, room_id(&key), "room derivation is stable");
    assert_ne!(room, room_id(&other));

    assert!(key_from_secret("too-short").is_err());
}

#[test]
fn relay_urls_use_room_and_fragment_secret() {
    let secret = generate_relay_secret();
    let key = key_from_secret(&secret).unwrap();
    let ws = relay_ws_url("wss://formless.athiwat.xyz/", &secret).unwrap();
    assert_eq!(
        ws,
        format!(
            "wss://formless.athiwat.xyz/relay/{}?role=daemon",
            room_id(&key)
        )
    );
    let cfg = RelayCfg {
        enabled: true,
        url: "wss://formless.athiwat.xyz".into(),
    };
    let web = browser_url(&cfg, &secret);
    assert!(web.starts_with("https://formless.athiwat.xyz/#k="), "{web}");
    assert!(web.ends_with(&secret));
    // The relay-facing websocket URL never carries the secret itself.
    assert!(!ws.contains(&secret));
}

#[test]
fn web_settings_summary_mirrors_tui_and_omits_secrets() {
    let base = std::env::temp_dir().join(format!(
        "fl-settings-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let paths = formless_core::Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    let mut cfg = formless_core::Config::default();
    cfg.provider.base_url = "https://api.example.com/v1".into();
    cfg.provider.model = "m1".into();
    cfg.provider
        .extra_headers
        .insert("Authorization".into(), "Bearer SUPER-SECRET".into());
    let v = cfg.settings_summary(&paths);
    assert_eq!(v["provider"]["base_url"], "https://api.example.com/v1");
    assert_eq!(v["provider"]["model"], "m1");
    assert_eq!(v["provider"]["auth_mode"], cfg.provider.auth.mode.as_str());
    assert_eq!(v["relay"]["enabled"], false);
    assert!(v["paths"]["config"]
        .as_str()
        .unwrap()
        .ends_with("config.toml"));
    let text = v.to_string();
    assert!(
        !text.contains("SUPER-SECRET") && !text.contains("extra_headers"),
        "{text}"
    );
}
