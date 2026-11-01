//! Provider selection, evidenced issuer compatibility and request-wide recovery.
//! Only loopback mock HTTP and a mock keyring backend are used.
#[path = "support/env.rs"]
mod credential_env;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use formless_core::{
    auth::{self, OAuthTokens},
    config::{Config, ProviderCfg},
    provider::{self, Provider},
    Paths,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Once},
    time::Duration,
};

// keyring::mock entries are entry-local. Share only private in-memory mock
// credentials so lookup through a newly constructed Entry really exercises
// keyring-only recovery; never delegate to an operating-system backend.
#[derive(Default)]
struct SharedMockBuilder(
    Mutex<HashMap<(Option<String>, String, String), Arc<keyring::mock::MockCredential>>>,
);
struct SharedMockCredential(Arc<keyring::mock::MockCredential>);
impl keyring::credential::CredentialApi for SharedMockCredential {
    fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
        keyring::credential::CredentialApi::set_secret(self.0.as_ref(), secret)
    }
    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        keyring::credential::CredentialApi::get_secret(self.0.as_ref())
    }
    fn delete_credential(&self) -> keyring::Result<()> {
        keyring::credential::CredentialApi::delete_credential(self.0.as_ref())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
impl keyring::credential::CredentialBuilderApi for SharedMockBuilder {
    fn build(
        &self,
        target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<keyring::credential::Credential>> {
        let credential = self
            .0
            .lock()
            .unwrap()
            .entry((target.map(str::to_string), service.into(), user.into()))
            .or_default()
            .clone();
        Ok(Box::new(SharedMockCredential(credential)))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn persistence(&self) -> keyring::credential::CredentialPersistence {
        keyring::credential::CredentialPersistence::UntilDelete
    }
}

struct Fixture {
    paths: Paths,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new(_guard: &credential_env::EnvGuard) -> Self {
        _guard.set("NO_PROXY", "127.0.0.1,localhost");
        static MOCK: Once = Once::new();
        MOCK.call_once(|| {
            keyring::set_default_credential_builder(Box::<SharedMockBuilder>::default())
        });
        let root = std::env::temp_dir().join(format!("fl-oauth-recovery-{}", uuid::Uuid::new_v4()));
        let paths = Paths {
            data_dir: root.join("data"),
            config_dir: root.join("config"),
            profile: uuid::Uuid::new_v4().to_string(),
        };
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        Self { paths, root }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn config(base: &str) -> ProviderCfg {
    let mut cfg = ProviderCfg {
        base_url: format!("{base}/v1"),
        model: "primary".into(),
        max_retries: 0,
        ..Default::default()
    };
    cfg.auth.mode = "oauth".into();
    cfg.auth.token_url = format!("{base}/token");
    cfg.auth.authorize_url = format!("{base}/authorize");
    cfg.auth.client_id = "client".into();
    cfg
}
fn tokens() -> OAuthTokens {
    OAuthTokens {
        access_token: "old-access".into(),
        refresh_token: Some("private-refresh".into()),
        expires_at: Some(auth::now_epoch() + 3600),
        token_type: "Bearer".into(),
        scope: None,
    }
}
fn write_old_slot(paths: &Paths, cfg: &ProviderCfg, issuer: Option<&str>) -> std::path::PathBuf {
    let hash = Sha256::digest(format!("{}\n{}", cfg.auth.token_url, cfg.auth.client_id));
    let id: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
    let file = paths.config_dir.join(format!("oauth_tokens-{id}.json"));
    let mut value = serde_json::to_value(tokens()).unwrap();
    if let Some(issuer) = issuer {
        value["token_url"] = json!(issuer);
    }
    formless_core::config::write_secret_file(&file, &value.to_string()).unwrap();
    file
}

#[test]
fn endpoint_spelling_reuses_versioned_login_without_adopting_legacy_labels() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let mut old = config("https://EXAMPLE.com:443");
    old.auth.token_url = "HTTPS://EXAMPLE.com:443/Token/".into();
    let file = write_old_slot(&f.paths, &old, Some(&old.auth.token_url));
    let mut new = old.clone();
    new.auth.token_url = "https://example.com/Token".into();
    assert_eq!(
        auth::canonical_token_url(&old.auth.token_url).unwrap(),
        auth::canonical_token_url(&new.auth.token_url).unwrap()
    );
    assert!(
        auth::load_oauth_tokens(&f.paths, &new.auth).is_none(),
        "legacy issuer/hash could have been stamped by config-derived migration"
    );
    assert!(
        file.exists(),
        "diagnostics/compatibility lookup never rewrite or delete evidence"
    );
    auth::save_oauth_tokens(&f.paths, &old.auth, &tokens()).unwrap();
    assert_eq!(auth::load_oauth_tokens(&f.paths, &new.auth), Some(tokens()));
    assert_eq!(
        auth::oauth_tokens_file(&f.paths, &old.auth),
        auth::oauth_tokens_file(&f.paths, &new.auth)
    );
    let mut different = new.clone();
    different.auth.token_url = "https://example.com/token".into();
    assert!(
        auth::load_oauth_tokens(&f.paths, &different.auth).is_none(),
        "path case remains meaningful"
    );
    different = new.clone();
    different.auth.client_id = "other-client".into();
    assert!(auth::load_oauth_tokens(&f.paths, &different.auth).is_none());
    auth::delete_oauth_tokens(&f.paths, &new.auth);
    assert!(
        !file.exists(),
        "logout also removes evidenced raw-slot aliases"
    );
    assert!(auth::load_oauth_tokens(&f.paths, &old.auth).is_none());
}

#[test]
fn legacy_without_issuer_and_wrong_issuer_are_never_adopted_by_new_default() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let cfg = config("https://new.example");
    let mut saved = Config::default();
    saved.provider = cfg.clone();
    saved.save(&f.paths).unwrap();
    for issuer in [None, Some("https://old.example/token")] {
        let mut value = serde_json::to_value(tokens()).unwrap();
        if let Some(i) = issuer {
            value["token_url"] = json!(i);
        }
        formless_core::config::write_secret_file(&f.paths.oauth_tokens_file(), &value.to_string())
            .unwrap();
        let before = std::fs::read(f.paths.oauth_tokens_file()).unwrap();
        assert!(auth::load_oauth_tokens(&f.paths, &cfg.auth).is_none());
        assert!(!auth::has_provider_credential(&f.paths, &cfg));
        let status = auth::auth_status(&f.paths, &cfg);
        assert!(!status.credential_present);
        assert!(status.warning.unwrap().contains("proven matching issuer"));
        assert_eq!(before, std::fs::read(f.paths.oauth_tokens_file()).unwrap());
        assert!(!auth::oauth_tokens_file(&f.paths, &cfg.auth).exists());
    }
    let mut value = serde_json::to_value(tokens()).unwrap();
    value["token_url"] = json!("https://NEW.example:443/token/");
    formless_core::config::write_secret_file(&f.paths.oauth_tokens_file(), &value.to_string())
        .unwrap();
    assert!(
        auth::load_oauth_tokens(&f.paths, &cfg.auth).is_none(),
        "profile-wide legacy records do not prove client/transport binding"
    );
}

#[tokio::test]
async fn mismatched_or_missing_issuer_blocks_even_unexpired_access_tokens() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let cfg = config("http://127.0.0.1:9");
    for issuer in [None, Some("https://wrong.example/token")] {
        write_old_slot(&f.paths, &cfg, issuer);
        let error = auth::resolve_provider_token(&f.paths, &cfg)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("login required"));
        assert!(auth::peek_provider_token(&f.paths, &cfg).is_err());
    }
}

#[derive(Default)]
struct Seen {
    refreshes: usize,
    calls: Vec<(String, String, String)>,
    fail_refresh: bool,
    echo_credentials: bool,
    echo_query: bool,
    accept_any_credential: bool,
    reject_forced_choice: bool,
    forced_rejections: usize,
    fallback_ok: bool,
    reject_new: bool,
    pages: bool,
    fallback_status: Option<u16>,
    drop_stream_once: bool,
    stream_delivered: bool,
    stream_drop_gate: Option<tokio::sync::oneshot::Receiver<()>>,
    expected_token_path: Option<String>,
    token_forms: Vec<String>,
    allow_rotated_refresh: bool,
    redirect_refresh: bool,
}
struct Server {
    base: String,
    seen: Arc<Mutex<Seen>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
async fn endpoint(
    State(seen): State<Arc<Mutex<Seen>>>,
    headers: HeaderMap,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    body: axum::body::Bytes,
) -> Response {
    let path = uri.path().to_string();
    let credential = headers
        .get("authorization")
        .or_else(|| headers.get("x-api-key"))
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let payload: Value = serde_json::from_slice(&body).unwrap_or_default();
    let mut seen = seen.lock().unwrap();
    seen.calls.push((
        uri.to_string(),
        credential.clone(),
        payload["model"].as_str().unwrap_or_default().into(),
    ));
    if path.trim_end_matches('/') == "/token" {
        seen.refreshes += 1;
        let body = String::from_utf8_lossy(&body);
        seen.token_forms.push(body.to_string());
        if seen
            .expected_token_path
            .as_deref()
            .is_some_and(|expected| expected != path)
        {
            return (
                StatusCode::FORBIDDEN,
                "unproven alias must not receive credentials",
            )
                .into_response();
        }
        if seen.redirect_refresh {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [(axum::http::header::LOCATION, "/trap")],
                Json(json!({"error":"redirect_refused"})),
            )
                .into_response();
        }
        assert!(
            body.contains("refresh_token=private-refresh")
                || ((seen.drop_stream_once || seen.allow_rotated_refresh)
                    && body.contains("refresh_token=rotated-refresh"))
        );
        return if seen.fail_refresh {
            let description = if seen.echo_query {
                let url = reqwest::Url::parse(&format!("http://private.fixture{uri}")).unwrap();
                let value = url.query_pairs().next().unwrap().1;
                let raw = uri.query().unwrap().split_once('=').unwrap().1;
                let percent: String = value
                    .bytes()
                    .map(|byte| {
                        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                            (byte as char).to_string()
                        } else {
                            format!("%{byte:02X}")
                        }
                    })
                    .collect();
                format!(
                    "opaque query [{value}] [{raw}] [{percent}] {}",
                    "X".repeat(350)
                )
            } else if seen.echo_credentials {
                use base64::Engine;
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(credential.trim_start_matches("Basic "))
                    .unwrap();
                let basic = String::from_utf8(decoded).unwrap();
                let secret = basic.split_once(':').unwrap().1;
                format!("rejected opaque material [private-refresh] [{secret}] [{credential}]")
            } else {
                "refresh refused".into()
            };
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant", "error_description": description})),
            )
                .into_response()
        } else {
            Json(json!({"access_token":"new-access","refresh_token":"rotated-refresh","expires_in":3600})).into_response()
        };
    }
    if payload["model"] == "fallback" {
        if let Some(status) = seen.fallback_status {
            return (
                StatusCode::from_u16(status).unwrap(),
                "fallback policy rejection",
            )
                .into_response();
        }
    }
    if seen.drop_stream_once && seen.stream_delivered && payload["stream"] == true {
        return (
            StatusCode::UNAUTHORIZED,
            "second revoked credential after stream drop",
        )
            .into_response();
    }
    if seen.reject_forced_choice && payload.get("tool_choice").is_some() {
        seen.forced_rejections += 1;
        return (
            StatusCode::BAD_REQUEST,
            "Thinking mode does not support this tool_choice",
        )
            .into_response();
    }
    let good = seen.accept_any_credential
        || (credential.ends_with("new-access") && !seen.reject_new)
        || (seen.fallback_ok && payload["model"] == "fallback");
    if !good {
        let detail = if seen.echo_credentials {
            format!(
                "original revoked credential; echoed opaque value [{}]",
                credential.trim_start_matches("Bearer ")
            )
        } else {
            "original revoked credential".into()
        };
        return (StatusCode::UNAUTHORIZED, detail).into_response();
    }
    if seen.drop_stream_once && payload["stream"] == true {
        seen.stream_delivered = true;
        let gate = seen.stream_drop_gate.take().expect("private drop gate");
        // Deliver response headers and an SSE comment first. The test's Bytes
        // callback acknowledges consuming that body chunk before releasing the
        // transport error, proving this retry occurs after post_provider returns.
        let stream =
            futures_util::stream::unfold((false, Some(gate)), |(emitted, gate)| async move {
                if !emitted {
                    Some((
                        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(
                            b": headers ready\n\n",
                        )),
                        (true, gate),
                    ))
                } else {
                    let gate = gate?;
                    let _ = gate.await;
                    Some((
                        Err(std::io::Error::other("intentional withheld stream drop")),
                        (true, None),
                    ))
                }
            });
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(axum::body::Body::from_stream(stream))
            .unwrap();
    }
    if path.ends_with("/models") {
        let more = seen.pages && uri.query().is_none_or(|q| !q.contains("after_id"));
        return Json(json!({"data":[{"id":if more {"page-one"} else {"page-two"}}],"has_more":more,"last_id":"page-one"})).into_response();
    }
    if path.ends_with("/compact") {
        return Json(json!({"summary":"compacted"})).into_response();
    }
    if seen.reject_forced_choice && payload.get("tools").is_some() {
        return Json(json!({"choices":[{"message":{"content":null,"tool_calls":[{"id":"probe","type":"function","function":{"name":"probe_ping","arguments":"{}"}}]}}]})).into_response();
    }
    Json(json!({"choices":[{"message":{"content":"fallback result"}}]})).into_response()
}
impl Server {
    async fn new(options: Seen) -> Self {
        let seen = Arc::new(Mutex::new(options));
        let app = Router::new()
            .route("/token", post(endpoint))
            .route("/token/", post(endpoint))
            .route("/trap", post(endpoint))
            .route("/v1/models", get(endpoint))
            .route("/v1/chat/completions", post(endpoint))
            .route("/v1/responses", post(endpoint))
            .route("/v1/messages", post(endpoint))
            .route("/v1/compact", post(endpoint))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (stop, done) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = done.await;
                })
                .await
                .unwrap();
        });
        Self {
            base,
            seen,
            stop: Some(stop),
            task: Some(task),
        }
    }
    async fn close(mut self) {
        let _ = self.stop.take().unwrap().send(());
        tokio::time::timeout(Duration::from_secs(3), self.task.take().unwrap())
            .await
            .expect("mock shutdown timeout")
            .unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
async fn managed(f: &Fixture, cfg: ProviderCfg, name: &str) -> Provider {
    auth::save_oauth_tokens(&f.paths, &cfg.auth, &tokens()).unwrap();
    Provider::for_named_profile_with(cfg, f.paths.clone(), reqwest::Client::new(), name)
        .await
        .unwrap()
}
async fn complete(p: &Provider) -> anyhow::Result<String> {
    tokio::time::timeout(
        Duration::from_secs(3),
        p.complete(&[json!({"role":"user","content":"hi"})], "primary", None),
    )
    .await
    .expect("request timeout")
}

#[tokio::test]
async fn models_401_recovers_once_across_all_pages() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen {
        pages: true,
        ..Default::default()
    })
    .await;
    let mut cfg = config(&server.base);
    cfg.api_style = "anthropic".into();
    let p = managed(&f, cfg, "secondary").await;
    let models = tokio::time::timeout(Duration::from_secs(3), p.list_models())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(models.len(), 2);
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.refreshes, 1);
    assert_eq!(
        seen.calls
            .iter()
            .filter(|(u, _, _)| u.starts_with("/v1/models"))
            .count(),
        3
    );
    drop(seen);
    server.close().await;
}

#[tokio::test]
async fn compact_401_recovers_and_persists_rotation() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen::default()).await;
    let cfg = config(&server.base);
    let p = managed(&f, cfg.clone(), "secondary").await;
    let out = tokio::time::timeout(Duration::from_secs(3), p.compact_server("input", "primary"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out, "compacted");
    assert_eq!(server.seen.lock().unwrap().refreshes, 1);
    assert_eq!(
        auth::load_oauth_tokens(&f.paths, &cfg.auth)
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("rotated-refresh")
    );
    server.close().await;
}

#[tokio::test]
async fn failed_refresh_does_not_skip_successful_fallback() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen {
        fail_refresh: true,
        fallback_ok: true,
        ..Default::default()
    })
    .await;
    let mut cfg = config(&server.base);
    cfg.fallback_models = vec!["fallback".into()];
    let p = managed(&f, cfg, "secondary").await;
    assert_eq!(complete(&p).await.unwrap(), "fallback result");
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.refreshes, 1);
    assert!(seen.calls.iter().any(|(_, _, m)| m == "fallback"));
    drop(seen);
    server.close().await;
}

#[tokio::test]
async fn failed_refresh_and_exhausted_fallback_retain_original_401_context() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen {
        fail_refresh: true,
        ..Default::default()
    })
    .await;
    let mut cfg = config(&server.base);
    cfg.fallback_models = vec!["fallback".into(), "third".into()];
    let p = managed(&f, cfg, "secondary").await;
    let error = format!("{:#}", complete(&p).await.unwrap_err());
    assert!(error.contains("401"));
    assert!(error.contains("original revoked credential"));
    assert!(error.contains("invalid_grant"));
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.refreshes, 1);
    assert_eq!(
        seen.calls
            .iter()
            .filter(|(u, _, _)| u.ends_with("chat/completions"))
            .count(),
        3
    );
    drop(seen);
    server.close().await;
}

#[tokio::test]
async fn repeated_401_after_successful_refresh_never_refreshes_per_fallback_model() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen {
        reject_new: true,
        ..Default::default()
    })
    .await;
    let mut cfg = config(&server.base);
    cfg.fallback_models = vec!["fallback".into(), "third".into()];
    let p = managed(&f, cfg, "secondary").await;
    assert!(complete(&p).await.is_err());
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.refreshes, 1);
    assert_eq!(
        seen.calls
            .iter()
            .filter(|(u, _, _)| u.ends_with("chat/completions"))
            .count(),
        4
    );
    drop(seen);
    server.close().await;
}

#[tokio::test]
async fn named_oauth_guidance_is_exact_and_named_api_keys_remain_separate() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen::default()).await;
    let cfg = config(&server.base);
    let error = match Provider::for_named_profile_with(
        cfg.clone(),
        f.paths.clone(),
        reqwest::Client::new(),
        "work",
    )
    .await
    {
        Ok(_) => panic!("missing token accepted"),
        Err(e) => format!("{e:#}"),
    };
    assert!(error.contains("provider 'work'"));
    assert!(error.contains("formless auth login --provider work"));
    env.set("FORMLESS_API_KEY_WORK", "new-access");
    env.set("FORMLESS_API_KEY", "wrong-default");
    let mut api = cfg;
    api.auth.mode = "api_key".into();
    let p = Provider::for_named_profile_with(api, f.paths.clone(), reqwest::Client::new(), "work")
        .await
        .unwrap();
    assert_eq!(complete(&p).await.unwrap(), "fallback result");
    assert_eq!(server.seen.lock().unwrap().refreshes, 0);
    server.close().await;
}

#[tokio::test]
async fn environment_override_is_not_refreshed_or_replaced_by_stored_tokens() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen::default()).await;
    env.set("FORMLESS_OAUTH_ACCESS_TOKEN", "explicit-override");
    let p = managed(&f, config(&server.base), "work").await;
    assert!(complete(&p).await.is_err());
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.refreshes, 0);
    assert!(seen
        .calls
        .iter()
        .all(|(_, c, _)| c.ends_with("explicit-override")));
    drop(seen);
    server.close().await;
}

#[tokio::test]
async fn daemon_test_provider_uses_named_oauth_and_logout_preserves_other_slot() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen::default()).await;
    let named = config(&server.base);
    let mut cfg = Config::default();
    cfg.provider = config("https://default.example");
    cfg.naming.enabled = false;
    cfg.providers.insert("work".into(), named.clone());
    cfg.save(&f.paths).unwrap();
    auth::save_oauth_tokens(&f.paths, &cfg.provider.auth, &tokens()).unwrap();
    let _p = managed(&f, named.clone(), "work").await;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(f.paths.clone()).unwrap());
    let listing = tokio::time::timeout(
        Duration::from_secs(5),
        daemon.handle(formless_protocol::Request::TestProvider {
            name: "work".into(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(listing["provider"], "work");
    assert!(listing["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "page-two"));
    assert_eq!(server.seen.lock().unwrap().refreshes, 1);
    let lines = auth::logout_provider(&f.paths, &named).join("\n");
    assert!(lines.contains("Deleted the stored OAuth tokens"));
    assert!(!auth::auth_status(&f.paths, &named).credential_present);
    assert!(auth::auth_status(&f.paths, &cfg.provider).credential_present);
    let weak = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(Duration::from_secs(5), async {
        while weak.strong_count() > 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("daemon tasks outlived credential guard");
    server.close().await;
}

#[tokio::test]
async fn managed_operational_probe_recovers_revoked_token_in_models_stage() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen::default()).await;
    let p = managed(&f, config(&server.base), "work").await;
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        provider::probe_provider(&p, "primary"),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.models.iter().any(|m| m == "page-two"));
    assert_eq!(server.seen.lock().unwrap().refreshes, 1);
    server.close().await;
}

#[tokio::test]
async fn equivalent_versioned_endpoints_share_refresh_lock_without_rewriting_legacy_bytes() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen {
        expected_token_path: Some("/token/".into()),
        ..Default::default()
    })
    .await;
    let cfg = config(&server.base);
    let mut spelling = cfg.clone();
    spelling.auth.token_url.push('/');
    let old_file = write_old_slot(&f.paths, &spelling, Some(&spelling.auth.token_url));
    let old_bytes = std::fs::read(&old_file).unwrap();
    // Explicit re-login supplies independent exact-endpoint evidence; the old
    // issuer/hash pair alone must never bootstrap a trusted credential.
    auth::save_oauth_tokens(&f.paths, &spelling.auth, &tokens()).unwrap();
    let one = Provider::for_named_profile_with(
        cfg.clone(),
        f.paths.clone(),
        reqwest::Client::new(),
        "one",
    )
    .await
    .unwrap();
    let two = Provider::for_named_profile_with(
        spelling.clone(),
        f.paths.clone(),
        reqwest::Client::new(),
        "two",
    )
    .await
    .unwrap();
    let (a, b) = tokio::join!(complete(&one), complete(&two));
    assert!(a.is_ok(), "{a:?}");
    assert!(b.is_ok(), "{b:?}");
    assert_eq!(server.seen.lock().unwrap().refreshes, 1);
    assert_eq!(
        auth::oauth_tokens_file(&f.paths, &cfg.auth),
        auth::oauth_tokens_file(&f.paths, &spelling.auth)
    );
    let stored = std::fs::read_to_string(auth::oauth_tokens_file(&f.paths, &cfg.auth)).unwrap();
    assert!(stored.contains("rotated-refresh"));
    let record: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(record["v"]["token_endpoint"], spelling.auth.token_url);
    assert_eq!(
        std::fs::read(&old_file).unwrap(),
        old_bytes,
        "refresh saves a verified new record without rewriting old evidence"
    );
    server.close().await;
}

#[tokio::test]
async fn exact_endpoint_survives_slash_edits_expiry_401_rotation_and_restart() {
    let env = credential_env::EnvGuard::isolated();
    for original_slash in [false, true] {
        for expired in [false, true] {
            let f = Fixture::new(&env);
            let path = if original_slash { "/token/" } else { "/token" };
            let alias = if original_slash { "/token" } else { "/token/" };
            let server = Server::new(Seen {
                expected_token_path: Some(path.into()),
                allow_rotated_refresh: true,
                ..Default::default()
            })
            .await;
            let mut original = config(&server.base);
            original.auth.token_url = format!("{}{path}?binding=private-fixture", server.base);
            let mut changed = original.clone();
            changed.auth.token_url = format!("{}{alias}?binding=private-fixture", server.base);
            let mut saved = tokens();
            if expired {
                saved.expires_at = Some(auth::now_epoch() - 60);
            }
            auth::save_oauth_tokens(&f.paths, &original.auth, &saved).unwrap();
            assert_eq!(
                auth::oauth_tokens_file(&f.paths, &original.auth),
                auth::oauth_tokens_file(&f.paths, &changed.auth)
            );
            if expired {
                assert_eq!(
                    tokio::time::timeout(
                        Duration::from_secs(5),
                        auth::resolve_provider_token(&f.paths, &changed)
                    )
                    .await
                    .unwrap()
                    .unwrap(),
                    "new-access"
                );
            } else {
                let p = Provider::for_named_profile_with(
                    changed.clone(),
                    f.paths.clone(),
                    reqwest::Client::new(),
                    "private",
                )
                .await
                .unwrap();
                assert_eq!(complete(&p).await.unwrap(), "fallback result");
                assert!(
                    server
                        .seen
                        .lock()
                        .unwrap()
                        .calls
                        .iter()
                        .any(|(url, credential, _)| url == "/v1/chat/completions"
                            && credential == "Bearer old-access"),
                    "real provider401 drives forced refresh"
                );
                drop(p);
            }
            let record_file = auth::oauth_tokens_file(&f.paths, &changed.auth);
            let record: Value =
                serde_json::from_slice(&std::fs::read(&record_file).unwrap()).unwrap();
            assert_eq!(record["v"]["oauth_record_version"], 1);
            assert_eq!(record["v"]["token_endpoint"], original.auth.token_url);
            assert_eq!(
                record["v"]["token_url"],
                auth::canonical_token_url(&changed.auth.token_url).unwrap()
            );
            assert_eq!(record["v"]["client_id"], "client");
            assert_eq!(record["v"]["refresh_token"], "rotated-refresh");
            // Recreate the managed provider, rather than reuse its cached
            // credential, then redeem the rotated token through the alias.
            let restarted = Provider::for_named_profile_with(
                changed.clone(),
                f.paths.clone(),
                reqwest::Client::new(),
                "private",
            )
            .await
            .unwrap();
            assert_eq!(complete(&restarted).await.unwrap(), "fallback result");
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    auth::force_refresh_provider_token(&f.paths, &changed, "new-access")
                )
                .await
                .unwrap()
                .unwrap(),
                "new-access"
            );
            let after: Value =
                serde_json::from_slice(&std::fs::read(&record_file).unwrap()).unwrap();
            assert_eq!(after["v"]["token_endpoint"], original.auth.token_url);
            {
                let seen = server.seen.lock().unwrap();
                let token_calls: Vec<_> = seen
                    .calls
                    .iter()
                    .filter(|(url, _, _)| url.starts_with("/token"))
                    .collect();
                assert_eq!(token_calls.len(), 2);
                assert!(
                    token_calls
                        .iter()
                        .all(|(url, _, _)| url == &format!("{path}?binding=private-fixture")),
                    "no refresh credential reaches the unproven alias: {:?}",
                    seen.calls
                );
                assert!(seen.token_forms[0].contains("refresh_token=private-refresh"));
                assert!(seen.token_forms[1].contains("refresh_token=rotated-refresh"));
            }
            drop(restarted);
            server.close().await;
        }
    }
}

#[tokio::test]
async fn token_redirects_do_not_forward_credentials_or_replace_stored_evidence() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen {
        redirect_refresh: true,
        ..Default::default()
    })
    .await;
    let cfg = config(&server.base);
    let mut saved = tokens();
    saved.expires_at = Some(auth::now_epoch() - 60);
    auth::save_oauth_tokens(&f.paths, &cfg.auth, &saved).unwrap();
    let file = auth::oauth_tokens_file(&f.paths, &cfg.auth);
    let before = std::fs::read(&file).unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        auth::resolve_provider_token(&f.paths, &cfg),
    )
    .await
    .unwrap()
    .unwrap_err();
    let error = format!("{error:#}");
    assert!(
        error.contains("307") && error.contains("redirect_refused"),
        "{error}"
    );
    assert_eq!(std::fs::read(file).unwrap(), before);
    assert_eq!(
        server
            .seen
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|(url, _, _)| url.as_str())
            .collect::<Vec<_>>(),
        ["/token"],
        "no request reaches the307 target"
    );
    server.close().await;
}

#[tokio::test]
async fn token_query_echoes_are_masked_in_decoded_and_wire_forms_before_truncation() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen {
        fail_refresh: true,
        echo_query: true,
        ..Default::default()
    })
    .await;
    let secret = "opaqueQueryQ7/with space+tail";
    assert_eq!(
        formless_core::secrets::redact(secret).0,
        secret,
        "fixture cannot rely on a token-pattern scanner"
    );
    let mut cfg = config(&server.base);
    let mut endpoint = reqwest::Url::parse(&cfg.auth.token_url).unwrap();
    endpoint.query_pairs_mut().append_pair("credential", secret);
    cfg.auth.token_url = endpoint.to_string();
    let mut expired = tokens();
    expired.expires_at = Some(auth::now_epoch() - 60);
    auth::save_oauth_tokens(&f.paths, &cfg.auth, &expired).unwrap();
    let file = auth::oauth_tokens_file(&f.paths, &cfg.auth);
    let before = std::fs::read(&file).unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        auth::resolve_provider_token(&f.paths, &cfg),
    )
    .await
    .unwrap()
    .unwrap_err();
    let detail = format!("{error:#}");
    assert!(
        detail.contains("400") && detail.contains("invalid_grant") && detail.contains("[REDACTED]"),
        "{detail}"
    );
    assert!(
        !detail.contains(secret) && !detail.contains("opaqueQueryQ7"),
        "{detail}"
    );
    assert!(
        !detail.contains(endpoint.query().unwrap().split_once('=').unwrap().1),
        "wire form exposed: {detail}"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        before,
        "rejected refresh cannot replace endpoint evidence"
    );
    assert_eq!(server.seen.lock().unwrap().refreshes, 1);
    server.close().await;
}

#[tokio::test]
async fn token_transport_and_parse_errors_retain_classification_without_query_credentials() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let env = credential_env::EnvGuard::isolated();
    let _f = Fixture::new(&env);
    let secret = "opaquePrivateQueryQ8";
    // This port was owned by this fixture, and is closed before the request.
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    let mut cfg = config(&format!("http://{address}"));
    cfg.auth
        .token_url
        .push_str(&format!("?credential={secret}"));
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        auth::refresh_tokens(&cfg.auth, &tokens()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.downcast_ref::<reqwest::Error>().unwrap().is_connect());
    assert!(error
        .downcast_ref::<reqwest::Error>()
        .unwrap()
        .url()
        .is_none());
    let detail = format!("{error:#} {error:?}");
    assert!(detail.contains("send OAuth token request"));
    assert!(
        !detail.contains(secret) && !detail.contains("credential="),
        "{detail}"
    );

    // Fully consume the owned request, send valid headers with a truncated
    // body, then close. This exercises response-body failure after send.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, request) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let count = tcp.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                assert!(bytes.len() < 16 * 1024);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let size: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .map(|(_, size)| size.trim().parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + size {
                        sent.send(headers.lines().next().unwrap().to_string())
                            .unwrap();
                        break;
                    }
                }
            }
            tcp.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{")
                .await
                .unwrap();
            tcp.shutdown().await.unwrap();
        })
        .await
        .expect("owned truncated-response peer completes");
    });
    let mut cfg = config(&format!("http://{address}"));
    cfg.auth
        .token_url
        .push_str(&format!("?credential={secret}"));
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        auth::refresh_tokens(&cfg.auth, &tokens()),
    )
    .await
    .unwrap()
    .unwrap_err();
    let detail = format!("{error:#} {error:?}");
    assert!(detail.contains("read OAuth token response"), "{detail}");
    assert!(error
        .downcast_ref::<reqwest::Error>()
        .unwrap()
        .url()
        .is_none());
    assert!(
        !detail.contains(secret) && !detail.contains("credential="),
        "{detail}"
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .unwrap()
            .unwrap(),
        format!("POST /token?credential={secret} HTTP/1.1"),
        "masking never alters the actual endpoint query"
    );
    tokio::time::timeout(Duration::from_secs(5), peer)
        .await
        .unwrap()
        .unwrap();

    cfg.auth.token_url = format!("http://127.0.0.1:invalid/token?credential={secret}");
    let error = auth::refresh_tokens(&cfg.auth, &tokens())
        .await
        .unwrap_err();
    let detail = format!("{error:#} {error:?}");
    assert!(detail.contains("not a valid URL"), "{detail}");
    assert!(
        !detail.contains(secret) && !detail.contains("credential="),
        "{detail}"
    );
}

#[tokio::test]
async fn ambiguous_canonical_legacy_and_invalid_evidence_require_relogin_without_network() {
    let env = credential_env::EnvGuard::isolated();
    for marker in [None, Some(1), Some(2)] {
        let f = Fixture::new(&env);
        let server = Server::new(Seen::default()).await;
        let cfg = config(&server.base);
        let mut value = serde_json::to_value(tokens()).unwrap();
        value["token_url"] = json!(cfg.auth.token_url);
        if let Some(marker) = marker {
            value["oauth_record_version"] = json!(marker);
            value["token_endpoint"] = json!(format!("{}/different-issuer", server.base));
            value["client_id"] = json!("client");
        }
        let file = auth::oauth_tokens_file(&f.paths, &cfg.auth);
        formless_core::config::write_secret_file(&file, &value.to_string()).unwrap();
        let before = std::fs::read(&file).unwrap();
        let error = auth::resolve_provider_token(&f.paths, &cfg)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("login required") && error.contains("exact token endpoint"),
            "{error}"
        );
        assert!(
            auth::force_refresh_provider_token(&f.paths, &cfg, "old-access")
                .await
                .is_err()
        );
        assert!(server.seen.lock().unwrap().calls.is_empty());
        assert_eq!(std::fs::read(&file).unwrap(), before);
        assert!(auth::peek_provider_token(&f.paths, &cfg).is_err());
        server.close().await;
    }
}

#[test]
fn finite_raw_keyring_alias_recovery_requires_independent_versioned_endpoint_evidence() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let mut old = config("https://example.com");
    old.auth.token_url = "https://example.com/token/?binding=fixture".into();
    auth::save_oauth_tokens(&f.paths, &old.auth, &tokens()).unwrap();
    let canonical_file = auth::oauth_tokens_file(&f.paths, &old.auth);
    let record: Value = serde_json::from_slice(&std::fs::read(&canonical_file).unwrap()).unwrap();
    let hash = Sha256::digest(format!("{}\n{}", old.auth.token_url, old.auth.client_id));
    let slot: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
    let user = format!("oauth-tokens-{}-{slot}", f.paths.profile);
    let entry = keyring::Entry::new("formless", &user).unwrap();
    let before = record["v"].to_string();
    entry.set_password(&before).unwrap();
    let canonical_slot = canonical_file
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("oauth_tokens-")
        .unwrap();
    keyring::Entry::new(
        "formless",
        &format!("oauth-tokens-{}-{canonical_slot}", f.paths.profile),
    )
    .unwrap()
    .delete_credential()
    .unwrap();
    std::fs::remove_file(&canonical_file).unwrap();
    let mut changed = old.clone();
    changed.auth.token_url = "https://example.com/token?binding=fixture".into();
    assert_eq!(
        auth::load_oauth_tokens(&f.paths, &changed.auth),
        Some(tokens())
    );
    assert_eq!(entry.get_password().unwrap(), before);
    assert!(
        !auth::oauth_tokens_file(&f.paths, &changed.auth).exists(),
        "read-only compatibility does not manufacture a new record"
    );
    entry.delete_credential().unwrap();
}

#[test]
fn unknown_historical_raw_keyring_hash_is_not_guessed_or_relabelled() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let mut original = config("https://example.com");
    original.auth.token_url = "HTTPS://ExAmPlE.com:443/token/".into();
    let hash = Sha256::digest(format!(
        "{}\n{}",
        original.auth.token_url, original.auth.client_id
    ));
    let slot: String = hash[..6].iter().map(|b| format!("{b:02x}")).collect();
    let entry = keyring::Entry::new(
        "formless",
        &format!("oauth-tokens-{}-{slot}", f.paths.profile),
    )
    .unwrap();
    let mut value = serde_json::to_value(tokens()).unwrap();
    value["token_url"] = json!(original.auth.token_url);
    let before = value.to_string();
    entry.set_password(&before).unwrap();
    let changed = config("https://example.com");
    assert!(
        auth::load_oauth_tokens(&f.paths, &changed.auth).is_none(),
        "arbitrary old raw-hash keyring names are not discoverable from canonical identity"
    );
    assert_eq!(entry.get_password().unwrap(), before);
    assert!(auth::load_oauth_tokens(&f.paths, &original.auth).is_none(), "even restoring the raw spelling cannot prove an old issuer was not fabricated by migration");
    assert!(!auth::oauth_tokens_file(&f.paths, &changed.auth).exists());
    entry.delete_credential().unwrap();
}

#[tokio::test]
async fn historical_config_derived_migration_labels_never_authorize_provider_a_tokens_at_b() {
    let env = credential_env::EnvGuard::isolated();
    for keyring_only in [false, true] {
        for expired in [false, true] {
            let f = Fixture::new(&env);
            let server = Server::new(Seen::default()).await;
            let mut b = config(&server.base);
            b.auth.token_url.push('/');
            let mut config_b = Config::default();
            config_b.provider = b.clone();
            config_b.save(&f.paths).unwrap();
            let mut a = tokens();
            a.access_token = "provider-A-access".into();
            a.refresh_token = Some("provider-A-refresh".into());
            if expired {
                a.expires_at = Some(auth::now_epoch() - 60);
            }
            // Exact output of pre-1228 load_with_issuer/save_oauth_tokens at
            // 8739dd0: it ignored the profile-wide set's issuer, stamped B's
            // current raw URL, and keyed both copies by SHA256(URL+'\n'+ID)[..6].
            let mut relabelled = serde_json::to_value(&a).unwrap();
            relabelled["token_url"] = json!(b.auth.token_url);
            let wrapped = json!({"v":relabelled,"saved_at":1234}).to_string();
            let digest = Sha256::digest(format!("{}\n{}", b.auth.token_url, b.auth.client_id));
            let slot: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
            let old_file = f.paths.config_dir.join(format!("oauth_tokens-{slot}.json"));
            let entry = keyring::Entry::new(
                "formless",
                &format!("oauth-tokens-{}-{slot}", f.paths.profile),
            )
            .unwrap();
            if keyring_only {
                entry.set_password(&wrapped).unwrap();
            } else {
                formless_core::config::write_secret_file(&old_file, &wrapped).unwrap();
            }
            assert_ne!(
                old_file,
                auth::oauth_tokens_file(&f.paths, &b.auth),
                "raw noncanonical slot/hash can still have been config-derived"
            );
            assert!(auth::resolve_provider_token(&f.paths, &b)
                .await
                .unwrap_err()
                .to_string()
                .contains("login required"));
            assert!(
                auth::force_refresh_provider_token(&f.paths, &b, "provider-A-access")
                    .await
                    .is_err()
            );
            assert!(auth::peek_provider_token(&f.paths, &b).is_err());
            let status = auth::auth_status(&f.paths, &b);
            assert!(!status.credential_present);
            assert!(status
                .warning
                .unwrap()
                .contains("could have been derived from configuration"));
            assert!(
                server.seen.lock().unwrap().calls.is_empty(),
                "neither A's access nor refresh token reaches B"
            );
            if keyring_only {
                assert_eq!(entry.get_password().unwrap(), wrapped);
                entry.delete_credential().unwrap();
            } else {
                assert_eq!(std::fs::read_to_string(&old_file).unwrap(), wrapped);
            }
            assert!(
                !auth::oauth_tokens_file(&f.paths, &b.auth).exists(),
                "lookup cannot convert a forged legacy label into new transport evidence"
            );
            server.close().await;
        }
    }
}

#[tokio::test]
async fn changing_default_does_not_send_unknown_legacy_refresh_token_to_new_issuer() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen::default()).await;
    let cfg = config(&server.base);
    let mut saved = Config::default();
    saved.provider = cfg.clone();
    saved.save(&f.paths).unwrap();
    let legacy = serde_json::to_string(&tokens()).unwrap();
    formless_core::config::write_secret_file(&f.paths.oauth_tokens_file(), &legacy).unwrap();
    assert!(auth::resolve_provider_token(&f.paths, &cfg).await.is_err());
    assert!(
        auth::force_refresh_provider_token(&f.paths, &cfg, "old-access")
            .await
            .is_err()
    );
    assert!(
        server.seen.lock().unwrap().calls.is_empty(),
        "no access or refresh token may reach an unproven issuer"
    );
    assert_eq!(
        std::fs::read_to_string(f.paths.oauth_tokens_file()).unwrap(),
        legacy
    );
    server.close().await;
}

#[tokio::test]
async fn forced_refresh_is_shared_across_withheld_stream_drop_retries_for_all_apis() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    for style in ["chat", "responses", "anthropic"] {
        let (drop_now, gate) = tokio::sync::oneshot::channel();
        let server = Server::new(Seen {
            drop_stream_once: true,
            stream_drop_gate: Some(gate),
            ..Default::default()
        })
        .await;
        let mut cfg = config(&server.base);
        cfg.api_style = style.into();
        cfg.max_retries = 1;
        cfg.max_backoff_secs = 1;
        let p = managed(&f, cfg, "work").await;
        let mut drop_now = Some(drop_now);
        let mut bytes_seen = 0;
        let mut shown = String::new();
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            p.chat_stream(
                &[json!({"role":"user","content":"hi"})],
                &[],
                "primary",
                None,
                &cancel,
                |kind, text| {
                    if kind == provider::DeltaKind::Bytes {
                        bytes_seen += 1;
                        if let Some(sender) = drop_now.take() {
                            sender.send(()).expect("mock stream still waiting");
                        }
                    }
                    if kind == provider::DeltaKind::Content {
                        shown.push_str(text);
                    }
                },
            ),
        )
        .await
        .expect("withheld stream retry timed out");
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            error.contains("second revoked credential"),
            "{style}: {error}"
        );
        assert!(
            bytes_seen > 0,
            "{style}: error was released only after receiving body bytes"
        );
        assert!(
            shown.is_empty(),
            "{style}: fixture never emits a visible delta"
        );
        let seen = server.seen.lock().unwrap();
        assert!(
            seen.stream_delivered,
            "{style}: successful200 stream preceded retry"
        );
        assert_eq!(
            seen.refreshes, 1,
            "{style}: one forced refresh for the whole logical stream"
        );
        assert_eq!(
            seen.calls
                .iter()
                .filter(|(path, _, _)| path != "/token")
                .count(),
            3,
            "{style}: initial401, refreshed200, retry401"
        );
        drop(seen);
        server.close().await;
    }
}

#[tokio::test]
async fn failed_refresh_context_survives_fallback_hard_error() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    for status in [400, 403] {
        let server = Server::new(Seen {
            fail_refresh: true,
            fallback_status: Some(status),
            ..Default::default()
        })
        .await;
        let mut cfg = config(&server.base);
        cfg.fallback_models = vec!["fallback".into()];
        let p = managed(&f, cfg, "work").await;
        let error = complete(&p).await.unwrap_err().to_string();
        let status_text = status.to_string();
        for detail in [
            "401",
            "original revoked credential",
            "invalid_grant",
            "fallback policy rejection",
            status_text.as_str(),
        ] {
            assert!(error.contains(detail), "missing{detail} in {error}");
        }
        let seen = server.seen.lock().unwrap();
        assert_eq!(seen.refreshes, 1);
        assert!(seen.calls.iter().any(|(_, _, model)| model == "fallback"));
        drop(seen);
        server.close().await;
    }
}

#[tokio::test]
async fn provider_and_refresh_error_echoes_never_expose_opaque_submitted_credentials() {
    use base64::Engine;
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let opaque = "echo [old-access] [private-refresh] [plain-client-phrase]";
    assert_eq!(
        formless_core::secrets::redact(opaque).0,
        opaque,
        "fixture credentials have no scanner-recognizable shape"
    );
    let client_secret = "plain-client-phrase";
    env.set("FORMLESS_OAUTH_CLIENT_SECRET", client_secret);
    for operation in ["chat", "models", "compact"] {
        let server = Server::new(Seen {
            fail_refresh: true,
            echo_credentials: true,
            ..Default::default()
        })
        .await;
        let p = managed(&f, config(&server.base), "work").await;
        let error = tokio::time::timeout(Duration::from_secs(5), async {
            match operation {
                "chat" => complete(&p).await.unwrap_err().to_string(),
                "models" => p.list_models().await.unwrap_err().to_string(),
                _ => p
                    .compact_server("input", "primary")
                    .await
                    .unwrap_err()
                    .to_string(),
            }
        })
        .await
        .expect("credential echo fixture timeout");
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("client:{client_secret}"));
        for secret in [
            "old-access",
            "private-refresh",
            client_secret,
            basic.as_str(),
        ] {
            assert!(
                !error.contains(secret),
                "{operation}: opaque credential exposed in {error}"
            );
        }
        assert!(
            error.contains("401"),
            "{operation}: status context retained: {error}"
        );
        assert!(
            error.contains("invalid_grant"),
            "{operation}: refresh error retained: {error}"
        );
        assert!(
            error.contains("[REDACTED]"),
            "{operation}: exact credential values masked"
        );
        assert_eq!(server.seen.lock().unwrap().refreshes, 1);
        server.close().await;
    }
}

#[tokio::test]
async fn probe_recovery_uses_raw_rejection_classification_when_credential_masks_keyword() {
    let env = credential_env::EnvGuard::isolated();
    let _fixture = Fixture::new(&env);
    let server = Server::new(Seen {
        accept_any_credential: true,
        reject_forced_choice: true,
        ..Default::default()
    })
    .await;
    let mut cfg = config(&server.base);
    cfg.auth.mode = "api_key".into();
    // The diagnostic must mask this exact value even where it occurs as a
    // protocol keyword. The typed rejection still permits the unforced probe.
    let p = Provider::new(cfg, "tool_choice".into()).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        provider::probe_provider(&p, "primary"),
    )
    .await
    .expect("masked probe fixture timeout")
    .unwrap();
    assert!(result.tool_calling);
    assert!(
        !result.prompt_fallback,
        "native probe was retried without forced choice"
    );
    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.forced_rejections, 1);
    assert_eq!(seen.refreshes, 0);
    assert!(seen
        .calls
        .iter()
        .all(|(_, header, _)| header == "Bearer tool_choice"));
    drop(seen);
    server.close().await;
}
