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
        env.set("FORMLESS_API_KEY", "private-loopback-key");
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
    assert_eq!(result["ok"], true);
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
