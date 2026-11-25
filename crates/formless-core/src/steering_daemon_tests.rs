use super::*;
use futures_util::FutureExt;

struct Fixture {
    daemon: Arc<Daemon>,
    root: std::path::PathBuf,
    session: String,
    run: String,
    wakeups: mpsc::UnboundedReceiver<i64>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-steering-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let (session, run) = daemon
            .with_store(|st| {
                let session = st.create_session(None)?.id;
                st.append_message(&session, "user", "original task")?;
                let run = st.create_run(&session)?;
                Ok((session, run))
            })
            .unwrap();
        let (steer_tx, wakeups) = mpsc::unbounded_channel();
        daemon.runs.locked().insert(
            session.clone(),
            RunCtl {
                run_id: run.clone(),
                cancel: CancellationToken::new(),
                cancel_flag: Arc::new(AtomicBool::new(false)),
                steer_tx,
                steering: Arc::new(Default::default()),
                retry: Arc::new(provider_retry_daemon::ProviderRetry::default()),
            },
        );
        Self {
            daemon,
            root,
            session,
            run,
            wakeups,
        }
    }
    async fn steer(&self, content: &str) -> Value {
        self.daemon
            .send_message(self.session.clone(), content.into(), SendMode::Steer)
            .await
            .unwrap()
    }
    fn context(&self) -> (Vec<Value>, i64) {
        self.daemon
            .with_store(|st| {
                let mut cache = ContextCache::default();
                let built = cache.build(st, &self.session, "private fixture", 100_000, 0.8)?;
                Ok((built.0, cache.last_included_seq()))
            })
            .unwrap()
    }
    fn assert_queued_without_append(&self, expected: &[&str]) {
        self.daemon
            .with_reader(|st| {
                assert_eq!(st.list_queued_messages(&self.session)?, expected);
                assert!(!st
                    .list_messages(&self.session)?
                    .iter()
                    .any(|m| expected.contains(&m.content.as_str())));
                Ok(())
            })
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn included_tool_boundary_steer_does_not_repeat_final_request() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new();
    let ack = f.steer("redirect during tool work").await;
    assert_eq!(ack, json!({"steered":true,"run_id":f.run}));
    let (request, boundary) = f.context();
    assert!(request
        .iter()
        .any(|m| m["content"] == "redirect during tool work"));
    f.daemon.mark_context_consumed(&f.session, &f.run, boundary);
    // Even an undrained real notification has no continuation authority once
    // the next request already included its user message.
    assert_eq!(f.wakeups.try_recv().unwrap(), boundary);
    assert_eq!(
        f.daemon.extend_or_close_steering(&f.session, &f.run, false),
        SteeringOutcome::Completed
    );
    assert_eq!(f.steer("after final close").await, json!({"queued":true}));
    f.assert_queued_without_append(&["after final close"]);
}

#[tokio::test]
async fn post_snapshot_steer_extends_until_a_request_actually_includes_it() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    let (frozen, frozen_boundary) = f.context();
    f.steer("arrived after admission snapshot").await;
    assert!(!frozen
        .iter()
        .any(|m| m["content"] == "arrived after admission snapshot"));
    f.daemon
        .mark_context_consumed(&f.session, &f.run, frozen_boundary);
    assert_eq!(
        f.daemon.extend_or_close_steering(&f.session, &f.run, false),
        SteeringOutcome::Extend
    );
    let (_, included_boundary) = f.context();
    assert!(included_boundary > frozen_boundary);
    // Merely building a preview does not consume steering.
    assert_eq!(
        f.daemon.extend_or_close_steering(&f.session, &f.run, false),
        SteeringOutcome::Extend
    );
    f.daemon
        .mark_context_consumed(&f.session, &f.run, included_boundary);
    assert_eq!(
        f.daemon.extend_or_close_steering(&f.session, &f.run, false),
        SteeringOutcome::Completed
    );
}

#[tokio::test]
async fn actual_finish_publication_gap_queues_send_and_blocks_queue_drain() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    assert_eq!(
        f.daemon.extend_or_close_steering(&f.session, &f.run, false),
        SteeringOutcome::Completed
    );
    let weak = Arc::downgrade(&f.daemon);
    let sid = f.session.clone();
    let checked = Arc::new(AtomicBool::new(false));
    let observed = checked.clone();
    *f.daemon.run_publication_test.locked() = Some(Arc::new(move || {
        let daemon = weak.upgrade().unwrap();
        assert!(!daemon.runs.locked().contains_key(&sid));
        assert!(daemon.session_is_publishing(&sid));
        let ack = daemon
            .send_message(
                sid.clone(),
                "at terminal publication".into(),
                SendMode::Send,
            )
            .now_or_never()
            .expect("queued Send unexpectedly awaited")
            .unwrap();
        assert_eq!(ack, json!({"queued":true}));
        daemon.admit_next_queued_message(sid.clone(), MainChatBackend::Inherit, None);
        assert!(!daemon.runs.locked().contains_key(&sid));
        observed.store(true, Ordering::SeqCst);
    }));
    f.daemon
        .finish_run_tail(&f.session, &f.run, "completed", None)
        .await;
    assert!(checked.load(Ordering::SeqCst));
    assert!(!f.daemon.session_is_publishing(&f.session));
    f.assert_queued_without_append(&["at terminal publication"]);
    assert_eq!(
        f.daemon
            .with_reader(|st| st.run_status(&f.run))
            .unwrap()
            .as_deref(),
        Some("completed")
    );
}

#[tokio::test]
async fn cancellation_closes_new_sends_without_replaying_accepted_transcript() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.steer("accepted before explicit stop").await;
    f.daemon
        .runs
        .locked()
        .get(&f.session)
        .unwrap()
        .cancel
        .cancel();
    assert_eq!(
        f.daemon.extend_or_close_steering(&f.session, &f.run, false),
        SteeringOutcome::Interrupted
    );
    assert_eq!(f.steer("new send after stop").await, json!({"queued":true}));
    f.assert_queued_without_append(&["new send after stop"]);
    let transcript = f
        .daemon
        .with_reader(|st| st.list_messages(&f.session))
        .unwrap();
    assert_eq!(
        transcript
            .iter()
            .filter(|m| m.content == "accepted before explicit stop")
            .count(),
        1
    );
    assert_eq!(
        transcript
            .last()
            .unwrap()
            .meta
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap()["steer"]["run_id"],
        f.run
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_finish_hands_publication_gap_send_to_one_answering_successor() {
    use axum::{response::IntoResponse, routing::post, Json, Router};
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-steer-gap-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let f = Fixture::new();
    let requests = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let app = Router::new().route("/chat/completions", post(move |Json(body): Json<Value>| {
        let seen = seen.clone();
        async move {
            seen.locked().push(body);
            let reply = json!({"choices":[{"index":0,"delta":{"content":"queued work answered"},"finish_reason":"stop"}]});
            ([("content-type", "text/event-stream")], format!("data: {reply}\n\ndata: [DONE]\n\n")).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    struct Server(tokio::task::JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    f.daemon
        .with_config_edit(|cfg| {
            cfg.provider.base_url = base;
            cfg.provider.model = "private-steer-gap-model".into();
            cfg.provider
                .model_context_tokens
                .insert(cfg.provider.model.clone(), 100_000);
            cfg.provider.auto_test_models = false;
            cfg.provider.max_retries = 0;
            Ok(true)
        })
        .unwrap();
    let (tx, mut events) = mpsc::channel(256);
    f.daemon.subs.locked().push(Subscriber {
        conn: 1,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: Default::default(),
    });
    f.steer("original accepted redirect").await;
    let (_, consumed) = f.context();
    f.daemon.mark_context_consumed(&f.session, &f.run, consumed);
    assert_eq!(
        f.daemon.extend_or_close_steering(&f.session, &f.run, false),
        SteeringOutcome::Completed
    );
    let weak = Arc::downgrade(&f.daemon);
    let sid = f.session.clone();
    *f.daemon.run_publication_test.locked() = Some(Arc::new(move || {
        let daemon = weak.upgrade().unwrap();
        assert!(daemon.session_is_publishing(&sid));
        assert!(!daemon.runs.locked().contains_key(&sid));
        let ack = daemon
            .send_message(
                sid.clone(),
                "queued during final publication".into(),
                SendMode::Steer,
            )
            .now_or_never()
            .unwrap()
            .unwrap();
        assert_eq!(ack, json!({"queued":true}));
    }));
    // Unlike the narrower tail test, exercise the actual finally/drain path.
    f.daemon
        .finish_run(f.session.clone(), f.run.clone(), "completed")
        .await;
    let successor = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut old_finished = false;
        let mut successor = None;
        let mut answered = false;
        loop {
            match events.recv().await.expect("event stream closed") {
                ServerMessage::Event(Event::RunFinished { run_id, status, .. })
                    if run_id == f.run =>
                {
                    assert_eq!(status, "completed");
                    old_finished = true;
                }
                ServerMessage::Event(Event::RunStarted { run_id, .. }) => {
                    assert!(old_finished, "successor overtook terminal publication");
                    assert!(
                        successor.replace(run_id.clone()).is_none(),
                        "queue started twice"
                    );
                    assert_ne!(run_id, f.run);
                }
                ServerMessage::Event(Event::MessageComplete {
                    run_id, message, ..
                }) if Some(&run_id) == successor.as_ref() => {
                    assert_eq!(message.content, "queued work answered");
                    answered = true;
                }
                ServerMessage::Event(Event::RunFinished { run_id, status, .. })
                    if Some(&run_id) == successor.as_ref() =>
                {
                    assert_eq!(status, "completed");
                    assert!(answered);
                    break run_id;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("queued successor did not answer and finish");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while Arc::strong_count(&f.daemon) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successor task outlived fixture");
    assert!(f.daemon.runs.locked().is_empty());
    assert_eq!(
        f.daemon
            .with_reader(|st| st.queued_count(&f.session))
            .unwrap(),
        0
    );
    let transcript = f
        .daemon
        .with_reader(|st| st.list_messages(&f.session))
        .unwrap();
    for text in [
        "original accepted redirect",
        "queued during final publication",
    ] {
        assert_eq!(
            transcript.iter().filter(|m| m.content == text).count(),
            1,
            "duplicated {text}"
        );
    }
    let posted = requests.locked();
    assert_eq!(posted.len(), 1, "queue triggered duplicate provider work");
    assert_eq!(
        posted[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["content"] == "queued during final publication")
            .count(),
        1
    );
    assert_eq!(
        f.daemon
            .with_reader(|st| st.run_status(&successor))
            .unwrap()
            .as_deref(),
        Some("completed")
    );
}

#[tokio::test]
async fn completion_decision_refuses_late_cancel_by_id_or_session() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    assert_eq!(
        f.daemon.extend_or_close_steering(&f.session, &f.run, false),
        SteeringOutcome::Completed
    );
    let by_id = f
        .daemon
        .handle(Request::CancelRunId {
            run_id: f.run.clone(),
        })
        .await
        .unwrap();
    assert_eq!(by_id, json!({"run_id":f.run,"interrupted":false}));
    assert_eq!(
        f.daemon
            .handle(Request::InterruptRun {
                session_id: f.session.clone()
            })
            .await
            .unwrap(),
        json!({"interrupted":false})
    );
    let control = f.daemon.runs.locked().get(&f.session).unwrap().clone();
    assert!(!control.cancel.is_cancelled());
    assert!(!control.cancel_flag.load(Ordering::SeqCst));
    f.daemon
        .finish_run_tail(&f.session, &f.run, "completed", None)
        .await;
    assert_eq!(
        f.daemon
            .with_reader(|st| st.run_status(&f.run))
            .unwrap()
            .as_deref(),
        Some("completed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_after_final_answer_before_close_is_interrupted_and_resume_consumes_steer() {
    use axum::{response::IntoResponse, routing::post, Json, Router};
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-cancel-close-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let mut f = Fixture::new();
    let requests = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let app = Router::new().route("/chat/completions",post(move |Json(body):Json<Value>|{
        seen.locked().push(body);
        async move {
            let reply=json!({"choices":[{"index":0,"delta":{"content":"final answer"},"finish_reason":"stop"}]});
            ([("content-type","text/event-stream")],format!("data: {reply}\n\ndata: [DONE]\n\n")).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    struct Server(tokio::task::JoinHandle<()>);
    impl Drop for Server {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    f.daemon
        .with_config_edit(|cfg| {
            cfg.provider.base_url = base;
            cfg.provider.model = "private-cancel-close-model".into();
            cfg.provider
                .model_context_tokens
                .insert(cfg.provider.model.clone(), 100_000);
            cfg.provider.auto_test_models = false;
            cfg.provider.max_retries = 0;
            Ok(true)
        })
        .unwrap();
    let (tx, mut events) = mpsc::channel(256);
    f.daemon.subs.locked().push(Subscriber {
        conn: 1,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: Default::default(),
    });
    let weak = Arc::downgrade(&f.daemon);
    let sid = f.session.clone();
    let rid = f.run.clone();
    *f.daemon.run_final_decision_test.locked() = Some(Arc::new(move || {
        let daemon = weak.upgrade().unwrap();
        assert_eq!(
            daemon
                .send_message(
                    sid.clone(),
                    "unconsumed final-boundary steer".into(),
                    SendMode::Steer
                )
                .now_or_never()
                .unwrap()
                .unwrap(),
            json!({"steered":true,"run_id":rid})
        );
        assert_eq!(
            daemon
                .handle(Request::CancelRunId {
                    run_id: rid.clone()
                })
                .now_or_never()
                .unwrap()
                .unwrap(),
            json!({"run_id":rid,"interrupted":true})
        );
    }));
    let control = f.daemon.runs.locked().get(&f.session).unwrap().clone();
    let wakeups = std::mem::replace(&mut f.wakeups, mpsc::unbounded_channel().1);
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        f.daemon.clone().run_loop_inner(
            f.session.clone(),
            f.run.clone(),
            control.cancel,
            control.cancel_flag,
            wakeups,
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        f.daemon
            .with_reader(|st| st.run_status(&f.run))
            .unwrap()
            .as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        f.daemon
            .with_reader(|st| st.run_error(&f.run))
            .unwrap()
            .as_deref(),
        Some("interrupted by you")
    );
    assert_eq!(
        requests.locked().len(),
        1,
        "cancel must not replay accepted steering automatically"
    );
    assert!(!requests.locked()[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["content"] == "unconsumed final-boundary steer"));
    let mut old_finished = false;
    while let Ok(message) = events.try_recv() {
        if let ServerMessage::Event(Event::RunFinished { run_id, status, .. }) = message {
            assert_eq!(run_id, f.run);
            assert_eq!(status, "interrupted");
            old_finished = true;
        }
    }
    assert!(old_finished);
    assert_eq!(
        f.daemon
            .handle(Request::ResumeRun {
                session_id: f.session.clone()
            })
            .await
            .unwrap(),
        json!({"started":true})
    );
    let successor = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let ServerMessage::Event(Event::RunFinished { run_id, status, .. }) =
                events.recv().await.unwrap()
            {
                assert_ne!(run_id, f.run);
                assert_eq!(status, "completed");
                break run_id;
            }
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while Arc::strong_count(&f.daemon) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let posted = requests.locked();
    assert_eq!(posted.len(), 2);
    let messages = posted[1]["messages"].as_array().unwrap();
    // Resume appends a harness note. ContextCache intentionally joins such a
    // note to the preceding user message for strictly alternating templates.
    let expected = format!(
        "unconsumed final-boundary steer\n\n[harness note] {}",
        crate::forecast::resume_note("interrupted")
    );
    let diagnostics: Vec<_> = messages
        .iter()
        .map(|message| {
            let content = message["content"].as_str().unwrap_or_default();
            let tail: String = content
                .chars()
                .rev()
                .take(160)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            json!({"role":message["role"],"chars":content.chars().count(),"tail":tail})
        })
        .collect();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["role"] == "user" && m["content"] == expected)
            .count(),
        1,
        "Resume must include the exact steer with its expected harness note: {diagnostics:?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter_map(|m| m["content"].as_str())
            .map(|text| text.matches("unconsumed final-boundary steer").count())
            .sum::<usize>(),
        1,
        "Resume must carry the steer once, without duplication: {diagnostics:?}"
    );
    let transcript = f
        .daemon
        .with_reader(|st| st.list_messages(&f.session))
        .unwrap();
    assert_eq!(
        transcript
            .iter()
            .filter(|m| m.role == "user" && m.content == "unconsumed final-boundary steer")
            .count(),
        1
    );
    assert_eq!(
        f.daemon
            .with_reader(|st| st.run_status(&successor))
            .unwrap()
            .as_deref(),
        Some("completed")
    );
}
