//! Actual provider/IPC acceptance for steering, approval interruption and
//! answer/cancel ordering. Every wait is tied to an observed event or barrier.
#![cfg(unix)]
#[path = "support/env.rs"]
mod credential_env;

use axum::{
    routing::{get, post},
    Json, Router,
};
use formless_core::{config::HookCfg, daemon::Daemon, Config, Paths, Store};
use formless_protocol::{
    ApprovalScope, Event, Request, RequestEnvelope, SendMode, ServerMessage, PROTOCOL_VERSION,
};
use formless_runtime::Capabilities;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot},
};

type Lines = tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>;
struct Client {
    read: Lines,
    write: tokio::net::unix::OwnedWriteHalf,
    events: Vec<Event>,
    cursor: usize,
    serial: u64,
}
impl Client {
    async fn connect(paths: &Paths) -> Self {
        let stream = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(stream) = tokio::net::UnixStream::connect(paths.socket_path()).await {
                    break stream;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("private IPC listener did not appear");
        let (read, write) = stream.into_split();
        let mut client = Self {
            read: tokio::io::BufReader::new(read).lines(),
            write,
            events: Vec::new(),
            cursor: 0,
            serial: 0,
        };
        client
            .rpc(Request::Subscribe {
                sessions: None,
                deltas: true,
                client: Some("race-acceptance".into()),
            })
            .await;
        client
    }
    async fn message(&mut self) -> ServerMessage {
        let line = tokio::time::timeout(Duration::from_secs(10), self.read.next_line())
            .await
            .expect("bounded IPC response/event missing")
            .unwrap()
            .expect("private daemon disconnected");
        serde_json::from_str(&line).unwrap()
    }
    async fn rpc(&mut self, request: Request) -> Value {
        self.serial += 1;
        let id = self.serial;
        let request = RequestEnvelope {
            id,
            protocol: PROTOCOL_VERSION,
            request,
        };
        self.write
            .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
            .await
            .unwrap();
        loop {
            match self.message().await {
                ServerMessage::Event(event) => self.events.push(event),
                ServerMessage::Response {
                    id: actual,
                    ok,
                    result,
                    error,
                } if actual == id => {
                    assert!(ok, "private RPC rejected: {error:?}");
                    return result.unwrap_or(Value::Null);
                }
                other => panic!("unexpected private RPC reply: {other:?}"),
            }
        }
    }
    async fn event(&mut self, predicate: impl Fn(&Event) -> bool) -> Event {
        loop {
            while let Some(event) = self.events.get(self.cursor) {
                self.cursor += 1;
                if predicate(event) {
                    return event.clone();
                }
            }
            if let ServerMessage::Event(event) = self.message().await {
                self.events.push(event);
            }
        }
    }
    async fn finished(&mut self, run: &str, status: &str) {
        let event = self
            .event(|event| matches!(event,Event::RunFinished {run_id,..} if run_id==run))
            .await;
        let Event::RunFinished { status: actual, .. } = event else {
            unreachable!()
        };
        assert_eq!(actual, status);
    }
}
struct ProviderCall {
    body: Value,
    reply: oneshot::Sender<Value>,
}
impl ProviderCall {
    fn answer(self, delta: Value) {
        self.reply
            .send(delta)
            .expect("provider request cancelled before scripted reply");
    }
}
struct Fixture {
    root: std::path::PathBuf,
    session: String,
    store: Store,
    daemon: Arc<Daemon>,
    server: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    provider: tokio::task::JoinHandle<()>,
    calls: mpsc::UnboundedReceiver<ProviderCall>,
    gates: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    provider_count: Arc<AtomicUsize>,
    base: String,
}
impl Fixture {
    async fn new(env: &credential_env::EnvGuard, secure: bool, hook: bool) -> (Self, Client) {
        env.set("FORMLESS_API_KEY", "private-control-race-key");
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");
        let root = std::path::PathBuf::from("/tmp").join(format!("fl-cr-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let (tx, calls) = mpsc::unbounded_channel();
        let (gate_tx, gates) = mpsc::unbounded_channel();
        let provider_count = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route("/chat/completions",post({
            let count = provider_count.clone();
            move |Json(body):Json<Value>| {
                let tx = tx.clone();let count = count.clone();
                async move {
                    count.fetch_add(1,Ordering::SeqCst);
                    let (reply,rx) = oneshot::channel();
                    tx.send(ProviderCall {body,reply}).unwrap();
                    let delta = rx.await.unwrap_or_else(|_|json!({"content":"private fixture request cancelled"}));
                    let sse = format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"delta":delta,"finish_reason":if delta.get("tool_calls").is_some(){"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}}));
                    axum::response::Response::builder().header("content-type","text/event-stream").body(axum::body::Body::from(sse)).unwrap()
                }
            }
        })).route("/gate",get(move || {
            let tx = gate_tx.clone();async move {
                let (reply,rx) = oneshot::channel();tx.send(reply).unwrap();
                rx.await.expect("tool barrier must be released");"tool gate released"
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let provider = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = Config::default();
        config.provider.base_url = base.clone();
        config.provider.model = "private-scripted".into();
        config.provider.api_style = "chat".into();
        config.provider.max_retries = 0;
        config.naming.enabled = false;
        config.skills.auto_create = false;
        config.security.secure = secure;
        config.tools.pending_threshold_secs = 60;
        config.tools.approval_summaries = false;
        config.memory.confirm_writes = "always".into();
        if hook {
            config.hooks.push(HookCfg {
                event: "pre_tool".into(),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf hook > \"$1\"".into(),
                    "race-hook".into(),
                    root.join("hook-ran").to_string_lossy().into_owned(),
                ],
                trusted: true,
                ..Default::default()
            });
        }
        config.save(&paths).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap().id;
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        let server = tokio::spawn({
            let daemon = daemon.clone();
            async move { daemon.serve().await }
        });
        let client = Client::connect(&paths).await;
        (
            Self {
                root,
                session,
                store,
                daemon,
                server: Some(server),
                provider,
                calls,
                gates,
                provider_count,
                base,
            },
            client,
        )
    }
    fn tool(&self, name: &str, caps: &[String], code: &str) -> String {
        let (info, _version) = self
            .store
            .create_tool_version(
                name,
                "private race acceptance",
                r#"{"type":"object"}"#,
                "{}",
                code,
                &Capabilities::parse(caps).unwrap(),
                64,
                "user",
            )
            .unwrap();
        info.id
    }
    async fn next_call(&mut self) -> ProviderCall {
        tokio::time::timeout(Duration::from_secs(10), self.calls.recv())
            .await
            .expect("provider request did not reach barrier")
            .expect("private provider stopped")
    }
    async fn start(&self, client: &mut Client) -> String {
        let ack = client
            .rpc(Request::SendMessage {
                session_id: self.session.clone(),
                content: "initial race acceptance task".into(),
                mode: SendMode::Send,
                client_msg_id: None,
            })
            .await;
        ack["run_id"]
            .as_str()
            .expect("started Run must be acknowledged")
            .into()
    }
    async fn shutdown(mut self) {
        self.daemon.handle(Request::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.server.take().unwrap())
            .await
            .expect("private daemon shutdown")
            .unwrap()
            .unwrap();
        self.provider.abort();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.provider.abort();
        if let Some(server) = &self.server {
            server.abort();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn calls(tools: &[(&str, Value)]) -> Value {
    json!({"tool_calls":tools.iter().enumerate().map(|(index,(name,args))|json!({"index":index,"id":format!("call-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})).collect::<Vec<_>>()})
}
fn contains(body: &Value, text: &str) -> bool {
    body["messages"].to_string().contains(text)
}

#[tokio::test(flavor = "multi_thread")]
async fn steer_during_real_tool_execution_produces_exactly_one_final_answer() {
    let env = credential_env::EnvGuard::isolated();
    let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
    fixture.tool(
        "race_gate",
        &["net:127.0.0.1".into()],
        &format!(
            "function run(){{ return fetch('{}/gate').body; }}",
            fixture.base
        ),
    );
    let run = fixture.start(&mut client).await;
    fixture
        .next_call()
        .await
        .answer(calls(&[("race_gate", json!({}))]));
    let release = tokio::time::timeout(Duration::from_secs(10), fixture.gates.recv())
        .await
        .expect("tool did not enter actual HTTP gate")
        .unwrap();
    let ack = client
        .rpc(Request::SendMessage {
            session_id: fixture.session.clone(),
            content: "STEER_DURING_TOOL_964".into(),
            mode: SendMode::Steer,
            client_msg_id: None,
        })
        .await;
    assert_eq!(ack["steered"], true);
    assert_eq!(ack["run_id"], run);
    release.send(()).unwrap();
    let final_call = fixture.next_call().await;
    assert!(contains(&final_call.body, "STEER_DURING_TOOL_964"));
    final_call.answer(json!({"content":"single final answer including steer"}));
    client.finished(&run, "completed").await;
    assert_eq!(
        fixture.provider_count.load(Ordering::SeqCst),
        2,
        "already-consumed steer must not request a duplicate final turn"
    );
    let finals: Vec<_> = fixture
        .store
        .list_messages(&fixture.session)
        .unwrap()
        .into_iter()
        .filter(|m| m.role == "assistant" && !m.content.is_empty())
        .collect();
    assert_eq!(finals.len(), 1);
    assert_eq!(finals[0].content, "single final answer including steer");
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn steer_on_both_sides_of_terminal_event_is_answered_by_an_owned_run() {
    let env = credential_env::EnvGuard::isolated();
    for before in [true, false] {
        let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
        let run = fixture.start(&mut client).await;
        let final_call = fixture.next_call().await;
        if before {
            let ack = client
                .rpc(Request::SendMessage {
                    session_id: fixture.session.clone(),
                    content: "LATE_STEER_964".into(),
                    mode: SendMode::Steer,
                    client_msg_id: None,
                })
                .await;
            assert_eq!(ack["steered"], true);
            assert_eq!(ack["run_id"], run);
            final_call.answer(json!({"content":"original request final"}));
            let answer = fixture.next_call().await;
            assert!(contains(&answer.body, "LATE_STEER_964"));
            answer.answer(json!({"content":"late steer answered"}));
            client.finished(&run, "completed").await;
        } else {
            final_call.answer(json!({"content":"original request final"}));
            client.finished(&run, "completed").await;
            let ack = client
                .rpc(Request::SendMessage {
                    session_id: fixture.session.clone(),
                    content: "LATE_STEER_964".into(),
                    mode: SendMode::Steer,
                    client_msg_id: None,
                })
                .await;
            let next = ack["run_id"]
                .as_str()
                .expect("post-terminal steer must start an acknowledged Run");
            assert_ne!(next, run);
            let answer = fixture.next_call().await;
            assert!(contains(&answer.body, "LATE_STEER_964"));
            answer.answer(json!({"content":"late steer answered"}));
            client.finished(next, "completed").await;
        }
        assert_eq!(fixture.provider_count.load(Ordering::SeqCst), 2);
        let messages = fixture.store.list_messages(&fixture.session).unwrap();
        let steer = messages
            .iter()
            .position(|m| m.role == "user" && m.content == "LATE_STEER_964")
            .unwrap();
        assert!(
            messages[steer + 1..]
                .iter()
                .any(|m| m.role == "assistant" && m.content == "late steer answered"),
            "acknowledged steer must not remain orphaned"
        );
        fixture.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupt_first_approval_stops_entire_batch_without_denials_or_side_effects() {
    let env = credential_env::EnvGuard::isolated();
    let (mut fixture, mut client) = Fixture::new(&env, true, true).await;
    let marker = fixture.root.join("tool-ran");
    for name in ["sensitive_a", "sensitive_b"] {
        fixture.tool(
            name,
            &[format!("fs:write:{}", marker.display())],
            &format!(
                "function run(){{ fs.write({:?},'ran'); return {{ran:true}}; }}",
                marker.to_string_lossy()
            ),
        );
    }
    let run = fixture.start(&mut client).await;
    fixture.next_call().await.answer(calls(&[
        ("sensitive_a", json!({})),
        ("sensitive_b", json!({})),
        (
            "remember",
            json!({"text":"must not persist after interrupt"}),
        ),
        (
            "schedule_run",
            json!({"prompt":"must not schedule","at":"2099-01-01T00:00:00Z"}),
        ),
    ]));
    let event = client
        .event(|event| matches!(event, Event::ApprovalRequested { .. }))
        .await;
    let Event::ApprovalRequested { approval_id, .. } = event else {
        unreachable!()
    };
    let interrupted = client
        .rpc(Request::InterruptRun {
            session_id: fixture.session.clone(),
        })
        .await;
    assert_eq!(interrupted["interrupted"], true);
    client.finished(&run, "interrupted").await;
    assert_eq!(
        closed_approvals(&client.events, &approval_id),
        1,
        "interrupted approval must explicitly close exactly once"
    );
    assert!(
        !client.events.iter().any(
            |event| matches!(event,Event::ApprovalAnswered {approval_id:id,..} if id==&approval_id)
        ),
        "cancellation is not an approval answer"
    );
    assert_eq!(
        client
            .events
            .iter()
            .filter(|event| matches!(event, Event::ApprovalRequested { .. }))
            .count(),
        1,
        "cancelled pre-pass must not flash later prompts"
    );
    assert!(client
        .rpc(Request::PendingApprovals)
        .await
        .as_array()
        .unwrap()
        .is_empty());
    let losing = client
        .rpc(Request::ApprovalDecision {
            approval_id,
            approved: true,
            scope: ApprovalScope::Always,
            paths: Default::default(),
            remember_denial: false,
        })
        .await;
    assert_eq!(losing["resolved"], false);
    assert!(fixture
        .store
        .run_invocations(&run)
        .unwrap()
        .iter()
        .all(|(_, _, status)| status != "denied"));
    assert!(fixture
        .store
        .recent_audit(Some("tool.deny"), 100)
        .unwrap()
        .is_empty());
    assert!(client
        .rpc(Request::ListDenials)
        .await
        .as_array()
        .unwrap()
        .is_empty());
    assert!(fixture.store.builtin_stats(None).unwrap().is_empty());
    assert!(fixture.store.list_memories().unwrap().is_empty());
    assert!(fixture.store.list_jobs().unwrap().is_empty());
    assert!(!marker.exists());
    assert!(!fixture.root.join("hook-ran").exists());
    assert_eq!(
        fixture.store.run_status(&run).unwrap().as_deref(),
        Some("interrupted")
    );
    assert_eq!(fixture.provider_count.load(Ordering::SeqCst), 1);
    fixture.shutdown().await;
}

fn closed_approvals(events: &[Event], id: &str) -> usize {
    events
        .iter()
        .filter(|event| {
            let value = serde_json::to_value(event).unwrap();
            value["type"] == "approval_closed" && value["approval_id"] == id
        })
        .count()
}
fn approval_decision(id: &str) -> Request {
    Request::ApprovalDecision {
        approval_id: id.into(),
        approved: true,
        scope: ApprovalScope::Always,
        paths: Default::default(),
        remember_denial: false,
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn approval_answer_then_interrupt_and_interrupt_then_answer_have_one_consistent_winner() {
    let env = credential_env::EnvGuard::isolated();
    for answer_first in [true, false] {
        let (mut fixture, mut client) = Fixture::new(&env, true, false).await;
        let tool = fixture.tool(
            "lasting_grant",
            &[format!(
                "fs:write:{}",
                fixture.root.join("unused-output").display()
            )],
            "function run(){ return {approved:true}; }",
        );
        let run = fixture.start(&mut client).await;
        fixture
            .next_call()
            .await
            .answer(calls(&[("lasting_grant", json!({}))]));
        let event = client
            .event(|event| matches!(event, Event::ApprovalRequested { .. }))
            .await;
        let Event::ApprovalRequested { approval_id, .. } = event else {
            unreachable!()
        };
        if answer_first {
            let ack = client.rpc(approval_decision(&approval_id)).await;
            assert_eq!(ack["resolved"], true);
            assert_eq!(ack["audit_recorded"], true);
        }
        let interrupt = client
            .rpc(Request::InterruptRun {
                session_id: fixture.session.clone(),
            })
            .await;
        assert_eq!(interrupt["interrupted"], true);
        client.finished(&run, "interrupted").await;
        assert_eq!(
            closed_approvals(&client.events, &approval_id),
            usize::from(!answer_first)
        );
        let duplicate = client.rpc(approval_decision(&approval_id)).await;
        assert_eq!(
            duplicate["resolved"], false,
            "cancel-first and duplicate answers must not claim resolution"
        );
        assert_eq!(
            fixture.store.get_approval(&tool).unwrap().is_some(),
            answer_first,
            "acknowledged Always grant must survive interruption; losing answer must not grant"
        );
        assert_eq!(
            fixture
                .store
                .recent_audit(Some("approval.decision"), 100)
                .unwrap()
                .len(),
            usize::from(answer_first)
        );
        assert_eq!(
            fixture
                .store
                .recent_audit(Some("tool.approve"), 100)
                .unwrap()
                .len(),
            usize::from(answer_first)
        );
        assert!(
            fixture
                .store
                .recent_audit(Some("tool.deny"), 100)
                .unwrap()
                .is_empty(),
            "interruption must not become an explicit denial"
        );
        assert_eq!(client.events.iter().filter(|event|matches!(event,Event::ApprovalAnswered {approval_id:id,approved:true,..} if id==&approval_id)).count(),usize::from(answer_first));
        assert_eq!(
            client
                .events
                .iter()
                .filter(|event| matches!(event, Event::ApprovalRequested { .. }))
                .count(),
            1
        );
        assert!(client
            .rpc(Request::PendingApprovals)
            .await
            .as_array()
            .unwrap()
            .is_empty());
        fixture.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn question_answer_then_interrupt_and_interrupt_then_answer_preserve_acknowledged_answer_once(
) {
    let env = credential_env::EnvGuard::isolated();
    for answer_first in [true, false] {
        let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
        let run = fixture.start(&mut client).await;
        fixture.next_call().await.answer(calls(&[(
            "ask_user",
            json!({"question":"Which exact race answer?"}),
        )]));
        let event = client
            .event(|event| matches!(event, Event::UserQuestion { .. }))
            .await;
        let Event::UserQuestion { question_id, .. } = event else {
            unreachable!()
        };
        if answer_first {
            let ack = client
                .rpc(Request::AnswerQuestion {
                    question_id: question_id.clone(),
                    answer: "WINNING_ANSWER_966".into(),
                    answers: vec![],
                })
                .await;
            assert_eq!(ack["answered"], true);
        }
        let interrupt = client
            .rpc(Request::InterruptRun {
                session_id: fixture.session.clone(),
            })
            .await;
        assert_eq!(interrupt["interrupted"], true);
        client.finished(&run, "interrupted").await;
        let losing = client
            .rpc(Request::AnswerQuestion {
                question_id: question_id.clone(),
                answer: "LOSING_ANSWER_966".into(),
                answers: vec![],
            })
            .await;
        assert_eq!(losing["answered"], false);
        let messages = fixture.store.list_messages(&fixture.session).unwrap();
        let applied: Vec<_> = messages
            .iter()
            .filter(|m| m.role == "tool" && m.content.contains("WINNING_ANSWER_966"))
            .collect();
        assert_eq!(
            applied.len(),
            usize::from(answer_first),
            "acknowledged answer must remain in durable tool result despite interrupt"
        );
        if answer_first {
            assert_eq!(
                serde_json::from_str::<Value>(&applied[0].content).unwrap(),
                json!({"answer":"WINNING_ANSWER_966"})
            );
        }
        assert!(!messages
            .iter()
            .any(|m| m.content.contains("LOSING_ANSWER_966")));
        assert_eq!(client.events.iter().filter(|event|matches!(event,Event::UserQuestionClosed {question_id:id, .. } if id==&question_id)).count(),1);
        let state = client
            .rpc(Request::GetSession {
                id: fixture.session.clone(),
            })
            .await;
        assert!(!state["pending_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["question_id"] == question_id));
        fixture.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn builtin_memory_approval_answer_cancel_orders_apply_only_the_winning_answer() {
    let env = credential_env::EnvGuard::isolated();
    for answer_first in [true, false] {
        let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
        let run = fixture.start(&mut client).await;
        fixture
            .next_call()
            .await
            .answer(calls(&[("remember", json!({"text":"MEMORY_WINNER_966"}))]));
        let event=client.event(|event|matches!(event,Event::ApprovalRequested {tool_name,..} if tool_name=="memory")).await;
        let Event::ApprovalRequested { approval_id, .. } = event else {
            unreachable!()
        };
        if answer_first {
            let ack = client.rpc(approval_decision(&approval_id)).await;
            assert_eq!(ack["resolved"], true);
            assert_eq!(ack["audit_recorded"], true);
        }
        let interrupt = client
            .rpc(Request::InterruptRun {
                session_id: fixture.session.clone(),
            })
            .await;
        assert_eq!(interrupt["interrupted"], true);
        client.finished(&run, "interrupted").await;
        assert_eq!(
            client.rpc(approval_decision(&approval_id)).await["resolved"],
            false
        );
        assert_eq!(
            fixture
                .store
                .list_memories()
                .unwrap()
                .iter()
                .filter(|memory| memory.text == "MEMORY_WINNER_966")
                .count(),
            usize::from(answer_first)
        );
        assert_eq!(
            fixture
                .store
                .recent_audit(Some("memory.remember"), 100)
                .unwrap()
                .len(),
            usize::from(answer_first)
        );
        assert_eq!(
            fixture
                .store
                .recent_audit(Some("approval.decision"), 100)
                .unwrap()
                .len(),
            usize::from(answer_first)
        );
        assert_eq!(client.events.iter().filter(|event|matches!(event,Event::ApprovalAnswered {approval_id:id,approved:true,..} if id==&approval_id)).count(),usize::from(answer_first));
        assert!(client
            .rpc(Request::PendingApprovals)
            .await
            .as_array()
            .unwrap()
            .is_empty());
        fixture.shutdown().await;
    }
}
