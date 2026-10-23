//! Relay crypto/URL contract (SPEC §17 v2): the pairing secret is the
//! root key, each browser connection derives a fresh session key, frames
//! cannot be replayed or reflected, and the room is a one-way hash.

use formless_core::config::{generate_relay_secret, RelayCfg};
use formless_core::relay::{
    browser_url, hello_frame, key_from_secret, new_hello, parse_hello, relay_ws_url, room_id,
    Channel, Side,
};

fn pair(key: &[u8; 32]) -> (Channel, Channel) {
    let (b, d) = (new_hello(), new_hello());
    (
        Channel::new(key, &b, &d, Side::Browser),
        Channel::new(key, &b, &d, Side::Daemon),
    )
}

#[test]
fn relay_crypto_round_trips_and_room_is_one_way() {
    let secret = generate_relay_secret();
    let key = key_from_secret(&secret).unwrap();
    let (mut browser, mut daemon) = pair(&key);
    let frame = browser.seal("{\"hello\":\"world\"}").unwrap();
    assert!(
        !frame.contains("world"),
        "ciphertext must not leak plaintext"
    );
    assert_eq!(daemon.open(&frame).unwrap(), "{\"hello\":\"world\"}");

    let other = key_from_secret(&generate_relay_secret()).unwrap();
    let (_, mut wrong) = pair(&other);
    assert!(
        wrong.open(&browser.seal("x").unwrap()).is_err(),
        "wrong key must fail"
    );
    assert!(daemon.open("not json").is_err());

    let room = room_id(&key);
    assert_eq!(room.len(), 32);
    assert_eq!(room, room_id(&key), "room derivation is stable");
    assert_ne!(room, room_id(&other));

    assert!(key_from_secret("too-short").is_err());
}

#[test]
fn relay_frames_cannot_be_replayed_or_reflected() {
    let key = key_from_secret(&generate_relay_secret()).unwrap();
    let (mut browser, mut daemon) = pair(&key);
    let approve = browser.seal("approve").unwrap();
    assert_eq!(daemon.open(&approve).unwrap(), "approve");
    assert!(daemon.open(&approve).is_err(), "same frame twice");

    // An older frame after a newer one is rejected too.
    let first = browser.seal("1").unwrap();
    let second = browser.seal("2").unwrap();
    assert_eq!(daemon.open(&second).unwrap(), "2");
    assert!(daemon.open(&first).is_err());

    // A frame reflected back to its sender does not authenticate: the
    // direction is bound into the associated data.
    let (b, d) = (new_hello(), new_hello());
    let mut sender = Channel::new(&key, &b, &d, Side::Daemon);
    let mut same_side = Channel::new(&key, &b, &d, Side::Daemon);
    let mut receiver = Channel::new(&key, &b, &d, Side::Browser);
    let reply = sender.seal("reply").unwrap();
    assert!(same_side.open(&reply).is_err(), "reflected frame");
    assert_eq!(receiver.open(&reply).unwrap(), "reply");

    // A frame captured in one session is useless in the next.
    let (mut browser2, mut daemon2) = pair(&key);
    assert!(daemon2.open(&approve).is_err(), "cross-session replay");
    assert_eq!(daemon2.open(&browser2.seal("ok").unwrap()).unwrap(), "ok");

    // Handshake frames round-trip; v1 frames are refused.
    let nonce = new_hello();
    assert_eq!(parse_hello(&hello_frame(&nonce)).unwrap(), nonce);
    assert!(parse_hello(r#"{"v":1,"hello":"AAAAAAAAAAAAAAAAAAAAAA=="}"#).is_none());
    let v1 = r#"{"v":1,"iv":"AAAAAAAAAAAAAAAA","ct":"AAAA"}"#;
    assert!(daemon2.open(v1).is_err());
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
    // A browser is a subscriber only once it has completed the handshake.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(daemon.subscriber_count(), 0);

    async fn recv(
        ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> serde_json::Value {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(msg.to_text().unwrap()).unwrap()
    }

    let mut channels = std::collections::HashMap::new();
    for cid in ["a", "b"] {
        let browser_hello = new_hello();
        let frame = serde_json::json!({"cid": cid, "f": hello_frame(&browser_hello)});
        ws.send(Message::Text(frame.to_string())).await.unwrap();
        let reply = recv(&mut ws).await;
        assert_eq!(reply["cid"], cid);
        let daemon_hello = parse_hello(reply["f"].as_str().unwrap()).unwrap();
        channels.insert(
            cid,
            Channel::new(&key, &browser_hello, &daemon_hello, Side::Browser),
        );
    }
    settle(&daemon, 2).await;

    // Both browsers use request id 7; each must get its own answer.
    let req = serde_json::json!({"id": 7, "protocol": 1, "request": {"type": "ping"}});
    let mut sent = Vec::new();
    for cid in ["a", "b"] {
        let f = channels
            .get_mut(cid)
            .unwrap()
            .seal(&req.to_string())
            .unwrap();
        sent.push(f.clone());
        let frame = serde_json::json!({"cid": cid, "f": f});
        ws.send(Message::Text(frame.to_string())).await.unwrap();
    }
    let mut answered = Vec::new();
    while answered.len() < 2 {
        let frame = recv(&mut ws).await;
        let cid = frame["cid"].as_str().unwrap().to_string();
        let line = channels
            .get_mut(cid.as_str())
            .unwrap()
            .open(frame["f"].as_str().unwrap())
            .unwrap();
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        if resp["kind"] == "response" && resp["id"] == 7 {
            answered.push(cid);
        }
    }
    answered.sort();
    assert_eq!(answered, ["a", "b"]);

    // Replaying a's request is dropped: no second response arrives.
    let replay = serde_json::json!({"cid": "a", "f": sent[0]});
    ws.send(Message::Text(replay.to_string())).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), ws.next())
            .await
            .is_err(),
        "replayed frame must not be answered"
    );

    let close = serde_json::json!({"relay": "client_close", "cid": "a"});
    ws.send(Message::Text(close.to_string())).await.unwrap();
    settle(&daemon, 1).await;

    shutdown.cancel();
    link.await.unwrap().unwrap();
}

/// With no browser in the room the daemon sends the relay nothing, even
/// while events are being broadcast, and is not kept alive as a subscriber.
#[tokio::test]
async fn relay_link_is_silent_without_browsers() {
    use futures_util::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    let daemon = Arc::new(formless_core::daemon::Daemon::new(temp_paths("relayquiet")).unwrap());
    let secret = generate_relay_secret();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let link = tokio::spawn({
        let (d, s, t) = (daemon.clone(), secret.clone(), shutdown.clone());
        async move { formless_core::relay::connect_once(d, &base, &s, &t).await }
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();

    // Daemon activity that would be broadcast to any attached client.
    daemon
        .handle(formless_protocol::Request::CreateSession { name: None })
        .await
        .unwrap();
    let quiet = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                other => return other,
            }
        }
    })
    .await;
    assert!(quiet.is_err(), "no frames without a browser: {quiet:?}");
    assert_eq!(daemon.subscriber_count(), 0);
    shutdown.cancel();
    link.await.unwrap().unwrap();
}

/// A question waiting on the user asks the relay for a Web Push (SPEC §12),
/// a payload-free control frame.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn waiting_questions_ask_the_relay_for_a_push() {
    use futures_util::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    let paths = temp_paths("relaypush");
    formless_core::Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let sid = formless_core::Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Someone who could answer is connected.
    let _client = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let secret = generate_relay_secret();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let shutdown = tokio_util::sync::CancellationToken::new();
    tokio::spawn({
        let (d, s, t) = (daemon.clone(), secret.clone(), shutdown.clone());
        async move { formless_core::relay::connect_once(d, &base, &s, &t).await }
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
    for _ in 0..50 {
        if daemon.subscriber_count() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::spawn({
        let d = daemon.clone();
        async move {
            d.call_tool_direct(
                &sid,
                "ask_user",
                serde_json::json!({"question": "Which file?"}),
            )
            .await
        }
    });
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => return t.to_string(),
                Some(Ok(_)) => continue,
                other => panic!("relay link ended: {other:?}"),
            }
        }
    })
    .await
    .expect("a push request");
    assert_eq!(frame, r#"{"relay":"push"}"#);
    shutdown.cancel();
}

/// The relay's browsers are listed with their User-Agent and can be
/// disconnected one by one (SPEC §12).
#[tokio::test]
async fn relay_browsers_are_listed_and_can_be_disconnected() {
    use futures_util::{SinkExt, StreamExt};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    let daemon = Arc::new(formless_core::daemon::Daemon::new(temp_paths("relaydevices")).unwrap());
    let secret = generate_relay_secret();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let link = tokio::spawn({
        let (d, s, t) = (daemon.clone(), secret.clone(), shutdown.clone());
        async move { formless_core::relay::connect_once(d, &base, &s, &t).await }
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
    ws.send(Message::Text(
        r#"{"relay":"client_open","cid":"c1","ua":"Mozilla/5.0 (iPhone) Safari/604.1"}"#.into(),
    ))
    .await
    .unwrap();
    let list = loop {
        let v = daemon
            .handle(formless_protocol::Request::ListRelayClients)
            .await
            .unwrap();
        if !v.as_array().unwrap().is_empty() {
            break v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(list[0]["cid"], "c1");
    assert!(list[0]["user_agent"].as_str().unwrap().contains("iPhone"));

    let r = daemon
        .handle(formless_protocol::Request::DisconnectRelayClient { cid: "c1".into() })
        .await
        .unwrap();
    assert_eq!(r["disconnected"], true);
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Ok(Message::Text(t))) = ws.next().await {
                return t.to_string();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(frame, r#"{"cid":"c1","relay":"disconnect"}"#);
    let list = daemon
        .handle(formless_protocol::Request::ListRelayClients)
        .await
        .unwrap();
    assert!(list.as_array().unwrap().is_empty());
    // An unknown browser: nothing to do.
    let r = daemon
        .handle(formless_protocol::Request::DisconnectRelayClient { cid: "nope".into() })
        .await
        .unwrap();
    assert_eq!(r["disconnected"], false);
    shutdown.cancel();
    link.await.unwrap().unwrap();
    assert!(daemon
        .handle(formless_protocol::Request::ListRelayClients)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
}
