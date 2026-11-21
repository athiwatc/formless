//! Actual pipeline admission and factory HTTP requests, with private files,
//! dummy credentials and an owned loopback-only provider.
use super::*;
use crate::pipeline_policy::RunPolicy;
use axum::response::IntoResponse;
use std::collections::VecDeque;
use std::time::Duration;
struct OwnedTask<T>(Option<tokio::task::JoinHandle<T>>);
impl<T> OwnedTask<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self(Some(handle))
    }
    async fn join(mut self) -> T {
        match tokio::time::timeout(Duration::from_secs(10), self.0.as_mut().unwrap()).await {
            Ok(result) => {
                self.0.take();
                result.expect("owned private provider task panicked")
            }
            Err(_) => {
                let task = self.0.take().unwrap();
                task.abort();
                let _ = task.await;
                panic!("owned private provider task exceeded ten seconds");
            }
        }
    }
}
impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}
#[derive(Clone)]
struct Reply {
    text: String,
    usage: bool,
    hold: bool,
    cost: f64,
    stop_reason: &'static str,
}
impl Reply {
    fn text(text: &str) -> Self {
        Self {
            text: text.into(),
            usage: true,
            hold: false,
            cost: 0.02,
            stop_reason: "stop",
        }
    }
}
struct Fixture {
    daemon: Option<Arc<Daemon>>,
    root: std::path::PathBuf,
    session: String,
    seen: Arc<StdMutex<Vec<Value>>>,
    replies: Arc<StdMutex<VecDeque<Reply>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Semaphore>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(env: &crate::credential_env::EnvGuard) -> Self {
        env.set("FORMLESS_API_KEY", "private-pipeline-key");
        env.set("FORMLESS_API_KEY_OTHER", "private-pipeline-named-key");
        env.set("NO_PROXY", "127.0.0.1,localhost");
        env.set("no_proxy", "127.0.0.1,localhost");
        for key in ["FORMLESS_BASE_URL", "FORMLESS_MODEL", "FORMLESS_SECURE"] {
            env.remove(key);
        }
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let replies = Arc::new(StdMutex::new(VecDeque::<Reply>::new()));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let app=axum::Router::new().route("/chat/completions",axum::routing::post({
            let (seen,replies,entered,release)=(seen.clone(),replies.clone(),entered.clone(),release.clone());
            move |axum::Json(body):axum::Json<Value>| {
                let (seen,replies,entered,release)=(seen.clone(),replies.clone(),entered.clone(),release.clone());
                async move {
                    seen.locked().push(body.clone()); entered.notify_one();
                    let Some(reply)=replies.locked().pop_front() else { return (axum::http::StatusCode::INTERNAL_SERVER_ERROR,"unexpected private fixture request").into_response(); };
                    if reply.hold { let Ok(permit)=release.acquire().await else { return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(); }; permit.forget(); }
                    let usage=if reply.usage { json!({"prompt_tokens":9,"completion_tokens":3,"cost":reply.cost}) } else { Value::Null };
                    if body["stream"] == true {
                        let start=json!({"model":"model","choices":[{"index":0,"delta":{"content":reply.text},"finish_reason":null}]});
                        let finish=json!({"model":"model","choices":[{"index":0,"delta":{},"finish_reason":reply.stop_reason}],"usage":usage});
                        ([("content-type","text/event-stream")],format!("data: {start}\n\ndata: {finish}\n\ndata: [DONE]\n\n")).into_response()
                    } else { axum::Json(json!({"model":"model","choices":[{"message":{"role":"assistant","content":reply.text},"finish_reason":reply.stop_reason}],"usage":usage})).into_response() }
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let root =
            std::env::temp_dir().join(format!("fl-pipeline-daemon-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = base.clone();
        cfg.provider.model = "model".into();
        cfg.provider.max_retries = 0;
        cfg.provider.tool_use_nudge = false;
        cfg.provider
            .model_context_tokens
            .insert("model".into(), 100_000);
        cfg.provider.prices.insert(
            "model".into(),
            crate::config::ModelPrice {
                input: 1.0,
                output: 1.0,
                ..Default::default()
            },
        );
        cfg.providers.insert("other".into(), cfg.provider.clone());
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.tools.approval_summaries = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        assert_eq!(daemon.config().unwrap().provider.base_url, base);
        let session = daemon.with_store(|st| st.create_session(None)).unwrap().id;
        Self {
            daemon: Some(daemon),
            root,
            session,
            seen,
            replies,
            entered,
            release,
            server,
        }
    }
    fn d(&self) -> &Arc<Daemon> {
        self.daemon.as_ref().unwrap()
    }
    fn queue(&self, reply: Reply) {
        self.replies.locked().push_back(reply);
    }
    fn count(&self) -> usize {
        self.seen.locked().len()
    }
    fn cfg(&self, change: impl FnOnce(&mut Config)) {
        let mut cfg = Config::load(&self.d().paths).unwrap();
        change(&mut cfg);
        cfg.save(&self.d().paths).unwrap();
    }
    async fn start(&self, options: RunOptions, key: &str) -> String {
        let result = self
            .d()
            .handle(Request::StartPipelineRun {
                session_id: self.session.clone(),
                content: "task".into(),
                options,
                client_msg_id: Some(key.into()),
            })
            .await
            .unwrap();
        result["run_id"].as_str().unwrap().into()
    }
    async fn settled(&self, run: &str) -> String {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = self
                    .d()
                    .with_reader(|st| st.run_status(run))
                    .unwrap()
                    .unwrap();
                if status != "running" && !self.d().runs.locked().contains_key(&self.session) {
                    return status;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned private pipeline Run must settle")
    }
    fn manual(&self, cap: f64) -> (String, Arc<RunPolicy>) {
        let policy = Arc::new(RunPolicy::new(
            crate::pipeline::CompiledRunOptions::compile(RunOptions {
                max_usd: Some(cap),
                ..Default::default()
            })
            .unwrap(),
        ));
        let run = self
            .d()
            .with_store(|st| {
                let run = st.create_run(&self.session)?;
                st.record_pipeline_policy(&run, &policy.marker())?;
                Ok(run)
            })
            .unwrap();
        self.d()
            .pipeline_policies
            .locked()
            .insert(&run, &policy)
            .unwrap();
        (run, policy)
    }
    async fn factory_call(
        &self,
        run: &str,
        named: Option<&str>,
    ) -> Result<crate::provider::CompletionResult> {
        let cfg = Config::load(&self.d().paths)?;
        let provider = self.d().provider_named(&cfg, named).await?;
        counted(
            "llm",
            &self.session,
            run,
            provider.complete_result(
                &[json!({"role":"user","content":"private nested fixture"})],
                "model",
                None,
            ),
        )
        .await
    }
    async fn shutdown(mut self) {
        for ctl in self.d().runs.locked().values() {
            ctl.cancel_flag.store(true, Ordering::SeqCst);
            ctl.cancel.cancel();
        }
        self.release.add_permits(32);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.d().runs.locked().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        self.server.abort();
        let _ = (&mut self.server).await;
        drop(self.daemon.take());
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(daemon) = &self.daemon {
            for ctl in daemon.runs.locked().values() {
                ctl.cancel.cancel();
            }
        }
        self.release.close();
        self.server.abort();
        drop(self.daemon.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn schema_options() -> RunOptions {
    RunOptions {
        system: Some("one-Run-only ไทย private instructions".into()),
        schema: Some(
            json!({"type":"object","required":["n"],"properties":{"n":{"type":"integer"}},"additionalProperties":false}),
        ),
        max_usd: Some(1.0),
        ..Default::default()
    }
}
#[tokio::test]
async fn pipeline_repairs_once_accounts_both_calls_and_keeps_system_out_of_session() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env).await;
    f.queue(Reply::text(
        r#"{"n":"INVALID private candidate private value"}"#,
    ));
    f.queue(Reply::text(r#"{"n":2}"#));
    let (tx, mut delivered) = mpsc::channel(256);
    f.d().subs.locked().push(Subscriber {
        conn: 100,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    let options = schema_options();
    let run = f.start(options.clone(), "repair").await;
    assert_eq!(f.settled(&run).await, "completed");
    assert_eq!(f.count(), 2);
    let mut completed = 0;
    while let Ok(message) = delivered.try_recv() {
        if let ServerMessage::Event(event) = message {
            match event {
                Event::MessageDelta { .. } | Event::ReasoningDelta { .. } => {
                    panic!("unvalidated schema candidate escaped through a delta")
                }
                Event::MessageComplete { message, .. } => {
                    assert_eq!(message.content, r#"{"n":2}"#);
                    completed += 1;
                }
                _ => {}
            }
        }
    }
    assert_eq!(completed, 1);
    let seen = f.seen.locked().clone();
    assert_eq!(seen[0]["stream"], true);
    assert_eq!(seen[1]["stream"], false);
    let instruction = seen[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|message| {
            message["content"]
                .as_str()
                .filter(|text| text.contains("validation_failure"))
        })
        .unwrap();
    assert!(
        instruction.contains("type at $/[property]"),
        "{instruction}"
    );
    assert!(!instruction.contains("INVALID private candidate"));
    assert!(!instruction.contains("private value"));
    assert!(
        seen[1]["tools"].as_array().is_none_or(Vec::is_empty),
        "repair has no executable Tools"
    );
    assert!(seen[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains(options.system.as_ref().unwrap()));
    let messages = f
        .d()
        .with_reader(|st| st.list_messages(&f.session))
        .unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.role == "assistant")
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec![r#"{"n":2}"#]
    );
    assert!(!messages
        .iter()
        .any(|m| m.content.contains("one-Run-only") || m.content.contains("INVALID private")));
    assert!(!f
        .d()
        .with_reader(|st| st.session_system_prompt(&f.session))
        .unwrap()
        .unwrap()
        .contains("one-Run-only"));
    assert!(f
        .d()
        .with_reader(|st| st.get_session(&f.session))
        .unwrap()
        .unwrap()
        .instructions
        .is_none());
    let spend = f
        .d()
        .with_reader(|st| st.pipeline_spend(&run, &Config::load(&f.d().paths)?))
        .unwrap();
    assert_eq!(spend["usage_records"], 2);
    assert_eq!(spend["total_usd"].as_f64(), Some(0.04));
    let stats = f
        .d()
        .with_reader(|st| {
            st.routing_call_stats(&Config::load(&f.d().paths)?, "default", chrono::Utc::now())
        })
        .unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].model, "model");
    assert_eq!(
        stats[0].samples, 2,
        "both paid logical calls retain their physical receipts"
    );
    assert_eq!(
        stats[0].good, 1,
        "malformed first answer is a contract failure; valid repair is success"
    );
    assert_eq!(
        stats[0].usd,
        Some(0.04),
        "routing and billing see the same two charges exactly once"
    );
    let replay = f.start(options.clone(), "repair").await;
    assert_eq!(replay, run);
    assert_eq!(f.count(), 2);
    let mut changed = options;
    changed.max_usd = Some(2.0);
    assert!(f
        .d()
        .handle(Request::StartPipelineRun {
            session_id: f.session.clone(),
            content: "task".into(),
            options: changed,
            client_msg_id: Some("repair".into())
        })
        .await
        .is_err());
    f.queue(Reply::text("ordinary next answer"));
    f.d()
        .handle(Request::SendMessage {
            session_id: f.session.clone(),
            content: "next task".into(),
            mode: SendMode::Send,
            client_msg_id: None,
        })
        .await
        .unwrap();
    let next = f
        .d()
        .with_reader(|st| st.last_run(&f.session))
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(f.settled(&next).await, "completed");
    assert!(!f.seen.locked()[2]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("one-Run-only"));
    f.shutdown().await;
}
#[tokio::test]
async fn pipeline_second_mismatch_has_no_invalid_assistant_and_resume_is_refused_before_writes() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env).await;
    f.queue(Reply::text("first invalid"));
    f.queue(Reply::text(r#"{"n":"wrong type"}"#));
    let run = f.start(schema_options(), "mismatch").await;
    assert_eq!(f.settled(&run).await, "paused");
    assert_eq!(f.count(), 2);
    assert!(f
        .d()
        .with_reader(|st| st.run_error(&run))
        .unwrap()
        .unwrap()
        .starts_with("schema:"));
    let before = f
        .d()
        .with_reader(|st| st.list_messages(&f.session))
        .unwrap();
    assert!(!before.iter().any(|m| m.role == "assistant"));
    let error = f
        .d()
        .handle(Request::ResumeRun {
            session_id: f.session.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::INVALID
    );
    assert_eq!(
        f.d()
            .with_reader(|st| st.list_messages(&f.session))
            .unwrap(),
        before
    );
    assert_eq!(
        f.d()
            .with_reader(|st| st.run_status(&run))
            .unwrap()
            .as_deref(),
        Some("paused")
    );
    assert!(!f.d().budget_waived.locked().contains(&f.session));
    assert_eq!(f.count(), 2);
    let paths = f.d().paths.clone();
    drop(f.daemon.take());
    f.daemon = Some(Arc::new(Daemon::new(paths).unwrap()));
    let error = f
        .d()
        .handle(Request::ResumeRun {
            session_id: f.session.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::INVALID
    );
    assert_eq!(
        f.d()
            .with_reader(|st| st.list_messages(&f.session))
            .unwrap(),
        before
    );
    assert_eq!(
        f.d()
            .with_reader(|st| st.run_status(&run))
            .unwrap()
            .as_deref(),
        Some("paused")
    );
    assert_eq!(
        f.count(),
        2,
        "actual daemon restart cannot fall back to Session defaults and spend"
    );
    f.shutdown().await;
}
#[tokio::test]
async fn pipeline_explicit_zero_has_no_provider_post_or_usage_and_reports_known_zero() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env).await;
    let run = f
        .start(
            RunOptions {
                max_usd: Some(0.0),
                ..Default::default()
            },
            "zero",
        )
        .await;
    assert_eq!(f.settled(&run).await, "paused");
    assert_eq!(f.count(), 0);
    let spend = f
        .d()
        .with_reader(|st| st.pipeline_spend(&run, &Config::load(&f.d().paths)?))
        .unwrap();
    assert_eq!(spend["admitted_attempts"], 0);
    assert_eq!(spend["usage_records"], 0);
    assert_eq!(spend["total_usd"].as_f64(), Some(0.0));
    f.shutdown().await;
}
#[tokio::test]
async fn pipeline_nested_named_factory_caps_raise_and_lower_only_the_run_cap() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env).await;
    f.cfg(|cfg| cfg.limits.run_usd = Some(0.001));
    let (run, _owner) = f.manual(0.05);
    for _ in 0..3 {
        f.queue(Reply::text("nested answer"));
    }
    f.factory_call(&run, None).await.unwrap();
    f.factory_call(&run, Some("other")).await.unwrap();
    f.factory_call(&run, None).await.unwrap();
    let error = f.factory_call(&run, None).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::BUDGET
    );
    assert_eq!(f.count(), 3);
    let spend = f
        .d()
        .with_reader(|st| st.pipeline_spend(&run, &Config::load(&f.d().paths)?))
        .unwrap();
    assert_eq!(spend["usage_records"], 3);
    assert!((spend["known_usd"].as_f64().unwrap() - 0.06).abs() < 1e-12);
    f.cfg(|cfg| cfg.limits.run_usd = Some(10.0));
    let (lower, _owner2) = f.manual(0.02);
    f.queue(Reply::text("one bounded answer"));
    f.factory_call(&lower, None).await.unwrap();
    assert!(f.factory_call(&lower, None).await.is_err());
    assert_eq!(f.count(), 4);
    assert_eq!(
        Config::load(&f.d().paths).unwrap().limits.run_usd,
        Some(10.0)
    );
    f.shutdown().await;
}
#[tokio::test]
async fn pipeline_missing_settled_usage_blocks_next_post_and_marker_miss_never_defaults() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env).await;
    let (run, _owner) = f.manual(1.0);
    let mut reply = Reply::text("usage omitted");
    reply.usage = false;
    f.queue(reply);
    f.factory_call(&run, None).await.unwrap();
    assert_eq!(f.count(), 1);
    let error = f.factory_call(&run, None).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::BUDGET
    );
    assert!(error.to_string().contains("incomplete or unknown"));
    assert_eq!(f.count(), 1);
    *f.d().pipeline_policies.locked() = crate::pipeline_policy::Registry::default();
    assert!(f
        .factory_call(&run, None)
        .await
        .unwrap_err()
        .to_string()
        .contains("pipeline policy"));
    assert_eq!(f.count(), 1);
    f.shutdown().await;
}
#[tokio::test]
async fn pipeline_override_cannot_use_a_session_waiver_to_erase_the_daily_cap() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env).await;
    f.cfg(|cfg| cfg.limits.daily_usd = Some(0.01));
    let (run, _owner) = f.manual(1.0);
    f.d().budget_waived.locked().insert(f.session.clone());
    f.queue(Reply::text("one inflight response"));
    f.factory_call(&run, None).await.unwrap();
    let error = f.factory_call(&run, None).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::BUDGET
    );
    assert!(error.to_string().contains("daily_usd"));
    assert_eq!(f.count(), 1);
    f.shutdown().await;
}

#[tokio::test]
async fn pipeline_post_fifo_price_removal_refuses_unsent_work_and_releases_the_lease() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env).await;
    let (held_run, _held_owner) = f.manual(1.0);
    let other = f.d().with_store(|st| st.create_session(None)).unwrap().id;
    let blocked_policy = Arc::new(RunPolicy::new(
        crate::pipeline::CompiledRunOptions::compile(RunOptions {
            max_usd: Some(1.0),
            ..Default::default()
        })
        .unwrap(),
    ));
    let blocked_run = f
        .d()
        .with_store(|st| {
            let run = st.create_run(&other)?;
            st.record_pipeline_policy(&run, &blocked_policy.marker())?;
            Ok(run)
        })
        .unwrap();
    f.d()
        .pipeline_policies
        .locked()
        .insert(&blocked_run, &blocked_policy)
        .unwrap();
    let mut held = Reply::text("first accounted response");
    held.hold = true;
    f.queue(held);
    let cfg = Config::load(&f.d().paths).unwrap();
    let provider = f.d().provider(&cfg).await.unwrap();
    let first = OwnedTask::new({
        let provider = provider.clone();
        let sid = f.session.clone();
        let run = held_run.clone();
        tokio::spawn(async move {
            counted(
                "llm",
                &sid,
                &run,
                provider.complete_result(&[json!({"role":"user","content":"held"})], "model", None),
            )
            .await
        })
    });
    tokio::time::timeout(Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    assert_eq!(f.count(), 1);
    let second = OwnedTask::new({
        let sid = other.clone();
        let run = blocked_run.clone();
        tokio::spawn(async move {
            counted(
                "llm",
                &sid,
                &run,
                provider.complete_result(
                    &[json!({"role":"user","content":"queued"})],
                    "model",
                    None,
                ),
            )
            .await
        })
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.d().budget_admission.entered.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.count(),
        1,
        "second request really waits behind the first physical lease"
    );
    f.cfg(|cfg| {
        cfg.provider.prices.remove("model");
    });
    assert!(Config::load(&f.d().paths)
        .unwrap()
        .cost_of_known("default", "model", 1, 1, 0, 0, None, false)
        .is_none());
    f.release.add_permits(1);
    first.join().await.unwrap();
    let error = second.join().await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::BUDGET
    );
    assert!(error.to_string().contains("physical model/media pricing"));
    assert_eq!(f.count(), 1);
    let spend = f
        .d()
        .with_reader(|st| st.pipeline_spend(&blocked_run, &Config::load(&f.d().paths)?))
        .unwrap();
    assert_eq!(spend["usage_records"], 0);
    assert_eq!(spend["known_usd"].as_f64(), Some(0.0));
    assert_eq!(
        spend["admitted_attempts"], 1,
        "conservative pre-send coverage is not a paid HTTP count"
    );
    assert!(spend["total_usd"].is_null());
    f.cfg(|cfg| {
        cfg.provider.prices.insert(
            "model".into(),
            crate::config::ModelPrice {
                input: 1.0,
                output: 1.0,
                ..Default::default()
            },
        );
    });
    let (next, _next_owner) = f.manual(1.0);
    f.queue(Reply::text("after local refusal"));
    tokio::time::timeout(Duration::from_secs(5), f.factory_call(&next, None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        f.count(),
        2,
        "post-FIFO refusal released admission instead of stranding later work"
    );
    f.shutdown().await;
}

#[tokio::test]
async fn pipeline_forecast_consent_cannot_authorize_changed_system_schema_or_cap() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env).await;
    f.queue(Reply::text(r#"{"n":1}"#));
    let options = schema_options();
    let first = f.start(options.clone(), "seed").await;
    assert_eq!(f.settled(&first).await, "completed");
    f.cfg(|cfg| cfg.limits.forecast_confirm_usd = Some(0.0));
    let before = f
        .d()
        .with_reader(|st| st.list_messages(&f.session))
        .unwrap();
    for index in 0..3 {
        let input = ForecastInput {
            session_id: Some(f.session.clone()),
            draft: "task".into(),
            options: options.clone(),
            ..Default::default()
        };
        let quote: RunForecast = serde_json::from_value(
            f.d()
                .handle(Request::ForecastRun {
                    input,
                    for_send: true,
                })
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(
            quote.confirmation_required,
            "seed provides a priced comparable observation"
        );
        f.d()
            .handle(Request::ConfirmForecast {
                id: quote.confirmation_id.unwrap(),
            })
            .await
            .unwrap();
        let mut changed = options.clone();
        match index {
            0 => changed.system = Some("different Run-only instructions".into()),
            1 => changed.schema = Some(json!({"type":"integer"})),
            _ => changed.max_usd = Some(2.0),
        }
        let error = f
            .d()
            .handle(Request::StartPipelineRun {
                session_id: f.session.clone(),
                content: "task".into(),
                options: changed,
                client_msg_id: Some(format!("changed-{index}")),
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<RequestFailure>().unwrap().code,
            codes::INVALID
        );
        assert_eq!(
            f.d()
                .with_reader(|st| st.list_messages(&f.session))
                .unwrap(),
            before
        );
        assert_eq!(f.count(), 1);
    }
    f.shutdown().await;
}

#[tokio::test]
async fn pipeline_model_and_effort_are_frozen_run_only_without_auto_test_or_restore() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env).await;
    f.cfg(|cfg| {
        cfg.provider.auto_test_models = true;
        cfg.provider.aliases.insert("pick".into(), "model".into());
        cfg.provider
            .aliases
            .insert("model".into(), "wrong-second-resolution".into());
    });
    f.d()
        .with_store(|st| {
            st.set_session_model(
                &f.session,
                Some("standing-model".into()),
                Some("high".into()),
            )
        })
        .unwrap();
    let original = f
        .d()
        .with_reader(|st| st.get_session(&f.session))
        .unwrap()
        .unwrap();
    let options = RunOptions {
        model: Some("pick".into()),
        reasoning_effort: Some("low".into()),
        max_usd: Some(0.0),
        ..Default::default()
    };
    let quote = f
        .d()
        .handle(Request::ForecastRun {
            input: ForecastInput {
                session_id: Some(f.session.clone()),
                draft: "task".into(),
                options: options.clone(),
                ..Default::default()
            },
            for_send: false,
        })
        .await
        .unwrap();
    assert_eq!(quote["basis"]["model"], "model");
    assert_eq!(quote["basis"]["reasoning_effort"], "low");
    assert_eq!(
        f.count(),
        0,
        "preview and selector resolution cannot auto-test a model"
    );
    let zero = f.start(options.clone(), "model-zero").await;
    assert_eq!(f.settled(&zero).await, "paused");
    assert_eq!(
        f.count(),
        0,
        "explicit zero also blocks model tests and restore tests"
    );
    f.queue(Reply::text("selected answer"));
    let run = f
        .start(
            RunOptions {
                max_usd: Some(0.10),
                ..options
            },
            "model-positive",
        )
        .await;
    assert_eq!(f.settled(&run).await, "completed");
    let seen = f.seen.locked().clone();
    assert_eq!(
        seen.len(),
        1,
        "only the requested Run makes a provider call"
    );
    assert_eq!(
        seen[0]["model"], "model",
        "resolved admitted identity must not be aliased again"
    );
    assert_eq!(seen[0]["reasoning_effort"], "low");
    let (current, forecasts) = f
        .d()
        .with_reader(|st| {
            Ok((
                st.get_session(&f.session)?.unwrap(),
                st.recorded_forecasts()?,
            ))
        })
        .unwrap();
    assert_eq!(current.model, original.model);
    assert_eq!(current.reasoning_effort, original.reasoning_effort);
    let frozen = forecasts.iter().find(|(id, _)| id == &run).unwrap();
    assert_eq!(frozen.1.basis.model, "model");
    assert_eq!(frozen.1.basis.reasoning_effort.as_deref(), Some("low"));
    f.shutdown().await;
}

#[tokio::test]
async fn pipeline_refused_or_truncated_candidates_are_not_repaired_or_scored_as_contract_failures()
{
    let env = crate::credential_env::EnvGuard::isolated();
    for stop_reason in ["content_filter", "length"] {
        let f = Fixture::new(&env).await;
        let mut reply = Reply::text("INVALID terminal candidate");
        reply.stop_reason = stop_reason;
        f.queue(reply);
        let run = f.start(schema_options(), stop_reason).await;
        assert_eq!(f.settled(&run).await, "paused");
        assert_eq!(
            f.count(),
            1,
            "terminal provider outcomes cannot trigger a paid repair"
        );
        let (messages, stats, spend) = f
            .d()
            .with_reader(|st| {
                Ok((
                    st.list_messages(&f.session)?,
                    st.routing_call_stats(
                        &Config::load(&f.d().paths)?,
                        "default",
                        chrono::Utc::now(),
                    )?,
                    st.pipeline_spend(&run, &Config::load(&f.d().paths)?)?,
                ))
            })
            .unwrap();
        assert!(messages.iter().all(|m| m.role != "assistant"));
        assert!(
            stats.is_empty(),
            "terminal/incomplete output is neutral rather than a model contract failure"
        );
        assert_eq!(spend["usage_records"], 1);
        assert_eq!(spend["total_usd"].as_f64(), Some(0.02));
        f.shutdown().await;
    }
}

#[tokio::test]
async fn pipeline_auto_skill_background_keeps_its_origin_policy_and_spend_after_main_cleanup() {
    let env = crate::credential_env::EnvGuard::isolated();
    for (cap, allowed) in [(0.03, true), (0.02, false)] {
        let f = Fixture::new(&env).await;
        f.cfg(|cfg| {
            cfg.skills.auto_create = true;
            cfg.skills.min_tool_calls = 1;
            cfg.skills.model = Some("model".into());
        });
        let (run, owner) = f.manual(cap);
        f.queue(Reply::text("root answer"));
        f.factory_call(&run, None).await.unwrap();
        f.d()
            .with_store(|st| {
                let (tool, version) = st.create_tool_version(
                    "background-fixture",
                    "Private fixture",
                    "{}",
                    "{}",
                    "function run(){return {};}",
                    &Capabilities::default(),
                    32,
                    "user",
                )?;
                let inv = st.start_invocation(
                    &tool.id,
                    version,
                    Some(&f.session),
                    Some(&run),
                    None,
                    "{}",
                )?;
                st.finish_invocation(&inv, &tool.id, "success", "", "{}", "", 1, 10)?;
                st.append_message(&f.session, "user", "private useful task")?;
                st.append_message(&f.session, "assistant", "private useful result")?;
                st.set_run_status(&run, "completed")
            })
            .unwrap();
        if allowed {
            let mut reply = Reply::text("NONE");
            reply.hold = true;
            f.queue(reply);
        }
        // Close the original responsible task before maintenance starts. Its
        // identity/clock must stay fixed while late resource cost is captured.
        let task = crate::routing_task_observation::Task::with_recorder(
            "original-role",
            None,
            f.d().routing_recorder.clone(),
        )
        .unwrap();
        let identity = crate::routing_observation::Identity::new(
            "default",
            &Config::load(&f.d().paths).unwrap().provider,
            "original-answer-model",
        );
        task.responsible(identity.clone());
        task.finish(crate::routing_task_observation::TaskOutcome::Completed);
        let weak_task = Arc::downgrade(&task);
        crate::routing_task_observation::scope(Some(task.clone()), async {
            // The terminal Run's main owner moves into the actual automatic job.
            f.d()
                .maybe_auto_skill(&f.session, &run, "completed", Some(owner));
        })
        .await;
        drop(task);
        if allowed {
            tokio::time::timeout(Duration::from_secs(5), async {
                while f.count() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(
                f.d().pipeline_policies.locked().lookup(&run).is_some(),
                "background policy outlives main cleanup and remains live while HTTP is pending"
            );
            f.release.add_permits(1);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while f.d().auto_skill.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            f.d().pipeline_policies.locked().lookup(&run).is_none(),
            "last real owner is released when background work settles"
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while weak_task.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let batch = f.d().routing_recorder.try_take().unwrap();
        let final_task = batch
            .receipts
            .iter()
            .find_map(|receipt| match receipt {
                crate::routing_recorder::Receipt::Task(
                    crate::routing_task_observation::Event::Final(record),
                ) => Some(record),
                _ => None,
            })
            .expect("maintenance retains and finally releases the original task");
        let closed_task = batch
            .receipts
            .iter()
            .find_map(|receipt| match receipt {
                crate::routing_recorder::Receipt::Task(
                    crate::routing_task_observation::Event::Closed(record),
                ) => Some(record),
                _ => None,
            })
            .unwrap();
        assert_eq!(final_task.responsible.as_ref(), Some(&identity));
        assert_eq!(
            final_task.outcome,
            crate::routing_task_observation::TaskOutcome::Completed
        );
        assert_eq!(
            final_task.elapsed_ms, closed_task.elapsed_ms,
            "background time is not the model's logical response time"
        );
        if allowed {
            let calls: Vec<_> = batch
                .receipts
                .iter()
                .filter_map(|receipt| match receipt {
                    crate::routing_recorder::Receipt::Call(call) => Some(call),
                    _ => None,
                })
                .collect();
            assert_eq!(calls.len(), 1);
            assert!(
                calls[0].resource_only,
                "maintenance cannot claim model quality or responsibility"
            );
            assert_eq!(calls[0].attempts.len(), 1);
            assert_eq!(
                calls[0].attempts[0].usage.as_ref().unwrap().cost,
                Some(0.02)
            );
            assert_eq!(final_task.expected_calls, 1);
        }
        drop(batch);
        assert_eq!(f.count(), if allowed { 2 } else { 1 });
        let spend = f
            .d()
            .with_reader(|st| st.pipeline_spend(&run, &Config::load(&f.d().paths)?))
            .unwrap();
        assert_eq!(
            spend["usage_records"].as_u64(),
            Some(if allowed { 2 } else { 1 })
        );
        assert_eq!(
            spend["total_usd"].as_f64(),
            Some(if allowed { 0.04 } else { 0.02 })
        );
        f.shutdown().await;
    }
}

#[tokio::test]
async fn pipeline_compaction_uses_the_actual_run_cap_and_ledger_instead_of_internal_defaults() {
    let env = crate::credential_env::EnvGuard::isolated();
    for (cap, allowed) in [(0.03, true), (0.02, false), (0.0, false)] {
        let f = Fixture::new(&env).await;
        let (run, _owner) = f.manual(cap);
        if cap > 0.0 {
            f.queue(Reply::text("root answer"));
            f.factory_call(&run, None).await.unwrap();
        }
        let last = f
            .d()
            .with_store(|st| {
                st.append_message(&f.session, "user", "first private turn")?;
                Ok(st
                    .append_message(&f.session, "assistant", "first answer")?
                    .seq)
            })
            .unwrap();
        let cfg = Config::load(&f.d().paths).unwrap();
        let provider = f.d().provider(&cfg).await.unwrap();
        if allowed {
            f.queue(Reply::text("A private concise summary."));
        }
        let result = f
            .d()
            .compact_for_run(
                &provider, &cfg, &f.session, &run, "model", 100_000, last, None,
            )
            .await;
        assert_eq!(result.is_ok(), allowed, "{result:?}");
        assert_eq!(
            f.count(),
            if allowed {
                2
            } else if cap == 0.0 {
                0
            } else {
                1
            }
        );
        let (checkpoint, spend) = f
            .d()
            .with_reader(|st| {
                Ok((
                    st.latest_checkpoint(&f.session)?,
                    st.pipeline_spend(&run, &cfg)?,
                ))
            })
            .unwrap();
        assert_eq!(checkpoint.is_some(), allowed);
        assert_eq!(
            spend["usage_records"].as_u64(),
            Some(if allowed {
                2
            } else if cap == 0.0 {
                0
            } else {
                1
            })
        );
        assert_eq!(
            spend["total_usd"].as_f64(),
            Some(if allowed {
                0.04
            } else if cap == 0.0 {
                0.0
            } else {
                0.02
            })
        );
        f.shutdown().await;
    }
}

#[tokio::test]
async fn pipeline_approval_summary_spawn_retains_policy_and_cannot_escape_the_origin_cap() {
    let env = crate::credential_env::EnvGuard::isolated();
    for (cap, allowed) in [(0.03, true), (0.02, false)] {
        let f = Fixture::new(&env).await;
        f.cfg(|cfg| cfg.tools.approval_summaries = true);
        let (run, owner) = f.manual(cap);
        f.queue(Reply::text("root answer"));
        f.factory_call(&run, None).await.unwrap();
        let (info, ver) = f
            .d()
            .with_store(|st| {
                st.create_tool_version(
                    "summary-fixture",
                    "Private fixture",
                    "{}",
                    "{}",
                    "function run(){return {};}",
                    &Capabilities::default(),
                    32,
                    "user",
                )?;
                st.get_tool_by_name("summary-fixture")?
                    .context("private Tool missing")
            })
            .unwrap();
        let cfg = Config::load(&f.d().paths).unwrap();
        if allowed {
            let mut reply = Reply::text("Private fixture explanation.");
            reply.hold = true;
            f.queue(reply);
        }
        f.d()
            .send_approval_summary(&cfg, &f.session, &run, "private-approval", &info, &ver);
        drop(owner);
        if allowed {
            tokio::time::timeout(Duration::from_secs(5), async {
                while f.count() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(f.d().pipeline_policies.locked().lookup(&run).is_some());
            f.release.add_permits(1);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while f.d().pipeline_policies.locked().lookup(&run).is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let key = format!("{}:v{}", info.id, ver.version);
        if allowed {
            tokio::time::timeout(Duration::from_secs(5), async {
                while f
                    .d()
                    .with_reader(|st| st.kv_get(SUMMARY_NS, &key))
                    .unwrap()
                    .is_none()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        assert_eq!(
            f.d()
                .with_reader(|st| st.kv_get(SUMMARY_NS, &key))
                .unwrap()
                .is_some(),
            allowed
        );
        assert_eq!(f.count(), if allowed { 2 } else { 1 });
        let spend = f
            .d()
            .with_reader(|st| st.pipeline_spend(&run, &cfg))
            .unwrap();
        assert_eq!(
            spend["usage_records"].as_u64(),
            Some(if allowed { 2 } else { 1 })
        );
        assert_eq!(
            spend["total_usd"].as_f64(),
            Some(if allowed { 0.04 } else { 0.02 })
        );
        f.shutdown().await;
    }
}
