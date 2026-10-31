//! Request-site classifications survive the real daemon transport (#1139).
#![cfg(unix)]
#[path = "support/env.rs"]
mod credential_env;

use formless_core::{Config, Paths};
use formless_protocol::{codes, Request, RequestEnvelope, ServerMessage, PROTOCOL_VERSION};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[tokio::test]
async fn resource_and_validation_errors_have_stable_codes_over_a_connection() {
    let _env = credential_env::EnvGuard::isolated();
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let root = std::env::temp_dir().join(format!("fl-ec-{short}"));
    let paths = Paths {
        data_dir: root.join("d"),
        config_dir: root.join("c"),
        profile: format!("test-ec-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    let stream = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(s) = tokio::net::UnixStream::connect(paths.socket_path()).await {
                break s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("daemon never opened its socket");
    let (read, mut write) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(read).lines();
    let cases = vec![
        // Both the first failed send and a cached retry retain the typed code.
        (
            Request::SendMessage {
                session_id: "missing".into(),
                content: "hello".into(),
                mode: formless_protocol::SendMode::Send,
                client_msg_id: Some("cached-error".into()),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::SendMessage {
                session_id: "missing".into(),
                content: "retry".into(),
                mode: formless_protocol::SendMode::Send,
                client_msg_id: Some("cached-error".into()),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::DeleteSessionTemplate {
                name: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::GetSkill {
                name: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::RemoveProvider {
                name: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::DeleteRole {
                name: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::RunJobNow {
                id: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::DeleteMemory {
                id: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::RemoveMcpServer {
                name: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::AbandonInvocation {
                handle: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::GetSession {
                id: "missing".into(),
            },
            codes::NOT_FOUND,
        ),
        (
            Request::RateRun {
                session_id: "missing".into(),
                run_id: None,
                rating: "not a rating".into(),
                comment: None,
            },
            codes::INVALID,
        ),
        (
            Request::SaveSessionTemplate {
                id: "missing".into(),
                name: " ".into(),
                first_message: None,
            },
            codes::INVALID,
        ),
    ];
    for (n, (request, code)) in cases.into_iter().enumerate() {
        let id = n as u64 + 1;
        let env = RequestEnvelope {
            id,
            protocol: PROTOCOL_VERSION,
            request,
        };
        write
            .write_all(format!("{}\n", serde_json::to_string(&env).unwrap()).as_bytes())
            .await
            .unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let line = lines.next_line().await.unwrap().expect("daemon closed");
                let msg: ServerMessage = serde_json::from_str(&line).unwrap();
                if matches!(&msg, ServerMessage::Response { id: got, .. } if *got == id) {
                    break msg;
                }
            }
        })
        .await
        .expect("request did not receive its correlated response");
        assert_eq!(reply.error_code(), Some(code), "{reply:?}");
        assert!(matches!(reply, ServerMessage::Response { ok: false, .. }));
    }
    daemon.handle(Request::Shutdown).await.unwrap();
    drop(lines);
    drop(write);
    tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .expect("daemon never stopped")
        .unwrap()
        .unwrap();
    let pending = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while pending.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("daemon tasks outlived credential isolation");
    let _ = std::fs::remove_dir_all(root);
}
