//! Actual IPC/HTTP boundaries, not a simulated presentation event feed.
//! These characterize the existing protocol; they do not enable live display.

use super::*;
use std::collections::{HashSet, VecDeque};
use tokio::sync::Notify;

#[path = "run_actions_daemon.rs"]
mod run_actions;

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
        Self::start_with_stall(app, retries, 120).await
    }

    async fn start_with_stall(app: axum::Router, retries: usize, stall_secs: u64) -> Self {
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
        config.provider.stall_notice_secs = stall_secs;
        config.naming.enabled = false;
        config.skills.auto_create = false;
        config.daemon.idle_grace_secs = 3600;
        config.daemon.delete_undo_secs = 0;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_step_choices_restore_revalidate_and_receive_exact_ack() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-step-fixture-key");
    for future in [false, true] {
        for choice in [
            formless_desktop::step_decision::Choice::Continue,
            formless_desktop::step_decision::Choice::Skip,
            formless_desktop::step_decision::Choice::Edit(" {\"native\":\"ไทย\"} ".into()),
            formless_desktop::step_decision::Choice::All,
        ] {
            exercise_step_handoff(future, choice).await;
        }
    }
}

fn step_inventory(update: &Update) -> Option<&formless_desktop::steps::Inventory> {
    match update {
        Update::Snapshot { snapshot, .. } => Some(&snapshot.steps),
        Update::Transcript(batch) => batch
            .baseline
            .as_ref()
            .map(|s| &s.steps)
            .or_else(|| batch.status.as_ref().map(|s| &s.steps)),
        _ => None,
    }
}

async fn exercise_step_handoff(
    future_reader: bool,
    choice: formless_desktop::step_decision::Choice,
) {
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture=Fixture::start(app(axum::routing::post({let requests=requests.clone();move|| {
        let number=requests.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
        async move {
            let gate=Arc::new(Notify::new());gate.notify_one();
            match number {
                1=>gated_response(prefix("Review this missing fixture Tool. "),Some(encode(&[
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"step-call","name":"no_such_step_fixture_tool"}}),
                    json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
                    json!({"type":"content_block_stop","index":1}),
                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}),json!({"type":"message_stop"})
                ])),gate),
                2=>gated_response(prefix("Tool gate finished. "),Some(tail("Done 😀")),gate),
                _=>panic!("unexpected provider replay {number}"),
            }
        }
    }})),0).await;
    let client = if future_reader {
        Client::start_future_attempts_for_validation(fixture.endpoint.clone())
    } else {
        Client::start(fixture.endpoint.clone())
    };
    until(&client, "step-desktop-connect", |update| {
        matches!(update, Update::Connection { ready: true, .. })
    })
    .await;
    client
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 1,
        })
        .unwrap();
    until(&client, "step-subscription-ready", |update| match update {
        Update::Snapshot { generation: 1, .. } => true,
        Update::Transcript(batch) => batch.owner.generation == 1 && batch.baseline.is_some(),
        _ => false,
    })
    .await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    let result = observer
        .request(Request::SetStepMode {
            session_id: fixture.session.clone(),
            on: true,
        })
        .await;
    assert_eq!(result["on"], true);
    send(&mut observer, &fixture.session).await;
    wait(&mut observer, |event| {
        matches!(event, Event::StepPending { .. })
    })
    .await;
    let step = observer
        .events
        .iter()
        .find_map(|event| match event {
            Event::StepPending { step_id, .. } => Some(step_id.clone()),
            _ => None,
        })
        .unwrap();
    until(&client,"step-native-handoff",|update|matches!(update,Update::Notice(text) if text.contains("Step review requested") && text.contains("Formless TUI"))).await;
    let before = snapshot(&mut observer, &fixture.session).await;
    assert_eq!(before["step_mode"], true);
    assert_eq!(before["running"], true);
    assert_eq!(before["pending_steps"]["prompts"][0]["step_id"], step);
    assert_eq!(
        before["pending_steps"]["prompts"][0]["arguments_preview"],
        "{}"
    );
    assert!(before["pending_events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["step_id"].as_str() != Some(step.as_str())));
    drop(client);
    drop(observer);
    let restored = if future_reader {
        Client::start_future_attempts_for_validation(fixture.endpoint.clone())
    } else {
        Client::start(fixture.endpoint.clone())
    };
    restored
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 1,
        })
        .unwrap();
    until(&restored,"step-restored-client-inventory",|update| {
        matches!(step_inventory(&update),Some(formless_desktop::steps::Inventory::Snapshot(s)) if s.prompts.len()==1 && s.prompts[0].step_id==step)
    }).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    let after = snapshot(&mut observer, &fixture.session).await;
    assert_eq!(after["step_mode"], true);
    assert_eq!(after["running"], true);
    assert_eq!(after["pending_steps"]["prompts"][0]["step_id"], step);
    assert!(after["pending_events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|event| event["step_id"].as_str() != Some(step.as_str())));
    assert!(
        !observer
            .events
            .iter()
            .any(|event| matches!(event, Event::StepPending { .. })),
        "Subscribe/GetSession do not replay the missing prompt"
    );
    let prompt: formless_protocol::PendingStep =
        serde_json::from_value(after["pending_steps"]["prompts"][0].clone()).unwrap();
    let action = formless_desktop::step_decision::Action {
        client: restored.instance(),
        connection: restored.connection(),
        generation: 1,
        epoch: restored.epoch(),
        operation: uuid::Uuid::new_v4(),
        prompt,
        choice: choice.clone(),
    };
    restored.decide_step(action.clone()).unwrap();
    assert!(
        restored
            .decide_step(formless_desktop::step_decision::Action {
                operation: uuid::Uuid::new_v4(),
                ..action
            })
            .is_err(),
        "pending choice cannot be resubmitted"
    );
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    future_cases::canonical(&restored, "Tool gate finished. Done 😀", &[]).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if matches!(
            restored.step_status(),
            formless_desktop::step_decision::Status::Finished {
                outcome: formless_desktop::step_decision::Outcome::Accepted,
                ..
            }
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native Step receipt deadline: {:?}",
            restored.step_status()
        );
        restored.drain().for_each(drop);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        snapshot(&mut observer, &fixture.session).await["step_mode"],
        !matches!(choice, formless_desktop::step_decision::Choice::All)
    );
    until(&restored,"step-resolved-client-inventory",|update| {
        matches!(step_inventory(&update),Some(formless_desktop::steps::Inventory::Snapshot(s)) if s.prompts.is_empty() && !s.more)
    }).await;
    assert!(
        snapshot(&mut observer, &fixture.session).await["pending_steps"]["prompts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    drop(restored);
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    drop(observer);
    fixture.finish().await;
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
    exercise_tool_turn(false).await;
    exercise_tool_turn(true).await;
}

async fn exercise_tool_turn(future_reader: bool) {
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
    let future = if future_reader {
        Some(future_cases::reader(&fixture).await)
    } else {
        None
    };
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    send(&mut observer, &fixture.session).await;
    wait(&mut observer, |event| matches!(event, Event::MessageDelta { delta, .. } if delta == "Checking fixture Tool ไทย. ")).await;
    if let Some(client) = &future {
        future_cases::live(client, "Checking fixture Tool ไทย. ").await;
    }
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
    if let Some(client) = future {
        future_cases::canonical(&client, "Final answer 漢字 😀", &[]).await;
        drop(client);
    }
    drop(observer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_question_restores_on_another_session_after_reconnect_and_answers_once() {
    use formless_desktop::questions::Outcome;
    use formless_protocol::QuestionAnswer;
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-question-fixture-key");
    let requests: Arc<Mutex<Vec<Value>>> = Arc::default();
    let fixture=Fixture::start(app(axum::routing::post({let requests=requests.clone();move|axum::Json(body):axum::Json<Value>|{
        let requests=requests.clone();async move {
            let number={let mut requests=requests.lock().unwrap();requests.push(body);requests.len()};
            let text=if number==1 {
                let args=json!({"questions":[{"question":"Choose any fixture options ไทย?","header":"Choices","multi_select":true,"options":[{"label":"Alpha","description":"First option"},{"label":"Beta"},{"label":"Gamma"}]},{"question":"Add context 漢字?"}]});
                prefix("A question follows. ")+&encode(&[
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"question-call","name":"ask_user"}}),
                    json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":args.to_string()}}),
                    json!({"type":"content_block_stop","index":1}),
                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}),json!({"type":"message_stop"}),
                ])
            } else {assert_eq!(number,2,"question response must not be replayed");prefix("Question answered ")+&tail("ไทย 😀")};
            axum::response::Response::builder().header("content-type","text/event-stream").body(axum::body::Body::from(text)).unwrap()
        }
    }})),0).await;
    let other = fixture
        .daemon
        .handle(Request::CreateSession {
            name: Some("Displayed other Session".into()),
        })
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let client = Client::start(fixture.endpoint.clone());
    until(&client, "question-connect", |update| {
        matches!(update, Update::Connection { ready: true, .. })
    })
    .await;
    client
        .send(Command::Select {
            session_id: Some(other.clone()),
            generation: 1,
        })
        .unwrap();
    until(
        &client,
        "question-other-session",
        |update| matches!(update,Update::Snapshot{snapshot,..} if snapshot.session_id==other),
    )
    .await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    send(&mut observer, &fixture.session).await;
    let mut open = None;
    until(&client, "question-global-event", |update| {
        if let Update::Questions(state) = update {
            open = state
                .open
                .into_iter()
                .find(|question| question.session_id == fixture.session);
        }
        open.is_some()
    })
    .await;
    let open = open.unwrap();
    assert_eq!(open.questions.len(), 2);
    assert!(open.questions[0].multi_select);
    let old_connection = client.connection();
    client.send(Command::Reconnect).unwrap();
    let mut restored = None;
    until(&client, "question-restored-global-snapshot", |update| {
        if let Update::Questions(state) = update {
            if state.connection > old_connection {
                restored = state
                    .open
                    .into_iter()
                    .find(|question| question.id == open.id);
            }
        }
        restored.is_some()
    })
    .await;
    assert_eq!(
        restored.as_ref(),
        Some(&open),
        "pending question retains its original Session and full definition"
    );
    let answers = vec![
        QuestionAnswer {
            selected: vec!["Alpha".into(), "Gamma".into()],
            text: Some("Own detail ไทย 😀".into()),
        },
        QuestionAnswer {
            selected: vec![],
            text: Some("Context 漢字".into()),
        },
    ];
    let operation = uuid::Uuid::new_v4();
    client
        .send(Command::AnswerQuestion {
            connection: client.connection(),
            operation,
            question: Box::new(open.clone()),
            answers: answers.clone(),
        })
        .unwrap();
    until(&client,"question-positive-ack",|update|matches!(update,Update::QuestionAnswer{operation:got,outcome:Outcome::Accepted,..} if got==operation)).await;
    wait(&mut observer,|event|matches!(event,Event::MessageComplete{message,..} if message.content=="Question answered ไทย 😀")).await;
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    let repeated = uuid::Uuid::new_v4();
    client
        .send(Command::AnswerQuestion {
            connection: client.connection(),
            operation: repeated,
            question: Box::new(open.clone()),
            answers,
        })
        .unwrap();
    until(&client,"question-duplicate-not-sent",|update|matches!(update,Update::QuestionAnswer{operation:got,outcome:Outcome::Rejected(_),..} if got==repeated)).await;
    let duplicate = observer
        .request(Request::AnswerQuestion {
            question_id: open.id,
            answer: "too late".into(),
            answers: vec![],
        })
        .await;
    assert_eq!(
        duplicate["answered"], false,
        "ok transport response is not a positive answer acknowledgement"
    );
    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let result = requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .find(|part| part["type"] == "tool_result" && part["tool_use_id"] == "question-call")
            .unwrap();
        let result: Value = serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["formless_data"], 1);
        let result: Value = serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["answers"][0]["selected"], json!(["Alpha", "Gamma"]));
        assert_eq!(result["answers"][0]["text"], "Own detail ไทย 😀");
        assert_eq!(result["answers"][1]["text"], "Context 漢字");
    }
    drop(client);
    drop(observer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn candidate_retries_reset_unpersisted_refusal_and_pending_tool_text() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-candidate-ordering-key");
    for pending_tool in [false, true] {
        let complete = Arc::new(Notify::new());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let discarded = if pending_tool {
            "Premature final while private work is running."
        } else {
            "I cannot access files on your computer."
        };
        let fixture = Fixture::start(
            app(axum::routing::post({
                let complete = complete.clone();
                let requests = requests.clone();
                move || {
                    let number = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let complete = complete.clone();
                    async move {
                        match number {
                            1 => {
                                let gate = Arc::new(Notify::new());
                                gate.notify_one();
                                gated_response(prefix(discarded), Some(tail("")), gate)
                            }
                            2 => gated_response(
                                prefix("Accepted candidate ไทย "),
                                Some(tail("😀")),
                                complete,
                            ),
                            _ => panic!("unexpected candidate retry {number}"),
                        }
                    }
                }
            })),
            0,
        )
        .await;
        let tool = if pending_tool {
            let created = fixture.daemon.call_tool_direct(&fixture.session, "create_tool", json!({
                "name":"desktop_candidate_ordering_fixture", "description":"private ordering fixture",
                "input_schema":{"type":"object"},"output_schema":{},"capabilities":[],
                "implementation_js":"async function run(){ await sleep(8000); return {done:true}; }"
            })).await;
            assert!(created.contains("created"), "{created}");
            let daemon = fixture.daemon.clone();
            let session = fixture.session.clone();
            let tool = tokio::spawn(async move {
                daemon
                    .call_tool_direct(&session, "desktop_candidate_ordering_fixture", json!({}))
                    .await
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while fixture
                .daemon
                .handle(Request::ListInvocations)
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
            {
                assert!(Instant::now() < deadline, "private Tool did not start");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Some(tool)
        } else {
            None
        };
        let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
        send(&mut observer, &fixture.session).await;
        wait(&mut observer,|event| matches!(event,Event::MessageDelta{delta,..} if delta=="Accepted candidate ไทย ")).await;
        let before = snapshot(&mut observer, &fixture.session).await;
        assert!(
            assistant_messages(&before).is_empty(),
            "discarded candidate is not persisted"
        );
        let discarded_index = observer
            .events
            .iter()
            .position(|event| matches!(event,Event::MessageDelta{delta,..} if delta==discarded))
            .unwrap();
        let accepted_index=observer.events.iter().position(|event| matches!(event,Event::MessageDelta{delta,..} if delta=="Accepted candidate ไทย ")).unwrap();
        let resets = observer.events[discarded_index + 1..accepted_index]
            .iter()
            .filter(|event| matches!(event, Event::StreamRestarted { .. }))
            .count();
        if let Some(tool) = tool {
            assert!(tokio::time::timeout(Duration::from_secs(15), tool)
                .await
                .unwrap()
                .unwrap()
                .contains("done"));
        }
        complete.notify_one();
        wait(&mut observer, |event| {
            matches!(event, Event::RunFinished { .. })
        })
        .await;
        let final_snapshot = snapshot(&mut observer, &fixture.session).await;
        let messages = assistant_messages(&final_snapshot);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["content"], "Accepted candidate ไทย 😀");
        assert_eq!(run_ids(&observer).len(), 1);
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        drop(observer);
        fixture.finish().await;
        assert_eq!(
            resets, 1,
            "unpersisted candidate must reset before the next attempt: pending_tool={pending_tool}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_window_retry_resets_the_unpersisted_candidate_after_compaction() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-context-ordering-key");
    exercise_context_compaction(false).await;
    exercise_context_compaction(true).await;
}

async fn exercise_context_compaction(future_reader: bool) {
    let complete = Arc::new(Notify::new());
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture=Fixture::start(app(axum::routing::post({
        let complete=complete.clone();let requests=requests.clone();
        move |axum::Json(body):axum::Json<Value>| {
            let complete=complete.clone();
            let number=requests.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
            async move {
                let gate=Arc::new(Notify::new());gate.notify_one();
                match number {
                    1|2=>gated_response(prefix("Private seed history."),Some(tail("")),gate),
                    3=>gated_response(prefix("Discard context-limited candidate."),Some(tail("").replace("end_turn","model_context_window_exceeded")),gate),
                    4=>if body["stream"]==true {
                        gated_response(prefix("Compacted private history."),Some(tail("")),gate)
                    } else { axum::response::IntoResponse::into_response(axum::Json(json!({
                        "id":"private-summary","type":"message","role":"assistant",
                        "content":[{"type":"text","text":"Compacted private history."}],
                        "stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":10}
                    }))) },
                    5=>gated_response(prefix("After compaction ไทย "),Some(tail("😀")),complete),
                    _=>panic!("unexpected context retry request {number}"),
                }
            }
        }
    })),0).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    // ContextCache preserves short histories; two completed turns plus the
    // next user input supply a real whole-turn compaction cut.
    for _ in 0..2 {
        send(&mut observer, &fixture.session).await;
        observer
            .until(|event| matches!(event, Event::RunFinished { .. }))
            .await;
        observer.events.clear();
    }
    let future = if future_reader {
        Some(future_cases::reader(&fixture).await)
    } else {
        None
    };
    send(&mut observer, &fixture.session).await;
    observer
        .until(|event| {
            matches!(event,Event::MessageDelta{delta,..} if delta=="After compaction ไทย ")
                || matches!(event, Event::RunFinished { .. })
        })
        .await;
    assert!(
        observer.events.iter().any(
            |event| matches!(event,Event::MessageDelta{delta,..} if delta=="After compaction ไทย ")
        ),
        "context retry ended without next attempt: {:?}",
        observer.events
    );
    let discarded=observer.events.iter().position(|event| matches!(event,Event::MessageDelta{delta,..} if delta=="Discard context-limited candidate.")).unwrap();
    let accepted = observer
        .events
        .iter()
        .position(
            |event| matches!(event,Event::MessageDelta{delta,..} if delta=="After compaction ไทย "),
        )
        .unwrap();
    let resets = observer.events[discarded + 1..accepted]
        .iter()
        .filter(|event| matches!(event, Event::StreamRestarted { .. }))
        .count();
    let before = snapshot(&mut observer, &fixture.session).await;
    assert_eq!(
        assistant_messages(&before).len(),
        2,
        "only the seed turns are persisted while retry is held"
    );
    complete.notify_one();
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    let final_snapshot = snapshot(&mut observer, &fixture.session).await;
    let messages = assistant_messages(&final_snapshot);
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["content"], "After compaction ไทย 😀");
    assert_eq!(run_ids(&observer).len(), 1);
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 5);
    if let Some(client) = future {
        future_cases::canonical(
            &client,
            "After compaction ไทย 😀",
            &["Compacted private history."],
        )
        .await;
        drop(client);
    }
    drop(observer);
    fixture.finish().await;
    assert_eq!(
        resets, 1,
        "compaction retry must retire its unpersisted candidate"
    );
}

#[path = "future_attempts.rs"]
mod future_attempts;

#[path = "future_cases.rs"]
mod future_cases;

#[path = "future_faults.rs"]
mod future_faults;
