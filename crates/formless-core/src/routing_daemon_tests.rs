//! Actual daemon producers, private loopback requests and guarded fixture state.
use super::*;
use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
#[path = "routing_exact_run_tests.rs"]
mod exact_run_tests;
#[derive(Clone, Copy)]
enum Mode {
    Fallback,
    FallbackToolCallRecovery,
    Neutral(u16),
    Pending,
    Held,
}
struct Fixture {
    daemon: Option<Arc<Daemon>>,
    root: std::path::PathBuf,
    cfg: Config,
    sid: String,
    posts: Arc<AtomicUsize>,
    models: Arc<StdMutex<Vec<String>>>,
    nested: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Semaphore>,
    server: Option<tokio::task::JoinHandle<()>>,
}
impl Fixture {
    async fn new(env: &crate::credential_env::EnvGuard, mode: Mode) -> Self {
        env.set("FORMLESS_API_KEY", "private-routing-key");
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");
        env.set("NO_PROXY", "127.0.0.1,localhost");
        let posts = Arc::new(AtomicUsize::new(0));
        let models = Arc::new(StdMutex::new(Vec::new()));
        let nested = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let turns = Arc::new(AtomicUsize::new(0));
        let app=Router::new().route("/chat/completions",post({let models=models.clone();let posts=posts.clone();let nested=nested.clone();let release=release.clone();move|Json(body):Json<Value>|{
            let models=models.clone();let posts=posts.clone();let nested=nested.clone();let release=release.clone();let turns=turns.clone();async move {
                let post_index=posts.fetch_add(1,Ordering::SeqCst);
                models.locked().push(body["model"].as_str().unwrap().to_string());
                assert!(matches!(body["model"].as_str(),Some("requested"|"served"|"agent-model"|"tool-model")),"unexpected physical identity: {}",body["model"]);
                if let Mode::Neutral(status)=mode {return (StatusCode::from_u16(status).unwrap(),Json(json!({"error":{"message":if status==400{"context_length_exceeded"}else{"private auth/rate/server failure"}},"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":0.004}}))).into_response();}
                if (matches!(mode,Mode::Fallback)||matches!(mode,Mode::FallbackToolCallRecovery))&&body["model"]=="requested"&&(!matches!(mode,Mode::FallbackToolCallRecovery)||post_index==0){return(StatusCode::NOT_FOUND,Json(json!({"error":{"code":"model_not_found","message":"model not found"},"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":0.004}}))).into_response();}
                if matches!(mode,Mode::Held) {
                    nested.notify_one();let Ok(permit)=release.acquire().await else{return StatusCode::SERVICE_UNAVAILABLE.into_response()};permit.forget();
                }
                if body["stream"]!=true {
                    assert_eq!(body["model"],"tool-model");nested.notify_one();
                    let Ok(permit)=release.acquire().await else{return StatusCode::SERVICE_UNAVAILABLE.into_response()};permit.forget();
                    return Json(json!({"choices":[{"message":{"content":"nested result"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":0.3}})).into_response();
                }
                let event=if matches!(mode,Mode::FallbackToolCallRecovery)&&body["model"]=="served"&&post_index==1 {
                    json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"fallback-call","type":"function","function":{"name":"recovery_tool","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":0.006}})
                }else if matches!(mode,Mode::Pending)&&turns.fetch_add(1,Ordering::SeqCst)==0 {
                    json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"private-pending-call","type":"function","function":{"name":"spender","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":0.1}})
                }else{json!({"choices":[{"index":0,"delta":{"content":if matches!(mode,Mode::FallbackToolCallRecovery){"recovered"}else{"done"}},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":if matches!(mode,Mode::Fallback){0.006}else{0.1}}})};
                ([("content-type","text/event-stream")],format!("data: {event}\n\ndata: [DONE]\n\n")).into_response()
            }
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = std::env::temp_dir().join(format!("fl-routing-daemon-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = base.clone();
        cfg.provider.model = match mode {
            Mode::Pending => "agent-model",
            Mode::FallbackToolCallRecovery => "auto",
            _ => "requested",
        }
        .into();
        cfg.provider.max_retries = 0;
        cfg.provider.max_backoff_secs = 0;
        cfg.provider.first_byte_timeout_secs = 0;
        cfg.provider.auto_test_models = false;
        if matches!(mode, Mode::Fallback | Mode::FallbackToolCallRecovery) {
            cfg.provider.fallback_models = vec!["served".into()];
        }
        if matches!(mode, Mode::FallbackToolCallRecovery) {
            cfg.provider.routing_models = vec!["requested".into()];
        }
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.security.secure = false;
        cfg.tools.pending_threshold_secs = 0;
        if matches!(mode, Mode::Pending) {
            cfg.roles.insert(
                "worker".into(),
                crate::config::RoleCfg {
                    model: "agent-model".into(),
                    ..Default::default()
                },
            );
        }
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        assert_eq!(
            daemon.config().unwrap().provider.base_url,
            base,
            "private effective endpoint"
        );
        let sid = daemon.with_store(|st| st.create_session(None)).unwrap().id;
        if matches!(mode, Mode::Pending) {
            daemon
                .with_store(|st| st.set_session_model(&sid, Some("role:worker".to_string()), None))
                .unwrap();
        }
        Self {
            daemon: Some(daemon),
            root,
            cfg,
            sid,
            posts,
            models,
            nested,
            release,
            server: Some(server),
        }
    }
    fn daemon(&self) -> &Arc<Daemon> {
        self.daemon.as_ref().unwrap()
    }
    async fn send(&self) {
        self.daemon()
            .handle(Request::SendMessage {
                session_id: self.sid.clone(),
                content: "answer the private fixture".into(),
                mode: SendMode::Send,
                client_msg_id: None,
            })
            .await
            .unwrap();
    }
    async fn idle(&self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.daemon().runs.locked().contains_key(&self.sid) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("private Run did not stop");
    }
    async fn shutdown(mut self) {
        self.stop();
        let daemon = self.daemon.take().unwrap();
        let weak = Arc::downgrade(&daemon);
        drop(daemon);
        tokio::time::timeout(Duration::from_secs(10), async {
            while weak.strong_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("private producer retained daemon");
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
    }
    fn stop(&self) {
        self.release.close();
        if let Some(daemon) = &self.daemon {
            daemon.shutdown.cancel();
            for run in daemon.runs.locked().values() {
                run.cancel.cancel();
                run.cancel_flag.store(true, Ordering::SeqCst);
            }
            for entry in daemon.pending.locked().values() {
                entry.cancel.cancel();
            }
        }
    }
}

#[test]
fn assistant_turn_metadata_keeps_tool_calls_and_uses_attempt_provenance() {
    let mut tool_meta = json!({"tool_calls": [{"id": "call-1"}]});
    add_served_model_meta(&mut tool_meta, "auto", "picked", "picked", false);
    assert_eq!(tool_meta["tool_calls"][0]["id"], "call-1");
    assert_eq!(tool_meta["model"]["name"], "picked");
    assert_eq!(tool_meta["model"]["via"], "routing:picked");
    assert_eq!(tool_meta["model"]["requested"], "requested");

    let mut fallback_meta = json!({});
    add_served_model_meta(
        &mut fallback_meta,
        "requested",
        "requested",
        "fallback",
        true,
    );
    assert_eq!(fallback_meta["model"]["via"], "fallback");

    let mut unchanged = json!({});
    add_served_model_meta(&mut unchanged, "requested", "requested", "requested", false);
    assert!(unchanged.get("model").is_none());
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop();
        if let Some(server) = self.server.take() {
            server.abort();
        }
        drop(self.daemon.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_run_fallback_records_served_quality_and_all_billed_physical_spend() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, Mode::Fallback).await;
    f.send().await;
    f.idle().await;
    let messages = f
        .daemon()
        .with_reader(|st| st.list_messages(&f.sid))
        .unwrap();
    let answer = messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && !message.content.is_empty())
        .unwrap();
    let metadata: Value = serde_json::from_str(answer.meta.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["model"]["name"], "served");
    assert_eq!(metadata["model"]["via"], "fallback");
    assert_eq!(metadata["model"]["requested"], "requested");
    tokio::time::timeout(Duration::from_secs(10), async {
        while Arc::strong_count(f.daemon()) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.daemon().flush_routing().unwrap();
    let stats = f
        .daemon()
        .with_reader(|st| st.routing_call_stats(&f.cfg, "default", chrono::Utc::now()))
        .unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].model, "served");
    assert_eq!(stats[0].good, 1);
    assert!((stats[0].usd.unwrap() - 0.01).abs() < 1e-12);
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    let ledger = f.daemon().usage_info(Some(&f.sid), None, &f.cfg).unwrap();
    assert!(
        (ledger.cost.unwrap() - 0.01).abs() < 1e-12,
        "routing cannot bill again"
    );
    f.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_routed_fallback_tool_call_keeps_failover_metadata_through_recovery() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, Mode::FallbackToolCallRecovery).await;
    let (created, ok) = f
        .daemon()
        .call_tool_direct_outcome(
            &f.sid,
            "create_tool",
            json!({
                "name": "recovery_tool",
                "description": "fallback recovery fixture",
                "input_schema": {"type": "object"},
                "output_schema": {},
                "implementation_js": "function run(){return {text:'tool complete'};}",
                "capabilities": []
            }),
        )
        .await;
    assert!(ok && created.contains("created"), "{created}");

    f.send().await;
    f.idle().await;
    assert_eq!(*f.models.locked(), ["requested", "served", "served"]);
    let messages = f
        .daemon()
        .with_reader(|st| st.list_messages(&f.sid))
        .unwrap();
    let tool_turn = messages
        .iter()
        .find(|message| {
            message.role == "assistant"
                && message
                    .meta
                    .as_deref()
                    .and_then(|meta| serde_json::from_str::<Value>(meta).ok())
                    .is_some_and(|meta| meta["tool_calls"].is_array())
        })
        .unwrap();
    let tool_meta: Value = serde_json::from_str(tool_turn.meta.as_deref().unwrap()).unwrap();
    assert_eq!(tool_meta["model"]["requested"], "requested");
    assert_eq!(tool_meta["model"]["name"], "served");
    assert_eq!(tool_meta["model"]["via"], "fallback");
    let answer = messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && message.content == "recovered")
        .unwrap();
    let answer_meta: Value = serde_json::from_str(answer.meta.as_deref().unwrap()).unwrap();
    assert_eq!(answer_meta["model"]["requested"], "requested");
    assert_eq!(answer_meta["model"]["name"], "served");
    assert_eq!(answer_meta["model"]["via"], "fallback");
    f.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_terminal_auth_rate_context_and_server_failures_remain_neutral_not_free() {
    let env = crate::credential_env::EnvGuard::isolated();
    for status in [400, 401, 403, 429, 503] {
        let f = Fixture::new(&env, Mode::Neutral(status)).await;
        f.send().await;
        f.idle().await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while Arc::strong_count(f.daemon()) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        f.daemon().flush_routing().unwrap();
        assert!(
            f.daemon()
                .with_reader(|st| st.routing_call_stats(&f.cfg, "default", chrono::Utc::now()))
                .unwrap()
                .is_empty(),
            "status{status}"
        );
        assert_eq!(f.posts.load(Ordering::SeqCst), 1);
        assert!(
            (f.daemon()
                .usage_info(Some(&f.sid), None, &f.cfg)
                .unwrap()
                .cost
                .unwrap()
                - 0.004)
                .abs()
                < 1e-12
        );
        f.shutdown().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn real_pending_tool_pins_role_cost_until_last_handle_owner_without_relabeling_model() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, Mode::Pending).await;
    let (created,ok)=f.daemon().call_tool_direct_outcome(&f.sid,"create_tool",json!({"name":"spender","description":"private routing ownership fixture","input_schema":{"type":"object"},"output_schema":{},"implementation_js":"function run(){return {text:llm.complete('nested',{model:'tool-model'})};}","capabilities":["llm"]})).await;
    assert!(ok && created.contains("created"), "{created}");
    assert_eq!(
        f.posts.load(Ordering::SeqCst),
        0,
        "save-gate smoke remains private/stubbed"
    );
    f.send().await;
    tokio::time::timeout(Duration::from_secs(10), f.nested.notified())
        .await
        .expect("real nested model POST missing");
    f.idle().await;
    let (handle, task_weak, task_id) = {
        let pending = f.daemon().pending.locked();
        assert_eq!(pending.len(), 1);
        let (handle, entry) = pending.iter().next().unwrap();
        (
            handle.clone(),
            Arc::downgrade(entry.routing_task.as_ref().expect("pending task owner")),
            entry.routing_task.as_ref().unwrap().id().to_string(),
        )
    };
    f.daemon().flush_routing().unwrap();
    assert!(
        f.daemon()
            .with_reader(|st| st.routing_task_stats(
                &f.cfg,
                "worker",
                "default",
                chrono::Utc::now()
            ))
            .unwrap()
            .is_empty(),
        "closed is not final while pending owns unpaid work"
    );
    assert!(
        f.daemon()
            .pending
            .locked()
            .get(&handle)
            .unwrap()
            .abandoned
            .load(Ordering::SeqCst),
        "real Runloop auto-abandons after its pending reminder"
    );
    f.release.add_permits(1);
    // Arc reaches strong_count=0 before Task::drop publishes Final. A
    // nonblocking flush also intentionally skips another in-progress batch.
    // Synchronize on the actual durable receipt, not either intermediate
    // lifecycle observation. Eligibility/cost assertions remain below.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if task_weak.strong_count() == 0 {
                f.daemon().flush_routing().unwrap();
                if f.daemon()
                    .with_reader(|st| st.routing_task_final_persisted(&task_id))
                    .unwrap()
                {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("abandoned executor did not release its last owner and persist Final");
    assert!(!f.daemon().pending.locked().contains_key(&handle));
    assert_eq!(task_weak.strong_count(), 0);
    let stats = f
        .daemon()
        .with_reader(|st| st.routing_task_stats(&f.cfg, "worker", "default", chrono::Utc::now()))
        .unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].model, "agent-model");
    assert_eq!(stats[0].good, 1);
    assert!((stats[0].usd.unwrap() - 0.6).abs() < 1e-12);
    assert_eq!(f.posts.load(Ordering::SeqCst), 4);
    assert!(
        (f.daemon()
            .usage_info(Some(&f.sid), None, &f.cfg)
            .unwrap()
            .cost
            .unwrap()
            - 0.6)
            .abs()
            < 1e-12
    );
    let run = f
        .daemon()
        .with_reader(|st| st.last_run(&f.sid))
        .unwrap()
        .unwrap()
        .0;
    f.daemon()
        .with_store(|st| st.rate_run(&f.sid, Some(&run), "bad", None))
        .unwrap();
    assert_eq!(
        f.daemon()
            .with_reader(|st| st.routing_task_stats(
                &f.cfg,
                "worker",
                "default",
                chrono::Utc::now()
            ))
            .unwrap()[0]
            .good,
        0
    );
    f.shutdown().await;
}

#[tokio::test]
async fn selection_keeps_one_pass_aliases_and_admitted_role_identity_without_http() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, Mode::Fallback).await;
    let mut cfg = f.cfg.clone();
    cfg.provider.aliases.insert("first".into(), "second".into());
    cfg.provider
        .aliases
        .insert("second".into(), "requested".into());
    let target = f
        .daemon()
        .target(&cfg, &f.sid, Some("first"))
        .await
        .unwrap();
    assert_eq!(target.model, "second");
    assert_eq!(
        f.daemon()
            .resolve_model_named(&target.model, &cfg, target.provider.name(), false)
            .unwrap(),
        "second"
    );
    cfg.roles.insert(
        "worker".into(),
        crate::config::RoleCfg {
            model: "served".into(),
            ..Default::default()
        },
    );
    f.daemon().run_admissions.locked().insert(
        "admitted-private-run".into(),
        Arc::new(RunAdmission {
            pipeline: None,
            cfg: cfg.clone(),
            requested: "role:worker".into(),
            resolved_model: "served".into(),
            effort: None,
            recovery_messages: vec![],
            resumed_from: None,
            first_message_seq: 0,
            first: StdMutex::new(None),
        }),
    );
    let task = f
        .daemon()
        .routing_task_for_run(&f.sid, "admitted-private-run")
        .unwrap();
    assert_eq!(
        task.role(),
        Some("worker"),
        "admission is authoritative even when Session keeps a different model"
    );
    task.finish(crate::routing_task_observation::TaskOutcome::Neutral(
        crate::routing_observation::Neutral::Cancelled,
    ));
    drop(task);
    f.daemon()
        .run_admissions
        .locked()
        .remove("admitted-private-run");
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    drop(target);
    f.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_interrupt_drops_header_wait_without_quality_or_a_fabricated_known_cost() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, Mode::Held).await;
    f.send().await;
    tokio::time::timeout(Duration::from_secs(10), f.nested.notified())
        .await
        .unwrap();
    f.daemon()
        .handle(Request::InterruptRun {
            session_id: f.sid.clone(),
        })
        .await
        .unwrap();
    f.idle().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while Arc::strong_count(f.daemon()) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    f.daemon().flush_routing().unwrap();
    assert!(f
        .daemon()
        .with_reader(|st| st.routing_call_stats(&f.cfg, "default", chrono::Utc::now()))
        .unwrap()
        .is_empty());
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert!(
        f.daemon()
            .usage_info(Some(&f.sid), None, &f.cfg)
            .unwrap()
            .cost
            .is_none(),
        "no reported usage is not a zero-dollar complete response"
    );
    f.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_pending_map_alone_retains_exact_role_task_after_real_late_http() {
    // The normal Run loop deliberately auto-abandons unfinished work. Use an
    // actual PendingEntry callback owner here to isolate the *completed* map
    // lifetime, while both responsible and late resource requests use HTTP.
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, Mode::Fallback).await;
    let daemon = f.daemon();
    let run = daemon.with_store(|st| st.create_run(&f.sid)).unwrap();
    let task = crate::routing_task_observation::Task::with_recorder(
        "worker",
        None,
        daemon.routing_recorder.clone(),
    )
    .unwrap();
    let weak = Arc::downgrade(&task);
    let p = daemon.provider(&f.cfg).await.unwrap();
    crate::routing_task_observation::scope(Some(task.clone()), async {
        let call =
            crate::routing_observation::Call::responsible_step(daemon.routing_recorder.call_sink());
        let reply = crate::routing_observation::scope(
            call.clone(),
            counted(
                "routing-private",
                &f.sid,
                &run,
                p.chat_stream(
                    &[],
                    &[],
                    "requested",
                    None,
                    &CancellationToken::new(),
                    |_, _| {},
                ),
            ),
        )
        .await;
        Daemon::finish_chat_observation(&call, &p, &reply, false);
        assert_eq!(reply.unwrap().model_used, "served");
        drop(call);
    })
    .await;
    daemon
        .with_store(|st| {
            st.record_task_with_id(
                task.id(),
                "worker",
                "served",
                &f.sid,
                &run,
                "",
                10,
                0,
                1,
                "completed",
            )
        })
        .unwrap();
    let handle = uuid::Uuid::new_v4().to_string();
    let entry = Arc::new(PendingEntry {
        progress: Arc::new(Default::default()),
        _pipeline: StdMutex::new(None),
        routing_task: Some(task.clone()),
        taint_scope: daemon.taint_scope(&f.sid, &run),
        tool_name: "controlled-private-resource".into(),
        run_id: run.clone(),
        started_ms: epoch_ms(),
        session_id: f.sid.clone(),
        result: StdMutex::new(None),
        delivered: AtomicBool::new(false),
        abandoned: AtomicBool::new(false),
        stalled_noted: AtomicBool::new(false),
        activity: Arc::new(std::sync::atomic::AtomicU64::new(epoch_ms())),
        notify: tokio::sync::Notify::new(),
        cancel: CancellationToken::new(),
        order: 0,
    });
    daemon
        .pending
        .locked()
        .insert(handle.clone(), entry.clone());
    let job = {
        let daemon = daemon.clone();
        let sid = f.sid.clone();
        let handle = handle.clone();
        let scoped = entry.routing_task.clone();
        tokio::spawn(crate::routing_task_observation::scope(scoped, async move {
            let result = counted(
                "routing-private",
                &sid,
                &run,
                p.complete_result(&[], "tool-model", None),
            )
            .await
            .unwrap();
            daemon.complete_pending(&handle, &entry, true, result.text);
        }))
    };
    tokio::time::timeout(Duration::from_secs(10), f.nested.notified())
        .await
        .expect("private late physical HTTP did not start");
    task.finish(crate::routing_task_observation::TaskOutcome::Completed);
    drop(task);
    daemon.flush_routing().unwrap();
    assert!(daemon
        .with_reader(|st| st.routing_task_stats(&f.cfg, "worker", "default", chrono::Utc::now()))
        .unwrap()
        .is_empty());
    f.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(10), job)
        .await
        .unwrap()
        .unwrap();
    // All producer/local Task and Entry Arcs have gone. A borrowed map lookup
    // proves ownership without creating another strong lease across an await.
    {
        let pending = daemon.pending.locked();
        let entry = pending.get(&handle).unwrap();
        assert!(entry.result.locked().is_some());
        assert_eq!(Arc::strong_count(entry), 1);
        assert_eq!(Arc::strong_count(entry.routing_task.as_ref().unwrap()), 1);
    }
    assert_eq!(weak.strong_count(), 1);
    daemon
        .handle(Request::AbandonInvocation { handle })
        .await
        .unwrap();
    assert!(weak.upgrade().is_none());
    daemon.flush_routing().unwrap();
    let stats = daemon
        .with_reader(|st| st.routing_task_stats(&f.cfg, "worker", "default", chrono::Utc::now()))
        .unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].model, "served");
    assert!((stats[0].usd.unwrap() - 0.31).abs() < 1e-12);
    assert_eq!(stats[0].total_tokens, Some(15.0));
    assert_eq!(f.posts.load(Ordering::SeqCst), 3);
    assert!(
        (daemon
            .usage_info(Some(&f.sid), None, &f.cfg)
            .unwrap()
            .cost
            .unwrap()
            - 0.31)
            .abs()
            < 1e-12
    );
    f.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_admitted_alias_forecast_and_role_identity_reach_the_same_wire_model() {
    let env = crate::credential_env::EnvGuard::isolated();
    for role in [false, true] {
        let mut f = Fixture::new(&env, Mode::Held).await;
        f.cfg
            .provider
            .aliases
            .insert("first".into(), "requested".into());
        f.cfg
            .provider
            .aliases
            .insert("requested".into(), "served".into());
        f.cfg
            .provider
            .aliases
            .insert("served".into(), "agent-model".into());
        f.cfg.provider.model = "first".into();
        if role {
            f.cfg.roles.insert(
                "worker".into(),
                crate::config::RoleCfg {
                    model: "first".into(),
                    ..Default::default()
                },
            );
            f.daemon()
                .with_store(|st| st.set_session_model(&f.sid, Some("role:worker".into()), None))
                .unwrap();
        }
        f.cfg.save(&f.daemon().paths).unwrap();
        let input = ForecastInput {
            session_id: Some(f.sid.clone()),
            draft: "answer the private fixture".into(),
            ..Default::default()
        };
        let forecast = f
            .daemon()
            .handle(Request::ForecastRun {
                input: input.clone(),
                for_send: false,
            })
            .await
            .unwrap();
        assert_eq!(
            forecast["basis"]["model"], "requested",
            "raw/default and role aliases resolve exactly once"
        );
        let consent = f
            .daemon()
            .handle(Request::ForecastRun {
                input,
                for_send: true,
            })
            .await
            .unwrap();
        assert_eq!(consent["basis"]["model"], "requested");
        f.daemon()
            .handle(Request::ConfirmForecast {
                id: consent["confirmation_id"].as_str().unwrap().into(),
            })
            .await
            .unwrap();
        assert_eq!(
            f.posts.load(Ordering::SeqCst),
            0,
            "forecasts/confirmation remain local"
        );
        f.send().await;
        tokio::time::timeout(Duration::from_secs(10), f.nested.notified())
            .await
            .unwrap();
        assert_eq!(
            *f.models.locked(),
            vec!["requested".to_string()],
            "admission and Provider serialization must not resolve the frozen model again"
        );
        f.release.add_permits(1);
        f.idle().await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while Arc::strong_count(f.daemon()) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        f.daemon().flush_routing().unwrap();
        let stats = f
            .daemon()
            .with_reader(|st| st.routing_call_stats(&f.cfg, "default", chrono::Utc::now()))
            .unwrap();
        assert_eq!(stats[0].model, "requested");
        assert_eq!(stats[0].good, 1);
        assert_eq!(stats[0].usd, Some(0.1));
        f.shutdown().await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scoped_resolved_primary_preserves_independently_aliased_configured_fallbacks() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env, Mode::Fallback).await;
    f.cfg.provider.model = "first".into();
    f.cfg
        .provider
        .aliases
        .insert("first".into(), "requested".into());
    f.cfg
        .provider
        .aliases
        .insert("requested".into(), "served".into());
    f.cfg
        .provider
        .aliases
        .insert("served".into(), "agent-model".into());
    // Primary first->requested; configured fallback served->agent-model. A
    // naive alias-map clear would erase the independent fallback mapping.
    f.cfg.provider.fallback_models = vec!["served".into()];
    f.cfg.save(&f.daemon().paths).unwrap();
    let raw = f.daemon().provider(&f.cfg).await.unwrap();
    let scoped = raw.resolved_dispatch("requested");
    assert_eq!(
        raw.settings().aliases,
        f.cfg.provider.aliases,
        "raw Provider API unchanged"
    );
    assert!(scoped.settings().aliases.is_empty());
    assert_eq!(scoped.settings().fallback_models, vec!["agent-model"]);
    drop(raw);
    drop(scoped);
    f.send().await;
    f.idle().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while Arc::strong_count(f.daemon()) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        *f.models.locked(),
        vec!["requested".to_string(), "agent-model".to_string()]
    );
    f.daemon().flush_routing().unwrap();
    let stats = f
        .daemon()
        .with_reader(|st| st.routing_call_stats(&f.cfg, "default", chrono::Utc::now()))
        .unwrap();
    assert_eq!(stats[0].model, "agent-model");
    assert!((stats[0].usd.unwrap() - 0.01).abs() < 1e-12);
    f.shutdown().await;
}
