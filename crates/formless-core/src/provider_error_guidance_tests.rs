//! Real pause/resume banners against an owned failing provider.
use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn pause(
    d: &Arc<Daemon>,
    rx: &mut mpsc::Receiver<ServerMessage>,
    sid: &str,
) -> (String, ErrorExplained) {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(ServerMessage::Event(Event::RunPaused {
                session_id,
                run_id,
                explained: Some(explained),
                ..
            })) = rx.recv().await
            {
                if session_id == sid {
                    break (run_id, explained);
                }
            }
        }
    })
    .await
    .expect("private provider did not produce a pause banner");
    tokio::time::timeout(Duration::from_secs(10), async {
        while d.runs.locked().contains_key(sid) || d.session_is_publishing(sid) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("private Run did not finish publishing");
    result
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_provider_error_after_resume_escalates_but_new_input_resets_it() {
    let env = crate::credential_env::EnvGuard::isolated();
    for key in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
        env.remove(key);
    }
    env.set("FORMLESS_API_KEY", "private-unused-error-guidance");
    let root = Fixture(std::env::temp_dir().join(format!("fl-errors-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(root.0.join("d"), root.0.join("c")).unwrap();
    let posts = Arc::new(AtomicUsize::new(0));
    let observed = posts.clone();
    let app = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    "unknown parameter: reasoning_effort",
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.provider.model = "private-model".into();
    cfg.provider.max_retries = 0;
    cfg.provider.auto_test_models = false;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let d = Arc::new(Daemon::new(paths).unwrap());
    let sid = d.with_store(|st| st.create_session(None)).unwrap().id;
    let (tx, mut rx) = mpsc::channel(128);
    d.subs.locked().push(Subscriber {
        conn: 812,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: Default::default(),
    });
    d.handle(Request::SendMessage {
        session_id: sid.clone(),
        content: "work".into(),
        mode: SendMode::Send,
        client_msg_id: None,
    })
    .await
    .unwrap();
    let (first_id, first) = pause(&d, &mut rx, &sid).await;
    d.handle(Request::ResumeRun {
        session_id: sid.clone(),
    })
    .await
    .unwrap();
    let (second_id, second) = pause(&d, &mut rx, &sid).await;
    d.handle(Request::SendMessage {
        session_id: sid.clone(),
        content: "new task".into(),
        mode: SendMode::Send,
        client_msg_id: None,
    })
    .await
    .unwrap();
    let (_, fresh) = pause(&d, &mut rx, &sid).await;
    d.shutdown.cancel();
    let weak = Arc::downgrade(&d);
    drop(d);
    tokio::time::timeout(Duration::from_secs(10), async {
        while weak.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
    assert_ne!(
        first_id, second_id,
        "Resume must be covered across distinct Run ids"
    );
    assert_eq!(
        posts.load(Ordering::SeqCst),
        3,
        "persistent 400s must not retry automatically"
    );
    for e in [&first, &second, &fresh] {
        assert_eq!(e.category, "request");
        let action = e.action.as_deref().unwrap();
        for hint in ["/details", "/model", "formless doctor"] {
            assert!(action.contains(hint), "{action}");
        }
    }
    assert!(second.message.contains("again"), "{}", second.message);
    assert!(second
        .action
        .as_deref()
        .unwrap()
        .contains("provider.api_style"));
    assert!(second
        .action
        .as_deref()
        .unwrap()
        .contains("provider.reasoning_effort"));
    assert!(!first.message.contains("again"));
    assert!(!fresh.message.contains("again"));
}
