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
