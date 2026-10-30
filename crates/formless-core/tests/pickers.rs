//! Model and file pickers: `ListModels` (SPEC §4.2) and `@` mentions (§5.6).

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

/// `@` mentions (SPEC §5.6): ListFiles finds files under a root, and
/// AttachPath attaches small files and references large ones and
/// directories, which reach the model with the next message.
#[tokio::test(flavor = "multi_thread")]
async fn mentions_list_attach_and_reference_files() {
    use axum::routing::post;
    std::env::set_var("FORMLESS_API_KEY", "key-a");
    let seen: Arc<std::sync::Mutex<Vec<serde_json::Value>>> = Arc::default();
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body);
                    let sse = format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        serde_json::json!({"choices": [{"delta": {"content": "ok"}}]})
                    );
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let paths = paths("mentions");
    let repo = paths.data_dir.parent().unwrap().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(repo.join("big.log"), vec![b'x'; 300 * 1024]).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "m".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let root = Some(repo.to_string_lossy().into_owned());

    let r = daemon
        .handle(Request::ListFiles {
            root: root.clone(),
            query: "main".into(),
            limit: Some(5),
            session_id: None,
        })
        .await
        .unwrap();
    assert_eq!(r["files"][0]["path"], "src/main.rs", "{r}");
    assert_eq!(r["query"], "main");

    let small = daemon
        .handle(Request::AttachPath {
            session_id: sid.clone(),
            path: "src/main.rs".into(),
            root: root.clone(),
        })
        .await
        .unwrap();
    assert_eq!(small["reference"], false);
    let big = daemon
        .handle(Request::AttachPath {
            session_id: sid.clone(),
            path: "big.log".into(),
            root: root.clone(),
        })
        .await
        .unwrap();
    assert_eq!(big["reference"], true);
    let dir = daemon
        .handle(Request::AttachPath {
            session_id: sid.clone(),
            path: "src".into(),
            root: root.clone(),
        })
        .await
        .unwrap();
    assert_eq!(dir["dir"], true);
    assert!(daemon
        .handle(Request::AttachPath {
            session_id: sid.clone(),
            path: "nope.txt".into(),
            root: root.clone(),
        })
        .await
        .is_err());

    daemon
        .handle(Request::SendMessage {
            session_id: sid.clone(),
            content: "explain @src/main.rs @big.log @src".into(),
            mode: formless_protocol::SendMode::Send,
            client_msg_id: None,
        })
        .await
        .unwrap();
    for _ in 0..100 {
        if !seen.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let body = seen.lock().unwrap()[0].to_string();
    assert!(body.contains(r#"[attached: \"main.rs\" (text/"#), "{body}");
    assert!(
        body.contains("big.log (307200 bytes); not attached"),
        "{body}"
    );
    assert!(body.contains("src (directory); not attached"), "{body}");
}
