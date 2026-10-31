//! End-to-end admission tests use held responses and admission notifications:
//! every competing request reaches the lane before the paid response settles.
use super::*;
use axum::response::IntoResponse;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio::sync::Notify;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Mock {
    posts: AtomicUsize,
    arrived: Notify,
    release: Notify,
    hold: bool,
    failover: bool,
}
impl Mock {
    fn new(hold: bool, failover: bool) -> Arc<Self> {
        Arc::new(Self {
            posts: AtomicUsize::new(0),
            arrived: Notify::new(),
            release: Notify::new(),
            hold,
            failover,
        })
    }
}

async fn setup(
    env: &crate::credential_env::EnvGuard,
    mock: Arc<Mock>,
    limit: Option<f64>,
    daily: bool,
) -> (
    Fixture,
    Paths,
    Config,
    Arc<Daemon>,
    tokio::task::JoinHandle<()>,
) {
    env.set("FORMLESS_API_KEY", "budget-fixture-key");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let handler = mock.clone();
    let app = axum::Router::new().route("/chat/completions", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
        let mock = handler.clone();
        async move {
            let index = mock.posts.fetch_add(1, Ordering::SeqCst);
            mock.arrived.notify_one();
            if mock.hold && index == 0 { mock.release.notified().await; }
            if mock.failover && body["model"] == "primary" {
                return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "fixture primary unavailable").into_response();
            }
            let usage = json!({"prompt_tokens":10,"completion_tokens":10});
            if body["stream"] == true {
                let values = [json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":usage})];
                let events = values.into_iter().map(|value| Ok::<_,std::convert::Infallible>(axum::response::sse::Event::default().data(value.to_string())))
                    .chain(std::iter::once(Ok(axum::response::sse::Event::default().data("[DONE]"))));
                axum::response::Sse::new(futures_util::stream::iter(events)).into_response()
            } else {
                axum::Json(json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":usage})).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root = Fixture(std::env::temp_dir().join(format!("fl-admission-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(root.0.join("data"), root.0.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.provider.model = "primary".into();
    cfg.provider.max_retries = 0;
    cfg.provider.first_byte_timeout_secs = 0;
    cfg.provider.prices.insert(
        "primary".into(),
        crate::config::ModelPrice {
            input: 50_000.0,
            output: 50_000.0,
            cached_input: None,
            cache_write_input: None,
            retrieved_at: None,
        },
    );
    cfg.provider.prices.insert(
        "fallback".into(),
        crate::config::ModelPrice {
            input: 100_000.0,
            output: 100_000.0,
            cached_input: None,
            cache_write_input: None,
            retrieved_at: None,
        },
    );
    if mock.failover {
        cfg.provider.fallback_models = vec!["fallback".into()];
    }
    if daily {
        cfg.limits.daily_usd = limit;
    } else {
        cfg.limits.run_usd = limit;
    }
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (root, paths, cfg, daemon, server)
}

async fn entered(daemon: &Daemon, count: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let changed = daemon.budget_admission.changed.notified();
            if daemon.budget_admission.entered.load(Ordering::SeqCst) >= count {
                break;
            }
            changed.await;
        }
    })
    .await
    .expect("competing request did not reach admission");
}

async fn arrived(mock: &Mock) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let notice = mock.arrived.notified();
            if mock.posts.load(Ordering::SeqCst) > 0 {
                break;
            }
            notice.await;
        }
    })
    .await
    .expect("mock did not receive the admitted request");
}

async fn teardown(daemon: Arc<Daemon>, server: tokio::task::JoinHandle<()>) {
    daemon.shutdown.cancel();
    let weak = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(Duration::from_secs(10), async {
        while weak.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("daemon-owned tasks retained fixture credentials");
    server.abort();
    let _ = server.await;
}

async fn create(daemon: &Arc<Daemon>, sid: &str, name: &str, code: &str, caps: &[&str]) {
    let (result, ok) = daemon.call_tool_direct_outcome(sid, "create_tool", json!({"name":name,"description":"budget regression","input_schema":{"type":"object"},"output_schema":{},"implementation_js":code,"capabilities":caps})).await;
    assert!(ok && result.contains("\"created\""), "{name}: {result}");
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_nested_tools_reserve_headroom_before_spending() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(true, false);
    let (_root, paths, _cfg, daemon, server) = setup(&env, mock.clone(), Some(1.0), false).await;
    let sid = daemon.with_store(|st| st.create_session(None)).unwrap().id;
    create(
        &daemon,
        &sid,
        "spender",
        "function run(){ return {text:llm.complete('spend')}; }",
        &["llm"],
    )
    .await;
    create(&daemon, &sid, "batch", "async function run(){ const results=await Promise.allSettled(Array.from({length:8},()=>tools.call('spender',{}))); const failed=results.find(r=>r.status==='rejected'); if(failed) throw failed.reason; return results; }", &[]).await;
    let task = tokio::spawn({
        let daemon = daemon.clone();
        let sid = sid.clone();
        async move {
            daemon
                .call_tool_direct_outcome(&sid, "batch", json!({}))
                .await
        }
    });
    entered(&daemon, 8).await;
    arrived(&mock).await;
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    mock.release.notify_one();
    let (detail, ok) = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap();
    assert!(!ok && detail.contains("budget:"), "{detail}");
    assert_eq!(
        mock.posts.load(Ordering::SeqCst),
        1,
        "parallel batch multiplied paid overshoot"
    );
    let db = rusqlite::Connection::open(paths.db_file()).unwrap();
    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM run_usage WHERE source='llm'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1, "usage was lost or double-counted");
    drop(db);
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn real_concurrent_runs_share_daily_admission_and_commit_before_release() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(true, false);
    let (_root, _paths, _cfg, daemon, server) = setup(&env, mock.clone(), Some(1.0), true).await;
    let a = daemon.with_store(|st| st.create_session(None)).unwrap().id;
    let b = daemon.with_store(|st| st.create_session(None)).unwrap().id;
    for sid in [&a, &b] {
        daemon
            .handle(Request::SendMessage {
                session_id: sid.clone(),
                content: "answer".into(),
                mode: SendMode::Send,
                client_msg_id: None,
            })
            .await
            .unwrap();
    }
    entered(&daemon, 2).await;
    arrived(&mock).await;
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    mock.release.notify_one();
    let statuses = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let statuses: Vec<_> = [&a, &b]
                .into_iter()
                .map(|sid| {
                    daemon
                        .with_store(|st| st.get_session(sid))
                        .unwrap()
                        .unwrap()
                        .last_run_status
                })
                .collect();
            if statuses
                .iter()
                .all(|s| matches!(s.as_deref(), Some("completed" | "paused")))
            {
                break statuses;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        statuses
            .iter()
            .filter(|s| s.as_deref() == Some("completed"))
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| s.as_deref() == Some("paused"))
            .count(),
        1
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        daemon
            .with_store(|st| st.usage_grouped(None, "purpose"))
            .unwrap()[0]
            .0,
        "run"
    );
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_and_agent_fallback_usage_and_parent_cost_use_serving_model() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(false, true);
    let (_root, paths, cfg, daemon, server) = setup(&env, mock, None, false).await;
    let sid = daemon.with_store(|st| st.create_session(None)).unwrap().id;
    for (name, body, cap) in [
        ("nested_cost", "return {text:llm.complete('work')};", "llm"),
        ("agent_cost", "return {text:agents.run('work')};", "agent"),
    ] {
        create(
            &daemon,
            &sid,
            name,
            &format!("function run(){{ {body} }}"),
            &[cap],
        )
        .await;
        let (_, inv) = daemon
            .clone()
            .execute_tool_at(
                sid.clone(),
                "served-test".into(),
                name.into(),
                json!({}),
                0,
                None,
                Arc::new(AtomicBool::new(false)),
                None,
                CancellationToken::new(),
                cfg.clone(),
                true,
                0,
            )
            .await
            .unwrap();
        let db = rusqlite::Connection::open(paths.db_file()).unwrap();
        let (tokens, cost, unpriced): (i64, f64, i64) = db
            .query_row(
                "SELECT model_tokens,model_cost,model_unpriced FROM invocations WHERE id=?1",
                [inv],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((tokens, cost, unpriced), (20, 2.0, 0));
    }
    let db = rusqlite::Connection::open(paths.db_file()).unwrap();
    let rows: Vec<(String, String, String)> = db
        .prepare("SELECT source,provider,model FROM run_usage ORDER BY source")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![
            ("agent".into(), "default".into(), "fallback".into()),
            ("llm".into(), "default".into(), "fallback".into())
        ]
    );
    assert_eq!(
        daemon
            .usage_info(None, Some("served-test"), &cfg)
            .unwrap()
            .cost,
        Some(4.0)
    );
    drop(db);
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_releases_waiting_and_inflight_reservations() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(true, false);
    let (_root, _paths, cfg, daemon, server) = setup(&env, mock.clone(), Some(5.0), true).await;
    let start = |daemon: Arc<Daemon>, cfg: Config| {
        tokio::spawn(async move {
            let provider = daemon.provider(&cfg).await.unwrap();
            counted(
                "llm",
                "",
                "cancel-test",
                provider.complete_result(
                    &[json!({"role":"user","content":"work"})],
                    "primary",
                    None,
                ),
            )
            .await
        })
    };
    let first = start(daemon.clone(), cfg.clone());
    entered(&daemon, 1).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while mock.posts.load(Ordering::SeqCst) == 0 {
            mock.arrived.notified().await;
        }
    })
    .await
    .unwrap();
    let queued = start(daemon.clone(), cfg.clone());
    entered(&daemon, 2).await;
    queued.abort();
    assert!(queued.await.unwrap_err().is_cancelled());
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    mock.release.notify_one();
    let third = start(daemon.clone(), cfg);
    let result = tokio::time::timeout(Duration::from_secs(10), third)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.text, "ok");
    assert_eq!(
        mock.posts.load(Ordering::SeqCst),
        2,
        "cancelled queued request reached HTTP"
    );
    assert_eq!(
        daemon
            .with_store(|st| st.run_usage("cancel-test"))
            .unwrap()
            .prompt_tokens,
        10
    );
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn persistence_failure_poisoning_prevents_followup_paid_requests() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(false, false);
    let (_root, paths, cfg, daemon, server) = setup(&env, mock.clone(), Some(5.0), true).await;
    let db = rusqlite::Connection::open(paths.db_file()).unwrap();
    db.execute_batch("CREATE TRIGGER reject_budget_usage BEFORE INSERT ON run_usage BEGIN SELECT RAISE(FAIL,'injected accounting failure'); END;").unwrap();
    let provider = daemon.provider(&cfg).await.unwrap();
    let messages = [json!({"role":"user","content":"work"})];
    let first = provider
        .complete_result(&messages, "primary", None)
        .await
        .unwrap_err();
    assert!(format!("{first:#}").contains("injected accounting failure"));
    db.execute_batch("DROP TRIGGER reject_budget_usage")
        .unwrap();
    let second = provider
        .complete_result(&messages, "primary", None)
        .await
        .unwrap_err();
    assert!(format!("{second:#}").contains("usage accounting failed"));
    assert_eq!(
        mock.posts.load(Ordering::SeqCst),
        1,
        "failed usage commit reopened budget headroom"
    );
    drop(db);
    drop(provider);
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn internal_calls_share_limits_and_explicit_resume_waiver_still_works() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(false, false);
    let (_root, _paths, cfg, daemon, server) = setup(&env, mock.clone(), Some(0.5), true).await;
    let provider = daemon.provider(&cfg).await.unwrap();
    let messages = [json!({"role":"user","content":"work"})];
    counted(
        "compaction",
        "",
        "",
        provider.complete_result(&messages, "primary", None),
    )
    .await
    .unwrap();
    assert!(provider
        .complete_result(&messages, "primary", None)
        .await
        .unwrap_err()
        .to_string()
        .contains("daily_usd"));
    daemon
        .budget_waived
        .locked()
        .insert("waived-session".into());
    counted(
        "llm",
        "waived-session",
        "waived-run",
        provider.complete_result(&messages, "primary", None),
    )
    .await
    .unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);
    let sources = daemon
        .with_store(|st| st.usage_grouped(None, "purpose"))
        .unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!((&*sources[0].0, &*sources[1].0), ("compaction", "llm"));
    drop(provider);
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unpriced_aggregate_retains_existing_dollar_limit_semantics() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(false, false);
    let (_root, _paths, mut cfg, daemon, server) =
        setup(&env, mock.clone(), Some(0.01), false).await;
    cfg.provider.prices.clear();
    let provider = daemon.provider(&cfg).await.unwrap();
    let messages = [json!({"role":"user","content":"work"})];
    for _ in 0..2 {
        counted(
            "llm",
            "",
            "unpriced",
            provider.complete_result(&messages, "primary", None),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        daemon
            .usage_info(None, Some("unpriced"), &cfg)
            .unwrap()
            .cost,
        None
    );
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);
    drop(provider);
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_error_releases_reservation_without_poisoning_accounting() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(false, true);
    let (_root, _paths, mut cfg, daemon, server) =
        setup(&env, mock.clone(), Some(5.0), false).await;
    cfg.provider.fallback_models.clear();
    let provider = daemon.provider(&cfg).await.unwrap();
    let messages = [json!({"role":"user","content":"work"})];
    let failure = counted(
        "llm",
        "",
        "error-release",
        provider.complete_result(&messages, "primary", None),
    )
    .await
    .unwrap_err();
    assert!(failure.to_string().contains("503"));
    let recovered = tokio::time::timeout(
        Duration::from_secs(10),
        counted(
            "llm",
            "",
            "error-release",
            provider.complete_result(&messages, "fallback", None),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.model_used, "fallback");
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);
    assert_eq!(
        daemon
            .usage_info(None, Some("error-release"), &cfg)
            .unwrap()
            .cost,
        Some(2.0)
    );
    drop(provider);
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_served_identity_and_compatibility_wrappers_cover_all_completion_apis() {
    for style in ["chat", "responses", "anthropic"] {
        let replies = Arc::new(StdMutex::new(Vec::new()));
        let recorded = replies.clone();
        let app=axum::Router::new().fallback(move |axum::Json(body):axum::Json<Value>| async move {
            if body["model"]=="primary" {
                return (axum::http::StatusCode::SERVICE_UNAVAILABLE,"fixture primary unavailable").into_response();
            }
            axum::Json(match style {
                "responses"=>json!({"status":"incomplete","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":10,"output_tokens":10}}),
                "anthropic"=>json!({"stop_reason":"max_tokens","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":10}}),
                _=>json!({"choices":[{"message":{"content":"ok"},"finish_reason":"length"}],"usage":{"prompt_tokens":10,"completion_tokens":10}}),
            }).into_response()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = ProviderCfg {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            api_style: style.into(),
            model: "primary".into(),
            max_retries: 0,
            fallback_models: vec!["fallback".into()],
            ..Default::default()
        };
        // Keep a separate endpoint identity per API; breaker state cannot leak.
        cfg.aliases.insert("preferred".into(), "primary".into());
        let provider = Provider::new(cfg, "fixture-key".into())
            .unwrap()
            .with_usage_hook(Arc::new(move |_, model, usage| {
                recorded
                    .locked()
                    .push((model.to_string(), usage.prompt_tokens));
                Ok(())
            }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let messages = [json!({"role":"user","content":"work"})];
        let full = provider
            .complete_result(&messages, "preferred", None)
            .await
            .unwrap();
        assert_eq!(
            (&*full.text, &*full.model_used, full.output_limited),
            ("ok", "fallback", true),
            "{style}"
        );
        assert_eq!(full.usage.unwrap().prompt_tokens, 10);
        let (text, usage, cut) = provider
            .complete_full(&messages, "preferred", None)
            .await
            .unwrap();
        assert_eq!(
            (text, usage.unwrap().prompt_tokens, cut),
            ("ok".into(), 10, true)
        );
        let (text, usage) = provider
            .complete_with_usage(&messages, "preferred", None)
            .await
            .unwrap();
        assert_eq!((text, usage.unwrap().completion_tokens), ("ok".into(), 10));
        assert_eq!(
            provider
                .complete(&messages, "preferred", None)
                .await
                .unwrap(),
            "ok"
        );
        assert_eq!(*replies.locked(), vec![("fallback".into(), 10); 4]);
        drop(provider);
        server.abort();
        let _ = server.await;
    }
}
