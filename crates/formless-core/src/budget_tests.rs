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

fn seed_mixed_floor(daemon: &Daemon, sid: &str, run: &str, known: Option<f64>) {
    daemon
        .with_store(|st| {
            for (input, output) in [(3, 2), (4, 3)] {
                st.record_usage_full(
                    sid,
                    run,
                    "default",
                    "primary",
                    &crate::provider::Usage {
                        prompt_tokens: input,
                        completion_tokens: output,
                        reported: 3,
                        ..Default::default()
                    },
                    "internal",
                )?;
            }
            if let Some(cost) = known {
                st.record_usage_full(
                    sid,
                    run,
                    "default",
                    "primary",
                    &crate::provider::Usage {
                        cost: Some(cost),
                        reported: 3,
                        ..Default::default()
                    },
                    "internal",
                )?;
            }
            Ok(())
        })
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn known_reported_usd_blocks_mixed_unpriced_factory_requests_before_http() {
    let env = crate::credential_env::EnvGuard::isolated();
    for daily in [false, true] {
        let mock = Mock::new(false, false);
        let (_root, _paths, mut cfg, daemon, server) =
            setup(&env, mock.clone(), Some(1.0), daily).await;
        cfg.provider.prices.clear();
        cfg.save(&daemon.paths).unwrap();
        let effective = daemon.config().unwrap();
        assert_eq!(effective.provider.base_url, cfg.provider.base_url);
        assert_eq!(
            reqwest::Url::parse(&effective.provider.base_url)
                .unwrap()
                .host_str(),
            Some("127.0.0.1")
        );
        let sid = daemon.with_store(|st| st.create_session(None)).unwrap().id;
        seed_mixed_floor(&daemon, &sid, "known-floor", Some(2.0));
        let display = daemon.usage_info(None, Some("known-floor"), &cfg).unwrap();
        let policy_error = daemon
            .check_budget(&sid, "known-floor", &cfg)
            .err()
            .map(|error| error.to_string());
        let provider = daemon.provider(&cfg).await.unwrap();
        assert_eq!(provider.settings().base_url, cfg.provider.base_url);
        let (result,posted)=tokio::time::timeout(Duration::from_secs(5),async {
            let messages=[json!({"role":"user","content":"must remain local"})];
            let request=counted("internal",&sid,"known-floor",provider.complete(&messages,"primary",None));
            tokio::pin!(request);
            tokio::select! {
                result=&mut request=>(result,false),
                _=mock.arrived.notified()=>(Err(anyhow::anyhow!("private peer observed a request despite known USD floor")),true),
            }
        }).await.unwrap();
        let posts = mock.posts.load(Ordering::SeqCst);
        drop(provider);
        teardown(daemon, server).await;
        assert_eq!(display.cost, None);
        assert_eq!(display.unpriced, vec!["primary"]);
        assert!(
            !posted && posts == 0,
            "known USD2 plus unpriced7/5 cannot bypass USD1 cap"
        );
        assert!(
            policy_error
                .as_ref()
                .is_some_and(|error| error.contains("at least $2.00")),
            "{policy_error:?}"
        );
        assert!(result.unwrap_err().to_string().contains("budget:"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_and_below_limit_floors_keep_compatibility_and_honest_eighty_percent_notes() {
    let env = crate::credential_env::EnvGuard::isolated();
    for daily in [false, true] {
        for known in [None, Some(0.25), Some(0.85)] {
            let mock = Mock::new(false, false);
            let (_root, _paths, mut cfg, daemon, server) =
                setup(&env, mock.clone(), Some(1.0), daily).await;
            cfg.provider.prices.clear();
            cfg.save(&daemon.paths).unwrap();
            assert_eq!(
                daemon.config().unwrap().provider.base_url,
                cfg.provider.base_url
            );
            let sid = daemon.with_store(|st| st.create_session(None)).unwrap().id;
            seed_mixed_floor(&daemon, &sid, "below-floor", known);
            let (tx, mut rx) = mpsc::channel(8);
            daemon.subs.locked().push(Subscriber {
                conn: 987,
                tx,
                filter: Arc::new(StdMutex::new(EventFilter::default())),
                kick: CancellationToken::new(),
                gaps: Default::default(),
            });
            daemon.check_budget(&sid, "below-floor", &cfg).unwrap();
            daemon.check_budget(&sid, "below-floor", &cfg).unwrap();
            let notes = std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|message| match message {
                    ServerMessage::Event(Event::Note { text, .. }) => Some(text),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let provider = daemon.provider(&cfg).await.unwrap();
            assert_eq!(provider.settings().base_url, cfg.provider.base_url);
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                counted(
                    "internal",
                    &sid,
                    "below-floor",
                    provider.complete(
                        &[json!({"role":"user","content":"private allowed fixture"})],
                        "primary",
                        None,
                    ),
                ),
            )
            .await
            .unwrap();
            let display = daemon.usage_info(None, Some("below-floor"), &cfg).unwrap();
            let posts = mock.posts.load(Ordering::SeqCst);
            drop(provider);
            teardown(daemon, server).await;
            assert_eq!(result.unwrap(), "ok");
            assert_eq!(posts, 1);
            assert_eq!(display.cost, None);
            assert_eq!(display.unpriced, vec!["primary"]);
            if known == Some(0.85) {
                assert_eq!(notes.len(), 1);
                assert!(
                    notes[0].contains("at least $0.85") && notes[0].contains("80%"),
                    "{notes:?}"
                );
            } else {
                assert!(notes.is_empty(), "{notes:?}");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_resume_waiver_still_bypasses_a_known_spending_floor() {
    let env = crate::credential_env::EnvGuard::isolated();
    for daily in [false, true] {
        let mock = Mock::new(false, false);
        let (_root, _paths, mut cfg, daemon, server) =
            setup(&env, mock.clone(), Some(1.0), daily).await;
        cfg.provider.prices.clear();
        cfg.save(&daemon.paths).unwrap();
        assert_eq!(
            daemon.config().unwrap().provider.base_url,
            cfg.provider.base_url
        );
        let sid = daemon.with_store(|st| st.create_session(None)).unwrap().id;
        seed_mixed_floor(&daemon, &sid, "waived-floor", Some(2.0));
        daemon.budget_waived.locked().insert(sid.clone());
        daemon.check_budget(&sid, "waived-floor", &cfg).unwrap();
        let provider = daemon.provider(&cfg).await.unwrap();
        assert_eq!(provider.settings().base_url, cfg.provider.base_url);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            counted(
                "internal",
                &sid,
                "waived-floor",
                provider.complete(
                    &[json!({"role":"user","content":"explicit waived fixture"})],
                    "primary",
                    None,
                ),
            ),
        )
        .await
        .unwrap();
        let posts = mock.posts.load(Ordering::SeqCst);
        drop(provider);
        teardown(daemon, server).await;
        assert_eq!(result.unwrap(), "ok");
        assert_eq!(posts, 1);
    }
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
    let stats = daemon.with_store(|st| st.list_model_stats()).unwrap();
    assert_eq!(stats.iter().map(|s| s.successes).sum::<i64>(), 1);
    assert_eq!(
        stats.iter().map(|s| s.failures).sum::<i64>(),
        0,
        "a queued budget refusal is not a failed model request"
    );
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
    cfg.save(&daemon.paths).unwrap();
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
        let cutoff = Arc::new(AtomicBool::new(false));
        let response_cutoff = cutoff.clone();
        let app=axum::Router::new().fallback(move |axum::Json(body):axum::Json<Value>| {
            let cutoff = response_cutoff.clone();
            async move {
            if body["model"]=="primary" {
                return (axum::http::StatusCode::SERVICE_UNAVAILABLE,"fixture primary unavailable").into_response();
            }
            axum::Json(match style {
                "responses"=>json!({"status":"incomplete","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":10,"output_tokens":10}}),
                "anthropic"=>json!({"stop_reason":if cutoff.load(Ordering::SeqCst) {"max_tokens"} else {"end_turn"},"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":10}}),
                _=>json!({"choices":[{"message":{"content":"ok"},"finish_reason":"length"}],"usage":{"prompt_tokens":10,"completion_tokens":10}}),
            }).into_response()
        }});
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
        let expected_limited = style != "anthropic";
        let full = provider
            .complete_result(&messages, "preferred", None)
            .await
            .unwrap();
        assert_eq!(
            (&*full.text, &*full.model_used, full.output_limited),
            ("ok", "fallback", expected_limited),
            "{style}"
        );
        assert_eq!(full.usage.unwrap().prompt_tokens, 10);
        let (text, usage, cut) = provider
            .complete_full(&messages, "preferred", None)
            .await
            .unwrap();
        assert_eq!(
            (text, usage.unwrap().prompt_tokens, cut),
            ("ok".into(), 10, expected_limited)
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
        if style == "anthropic" {
            // Messages max_tokens is an error under the existing contract,
            // unlike Chat/Responses' successful-but-limited completion result.
            cutoff.store(true, Ordering::SeqCst);
            let typed = provider
                .complete_result(&messages, "preferred", None)
                .await
                .unwrap_err();
            assert!(
                typed
                    .to_string()
                    .contains("cut off at the output token limit"),
                "{typed:#}"
            );
            let full = provider
                .complete_full(&messages, "preferred", None)
                .await
                .unwrap_err();
            assert!(
                full.to_string()
                    .contains("cut off at the output token limit"),
                "{full:#}"
            );
            let usage = provider
                .complete_with_usage(&messages, "preferred", None)
                .await
                .unwrap_err();
            assert!(
                usage
                    .to_string()
                    .contains("cut off at the output token limit"),
                "{usage:#}"
            );
            let text = provider
                .complete(&messages, "preferred", None)
                .await
                .unwrap_err();
            assert!(
                text.to_string()
                    .contains("cut off at the output token limit"),
                "{text:#}"
            );
            assert_eq!(
                *replies.locked(),
                vec![("fallback".into(), 10); 8],
                "reported usage from rejected responses must count too"
            );
        }

        drop(provider);
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_admission_reloads_live_limit_before_dispatch() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(true, false);
    let (_root, _paths, cfg, daemon, server) = setup(&env, mock.clone(), Some(5.0), true).await;
    let start = |daemon: Arc<Daemon>, cfg: Config| {
        tokio::spawn(async move {
            let provider = daemon.provider(&cfg).await.unwrap();
            counted(
                "llm",
                "",
                "live-limit",
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
    arrived(&mock).await;
    let queued = start(daemon.clone(), cfg);
    entered(&daemon, 2).await;
    let changed = daemon
        .handle(Request::SetSetting {
            key: "limits.daily_usd".into(),
            value: json!(0.50),
        })
        .await
        .unwrap();
    assert_eq!(changed["applies"], "now");
    mock.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let failure = tokio::time::timeout(Duration::from_secs(10), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(failure.to_string().contains("daily_usd"), "{failure:#}");
    assert_eq!(
        mock.posts.load(Ordering::SeqCst),
        1,
        "queued request used pre-wait limit"
    );
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_admission_reprices_committed_usage_with_live_prices() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(true, false);
    let (_root, _paths, cfg, daemon, server) = setup(&env, mock.clone(), Some(5.0), true).await;
    let start = |daemon: Arc<Daemon>, cfg: Config| {
        tokio::spawn(async move {
            let provider = daemon.provider(&cfg).await.unwrap();
            counted(
                "llm",
                "",
                "live-price",
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
    arrived(&mock).await;
    let queued = start(daemon.clone(), cfg);
    entered(&daemon, 2).await;
    daemon
        .handle(Request::SetSetting {
            key: "provider.prices".into(),
            value: json!({"primary":{"input":500_000.0,"output":50_000.0}}),
        })
        .await
        .unwrap();
    mock.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let failure = tokio::time::timeout(Duration::from_secs(10), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(failure.to_string().contains("daily_usd"), "{failure:#}");
    assert_eq!(
        mock.posts.load(Ordering::SeqCst),
        1,
        "queued request used pre-wait price table"
    );
    teardown(daemon, server).await;
}

#[derive(Clone, Copy)]
enum ReportedFailure {
    Cutoff,
    Http,
    Drop,
    Event,
}

struct FailedMock {
    posts: AtomicUsize,
    arrived: Notify,
    release: Arc<Notify>,
}

async fn reported_failure_fixture(
    env: &crate::credential_env::EnvGuard,
    style: &'static str,
    failure: ReportedFailure,
) -> (
    Fixture,
    Paths,
    Config,
    Arc<Daemon>,
    Arc<FailedMock>,
    tokio::task::JoinHandle<()>,
) {
    env.set("FORMLESS_API_KEY", "reported-failure-fixture");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let mock = Arc::new(FailedMock {
        posts: AtomicUsize::new(0),
        arrived: Notify::new(),
        release: Arc::new(Notify::new()),
    });
    let state = mock.clone();
    let app=axum::Router::new().fallback(move || {
        let mock=state.clone();
        async move {
            mock.posts.fetch_add(1,Ordering::SeqCst);
            mock.arrived.notify_one();
            match failure {
                ReportedFailure::Cutoff|ReportedFailure::Http=>{
                    mock.release.notified().await;
                    if matches!(failure,ReportedFailure::Http) {
                        (axum::http::StatusCode::SERVICE_UNAVAILABLE,axum::Json(json!({"error":{"type":"api_error","message":"charged failure"},"usage":{"prompt_tokens":10,"completion_tokens":0}}))).into_response()
                    } else {
                        axum::Json(json!({"content":[{"type":"text","text":"cut off"}],"stop_reason":"max_tokens","usage":{"input_tokens":10,"output_tokens":0}})).into_response()
                    }
                },
                ReportedFailure::Drop|ReportedFailure::Event=>{
                    let usage=match style {
                        "anthropic"=>json!({"type":"message_start","message":{"usage":{"input_tokens":10}}}),
                        "responses"=>json!({"type":"response.created","response":{"usage":{"input_tokens":10}}}),
                        _=>json!({"choices":[],"usage":{"prompt_tokens":10}}),
                    };
                    let content=match style {
                        "anthropic"=>json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}),
                        "responses"=>json!({"type":"response.output_text.delta","delta":"partial"}),
                        _=>json!({"choices":[{"delta":{"content":"partial"}}]}),
                    };
                    // Error-event retries are eligible before text is shown;
                    // transport drops are also retried after partial text.
                    let first=if matches!(failure,ReportedFailure::Drop) {
                        let block=if style=="anthropic" {format!("data: {}\n\n",json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}))} else {String::new()};
                        format!("data: {usage}\n\n{block}data: {content}\n\n")
                    } else {format!("data: {usage}\n\n")};
                    let release=mock.release.clone();
                    let stream=futures_util::stream::unfold(0,move |phase| {
                        let release=release.clone(); let first=first.clone();
                        async move {
                            match phase {
                                0=>Some((Ok::<_,std::io::Error>(first),1)),
                                1=>{
                                    release.notified().await;
                                    if matches!(failure,ReportedFailure::Drop) {
                                        Some((Err(std::io::Error::other("controlled paid stream drop")),2))
                                    } else {
                                        let error=match style {
                                            "anthropic"=>json!({"type":"error","error":{"type":"overloaded_error","message":"charged error"}}),
                                            "responses"=>json!({"type":"response.failed","response":{"error":{"code":"server_error","message":"charged error"},"usage":{"input_tokens":10}}}),
                                            _=>json!({"error":{"code":"server_error","message":"charged error"},"usage":{"prompt_tokens":10}}),
                                        };
                                        Some((Ok(format!("data: {error}\n\n")),2))
                                    }
                                },
                                _=>None,
                            }
                        }
                    });
                    axum::response::Response::builder().header("content-type","text/event-stream").body(axum::body::Body::from_stream(stream)).unwrap()
                }
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let root =
        Fixture(std::env::temp_dir().join(format!("fl-reported-charge-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(root.0.join("data"), root.0.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.provider.api_style = style.into();
    cfg.provider.model = "primary".into();
    cfg.provider.max_retries = 1;
    cfg.limits.daily_usd = Some(0.50);
    cfg.provider.prices.insert(
        "primary".into(),
        crate::config::ModelPrice {
            input: 100_000.0,
            output: 100_000.0,
            cached_input: None,
            cache_write_input: None,
            retrieved_at: None,
        },
    );
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (root, paths, cfg, daemon, mock, server)
}

async fn failed_arrived(mock: &FailedMock) {
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
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_anthropic_cutoff_commits_reported_charge_before_queued_admission() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, _paths, cfg, daemon, mock, server) =
        reported_failure_fixture(&env, "anthropic", ReportedFailure::Cutoff).await;
    let start = |daemon: Arc<Daemon>, cfg: Config| {
        tokio::spawn(async move {
            let provider = daemon.provider(&cfg).await.unwrap();
            counted(
                "llm",
                "",
                "paid-cutoff",
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
    failed_arrived(&mock).await;
    let second = start(daemon.clone(), cfg.clone());
    entered(&daemon, 2).await;
    mock.release.notify_one();
    let error = tokio::time::timeout(Duration::from_secs(10), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cut off at the output token limit"),
        "{error:#}"
    );
    let refused = tokio::time::timeout(Duration::from_secs(10), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(refused.to_string().contains("daily_usd"), "{refused:#}");
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        daemon
            .usage_info(None, Some("paid-cutoff"), &cfg)
            .unwrap()
            .cost,
        Some(1.0)
    );
    let rows = daemon
        .with_store(|st| st.usage_grouped(None, "purpose"))
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((&*rows[0].0, &*rows[0].2), ("llm", "primary"));
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reported_http_error_charge_blocks_automatic_retry() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, _paths, cfg, daemon, mock, server) =
        reported_failure_fixture(&env, "chat", ReportedFailure::Http).await;
    let task = tokio::spawn({
        let daemon = daemon.clone();
        let cfg = cfg.clone();
        async move {
            let provider = daemon.provider(&cfg).await.unwrap();
            counted(
                "llm",
                "",
                "paid-http-error",
                provider.complete_result(
                    &[json!({"role":"user","content":"work"})],
                    "primary",
                    None,
                ),
            )
            .await
        }
    });
    failed_arrived(&mock).await;
    mock.release.notify_one();
    let refused = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(refused.to_string().contains("daily_usd"), "{refused:#}");
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        daemon
            .usage_info(None, Some("paid-http-error"), &cfg)
            .unwrap()
            .cost,
        Some(1.0)
    );
    teardown(daemon, server).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn known_stream_usage_survives_transport_drop_and_error_event_before_retry() {
    let env = crate::credential_env::EnvGuard::isolated();
    for style in ["chat", "responses", "anthropic"] {
        for failure in [ReportedFailure::Drop, ReportedFailure::Event] {
            let (_root, _paths, cfg, daemon, mock, server) =
                reported_failure_fixture(&env, style, failure).await;
            let parsed = Arc::new(Notify::new());
            let visible = parsed.clone();
            let task = tokio::spawn({
                let daemon = daemon.clone();
                let cfg = cfg.clone();
                async move {
                    let provider = daemon.provider(&cfg).await.unwrap();
                    counted(
                        "agent",
                        "",
                        "paid-stream",
                        provider.chat_stream(
                            &[json!({"role":"user","content":"work"})],
                            &[],
                            "primary",
                            None,
                            &CancellationToken::new(),
                            move |kind, _| {
                                if kind
                                    == if matches!(failure, ReportedFailure::Drop) {
                                        crate::provider::DeltaKind::Content
                                    } else {
                                        crate::provider::DeltaKind::Bytes
                                    }
                                {
                                    visible.notify_one();
                                }
                            },
                        ),
                    )
                    .await
                }
            });
            tokio::time::timeout(Duration::from_secs(10), parsed.notified())
                .await
                .unwrap();
            mock.release.notify_one();
            let refused = tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(
                refused.to_string().contains("daily_usd"),
                "{style}: {refused:#}"
            );
            assert_eq!(
                mock.posts.load(Ordering::SeqCst),
                1,
                "{style} retried a known paid attempt past its cap"
            );
            assert_eq!(
                daemon
                    .usage_info(None, Some("paid-stream"), &cfg)
                    .unwrap()
                    .cost,
                Some(1.0),
                "{style}"
            );
            teardown(daemon, server).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_native_stream_after_reported_input_commits_known_charge() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (_root, _paths, cfg, daemon, mock, server) =
        reported_failure_fixture(&env, "anthropic", ReportedFailure::Drop).await;
    let parsed = Arc::new(Notify::new());
    let visible = parsed.clone();
    let task = tokio::spawn({
        let daemon = daemon.clone();
        let cfg = cfg.clone();
        async move {
            let provider = daemon.provider(&cfg).await.unwrap();
            counted(
                "agent",
                "",
                "cancel-paid-stream",
                provider.chat_stream(
                    &[json!({"role":"user","content":"work"})],
                    &[],
                    "primary",
                    None,
                    &CancellationToken::new(),
                    move |kind, _| {
                        if kind == crate::provider::DeltaKind::Content {
                            visible.notify_one();
                        }
                    },
                ),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(10), parsed.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        daemon
            .usage_info(None, Some("cancel-paid-stream"), &cfg)
            .unwrap()
            .cost,
        Some(1.0)
    );
    let provider = daemon.provider(&cfg).await.unwrap();
    let refused = provider
        .complete_result(&[json!({"role":"user","content":"next"})], "primary", None)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("daily_usd"));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    mock.release.notify_one();
    drop(provider);
    teardown(daemon, server).await;
}

#[test]
fn model_failures_exclude_typed_admission_and_accounting_errors_only() {
    let budget = anyhow::Error::new(RequestFailure::budget("arbitrary admission wording"))
        .context("outer request context");
    assert!(!is_model_failure(&budget));
    let accounting = anyhow!("injected persistence detail")
        .context(crate::provider::UsageAccountingFailure)
        .context("outer request context");
    assert!(!is_model_failure(&accounting));
    assert!(is_model_failure(&anyhow!(
        "provider error: budget exhausted at remote provider"
    )));
    assert!(is_model_failure(&anyhow!(
        "provider error: 503 unavailable"
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn run_accounting_failure_is_local_but_actual_http_fault_still_counts() {
    let env = crate::credential_env::EnvGuard::isolated();
    for accounting in [true, false] {
        let mock = Mock::new(false, !accounting);
        let (_root, paths, mut cfg, daemon, server) =
            setup(&env, mock.clone(), Some(5.0), true).await;
        cfg.provider.fallback_models.clear();
        cfg.save(&paths).unwrap();
        let db = rusqlite::Connection::open(paths.db_file()).unwrap();
        if accounting {
            db.execute_batch("CREATE TRIGGER reject_run_usage BEFORE INSERT ON run_usage BEGIN SELECT RAISE(FAIL,'injected run accounting failure'); END;").unwrap();
        }
        let sid = daemon.with_store(|st| st.create_session(None)).unwrap().id;
        daemon
            .handle(Request::SendMessage {
                session_id: sid.clone(),
                content: "answer".into(),
                mode: SendMode::Send,
                client_msg_id: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let status = daemon
                    .with_store(|st| st.get_session(&sid))
                    .unwrap()
                    .unwrap()
                    .last_run_status;
                if matches!(
                    status.as_deref(),
                    Some("paused" | "failed" | "interrupted" | "completed")
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
        let failures = daemon
            .with_store(|st| st.list_model_stats())
            .unwrap()
            .iter()
            .map(|s| s.failures)
            .sum::<i64>();
        assert_eq!(
            failures,
            if accounting { 0 } else { 1 },
            "local accounting failure attribution={accounting}"
        );
        drop(db);
        teardown(daemon, server).await;
    }
}

#[tokio::test]
async fn strict_dollar_limits_refuse_unknown_native_input_before_http_or_admission() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(false, false);
    let (_root, paths, mut cfg, daemon, server) = setup(&env, mock.clone(), Some(1.0), false).await;
    cfg.provider.pdf_input = Some(true);
    cfg.save(&paths).unwrap();
    let sid = daemon
        .with_store(|st| Ok(st.create_session(None)?.id))
        .unwrap();
    daemon
        .handle(Request::AttachFile {
            session_id: sid.clone(),
            name: "private.pdf".into(),
            data_b64: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                b"%PDF-1.4\n%%EOF\n",
            ),
            mime: Some("application/pdf".into()),
        })
        .await
        .unwrap();
    let (run, messages) = daemon
        .with_store(|st| {
            let run = st.create_run(&sid)?;
            let held = st.take_pending_attachments(&sid)?;
            let m = st.append_message_meta(
                &sid,
                "user",
                "analyze",
                Some(json!({"attachments":held}).to_string()),
            )?;
            Ok((run, vec![crate::storage::provider_message(&m)]))
        })
        .unwrap();
    let provider = daemon.provider(&cfg).await.unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        counted(
            "run",
            &sid,
            &run,
            provider.complete(&messages, "primary", None),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
        error.downcast_ref::<RequestFailure>().is_some(),
        "{error:#}"
    );
    assert!(error.to_string().contains("cannot be bounded locally"));
    assert_eq!(mock.posts.load(Ordering::SeqCst), 0);
    assert_eq!(
        daemon.budget_admission.entered.load(Ordering::SeqCst),
        0,
        "refusal precedes paid-attempt admission/coverage"
    );
    assert!(daemon
        .with_store(|st| st.usage_rows(None, Some(&run)))
        .unwrap()
        .0
        .is_empty());
    teardown(daemon, server).await;
}

#[tokio::test]
async fn unreported_native_cost_stays_unknown_after_price_changes_restart_and_capping() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mock = Mock::new(false, false);
    let (_root, paths, mut cfg, daemon, server) = setup(&env, mock.clone(), None, false).await;
    cfg.provider.pdf_input = Some(true);
    cfg.save(&paths).unwrap();
    let sid = daemon
        .with_store(|st| Ok(st.create_session(None)?.id))
        .unwrap();
    daemon
        .handle(Request::AttachFile {
            session_id: sid.clone(),
            name: "private.pdf".into(),
            data_b64: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                b"%PDF-1.4\n%%EOF\n",
            ),
            mime: Some("application/pdf".into()),
        })
        .await
        .unwrap();
    let (run, messages) = daemon
        .with_store(|st| {
            let run = st.create_run(&sid)?;
            let held = st.take_pending_attachments(&sid)?;
            let m = st.append_message_meta(
                &sid,
                "user",
                "analyze",
                Some(json!({"attachments":held}).to_string()),
            )?;
            Ok((run, vec![crate::storage::provider_message(&m)]))
        })
        .unwrap();
    let provider = daemon.provider(&cfg).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        counted(
            "run",
            &sid,
            &run,
            provider.complete_result(&messages, "primary", None),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.usage.unwrap().cost_unknown);
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        daemon.usage_info(None, Some(&run), &cfg).unwrap().cost,
        None,
        "generic text prices cannot invent native cost"
    );
    assert_eq!(
        daemon
            .with_store(|st| st.forecast_usage(&run, &cfg))
            .unwrap()
            .0,
        None
    );
    drop(provider);
    drop(daemon);
    cfg.provider.prices.get_mut("primary").unwrap().input = 1.0;
    cfg.limits.run_usd = Some(100.0);
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    assert_eq!(
        daemon.usage_info(None, Some(&run), &cfg).unwrap().cost,
        None,
        "unknown basis survives restart and settings changes"
    );
    let provider = daemon.provider(&cfg).await.unwrap();
    let error = counted("run", &sid, &run, provider.complete(&[], "primary", None))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("native media cost is unknown"),
        "{error:#}"
    );
    assert_eq!(
        mock.posts.load(Ordering::SeqCst),
        1,
        "a later text-only request cannot bypass unknown historical strict spend"
    );
    drop(provider);
    teardown(daemon, server).await;
}
