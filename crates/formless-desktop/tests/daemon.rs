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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_client_displays_provisional_answer_and_reasoning_before_persistence() {
    stream_ordering::production_live_stream().await;
}

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
                Update::Transcript(batch) => format!(
                    "live reader generation {} / reload={}",
                    batch.owner.generation, batch.reload
                ),
                Update::Invocations { result, .. } => {
                    format!("Invocation snapshot: ok={}", result.is_ok())
                }
                Update::RunInspection { target, result, .. } => format!(
                    "Run inspection {} / {}: ok={}",
                    target.run_id,
                    target.session_id,
                    result.is_ok()
                ),
                Update::SessionComparison { pair, result, .. } => format!(
                    "comparison {} / {}: ok={}",
                    pair.left,
                    pair.right,
                    result.is_ok()
                ),
                Update::SessionUsage { target, result, .. } => format!(
                    "recorded usage {}: ok={}",
                    target.session_id,
                    result.is_ok()
                ),
                Update::SessionExport {
                    session_id,
                    format,
                    result,
                    ..
                } => format!(
                    "export {session_id} {}: ok={}",
                    format.wire(),
                    result.is_ok()
                ),
                Update::ProfileSearch { query, result, .. } => {
                    format!("profile search {query}: ok={}", result.is_ok())
                }
                Update::Connection { ready, notice } => format!("connection {ready}: {notice}"),
                Update::LiveOverlay {
                    session_id, view, ..
                } => format!(
                    "provisional {session_id}: answer={} reasoning={} unavailable={}",
                    view.answer.len(),
                    view.reasoning.len(),
                    view.unavailable
                ),
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
                Update::SessionStatusInvalidated { session_id, .. } => {
                    format!("status invalidated {session_id}")
                }
                Update::SnapshotError { error, .. } => format!("snapshot error {error}"),
                Update::Sessions(sessions) => format!("Sessions={}", sessions.len()),
                Update::Page {
                    destination,
                    result,
                } => format!("page {destination:?} ok={}", result.is_ok()),
                Update::Action { result, .. } => format!("action {result:?}"),
                Update::Notice(notice) => notice.clone(),
                Update::ApprovalInventory {
                    result, revision, ..
                } => format!("approval inventory {revision}: ok={}", result.is_ok()),
                Update::ApprovalInvalidated { revision, .. } => {
                    format!("approval invalidated {revision}")
                }
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
                Update::SessionEdited {
                    session_id,
                    outcome,
                    ..
                } => format!("metadata {session_id}: {outcome:?}"),
                Update::PendingAttachments { result, .. } => {
                    format!("pending files: ok={}", result.is_ok())
                }
                Update::ModelCatalog { result, .. } => {
                    format!("catalog read: ok={}", result.is_ok())
                }
                Update::SessionDetails {
                    session_id, result, ..
                } => format!("metadata read {session_id}: ok={}", result.is_ok()),
                Update::Prepared(prepared) => format!(
                    "forecast {} confirmation={}",
                    prepared.label, prepared.quote.confirmation_required
                ),
                Update::PreparationReady { .. } => "forecast consent acknowledged".into(),
                Update::PreparationFailed { error, .. } => format!("forecast error {error}"),
            };
            eprintln!("{stage}: {summary}");
            // This headless fixture has no editable GUI state. It commits a
            // current quote only after the test caller explicitly confirms
            // pending metadata, or after separately required cost consent.
            // It never manufactures a required cost confirmation.
            match &update {
                Update::Prepared(prepared)
                    if !prepared.needs_review() && prepared.epoch == client.epoch() =>
                {
                    client.send(Command::CommitPrepared(prepared.id)).unwrap();
                }
                Update::Prepared(prepared)
                    if prepared.attachments.is_some()
                        && !prepared.quote.confirmation_required
                        && prepared.epoch == client.epoch() =>
                {
                    client.send(Command::ConfirmPrepared(prepared.id)).unwrap();
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
    // Metadata changes are separate, acknowledged setters for a frozen
    // target, independent of the open transcript and provider execution.
    let metadata_read = uuid::Uuid::new_v4();
    client
        .send(Command::ReadSessionDetails {
            connection: client.connection(),
            operation: metadata_read,
            session_id: b.clone(),
        })
        .unwrap();
    until(&client,"metadata-read",|u|matches!(u,Update::SessionDetails {operation,result:Ok(metadata),..} if operation==metadata_read && metadata.id==b)).await;
    for change in [
        formless_desktop::session_edit::Change::Name("Desktop renamed ไทย 😀".into()),
        formless_desktop::session_edit::Change::Tags(vec![
            "#Work".into(),
            "work".into(),
            "ไทย 漢字 😀".into(),
        ]),
        formless_desktop::session_edit::Change::Pin(true),
    ] {
        let operation = uuid::Uuid::new_v4();
        let expected = change.clone();
        client
            .send(Command::EditSession {
                connection: client.connection(),
                operation,
                session_id: b.clone(),
                change,
            })
            .unwrap();
        until(&client,"metadata-save",|u|matches!(u,Update::SessionEdited {operation:ack,session_id,outcome:formless_desktop::session_edit::Outcome::Applied(metadata),..} if ack==operation && session_id==b && expected.matches(&metadata))).await;
    }
    let metadata = daemon
        .handle(Request::GetSession { id: b.clone() })
        .await
        .unwrap();
    assert_eq!(metadata["session"]["name"], "Desktop renamed ไทย 😀");
    assert_eq!(metadata["session"]["tags"], json!(["work", "ไทย 漢字 😀"]));
    assert_eq!(metadata["session"]["pinned"], true);
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
    for archived in [true, false] {
        let operation = uuid::Uuid::new_v4();
        client
            .send(Command::EditSession {
                connection: client.connection(),
                operation,
                session_id: a.clone(),
                change: formless_desktop::session_edit::Change::Archive(archived),
            })
            .unwrap();
        until(&client,"archive-running-session",|update|matches!(update,Update::SessionEdited {operation:ack,session_id,outcome:formless_desktop::session_edit::Outcome::Applied(metadata),..} if ack==operation && session_id==a && metadata.archived==archived)).await;
        let current = daemon
            .handle(Request::GetSession { id: a.clone() })
            .await
            .unwrap();
        assert_eq!(current["session"]["archived"], archived);
        assert_eq!(
            current["running"], true,
            "archiving/restoring is not a Run control"
        );
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        while requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    for instructions in [
        Some("  DESKTOP_STANDING_INSTRUCTIONS_1753\nไทย 😀  ".to_string()),
        None,
        Some("DESKTOP_STANDING_INSTRUCTIONS_1753\n\tไทย 漢字 😀".to_string()),
    ] {
        let operation = uuid::Uuid::new_v4();
        let change = formless_desktop::session_edit::Change::Instructions(instructions.clone());
        let expected = change.clone();
        client
            .send(Command::EditSession {
                connection: client.connection(),
                operation,
                session_id: a.clone(),
                change,
            })
            .unwrap();
        until(&client,"instructions-running-session",|update|matches!(update,Update::SessionEdited{operation:ack,session_id,outcome:formless_desktop::session_edit::Outcome::Applied(metadata),..} if ack==operation && session_id==a && expected.matches(&metadata))).await;
        let current = daemon
            .handle(Request::GetSession { id: a.clone() })
            .await
            .unwrap();
        assert_eq!(
            current["session"]["instructions"],
            json!(instructions
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty()))
        );
        assert_eq!(
            current["running"], true,
            "editing instructions does not stop the held Run"
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "instruction setters must not call the provider"
        );
    }
    // The connected client exports A while B is selected and A's provider
    // response is held. Export must read persisted data without Run control.
    for format in [
        formless_desktop::session_export::Format::Json,
        formless_desktop::session_export::Format::Markdown,
        formless_desktop::session_export::Format::Html,
    ] {
        let expected = formless_desktop::session_export::decode(
            format,
            daemon
                .handle(Request::ExportSession {
                    id: a.clone(),
                    format: Some(format.wire().into()),
                })
                .await
                .unwrap(),
        )
        .unwrap()
        .text;
        assert!(
            expected.contains("Desktop real transport request"),
            "persisted user text is included"
        );
        assert!(
            !expected.contains("Canonical provider reply"),
            "held provider output is not yet persisted"
        );
        let operation = uuid::Uuid::new_v4();
        client
            .send(Command::ExportSession {
                connection: client.connection(),
                operation,
                session_id: a.clone(),
                format,
            })
            .unwrap();
        until(&client,"export-running-session",|update|matches!(update,Update::SessionExport{operation:ack,session_id,format:actual,result:Ok(document),..} if ack==operation&&session_id==a&&actual==format&&document.text==expected)).await;
        assert_eq!(
            daemon
                .handle(Request::GetSession { id: a.clone() })
                .await
                .unwrap()["running"],
            true
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            1,
            "export must not call the provider"
        );
    }
    // A comparison is a persisted read even while one Run is in flight.
    let pair = formless_desktop::comparison::Pair {
        left: a.clone(),
        right: b.clone(),
    };
    let before = daemon
        .handle(Request::GetSession { id: a.clone() })
        .await
        .unwrap();
    let expected = formless_desktop::comparison::Report::decode(
        &pair,
        daemon
            .handle(Request::CompareSessions {
                left: a.clone(),
                right: b.clone(),
            })
            .await
            .unwrap(),
    )
    .unwrap()
    .page
    .rows[0]
        .detail
        .clone();
    let operation = uuid::Uuid::new_v4();
    client
        .send(Command::CompareSessions {
            connection: client.connection(),
            operation,
            pair: pair.clone(),
        })
        .unwrap();
    until(&client,"compare-running-session",|update|matches!(update,Update::SessionComparison{operation:ack,pair:actual,result:Ok(report),..} if ack==operation&&actual==pair&&report.page.rows[0].detail==expected)).await;
    assert_eq!(
        daemon
            .handle(Request::GetSession { id: a.clone() })
            .await
            .unwrap(),
        before,
        "comparison does not mutate the persisted Session or running state"
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "comparison never calls a provider"
    );
    let metrics = daemon
        .handle(Request::RunMetrics {
            limit: Some(100),
            session_id: Some(a.clone()),
            since: None,
        })
        .await
        .unwrap();
    let target = formless_desktop::run_inspection::Target::from_metric(&metrics["runs"][0])
        .expect("a recorded Run for A");
    let raw = daemon
        .handle(Request::GetRun {
            run_id: target.run_id.clone(),
        })
        .await
        .unwrap();
    let expected = formless_desktop::run_inspection::Report::decode(&target, raw.clone())
        .unwrap()
        .page
        .rows[0]
        .detail
        .clone();
    let operation = uuid::Uuid::new_v4();
    client
        .send(Command::InspectRun {
            connection: client.connection(),
            operation,
            target: target.clone(),
        })
        .unwrap();
    until(&client,"inspect-running-run",|update|matches!(update,Update::RunInspection{operation:ack,target:actual,result:Ok(report),..} if ack==operation&&actual==target&&report.page.rows[0].detail==expected)).await;
    assert_eq!(
        daemon
            .handle(Request::GetRun {
                run_id: target.run_id.clone()
            })
            .await
            .unwrap(),
        raw,
        "inspection does not mutate the stored Run"
    );
    assert_eq!(
        daemon
            .handle(Request::GetSession { id: a.clone() })
            .await
            .unwrap(),
        before,
        "inspection does not mutate the Session or running state"
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "inspection must not call a provider"
    );
    // One explicit durable fork, with A's Provider response held. The shared
    // receipt is independent of the bounded UI update queue.
    let source_before = daemon
        .handle(Request::GetSession { id: a.clone() })
        .await
        .unwrap();
    let fork_action = formless_desktop::session_fork::Action {
        connection: client.connection(),
        operation: uuid::Uuid::new_v4(),
        source_id: a.clone(),
    };
    client
        .send(Command::ForkSession(fork_action.clone()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let receipt = loop {
        match client.fork_status() {
            formless_desktop::session_fork::Status::Finished {
                action,
                outcome: formless_desktop::session_fork::Outcome::Applied(receipt),
            } if action == fork_action => break receipt,
            formless_desktop::session_fork::Status::Finished { outcome, .. } => {
                panic!("fork receipt failed: {outcome:?}")
            }
            _ => {}
        }
        assert!(Instant::now() < deadline, "fork receipt deadline");
        for _ in client.drain() {}
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let child = daemon
        .handle(Request::GetSession {
            id: receipt.child_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(child["session"]["forked_from"], a);
    assert_eq!(
        child["session"]["fork_point_seq"].as_u64(),
        Some(receipt.fork_point_seq)
    );
    assert_eq!(child["running"], false);
    assert!(child["unfinished_run"].is_null());
    assert_eq!(child["queued_items"], json!([]));
    let transcript = |value: &Value| {
        value["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| {
                (
                    message["seq"].clone(),
                    message["role"].clone(),
                    message["content"].clone(),
                    message["meta"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        transcript(&child),
        transcript(&source_before),
        "fork copies persisted messages exactly, with independent message identities"
    );
    assert_eq!(
        daemon
            .handle(Request::GetSession { id: a.clone() })
            .await
            .unwrap(),
        source_before,
        "fork leaves its source and Run untouched"
    );
    assert_eq!(
        daemon
            .handle(Request::RunMetrics {
                limit: Some(100),
                session_id: Some(receipt.child_id.clone()),
                since: None
            })
            .await
            .unwrap()["runs"],
        json!([]),
        "fork starts no Run"
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "fork never calls a provider"
    );
    // A safe private Tool sleeps without capabilities or Provider access.
    // The native client only observes it; it issues no Tool control request.
    let created=daemon.call_tool_direct(&b,"create_tool",json!({"name":"desktop_invocation_fixture","description":"private snapshot fixture","input_schema":{"type":"object"},"output_schema":{},"implementation_js":"async function run(){ progress(0.5, 'private fixture running'); await sleep(8000); return {done:true}; }","capabilities":[]})).await;
    assert!(created.contains("created"), "{created}");
    // Start the timed Tool only after a fresh subscription. If the client
    // is already in backoff, first wait for a live generation before
    // requesting a reconnect, which an offline worker would discard.
    let mut retired = None;
    let mut invocation_connection = None;
    until(&client, "invocations-reconnect", |update| {
        let connection = client.connection();
        if !client.is_ready() || client.connection() != connection {
            return false;
        }
        let Some(previous) = retired else {
            client.send(Command::Reconnect).unwrap();
            retired = Some(connection);
            return false;
        };
        if matches!(update, Update::Connection { ready: true, .. }) && connection > previous {
            invocation_connection = Some(connection);
            true
        } else {
            false
        }
    })
    .await;
    let invocation_connection = invocation_connection.unwrap();
    let mut tool_call = tokio::spawn({
        let daemon = daemon.clone();
        let session = b.clone();
        async move {
            daemon
                .call_tool_direct(&session, "desktop_invocation_fixture", json!({}))
                .await
        }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let expected = loop {
        // Keep consuming canonical updates while polling the private daemon
        // directly, just as the desktop does while the Tool is running.
        for _ in client.drain() {}
        let raw = daemon.handle(Request::ListInvocations).await.unwrap();
        let report = formless_desktop::invocations::Report::decode(raw.clone()).unwrap();
        if let Some(key) = report
            .keys
            .iter()
            .find(|key| key.tool_name == "desktop_invocation_fixture")
        {
            if raw.as_array().unwrap().iter().any(|record| {
                record["handle"].as_str() == Some(key.handle.as_str())
                    && record["last_fraction"] == 0.5
            }) {
                break key.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "private Invocation did not expose progress"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        client.is_ready(),
        "Invocation fixture connection is offline"
    );
    assert_eq!(client.connection(), invocation_connection);
    let operation = uuid::Uuid::new_v4();
    client
        .send(Command::ReadInvocations {
            connection: invocation_connection,
            operation,
        })
        .unwrap();
    until(&client, "invocations-active", |update| match update {
        Update::Invocations {
            connection,
            operation: ack,
            result,
        } if ack == operation => {
            assert_eq!(connection, invocation_connection);
            let report = result.expect("active Invocation read must succeed");
            let index = report
                .keys
                .iter()
                .position(|key| key == &expected)
                .expect("the exact active Invocation is present");
            let record: Value = serde_json::from_str(&report.page.rows[index + 1].detail).unwrap();
            assert_eq!(record["session_id"], b);
            assert_eq!(record["last_fraction"], 0.5);
            assert_eq!(record["last_label"], "private fixture running");
            assert_eq!(record["abandoned"], false);
            true
        }
        _ => false,
    })
    .await;
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "Invocation reads never call a Provider"
    );
    let completed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            tokio::select! {
                result = &mut tool_call => break result.unwrap(),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    for _ in client.drain() {}
                }
            }
        }
    })
    .await
    .expect("private Invocation completion deadline");
    assert!(completed.contains("done"), "{completed}");
    assert_eq!(
        daemon.handle(Request::ListInvocations).await.unwrap(),
        json!([])
    );
    let operation = uuid::Uuid::new_v4();
    assert!(
        client.is_ready(),
        "Invocation fixture connection is offline"
    );
    assert_eq!(client.connection(), invocation_connection);
    client
        .send(Command::ReadInvocations {
            connection: invocation_connection,
            operation,
        })
        .unwrap();
    until(&client, "invocations-empty", |update| match update {
        Update::Invocations {
            connection,
            operation: ack,
            result,
        } if ack == operation => {
            assert_eq!(connection, invocation_connection);
            let report = result.expect("empty Invocation read must succeed");
            assert!(report.keys.is_empty());
            assert_eq!(report.page.rows[0].detail, "[]");
            true
        }
        _ => false,
    })
    .await;
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "normal Tool completion needs no Provider call or native cancellation"
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
    let recorded = daemon
        .handle(Request::SessionUsage {
            session_id: a.clone(),
        })
        .await
        .unwrap();
    assert_eq!(recorded["total"]["prompt_tokens"], 2000);
    assert_eq!(recorded["total"]["completion_tokens"], 100);
    assert!((recorded["total"]["cost"].as_f64().unwrap() - 0.0022).abs() < 1e-10);
    let expected = formless_desktop::session_usage::Report::decode(recorded)
        .unwrap()
        .copy();
    let usage_operation = uuid::Uuid::new_v4();
    let target = formless_desktop::session_usage::Target {
        session_id: a.clone(),
        connection: client.connection(),
        generation: 3,
    };
    client
        .send(Command::ReadSessionUsage {
            target: target.clone(),
            operation: usage_operation,
        })
        .unwrap();
    until(&client,"recorded-session-usage",|update|matches!(update,Update::SessionUsage{target:actual,operation,result:Ok(report)} if actual==target&&operation==usage_operation&&report.copy()==expected)).await;
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "recorded usage must not contact the Provider"
    );
    // Search is a separate, Profile-wide read: it includes archived stored
    // messages and never starts another provider request.
    daemon
        .handle(Request::SetSessionArchived {
            id: a.clone(),
            archived: true,
        })
        .await
        .unwrap();
    let operation = uuid::Uuid::new_v4();
    client
        .send(Command::SearchProfile {
            connection: client.connection(),
            operation,
            query: "Canonical provider".into(),
        })
        .unwrap();
    until(&client,"profile-search-archived",|update|matches!(update,Update::ProfileSearch{operation:ack,result:Ok(hits),..} if ack==operation && hits.iter().any(|hit|hit.session_id==a && hit.role=="assistant" && hit.snippet.contains("Canonical provider reply")))).await;
    assert_eq!(requests.lock().unwrap().len(), 1);
    daemon
        .handle(Request::SetSessionArchived {
            id: a.clone(),
            archived: false,
        })
        .await
        .unwrap();
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
        if !abandon {
            assert!(
                requests.lock().unwrap()[1]["system"]
                    .to_string()
                    .contains("DESKTOP_STANDING_INSTRUCTIONS_1753"),
                "the next Run must receive the saved standing instructions"
            );
        }
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
        |u| matches!(u, Update::Sessions(sessions) if sessions.len() == 4 && [&a,&b,&receipt.child_id].iter().all(|id|sessions.iter().any(|session|&session.id==*id))),
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
    // Arc's strong count reaches zero before its destructor finishes dropping
    // fields. The private old daemon's Profile lease can therefore still be
    // releasing here. Retry only that exact lock error for this owned fixture;
    // other errors fail immediately and no other process is stopped.
    let daemon2 = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match formless_core::daemon::Daemon::new(paths.clone()) {
                Ok(daemon) => break Arc::new(daemon),
                Err(error) if error.to_string() == "profile is owned by a daemon or maintenance operation; stop it and try again" => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(error) => panic!("private restored daemon: {error:#}"),
            }
        }
    }).await.expect("old private fixture Profile lease did not retire");
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

#[cfg(unix)]
#[path = "support/native_step_transport.rs"]
mod native_step_transport;

#[path = "support/status_fence.rs"]
mod status_fence;

#[path = "support/cursor_order.rs"]
mod cursor_order;

#[cfg(unix)]
#[path = "support/step_mode_transport.rs"]
mod step_mode_transport;

#[cfg(unix)]
#[path = "support/native_approval_transport.rs"]
mod native_approval_transport;

#[cfg(unix)]
#[path = "support/session_model_transport.rs"]
mod session_model_transport;
