use super::*;
use crate::main_topic_suggestions::{PROPOSE_TOPIC, READ_TOPIC_CONTEXT};
use axum::response::IntoResponse;

const WORK: &str = "Lets work in Formless to add X feature, Y feature";

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new(config: &Config) -> (Self, Arc<Daemon>) {
        let dir = std::env::temp_dir().join(format!("fl-main-topic-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(dir.join("data"), dir.join("config")).unwrap();
        config.save(&paths).unwrap();
        (Self(dir), Arc::new(Daemon::new(paths).unwrap()))
    }

    fn repo(&self) -> std::path::PathBuf {
        let repo = self.0.join("Formless");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "initial",
            ],
        ] {
            assert!(std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        repo.canonicalize().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn proposal(repo: &std::path::Path, id: &str) -> Value {
    json!({
        "name": "Formless",
        "repo": repo,
        "message_ids": [id],
        "builder": "claude",
        "builder_model": "opus",
        "reviewer": "codex",
        "reviewer_model": "sol",
        "features": [
            {"source_message_id": id, "request": "Add X feature"},
            {"source_message_id": id, "request": "Add Y feature"},
        ],
    })
}

async fn dispatch(
    daemon: &Arc<Daemon>,
    session: &str,
    depth: usize,
    name: &str,
    args: Value,
) -> (String, bool) {
    daemon
        .dispatch_call_checked(
            session,
            "fixture",
            &ToolCall {
                id: "fixture-call".into(),
                name: name.into(),
                arguments: args.to_string(),
            },
            &Config::default(),
            &Arc::new(AtomicBool::new(false)),
            &CancellationToken::new(),
            &HashMap::new(),
            depth,
        )
        .await
}

#[tokio::test]
async fn main_topic_tools_are_scoped_and_validate_source_ids() {
    let (fixture, daemon) = Fixture::new(&Config::default());
    let repo = fixture.repo();
    let (main, other, source, foreign, assistant) = daemon
        .with_store(|st| {
            let other = st.create_session(Some("Main".into()))?;
            let main = st.main_chat()?;
            assert_eq!(st.main_chat()?.id, main.id);
            let source = st.append_message(&main.id, "user", WORK)?;
            let foreign = st.append_message(&other.id, "user", WORK)?;
            let assistant = st.append_message(&main.id, "assistant", WORK)?;
            for (session, expected) in [
                (Some(main.id.as_str()), true),
                (Some(other.id.as_str()), false),
                (None, false),
            ] {
                for defs in [
                    toolreg::tool_definitions(st, session, 30)?,
                    toolreg::preview_tool_definitions(st, session, 30)?,
                ] {
                    for tool in [PROPOSE_TOPIC, READ_TOPIC_CONTEXT] {
                        assert_eq!(
                            defs.iter().any(|def| def["function"]["name"] == tool),
                            expected
                        );
                    }
                }
            }
            Ok((main, other, source, foreign, assistant))
        })
        .unwrap();
    for (session, depth) in [(&other.id, 0), (&main.id, 1)] {
        for (tool, args) in [
            (READ_TOPIC_CONTEXT, json!({})),
            (PROPOSE_TOPIC, proposal(&repo, &source.id)),
        ] {
            let (text, failed) = dispatch(&daemon, session, depth, tool, args).await;
            assert!(
                failed && text.contains("only available to the Main Chat assistant"),
                "{text}"
            );
        }
    }
    for id in ["not-a-message", foreign.id.as_str(), assistant.id.as_str()] {
        assert!(
            dispatch(&daemon, &main.id, 0, PROPOSE_TOPIC, proposal(&repo, id))
                .await
                .1
        );
    }
    let mut bad = proposal(&repo, &source.id);
    bad["features"][0]["source_message_id"] = json!(foreign.id);
    assert!(dispatch(&daemon, &main.id, 0, PROPOSE_TOPIC, bad).await.1);
    assert!(
        dispatch(
            &daemon,
            &main.id,
            0,
            READ_TOPIC_CONTEXT,
            json!({"session_id": other.id})
        )
        .await
        .1
    );
    assert_eq!(daemon.handle(Request::ListTopics).await.unwrap(), json!([]));
}

fn reply(call: Option<(&str, Value)>, text: &str) -> axum::response::Response {
    let (delta, finish) = match call {
        Some((name, args)) => (
            json!({"tool_calls":[{"index":0,"id":name,"type":"function",
            "function":{"name":name,"arguments":args.to_string()}}]}),
            "tool_calls",
        ),
        None => (json!({"content":text}), "stop"),
    };
    let payload = json!({"choices":[{"delta":delta,"finish_reason":finish}],
        "usage":{"prompt_tokens":1,"completion_tokens":1}});
    (
        [("content-type", "text/event-stream")],
        format!("data: {payload}\n\ndata: [DONE]\n\n"),
    )
        .into_response()
}

#[tokio::test]
async fn main_topic_chat_suggests_two_features_and_rejection_leaves_the_original() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "fixture");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.provider.model = "fixture-model".into();
    cfg.provider
        .model_context_tokens
        .insert(cfg.provider.model.clone(), 100_000);
    cfg.provider.auto_test_models = false;
    cfg.provider.max_retries = 0;
    cfg.provider.tool_use_nudge = false;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    let (fixture, daemon) = Fixture::new(&cfg);
    let repo = fixture.repo();
    let main: SessionInfo =
        serde_json::from_value(daemon.handle(Request::GetMainChat).await.unwrap()).unwrap();
    daemon
        .with_store(|st| {
            st.set_session_cwd(&main.id, repo.to_str())?;
            st.set_session_system_prompt(&main.id, "Existing cached prompt")
        })
        .unwrap();
    let app = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| async move {
            let messages = body["messages"].as_array().unwrap();
            let system = messages[0]["content"].as_str().unwrap();
            assert!(system.contains("Existing cached prompt"));
            assert_eq!(system.matches("Main Chat topic suggestions:").count(), 1);
            for tool in [READ_TOPIC_CONTEXT, PROPOSE_TOPIC] {
                assert!(body["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|def| def["function"]["name"] == tool));
            }
            if messages.last().unwrap()["content"] == "Hello!" {
                return reply(None, "Hello back!");
            }
            let last = messages.last().unwrap();
            match last["role"].as_str() {
                Some("user") => reply(Some((READ_TOPIC_CONTEXT, json!({}))), ""),
                Some("tool") => {
                    let frame: Value =
                        serde_json::from_str(last["content"].as_str().unwrap()).unwrap();
                    let data: Value =
                        serde_json::from_str(frame["content"].as_str().unwrap()).unwrap();
                    match frame["source_tool"].as_str() {
                        Some(READ_TOPIC_CONTEXT) => {
                            let message = data["messages"].as_array().unwrap().last().unwrap();
                            assert_eq!(message["content"], WORK);
                            let repo = std::path::Path::new(data["cwd"].as_str().unwrap());
                            reply(
                                Some((
                                    PROPOSE_TOPIC,
                                    proposal(repo, message["id"].as_str().unwrap()),
                                )),
                                "",
                            )
                        }
                        Some(PROPOSE_TOPIC) => {
                            assert_eq!(data["status"], "pending_approval");
                            reply(None, "Review the pending topic proposal in the client.")
                        }
                        other => panic!("unexpected tool: {other:?}"),
                    }
                }
                other => panic!("unexpected role: {other:?}"),
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (tx, mut events) = mpsc::channel(128);
    daemon.subs.locked().push(Subscriber {
        conn: 1,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    for request in ["Hello!", WORK] {
        daemon
            .handle(Request::SendMessage {
                session_id: main.id.clone(),
                content: request.into(),
                mode: SendMode::Send,
                client_msg_id: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(message) = events.recv().await {
                if let ServerMessage::Event(Event::RunFinished {
                    session_id, status, ..
                }) = message
                {
                    if session_id == main.id {
                        assert_eq!(status, "completed");
                        return;
                    }
                }
            }
            panic!("Run did not finish");
        })
        .await
        .unwrap();
        if request == "Hello!" {
            assert_eq!(daemon.handle(Request::ListTopics).await.unwrap(), json!([]));
        }
    }
    server.abort();
    let topics: Vec<TopicInfo> =
        serde_json::from_value(daemon.handle(Request::ListTopics).await.unwrap()).unwrap();
    let [topic] = topics.as_slice() else {
        panic!("expected one topic, got {topics:?}")
    };
    let original = daemon
        .with_reader(|st| st.last_user_message(&main.id))
        .unwrap()
        .unwrap();
    assert_eq!(original.2, WORK);
    assert_eq!(topic.source_session_id, main.id);
    assert_eq!(topic.status, TopicStatus::PendingApproval);
    assert!(topic.attempts.is_empty());
    assert!(topic.topic_session_id.is_none());
    assert_eq!(
        topic
            .features
            .iter()
            .map(|feature| feature.request.as_str())
            .collect::<Vec<_>>(),
        ["Add X feature", "Add Y feature"]
    );
    assert!(
        topic
            .features
            .iter()
            .all(|feature| feature.source_message_id == original.0
                && feature.source_seq == original.1)
    );
    assert_ne!(topic.features[0].id, topic.features[1].id);
    let args = proposal(&repo, &original.0);
    let (duplicate, failed) = dispatch(&daemon, &main.id, 0, PROPOSE_TOPIC, args.clone()).await;
    assert!(!failed, "{duplicate}");
    assert_eq!(
        serde_json::from_str::<TopicInfo>(&duplicate).unwrap().id,
        topic.id
    );
    let rejected: TopicInfo = serde_json::from_value(
        daemon
            .handle(Request::RejectTopic {
                rejection: TopicRejectionRequest {
                    topic_id: topic.id.clone(),
                    reason: "Keep in Main".into(),
                },
            })
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rejected.status, TopicStatus::Rejected);
    assert!(rejected.attempts.is_empty());
    assert!(rejected.topic_session_id.is_none());
    assert!(dispatch(&daemon, &main.id, 0, PROPOSE_TOPIC, args).await.1);
    daemon
        .with_reader(|st| {
            assert_eq!(st.list_topics()?.len(), 1);
            assert_eq!(st.list_sessions()?.len(), 1);
            assert_eq!(
                st.list_messages(&main.id)?
                    .into_iter()
                    .filter(|msg| msg.role == "user")
                    .map(|msg| (msg.id, msg.seq, msg.content))
                    .last(),
                Some(original)
            );
            assert_eq!(
                st.list_messages(&main.id)?
                    .into_iter()
                    .filter(|msg| msg.role == "user")
                    .count(),
                2
            );
            Ok(())
        })
        .unwrap();
}
