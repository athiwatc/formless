//! Real daemon and HTTP provider seam in a private profile, with no GUI or
//! external credentials/provider. Runs on native macOS, Linux and Windows.
#![cfg(feature = "daemon-tests")]

#[path = "../../formless-core/tests/support/env.rs"]
mod credential_env;

use formless_core::{Config, Paths};
use formless_desktop::client::{Client, Command, Endpoint, Update};
use formless_desktop::model::{Destination, Intent};
use formless_protocol::{
    Event, Request, RequestEnvelope, SendMode, ServerMessage, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[path = "support/stream_ordering.rs"]
mod stream_ordering;

#[cfg(unix)]
type LocalStream = tokio::net::UnixStream;
#[cfg(windows)]
type LocalStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// A second real IPC subscriber characterizes deltas independently of the
/// desktop's canonical-only transport. No presentation seam supplies events.
struct Observer {
    socket: BufReader<LocalStream>,
    id: u64,
    events: Vec<Event>,
}

impl Observer {
    async fn connect(endpoint: &Endpoint, session: &str) -> Self {
        let stream = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                #[cfg(unix)]
                let opened = LocalStream::connect(&endpoint.0).await;
                #[cfg(windows)]
                let opened =
                    tokio::net::windows::named_pipe::ClientOptions::new().open(&endpoint.0);
                match opened {
                    Ok(stream) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            || error.raw_os_error() == Some(231) =>
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("observer connection: {error}"),
                }
            }
        })
        .await
        .expect("observer connection deadline");
        let mut observer = Self {
            socket: BufReader::new(stream),
            id: 0,
            events: Vec::new(),
        };
        observer
            .request(Request::Subscribe {
                sessions: Some(vec![session.into()]),
                deltas: true,
                client: Some("private stream characterization".into()),
            })
            .await;
        observer
    }

    async fn next(&mut self) -> ServerMessage {
        let mut line = String::new();
        let count = tokio::time::timeout(Duration::from_secs(10), self.socket.read_line(&mut line))
            .await
            .expect("observer frame deadline")
            .unwrap();
        assert_ne!(count, 0, "observer IPC ended unexpectedly");
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, request: Request) -> Value {
        self.id += 1;
        let mut line = serde_json::to_vec(&RequestEnvelope {
            id: self.id,
            protocol: PROTOCOL_VERSION,
            request,
        })
        .unwrap();
        line.push(b'\n');
        self.socket.get_mut().write_all(&line).await.unwrap();
        loop {
            match self.next().await {
                ServerMessage::Event(event) => self.events.push(event),
                ServerMessage::Response {
                    id,
                    ok,
                    result,
                    error,
                } => {
                    assert_eq!(id, self.id);
                    assert!(ok, "observer request failed: {error:?}");
                    return result.unwrap();
                }
            }
        }
    }

    async fn until(&mut self, mut predicate: impl FnMut(&Event) -> bool) {
        loop {
            let ServerMessage::Event(event) = self.next().await else {
                panic!("unsolicited observer response");
            };
            let found = predicate(&event);
            self.events.push(event);
            if found {
                break;
            }
        }
    }
}

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
                Update::Questions(state) => format!(
                    "profile questions={} blocked={}",
                    state.open.len(),
                    state.blocked.len()
                ),
                Update::QuestionAnswer {
                    question_id,
                    outcome,
                    ..
                } => format!("answer {question_id}: {outcome:?}"),
                Update::Prepared(prepared) => format!(
                    "forecast {} confirmation={}",
                    prepared.label, prepared.quote.confirmation_required
                ),
                Update::PreparationReady { .. } => "forecast consent acknowledged".into(),
                Update::PreparationFailed { error, .. } => format!("forecast error {error}"),
            };
            eprintln!("{stage}: {summary}");
            // This headless fixture has no editable GUI state. It commits a
            // current quote only when no consent is required, or after the
            // test explicitly confirmed it. It never manufactures consent.
            match &update {
                Update::Prepared(prepared)
                    if !prepared.quote.confirmation_required
                        && prepared.epoch == client.epoch() =>
                {
                    client.send(Command::CommitPrepared(prepared.id)).unwrap();
                }
                Update::PreparationReady { id, epoch } if *epoch == client.epoch() => {
                    client.send(Command::CommitPrepared(*id)).unwrap();
                }
                _ => {}
            }
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
    let first_answer = Arc::new(tokio::sync::Notify::new());
    let finish_answer = Arc::new(tokio::sync::Notify::new());
    let interrupt_answers = Arc::new(tokio::sync::Notify::new());
    let app = axum::Router::new()
        .route("/models", axum::routing::get(|| async { axum::Json(json!({"data":[{"id":"desktop-test-model"}]})) }))
        .route("/messages", axum::routing::post({
            let requests = requests.clone();
            let first_answer = first_answer.clone();
            let finish_answer = finish_answer.clone();
            let interrupt_answers = interrupt_answers.clone();
            move |axum::Json(body): axum::Json<Value>| {
                let requests = requests.clone();
                let first_answer = first_answer.clone();
                let finish_answer = finish_answer.clone();
                let interrupt_answers = interrupt_answers.clone();
                async move {
                    let number = { let mut requests = requests.lock().unwrap(); requests.push(body); requests.len() };
                    let first = number == 1;
                    if first { first_answer.notified().await; }
                    else if matches!(number, 2 | 4) { interrupt_answers.notified().await; }
                    else { tokio::time::sleep(Duration::from_millis(300)).await; }
                    let events = [
                        json!({"type":"message_start","message":{"usage":{"input_tokens":2000}}}),
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Canonical provider reply "}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ไทย 😀"}}),
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":100}}),
                        json!({"type":"message_stop"}),
                    ];
                    let encode = |events: &[Value]| -> String { events.iter().map(|event| format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())).collect() };
                    let chunks = std::collections::VecDeque::from([encode(&events[..3]), encode(&events[3..])]);
                    let body = futures_util::stream::unfold((chunks, finish_answer), move |(mut chunks, gate)| async move {
                        let chunk = chunks.pop_front()?;
                        if first && chunks.is_empty() { gate.notified().await; }
                        Some((Ok::<_, std::io::Error>(chunk), (chunks, gate)))
                    });
                    axum::response::Response::builder().header("content-type", "text/event-stream").body(axum::body::Body::from_stream(body)).unwrap()
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
    config.provider.prices.insert(
        "desktop-test-model".into(),
        formless_core::config::ModelPrice {
            input: 1.,
            output: 2.,
            ..Default::default()
        },
    );
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
    let mut observer = Observer::connect(&endpoint, &a).await;
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
        .review(
            Intent::SendMessage {
                session_id: a.clone(),
                content: "Desktop real transport request".into(),
                mode: SendMode::Send,
            },
            "Desktop real transport request".into(),
        )
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
    assert_eq!(
        daemon
            .handle(Request::GetSession { id: a.clone() })
            .await
            .unwrap()["running"],
        true,
        "the provider is still blocked during navigation"
    );
    first_answer.notify_one();
    observer
        .until(|event| {
            matches!(event, Event::MessageDelta { session_id, delta, .. }
        if session_id == &a && delta == "Canonical provider reply ")
        })
        .await;
    let during = observer
        .request(Request::GetSession { id: a.clone() })
        .await;
    assert_eq!(during["running"], true);
    assert!(
        !during["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "assistant"),
        "in-flight provider deltas are not part of a canonical snapshot"
    );
    finish_answer.notify_one();
    observer
        .until(|event| {
            matches!(event, Event::MessageComplete { session_id, message, .. }
        if session_id == &a && message.content == "Canonical provider reply ไทย 😀")
        })
        .await;
    let live: String = observer
        .events
        .iter()
        .filter_map(|event| match event {
            Event::MessageDelta {
                session_id, delta, ..
            } if session_id == &a => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(live, "Canonical provider reply ไทย 😀");
    let completed = observer
        .request(Request::GetSession { id: a.clone() })
        .await;
    assert_eq!(
        completed["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "assistant" && message["content"] == live)
            .count(),
        1
    );
    drop(observer);
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
    for abandon in [false, true] {
        let request_count = if abandon { 4 } else { 2 };
        client
            .review(
                Intent::SendMessage {
                    session_id: a.clone(),
                    content: "interrupt this private request".into(),
                    mode: SendMode::Send,
                },
                "interrupt this private request".into(),
            )
            .unwrap();
        until(&client, "recovery-send-ack", |update| {
            matches!(update, Update::Action { result: Ok(()), .. })
        })
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while requests.lock().unwrap().len() < request_count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("provider request did not reach its interruption gate");
        client
            .send(Command::Intent(Intent::InterruptRun {
                session_id: a.clone(),
            }))
            .unwrap();
        let mut stop_acknowledged = false;
        let mut interrupted = false;
        until(&client, "recovery-interrupted", |update| {
            match update {
                Update::Action { result: Ok(()), .. } => stop_acknowledged = true,
                Update::Snapshot { snapshot, .. } => {
                    interrupted = !snapshot.running
                        && snapshot
                            .unfinished_run
                            .as_ref()
                            .is_some_and(|run| run.status == "interrupted")
                }
                _ => {}
            }
            stop_acknowledged && interrupted
        })
        .await;
        if abandon {
            client
                .send(Command::Intent(Intent::AbandonRun {
                    session_id: a.clone(),
                }))
                .unwrap();
        } else {
            client
                .review(
                    Intent::ResumeRun {
                        session_id: a.clone(),
                    },
                    "retained recovery draft".into(),
                )
                .unwrap();
        }
        until(&client, "recovery-action-ack", |update| {
            matches!(update, Update::Action { result: Ok(()), .. })
        })
        .await;
        until(&client, "recovery-completed", |update| matches!(update,
            Update::Snapshot { snapshot, .. } if !snapshot.running && snapshot.unfinished_run.is_none()
        )).await;
        assert_eq!(
            requests.lock().unwrap().len(),
            if abandon { 4 } else { 3 },
            "Resume starts one provider request; Abandon starts none"
        );
    }
    interrupt_answers.notify_waiters();
    daemon
        .handle(Request::SetSetting {
            key: "limits.forecast_confirm_usd".into(),
            value: json!(0.0),
        })
        .await
        .unwrap();
    let cost_message = "explicitly reviewed private fixture request";
    client
        .review(
            Intent::SendMessage {
                session_id: a.clone(),
                content: cost_message.into(),
                mode: SendMode::Send,
            },
            cost_message.into(),
        )
        .unwrap();
    let mut retired_quote = None;
    until(
        &client,
        "required-forecast-before-navigation",
        |update| match update {
            Update::Prepared(prepared) => {
                assert!(
                    prepared.quote.confirmation_required,
                    "priced completed fixture Runs must require consent at a zero threshold: {:?}",
                    prepared.quote
                );
                retired_quote = Some(prepared.id);
                true
            }
            _ => false,
        },
    )
    .await;
    assert_eq!(
        requests.lock().unwrap().len(),
        4,
        "forecasting itself cannot call the provider"
    );
    client
        .send(Command::Select {
            session_id: Some(b.clone()),
            generation: 4,
        })
        .unwrap();
    until(&client, "required-forecast-switch", |update| {
        matches!(update, Update::Snapshot { generation: 4, .. })
    })
    .await;
    client
        .send(Command::ConfirmPrepared(retired_quote.unwrap()))
        .unwrap();
    client
        .send(Command::CommitPrepared(retired_quote.unwrap()))
        .unwrap();
    client
        .send(Command::Select {
            session_id: Some(a.clone()),
            generation: 5,
        })
        .unwrap();
    until(&client, "required-forecast-return", |update| {
        matches!(update, Update::Snapshot { generation: 5, .. })
    })
    .await;
    assert_eq!(
        requests.lock().unwrap().len(),
        4,
        "retired quote identities cannot start work"
    );
    client
        .review(
            Intent::SendMessage {
                session_id: a.clone(),
                content: cost_message.into(),
                mode: SendMode::Send,
            },
            cost_message.into(),
        )
        .unwrap();
    let mut current_quote = None;
    until(
        &client,
        "required-forecast-current",
        |update| match update {
            Update::Prepared(prepared) => {
                assert!(prepared.quote.confirmation_required);
                current_quote = Some(prepared.id);
                true
            }
            _ => false,
        },
    )
    .await;
    assert_eq!(requests.lock().unwrap().len(), 4);
    // Explicit fixture-user confirmation, on the same real connection. The
    // helper only commits after the daemon's definite confirmation ACK.
    client
        .send(Command::ConfirmPrepared(current_quote.unwrap()))
        .unwrap();
    until(&client, "required-forecast-send-ack", |update| {
        matches!(update, Update::Action { result: Ok(()), .. })
    })
    .await;
    until(&client, "required-forecast-completed", |update| matches!(update, Update::Snapshot { generation: 5, snapshot } if !snapshot.running && snapshot.blocks.iter().filter(|block| block.markdown() == "Canonical provider reply ไทย 😀").count() == 3)).await;
    assert_eq!(
        requests.lock().unwrap().len(),
        5,
        "one confirmed action starts one provider request"
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
    until(&client, "stage-10", |u| matches!(u, Update::Snapshot { generation: 5, snapshot } if snapshot.blocks.iter().filter(|block| block.markdown() == "Canonical provider reply ไทย 😀").count() == 3)).await;
    assert_eq!(
        requests.lock().unwrap().len(),
        5,
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
    let retired = Arc::downgrade(&daemon2);
    drop(daemon2);
    tokio::time::timeout(Duration::from_secs(10), async {
        while retired.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fixture tasks retained the Profile lease during cleanup");
    // Keep private fixture evidence on panic; remove only this test's tree.
    std::fs::remove_dir_all(root).unwrap();
}
