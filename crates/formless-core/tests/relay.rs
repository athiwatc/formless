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
            "wss://formless.athiwat.xyz/relay/{}?role=daemon&mux=1",
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

fn temp_paths(tag: &str) -> formless_core::Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = formless_core::Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

/// A fake relay: each browser gets its own daemon connection, responses are
/// routed back by client id, and only attached browsers are subscribers.
#[tokio::test]
async fn relay_gives_each_browser_its_own_connection() {
    use futures_util::{SinkExt, StreamExt};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    let paths = temp_paths("relaymux");
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths).unwrap());
    let secret = generate_relay_secret();
    let key = key_from_secret(&secret).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let link = tokio::spawn({
        let (d, s, t) = (daemon.clone(), secret.clone(), shutdown.clone());
        async move { formless_core::relay::connect_once(d, &base, &s, &t).await }
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();

    async fn settle(daemon: &formless_core::daemon::Daemon, want: usize) {
        for _ in 0..50 {
            if daemon.subscriber_count() == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(daemon.subscriber_count(), want);
    }

    // The relay link alone is not a subscriber.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(daemon.subscriber_count(), 0);

    for cid in ["a", "b"] {
        let open = serde_json::json!({"relay": "client_open", "cid": cid});
        ws.send(Message::Text(open.to_string())).await.unwrap();
    }
    settle(&daemon, 2).await;

    // Both browsers use request id 7; each must get its own answer.
    for cid in ["a", "b"] {
        let req = serde_json::json!({"id": 7, "protocol": 1, "request": {"type": "ping"}});
        let f = encrypt_line(&key, &req.to_string()).unwrap();
        let frame = serde_json::json!({"cid": cid, "f": f});
        ws.send(Message::Text(frame.to_string())).await.unwrap();
    }
    let mut answered = Vec::new();
    while answered.len() < 2 {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
        let line = decrypt_line(&key, frame["f"].as_str().unwrap()).unwrap();
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        if resp["kind"] == "response" && resp["id"] == 7 {
            answered.push(frame["cid"].as_str().unwrap().to_string());
        }
    }
    answered.sort();
    assert_eq!(answered, ["a", "b"]);

    let close = serde_json::json!({"relay": "client_close", "cid": "a"});
    ws.send(Message::Text(close.to_string())).await.unwrap();
    settle(&daemon, 1).await;

    shutdown.cancel();
    link.await.unwrap().unwrap();
}
