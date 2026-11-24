#[path = "../support/env.rs"]
mod credential_env;

pub mod scripted_provider;

use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::{Request, SendMode};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, time::Duration};

use credential_env::EnvGuard;
use scripted_provider::{ProviderCall, ScriptedProvider};

pub struct Scenario {
    _env: EnvGuard,
    root: PathBuf,
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
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        Self {
            _env: env,
            root,
            session,
            store,
            daemon,
            provider,
        }
    }

    pub fn new_session(&self) -> String {
        self.store.create_session(None).unwrap().id
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
