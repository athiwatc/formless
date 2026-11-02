//! Actual JavaScript -> daemon -> IPC regressions for issue #1006.
//! Progress reports must not become an unlimited activity heartbeat.
#[path = "support/env.rs"]
mod credential_env;

use formless_core::{daemon::Daemon, storage::Store, Config, Paths};
use formless_protocol::{Request, RequestEnvelope, ServerMessage, PROTOCOL_VERSION};
use formless_runtime::Capabilities;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};

trait Connection: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Connection for T {}
type Stream = Box<dyn Connection>;
struct Client {
    write: tokio::io::WriteHalf<Stream>,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::io::ReadHalf<Stream>>>,
}
impl Client {
    async fn connect(paths: &Paths) -> Self {
        let stream: Stream = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                #[cfg(unix)]
                if let Ok(stream) = tokio::net::UnixStream::connect(paths.socket_path()).await {
                    break Box::new(stream) as Stream;
                }
                #[cfg(windows)]
                if let Ok(stream) =
                    tokio::net::windows::named_pipe::ClientOptions::new().open(paths.pipe_name())
                {
                    break Box::new(stream) as Stream;
                }
                // Wait for listener publication, rather than assuming a startup delay.
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("daemon did not publish its IPC listener");
        let (read, write) = tokio::io::split(stream);
        Self {
            write,
            lines: tokio::io::BufReader::new(read).lines(),
        }
    }
    async fn subscribe(&mut self) {
        let request = RequestEnvelope {
            id: 1,
            protocol: PROTOCOL_VERSION,
            request: Request::Subscribe {
                sessions: None,
                deltas: true,
                client: Some("progress-regression".into()),
            },
        };
        self.write
            .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
            .await
            .unwrap();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(5), self.lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if matches!(
                serde_json::from_str::<ServerMessage>(&line).unwrap(),
                ServerMessage::Response {
                    id: 1,
                    ok: true,
                    ..
                }
            ) {
                break;
            }
        }
    }
    async fn event(&mut self, deadline: tokio::time::Instant) -> Value {
        loop {
            let line = tokio::time::timeout_at(deadline, self.lines.next_line())
                .await
                .expect("expected daemon event before the regression deadline")
                .unwrap()
                .expect("daemon disconnected");
            if let ServerMessage::Event(event) =
                serde_json::from_str::<ServerMessage>(&line).unwrap()
            {
                return serde_json::to_value(event).unwrap();
            }
        }
    }
}
struct Fixture {
    daemon: Arc<Daemon>,
    server: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    root: std::path::PathBuf,
    session: String,
    provider: std::net::TcpListener,
}
impl Fixture {
    async fn new(env: &credential_env::EnvGuard, code: &str) -> (Self, Client) {
        env.set("FORMLESS_API_KEY", "private-progress-fixture");
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");
        let root = std::env::temp_dir().join(format!("fl-progress-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let provider = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        provider.set_nonblocking(true).unwrap();
        let mut config = Config::default();
        config.provider.base_url = format!("http://{}", provider.local_addr().unwrap());
        config.provider.model = "private-unused-provider".into();
        config.naming.enabled = false;
        config.skills.auto_create = false;
        // TOML integers are signed; use the supported foreground stay mode
        // here. The private watcher unit separately covers the MAX sentinel.
        env.set(formless_core::daemon::STAY_ENV, "1");
        config.daemon.idle_grace_secs = 3600;
        config.daemon.stall_warning_secs = 1;
        config.tools.pending_threshold_secs = 0;
        config.security.secure = false;
        config.save(&paths).unwrap();
        let session = {
            let store = Store::open(&paths.db_file()).unwrap();
            let session = store.create_session(None).unwrap().id;
            store
                .create_tool_version(
                    "progress_loop",
                    "private progress regression",
                    r#"{"type":"object"}"#,
                    r#"{"type":"object"}"#,
                    code,
                    &Capabilities::default(),
                    256,
                    "user",
                )
                .unwrap();
            session
        };
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        let server = tokio::spawn({
            let daemon = daemon.clone();
            async move { daemon.serve().await }
        });
        let mut client = Client::connect(&paths).await;
        client.subscribe().await;
        (
            Self {
                daemon,
                server: Some(server),
                root,
                session,
                provider,
            },
            client,
        )
    }
    async fn start(&self, tag: &str) -> String {
        let text = self
            .daemon
            .call_tool_direct(&self.session, "progress_loop", json!({"tag":tag}))
            .await;
        let pending: Value =
            serde_json::from_str(&text).expect("direct call returns a pending handle");
        assert_eq!(pending["status"], "pending", "{text}");
        pending["invocation_id"].as_str().unwrap().into()
    }
    async fn cancel(&self, handle: &str) {
        self.daemon
            .handle(Request::CancelInvocation {
                handle: handle.into(),
            })
            .await
            .unwrap();
    }
    async fn shutdown(&mut self) {
        self.daemon.handle(Request::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), self.server.take().unwrap())
            .await
            .expect("fixture daemon did not stop")
            .unwrap()
            .unwrap();
    }
    fn no_provider_calls(&self) {
        assert!(
            matches!(self.provider.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock),
            "progress fixtures must not contact a provider"
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(server) = &self.server {
            server.abort();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn invocation(event: &Value) -> String {
    event["invocation_id"]
        .as_str()
        .expect("new lifecycle events carry a registry invocation id")
        .into()
}
fn bounded_progress(event: &Value) {
    let fraction = event["fraction"].as_f64().expect("finite numeric progress");
    assert!(fraction.is_finite() && (0.0..=1.0).contains(&fraction));
    let label = event["label"].as_str().unwrap();
    assert!(label.chars().count() <= 120);
    assert!(!label.chars().any(char::is_control));
}

#[tokio::test]
async fn identical_progress_loop_stalls_in_supported_stay_mode() {
    let env = credential_env::EnvGuard::isolated();
    let code="function run(){const until=Date.now()+8000;while(Date.now()<until)progress(0.5,'working');return {}; }";
    let (mut fixture, mut client) = Fixture::new(&env, code).await;
    let handle = fixture.start("single").await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut progress_id = None;
    loop {
        let event = client.event(deadline).await;
        match event["type"].as_str() {
            Some("tool_progress") => {
                bounded_progress(&event);
                assert_eq!(event["fraction"], 0.5);
                progress_id = Some(invocation(&event));
            }
            Some("invocation_stalled") => {
                assert_eq!(event["handle"], handle);
                assert_eq!(event["last_fraction"], 0.5);
                assert_eq!(event["last_label"], "working");
                assert_eq!(Some(invocation(&event)), progress_id);
                break;
            }
            Some("tool_call_finished") => {
                panic!("progress-only loop finished before stall detection: {event}")
            }
            _ => {}
        }
    }
    let listed = fixture
        .daemon
        .handle(Request::ListInvocations)
        .await
        .unwrap();
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["handle"], handle);
    assert_eq!(listed[0]["invocation_id"], progress_id.unwrap());
    assert_eq!(listed[0]["last_fraction"], 0.5);
    assert_eq!(listed[0]["last_label"], "working");
    assert_eq!(listed[0]["stalled"], true);
    let cancelled = Instant::now();
    fixture.cancel(&handle).await;
    loop {
        let event = client.event(deadline).await;
        if event["type"] == "tool_call_finished" {
            assert_eq!(event["ok"], false);
            break;
        }
    }
    assert!(
        cancelled.elapsed() < Duration::from_secs(2),
        "cancellation must interrupt the JS loop promptly"
    );
    fixture.no_provider_calls();
    fixture.shutdown().await;
}

#[tokio::test]
async fn parallel_same_name_progress_is_bounded_and_finish_cancels_only_one_invocation() {
    let env = credential_env::EnvGuard::isolated();
    // Raw bridge bypasses the public shim's clamp and 120-code-unit label slice.
    // Alternating labels and repeated completion fractions must not bypass the cap.
    let code = r#"function run(i){const until=Date.now()+8000;let n=0;while(Date.now()<until)__call('progress',{fraction:2,label:i.tag+'\n\u001b'+(n++%2?'A':'B')+'界'.repeat(160)});return {}; }"#;
    let (mut fixture, mut client) = Fixture::new(&env, code).await;
    let started = Instant::now();
    let first = fixture.start("first").await;
    let second = fixture.start("second").await;
    assert_ne!(first, second);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    let mut ids = BTreeMap::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut starts = BTreeMap::new();
    let first_id = loop {
        let event = client.event(deadline).await;
        match event["type"].as_str() {
            Some("tool_call_started") => {
                assert_eq!(event["tool_name"], "progress_loop");
                assert_eq!(event["depth"], 0);
                let arguments: Value =
                    serde_json::from_str(event["arguments"].as_str().unwrap()).unwrap();
                starts.insert(
                    invocation(&event),
                    arguments["tag"].as_str().unwrap().to_string(),
                );
            }
            Some("tool_progress") => {
                bounded_progress(&event);
                assert_eq!(event["fraction"], 1.0);
                let id = invocation(&event);
                let tag = if event["label"].as_str().unwrap().starts_with("first") {
                    "first"
                } else {
                    "second"
                };
                assert_eq!(starts.get(&id).map(String::as_str), Some(tag));
                ids.insert(tag.to_string(), id.clone());
                *counts.entry(id).or_default() += 1;
            }
            Some("invocation_stalled") if event["handle"] == first => {
                assert_eq!(event["last_fraction"], 1.0);
                let label = event["last_label"].as_str().unwrap();
                assert!(label.starts_with("first"));
                assert!(label.chars().count() <= 120 && !label.chars().any(char::is_control));
                let first_id = invocation(&event);
                fixture.cancel(&first).await;
                break first_id;
            }
            Some("tool_call_finished") => {
                panic!("a progress-only sibling finished before explicit cancellation: {event}")
            }
            _ => {}
        }
    };
    assert_eq!(ids.get("first"), Some(&first_id));
    let second_id = ids
        .get("second")
        .expect("second sibling has its own progress stream")
        .clone();
    assert_ne!(first_id, second_id);
    loop {
        let event = client.event(deadline).await;
        if event["type"] == "tool_call_finished" {
            assert_eq!(invocation(&event), first_id);
            assert_eq!(event["ok"], false);
            break;
        }
    }
    // The sibling must still emit under its original identity after the first
    // finish; shared name/depth state cleanup cannot remove or cancel it.
    loop {
        let event = client.event(deadline).await;
        if event["type"] == "tool_progress" && invocation(&event) == second_id {
            bounded_progress(&event);
            *counts.entry(second_id.clone()).or_default() += 1;
            break;
        }
        assert!(
            event["type"] != "tool_call_finished",
            "second sibling ended without cancellation: {event}"
        );
    }
    let listed = fixture
        .daemon
        .handle(Request::ListInvocations)
        .await
        .unwrap();
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 1, "only the selected sibling was cancelled");
    assert_eq!(listed[0]["handle"], second);
    assert_eq!(listed[0]["invocation_id"], second_id);
    assert_eq!(listed[0]["last_fraction"], 1.0);
    fixture.cancel(&second).await;
    loop {
        let event = client.event(deadline).await;
        if event["type"] == "tool_call_finished" {
            assert_eq!(invocation(&event), second_id);
            assert_eq!(event["ok"], false);
            break;
        }
    }
    let max = 2 + started.elapsed().as_millis() / 200;
    for (id, count) in counts {
        assert!(
            count as u128 <= max,
            "{id} emitted {count} events over {:?}, above the hard five-per-second cap",
            started.elapsed()
        );
    }
    fixture.no_provider_calls();
    fixture.shutdown().await;
}
