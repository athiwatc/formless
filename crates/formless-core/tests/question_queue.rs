//! Concurrent main/nested ask_user calls share one FIFO gate per Session.
#![cfg(unix)]
#[path = "support/env.rs"]
mod credential_env;
use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::{Event, Request, ServerMessage};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::io::AsyncBufReadExt;

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_questions_are_presented_and_answered_in_call_order() {
    let _env = credential_env::EnvGuard::isolated();
    let base = std::path::PathBuf::from(format!("/tmp/fl-qq-{}", uuid::Uuid::new_v4()));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: "question-queue".into(),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let sid = Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    let server = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.serve().await }
    });
    let stream = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(stream) = tokio::net::UnixStream::connect(paths.socket_path()).await {
                break stream;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("daemon socket never appeared");
    let mut lines = tokio::io::BufReader::new(stream).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        while daemon.subscriber_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("no question subscriber registered");
    let asking = tokio::spawn({
        let daemon = daemon.clone();
        let sid = sid.clone();
        async move {
            tokio::join!(
                daemon.call_tool_direct(&sid, "ask_user", json!({"question": "first?"})),
                daemon.call_tool_direct(&sid, "ask_user", json!({"question": "second?"})),
            )
        }
    });
    for (question, answer) in [("first?", "first answer"), ("second?", "second answer")] {
        let id = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("subscriber disconnected");
                if let Ok(ServerMessage::Event(Event::UserQuestion {
                    question_id,
                    question: actual,
                    ..
                })) = serde_json::from_str(&line)
                {
                    assert_eq!(actual, question);
                    break question_id;
                }
            }
        })
        .await
        .expect("next question never appeared");
        let snapshot = daemon
            .handle(Request::GetSession { id: sid.clone() })
            .await
            .unwrap();
        assert_eq!(snapshot["pending_events"].as_array().unwrap().len(), 1);
        assert_eq!(snapshot["pending_events"][0]["question_id"], id);
        // No sibling question may appear until this question is answered.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), async {
                loop {
                    let line = lines
                        .next_line()
                        .await
                        .unwrap()
                        .expect("subscriber disconnected");
                    if let Ok(ServerMessage::Event(Event::UserQuestion { .. })) =
                        serde_json::from_str::<ServerMessage>(&line)
                    {
                        break;
                    }
                }
            })
            .await
            .is_err(),
            "overlapping question event"
        );
        let reply = daemon
            .handle(Request::AnswerQuestion {
                question_id: id,
                answer: answer.into(),
                answers: vec![],
            })
            .await
            .unwrap();
        assert_eq!(reply["answered"], true);
    }
    let (first, second) = tokio::time::timeout(Duration::from_secs(10), asking)
        .await
        .expect("questions did not finish")
        .unwrap();
    assert_eq!(first, r#"{"answer":"first answer"}"#);
    assert_eq!(second, r#"{"answer":"second answer"}"#);
    daemon.handle(Request::Shutdown).await.unwrap();
    drop(lines);
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("daemon never stopped")
        .unwrap()
        .unwrap();
    let pending = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(Duration::from_secs(10), async {
        while pending.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("question daemon tasks outlived credential isolation");
    let _ = std::fs::remove_dir_all(base);
}
