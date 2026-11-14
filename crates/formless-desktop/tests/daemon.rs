//! Real daemon and HTTP provider seam in a private profile, with no GUI or
//! external credentials/provider. Runs on native macOS, Linux and Windows.
#![cfg(feature = "daemon-tests")]

#[path = "../../formless-core/tests/support/env.rs"]
mod credential_env;

use formless_core::{Config, Paths};
use formless_desktop::client::{Client, Command, Endpoint, Update};
use formless_desktop::model::{Destination, Intent};
use formless_protocol::{Request, SendMode};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

async fn until(client: &Client, stage: &'static str, mut predicate: impl FnMut(Update) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        for update in client.drain() {
            let summary = match &update {
                Update::Connection { ready, notice } => format!("connection {ready}: {notice}"),
                Update::Snapshot {
                    generation,
                    snapshot,
                } => format!(
                    "snapshot generation={generation} running={} blocks={} text={:?}",
                    snapshot.running,
                    snapshot.blocks.len(),
                    snapshot
                        .blocks
                        .iter()
                        .map(|b| b.markdown().chars().take(80).collect::<String>())
                        .collect::<Vec<_>>()
                ),
                Update::SnapshotError { error, .. } => format!("snapshot error {error}"),
                Update::Sessions(sessions) => format!("Sessions={}", sessions.len()),
                Update::Page {
                    destination,
                    result,
                } => format!("page {destination:?} ok={}", result.is_ok()),
                Update::Action { result, .. } => format!("action {result:?}"),
                Update::Notice(notice) => notice.clone(),
                Update::Attention { pending, .. } => format!("attention pending={pending}"),
            };
            eprintln!("{stage}: {summary}");
            if predicate(update) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "desktop daemon integration deadline expired at {stage}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_daemon_provider_send_navigation_resources_and_reconnect() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-desktop-fixture-key");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
    let app = axum::Router::new()
        .route("/models", axum::routing::get(|| async { axum::Json(json!({"data":[{"id":"desktop-test-model"}]})) }))
        .route("/messages", axum::routing::post({
            let requests = requests.clone();
            move |axum::Json(body): axum::Json<Value>| {
                let requests = requests.clone();
                async move {
                    requests.lock().unwrap().push(body);
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let events = [
                        json!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Canonical provider reply ไทย 😀"}}),
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
                        json!({"type":"message_stop"}),
                    ];
                    let body: String = events.iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect();
                    axum::response::Response::builder().header("content-type", "text/event-stream").body(axum::body::Body::from(body)).unwrap()
                }
            }
        }));
    let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let root = std::env::temp_dir().join(format!(
        "fl-desk-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
    let mut config = Config::default();
    config.provider.base_url = base_url;
    config.provider.api_style = "anthropic".into();
    config.provider.model = "desktop-test-model".into();
    config.provider.max_retries = 0;
    config.naming.enabled = false;
    config.skills.auto_create = false;
    config.daemon.idle_grace_secs = 3600;
    config.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let a = daemon
        .handle(Request::CreateSession {
            name: Some("Desktop A".into()),
        })
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = daemon
        .handle(Request::CreateSession {
            name: Some("Desktop B".into()),
        })
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let serving = tokio::spawn(daemon.clone().serve());
    #[cfg(unix)]
    let endpoint = Endpoint(paths.socket_path());
    #[cfg(windows)]
    let endpoint = Endpoint(paths.pipe_name().into());
    let client = Client::start(endpoint.clone());
    until(&client, "stage-1", |u| {
        matches!(u, Update::Connection { ready: true, .. })
    })
    .await;
    client
        .send(Command::Select {
            session_id: Some(a.clone()),
            generation: 1,
        })
        .unwrap();
    until(
        &client,
        "stage-2",
        |u| matches!(u, Update::Snapshot { generation: 1, snapshot } if snapshot.blocks.is_empty()),
    )
    .await;
    client
        .send(Command::Intent(Intent::SendMessage {
            session_id: a.clone(),
            content: "Desktop real transport request".into(),
            mode: SendMode::Send,
        }))
        .unwrap();
    until(&client, "stage-3", |u| {
        matches!(u, Update::Action { result: Ok(()), .. })
    })
    .await;
    // Navigate while the actual provider request is outstanding. The
    // selected generation, rather than Session identity alone, owns replies.
    client
        .send(Command::Select {
            session_id: Some(b.clone()),
            generation: 2,
        })
        .unwrap();
    until(&client, "stage-4", |u| matches!(u, Update::Snapshot { generation: 2, snapshot } if snapshot.session_id == b && snapshot.blocks.is_empty())).await;
    client
        .send(Command::Select {
            session_id: Some(a.clone()),
            generation: 3,
        })
        .unwrap();
    until(&client, "stage-5", |u| matches!(u, Update::Snapshot { generation: 3, snapshot } if !snapshot.running && snapshot.blocks.iter().any(|block| block.markdown() == "Canonical provider reply ไทย 😀"))).await;
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "one user send caused one provider request"
    );
    for destination in [
        Destination::Tools,
        Destination::Metrics,
        Destination::Proposals,
        Destination::Settings,
    ] {
        client.send(Command::Page(destination)).unwrap();
        until(&client, "stage-6", |u| match u {
            Update::Page {
                destination: got,
                result: Ok(value),
            } if got == destination => {
                assert!(formless_desktop::pages::Page::decode(destination, value).is_ok());
                true
            }
            _ => false,
        })
        .await;
    }
    client.send(Command::CreateSession).unwrap();
    until(
        &client,
        "stage-7",
        |u| matches!(u, Update::Sessions(sessions) if sessions.len() == 3),
    )
    .await;

    daemon.handle(Request::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // serve() stops accepting; a real daemon process also drops its
    // runtime. This in-process fixture retains accepted tasks, so explicitly
    // retire the client's socket before testing restoration on a new daemon.
    client.send(Command::Reconnect).unwrap();
    until(&client, "stage-8", |u| {
        matches!(u, Update::Connection { ready: false, .. })
    })
    .await;
    let retired = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(Duration::from_secs(10), async {
        while retired.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("old fixture daemon tasks retained the Profile lease");
    let daemon2 = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let serving2 = tokio::spawn(daemon2.clone().serve());
    until(&client, "stage-9", |u| {
        matches!(u, Update::Connection { ready: true, .. })
    })
    .await;
    until(&client, "stage-10", |u| matches!(u, Update::Snapshot { generation: 3, snapshot } if snapshot.blocks.iter().filter(|block| block.markdown() == "Canonical provider reply ไทย 😀").count() == 1)).await;
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "reconnect did not replay a user mutation"
    );
    if let Ok(seconds) = std::env::var("FORMLESS_DESKTOP_NATIVE_HOLD_SECONDS") {
        let seconds: u64 = seconds.parse().expect("native fixture hold seconds");
        assert!(seconds <= 600);
        eprintln!("native-fixture-endpoint={}", endpoint.0.display());
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while Instant::now() < deadline {
            if std::env::var_os("FORMLESS_DESKTOP_NATIVE_RELEASE_FILE")
                .is_some_and(|path| std::path::Path::new(&path).exists())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    drop(client);
    daemon2.handle(Request::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), serving2)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    provider.abort();
    drop(daemon2);
    // Keep private fixture evidence on panic; remove only this test's tree.
    std::fs::remove_dir_all(root).unwrap();
}
