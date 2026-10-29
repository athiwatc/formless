//! Subscription/OAuth providers: authorization-code + PKCE login, secure
//! token storage, refresh-on-expiry, and API-key backward compatibility.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use formless_core::auth::{
    auth_status, build_authorize_url, delete_oauth_tokens, exchange_code, has_provider_credential,
    load_oauth_tokens, login_with_timeout, now_epoch, parse_token_response, pkce_challenge,
    resolve_provider_token, save_oauth_tokens, validate_oauth_config, OAuthTokens,
};
use formless_core::config::{delete_api_key, save_api_key, Config, ProviderCfg};
use formless_core::provider::Provider;
use formless_core::Paths;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn clear_credential_env() {
    std::env::remove_var("FORMLESS_API_KEY");
    std::env::remove_var("FORMLESS_OAUTH_ACCESS_TOKEN");
    std::env::remove_var("FORMLESS_OAUTH_CLIENT_SECRET");
}

fn test_paths(tag: &str) -> Paths {
    clear_credential_env();
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-auth-{tag}-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

fn oauth_cfg(base: &str) -> ProviderCfg {
    let mut cfg = ProviderCfg {
        base_url: format!("{base}/v1"),
        model: "test-model".into(),
        ..Default::default()
    };
    cfg.auth.mode = "oauth".into();
    cfg.auth.authorize_url = format!("{base}/authorize");
    cfg.auth.token_url = format!("{base}/token");
    cfg.auth.client_id = "test-client".into();
    cfg.auth.scopes = "openid profile".into();
    cfg.auth.redirect_port = 0;
    cfg
}

type Captured = Arc<Mutex<Vec<String>>>;

fn spawn_token_server() -> (String, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let state = captured.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let app = Router::new()
                .route(
                    "/v1/models",
                    get(|headers: axum::http::HeaderMap| async move {
                        let authorization = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        let authorized =
                            authorization.strip_prefix("Bearer ") == Some("new-access");
                        if authorized {
                            Json(json!({"data": [{"id": "test-model"}]})).into_response()
                        } else {
                            (
                                StatusCode::UNAUTHORIZED,
                                Json(json!({"error": {"message": "expired token"}})),
                            )
                                .into_response()
                        }
                    }),
                )
                .route(
                    "/v1/chat/completions",
                    post(|headers: axum::http::HeaderMap| async move {
                        let ok = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            == Some("Bearer new-access");
                        if ok {
                            Json(json!({"choices": [{"message": {"content": "hi"}}]}))
                                .into_response()
                        } else {
                            (
                                StatusCode::UNAUTHORIZED,
                                Json(json!({"error": {"message": "revoked"}})),
                            )
                                .into_response()
                        }
                    }),
                )
                .route(
                    "/token",
                    post(
                        |State(captured): State<Captured>, body: String| async move {
                            captured.lock().unwrap().push(body.clone());
                            if body.contains("grant_type=refresh_token") {
                                if !body.contains("refresh_token=old-refresh") {
                                    return (
                                        StatusCode::BAD_REQUEST,
                                        Json(json!({"error": "invalid_grant"})),
                                    )
                                        .into_response();
                                }
                                return Json(json!({
                                    "access_token": "new-access",
                                    "refresh_token": "rotated-refresh",
                                    "expires_in": 3600,
                                    "token_type": "Bearer",
                                    "scope": "openid profile"
                                }))
                                .into_response();
                            }
                            if !body.contains("grant_type=authorization_code")
                                || !body.contains("code=good-code")
                                || !body.contains("client_id=test-client")
                                || !body.contains("code_verifier=")
                                || body.contains("code_verifier=&")
                            {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    Json(json!({
                                        "error": "invalid_request",
                                        "error_description": "missing PKCE verifier or code"
                                    })),
                                )
                                    .into_response();
                            }
                            Json(json!({
                                "access_token": "login-access",
                                "refresh_token": "login-refresh",
                                "expires_in": 3600,
                                "token_type": "Bearer",
                                "scope": "openid profile"
                            }))
                            .into_response()
                        },
                    ),
                )
                .with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(format!("http://{}", listener.local_addr().unwrap()))
                .unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    (rx.recv_timeout(Duration::from_secs(5)).unwrap(), captured)
}

fn form_value<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

fn query_value(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    let raw = form_value(query, key)?;
    Some(
        raw.replace("%3A", ":")
            .replace("%3a", ":")
            .replace("%2F", "/")
            .replace("%2f", "/"),
    )
}

#[test]
fn pkce_matches_rfc7636_vector_and_authorize_url_is_encoded() {
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );

    let mut cfg = oauth_cfg("http://127.0.0.1:4567");
    cfg.auth.authorize_url = "https://auth.example/authorize?tenant=main".into();
    cfg.auth
        .extra_authorize_params
        .insert("access_type".into(), "offline".into());
    cfg.auth
        .extra_authorize_params
        .insert("audience".into(), "https://api.example".into());
    let url = build_authorize_url(
        &cfg.auth,
        "http://127.0.0.1:8977/callback",
        "state-123",
        "challenge-456",
    )
    .unwrap();
    assert!(url.starts_with("https://auth.example/authorize?tenant=main&response_type=code&"));
    assert!(url.contains("client_id=test-client"));
    assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A8977%2Fcallback"));
    assert!(url.contains("state=state-123"));
    assert!(url.contains("code_challenge=challenge-456"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("scope=openid%20profile"));
    assert!(url.contains("access_type=offline"));
    assert!(url.contains("audience=https%3A%2F%2Fapi.example"));

    let mut insecure = oauth_cfg("http://auth.example");
    insecure.auth.authorize_url = "http://auth.example/authorize".into();
    insecure.auth.token_url = "https://auth.example/token".into();
    assert!(validate_oauth_config(&insecure)
        .unwrap_err()
        .to_string()
        .contains("must use HTTPS"));
}

#[test]
fn oauth_config_toml_shape_and_token_storage() {
    let paths = test_paths("storage");
    std::fs::write(
        paths.config_file(),
        r#"
[provider]
base_url = "https://api.example/v1"
model = "subscription-model"

[provider.auth]
mode = "oauth"
authorize_url = "https://auth.example/authorize"
token_url = "https://auth.example/token"
client_id = "public-client"
scopes = "openid offline_access"
redirect_port = 8977

[provider.auth.extra_authorize_params]
access_type = "offline"
prompt = "consent"
"#,
    )
    .unwrap();
    let cfg = Config::load(&paths).unwrap();
    assert!(cfg.provider.oauth());
    assert_eq!(cfg.provider.auth.client_id, "public-client");
    assert_eq!(
        cfg.provider.auth.extra_authorize_params.get("prompt"),
        Some(&"consent".to_string())
    );
    validate_oauth_config(&cfg.provider).unwrap();

    let parsed = parse_token_response(
        &json!({"access_token": "stored-access", "expires_in": "120", "token_type": "bearer"}),
        1_000,
        Some("previous-refresh"),
    )
    .unwrap();
    assert_eq!(parsed.expires_at, Some(1_120));
    assert_eq!(parsed.refresh_token.as_deref(), Some("previous-refresh"));

    save_oauth_tokens(&paths, &cfg.provider.auth, &parsed).unwrap();
    let loaded = load_oauth_tokens(&paths, &cfg.provider.auth).unwrap();
    assert_eq!(loaded, parsed);
    assert!(formless_core::auth::oauth_tokens_file(&paths, &cfg.provider.auth).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(formless_core::auth::oauth_tokens_file(
            &paths,
            &cfg.provider.auth,
        ))
        .unwrap()
        .permissions()
        .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let debug = format!("{:?}", loaded);
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("stored-access"));
    assert!(!debug.contains("previous-refresh"));

    let status = auth_status(&paths, &cfg.provider);
    assert_eq!(status.mode, "oauth");
    assert!(status.has_refresh_token);
    delete_oauth_tokens(&paths, &cfg.provider.auth);
    assert!(load_oauth_tokens(&paths, &cfg.provider.auth).is_none());
}

#[tokio::test]
async fn expired_tokens_refresh_once_and_rotated_tokens_persist() {
    let paths = test_paths("refresh");
    let (base, captured) = spawn_token_server();
    let cfg = oauth_cfg(&base);
    let old = OAuthTokens {
        access_token: "old-access".into(),
        refresh_token: Some("old-refresh".into()),
        expires_at: Some(now_epoch() - 10),
        token_type: "Bearer".into(),
        scope: Some("openid profile".into()),
    };
    save_oauth_tokens(&paths, &cfg.auth, &old).unwrap();
    assert!(has_provider_credential(&paths, &cfg));

    let first = resolve_provider_token(&paths, &cfg).await.unwrap();
    assert_eq!(first, "new-access");
    let stored = load_oauth_tokens(&paths, &cfg.auth).unwrap();
    assert_eq!(stored.access_token, "new-access");
    assert_eq!(stored.refresh_token.as_deref(), Some("rotated-refresh"));
    assert_eq!(captured.lock().unwrap().len(), 1);
    assert!(captured.lock().unwrap()[0].contains("grant_type=refresh_token"));

    let second = resolve_provider_token(&paths, &cfg).await.unwrap();
    assert_eq!(second, "new-access");
    assert_eq!(
        captured.lock().unwrap().len(),
        1,
        "a still-valid access token must not be refreshed again"
    );
    delete_oauth_tokens(&paths, &cfg.auth);
}

#[tokio::test]
async fn managed_provider_refreshes_again_when_token_expires_mid_run() {
    let paths = test_paths("managed-provider");
    let (base, captured) = spawn_token_server();
    let cfg = oauth_cfg(&base);
    let expired = OAuthTokens {
        access_token: "old-access".into(),
        refresh_token: Some("old-refresh".into()),
        expires_at: Some(now_epoch() - 10),
        token_type: "Bearer".into(),
        scope: Some("openid profile".into()),
    };
    save_oauth_tokens(&paths, &cfg.auth, &expired).unwrap();
    let provider = Provider::for_profile(cfg.clone(), paths.clone())
        .await
        .unwrap();
    assert_eq!(captured.lock().unwrap().len(), 1);
    assert_eq!(
        load_oauth_tokens(&paths, &cfg.auth).unwrap().access_token,
        "new-access"
    );
    assert_eq!(provider.list_models().await.unwrap(), ["test-model"]);

    // Simulate time passing inside the same long-lived provider: storing an
    // expired token set must be noticed before the next HTTP request, not
    // only when some outer daemon code constructs another Provider.
    save_oauth_tokens(&paths, &cfg.auth, &expired).unwrap();
    assert_eq!(provider.list_models().await.unwrap(), ["test-model"]);
    assert_eq!(captured.lock().unwrap().len(), 2);
    assert_eq!(
        load_oauth_tokens(&paths, &cfg.auth)
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("rotated-refresh")
    );
    delete_oauth_tokens(&paths, &cfg.auth);
}

#[tokio::test]
async fn exchange_code_sends_pkce_verifier() {
    clear_credential_env();
    let (base, captured) = spawn_token_server();
    let cfg = oauth_cfg(&base);
    let verifier = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
    let tokens = exchange_code(
        &cfg.auth,
        "good-code",
        "http://127.0.0.1:8977/callback",
        verifier,
    )
    .await
    .unwrap();
    assert_eq!(tokens.access_token, "login-access");
    assert_eq!(tokens.refresh_token.as_deref(), Some("login-refresh"));
    let body = captured.lock().unwrap()[0].clone();
    assert_eq!(form_value(&body, "code_verifier"), Some(verifier));
    assert_eq!(
        form_value(&body, "redirect_uri"),
        Some("http%3A%2F%2F127.0.0.1%3A8977%2Fcallback")
    );
}

fn send_callback(redirect_uri: &str, code: &str, state: &str) -> String {
    let without_scheme = redirect_uri.strip_prefix("http://").unwrap();
    let (authority, path) = without_scheme.split_once('/').unwrap();
    let mut stream = TcpStream::connect(authority).unwrap();
    write!(
        stream,
        "GET /{path}?code={code}&state={state} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn browser_login_callback_exchanges_code_and_saves_tokens() {
    let paths = test_paths("login");
    let (base, captured) = spawn_token_server();
    let cfg = oauth_cfg(&base);
    let auth = cfg.auth.clone();
    let (url_tx, url_rx) = std::sync::mpsc::channel();
    let login_paths = paths.clone();
    let handle = std::thread::spawn(move || {
        login_with_timeout(
            &login_paths,
            &cfg,
            |url| url_tx.send(url.to_string()).unwrap(),
            Duration::from_secs(10),
        )
    });

    let url = url_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let redirect_uri = query_value(&url, "redirect_uri").unwrap();
    let state = query_value(&url, "state").unwrap();
    let challenge = query_value(&url, "code_challenge").unwrap();
    let response = send_callback(&redirect_uri, "good-code", &state);
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("Authorization received"));

    let tokens = handle.join().unwrap().unwrap();
    assert_eq!(tokens.access_token, "login-access");
    assert_eq!(load_oauth_tokens(&paths, &auth).unwrap(), tokens);
    let body = captured.lock().unwrap()[0].clone();
    let verifier = form_value(&body, "code_verifier").unwrap();
    assert_eq!(verifier.len(), 43);
    assert_eq!(pkce_challenge(verifier), challenge);
    delete_oauth_tokens(&paths, &auth);
}

#[test]
fn browser_login_rejects_state_mismatch() {
    let paths = test_paths("badstate");
    let (base, _captured) = spawn_token_server();
    let cfg = oauth_cfg(&base);
    let auth = cfg.auth.clone();
    let (url_tx, url_rx) = std::sync::mpsc::channel();
    let login_paths = paths.clone();
    let handle = std::thread::spawn(move || {
        login_with_timeout(
            &login_paths,
            &cfg,
            |url| url_tx.send(url.to_string()).unwrap(),
            Duration::from_secs(10),
        )
    });

    let url = url_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let redirect_uri = query_value(&url, "redirect_uri").unwrap();
    let response = send_callback(&redirect_uri, "good-code", "attacker-state");
    assert!(
        response.starts_with("HTTP/1.1 400 Bad Request"),
        "{response}"
    );
    let error = handle.join().unwrap().unwrap_err();
    assert!(format!("{error:#}").contains("state mismatch"));
    assert!(load_oauth_tokens(&paths, &auth).is_none());
}

#[tokio::test]
async fn credential_resolution_errors_and_api_key_mode_are_actionable() {
    let paths = test_paths("errors");
    let oauth = oauth_cfg("http://127.0.0.1:9");
    assert!(!has_provider_credential(&paths, &oauth));
    let error = resolve_provider_token(&paths, &oauth).await.unwrap_err();
    assert!(format!("{error:#}").contains("formless auth login"));

    let mut invalid = ProviderCfg::default();
    invalid.auth.mode = "subscription".into();
    assert!(!has_provider_credential(&paths, &invalid));
    let error = resolve_provider_token(&paths, &invalid).await.unwrap_err();
    assert!(format!("{error:#}").contains("unsupported provider.auth.mode"));

    let api_key_cfg = ProviderCfg {
        base_url: "https://api.example/v1".into(),
        model: "test".into(),
        ..Default::default()
    };
    save_api_key(&paths, "plain-api-key").unwrap();
    assert!(has_provider_credential(&paths, &api_key_cfg));
    assert_eq!(
        resolve_provider_token(&paths, &api_key_cfg).await.unwrap(),
        "plain-api-key"
    );
    let status = auth_status(&paths, &api_key_cfg);
    assert_eq!(status.mode, "api_key");
    assert!(status.credential_present);
    delete_api_key(&paths);
}

fn tokens(access: &str, refresh: &str, expires_at: i64) -> OAuthTokens {
    OAuthTokens {
        access_token: access.into(),
        refresh_token: Some(refresh.into()),
        expires_at: Some(expires_at),
        token_type: "Bearer".into(),
        scope: None,
    }
}

#[tokio::test]
async fn two_oauth_providers_keep_separate_tokens() {
    let paths = test_paths("two");
    let a = oauth_cfg("http://a.example");
    let b = oauth_cfg("http://b.example");
    save_oauth_tokens(
        &paths,
        &a.auth,
        &tokens("a-access", "a-refresh", now_epoch() + 600),
    )
    .unwrap();
    save_oauth_tokens(
        &paths,
        &b.auth,
        &tokens("b-access", "b-refresh", now_epoch() + 600),
    )
    .unwrap();
    assert_eq!(
        resolve_provider_token(&paths, &a).await.unwrap(),
        "a-access"
    );
    assert_eq!(
        resolve_provider_token(&paths, &b).await.unwrap(),
        "b-access"
    );
    delete_oauth_tokens(&paths, &a.auth);
    assert!(load_oauth_tokens(&paths, &a.auth).is_none());
    assert_eq!(
        load_oauth_tokens(&paths, &b.auth).unwrap().access_token,
        "b-access"
    );
    delete_oauth_tokens(&paths, &b.auth);
}

#[tokio::test]
async fn legacy_tokens_move_to_the_default_provider_only() {
    let paths = test_paths("legacy");
    let mut cfg = formless_core::Config::default();
    cfg.provider = oauth_cfg("http://default.example");
    cfg.save(&paths).unwrap();
    let other = oauth_cfg("http://other.example");
    let legacy = serde_json::to_string(&tokens("old", "r", now_epoch() + 600)).unwrap();
    formless_core::config::write_secret_file(&paths.oauth_tokens_file(), &legacy).unwrap();
    // Another provider never picks up (or refreshes with) the default's tokens.
    assert!(load_oauth_tokens(&paths, &other.auth).is_none());
    assert_eq!(
        load_oauth_tokens(&paths, &cfg.provider.auth)
            .unwrap()
            .access_token,
        "old"
    );
    assert!(
        !paths.oauth_tokens_file().exists(),
        "migrated out of the legacy file"
    );
    assert!(formless_core::auth::oauth_tokens_file(&paths, &cfg.provider.auth).exists());
    delete_oauth_tokens(&paths, &cfg.provider.auth);
}

#[tokio::test]
async fn a_401_forces_one_refresh_and_resends() {
    let paths = test_paths("401");
    let (base, captured) = spawn_token_server();
    let mut cfg = oauth_cfg(&base);
    cfg.max_retries = 1;
    // Not expired by the clock, but the server has revoked it.
    save_oauth_tokens(
        &paths,
        &cfg.auth,
        &tokens("old-access", "old-refresh", now_epoch() + 600),
    )
    .unwrap();
    let provider = Provider::for_profile(cfg.clone(), paths.clone())
        .await
        .unwrap();
    let out = provider
        .complete(
            &[json!({"role": "user", "content": "hi"})],
            "test-model",
            None,
        )
        .await
        .unwrap();
    assert_eq!(out, "hi");
    assert_eq!(captured.lock().unwrap().len(), 1, "one refresh");
    assert_eq!(
        load_oauth_tokens(&paths, &cfg.auth).unwrap().access_token,
        "new-access"
    );
    delete_oauth_tokens(&paths, &cfg.auth);
}
