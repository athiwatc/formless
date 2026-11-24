#[path = "../support/env.rs"]
mod credential_env;

pub mod scripted_provider;

use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::{
    Event, Request, RequestEnvelope, SendMode, ServerMessage, PROTOCOL_VERSION,
};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use credential_env::EnvGuard;
use scripted_provider::{ProviderCall, ScriptedProvider};

#[cfg(unix)]
type EventTransport = tokio::net::UnixStream;
#[cfg(windows)]
type EventTransport = tokio::net::windows::named_pipe::NamedPipeClient;
type EventReader =
    tokio::io::Lines<tokio::io::BufReader<tokio::io::ReadHalf<EventTransport>>>;

pub struct EventWatcher {
    reader: EventReader,
    writer: tokio::io::WriteHalf<EventTransport>,
    server: tokio::task::JoinHandle<()>,
    daemon: Arc<Daemon>,
}

impl EventWatcher {
    async fn connect(paths: &Paths, daemon: Arc<Daemon>, session: &str) -> Self {
        let server_daemon = daemon.clone();
        let server = tokio::spawn(async move {
            let _ = server_daemon.serve().await;
        });
        let stream = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(stream) = connect_event_transport(paths).await {
                    break stream;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("private IPC listener did not appear");
        let (reader, writer) = tokio::io::split(stream);
        let mut watcher = Self {
            reader: tokio::io::BufReader::new(reader).lines(),
            writer,
            server,
            daemon,
        };
        watcher.subscribe(session).await;
        watcher
    }

    async fn message(&mut self) -> ServerMessage {
        let line = tokio::time::timeout(Duration::from_secs(10), self.reader.next_line())
            .await
            .expect("bounded IPC event missing")
            .unwrap()
            .expect("private daemon disconnected");
        serde_json::from_str(&line).unwrap()
    }

    async fn subscribe(&mut self, session: &str) {
        let request = RequestEnvelope {
            id: 1,
            protocol: PROTOCOL_VERSION,
            request: Request::Subscribe {
                sessions: Some(vec![session.to_owned()]),
                deltas: true,
                client: Some("agent-scenarios".into()),
            },
        };
        self.writer
            .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
            .await
            .unwrap();
        loop {
            match self.message().await {
                ServerMessage::Event(_) => {}
                ServerMessage::Response {
                    id: 1,
                    ok,
                    error,
                    ..
                } => {
                    assert!(ok, "event subscription rejected: {error:?}");
                    return;
                }
                other => panic!("unexpected event subscription reply: {other:?}"),
            }
        }
    }

    pub async fn wait_for_message_delta(&mut self, session: &str, run: &str, expected: &str) {
        let delta = tokio::time::timeout(Duration::from_secs(15), async {
            let mut observed = String::new();
            loop {
                if let ServerMessage::Event(Event::MessageDelta {
                    session_id,
                    run_id,
                    delta,
                }) = self.message().await
                {
                    if session_id == session && run_id == run {
                        observed.push_str(&delta);
                        if observed.contains(expected) {
                            return observed;
                        }
                    }
                }
            }
        })
        .await
        .expect("daemon did not emit the scripted partial message delta");
        assert!(delta.contains(expected));
    }

    pub async fn shutdown(self) {
        let Self {
            reader,
            writer,
            server,
            daemon,
        } = self;
        daemon.request_shutdown();
        drop(reader);
        drop(writer);
        let _ = server.await;
    }
}

#[cfg(unix)]
async fn connect_event_transport(paths: &Paths) -> std::io::Result<EventTransport> {
    tokio::net::UnixStream::connect(paths.socket_path()).await
}

#[cfg(windows)]
async fn connect_event_transport(paths: &Paths) -> std::io::Result<EventTransport> {
    tokio::net::windows::named_pipe::ClientOptions::new().open(paths.pipe_name())
}

pub struct Scenario {
    _env: EnvGuard,
    root: PathBuf,
    paths: Paths,
    pub session: String,
    pub store: Store,
    pub daemon: Arc<Daemon>,
    pub provider: ScriptedProvider,
}

impl Scenario {
    pub async fn new(max_retries: usize) -> Self {
        let env = EnvGuard::isolated();
        env.set("FORMLESS_API_KEY", "offline-scenario-key");
        env.set("NO_PROXY", "127.0.0.1,localhost");
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");

        let provider = ScriptedProvider::start().await;
        let root = std::env::temp_dir().join(format!("formless-scenario-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut config = Config::default();
        config.provider.base_url = provider.base_url().to_owned();
        config.provider.model = "offline-scripted".into();
        config.provider.api_style = "chat".into();
        config.provider.max_retries = max_retries;
        config.provider.fallback_models.clear();
        config.naming.enabled = false;
        config.skills.auto_create = false;
        config.save(&paths).unwrap();

        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap().id;
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        Self {
            _env: env,
            root,
            paths,
            session,
            store,
            daemon,
            provider,
        }
    }

    pub fn new_session(&self) -> String {
        self.store.create_session(None).unwrap().id
    }

    pub async fn watch_events(&self, session: &str) -> EventWatcher {
        EventWatcher::connect(&self.paths, self.daemon.clone(), session).await
    }

    pub async fn send(&self, session: &str, content: &str, mode: SendMode) -> String {
        let result = self
            .daemon
            .handle(Request::SendMessage {
                session_id: session.into(),
                content: content.into(),
                mode,
                client_msg_id: None,
            })
            .await
            .unwrap();
        result["run_id"].as_str().unwrap().to_owned()
    }

    pub async fn next_call(&mut self) -> ProviderCall {
        self.provider.next_call().await
    }

    pub async fn finish(&self, session: &str, run_id: &str) -> Value {
        let state = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let state = self
                    .daemon
                    .handle(Request::GetSession { id: session.into() })
                    .await
                    .unwrap();
                if state["running"] == false {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("scripted Run did not finish");
        assert_eq!(state["running"], false, "Run should be stopped");
        let status = self.store.run_status(run_id).unwrap();
        assert_eq!(
            status.as_deref(),
            Some("completed"),
            "Run should complete successfully"
        );
        state
    }

    pub async fn wait_for_checkpoint(&self, session: &str) -> (String, i64) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(checkpoint) = self.store.latest_checkpoint(session).unwrap() {
                    return checkpoint;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("scripted compaction did not save a checkpoint")
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
