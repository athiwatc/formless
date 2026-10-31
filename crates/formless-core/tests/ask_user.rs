//! `ask_user` (SPEC §5.5): the agent asks mid-Run; a client answers.
#![cfg(unix)]

use formless_core::Paths;
use formless_protocol::{
    Event, QuestionAnswer, Request, RequestEnvelope, ServerMessage, PROTOCOL_VERSION,
};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_user_fails_fast_headless_and_waits_for_a_client_answer() {
    let paths = temp_paths("ask");
    formless_core::Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let sid = formless_core::Store::open(&paths.db_file())
        .unwrap()
        .create_session(None)
        .unwrap()
        .id;
    // Nobody connected: an immediate, explanatory error.
    let headless = daemon
        .call_tool_direct(
            &sid,
            "ask_user",
            serde_json::json!({"question": "Which file?"}),
        )
        .await;
    assert!(headless.contains("no user is connected"), "{headless}");

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
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while daemon.subscriber_count() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("no subscriber registered within 10 seconds");

    let asking = tokio::spawn({
        let (d, sid) = (daemon.clone(), sid.clone());
        async move {
            d.call_tool_direct(
                &sid,
                "ask_user",
                serde_json::json!({"question": "Which format?", "options": ["csv", "json"]}),
            )
            .await
        }
    });
    let question_id = loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .expect("no question")
            .unwrap()
            .unwrap();
        if let Ok(ServerMessage::Event(Event::UserQuestion {
            question_id,
            question,
            options,
            questions,
            ..
        })) = serde_json::from_str::<ServerMessage>(&line)
        {
            assert_eq!(question, "Which format?");
            assert_eq!(options, ["csv", "json"]);
            assert_eq!(questions.len(), 1);
            assert_eq!(questions[0].options[1].label, "json");
            break question_id;
        }
    };
    let env = RequestEnvelope {
        id: 1,
        protocol: PROTOCOL_VERSION,
        request: Request::AnswerQuestion {
            question_id,
            answer: "json".into(),
            answers: vec![],
        },
    };
    w.write_all(format!("{}\n", serde_json::to_string(&env).unwrap()).as_bytes())
        .await
        .unwrap();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), asking)
        .await
        .expect("answer never arrived")
        .unwrap();
    assert_eq!(out, r#"{"answer":"json"}"#);

    // Several structured questions, answered with structured answers.
    let asking = tokio::spawn({
        let (d, sid) = (daemon.clone(), sid.clone());
        async move {
            d.call_tool_direct(
                &sid,
                "ask_user",
                serde_json::json!({"questions": [
                    {"question": "Which formats?", "header": "Format", "multi_select": true,
                     "options": [{"label": "csv", "description": "Spreadsheets"}, {"label": "json"}]},
                    {"question": "Where to?"}
                ]}),
            )
            .await
        }
    });
    let question_id = loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .expect("no question")
            .unwrap()
            .unwrap();
        if let Ok(ServerMessage::Event(Event::UserQuestion {
            question_id,
            question,
            options,
            questions,
            ..
        })) = serde_json::from_str::<ServerMessage>(&line)
        {
            // Older clients see a numbered summary and no options.
            assert_eq!(question, "1. Which formats?\n2. Where to?");
            assert!(options.is_empty());
            assert_eq!(questions[0].header.as_deref(), Some("Format"));
            assert!(questions[0].multi_select);
            assert_eq!(
                questions[0].options[0].description.as_deref(),
                Some("Spreadsheets")
            );
            break question_id;
        }
    };
    let env = RequestEnvelope {
        id: 2,
        protocol: PROTOCOL_VERSION,
        request: Request::AnswerQuestion {
            question_id,
            answer: "csv, json; ./out".into(),
            answers: vec![
                QuestionAnswer {
                    selected: vec!["csv".into(), "json".into()],
                    text: None,
                },
                QuestionAnswer {
                    selected: vec![],
                    text: Some("./out".into()),
                },
            ],
        },
    };
    w.write_all(format!("{}\n", serde_json::to_string(&env).unwrap()).as_bytes())
        .await
        .unwrap();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), asking)
        .await
        .expect("answer never arrived")
        .unwrap();
    let out: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        out,
        serde_json::json!({"answers": [
            {"question": "Which formats?", "selected": ["csv", "json"]},
            {"question": "Where to?", "selected": [], "text": "./out"}
        ]})
    );

    // Too many questions is an error the agent can act on.
    let five: Vec<_> = (0..5)
        .map(|i| serde_json::json!({"question": format!("q{i}")}))
        .collect();
    let err = daemon
        .call_tool_direct(&sid, "ask_user", serde_json::json!({"questions": five}))
        .await;
    assert!(err.contains("1 to 4 questions"), "{err}");
}
