//! A slow request never holds up control requests on the same connection
//! (SPEC §9.3).
#![cfg(unix)]

use formless_core::{Config, Paths};
use formless_protocol::{Request, RequestEnvelope, ServerMessage, PROTOCOL_VERSION};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread")]
async fn ping_answers_while_an_import_smoke_tests() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-cc-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-cc-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.tools.smoke_timeout_secs = 3;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (r, mut w) = tokio::io::split(stream);
    let mut lines = tokio::io::BufReader::new(r).lines();
    let send = |id: u64, request: Request| {
        format!(
            "{}\n",
            serde_json::to_string(&RequestEnvelope {
                id,
                protocol: PROTOCOL_VERSION,
                request
            })
            .unwrap()
        )
    };
    // A slow request: importing a Tool whose smoke test loops (3 s cap).
    let bundle = serde_json::json!({
        "format": "formless.tool/1", "name": "spin", "description": "loops",
        "input_schema": {"type": "object"}, "output_schema": {},
        "implementation_js": "function run(i){ while(true){} }", "capabilities": [], "memory_mb": 64
    });
    w.write_all(send(1, Request::ImportTool { bundle }).as_bytes())
        .await
        .unwrap();
    w.write_all(send(2, Request::Ping).as_bytes())
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let mut order = Vec::new();
    while order.len() < 2 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), lines.next_line())
            .await
            .expect("no reply")
            .unwrap()
            .unwrap();
        if let Ok(ServerMessage::Response { id, .. }) = serde_json::from_str::<ServerMessage>(&line)
        {
            if id == 2 {
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(2),
                    "Ping waited"
                );
            }
            order.push(id);
        }
    }
    assert_eq!(order, vec![2, 1], "the Ping overtook the slow import");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_requests_get_a_correlated_unsupported_error() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-cu-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-cu-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (r, mut w) = tokio::io::split(stream);
    let mut lines = tokio::io::BufReader::new(r).lines();
    w.write_all(b"{\"id\":7,\"protocol\":1,\"request\":{\"type\":\"teleport\"}}\n{\"id\":8,\"protocol\":1,\"request\":{\"type\":\"ping\"}}\n")
        .await
        .unwrap();
    let mut seen = 0;
    while seen < 2 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .expect("no reply")
            .unwrap()
            .unwrap();
        match serde_json::from_str::<ServerMessage>(&line) {
            Ok(ServerMessage::Response {
                id: 7,
                ok,
                result,
                error,
            }) => {
                assert!(!ok);
                assert_eq!(result.unwrap()["type"], "teleport");
                assert!(error.unwrap().starts_with("unsupported:"));
                seen += 1;
            }
            Ok(ServerMessage::Response { id: 8, result, .. }) => {
                let r = result.unwrap();
                assert_eq!(r["min_protocol"], 1);
                assert!(r["features"].as_array().unwrap().len() > 1);
                seen += 1;
            }
            _ => {}
        }
    }
}

/// A stall retry overtakes a slow request; an interrupt cancels a send
/// still queued behind it; a repeated send key sends once; a protocol
/// mismatch is coded (#885, #891).
#[tokio::test(flavor = "multi_thread")]
async fn control_ordering_and_idempotent_sends() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-co-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-co-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.tools.smoke_timeout_secs = 3;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let sid = daemon
        .handle(Request::CreateSession { name: None })
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (r, mut w) = tokio::io::split(stream);
    let mut lines = tokio::io::BufReader::new(r).lines();
    let send = |id: u64, protocol: u32, request: Request| {
        format!(
            "{}\n",
            serde_json::to_string(&RequestEnvelope {
                id,
                protocol,
                request
            })
            .unwrap()
        )
    };
    let bundle = serde_json::json!({
        "format": "formless.tool/1", "name": "spin", "description": "loops",
        "input_schema": {"type": "object"}, "output_schema": {},
        "implementation_js": "function run(i){ while(true){} }", "capabilities": [], "memory_mb": 64
    });
    for line in [
        send(1, PROTOCOL_VERSION, Request::ImportTool { bundle }),
        send(
            2,
            PROTOCOL_VERSION,
            Request::SendMessage {
                session_id: sid.clone(),
                content: "queued".into(),
                mode: formless_protocol::SendMode::Send,
                client_msg_id: None,
            },
        ),
        send(
            3,
            PROTOCOL_VERSION,
            Request::InterruptRun {
                session_id: sid.clone(),
            },
        ),
        send(
            4,
            PROTOCOL_VERSION,
            Request::RetryProviderRequest {
                session_id: sid.clone(),
            },
        ),
        send(5, 99, Request::Ping),
    ] {
        w.write_all(line.as_bytes()).await.unwrap();
    }
    let started = std::time::Instant::now();
    let mut got = std::collections::HashMap::new();
    while got.len() < 5 {
        let line = tokio::time::timeout(std::time::Duration::from_secs(20), lines.next_line())
            .await
            .expect("no reply")
            .unwrap()
            .unwrap();
        if let Ok(msg @ ServerMessage::Response { .. }) =
            serde_json::from_str::<ServerMessage>(&line)
        {
            let ServerMessage::Response { id, .. } = &msg else {
                unreachable!()
            };
            if *id == 4 {
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(2),
                    "retry waited"
                );
            }
            got.insert(*id, msg);
        }
    }
    let ServerMessage::Response { ok, error, .. } = &got[&2] else {
        unreachable!()
    };
    assert!(
        !ok && error
            .as_deref()
            .unwrap()
            .contains("interrupted before it started")
    );
    assert_eq!(got[&5].error_code(), Some("protocol_mismatch"));

    // The same key twice: one message.
    for _ in 0..2 {
        daemon
            .handle(Request::SendMessage {
                session_id: sid.clone(),
                content: "once".into(),
                mode: formless_protocol::SendMode::Send,
                client_msg_id: Some("k-1".into()),
            })
            .await
            .unwrap();
    }
    let store = formless_core::Store::open(&paths.db_file()).unwrap();
    let sent = store
        .list_messages(&sid)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == "user" && m.content == "once")
        .count();
    assert_eq!(sent, 1);
}
