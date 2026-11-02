//! Executable acceptance for isolated approval batches, bounded daemon
//! corrections and request-scoped retries. All provider traffic is loopback.
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
#[derive(Debug)]
enum Reply {
    Delta(Value),
    Truncated(Value),
    Stalled,
}
struct ProviderCall {
    body: Value,
    reply: oneshot::Sender<Reply>,
}
impl ProviderCall {
    fn answer(self, delta: Value) {
        self.reply
            .send(Reply::Delta(delta))
            .expect("provider request cancelled before scripted reply");
    }
}
impl ProviderCall {
    fn truncated(self, delta: Value) {
        self.reply
            .send(Reply::Truncated(delta))
            .unwrap_or_else(|_| panic!("provider request cancelled"));
    }
    fn stalled(self) {
        self.reply
            .send(Reply::Stalled)
            .unwrap_or_else(|_| panic!("provider request cancelled"));
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
        Self::with_pending_threshold(env, secure, hook, 60).await
    }
    async fn with_pending_threshold(
        env: &credential_env::EnvGuard,
        secure: bool,
        hook: bool,
        pending_threshold_secs: u64,
    ) -> (Self, Client) {
        env.set("FORMLESS_API_KEY", "private-control-race-key");
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");
        let root = std::path::PathBuf::from("/tmp").join(format!("fl-rc-{}", uuid::Uuid::new_v4()));
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
                    let reply = rx.await.unwrap_or_else(|_|Reply::Delta(json!({"content":"private fixture request cancelled"})));
                    let truncated=matches!(&reply,Reply::Truncated(_));
                    let body=match reply {
                        Reply::Stalled => axum::body::Body::from_stream(futures_util::stream::pending::<Result<String,std::io::Error>>()),
                        Reply::Delta(delta)|Reply::Truncated(delta) => {
                            let sse = format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"delta":delta,"finish_reason":if truncated {"length"}else if delta.get("tool_calls").is_some(){"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":5,"completion_tokens":2}}));
                            axum::body::Body::from(sse)
                        }
                    };
                    axum::response::Response::builder().header("content-type","text/event-stream").body(body).unwrap()
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
        config.provider.stall_notice_secs = 1;
        config.provider.first_byte_timeout_secs = 0;
        config.naming.enabled = false;
        config.skills.auto_create = false;
        config.security.secure = secure;
        config.tools.pending_threshold_secs = pending_threshold_secs;
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

fn raw_call(name: &str, args: &str) -> Value {
    json!({"tool_calls":[{"index":0,"id":"correction-call","type":"function","function":{"name":name,"arguments":args}}]})
}
#[tokio::test(flavor = "multi_thread")]
async fn repeated_daemon_corrections_pause_at_three_and_attach_retains_reason() {
    let env = credential_env::EnvGuard::isolated();
    for class in ["json", "truncated", "unknown"] {
        let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
        let run = fixture.start(&mut client).await;
        for _ in 0..3 {
            let request = fixture.next_call().await;
            match class {
                "json" => request.answer(raw_call("find_tools", "{")),
                "truncated" => request.truncated(raw_call("find_tools", "{\"query\":")),
                "unknown" => request.answer(raw_call("nonexistent_correction_target_970", "{}")),
                _ => unreachable!(),
            }
        }
        client.finished(&run, "paused").await;
        assert_eq!(
            fixture.provider_count.load(Ordering::SeqCst),
            3,
            "identical daemon correction must not loop indefinitely"
        );
        let paused: Vec<_> = client
            .events
            .iter()
            .filter_map(|event| {
                if let Event::RunPaused {
                    run_id,
                    error,
                    explained,
                    ..
                } = event
                {
                    Some((run_id.clone(), error.clone(), explained.clone()))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(paused.len(), 1);
        assert_eq!(paused[0].0, run);
        assert!(paused[0].1.starts_with("stuck:"));
        assert_eq!(paused[0].2.as_ref().unwrap().category, "stuck");
        assert!(paused[0].2.as_ref().unwrap().action.is_some());
        let attached = client
            .rpc(Request::GetSession {
                id: fixture.session.clone(),
            })
            .await;
        assert_eq!(attached["unfinished_run"]["run_id"], run);
        assert_eq!(attached["unfinished_run"]["status"], "paused");
        assert_eq!(attached["unfinished_run"]["error"], paused[0].1);
        let messages = fixture.store.list_messages(&fixture.session).unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.role == "tool")
                .count(),
            3,
            "each correction must have a durable result before pause"
        );
        fixture.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn corrected_json_on_second_attempt_and_changed_arguments_reset_correction_streak() {
    let env = credential_env::EnvGuard::isolated();
    for changed in [false, true] {
        let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
        let run = fixture.start(&mut client).await;
        fixture
            .next_call()
            .await
            .answer(raw_call("find_tools", "{"));
        if changed {
            fixture
                .next_call()
                .await
                .answer(raw_call("find_tools", "{"));
            fixture
                .next_call()
                .await
                .answer(raw_call("find_tools", "{ "));
        }
        fixture.next_call().await.answer(calls(&[(
            "find_tools",
            json!({"query":"private corrected query"}),
        )]));
        fixture
            .next_call()
            .await
            .answer(json!({"content":"corrected arguments completed"}));
        client.finished(&run, "completed").await;
        assert_eq!(
            fixture.provider_count.load(Ordering::SeqCst),
            if changed { 5 } else { 3 }
        );
        assert!(!client
            .events
            .iter()
            .any(|event| matches!(event, Event::RunPaused { .. })));
        assert!(fixture
            .store
            .list_messages(&fixture.session)
            .unwrap()
            .iter()
            .any(|message| message.role == "assistant"
                && message.content == "corrected arguments completed"));
        fixture.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_tool_errors_and_successful_repeated_polls_do_not_trigger_stuck_guard() {
    let env = credential_env::EnvGuard::isolated();
    for failure in [true, false] {
        let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
        let name = if failure {
            "ordinary_failure"
        } else {
            "poll_status"
        };
        fixture.tool(
            name,
            &[],
            if failure {
                "function run(){ throw new Error('ordinary repeated Tool error'); }"
            } else {
                "function run(){ return {status:'pending'}; }"
            },
        );
        let run = fixture.start(&mut client).await;
        for _ in 0..5 {
            fixture
                .next_call()
                .await
                .answer(calls(&[(name, json!({}))]));
        }
        fixture
            .next_call()
            .await
            .answer(json!({"content":"ordinary repeated calls completed"}));
        client.finished(&run, "completed").await;
        assert_eq!(fixture.provider_count.load(Ordering::SeqCst), 6);
        assert!(!client
            .events
            .iter()
            .any(|event| matches!(event, Event::RunPaused { .. })));
        assert_eq!(fixture.store.run_invocations(&run).unwrap().len(), 5);
        fixture.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_during_real_tool_execution_reports_false_without_restarting_next_request() {
    let env = credential_env::EnvGuard::isolated();
    let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
    fixture.tool(
        "retry_gate",
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
        .answer(calls(&[("retry_gate", json!({}))]));
    let release = tokio::time::timeout(Duration::from_secs(10), fixture.gates.recv())
        .await
        .unwrap()
        .unwrap();
    let ack = client
        .rpc(Request::RetryProviderRequest {
            session_id: fixture.session.clone(),
        })
        .await;
    assert_eq!(ack["retrying"], false);
    release.send(()).unwrap();
    fixture
        .next_call()
        .await
        .answer(json!({"content":"single request after tool"}));
    client.finished(&run, "completed").await;
    assert_eq!(fixture.provider_count.load(Ordering::SeqCst), 2);
    assert!(
        !client
            .events
            .iter()
            .any(|event| matches!(event, Event::StreamRestarted { .. })),
        "tool-phase retry cannot leave a future permit"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_of_observed_stalled_stream_resends_once_and_does_not_reach_successor_epoch() {
    let env = credential_env::EnvGuard::isolated();
    let (mut fixture, mut client) = Fixture::new(&env, false, false).await;
    fixture.tool(
        "retry_gate",
        &["net:127.0.0.1".into()],
        &format!(
            "function run(){{ return fetch('{}/gate').body; }}",
            fixture.base
        ),
    );
    let run = fixture.start(&mut client).await;
    let stalled = fixture.next_call().await;
    let original = stalled.body.clone();
    stalled.stalled();
    client
        .event(|event| matches!(event,Event::ProviderStalled {run_id,..} if run_id==&run))
        .await;
    assert_eq!(
        client
            .rpc(Request::RetryProviderRequest {
                session_id: fixture.session.clone()
            })
            .await["retrying"],
        true
    );
    let retry = fixture.next_call().await;
    assert_eq!(
        retry.body, original,
        "retry must resend the same in-flight request"
    );
    retry.answer(calls(&[("retry_gate", json!({}))]));
    let release = tokio::time::timeout(Duration::from_secs(10), fixture.gates.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        client
            .rpc(Request::RetryProviderRequest {
                session_id: fixture.session.clone()
            })
            .await["retrying"],
        false
    );
    release.send(()).unwrap();
    fixture
        .next_call()
        .await
        .answer(json!({"content":"successor request sent once"}));
    client.finished(&run, "completed").await;
    assert_eq!(fixture.provider_count.load(Ordering::SeqCst), 3);
    assert_eq!(
        client
            .events
            .iter()
            .filter(|event| matches!(event, Event::StreamRestarted { .. }))
            .count(),
        1
    );
    fixture.shutdown().await;
}

fn inspect_batch(event: Event, paths: &[String], index: usize) -> (String, String) {
    let Event::ApprovalRequested {
        approval_id,
        batch,
        capabilities,
        ..
    } = event
    else {
        unreachable!()
    };
    let batch = batch.expect("concurrent sub-agent pre-pass must retain its own immutable batch");
    assert_eq!(batch.index, index);
    assert_eq!(batch.calls.len(), paths.len());
    for (call, path) in batch.calls.iter().zip(paths) {
        assert_eq!(call.tool_name, "scoped_action");
        assert_eq!(call.capabilities, vec![format!("fs:write:{path}")]);
        assert_eq!(call.new_capabilities, call.capabilities);
    }
    assert_eq!(capabilities, vec![format!("fs:write:{}", paths[index - 1])]);
    (approval_id, batch.batch_id)
}
async fn approve(client: &mut Client, id: &str) {
    let ack = client
        .rpc(Request::ApprovalDecision {
            approval_id: id.into(),
            approved: true,
            scope: ApprovalScope::Once,
            paths: Default::default(),
            remember_denial: false,
        })
        .await;
    assert_eq!(ack["resolved"], true);
}
fn scoped_batch(prefix: &str, paths: &[String]) -> Value {
    let mut delta = calls(
        &paths
            .iter()
            .map(|path| ("scoped_action", json!({"path":path})))
            .collect::<Vec<_>>(),
    );
    for (index, call) in delta["tool_calls"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        call["id"] = json!(format!("{prefix}-{index}"));
    }
    delta
}
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_subagent_batches_and_direct_run_keep_independent_prompt_metadata() {
    let env = credential_env::EnvGuard::isolated();
    let (mut fixture, mut client) = Fixture::new(&env, true, false).await;
    let scoped = fixture.tool(
        "scoped_action",
        &["fs:write:$input.path".into()],
        "function run(i){ return {scope:i.path}; }",
    );
    for (name, task) in [
        ("delegate_a", "BATCH_ALPHA_969"),
        ("delegate_b", "BATCH_BETA_969"),
    ] {
        let id = fixture.tool(
            name,
            &["agent".into()],
            &format!("function run(){{ return agents.run('{task}'); }}"),
        );
        fixture
            .store
            .set_approval(&id, &Capabilities::parse(&["agent".into()]).unwrap(), 64)
            .unwrap();
    }
    let paths_a: Vec<_> = ["a1", "a2"]
        .iter()
        .map(|name| fixture.root.join(name).to_string_lossy().into_owned())
        .collect();
    let paths_b: Vec<_> = ["b1", "b2"]
        .iter()
        .map(|name| fixture.root.join(name).to_string_lossy().into_owned())
        .collect();
    let direct_path = fixture.root.join("direct").to_string_lossy().into_owned();
    let run = fixture.start(&mut client).await;
    fixture.next_call().await.answer(calls(&[
        ("delegate_a", json!({})),
        ("delegate_b", json!({})),
    ]));
    let first = fixture.next_call().await;
    let second = fixture.next_call().await;
    let (alpha, beta) = if contains(&first.body, "BATCH_ALPHA_969") {
        (first, second)
    } else {
        (second, first)
    };
    assert!(contains(&alpha.body, "BATCH_ALPHA_969"));
    assert!(contains(&beta.body, "BATCH_BETA_969"));
    alpha.answer(scoped_batch("alpha", &paths_a));
    let (approval_a1,batch_a)=inspect_batch(client.event(|event|matches!(event,Event::ApprovalRequested {tool_name,..} if tool_name=="scoped_action")).await,&paths_a,1);
    beta.answer(scoped_batch("beta", &paths_b));
    let (approval_b1,batch_b)=inspect_batch(client.event(|event|matches!(event,Event::ApprovalRequested {tool_name,..} if tool_name=="scoped_action")).await,&paths_b,1);
    assert_ne!(batch_a, batch_b);
    let paths = Paths::from_dirs(fixture.root.join("d"), fixture.root.join("c")).unwrap();
    let mut direct_client = Client::connect(&paths).await;
    let sid = fixture.session.clone();
    let input = json!({"path":direct_path});
    let direct = tokio::spawn(async move {
        direct_client
            .rpc(Request::RunTool {
                id: scoped,
                input,
                session_id: sid,
            })
            .await
    });
    let direct_event=client.event(|event|matches!(event,Event::ApprovalRequested {tool_name,..} if tool_name=="scoped_action")).await;
    let Event::ApprovalRequested {
        approval_id: direct_approval,
        batch,
        capabilities,
        ..
    } = direct_event
    else {
        unreachable!()
    };
    assert!(
        batch.is_none(),
        "standalone RunTool must not inherit another pre-pass batch"
    );
    assert_eq!(capabilities, vec![format!("fs:write:{direct_path}")]);
    // Complete beta while alpha and the standalone prompt remain pending.
    // Cleanup of beta must not erase alpha's second-call metadata.
    approve(&mut client, &approval_b1).await;
    let (approval_b2,actual_b)=inspect_batch(client.event(|event|matches!(event,Event::ApprovalRequested {tool_name,..} if tool_name=="scoped_action")).await,&paths_b,2);
    assert_eq!(actual_b, batch_b);
    approve(&mut client, &approval_b2).await;
    fixture
        .next_call()
        .await
        .answer(json!({"content":"beta done"}));
    approve(&mut client, &direct_approval).await;
    let result = tokio::time::timeout(Duration::from_secs(10), direct)
        .await
        .unwrap()
        .unwrap();
    assert!(result.to_string().contains(&direct_path));
    approve(&mut client, &approval_a1).await;
    let (approval_a2,actual_a)=inspect_batch(client.event(|event|matches!(event,Event::ApprovalRequested {tool_name,..} if tool_name=="scoped_action")).await,&paths_a,2);
    assert_eq!(actual_a, batch_a);
    approve(&mut client, &approval_a2).await;
    fixture
        .next_call()
        .await
        .answer(json!({"content":"alpha done"}));
    fixture
        .next_call()
        .await
        .answer(json!({"content":"both subagents completed"}));
    client.finished(&run, "completed").await;
    assert_eq!(fixture.provider_count.load(Ordering::SeqCst), 6);
    assert_eq!(client.events.iter().filter(|event|matches!(event,Event::ApprovalRequested {tool_name,..} if tool_name=="scoped_action")).count(),5);
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn real_pending_invocation_can_be_polled_identically_beyond_stuck_threshold() {
    let env = credential_env::EnvGuard::isolated();
    let (mut fixture, mut client) = Fixture::with_pending_threshold(&env, false, false, 0).await;
    fixture.tool(
        "pending_poll_gate",
        &["net:127.0.0.1".into()],
        &format!(
            "function run(){{ return fetch('{}/gate').body; }}",
            fixture.base
        ),
    );
    let pending = fixture
        .daemon
        .call_tool_direct(&fixture.session, "pending_poll_gate", json!({}))
        .await;
    let pending: Value = serde_json::from_str(&pending).unwrap();
    assert_eq!(pending["status"], "pending");
    let handle = pending["invocation_id"].as_str().unwrap();
    let release = tokio::time::timeout(Duration::from_secs(10), fixture.gates.recv())
        .await
        .unwrap()
        .unwrap();
    let run = fixture.start(&mut client).await;
    for _ in 0..5 {
        fixture.next_call().await.answer(calls(&[(
            "check_invocation",
            json!({"invocation_id":handle}),
        )]));
    }
    let final_call = fixture.next_call().await;
    assert!(contains(&final_call.body, "pending"));
    release.send(()).unwrap();
    // Releasing the HTTP response does not mean the sandbox has returned or
    // the pending result has been published. Observe that exact handle's
    // completion before supplying the model's final answer, otherwise the
    // daemon correctly asks for one additional turn to handle live work.
    let completion = client
        .event(
            |event| matches!(event,Event::InvocationCompleted {handle:actual,..} if actual==handle),
        )
        .await;
    let Event::InvocationCompleted { ok, tool_name, .. } = completion else {
        unreachable!()
    };
    assert!(ok, "the real pending Tool must finish successfully");
    assert_eq!(tool_name, "pending_poll_gate");
    final_call.answer(json!({"content":"five actual pending polls completed"}));
    client.finished(&run, "completed").await;
    assert_eq!(fixture.provider_count.load(Ordering::SeqCst), 6);
    let polls = fixture.store.builtin_stats(None).unwrap();
    let poll = polls
        .iter()
        .find(|stat| stat.name == "check_invocation")
        .unwrap();
    assert_eq!(poll.calls, 5);
    assert_eq!(poll.failures, 0);
    assert!(!client
        .events
        .iter()
        .any(|event| matches!(event, Event::RunPaused { .. })));
    fixture.shutdown().await;
}
