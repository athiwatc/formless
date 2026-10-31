//! Default and named internal providers report global breaker changes without a
//! main Run attaching a Session hook (compaction, optimiser and nested callers).
use super::*;
use axum::{response::IntoResponse, routing::post, Json, Router};

#[tokio::test]
async fn internal_provider_factories_broadcast_unattributed_breaker_notes() {
    let env = crate::credential_env::EnvGuard::isolated();
    let app = Router::new().route(
        "/chat/completions",
        post(|Json(body): Json<Value>| async move {
            if body["model"]
                .as_str()
                .is_some_and(|model| model.starts_with("bad"))
            {
                (axum::http::StatusCode::NOT_FOUND, "model not found").into_response()
            } else {
                Json(json!({"choices":[{"message":{"content":"ok"}}]})).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    env.set("FORMLESS_API_KEY", "k");
    env.set("FORMLESS_API_KEY_WORK", "k");
    env.set("FORMLESS_BASE_URL", &base);
    env.set("FORMLESS_MODEL", "bad");
    let root = std::env::temp_dir().join(format!("fl-breaker-note-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "bad".into();
    cfg.provider.fallback_models = vec!["good".into()];
    cfg.provider.max_retries = 0;
    cfg.naming.enabled = false;
    cfg.providers.insert("work".into(), cfg.provider.clone());
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let session = daemon
        .with_store(|store| Ok(store.create_session(None)?.id))
        .unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    daemon.subs.locked().push(Subscriber {
        conn: 1,
        tx,
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gap: None,
    });
    for name in [None, Some("work")] {
        let provider = daemon.provider_named(&cfg, name).await.unwrap();
        // This is the same factory used outside main Run steps. No Session
        // exists here and no client-specific note hook is installed.
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                provider.complete_with_usage(&[], "bad", None)
            )
            .await
            .expect("internal provider fixture did not finish")
            .unwrap()
            .0,
            "ok"
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let ServerMessage::Event(Event::Note { session_id, text }) = event else {
            panic!("expected breaker note: {event:?}")
        };
        assert_eq!(session_id, None);
        assert!(
            text.contains(&format!("provider '{}': bad", name.unwrap_or("default"))),
            "{text}"
        );
        assert!(text.contains("model is unavailable"), "{text}");
        assert!(!text.contains("Bearer"));

        // A main Run attaches its own warning hook after the factory has
        // installed a global breaker hook. This must not replace that hook.
        let scoped = daemon
            .provider_named(&cfg, name)
            .await
            .unwrap()
            .with_note_hook({
                let (session, subs) = (session.clone(), daemon.subs.clone());
                Arc::new(move |text| {
                    send_all(
                        &subs,
                        Event::Note {
                            session_id: Some(session.clone()),
                            text,
                        },
                    )
                })
            });
        let image = root.join("missing-private-image.png");
        let prepared = scoped.prepare_attachments(
            &[json!({"role":"user", "content":"image", "_attachments":[{
                "path":image.to_string_lossy(), "mime":"image/png", "name":"private-image.png"
            }]})],
            "gpt-4o",
        );
        assert!(prepared[0]["content"]
            .to_string()
            .contains("could not be attached"));
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let ServerMessage::Event(Event::Note { session_id, text }) = event else {
            panic!("expected Session image warning: {event:?}")
        };
        assert_eq!(session_id.as_deref(), Some(session.as_str()));
        assert!(text.contains("private-image.png"), "{text}");
        assert!(
            rx.try_recv().is_err(),
            "image warning was also broadcast globally"
        );

        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                scoped.complete_with_usage(&[], "bad_session", None)
            )
            .await
            .unwrap()
            .unwrap()
            .0,
            "ok"
        );
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let ServerMessage::Event(Event::Note { session_id, text }) = event else {
            panic!("expected global breaker transition: {event:?}")
        };
        assert_eq!(
            session_id, None,
            "Session warning hook captured breaker transition"
        );
        assert!(
            text.contains(&format!(
                "provider '{}': bad_session",
                name.unwrap_or("default")
            )),
            "{text}"
        );
        assert!(text.contains("model is unavailable"), "{text}");
        assert!(!text.contains("private-image.png"));
    }
    drop(daemon);
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "{error}");
    }
    std::fs::remove_dir_all(root).unwrap();
}
