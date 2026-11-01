//! Stage-one model tests use only private files and a loopback provider.
use super::*;
use axum::response::IntoResponse;
struct Fixture {
    daemon: Option<Arc<Daemon>>,
    root: std::path::PathBuf,
    session: String,
    base: String,
    count: Arc<std::sync::atomic::AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Semaphore>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(env: &crate::credential_env::EnvGuard, auto: bool) -> Self {
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");
        env.set("FORMLESS_API_KEY", "private-loopback-key");
        env.set("NO_PROXY", "127.0.0.1,localhost");
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let app=axum::Router::new().route("/chat/completions",axum::routing::post({let count=count.clone();let entered=entered.clone();let release=release.clone();move |axum::Json(body):axum::Json<Value>| {let count=count.clone();let entered=entered.clone();let release=release.clone();async move {
            count.fetch_add(1,Ordering::SeqCst);
            if body["stream"] == true {
                entered.notify_one();
                let Ok(permit)=release.acquire().await else {return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response()};
                permit.forget();
                let chunks=[json!({"choices":[{"delta":{"content":"o"}}]}),json!({"choices":[{"delta":{"content":"k"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}})];
                let text=chunks.into_iter().map(|value|format!("data: {value}\n\n")).collect::<String>()+"data: [DONE]\n\n";
                ([("content-type","text/event-stream")],text).into_response()
            } else {axum::Json(json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"private-call","type":"function","function":{"name":"probe_ping","arguments":"{}"}}]}}],"usage":{"prompt_tokens":4,"completion_tokens":3}})).into_response()}
        }}}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root =
            std::env::temp_dir().join(format!("fl-model-test-daemon-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = base.clone();
        cfg.provider.max_retries = 0;
        cfg.provider.auto_test_models = auto;
        cfg.provider.aliases.insert("alias".into(), "model".into());
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        assert_eq!(
            daemon.config().unwrap().provider.base_url,
            base,
            "fixture must never send to an inherited endpoint"
        );
        let session = daemon.with_store(|st| st.create_session(None)).unwrap().id;
        Self {
            daemon: Some(daemon),
            root,
            session,
            base,
            count,
            entered,
            release,
            server,
        }
    }
    fn daemon(&self) -> &Arc<Daemon> {
        self.daemon.as_ref().unwrap()
    }
    async fn selected(&self, model: &str) {
        self.daemon()
            .handle(Request::SetSessionModel {
                id: self.session.clone(),
                model: Some(model.into()),
                reasoning_effort: None,
            })
            .await
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.release.close();
        self.server.abort();
        drop(self.daemon.take());
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
#[tokio::test]
async fn default_model_selection_sends_nothing_but_explicit_test_still_runs() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, false).await;
    assert!(!ProviderCfg::default().auto_test_models);
    f.selected("model").await;
    assert!(f.daemon().test_new_model(&f.session, "alias").is_none());
    assert_eq!(f.count.load(Ordering::SeqCst), 0);
    assert!(f.daemon().model_tests().is_empty());
    f.release.add_permits(1);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.daemon().handle(Request::TestModel {
            model: "alias".into(),
            provider: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["model"], "model");
    assert_eq!(f.count.load(Ordering::SeqCst), 2);
    assert!(f
        .daemon()
        .model_tests()
        .contains_key(&format!("{} model", f.base)));
}
#[tokio::test]
async fn concurrent_picks_share_one_background_test_and_completion_is_cached() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, true).await;
    let task = f.daemon().test_new_model(&f.session, "model").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    f.selected("alias").await;
    assert!(f.daemon().test_new_model(&f.session, "alias").is_none());
    assert_eq!(f.count.load(Ordering::SeqCst), 1);
    f.release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.count.load(Ordering::SeqCst), 2);
    assert!(f
        .daemon()
        .model_tests()
        .contains_key(&format!("{} model", f.base)));
    assert!(f.daemon().test_new_model(&f.session, "model").is_none());
}
#[tokio::test]
async fn aborting_background_test_releases_reservation_without_marking_it_tested() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, true).await;
    let task = f.daemon().test_new_model(&f.session, "model").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    assert!(f
        .daemon()
        .model_test_cache
        .reserve(&f.base, "model")
        .unwrap()
        .is_some());
    assert!(f.daemon().model_tests().is_empty());
}
#[tokio::test]
async fn explicit_result_save_failure_is_visible_and_keeps_existing_bad_cache() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, false).await;
    let file = f.daemon().paths.data_dir.join("model_tests.json");
    std::fs::write(&file, b"{private broken fixture").unwrap();
    f.release.add_permits(1);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.daemon().handle(Request::TestModel {
            model: "model".into(),
            provider: None,
        }),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(format!("{error:#}").contains("result could not be saved"));
    assert_eq!(std::fs::read(&file).unwrap(), b"{private broken fixture");
    assert_eq!(f.count.load(Ordering::SeqCst), 2);
}
#[test]
fn auto_test_setting_is_default_off_roundtrips_and_is_documented_settable() {
    let mut cfg = Config::default();
    assert!(!cfg.provider.auto_test_models);
    cfg.set_key("provider.auto_test_models", json!(true))
        .unwrap();
    assert!(cfg.provider.auto_test_models);
    let loaded: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
    assert!(loaded.provider.auto_test_models);
    assert!(crate::config::setting_docs().contains_key("provider.auto_test_models"));
}

#[tokio::test]
async fn background_save_failure_is_a_visible_note_and_preserves_existing_cache() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, true).await;
    let (tx, mut events) = mpsc::channel(64);
    f.daemon().subs.locked().push(Subscriber {
        conn: 1045,
        tx,
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    let task = f.daemon().test_new_model(&f.session, "model").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    let file = f.daemon().paths.data_dir.join("model_tests.json");
    std::fs::write(&file, b"{private concurrent cache damage").unwrap();
    f.release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let note = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(message) = events.recv().await {
            if let ServerMessage::Event(Event::Note { session_id, text }) = message {
                return (session_id, text);
            }
        }
        panic!("private event stream closed")
    })
    .await
    .unwrap();
    assert_eq!(note.0.as_deref(), Some(f.session.as_str()));
    assert!(note.1.contains("result could not be saved"));
    assert_eq!(
        std::fs::read(file).unwrap(),
        b"{private concurrent cache damage"
    );
    assert_eq!(f.count.load(Ordering::SeqCst), 2);
    assert!(
        f.daemon().test_new_model(&f.session, "model").is_none(),
        "invalid cache prevents another automatic spend"
    );
}

#[tokio::test]
async fn failed_model_selection_never_starts_an_opted_in_background_test() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, true).await;
    let missing = f
        .daemon()
        .handle(Request::SetSessionModel {
            id: "private-missing-session".into(),
            model: Some("model".into()),
            reasoning_effort: None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        missing
            .downcast_ref::<RequestFailure>()
            .map(|failure| failure.code),
        Some(codes::NOT_FOUND)
    );
    assert_eq!(f.count.load(Ordering::SeqCst), 0);
    assert!(f.daemon().model_tests().is_empty());
    assert!(!f.daemon().paths.data_dir.join("model_tests.json").exists());
    // The background reservation is acquired synchronously before spawning:
    // obtaining it here proves no latent task can start after this assertion.
    let reservation = f
        .daemon()
        .model_test_cache
        .reserve(&f.base, "model")
        .unwrap()
        .expect("a rejected selection must not hold a background reservation");
    drop(reservation);

    // Exercise an actual database write refusal as well as a missing ID.
    // This trigger belongs only to this UUID fixture, never a live profile.
    let conn = rusqlite::Connection::open(f.daemon().paths.db_file()).unwrap();
    conn.execute_batch("CREATE TRIGGER private_reject_model BEFORE UPDATE OF model ON sessions BEGIN SELECT RAISE(ABORT, 'private injected model write refusal'); END;").unwrap();
    let refused = f
        .daemon()
        .handle(Request::SetSessionModel {
            id: f.session.clone(),
            model: Some("model".into()),
            reasoning_effort: None,
        })
        .await
        .unwrap_err();
    assert!(format!("{refused:#}").contains("private injected model write refusal"));
    assert_eq!(f.count.load(Ordering::SeqCst), 0);
    assert!(!f.daemon().paths.data_dir.join("model_tests.json").exists());
    let reservation = f
        .daemon()
        .model_test_cache
        .reserve(&f.base, "model")
        .unwrap()
        .expect("a failed database mutation must not hold a background reservation");
    drop(reservation);
    conn.execute_batch("DROP TRIGGER private_reject_model;")
        .unwrap();
}

#[tokio::test]
async fn chained_alias_uses_the_same_background_reservation_and_result_label() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, true).await;
    let mut cfg = f.daemon().config().unwrap();
    // The fixture already maps alias -> model. Keep provider-level alias
    // behavior unchanged; test only matching reservation/result identities.
    cfg.provider
        .aliases
        .insert("model".into(), "physical-model".into());
    cfg.save(&f.daemon().paths).unwrap();
    let task = f.daemon().test_new_model(&f.session, "alias").unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), f.entered.notified())
        .await
        .unwrap();
    f.release.add_permits(1);
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let results = f.daemon().model_tests();
    assert!(results.contains_key(&format!("{} model", f.base)));
    assert!(!results.contains_key(&format!("{} physical-model", f.base)));
    assert_eq!(f.count.load(Ordering::SeqCst), 2);
    assert!(f.daemon().test_new_model(&f.session, "alias").is_none());
    f.selected("alias").await;
    assert_eq!(
        f.count.load(Ordering::SeqCst),
        2,
        "cached selection starts no second background task"
    );
    // Explicit Test is intentional new work, regardless of that cache.
    f.release.add_permits(1);
    let fresh = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.daemon().handle(Request::TestModel {
            model: "alias".into(),
            provider: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fresh["model"], "model");
    assert_eq!(fresh["ok"], true);
    assert_eq!(f.count.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn explicit_probe_uses_factory_daily_budget_and_records_known_usage_once() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, false).await;
    f.release.add_permits(1);
    let result = f
        .daemon()
        .handle(Request::TestModel {
            model: "model".into(),
            provider: None,
        })
        .await
        .unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["probe_usage"]["reported_usage"]["prompt_tokens"], 7);
    assert_eq!(
        result["probe_usage"]["reported_usage"]["completion_tokens"],
        5
    );
    let db = rusqlite::Connection::open(f.daemon().paths.db_file()).unwrap();
    let (rows,input,output):(i64,i64,i64)=db.query_row("SELECT COUNT(*), SUM(prompt_tokens), SUM(completion_tokens) FROM run_usage WHERE source='internal'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        (rows, input, output),
        (2, 7, 5),
        "probe observer must preserve factory usage hooks without duplicate rows"
    );
    drop(db);
    let mut cfg = f.daemon().config().unwrap();
    cfg.limits.daily_usd = Some(1.0);
    cfg.save(&f.daemon().paths).unwrap();
    f.daemon()
        .with_store(|st| {
            st.record_usage_full(
                "",
                "",
                "default",
                "model",
                &crate::provider::Usage {
                    cost: Some(2.0),
                    reported: 3,
                    ..Default::default()
                },
                "internal",
            )
        })
        .unwrap();
    let count = f.count.load(Ordering::SeqCst);
    let blocked = f
        .daemon()
        .handle(Request::TestModel {
            model: "model".into(),
            provider: None,
        })
        .await
        .unwrap();
    assert_eq!(blocked["ok"], false);
    assert!(
        blocked["error"].as_str().unwrap().contains("budget"),
        "{blocked}"
    );
    assert_eq!(blocked["probe_usage"]["requests_sent"], 0);
    assert_eq!(
        f.count.load(Ordering::SeqCst),
        count,
        "exhausted daily budget must prevent every model POST"
    );
}
