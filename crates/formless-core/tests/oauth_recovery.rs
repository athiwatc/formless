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
    sync::{Arc, Mutex, Once},
    time::Duration,
};

struct Fixture {
    paths: Paths,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new(_guard: &credential_env::EnvGuard) -> Self {
        static MOCK: Once = Once::new();
        MOCK.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder())
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
fn endpoint_spelling_reuses_login_and_old_raw_slot_without_relabelling() {
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
    assert_eq!(auth::load_oauth_tokens(&f.paths, &new.auth), Some(tokens()));
    assert!(
        file.exists(),
        "diagnostics/compatibility lookup never rewrite or delete evidence"
    );
    auth::save_oauth_tokens(&f.paths, &new.auth, &tokens()).unwrap();
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
    assert_eq!(auth::load_oauth_tokens(&f.paths, &cfg.auth), Some(tokens()));
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
    fallback_ok: bool,
    reject_new: bool,
    pages: bool,
    fallback_status: Option<u16>,
    drop_stream_once: bool,
    stream_delivered: bool,
    stream_drop_gate: Option<tokio::sync::oneshot::Receiver<()>>,
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
        assert!(
            body.contains("refresh_token=private-refresh")
                || (seen.drop_stream_once && body.contains("refresh_token=rotated-refresh"))
        );
        return if seen.fail_refresh {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant"})),
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
    let good = (credential.ends_with("new-access") && !seen.reject_new)
        || (seen.fallback_ok && payload["model"] == "fallback");
    if !good {
        return (StatusCode::UNAUTHORIZED, "original revoked credential").into_response();
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
    Json(json!({"choices":[{"message":{"content":"fallback result"}}]})).into_response()
}
impl Server {
    async fn new(options: Seen) -> Self {
        let seen = Arc::new(Mutex::new(options));
        let app = Router::new()
            .route("/token", post(endpoint))
            .route("/token/", post(endpoint))
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
async fn equivalent_old_endpoint_spellings_share_refresh_lock_and_canonical_save() {
    let env = credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let server = Server::new(Seen::default()).await;
    let cfg = config(&server.base);
    let mut spelling = cfg.clone();
    spelling.auth.token_url.push('/');
    write_old_slot(&f.paths, &spelling, Some(&spelling.auth.token_url));
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
    server.close().await;
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
