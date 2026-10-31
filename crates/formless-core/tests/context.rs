//! How full a Session's context is (SPEC §10.6).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn context_usage_estimates_until_a_prompt_is_measured() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-ctx-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-ctx-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "small-model".into();
    cfg.provider.context_tokens = 8000;
    cfg.provider
        .model_context_tokens
        .insert("small-model".into(), 4000);
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    store
        .append_message(&sid, "user", &"x".repeat(4000))
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let r = daemon
        .handle(Request::ContextUsage {
            session_id: sid.clone(),
        })
        .await
        .unwrap();
    assert_eq!(r["context_tokens"], 4000, "per-model size wins");
    assert_eq!(r["measured"], false);
    assert!(r["breakdown"]["history"].as_u64().unwrap() >= 1000);
    assert!(r["breakdown"]["system_prompt"].as_u64().unwrap() > 100);
    let used = r["used_tokens"].as_u64().unwrap();
    assert!(used > 1000, "{r}");
}

/// A `/models` listing that fails is tried again, not cached as "no
/// windows": the next look gets the endpoint's size (#770).
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_model_listing_is_retried() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let app = axum::Router::new().route(
        "/v1/models",
        axum::routing::get(move || {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            async move {
                if n == 0 {
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "busy").into_response()
                } else {
                    axum::Json(serde_json::json!({"data": [{"id": "m", "context_length": 32000}]}))
                        .into_response()
                }
            }
        }),
    );
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-ctxw-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-ctxw-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "m".into();
    cfg.provider.context_tokens = 8000;
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let ask = || {
        daemon.handle(Request::ContextUsage {
            session_id: sid.clone(),
        })
    };

    let first = ask().await.unwrap();
    assert_eq!(
        (
            first["context_tokens"].as_u64(),
            first["context_source"].as_str()
        ),
        (Some(8000), Some("default"))
    );
    let second = ask().await.unwrap();
    assert_eq!(
        (
            second["context_tokens"].as_u64(),
            second["context_source"].as_str()
        ),
        (Some(32000), Some("endpoint"))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // Listed now: no further fetches.
    ask().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
