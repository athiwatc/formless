//! `/compact` on demand (SPEC §11).

#[path = "support/env.rs"]
mod credential_env;

use axum::routing::post;
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::{Arc, Mutex};

#[tokio::test(flavor = "multi_thread")]
async fn compact_session_summarises_everything_with_the_users_focus() {
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FORMLESS_API_KEY", "k");
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body);
                    axum::Json(serde_json::json!({"choices": [{"message": {"content": "SUMMARY: chose Postgres"}}]}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-compact-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-compact-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "m".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    assert!(daemon
        .handle(Request::CompactSession {
            session_id: sid.clone(),
            focus: None,
        })
        .await
        .unwrap_err()
        .to_string()
        .contains("nothing to compact"));

    store.append_message(&sid, "user", "which db?").unwrap();
    let last = store
        .append_message(&sid, "assistant", "Postgres, because…")
        .unwrap();
    let r = daemon
        .handle(Request::CompactSession {
            session_id: sid.clone(),
            focus: Some("the database decision".into()),
        })
        .await
        .unwrap();
    assert_eq!(r["started"], true);
    let mut checkpoint = None;
    for _ in 0..100 {
        checkpoint = store.latest_checkpoint(&sid).unwrap();
        if checkpoint.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let (summary, up_to) = checkpoint.expect("compacted");
    assert_eq!(up_to, last.seq, "everything so far");
    assert!(summary.contains("chose Postgres"));
    let body = seen.lock().unwrap()[0].to_string();
    assert!(body.contains("focus on: the database decision"), "{body}");
}
