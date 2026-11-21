use super::Daemon;
use crate::{
    config::{Config, ProviderCfg, DEFAULT_PROVIDER},
    probe_policy::{azure_deployment, same_model},
    Paths,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use formless_protocol::Event;
use formless_runtime::LockExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

const PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
const LIST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Selection {
    provider: String,
    model: String,
    where_configured: BTreeSet<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    selection: Selection,
    endpoint_key: String,
    misses: u8,
    flagged: bool,
    dismissed: bool,
}

#[derive(Serialize, Deserialize)]
struct Skipped {
    provider: String,
    reason: String,
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct State {
    checked_at: Option<String>,
    entries: Vec<Entry>,
    skipped: Vec<Skipped>,
}

type Wanted = BTreeMap<String, (String, BTreeMap<String, BTreeSet<String>>)>;
type Observations = BTreeMap<String, std::result::Result<Vec<String>, &'static str>>;

fn wanted(cfg: &Config) -> Wanted {
    let mut wanted = Wanted::new();
    let mut add = |provider: &str, pc: &ProviderCfg, model: &str, location: &str| {
        let model = pc.resolve_alias(model);
        if model.is_empty() || ProviderCfg::is_auto_model(&model) || model.starts_with("role:") {
            return;
        }
        let (_, models) = wanted.entry(provider.into()).or_insert_with(|| {
            (
                crate::forecast::hash(&(&pc.base_url, &pc.api_style, &pc.query_params)),
                BTreeMap::new(),
            )
        });
        models.entry(model).or_default().insert(location.into());
    };
    for (provider, pc) in std::iter::once((DEFAULT_PROVIDER, &cfg.provider))
        .chain(cfg.providers.iter().map(|(name, pc)| (name.as_str(), pc)))
    {
        add(provider, pc, &pc.model, "default");
        for (location, candidates) in [
            ("fallback", &pc.fallback_models),
            ("routing", &pc.routing_models),
            ("fast routing", &pc.routing_fast_models),
            ("strong routing", &pc.routing_strong_models),
        ] {
            for model in candidates {
                add(provider, pc, model, location);
            }
        }
    }
    for (name, role) in &cfg.roles {
        let provider = role.provider.as_deref().unwrap_or(DEFAULT_PROVIDER);
        let Ok(pc) = cfg.provider_cfg(Some(provider)) else {
            continue;
        };
        for model in std::iter::once(&role.model).chain(&role.candidates) {
            add(provider, &pc, model, &format!("Role {name}"));
        }
    }
    wanted
}

fn state_file(paths: &Paths) -> PathBuf {
    paths.data_dir.join("model_listing_health.json")
}

impl State {
    pub(super) fn load(paths: &Paths) -> Result<Self> {
        let file = state_file(paths);
        match fs::symlink_metadata(&file) {
            Ok(meta) if meta.is_file() => Ok(serde_json::from_slice(&fs::read(file)?)?),
            Ok(_) => anyhow::bail!("model listing state must be a regular file"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        let file = state_file(paths);
        crate::config::write_secret_file(&file, &serde_json::to_string(self)?)
            .context("save model listing warnings")?;
        #[cfg(unix)]
        fs::File::open(&paths.data_dir)?.sync_all()?;
        Ok(())
    }

    fn until_due(&self, now: DateTime<Utc>) -> Duration {
        self.checked_at
            .as_deref()
            .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
            .map_or(Duration::ZERO, |last| {
                PERIOD.saturating_sub(
                    (now - last.with_timezone(&Utc))
                        .to_std()
                        .unwrap_or_default(),
                )
            })
    }

    pub(super) fn warnings(&self) -> Value {
        json!(self.active().collect::<Vec<_>>())
    }

    fn active(&self) -> impl Iterator<Item = &Selection> {
        self.entries
            .iter()
            .filter(|entry| entry.flagged && !entry.dismissed)
            .map(|entry| &entry.selection)
    }

    pub(super) fn report(&self) -> (Value, Option<String>, Value) {
        (
            self.warnings(),
            self.checked_at.clone(),
            json!(self.skipped),
        )
    }

    fn advance(&self, desired: Wanted, mut observations: Observations, now: DateTime<Utc>) -> Self {
        let previous: BTreeMap<_, _> = self
            .entries
            .iter()
            .map(|entry| {
                (
                    (
                        entry.selection.provider.as_str(),
                        entry.selection.model.as_str(),
                    ),
                    entry,
                )
            })
            .collect();
        let mut entries = Vec::new();
        let mut skipped = Vec::new();
        for (provider, (endpoint_key, models)) in desired {
            let observed = observations
                .remove(&provider)
                .unwrap_or(Err("listing unavailable"));
            if let Err(reason) = observed {
                skipped.push(Skipped {
                    provider: provider.clone(),
                    reason: reason.into(),
                });
            }
            for (model, where_configured) in models {
                let prior = previous
                    .get(&(provider.as_str(), model.as_str()))
                    .filter(|entry| entry.endpoint_key == endpoint_key);
                let was_flagged = prior.is_some_and(|entry| entry.flagged);
                let dismissed = prior.is_some_and(|entry| entry.dismissed);
                let (misses, flagged) = match &observed {
                    Ok(list)
                        if list
                            .iter()
                            .any(|id| same_model(&model, id) || same_model(id, &model)) =>
                    {
                        (0, false)
                    }
                    Ok(_) => {
                        let misses = prior.map_or(1, |entry| entry.misses.saturating_add(1).min(2));
                        (misses, was_flagged || misses == 2)
                    }
                    Err(_) => (0, was_flagged),
                };
                entries.push(Entry {
                    selection: Selection {
                        provider: provider.clone(),
                        model,
                        where_configured,
                    },
                    endpoint_key: endpoint_key.clone(),
                    misses,
                    flagged,
                    dismissed,
                });
            }
        }
        Self {
            checked_at: Some(now.to_rfc3339()),
            entries,
            skipped,
        }
    }
}

impl Daemon {
    pub(super) async fn check_models_loop(self: Arc<Self>) {
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            let delay = self.model_listing_health.locked().until_due(Utc::now());
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
                continue;
            }
            if let Ok(cfg) = self.config() {
                if let Err(error) = self.check_model_listings(&cfg, Utc::now()).await {
                    tracing::warn!("model listing check could not save its result: {error:#}");
                }
            }
            tokio::time::sleep(PERIOD).await;
        }
    }

    async fn check_model_listings(&self, cfg: &Config, now: DateTime<Utc>) -> Result<()> {
        if !self.model_listing_health.locked().until_due(now).is_zero() {
            return Ok(());
        }
        let desired = wanted(cfg);
        let mut observations = Observations::new();
        for provider in desired.keys() {
            let pc = cfg.provider_cfg(Some(provider))?;
            let listing = if azure_deployment(&pc.base_url).is_some() {
                Err("Azure deployment URL does not identify an account model listing")
            } else {
                let fetch = async {
                    self.provider_named(cfg, Some(provider))
                        .await?
                        .models_for_health_check()
                        .await
                };
                match tokio::time::timeout(LIST_TIMEOUT, fetch).await {
                    Ok(Ok(Some(models))) => Ok(models),
                    Ok(Ok(None)) => Err("listing was empty or incomplete"),
                    _ => Err("listing unavailable"),
                }
            };
            observations.insert(provider.clone(), listing);
        }
        let newly_missing = {
            let mut state = self.model_listing_health.locked();
            let next = state.advance(desired, observations, now);
            next.save(&self.paths)?;
            let existing: BTreeSet<_> = state
                .active()
                .map(|selection| (&selection.provider, &selection.model))
                .collect();
            let newly_missing = next
                .active()
                .filter(|s| !existing.contains(&(&s.provider, &s.model)))
                .map(|s| {
                    format!(
                        "{} at {} ({})",
                        s.model,
                        s.provider,
                        s.where_configured
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>();
            *state = next;
            newly_missing
        };
        if !newly_missing.is_empty() {
            self.broadcast(Event::Note {
                session_id: None,
                text: format!("Missing from two model listings: {}. Pick another model with /model. Use `formless providers dismiss-warning PROVIDER MODEL` to hide an individual warning.", newly_missing.join(" · ")),
            });
        }
        Ok(())
    }

    pub(super) fn dismiss_missing_model(&self, provider: &str, model: &str) -> Result<Value> {
        let mut state = self.model_listing_health.locked();
        let Some(index) = state.entries.iter().position(|entry| {
            entry.flagged
                && !entry.dismissed
                && entry.selection.provider == provider
                && entry.selection.model == model
        }) else {
            return Ok(json!({"dismissed":false}));
        };
        state.entries[index].dismissed = true;
        if let Err(error) = state.save(&self.paths) {
            state.entries[index].dismissed = false;
            return Err(error);
        }
        Ok(json!({"dismissed":true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State as AxumState, response::IntoResponse, Json};
    use formless_protocol::Request;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Fixture {
        calls: AtomicUsize,
        phase: AtomicUsize,
    }

    async fn default_listing(AxumState(fixture): AxumState<Arc<Fixture>>) -> Json<Value> {
        fixture.calls.fetch_add(1, Ordering::SeqCst);
        Json(json!({"data":[{"id":"gpt-4o-2024-08-06"}]}))
    }

    async fn other_listing(AxumState(fixture): AxumState<Arc<Fixture>>) -> impl IntoResponse {
        fixture.calls.fetch_add(1, Ordering::SeqCst);
        let partial = fixture.phase.load(Ordering::SeqCst) == 0;
        let mut response = Json(json!({"data":[{"id":"available"}]})).into_response();
        if partial {
            response.headers_mut().insert(
                "link",
                "</other/models?page=2>; rel=\"next\"".parse().unwrap(),
            );
        }
        response
    }

    #[tokio::test]
    async fn warnings_require_complete_listings_and_survive_restart_and_dismissal() {
        let env = crate::credential_env::EnvGuard::isolated();
        env.set("NO_PROXY", "127.0.0.1,localhost");
        env.set("FORMLESS_API_KEY", "loopback-default");
        env.set("FORMLESS_API_KEY_OTHER", "loopback-named");
        env.remove("FORMLESS_MODEL");
        let fixture = Arc::new(Fixture::default());
        let server = axum::Router::new()
            .route("/main/models", axum::routing::get(default_listing))
            .route("/other/models", axum::routing::get(other_listing))
            .with_state(fixture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
        env.set("FORMLESS_BASE_URL", format!("{base}/main"));
        let root = std::env::temp_dir().join(format!("fl-model-listing-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = format!("{base}/main");
        cfg.provider.model = "gpt-4o".into();
        cfg.provider.fallback_models = vec!["shared".into()];
        let named = ProviderCfg {
            base_url: format!("{base}/other"),
            model: "shared".into(),
            ..ProviderCfg::default()
        };
        cfg.providers.insert("other".into(), named);
        let azure = ProviderCfg {
            base_url: format!("{base}/openai/deployments/example"),
            model: "example".into(),
            ..ProviderCfg::default()
        };
        cfg.providers.insert("azure".into(), azure);
        cfg.roles.insert(
            "review".into(),
            crate::config::RoleCfg {
                provider: Some("other".into()),
                model: "shared".into(),
                ..Default::default()
            },
        );
        cfg.save(&paths).unwrap();

        let first_at = Utc::now() - chrono::Duration::days(4);
        let day = chrono::Duration::days(1);
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        daemon.check_model_listings(&cfg, first_at).await.unwrap();
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        let first = daemon.handle(Request::Status).await.unwrap();
        assert_eq!(first["missing_models"], json!([]));
        assert_eq!(
            first["model_listing_checks_skipped"][0]["provider"],
            "azure"
        );
        assert_eq!(
            first["model_listing_checks_skipped"][1]["provider"],
            "other"
        );
        assert_eq!(
            first["model_listing_checks_skipped"][1]["reason"],
            "listing was empty or incomplete"
        );
        drop(daemon);

        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        daemon
            .check_model_listings(&cfg, first_at + chrono::Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            daemon.handle(Request::Status).await.unwrap()["model_listing_checked_at"],
            first["model_listing_checked_at"]
        );
        fixture.phase.store(1, Ordering::SeqCst);
        daemon
            .check_model_listings(&cfg, first_at + day)
            .await
            .unwrap();
        assert_eq!(
            daemon.handle(Request::Status).await.unwrap()["missing_models"],
            json!([
                {"provider":"default", "model":"shared", "where_configured":["fallback"]}
            ])
        );
        daemon
            .check_model_listings(&cfg, first_at + day * 2)
            .await
            .unwrap();
        assert_eq!(
            daemon.handle(Request::Status).await.unwrap()["missing_models"],
            json!([
                {"provider":"default", "model":"shared", "where_configured":["fallback"]},
                {"provider":"other", "model":"shared", "where_configured":["Role review", "default"]}
            ])
        );
        assert_eq!(
            daemon
                .handle(Request::DismissMissingModel {
                    provider: "other".into(),
                    model: "shared".into()
                })
                .await
                .unwrap()["dismissed"],
            true
        );
        drop(daemon);
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        daemon
            .check_model_listings(&cfg, first_at + day * 3)
            .await
            .unwrap();
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 8);
        assert_eq!(
            daemon.handle(Request::Status).await.unwrap()["missing_models"],
            json!([
                {"provider":"default", "model":"shared", "where_configured":["fallback"]}
            ])
        );
        drop(daemon);
        server.abort();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unknown_listing_breaks_a_miss_streak_and_repointing_resets_it() {
        let mut cfg = Config::default();
        cfg.provider.model = "target".into();
        let now = Utc::now();
        let missing = || Observations::from([(DEFAULT_PROVIDER.into(), Ok(vec!["other".into()]))]);
        let skipped = Observations::from([(DEFAULT_PROVIDER.into(), Err("partial"))]);
        let state = State::default().advance(wanted(&cfg), missing(), now);
        let state = state.advance(wanted(&cfg), skipped, now);
        let state = state.advance(wanted(&cfg), missing(), now);
        assert_eq!(state.warnings(), json!([]));
        let state = state.advance(wanted(&cfg), missing(), now);
        assert_eq!(state.active().count(), 1);
        cfg.provider.base_url = "https://different.example/v1".into();
        let state = state.advance(wanted(&cfg), missing(), now);
        assert_eq!(state.warnings(), json!([]));
    }
}
