//! Relay crypto/URL contract (SPEC §17 v2): the pairing secret is the
//! root key, each browser connection derives a fresh session key, frames
//! cannot be replayed or reflected, and the room is a one-way hash.

#[path = "support/env.rs"]
mod credential_env;
#[path = "support/relay_peer.rs"]
mod relay_peer;
use relay_peer::RoomPeer;

use formless_core::config::{generate_relay_secret, RelayCfg};
use formless_core::relay::{
    browser_url, confirm_frame, confirm_tag, hello_frame, key_from_secret, new_hello, parse_hello,
    relay_ws_url, room_id, Channel, Side,
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
    let _env = credential_env::EnvGuard::isolated();
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
    let _env = credential_env::EnvGuard::isolated();
    let key = key_from_secret(&generate_relay_secret()).unwrap();
    let (mut browser, mut daemon) = pair(&key);
    let approve = browser.seal("approve").unwrap();
    assert_eq!(daemon.open(&approve).unwrap(), "approve");
    assert!(daemon.open(&approve).is_err(), "same frame twice");

    // A withheld frame is not skipped over: the next one is refused as
    // out of sequence (the daemon then closes the channel, #905).
    let first = browser.seal("1").unwrap();
    let second = browser.seal("2").unwrap();
    let gap = daemon.open(&second).unwrap_err();
    assert!(format!("{gap}").contains("out of sequence"), "{gap}");
    assert_eq!(daemon.open(&first).unwrap(), "1");
    assert_eq!(daemon.open(&second).unwrap(), "2");
    assert!(
        daemon.open(&first).is_err(),
        "older frame after a newer one"
    );

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
    let _env = credential_env::EnvGuard::isolated();
    let secret = generate_relay_secret();
    let key = key_from_secret(&secret).unwrap();
    let ws = relay_ws_url("wss://formless.athiwat.xyz/", &secret).unwrap();
    assert_eq!(
        ws,
        format!(
            "wss://formless.athiwat.xyz/relay/{}?role=daemon&mux=1&transport=1",
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
    let _env = credential_env::EnvGuard::isolated();
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
    let _env = credential_env::EnvGuard::isolated();
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
    let mut ws = RoomPeer::accept(tcp).await;

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

    async fn recv(ws: &mut RoomPeer) -> serde_json::Value {
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
        // Not a subscriber until it proves it has the key (#904).
        let confirm = serde_json::json!({"cid": cid, "f": confirm_frame(&key, &browser_hello, &daemon_hello)});
        ws.send(Message::Text(confirm.to_string())).await.unwrap();
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

    // Replaying a's request is not answered: the daemon closes a's
    // channel as a link integrity failure instead (#905).
    let replay = serde_json::json!({"cid": "a", "f": sent[0]});
    ws.send(Message::Text(replay.to_string())).await.unwrap();
    let frame = recv(&mut ws).await;
    assert_eq!(frame["relay"], "disconnect", "{frame}");
    assert_eq!(frame["cid"], "a");
    assert_eq!(frame["code"], 4002);
    settle(&daemon, 1).await;

    shutdown.cancel();
    link.await.unwrap().unwrap();
}

/// With no browser in the room the daemon sends the relay nothing, even
/// while events are being broadcast, and is not kept alive as a subscriber.
#[tokio::test]
async fn relay_link_is_silent_without_browsers() {
    let _env = credential_env::EnvGuard::isolated();
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
    let mut ws = RoomPeer::accept(tcp).await;

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
    let _env = credential_env::EnvGuard::isolated();
    use formless_protocol::{Event, Request, ServerMessage};
    use futures_util::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;
    use tokio_tungstenite::tungstenite::Message;

    let paths = temp_paths("relaypush");
    formless_core::Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let sid = formless_core::Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    // Keep the actual answer-capable IPC connection, rather than a readiness
    // probe whose transient subscriber could disappear before ask_user.
    let client = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match tokio::net::UnixStream::connect(paths.socket_path()).await {
                Ok(client) => break client,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    tokio::task::yield_now().await;
                }
                Err(error) => panic!("connect private daemon: {error}"),
            }
        }
    })
    .await
    .expect("private daemon starts within 5s");
    let mut events = tokio::io::BufReader::new(client).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        while daemon.subscriber_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("IPC answerer registered");
    let secret = generate_relay_secret();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let link = tokio::spawn({
        let (d, s, t) = (daemon.clone(), secret.clone(), shutdown.clone());
        async move { formless_core::relay::connect_once(d, &base, &s, &t).await }
    });
    let (tcp, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .expect("relay TCP connection")
        .unwrap();
    // Hold the server's WebSocket handshake until the question has actually
    // been broadcast. Peek leaves the real Upgrade request for accept_async.
    let mut request = [0; 1];
    let received = tokio::time::timeout(Duration::from_secs(5), tcp.peek(&mut request))
        .await
        .expect("relay starts its WebSocket handshake")
        .unwrap();
    assert_eq!(received, 1);
    assert_eq!(request, [b'G']);
    let question = tokio::spawn({
        let (d, sid) = (daemon.clone(), sid.clone());
        async move {
            d.call_tool_direct(
                &sid,
                "ask_user",
                serde_json::json!({"question": "Which file?"}),
            )
            .await
        }
    });
    let question_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let line = events.next_line().await.unwrap().expect("IPC stays open");
            if let Ok(ServerMessage::Event(Event::UserQuestion {
                question_id,
                session_id,
                question,
                ..
            })) = serde_json::from_str(&line)
            {
                assert_eq!(session_id, sid);
                assert_eq!(question, "Which file?");
                // broadcast sends attention before delivering this event.
                // Thus the old late subscription deterministically loses it.
                break question_id;
            }
        }
    })
    .await
    .expect("question registered while WebSocket handshake is held");
    let snapshot = daemon
        .handle(Request::GetSession { id: sid.clone() })
        .await
        .unwrap();
    assert!(snapshot["pending_events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|ev| { ev["type"] == "user_question" && ev["question_id"] == question_id }));
    assert!(
        !question.is_finished(),
        "question still waits for an answer"
    );
    assert!(!link.is_finished(), "relay still waits for the handshake");
    let mut ws = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::accept_async(tcp))
        .await
        .expect("release held WebSocket handshake")
        .unwrap();
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
    let answered = daemon
        .handle(Request::AnswerQuestion {
            question_id,
            answer: "private-choice".into(),
            answers: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(answered["answered"], true);
    let reply = tokio::time::timeout(Duration::from_secs(5), question)
        .await
        .expect("question completes after explicit answer")
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&reply).unwrap(),
        serde_json::json!({"answer":"private-choice"})
    );
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), link)
        .await
        .expect("relay stops after cancellation")
        .unwrap()
        .unwrap();
    drop(ws);
    drop(events);
    daemon.handle(Request::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("private daemon acknowledges shutdown")
        .unwrap()
        .unwrap();
}

/// The relay's browsers are listed with their User-Agent and can be
/// disconnected one by one (SPEC §12).
#[tokio::test]
async fn relay_browsers_are_listed_and_can_be_disconnected() {
    let _env = credential_env::EnvGuard::isolated();
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
    let mut ws = RoomPeer::accept(tcp).await;
    ws.send(Message::Text(
        r#"{"relay":"client_open","cid":"c1","ua":"Mozilla/5.0 (iPhone) Safari/604.1"}"#.into(),
    ))
    .await
    .unwrap();
    // Listed only once its handshake is confirmed (#904).
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(daemon
        .handle(formless_protocol::Request::ListRelayClients)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let key = key_from_secret(&secret).unwrap();
    let bh = new_hello();
    ws.send(Message::Text(
        serde_json::json!({"cid": "c1", "f": hello_frame(&bh)}).to_string(),
    ))
    .await
    .unwrap();
    let reply: serde_json::Value = loop {
        if let Some(Ok(Message::Text(t))) = ws.next().await {
            break serde_json::from_str(&t).unwrap();
        }
    };
    let dh = parse_hello(reply["f"].as_str().unwrap()).unwrap();
    ws.send(Message::Text(
        serde_json::json!({"cid": "c1", "f": confirm_frame(&key, &bh, &dh)}).to_string(),
    ))
    .await
    .unwrap();
    let mut fixture_channel = Channel::new(&key, &bh, &dh, Side::Browser);
    ws.send(Message::Text(serde_json::json!({"cid":"c1","f":fixture_channel.seal(r#"{"id":99,"protocol":1,"request":{"type":"ping"}}"#).unwrap()}).to_string())).await.unwrap();
    let _ = ws.next().await;
    let list = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let v = daemon
                .handle(formless_protocol::Request::ListRelayClients)
                .await
                .unwrap();
            if !v.as_array().unwrap().is_empty() {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("relay client never registered within 10 seconds");
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
    let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
    assert_eq!(
        frame,
        serde_json::json!({"cid":"c1","relay":"disconnect","code":4001})
    );
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

/// A relay that accepts and drops at once keeps doubling the backoff to
/// its cap; only a link that lasted resets it; waits are jittered (#906).
#[test]
fn relay_backoff_resets_only_after_a_stable_link_and_is_jittered() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::relay::{jittered, next_backoff};
    use std::time::Duration;
    let mut ceiling = 1_000;
    for _ in 0..10 {
        ceiling = next_backoff(ceiling, Duration::from_millis(50));
    }
    assert_eq!(ceiling, 30_000, "short-lived links reach the cap");
    assert_eq!(next_backoff(ceiling, Duration::from_secs(61)), 1_000);
    let waits: std::collections::HashSet<_> = (0..20).map(|_| jittered(30_000)).collect();
    assert!(waits.len() > 1, "waits are not all the same");
    assert!(waits
        .iter()
        .all(|w| *w >= Duration::from_millis(250) && *w <= Duration::from_secs(30)));
}

/// Plain ws:// to another machine, or no scheme at all, is refused (#907).
#[test]
fn relay_url_must_be_wss_or_loopback() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::config::RelayCfg;
    let cfg = |url: &str| RelayCfg {
        enabled: true,
        url: url.into(),
    };
    _env.remove("FORMLESS_RELAY_ALLOW_INSECURE");
    assert!(cfg("wss://formless.athiwat.xyz").check().is_ok());
    assert!(cfg("ws://127.0.0.1:8787").check().is_ok());
    assert!(cfg("ws://localhost:8787").check().is_ok());
    assert!(cfg("ws://[::1]:8787").check().is_ok());
    let e = cfg("ws://relay.example.com").check().unwrap_err();
    assert!(format!("{e:#}").contains("wss://"), "{e:#}");
    assert!(cfg("relay.example.com").check().is_err());
    assert!(cfg("https://relay.example.com").check().is_err());
}

/// The pairing key and each session key are wiped when dropped (#908):
/// a source check, since nothing observable shows a zeroed buffer.
#[test]
fn relay_key_material_is_zeroized() {
    let _env = credential_env::EnvGuard::isolated();
    let src = include_str!("../src/relay.rs");
    assert!(
        src.contains("-> Result<Zeroizing<[u8; 32]>>"),
        "pairing key"
    );
    assert!(
        src.contains("let mut session_key = Zeroizing::new("),
        "session key"
    );
    assert!(
        src.contains("load_relay_secret(&daemon.paths).map(Zeroizing::new)"),
        "secret string"
    );
    let key =
        formless_core::relay::key_from_secret(&formless_core::config::generate_relay_secret())
            .unwrap();
    let _: &zeroize::Zeroizing<[u8; 32]> = &key;
}

/// The handshake is confirmed both ways before a browser counts; a
/// repeated hello gets the same answer; a wrong proof or a replayed old
/// hello never replaces a live channel (#904).
#[tokio::test(flavor = "multi_thread")]
async fn relay_handshake_is_confirmed_and_idempotent() {
    let _env = credential_env::EnvGuard::isolated();
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    let daemon = Arc::new(formless_core::daemon::Daemon::new(temp_paths("relayconfirm")).unwrap());
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
    let mut ws = RoomPeer::accept(tcp).await;
    async fn recv(ws: &mut RoomPeer) -> serde_json::Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let Message::Text(t) = msg {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }
    let send =
        |cid: &str, f: String| Message::Text(serde_json::json!({"cid": cid, "f": f}).to_string());

    // Before any proof: no subscriber, however many hellos arrive.
    let bh = new_hello();
    ws.send(send("a", hello_frame(&bh))).await.unwrap();
    let first = recv(&mut ws).await;
    let f = first["f"].as_str().unwrap();
    let dh = parse_hello(f).unwrap();
    let v: serde_json::Value = serde_json::from_str(f).unwrap();
    let proof = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        v["confirm"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(
        proof,
        confirm_tag(&key, &bh, &dh, Side::Daemon).to_vec(),
        "the daemon proves the key"
    );
    // A lost reply: the browser sends the same hello and gets the same one.
    ws.send(send("a", hello_frame(&bh))).await.unwrap();
    assert_eq!(recv(&mut ws).await, first, "idempotent");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        daemon.subscriber_count(),
        0,
        "no subscriber before the browser's proof"
    );

    // A wrong proof is ignored.
    let wrong = confirm_frame(
        &key_from_secret(&generate_relay_secret()).unwrap(),
        &bh,
        &dh,
    );
    ws.send(send("a", wrong)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(daemon.subscriber_count(), 0);

    // The right proof can still finish the unchanged pending transcript.
    ws.send(send("a", hello_frame(&bh))).await.unwrap();
    let dh = parse_hello(recv(&mut ws).await["f"].as_str().unwrap()).unwrap();
    ws.send(send("a", confirm_frame(&key, &bh, &dh)))
        .await
        .unwrap();
    let mut chan = Channel::new(&key, &bh, &dh, Side::Browser);
    let mut tries = 0;
    while daemon.subscriber_count() != 1 && tries < 50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        tries += 1;
    }
    assert_eq!(daemon.subscriber_count(), 1);

    // An established known hello is ignored, preserving the next seq1 frame.
    ws.send(send("a", hello_frame(&bh))).await.unwrap();
    let req = serde_json::json!({"id": 1, "protocol": 1, "request": {"type": "ping"}});
    ws.send(send("a", chan.seal(&req.to_string()).unwrap()))
        .await
        .unwrap();
    let frame = recv(&mut ws).await;
    let reply: serde_json::Value =
        serde_json::from_str(&chan.open(frame["f"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(reply["id"], 1);
    ws.send(send("a", hello_frame(&new_hello()))).await.unwrap();
    let closed = recv(&mut ws).await;
    assert_eq!(closed["relay"], "disconnect");
    assert_eq!(closed["code"], 4002);
    shutdown.cancel();
    link.await.unwrap().unwrap();
}

/// The largest attachment crosses the relay in pieces: every frame stays
/// far below the relay's 1 MB message limit, and the file arrives whole
/// (#909).
#[tokio::test(flavor = "multi_thread")]
async fn the_largest_attachment_crosses_the_relay_in_pieces() {
    let _env = credential_env::EnvGuard::isolated();
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message;

    let paths = temp_paths("relayupload");
    let sid = formless_core::Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
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
    let mut ws = RoomPeer::accept(tcp).await;
    let wrap = |f: String| Message::Text(serde_json::json!({"cid": "u", "f": f}).to_string());
    let bh = new_hello();
    ws.send(wrap(hello_frame(&bh))).await.unwrap();
    let reply: serde_json::Value = loop {
        if let Some(Ok(Message::Text(t))) = ws.next().await {
            break serde_json::from_str(&t).unwrap();
        }
    };
    let dh = parse_hello(reply["f"].as_str().unwrap()).unwrap();
    ws.send(wrap(confirm_frame(&key, &bh, &dh))).await.unwrap();
    let mut chan = Channel::new(&key, &bh, &dh, Side::Browser);
    let mut next_id = 0u64;
    let mut call = |chan: &mut Channel, request: serde_json::Value| {
        next_id += 1;
        let line =
            serde_json::json!({"id": next_id, "protocol": 1, "request": request}).to_string();
        (next_id, chan.seal(&line).unwrap())
    };
    async fn answer(ws: &mut RoomPeer, chan: &mut Channel, id: u64) -> serde_json::Value {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let Message::Text(t) = msg else { continue };
            let frame: serde_json::Value = serde_json::from_str(&t).unwrap();
            let Some(f) = frame["f"].as_str() else {
                continue;
            };
            let Ok(line) = chan.open(f) else { continue };
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            if v["kind"] == "response" && v["id"] == id {
                assert_eq!(v["ok"], true, "{v}");
                return v["result"].clone();
            }
        }
    }
    let size = formless_core::daemon::MAX_ATTACHMENT_BYTES;
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let (id, f) = call(
        &mut chan,
        serde_json::json!({"type": "attach_begin", "session_id": sid, "name": "big.bin", "bytes": size}),
    );
    ws.send(wrap(f)).await.unwrap();
    let begun = answer(&mut ws, &mut chan, id).await;
    let upload = begun["upload_id"].as_str().unwrap().to_string();
    let step = begun["chunk_bytes"].as_u64().unwrap() as usize;
    let mut biggest = 0;
    for (index, piece) in data.chunks(step).enumerate() {
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, piece);
        let (id, f) = call(
            &mut chan,
            serde_json::json!({"type": "attach_chunk", "upload_id": upload, "index": index, "data_b64": b64}),
        );
        let frame = serde_json::json!({"cid": "u", "f": f}).to_string();
        biggest = biggest.max(frame.len());
        ws.send(Message::Text(frame)).await.unwrap();
        answer(&mut ws, &mut chan, id).await;
    }
    assert!(
        biggest < 1_000_000,
        "every relay frame stays under 1 MB: {biggest}"
    );
    let (id, f) = call(
        &mut chan,
        serde_json::json!({"type": "attach_finish", "upload_id": upload}),
    );
    ws.send(wrap(f)).await.unwrap();
    let att = answer(&mut ws, &mut chan, id).await;
    assert_eq!(att["bytes"], size);
    let pending = formless_core::Store::open(&paths.db_file())
        .unwrap()
        .pending_attachments(&sid)
        .unwrap();
    let stored = std::fs::read(pending[0]["path"].as_str().unwrap()).unwrap();
    assert!(stored == data, "the file arrives whole");
    shutdown.cancel();
    link.await.unwrap().unwrap();
}

#[test]
fn daemon_claim_matches_worker_browser_vector_without_exposing_pairing_key_in_url() {
    use formless_core::relay::{daemon_authorization, daemon_claim};
    let key: [u8; 32] = std::array::from_fn(|i| i as u8);
    let claim = daemon_claim(&key);
    let claim_hex: String = claim.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(
        claim_hex,
        "dd8607ecc7b952bb2c46e93f9387c239f50599aaad3a67036b743df4ab64b061"
    );
    assert_eq!(room_id(&key), "c24c18b87313893139ea9647c754be6d");
    let header = daemon_authorization(&key);
    assert_eq!(header.to_str().unwrap(), format!("Bearer {claim_hex}"));
    assert!(header.is_sensitive());
    assert!(!format!("{header:?}").contains(&claim_hex));
    let secret = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, key);
    let url = relay_ws_url("wss://relay.invalid", &secret).unwrap();
    assert!(!url.contains(&claim_hex) && !url.contains(&secret));
    assert_eq!(
        url,
        "wss://relay.invalid/relay/c24c18b87313893139ea9647c754be6d?role=daemon&mux=1&transport=1"
    );
}

/// Worker/Node tests enforce the actual room policy. This private peer enforces
/// the same fixed commitment and checks the real Rust Upgrade headers, denied
/// squats and restart transport (no Node installation or hosted relay needed).
#[tokio::test]
async fn daemon_upgrade_proof_rejects_squat_and_wrong_claim_and_is_stable_on_restart() {
    use formless_core::relay::daemon_authorization;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::{
        client::IntoClientRequest,
        handshake::server::{ErrorResponse, Request, Response},
        protocol::{frame::coding::CloseCode, CloseFrame},
    };
    let _env = credential_env::EnvGuard::isolated();
    let paths = temp_paths("claim-proof");
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let secret = generate_relay_secret();
    let key = key_from_secret(&secret).unwrap();
    let expected = daemon_authorization(&key).to_str().unwrap().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let url = relay_ws_url(&base, &secret).unwrap();
    let expected_path = format!("/relay/{}", room_id(&key));
    let (first_ready, ready) = tokio::sync::oneshot::channel();
    let (restart_ready, restarted) = tokio::sync::oneshot::channel();
    let (release, finish) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let verify = move |request: &Request,
                           response: Response|
              -> std::result::Result<Response, ErrorResponse> {
            assert_eq!(request.uri().path(), expected_path);
            assert!(!request.uri().to_string().contains(&expected));
            if request
                .headers()
                .get("Authorization")
                .and_then(|v| v.to_str().ok())
                == Some(expected.as_str())
            {
                Ok(response)
            } else {
                Err(tokio_tungstenite::tungstenite::http::Response::builder()
                    .status(401)
                    .body(Some("daemon authentication required".into()))
                    .unwrap())
            }
        };
        for _ in 0..2 {
            let (tcp, _) = listener.accept().await.unwrap();
            assert!(
                tokio_tungstenite::accept_hdr_async(tcp, verify.clone())
                    .await
                    .is_err(),
                "unauthorized peers must not occupy a slot"
            );
        }
        let (tcp, _) = listener.accept().await.unwrap();
        let mut old = tokio_tungstenite::accept_hdr_async(tcp, verify.clone())
            .await
            .unwrap();
        first_ready.send(()).unwrap();
        let (tcp, _) = listener.accept().await.unwrap();
        let mut new = tokio_tungstenite::accept_hdr_async(tcp, verify)
            .await
            .unwrap();
        old.close(Some(CloseFrame {
            code: CloseCode::Library(4003),
            reason: "daemon connection replaced".into(),
        }))
        .await
        .unwrap();
        restart_ready.send(()).unwrap();
        finish.await.unwrap();
        new.close(None).await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        for authorization in [None, Some("Bearer ".to_owned() + &"00".repeat(32))] {
            let mut request = url.as_str().into_client_request().unwrap();
            if let Some(header) = authorization {
                request
                    .headers_mut()
                    .insert("Authorization", header.parse().unwrap());
            }
            let error = tokio_tungstenite::connect_async(request).await.unwrap_err();
            match error {
                tokio_tungstenite::tungstenite::Error::Http(response) => {
                    assert_eq!(response.status(), 401)
                }
                other => panic!("unexpected rejection: {other}"),
            }
        }
        let shutdown = tokio_util::sync::CancellationToken::new();
        let first = tokio::spawn({
            let (d, s, t, b) = (
                daemon.clone(),
                secret.clone(),
                shutdown.clone(),
                base.clone(),
            );
            async move { formless_core::relay::connect_once(d, &b, &s, &t).await }
        });
        ready.await.unwrap();
        let second_paths = temp_paths("claim-restart");
        let second_daemon =
            Arc::new(formless_core::daemon::Daemon::new(second_paths.clone()).unwrap());
        let second = tokio::spawn({
            let (d, s, t, b) = (
                second_daemon.clone(),
                secret.clone(),
                shutdown.clone(),
                base.clone(),
            );
            async move { formless_core::relay::connect_once(d, &b, &s, &t).await }
        });
        restarted.await.unwrap();
        first.await.unwrap().unwrap();
        release.send(()).unwrap();
        second.await.unwrap().unwrap();
        peer.await.unwrap();
        drop(second_daemon);
        std::fs::remove_dir_all(second_paths.data_dir.parent().unwrap()).unwrap();
    })
    .await
    .expect("bounded claim and restart exchange");
    drop(daemon);
    std::fs::remove_dir_all(paths.data_dir.parent().unwrap()).unwrap();
}

/// A complete maximum-sized IPC request traverses the real TCP/WebSocket,
/// fragments, authenticated E2E channel and daemon reader without truncation.
#[tokio::test(flavor = "multi_thread")]
async fn exact_32mib_ipc_request_crosses_the_native_relay_wire() {
    let _env = credential_env::EnvGuard::isolated();
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::Message;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(temp_paths("relay32m")).unwrap());
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
    let mut ws = RoomPeer::accept(tcp).await;
    let bh = new_hello();
    ws.send(Message::Text(
        serde_json::json!({"cid":"max","f":hello_frame(&bh)}).to_string(),
    ))
    .await
    .unwrap();
    let first = ws.next().await.unwrap().unwrap();
    let first: serde_json::Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
    let dh = parse_hello(first["f"].as_str().unwrap()).unwrap();
    ws.send(Message::Text(
        serde_json::json!({"cid":"max","f":confirm_frame(&key,&bh,&dh)}).to_string(),
    ))
    .await
    .unwrap();
    let mut channel = Channel::new(&key, &bh, &dh, Side::Browser);
    // Unknown envelope metadata is ignored by the typed protocol decoder. Its
    // exact byte count still exercises the complete 32MiB transport boundary.
    let prefix = r#"{"id":701,"protocol":1,"request":{"type":"ping"},"private_padding":""#;
    let mut line = String::with_capacity(formless_protocol::MAX_FRAME_BYTES);
    line.push_str(prefix);
    line.extend(std::iter::repeat_n(
        'x',
        formless_protocol::MAX_FRAME_BYTES - prefix.len() - 2,
    ));
    line.push_str("\"}");
    assert_eq!(line.len(), formless_protocol::MAX_FRAME_BYTES);
    let encrypted = channel.seal(&line).unwrap();
    drop(line);
    ws.send(Message::Text(
        serde_json::json!({"cid":"max","f":encrypted}).to_string(),
    ))
    .await
    .unwrap();
    loop {
        let frame = ws.next().await.unwrap().unwrap();
        let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        let line = channel.open(frame["f"].as_str().unwrap()).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        if response["kind"] == "response" && response["id"] == 701 {
            assert_eq!(response["ok"], true, "{response}");
            break;
        }
    }
    assert!(ws.biggest <= 64 * 1024 + 1024);
    assert!(ws.biggest > 64 * 1024);
    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), link)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(daemon.subscriber_count(), 0);
}

#[tokio::test]
async fn native_relay_client_cap_rejects_unconfirmed_peers_without_core_subscribers() {
    let _env = credential_env::EnvGuard::isolated();
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::Message;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(temp_paths("relaycap")).unwrap());
    let secret = generate_relay_secret();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("ws://{}", listener.local_addr().unwrap());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let link = tokio::spawn({
        let (d, s, t) = (daemon.clone(), secret.clone(), shutdown.clone());
        async move { formless_core::relay::connect_once(d, &base, &s, &t).await }
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut ws = RoomPeer::accept(tcp).await;
    for n in 0..17 {
        ws.send(Message::Text(
            serde_json::json!({"relay":"client_open","cid":format!("c{n}")}).to_string(),
        ))
        .await
        .unwrap();
    }
    let closed = ws.next().await.unwrap().unwrap();
    let closed: serde_json::Value = serde_json::from_str(closed.to_text().unwrap()).unwrap();
    assert_eq!(closed["relay"], "disconnect");
    assert_eq!(closed["cid"], "c16");
    assert_eq!(closed["code"], 1008);
    assert_eq!(daemon.subscriber_count(), 0);
    assert!(daemon
        .handle(formless_protocol::Request::ListRelayClients)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), link)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// The output boundary is exercised separately from the maximum UP Ping:
/// an owned stored message produces an exact32MiB actual GetSession response.
#[tokio::test(flavor = "multi_thread")]
async fn exact_32mib_session_reply_crosses_the_native_relay_wire_with_all_fragment_credits() {
    let _env = credential_env::EnvGuard::isolated();
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::Message;
    let paths = temp_paths("relay32down");
    let mut cfg = formless_core::Config::default();
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    // Both calibration states must have the same derived Session preview:
    // its 80-character prefix plus ellipsis is part of the actual RPC envelope.
    let seed = "d".repeat(128);
    let (sid, mid) = {
        let store = formless_core::Store::open(&paths.db_file()).unwrap();
        let sid = store.create_session(None).unwrap().id;
        let mid = store.append_message(&sid, "user", &seed).unwrap().id;
        (sid, mid)
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let snapshot = daemon
        .handle(formless_protocol::Request::GetSession { id: sid.clone() })
        .await
        .unwrap();
    let preview = snapshot["session"]["preview"].clone();
    let base = formless_protocol::ServerMessage::ok(801, snapshot);
    let overhead = serde_json::to_vec(&base).unwrap().len();
    drop(base);
    let content = "d".repeat(formless_protocol::MAX_FRAME_BYTES - overhead + seed.len());
    // Fixture preparation touches only the owned message's content. Keeping
    // timestamps fixed makes exact wire calibration independent of clock text.
    {
        let db = rusqlite::Connection::open(paths.db_file()).unwrap();
        assert_eq!(
            db.execute(
                "UPDATE messages SET content=?1 WHERE id=?2",
                rusqlite::params![&content, mid]
            )
            .unwrap(),
            1
        );
    }
    let expected_content_hash = Sha256::digest(content.as_bytes());
    drop(content);
    let snapshot = daemon
        .handle(formless_protocol::Request::GetSession { id: sid.clone() })
        .await
        .unwrap();
    assert_eq!(
        snapshot["session"]["preview"], preview,
        "derived preview is stable while only message bytes change"
    );
    let expected =
        serde_json::to_vec(&formless_protocol::ServerMessage::ok(801, snapshot)).unwrap();
    assert_eq!(
        expected.len(),
        formless_protocol::MAX_FRAME_BYTES,
        "actual RPC calibration is exactly the documented boundary"
    );
    let expected_frame_hash = Sha256::digest(&expected);
    drop(expected);
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
    let mut peer = RoomPeer::accept(tcp).await;
    let bh = new_hello();
    peer.send(Message::Text(
        serde_json::json!({"cid":"maxdown","f":hello_frame(&bh)}).to_string(),
    ))
    .await
    .unwrap();
    let reply = peer.next().await.unwrap().unwrap();
    let reply: serde_json::Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    let dh = parse_hello(reply["f"].as_str().unwrap()).unwrap();
    peer.send(Message::Text(
        serde_json::json!({"cid":"maxdown","f":confirm_frame(&key,&bh,&dh)}).to_string(),
    ))
    .await
    .unwrap();
    let mut channel = Channel::new(&key, &bh, &dh, Side::Browser);
    let request =
        serde_json::json!({"id":801,"protocol":1,"request":{"type":"get_session","id":sid}})
            .to_string();
    let cipher = channel.seal(&request).unwrap();
    peer.send(Message::Text(
        serde_json::json!({"cid":"maxdown","f":cipher}).to_string(),
    ))
    .await
    .unwrap();
    let reply = peer.next().await.unwrap().unwrap();
    let reply: serde_json::Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
    assert_eq!(reply["cid"], "maxdown");
    let line = channel.open(reply["f"].as_str().unwrap()).unwrap();
    assert_eq!(line.len(), formless_protocol::MAX_FRAME_BYTES);
    assert_eq!(
        Sha256::digest(line.as_bytes()),
        expected_frame_hash,
        "entire actual RPC reply survives windowed native transport exactly"
    );
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["id"], 801);
    assert_eq!(
        Sha256::digest(
            response["result"]["messages"][0]["content"]
                .as_str()
                .unwrap()
                .as_bytes()
        ),
        expected_content_hash
    );
    assert!(
        peer.down_fragments > 8,
        "actual output spans repeated credit windows"
    );
    assert!(peer.biggest <= 64 * 1024 + 1024);
    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), link)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(daemon.subscriber_count(), 0);
}
