//! Actual IPC/HTTP boundaries, not a simulated presentation event feed.
//! These characterize the existing protocol; they do not enable live display.

use super::*;
use std::collections::{HashSet, VecDeque};
use tokio::sync::Notify;

fn encode(events: &[Value]) -> String {
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect()
}

fn prefix(text: &str) -> String {
    encode(&[
        json!({"type":"message_start","message":{"usage":{"input_tokens":10}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
    ])
}

fn tail(text: &str) -> String {
    encode(&[
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}),
        json!({"type":"message_stop"}),
    ])
}

/// With no second chunk, releasing the gate ends an incomplete SSE response.
/// Otherwise it completes one response. Notify permits avoid timing sleeps.
fn gated_response(
    first: String,
    second: Option<String>,
    gate: Arc<Notify>,
) -> axum::response::Response {
    let chunks = VecDeque::from([Some(first), second]);
    let stream = futures_util::stream::unfold((chunks, gate), |(mut chunks, gate)| async move {
        if chunks.len() == 1 {
            gate.notified().await;
        }
        let chunk = chunks.pop_front()??;
        Some((Ok::<_, std::io::Error>(chunk), (chunks, gate)))
    });
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from_stream(stream))
        .unwrap()
}

struct Fixture {
    root: std::path::PathBuf,
    daemon: Arc<formless_core::daemon::Daemon>,
    provider: tokio::task::JoinHandle<()>,
    serving: tokio::task::JoinHandle<Result<(), String>>,
    session: String,
    endpoint: Endpoint,
}

impl Fixture {
    async fn start(app: axum::Router, retries: usize) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let root = std::env::temp_dir().join(format!(
            "fl-order-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        ));
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let mut config = Config::default();
        config.provider.base_url = base;
        config.provider.api_style = "anthropic".into();
        config.provider.model = "desktop-ordering-model".into();
        config.provider.max_retries = retries;
        config.naming.enabled = false;
        config.skills.auto_create = false;
        config.daemon.idle_grace_secs = 3600;
        config.save(&paths).unwrap();
        let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
        let session = daemon
            .handle(Request::CreateSession {
                name: Some("Private ordering fixture".into()),
            })
            .await
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let serving = tokio::spawn({
            let daemon = daemon.clone();
            async move { daemon.serve().await.map_err(|error| error.to_string()) }
        });
        #[cfg(unix)]
        let endpoint = Endpoint(paths.socket_path());
        #[cfg(windows)]
        let endpoint = Endpoint(paths.pipe_name().into());
        Self {
            root,
            daemon,
            provider,
            serving,
            session,
            endpoint,
        }
    }

    async fn finish(self) {
        self.daemon.handle(Request::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.serving)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        self.provider.abort();
        let _ = self.provider.await;
        let retired = Arc::downgrade(&self.daemon);
        drop(self.daemon);
        tokio::time::timeout(Duration::from_secs(10), async {
            while retired.strong_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ordering fixture retained its private Profile lease");
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

fn app(route: axum::routing::MethodRouter) -> axum::Router {
    axum::Router::new()
        .route(
            "/models",
            axum::routing::get(|| async {
                axum::Json(json!({"data":[{"id":"desktop-ordering-model"}]}))
            }),
        )
        .route("/messages", route)
}

fn assistant_messages(snapshot: &Value) -> Vec<&Value> {
    snapshot["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "assistant")
        .collect()
}

async fn snapshot(observer: &mut Observer, session: &str) -> Value {
    observer
        .request(Request::GetSession { id: session.into() })
        .await
}

// A fast provider may publish the event before the Send acknowledgement.
// Each predicate below names a unique boundary within its one-Run fixture.
async fn wait(observer: &mut Observer, predicate: impl FnMut(&Event) -> bool) {
    let mut predicate = predicate;
    if !observer.events.iter().any(&mut predicate) {
        observer.until(predicate).await;
    }
}

async fn send(observer: &mut Observer, session: &str) {
    let response = observer
        .request(Request::SendMessage {
            session_id: session.into(),
            content: "Exercise private stream ordering".into(),
            mode: SendMode::Send,
            client_msg_id: Some(uuid::Uuid::new_v4().to_string()),
        })
        .await;
    assert!(response.get("run_id").is_some(), "{response}");
}

fn run_ids(observer: &Observer) -> HashSet<&str> {
    observer
        .events
        .iter()
        .filter_map(|event| match event {
            Event::MessageDelta { run_id, .. }
            | Event::StreamRestarted { run_id, .. }
            | Event::MessageComplete { run_id, .. } => Some(run_id.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_replaces_partial_attempt_without_a_new_run_or_persisted_prefix() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-ordering-fixture-key");
    let dropped = Arc::new(Notify::new());
    let complete = Arc::new(Notify::new());
    let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
    let fixture = Fixture::start(
        app(axum::routing::post({
            let (dropped, complete, requests) =
                (dropped.clone(), complete.clone(), requests.clone());
            move |axum::Json(body): axum::Json<Value>| {
                let (dropped, complete, requests) =
                    (dropped.clone(), complete.clone(), requests.clone());
                async move {
                    let number = {
                        let mut requests = requests.lock().unwrap();
                        requests.push(body);
                        requests.len()
                    };
                    match number {
                        1 => gated_response(prefix("Discard this attempt ไทย "), None, dropped),
                        2 => gated_response(
                            prefix("Replacement attempt "),
                            Some(tail("漢字 😀")),
                            complete,
                        ),
                        _ => panic!("unexpected extra provider request {number}"),
                    }
                }
            }
        })),
        1,
    )
    .await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    send(&mut observer, &fixture.session).await;
    wait(&mut observer, |event| matches!(event, Event::MessageDelta { delta, .. } if delta == "Discard this attempt ไทย ")).await;
    assert!(assistant_messages(&snapshot(&mut observer, &fixture.session).await).is_empty());
    dropped.notify_one();
    wait(&mut observer, |event| {
        matches!(event, Event::StreamRestarted { .. })
    })
    .await;
    wait(&mut observer, |event| matches!(event, Event::MessageDelta { delta, .. } if delta == "Replacement attempt ")).await;
    assert!(assistant_messages(&snapshot(&mut observer, &fixture.session).await).is_empty());
    complete.notify_one();
    wait(&mut observer, |event| matches!(event, Event::MessageComplete { message, .. } if message.content == "Replacement attempt 漢字 😀")).await;
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    let final_snapshot = snapshot(&mut observer, &fixture.session).await;
    let messages = assistant_messages(&final_snapshot);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["content"], "Replacement attempt 漢字 😀");
    assert_eq!(
        run_ids(&observer).len(),
        1,
        "Run identity does not identify a provider attempt"
    );
    assert_eq!(
        observer
            .events
            .iter()
            .filter(|event| matches!(event, Event::StreamRestarted { .. }))
            .count(),
        1
    );
    assert_eq!(requests.lock().unwrap().len(), 2);
    eprintln!("ordering retry: 2 attempts, 1 Run, 1 restart, 1 canonical assistant; discarded prefix absent");
    drop(observer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_turn_and_final_answer_share_a_run_but_have_distinct_canonical_boundaries() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-ordering-fixture-key");
    let tool_done = Arc::new(Notify::new());
    let final_done = Arc::new(Notify::new());
    let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
    let fixture = Fixture::start(app(axum::routing::post({
        let (tool_done, final_done, requests) = (tool_done.clone(), final_done.clone(), requests.clone());
        move |axum::Json(body): axum::Json<Value>| {
            let (tool_done, final_done, requests) = (tool_done.clone(), final_done.clone(), requests.clone());
            async move {
                let number = { let mut requests = requests.lock().unwrap(); requests.push(body); requests.len() };
                match number {
                    // The intentionally missing Tool produces a local error
                    // result. No external command, file or provider Tool runs.
                    1 => gated_response(prefix("Checking fixture Tool ไทย. "), Some(encode(&[
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"ordering-call","name":"no_such_ordering_fixture_tool"}}),
                        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
                        json!({"type":"content_block_stop","index":1}),
                        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}),
                        json!({"type":"message_stop"}),
                    ])), tool_done),
                    2 => gated_response(prefix("Final answer "), Some(tail("漢字 😀")), final_done),
                    _ => panic!("unexpected extra provider request {number}"),
                }
            }
        }
    })), 0).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    send(&mut observer, &fixture.session).await;
    wait(&mut observer, |event| matches!(event, Event::MessageDelta { delta, .. } if delta == "Checking fixture Tool ไทย. ")).await;
    assert!(assistant_messages(&snapshot(&mut observer, &fixture.session).await).is_empty());
    tool_done.notify_one();
    wait(
        &mut observer,
        |event| matches!(event, Event::MessageDelta { delta, .. } if delta == "Final answer "),
    )
    .await;
    let mid = snapshot(&mut observer, &fixture.session).await;
    let assistants = assistant_messages(&mid);
    assert_eq!(assistants.len(), 1);
    assert_eq!(assistants[0]["content"], "Checking fixture Tool ไทย. ");
    assert!(observer.events.iter().any(|event| matches!(event, Event::MessageAppended { message, .. } if message.role == "assistant" && message.id == assistants[0]["id"].as_str().unwrap())));
    assert_eq!(
        mid["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        1
    );
    assert_eq!(mid["running"], true);
    final_done.notify_one();
    wait(&mut observer, |event| matches!(event, Event::MessageComplete { message, .. } if message.content == "Final answer 漢字 😀")).await;
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    let final_snapshot = snapshot(&mut observer, &fixture.session).await;
    let assistants = assistant_messages(&final_snapshot);
    assert_eq!(assistants.len(), 2);
    assert_ne!(assistants[0]["id"], assistants[1]["id"]);
    assert_eq!(assistants[1]["content"], "Final answer 漢字 😀");
    assert_eq!(
        run_ids(&observer).len(),
        1,
        "Run identity does not identify an assistant turn"
    );
    assert_eq!(requests.lock().unwrap().len(), 2);
    eprintln!("ordering tool turn: 2 requests, 1 Run, MessageAppended then MessageComplete, 2 distinct canonical assistants");
    drop(observer);
    fixture.finish().await;
}
