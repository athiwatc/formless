//! `ListModels` for the model picker (SPEC §4.5, §17).

use axum::routing::get;
use formless_core::config::RoleCfg;
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

fn paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

#[tokio::test(flavor = "multi_thread")]
async fn list_models_returns_provider_models_roles_and_recent() {
    std::env::set_var("FORMLESS_API_KEY", "key-a");
    let app = axum::Router::new().route(
        "/models",
        get(|| async {
            axum::Json(serde_json::json!({"data": [
                {"id": "anthropic/claude-sonnet-4"}, {"id": "openai/gpt-4o"}
            ]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let paths = paths("models");
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "openai/gpt-4o".into();
    cfg.naming.enabled = false;
    cfg.roles.insert(
        "explorer".into(),
        RoleCfg {
            description: "cheap".into(),
            model: "small".into(),
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let older = store.create_session(None).unwrap().id;
    store
        .set_session_model(&older, Some("openai/gpt-4o".into()), None)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let newer = store.create_session(None).unwrap().id;
    store
        .set_session_model(&newer, Some("my-custom".into()), None)
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let r = daemon.handle(Request::ListModels).await.unwrap();
    assert_eq!(
        r["models"],
        serde_json::json!(["anthropic/claude-sonnet-4", "openai/gpt-4o"])
    );
    assert_eq!(r["roles"][0]["name"], "explorer");
    assert_eq!(r["roles"][0]["model"], "small");
    assert_eq!(
        r["recent"],
        serde_json::json!(["my-custom", "openai/gpt-4o"])
    );
    assert_eq!(r["current"]["model"], "openai/gpt-4o");
    assert!(r["error"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn list_models_reports_an_unreachable_endpoint() {
    std::env::set_var("FORMLESS_API_KEY", "key-a");
    let paths = paths("models-down");
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let r = daemon.handle(Request::ListModels).await.unwrap();
    assert_eq!(r["models"], serde_json::json!([]));
    assert!(r["error"].as_str().unwrap().contains("/models"), "{r}");
}
