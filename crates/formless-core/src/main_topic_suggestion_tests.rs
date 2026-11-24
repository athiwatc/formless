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
        "workspace": repo,
        "message_ids": [id],
        "outline": [
            {"title": "Implement", "instructions": "Implement the feature", "harness": "claude",
                "model": "opus", "access": "write"},
            {"title": "Review", "instructions": "Review the change", "harness": "codex",
                "model": "sol", "access": "read_only"},
        ],
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
                failed && text.contains("only available to the Main or Topic Chat assistant"),
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
            assert_eq!(system.matches("Topic suggestions:").count(), 1);
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
    assert_eq!(topic.workspace.as_deref(), Some(repo.as_path()));
    assert!(topic.parent_topic_id.is_none());
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
                    .next_back(),
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

#[tokio::test]
async fn topic_suggestion_dedup_rejects_edited_source_messages() {
    let (_fixture, daemon) = Fixture::new(&Config::default());
    let (main, message) = daemon
        .with_store(|st| {
            let main = st.main_chat()?;
            let message = st.append_message(&main.id, "user", "Add the original feature")?;
            Ok((main, message))
        })
        .unwrap();
    let args = |request: &str| {
        json!({
            "name": "Feature",
            "message_ids": [message.id],
            "features": [{"source_message_id": message.id, "request": request}],
        })
    };
    let (created, failed) = dispatch(
        &daemon,
        &main.id,
        0,
        PROPOSE_TOPIC,
        args("Add the original feature"),
    )
    .await;
    assert!(!failed, "{created}");
    let topic: TopicInfo = serde_json::from_str(&created).unwrap();
    daemon
        .with_store(|st| {
            st.edit_message(&main.id, &message.id, "Add the edited feature")?;
            Ok(())
        })
        .unwrap();

    let (result, failed) = dispatch(
        &daemon,
        &main.id,
        0,
        PROPOSE_TOPIC,
        args("Add the edited feature"),
    )
    .await;
    assert!(
        failed && result.contains("changed since the pending proposal"),
        "{result}"
    );
    let current = daemon
        .with_reader(|st| {
            let current = st.get_message(&main.id, &message.id)?.unwrap();
            assert_eq!(current.content, "Add the edited feature");
            assert_eq!(st.list_topics()?.len(), 1);
            st.get_topic(&topic.id)
        })
        .unwrap()
        .unwrap();
    assert_eq!(current.status, TopicStatus::PendingApproval);
    assert!(current.attempts.is_empty());
}

#[tokio::test]
async fn topic_suggestions_reject_overlapping_pending_sources() {
    let (_fixture, daemon) = Fixture::new(&Config::default());
    let (main, first, second) = daemon
        .with_store(|st| {
            let main = st.main_chat()?;
            let first = st.append_message(&main.id, "user", "Add feature A")?;
            let second = st.append_message(&main.id, "user", "Add feature B")?;
            Ok((main, first, second))
        })
        .unwrap();
    let (created, failed) = dispatch(
        &daemon,
        &main.id,
        0,
        PROPOSE_TOPIC,
        json!({
            "name": "Feature A",
            "message_ids": [first.id],
            "features": [{"source_message_id": first.id, "request": first.content}],
        }),
    )
    .await;
    assert!(!failed, "{created}");
    let topic: TopicInfo = serde_json::from_str(&created).unwrap();

    let (result, failed) = dispatch(
        &daemon,
        &main.id,
        0,
        PROPOSE_TOPIC,
        json!({
            "name": "Features A and B",
            "message_ids": [first.id, second.id],
            "features": [
                {"source_message_id": first.id, "request": first.content},
                {"source_message_id": second.id, "request": second.content},
            ],
        }),
    )
    .await;
    assert!(
        failed && result.contains("overlap a pending topic proposal"),
        "{result}"
    );
    daemon
        .with_store(|st| {
            assert_eq!(st.list_topics()?.len(), 1);
            let approved = st.approve_topic(&topic.id)?;
            assert_eq!(approved.status, TopicStatus::Approved);
            assert_eq!(approved.features.len(), 1);
            assert_eq!(approved.features[0].source_message_id, first.id);
            assert!(st.get_message(&main.id, &first.id)?.is_none());
            assert_eq!(
                st.get_message(&main.id, &second.id)?.unwrap().content,
                second.content
            );
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn topic_context_excludes_generated_user_messages() {
    let (_fixture, daemon) = Fixture::new(&Config::default());
    let (main, generated) = daemon
        .with_store(|st| {
            let main = st.main_chat()?;
            let generated = st.append_message_generated(
                &main.id,
                "user",
                "Generated workflow kickoff",
                None,
                Some("topic_kickoff"),
            )?;
            Ok((main, generated))
        })
        .unwrap();

    let (empty, failed) = dispatch(&daemon, &main.id, 0, READ_TOPIC_CONTEXT, json!({})).await;
    assert!(!failed, "{empty}");
    assert!(serde_json::from_str::<Value>(&empty).unwrap()["messages"]
        .as_array()
        .unwrap()
        .is_empty());

    let normal = daemon
        .with_store(|st| st.append_message(&main.id, "user", "A real request"))
        .unwrap();
    let (context, failed) = dispatch(&daemon, &main.id, 0, READ_TOPIC_CONTEXT, json!({})).await;
    assert!(!failed, "{context}");
    let context: Value = serde_json::from_str(&context).unwrap();
    let messages = context["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["id"], normal.id);
    assert_ne!(messages[0]["id"], generated.id);

    let (result, failed) = dispatch(
        &daemon,
        &main.id,
        0,
        PROPOSE_TOPIC,
        json!({
            "name": "Invalid generated proposal",
            "message_ids": [generated.id],
            "features": [{"source_message_id": generated.id, "request": "Do not propose this"}],
        }),
    )
    .await;
    assert!(
        failed && result.contains("generated messages cannot be proposed"),
        "{result}"
    );
}

#[tokio::test]
async fn main_topic_suggests_accounting_outline_without_workspace() {
    let (_fixture, daemon) = Fixture::new(&Config::default());
    let request = "Set up accounting: scan receipts, do accounting, then create reports.";
    let (main, message) = daemon
        .with_store(|st| {
            let main = st.main_chat()?;
            let message = st.append_message(&main.id, "user", request)?;
            Ok((main, message))
        })
        .unwrap();
    let outline = json!([
        {"title": "Scan receipts", "instructions": "Read and extract receipt details.",
            "harness": "acp", "acp_agent": "gemini", "access": "read_only"},
        {"title": "Do accounting", "instructions": "Reconcile entries against receipts.",
            "harness": "acp", "acp_agent": "gemini", "access": "write"},
        {"title": "Create reports", "instructions": "Summarize the reconciled entries.",
            "access": "write"},
    ]);
    let mut args = json!({
        "name": "Accounting",
        "message_ids": [message.id],
        "outline": outline,
        "features": [{"source_message_id": message.id, "request": request}],
    });
    let (result, failed) = dispatch(&daemon, &main.id, 0, PROPOSE_TOPIC, args.clone()).await;
    assert!(!failed, "{result}");
    let topic: TopicInfo = serde_json::from_str(&result).unwrap();
    assert_eq!(topic.status, TopicStatus::PendingApproval);
    assert!(topic.workspace.is_none());
    assert_eq!(topic.outline.len(), 3);
    assert_eq!(topic.outline[0].title, "Scan receipts");
    assert!(topic.outline[0].harness.is_some());
    assert_eq!(topic.outline[0].harness, topic.outline[1].harness);
    assert_eq!(topic.outline[0].acp_agent.as_deref(), Some("gemini"));
    assert_eq!(topic.outline[1].title, "Do accounting");
    assert_eq!(topic.outline[2].title, "Create reports");
    assert!(topic.outline[2].harness.is_none());
    assert!(topic.attempts.is_empty());
    args["outline"][0]["acp_agent"] = Value::Null;
    let (result, failed) = dispatch(&daemon, &main.id, 0, PROPOSE_TOPIC, args).await;
    assert!(failed && result.contains("acp_agent"), "{result}");
}

#[tokio::test]
async fn topic_chat_suggests_story_under_current_project() {
    let (_fixture, daemon) = Fixture::new(&Config::default());
    let (main, message) = daemon
        .with_store(|st| {
            let main = st.main_chat()?;
            let message = st.append_message(&main.id, "user", "Organize Project A")?;
            Ok((main, message))
        })
        .unwrap();
    let (result, failed) = dispatch(
        &daemon,
        &main.id,
        0,
        PROPOSE_TOPIC,
        json!({
            "name": "Project A",
            "message_ids": [message.id],
            "features": [{"source_message_id": message.id, "request": message.content}],
        }),
    )
    .await;
    assert!(!failed, "{result}");
    let project: TopicInfo = serde_json::from_str(&result).unwrap();
    assert!(project.outline.is_empty());
    let approved: TopicInfo = serde_json::from_value(
        daemon
            .handle(Request::ApproveTopic {
                approval: formless_protocol::TopicApprovalRequest {
                    topic_id: project.id.clone(),
                },
            })
            .await
            .unwrap(),
    )
    .unwrap();
    let project_chat = approved.topic_session_id.unwrap();
    let story_message = daemon
        .with_store(|st| {
            let defs = toolreg::tool_definitions(st, Some(&project_chat), 30)?;
            for tool in [READ_TOPIC_CONTEXT, PROPOSE_TOPIC] {
                assert!(defs.iter().any(|def| def["function"]["name"] == tool));
            }
            st.append_message(&project_chat, "user", "Set up Story B in Project A")
        })
        .unwrap();
    let (context, failed) =
        dispatch(&daemon, &project_chat, 0, READ_TOPIC_CONTEXT, json!({})).await;
    assert!(!failed, "{context}");
    let context: Value = serde_json::from_str(&context).unwrap();
    assert_eq!(context["current_topic_id"], project.id);
    assert_eq!(
        context["topics"],
        json!([{
            "id": project.id, "name": "Project A", "parent_topic_id": null, "status": "approved",
        }])
    );
    assert_eq!(
        context["messages"].as_array().unwrap().last().unwrap()["id"],
        story_message.id
    );
    assert!(
        dispatch(&daemon, &project_chat, 1, READ_TOPIC_CONTEXT, json!({}))
            .await
            .1
    );

    let mut args = json!({
        "name": "Story B",
        "message_ids": [story_message.id],
        "features": [{"source_message_id": story_message.id, "request": story_message.content}],
    });
    let (result, failed) = dispatch(&daemon, &project_chat, 0, PROPOSE_TOPIC, args.clone()).await;
    assert!(!failed, "{result}");
    let story: TopicInfo = serde_json::from_str(&result).unwrap();
    assert_eq!(story.source_session_id, project_chat);
    assert_eq!(story.status, TopicStatus::PendingApproval);
    assert_eq!(story.parent_topic_id.as_deref(), Some(project.id.as_str()));
    assert!(story.topic_session_id.is_none());
    assert!(story.attempts.is_empty());
    args["parent_topic_id"] = json!(project.id);
    let (result, failed) = dispatch(&daemon, &project_chat, 0, PROPOSE_TOPIC, args).await;
    assert!(!failed, "{result}");
    assert_eq!(
        serde_json::from_str::<TopicInfo>(&result).unwrap().id,
        story.id
    );
    let attempt_session = daemon
        .with_store(|st| {
            st.claim_topic_workflow(&project.id)?;
            let attempt = st.begin_topic_attempt(
                &project.id,
                &project.features[0].id,
                "Plan",
                formless_protocol::StepAccess::ReadOnly,
                formless_protocol::HarnessKind::Claude,
                None,
                None,
                &[],
                &"fixture".repeat(5),
                approved.workspace.as_deref().unwrap(),
            )?;
            let defs = toolreg::tool_definitions(st, Some(&attempt.conversation_session_id), 30)?;
            for tool in [READ_TOPIC_CONTEXT, PROPOSE_TOPIC] {
                assert!(!defs.iter().any(|def| def["function"]["name"] == tool));
            }
            Ok(attempt.conversation_session_id)
        })
        .unwrap();
    for tool in [READ_TOPIC_CONTEXT, PROPOSE_TOPIC] {
        assert!(
            dispatch(&daemon, &attempt_session, 0, tool, json!({}))
                .await
                .1
        );
    }
}

#[tokio::test]
async fn topic_suggestion_context_is_scoped_until_user_enables_external_access() {
    use formless_protocol::{TopicProposalRequest, WorkspaceKind};
    use std::collections::BTreeSet;

    fn propose(
        st: &Store,
        source_session_id: &str,
        name: &str,
        parent_topic_id: Option<String>,
    ) -> Result<TopicInfo> {
        let message = st.append_message(source_session_id, "user", name)?;
        st.propose_topic(
            &TopicProposalRequest {
                source_session_id: source_session_id.into(),
                message_ids: vec![message.id],
                features: vec![],
                name: name.into(),
                parent_topic_id,
                workspace: None,
                outline: vec![],
            },
            WorkspaceKind::Scratch,
            None,
        )
    }

    let (_fixture, daemon) = Fixture::new(&Config::default());
    let (main, story, child_id, inside_id, scoped, all, scoped_proposals, all_proposals) = daemon
        .with_store(|st| {
            let main = st.main_chat()?.id;
            let project = propose(st, &main, "Project", None)?;
            let project = st.approve_topic(&project.id)?;
            let project_chat = project.topic_session_id.as_deref().unwrap();
            let story = propose(st, project_chat, "Story", None)?;
            let story = st.approve_topic(&story.id)?;
            let story_chat = story.topic_session_id.as_deref().unwrap();
            let child = propose(st, story_chat, "Child", None)?;
            let child = st.approve_topic(&child.id)?;
            let grandchild = propose(
                st,
                child.topic_session_id.as_deref().unwrap(),
                "Grandchild",
                None,
            )?;
            let inside = propose(st, story_chat, "Inside", None)?;
            let sibling = propose(st, project_chat, "Sibling", None)?;
            let other = propose(st, &main, "Unrelated", None)?;
            let other = st.approve_topic(&other.id)?;
            let outside = propose(st, story_chat, "Outside", Some(other.id.clone()))?;
            let child_id = child.id.clone();
            let inside_id = inside.id.clone();
            let scoped_proposals = BTreeSet::from([child.id.clone(), inside.id.clone()]);
            let scoped = BTreeSet::from([
                project.id,
                story.id.clone(),
                child.id,
                grandchild.id,
                inside.id.clone(),
            ]);
            let mut all = scoped.clone();
            all.extend([sibling.id, other.id, outside.id.clone()]);
            let all_proposals =
                BTreeSet::from([child_id.clone(), inside_id.clone(), outside.id.clone()]);
            Ok((
                main,
                story,
                child_id,
                inside_id,
                scoped,
                all,
                scoped_proposals,
                all_proposals,
            ))
        })
        .unwrap();
    let story_chat = story.topic_session_id.as_deref().unwrap();
    let ids = |context: &Value, collection: &str| {
        context[collection]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>()
    };
    let read = |session: &str| {
        let session = session.to_owned();
        let daemon = daemon.clone();
        async move {
            let (context, failed) =
                dispatch(&daemon, &session, 0, READ_TOPIC_CONTEXT, json!({})).await;
            assert!(!failed, "{context}");
            serde_json::from_str::<Value>(&context).unwrap()
        }
    };
    assert_eq!(ids(&read(&main).await, "topics"), all);
    let context = read(story_chat).await;
    assert_eq!(context["current_topic_id"], story.id);
    assert_eq!(ids(&context, "topics"), scoped);
    assert_eq!(ids(&context, "proposals"), scoped_proposals);
    assert!(ids(&context, "topics").contains(&inside_id));
    let sibling_id = inside_id.clone();
    let raw_context = context.to_string();
    let context_frame = crate::provenance::frame(READ_TOPIC_CONTEXT, &[], &raw_context);
    let (transcript, search, visible_search, framed_id, raw_id, user_quote_id, assistant_quote_id) =
        daemon
            .with_store(|st| {
                let framed = st.append_message_meta(
                    story_chat,
                    "tool",
                    &context_frame,
                    Some(crate::daemon::tool_result_meta(
                        "read-context-framed",
                        READ_TOPIC_CONTEXT,
                        false,
                        None,
                    )),
                )?;
                let raw = st.append_message_meta(
                    story_chat,
                    "tool",
                    &raw_context,
                    Some(crate::daemon::tool_result_meta(
                        "read-context-raw",
                        READ_TOPIC_CONTEXT,
                        false,
                        None,
                    )),
                )?;
                let user_quote = st.append_message_meta(
                    story_chat,
                    "user",
                    &raw_context,
                    Some(crate::daemon::tool_result_meta(
                        "quoted-read-context",
                        READ_TOPIC_CONTEXT,
                        false,
                        None,
                    )),
                )?;
                let assistant_quote = st.append_message(story_chat, "assistant", &raw_context)?;
                let transcript = st.topic_read_conversation(&child_id, story_chat)?;
                let search = st.topic_search_conversations(&child_id, &sibling_id)?;
                let visible_search = st.topic_search_conversations(&child_id, &child_id)?;
                Ok((
                    transcript,
                    search,
                    visible_search,
                    framed.id,
                    raw.id,
                    user_quote.id,
                    assistant_quote.id,
                ))
            })
            .unwrap();
    let messages = transcript["messages"].as_array().unwrap();
    let message = |id: &str| {
        messages
            .iter()
            .find(|message| message["id"].as_str() == Some(id))
            .unwrap()
    };
    let framed_message = message(&framed_id);
    let framed: Value = serde_json::from_str(framed_message["content"].as_str().unwrap()).unwrap();
    let framed_context: Value = serde_json::from_str(framed["content"].as_str().unwrap()).unwrap();
    assert!(ids(&framed_context, "topics").contains(&child_id));
    assert!(!ids(&framed_context, "topics").contains(&inside_id));
    assert!(!ids(&framed_context, "proposals").contains(&inside_id));
    let raw_message = message(&raw_id);
    let raw_stored_context: Value =
        serde_json::from_str(raw_message["content"].as_str().unwrap()).unwrap();
    assert!(ids(&raw_stored_context, "topics").contains(&child_id));
    assert!(!ids(&raw_stored_context, "topics").contains(&inside_id));
    assert!(!ids(&raw_stored_context, "proposals").contains(&inside_id));
    for (id, role) in [(&user_quote_id, "user"), (&assistant_quote_id, "assistant")] {
        let pasted = message(id);
        assert_eq!(pasted["role"], role);
        assert_eq!(pasted["content"], raw_context);
    }
    let search_ids = search
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["message_id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        search_ids,
        BTreeSet::from([user_quote_id, assistant_quote_id])
    );
    assert!(search
        .as_array()
        .unwrap()
        .iter()
        .all(|result| result["snippet"].as_str().unwrap().contains(&sibling_id)));
    let visible_search_ids = visible_search
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["message_id"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert!(visible_search_ids.contains(&framed_id));
    assert!(visible_search_ids.contains(&raw_id));
    for allow in [true, false] {
        daemon
            .handle(Request::SetTopicContextAccess {
                topic_id: story.id.clone(),
                allow,
            })
            .await
            .unwrap();
        let context = read(story_chat).await;
        if allow {
            assert_eq!(ids(&context, "topics"), all);
            assert_eq!(ids(&context, "proposals"), all_proposals);
        } else {
            assert_eq!(ids(&context, "topics"), scoped);
            assert_eq!(ids(&context, "proposals"), scoped_proposals);
        }
    }
}
