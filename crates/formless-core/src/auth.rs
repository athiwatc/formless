//! Provider credentials: static API keys and OAuth2 authorization-code
//! login with PKCE for subscription-style providers.
//!
//! OAuth is intentionally generic: the provider supplies its authorize and
//! token endpoints plus a public client id in `config.toml`. Tokens are
//! credentials, so they are stored through the same keychain + `0600` file
//! pattern as the API key and are never written to `config.toml` or logs.

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{load_api_key, AuthCfg, ProviderCfg};
use crate::Paths;

const KEYRING_SERVICE: &str = "formless";
const EXPIRY_SKEW_SECS: i64 = 30;
const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_CALLBACK_BYTES: usize = 16 * 1024;

/// Stored OAuth token set. `Debug` is implemented manually so tokens can
/// never be printed accidentally by formatted diagnostics.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix epoch seconds; `None` means the provider supplied no lifetime.
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(default)]
    pub scope: Option<String>,
}

fn default_token_type() -> String {
    "Bearer".into()
}

impl OAuthTokens {
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at
            .is_some_and(|expires| expires <= now.saturating_add(EXPIRY_SKEW_SECS))
    }

    pub fn has_refresh_token(&self) -> bool {
        self.refresh_token.as_deref().is_some_and(|s| !s.is_empty())
    }
}

impl fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthTokens")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Non-network credential status used by CLI diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStatus {
    pub mode: String,
    pub credential_present: bool,
    pub expires_at: Option<i64>,
    pub has_refresh_token: bool,
    pub scope: Option<String>,
}

/// Which token set a provider's OAuth config owns: its token endpoint and
/// client id, so two OAuth providers never share (or cross-refresh) tokens.
fn token_slot(auth: &AuthCfg) -> String {
    let digest = Sha256::digest(format!("{}\n{}", auth.token_url, auth.client_id));
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// The `0600` file holding the token set of the provider with `auth`.
pub fn oauth_tokens_file(paths: &Paths, auth: &AuthCfg) -> std::path::PathBuf {
    paths
        .config_dir
        .join(format!("oauth_tokens-{}.json", token_slot(auth)))
}

fn oauth_keyring_user(paths: &Paths, auth: &AuthCfg) -> String {
    format!("oauth-tokens-{}-{}", paths.profile, token_slot(auth))
}

/// Before tokens were keyed by provider, one set per profile.
fn legacy_keyring_user(paths: &Paths) -> String {
    format!("oauth-tokens-{}", paths.profile)
}

/// Stored form: the token set plus the endpoint it was issued by.
fn encode_tokens(auth: &AuthCfg, tokens: &OAuthTokens) -> Result<String> {
    let mut v = serde_json::to_value(tokens).context("encode OAuth tokens")?;
    v["token_url"] = Value::String(auth.token_url.clone());
    Ok(v.to_string())
}

/// (tokens, the token endpoint that issued them if recorded).
fn decode_tokens(text: &str) -> Option<(OAuthTokens, Option<String>)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let issuer = v["token_url"].as_str().map(str::to_string);
    let tokens: OAuthTokens = serde_json::from_value(v).ok()?;
    (!tokens.access_token.is_empty()).then_some((tokens, issuer))
}

pub fn save_oauth_tokens(paths: &Paths, auth: &AuthCfg, tokens: &OAuthTokens) -> Result<()> {
    let encoded = encode_tokens(auth, tokens)?;
    let file = oauth_tokens_file(paths, auth);
    let file_result = crate::config::write_secret_file(&file, &encoded);
    let keyring_ok = keyring::Entry::new(KEYRING_SERVICE, &oauth_keyring_user(paths, auth))
        .map(|entry| entry.set_password(&encoded).is_ok())
        .unwrap_or(false);
    if file_result.is_ok() || keyring_ok {
        Ok(())
    } else {
        file_result
    }
}

fn read_stored(file: &std::path::Path, user: &str) -> Option<(OAuthTokens, Option<String>)> {
    if let Some(found) = crate::config::read_secret_file(file).and_then(|t| decode_tokens(&t)) {
        return Some(found);
    }
    let text = keyring::Entry::new(KEYRING_SERVICE, user)
        .ok()?
        .get_password()
        .ok()?;
    decode_tokens(&text)
}

fn load_with_issuer(paths: &Paths, auth: &AuthCfg) -> Option<(OAuthTokens, Option<String>)> {
    if let Some(found) = read_stored(
        &oauth_tokens_file(paths, auth),
        &oauth_keyring_user(paths, auth),
    ) {
        return Some(found);
    }
    // The old profile-wide set was always from `formless auth login`, i.e.
    // the default provider: it moves into that provider's slot only.
    let owner = crate::Config::load_file_only(paths).ok()?.provider.auth;
    if token_slot(&owner) != token_slot(auth) {
        return None;
    }
    let (tokens, _) = read_stored(&paths.oauth_tokens_file(), &legacy_keyring_user(paths))?;
    save_oauth_tokens(paths, auth, &tokens).ok()?;
    delete_legacy_tokens(paths);
    Some((tokens, Some(auth.token_url.clone())))
}

/// The token set of the provider with `auth`, if one is stored.
pub fn load_oauth_tokens(paths: &Paths, auth: &AuthCfg) -> Option<OAuthTokens> {
    load_with_issuer(paths, auth).map(|(tokens, _)| tokens)
}

/// A JSON credential stored like the provider tokens (keychain + `0600`
/// file), under its own name (e.g. one per MCP server).
pub fn save_named_secret(paths: &Paths, name: &str, value: &Value) -> Result<()> {
    let encoded = value.to_string();
    let file = paths.config_dir.join(format!("{name}.json"));
    let file_result = (|| -> Result<()> { crate::config::write_secret_file(&file, &encoded) })();
    let keyring_ok = keyring::Entry::new(KEYRING_SERVICE, &format!("{name}-{}", paths.profile))
        .map(|entry| entry.set_password(&encoded).is_ok())
        .unwrap_or(false);
    if file_result.is_ok() || keyring_ok {
        Ok(())
    } else {
        file_result
    }
}

pub fn load_named_secret(paths: &Paths, name: &str) -> Option<Value> {
    let file = paths.config_dir.join(format!("{name}.json"));
    if let Some(v) =
        crate::config::read_secret_file(&file).and_then(|t| serde_json::from_str(&t).ok())
    {
        return Some(v);
    }
    keyring::Entry::new(KEYRING_SERVICE, &format!("{name}-{}", paths.profile))
        .ok()?
        .get_password()
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

pub fn delete_named_secret(paths: &Paths, name: &str) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &format!("{name}-{}", paths.profile)) {
        let _ = entry.delete_credential();
    }
    let _ = std::fs::remove_file(paths.config_dir.join(format!("{name}.json")));
}

pub fn delete_oauth_tokens(paths: &Paths, auth: &AuthCfg) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &oauth_keyring_user(paths, auth)) {
        let _ = entry.delete_credential();
    }
    let file = oauth_tokens_file(paths, auth);
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_file(file.with_extension("json.tmp"));
}

fn delete_legacy_tokens(paths: &Paths) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &legacy_keyring_user(paths)) {
        let _ = entry.delete_credential();
    }
    let _ = std::fs::remove_file(paths.oauth_tokens_file());
    let _ = std::fs::remove_file(paths.oauth_tokens_file().with_extension("json.tmp"));
}

/// Every OAuth token set of the profile (`formless reset`): the providers
/// in `cfg`, the legacy profile-wide set, and any orphaned token files.
pub fn delete_all_oauth_tokens(paths: &Paths, cfg: Option<&crate::Config>) {
    if let Some(cfg) = cfg {
        delete_oauth_tokens(paths, &cfg.provider.auth);
        for p in cfg.providers.values() {
            delete_oauth_tokens(paths, &p.auth);
        }
    }
    delete_legacy_tokens(paths);
    if let Ok(dir) = std::fs::read_dir(&paths.config_dir) {
        for e in dir.flatten() {
            if e.file_name().to_string_lossy().starts_with("oauth_tokens-") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Whether a usable credential exists without performing a network refresh.
/// An expired OAuth access token still counts when it can be refreshed.
pub fn has_provider_credential(paths: &Paths, cfg: &ProviderCfg) -> bool {
    if cfg.oauth() {
        if env_access_token().is_some() {
            return true;
        }
        let Some(tokens) = load_oauth_tokens(paths, &cfg.auth) else {
            return false;
        };
        !tokens.is_expired(now_epoch()) || tokens.has_refresh_token()
    } else if cfg.auth.mode.eq_ignore_ascii_case("api_key") {
        load_api_key(paths).is_some()
    } else {
        false
    }
}

pub fn auth_status(paths: &Paths, cfg: &ProviderCfg) -> AuthStatus {
    if cfg.oauth() {
        if env_access_token().is_some() {
            return AuthStatus {
                mode: cfg.auth.mode.clone(),
                credential_present: true,
                expires_at: None,
                has_refresh_token: false,
                scope: None,
            };
        }
        match load_oauth_tokens(paths, &cfg.auth) {
            Some(tokens) => AuthStatus {
                mode: cfg.auth.mode.clone(),
                credential_present: true,
                expires_at: tokens.expires_at,
                has_refresh_token: tokens.has_refresh_token(),
                scope: tokens.scope.clone(),
            },
            None => AuthStatus {
                mode: cfg.auth.mode.clone(),
                credential_present: false,
                expires_at: None,
                has_refresh_token: false,
                scope: None,
            },
        }
    } else {
        AuthStatus {
            mode: cfg.auth.mode.clone(),
            credential_present: cfg.auth.mode.eq_ignore_ascii_case("api_key")
                && load_api_key(paths).is_some(),
            expires_at: None,
            has_refresh_token: false,
            scope: None,
        }
    }
}

fn env_access_token() -> Option<String> {
    std::env::var("FORMLESS_OAUTH_ACCESS_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_b64url(byte_count: usize) -> String {
    let mut bytes = vec![0u8; byte_count];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    b64url(&bytes)
}

pub fn generate_pkce_verifier() -> String {
    // 32 bytes encode to the 43-character minimum verifier length in RFC 7636.
    random_b64url(32)
}

pub fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    b64url(&digest)
}

pub fn generate_state() -> String {
    random_b64url(32)
}

fn percent_encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn percent_decode(input: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                if i + 2 >= bytes.len() {
                    bail!("truncated percent escape in callback query");
                }
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .context("invalid percent escape in callback query")?;
                out.push(
                    u8::from_str_radix(hex, 16)
                        .context("invalid percent escape in callback query")?,
                );
                i += 2;
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8(out).context("callback query is not valid UTF-8")
}

fn parse_query(query: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    if query.is_empty() {
        return Ok(out);
    }
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(key)?, percent_decode(value)?);
    }
    Ok(out)
}

fn validate_endpoint(name: &str, raw: &str) -> Result<()> {
    if raw.trim().is_empty() {
        bail!("provider.auth.{name} is required for OAuth mode");
    }
    let url = reqwest::Url::parse(raw.trim())
        .with_context(|| format!("provider.auth.{name} is not a valid URL"))?;
    let host = url.host_str().unwrap_or_default();
    let local_http =
        url.scheme() == "http" && matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]");
    if url.scheme() != "https" && !local_http {
        bail!("provider.auth.{name} must use HTTPS (HTTP is allowed only on localhost)");
    }
    Ok(())
}

fn validate_token_config(cfg: &AuthCfg) -> Result<()> {
    validate_endpoint("token_url", &cfg.token_url)?;
    if cfg.client_id.trim().is_empty() {
        bail!("provider.auth.client_id is required for OAuth mode");
    }
    Ok(())
}

/// Validate the complete provider-side OAuth configuration needed to log in.
pub fn validate_oauth_config(cfg: &ProviderCfg) -> Result<()> {
    if !cfg.oauth() {
        bail!(
            "provider.auth.mode must be \"oauth\" (found {:?})",
            cfg.auth.mode
        );
    }
    validate_endpoint("authorize_url", &cfg.auth.authorize_url)?;
    validate_token_config(&cfg.auth)
}

pub fn build_authorize_url(
    cfg: &AuthCfg,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
) -> Result<String> {
    validate_endpoint("authorize_url", &cfg.authorize_url)?;
    if cfg.client_id.trim().is_empty() {
        bail!("provider.auth.client_id is required for OAuth mode");
    }
    const RESERVED: &[&str] = &[
        "response_type",
        "client_id",
        "redirect_uri",
        "state",
        "code_challenge",
        "code_challenge_method",
        "scope",
    ];
    for key in cfg.extra_authorize_params.keys() {
        if RESERVED.contains(&key.as_str()) {
            bail!("provider.auth.extra_authorize_params may not override {key:?}");
        }
    }
    let mut params: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", cfg.client_id.trim()),
        ("redirect_uri", redirect_uri),
        ("state", state),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
    ];
    if !cfg.scopes.trim().is_empty() {
        params.push(("scope", cfg.scopes.trim()));
    }
    for (key, value) in &cfg.extra_authorize_params {
        params.push((key, value));
    }
    let separator = if cfg.authorize_url.contains('?') {
        '&'
    } else {
        '?'
    };
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    Ok(format!("{}{separator}{query}", cfg.authorize_url.trim()))
}

pub fn parse_token_response(
    value: &Value,
    now: i64,
    previous_refresh_token: Option<&str>,
) -> Result<OAuthTokens> {
    let access_token = value["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .context("OAuth token response has no access_token")?
        .to_string();
    let token_type = value["token_type"].as_str().unwrap_or("Bearer").to_string();
    if !token_type.eq_ignore_ascii_case("bearer") {
        bail!("unsupported OAuth token_type {token_type:?}; only Bearer tokens are supported");
    }
    let expires_in = match &value["expires_in"] {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse::<i64>().ok(),
        _ => None,
    };
    let expires_at = expires_in
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| now.checked_add(seconds));
    let refresh_token = value["refresh_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .or_else(|| previous_refresh_token.map(str::to_string));
    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at,
        token_type,
        scope: value["scope"].as_str().map(str::to_string),
    })
}

fn sanitized_oauth_error(body: &[u8]) -> String {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let mut parts = Vec::new();
    if let Some(value) = parsed {
        if let Some(error) = value["error"].as_str() {
            parts.push(error.to_string());
        }
        if let Some(description) = value["error_description"].as_str() {
            parts.push(description.to_string());
        }
    }
    let text = if parts.is_empty() {
        String::from_utf8_lossy(body).trim().to_string()
    } else {
        parts.join(": ")
    };
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect()
}

async fn token_request(
    cfg: &AuthCfg,
    standard_params: Vec<(&str, &str)>,
    previous_refresh_token: Option<&str>,
) -> Result<OAuthTokens> {
    validate_token_config(cfg)?;
    let mut params: BTreeMap<String, String> = cfg.extra_token_params.clone();
    for (key, value) in standard_params {
        params.insert(key.to_string(), value.to_string());
    }
    params.insert("client_id".into(), cfg.client_id.trim().to_string());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("formless/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build OAuth HTTP client")?;
    let mut request = client.post(cfg.token_url.trim()).form(&params);
    if let Ok(secret) = std::env::var("FORMLESS_OAUTH_CLIENT_SECRET") {
        if !secret.is_empty() {
            // Confidential-client authentication stays out of config.toml.
            request = request.basic_auth(cfg.client_id.trim(), Some(secret));
        }
    }
    let response = request.send().await.context("send OAuth token request")?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .context("read OAuth token response")?;
    if !status.is_success() {
        let detail = sanitized_oauth_error(&body);
        if detail.is_empty() {
            bail!("OAuth token endpoint returned {status}");
        }
        bail!("OAuth token endpoint returned {status}: {detail}");
    }
    let value: Value =
        serde_json::from_slice(&body).context("OAuth token response is not valid JSON")?;
    parse_token_response(&value, now_epoch(), previous_refresh_token)
}

pub async fn exchange_code(
    cfg: &AuthCfg,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthTokens> {
    token_request(
        cfg,
        vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", code_verifier),
        ],
        None,
    )
    .await
}

pub fn exchange_code_blocking(
    cfg: &AuthCfg,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<OAuthTokens> {
    run_blocking(exchange_code(cfg, code, redirect_uri, code_verifier))
}

pub async fn refresh_tokens(cfg: &AuthCfg, tokens: &OAuthTokens) -> Result<OAuthTokens> {
    let refresh = tokens
        .refresh_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .context("OAuth access token expired and no refresh_token is stored")?;
    token_request(
        cfg,
        vec![("grant_type", "refresh_token"), ("refresh_token", refresh)],
        Some(refresh),
    )
    .await
}

fn refresh_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Resolve the bearer credential for one provider call, refreshing and
/// persisting OAuth tokens when the stored access token is expired.
pub async fn resolve_provider_token(paths: &Paths, cfg: &ProviderCfg) -> Result<String> {
    if cfg.auth.mode.eq_ignore_ascii_case("api_key") {
        return Ok(load_api_key(paths).unwrap_or_default());
    }
    if !cfg.oauth() {
        bail!(
            "unsupported provider.auth.mode {:?}; expected \"api_key\" or \"oauth\"",
            cfg.auth.mode
        );
    }
    if let Some(token) = env_access_token() {
        return Ok(token);
    }

    // Serialize refreshes within this process: several Runs may otherwise
    // redeem the same rotating refresh token concurrently.
    let _guard = refresh_lock().lock().await;
    let (tokens, issuer) = stored_tokens(paths, cfg)?;
    if !tokens.is_expired(now_epoch()) {
        return Ok(tokens.access_token);
    }
    refresh_stored(paths, cfg, &tokens, issuer.as_deref()).await
}

/// After the server rejected `rejected` (401: revoked, or clock skew past
/// the expiry margin), refresh once even though it has not expired. A
/// token another caller already replaced is returned as is.
pub async fn force_refresh_provider_token(
    paths: &Paths,
    cfg: &ProviderCfg,
    rejected: &str,
) -> Result<String> {
    if !cfg.oauth() || env_access_token().is_some() {
        bail!("the provider rejected the credential (401)");
    }
    let _guard = refresh_lock().lock().await;
    let (tokens, issuer) = stored_tokens(paths, cfg)?;
    if tokens.access_token != rejected && !tokens.is_expired(now_epoch()) {
        return Ok(tokens.access_token);
    }
    refresh_stored(paths, cfg, &tokens, issuer.as_deref()).await
}

fn stored_tokens(paths: &Paths, cfg: &ProviderCfg) -> Result<(OAuthTokens, Option<String>)> {
    load_with_issuer(paths, &cfg.auth)
        .ok_or_else(|| anyhow!("OAuth login required: run `formless auth login` for this profile"))
}

async fn refresh_stored(
    paths: &Paths,
    cfg: &ProviderCfg,
    tokens: &OAuthTokens,
    issuer: Option<&str>,
) -> Result<String> {
    if !tokens.has_refresh_token() {
        bail!(
            "OAuth access token expired and no refresh_token is stored: run `formless auth login`"
        );
    }
    // A refresh token only ever goes back to the endpoint that issued it.
    if let Some(issuer) = issuer.filter(|i| *i != cfg.auth.token_url) {
        bail!(
            "stored OAuth tokens were issued by {issuer}, not {}; run `formless auth login`",
            cfg.auth.token_url
        );
    }
    let refreshed = refresh_tokens(&cfg.auth, tokens)
        .await
        .context("OAuth token refresh failed; run `formless auth login` if this persists")?;
    // Persist before use so a rotated refresh token cannot be lost after the
    // provider has already invalidated its predecessor.
    save_oauth_tokens(paths, &cfg.auth, &refreshed).context("persist refreshed OAuth tokens")?;
    Ok(refreshed.access_token)
}

pub fn resolve_provider_token_blocking(paths: &Paths, cfg: &ProviderCfg) -> Result<String> {
    run_blocking(resolve_provider_token(paths, cfg))
}

fn run_blocking<T>(future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
    if tokio::runtime::Handle::try_current().is_ok() {
        bail!("blocking OAuth helper called inside a Tokio runtime; use the async API");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build OAuth runtime")?;
    runtime.block_on(future)
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .context("write OAuth callback response")?;
    stream.flush().context("flush OAuth callback response")
}

fn state_matches(candidate: Option<&str>, expected: &str) -> bool {
    let Some(candidate) = candidate else {
        return false;
    };
    let a = candidate.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// Returns an authorization code, or `None` for an unrelated request such as
/// a browser favicon fetch. Authorization errors and state mismatches fail.
fn handle_callback(mut stream: TcpStream, expected_state: &str) -> Result<Option<String>> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("configure OAuth callback socket")?;
    let mut request = Vec::new();
    let mut buffer = [0u8; 2048];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
        if request.len() >= MAX_CALLBACK_BYTES {
            bail!("OAuth callback request is too large");
        }
        let read = stream
            .read(&mut buffer)
            .context("read OAuth callback request")?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let request_text = String::from_utf8_lossy(&request);
    let request_line = request_text.lines().next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or_default();
    let target = parts
        .next()
        .context("OAuth callback request has no target")?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        let _ = write_http_response(&mut stream, "404 Not Found", "text/plain", "Not found\n");
        return Ok(None);
    }
    let params = parse_query(query)?;
    if let Some(error) = params.get("error") {
        let description = params
            .get("error_description")
            .map(String::as_str)
            .unwrap_or_default();
        let body = format!(
            "<!doctype html><title>Formless login failed</title><h1>Login failed</h1><p>{}</p>",
            html_escape(&format!("{error}: {description}"))
        );
        let _ = write_http_response(
            &mut stream,
            "400 Bad Request",
            "text/html; charset=utf-8",
            &body,
        );
        bail!("OAuth provider returned {error}: {description}");
    }
    if !state_matches(params.get("state").map(String::as_str), expected_state) {
        let _ = write_http_response(
            &mut stream,
            "400 Bad Request",
            "text/plain",
            "OAuth state mismatch\n",
        );
        bail!("OAuth callback state mismatch");
    }
    let Some(code) = params.get("code").filter(|code| !code.is_empty()) else {
        let _ = write_http_response(
            &mut stream,
            "400 Bad Request",
            "text/plain",
            "OAuth callback has no authorization code\n",
        );
        bail!("OAuth callback has no authorization code");
    };
    let body = "<!doctype html><title>Formless authorization received</title>\
<h1>Authorization received</h1><p>Return to Formless; login will finish in the terminal.</p>";
    write_http_response(&mut stream, "200 OK", "text/html; charset=utf-8", body)?;
    Ok(Some(code.clone()))
}

/// Run one browser authorization-code + PKCE login and persist its tokens.
/// `on_url` receives the provider URL after the loopback callback is bound,
/// so `redirect_port = 0` (any free port) is safe.
pub fn login_with_callback<F>(paths: &Paths, cfg: &ProviderCfg, on_url: F) -> Result<OAuthTokens>
where
    F: FnMut(&str),
{
    login_with_timeout(paths, cfg, on_url, DEFAULT_LOGIN_TIMEOUT)
}

pub fn login_with_timeout<F>(
    paths: &Paths,
    cfg: &ProviderCfg,
    on_url: F,
    timeout: Duration,
) -> Result<OAuthTokens>
where
    F: FnMut(&str),
{
    validate_oauth_config(cfg)?;
    let auth = cfg.auth.clone();
    let tokens = run_code_flow(cfg.auth.redirect_port, |_| Ok(auth), on_url, timeout)?;
    save_oauth_tokens(paths, &cfg.auth, &tokens).context("persist OAuth tokens")?;
    Ok(tokens)
}

/// The authorization-code + PKCE flow on a loopback callback. `prepare`
/// gets the redirect URI before anything is sent and returns the client
/// settings to use (dynamic client registration needs the real URI).
/// Returns the tokens; storing them is the caller's job.
pub fn run_code_flow<P, F>(
    port: u16,
    prepare: P,
    mut on_url: F,
    timeout: Duration,
) -> Result<OAuthTokens>
where
    P: FnOnce(&str) -> Result<AuthCfg>,
    F: FnMut(&str),
{
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("bind OAuth callback on 127.0.0.1:{port}"))?;
    let actual_port = listener
        .local_addr()
        .context("read OAuth callback address")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{actual_port}/callback");
    let auth = prepare(&redirect_uri)?;
    let verifier = generate_pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    let state = generate_state();
    let url = build_authorize_url(&auth, &redirect_uri, &state, &challenge)?;
    on_url(&url);

    listener
        .set_nonblocking(true)
        .context("configure OAuth callback listener")?;
    let deadline = Instant::now() + timeout;
    let code = loop {
        if Instant::now() >= deadline {
            bail!("timed out waiting for OAuth browser callback");
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(code) = handle_callback(stream, &state)? {
                    break code;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error).context("accept OAuth browser callback"),
        }
    };

    exchange_code_blocking(&auth, &code, &redirect_uri, &verifier)
        .context("exchange OAuth authorization code")
}
