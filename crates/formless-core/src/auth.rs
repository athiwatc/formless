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
    pub warning: Option<String>,
}

/// Which token set a provider's OAuth config owns: its token endpoint and
/// client id, so two OAuth providers never share (or cross-refresh) tokens.
/// Stable issuer identity. Path case and query remain significant; endpoint
/// spelling (including a trailing slash) does not create a new login.
pub fn canonical_token_url(raw: &str) -> Result<String> {
    let mut url = transport_token_url(raw)?;
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(if path.is_empty() { "/" } else { &path });
    Ok(url.to_string())
}

/// A transport destination preserves path/query spelling, unlike slot identity.
fn transport_token_url(raw: &str) -> Result<reqwest::Url> {
    validate_endpoint("token_url", raw)?;
    let url = reqwest::Url::parse(raw.trim())?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        bail!("OAuth token_url must not contain userinfo or a fragment");
    }
    Ok(url)
}

fn raw_token_slot(endpoint: &str, client_id: &str) -> String {
    let digest = Sha256::digest(format!("{endpoint}\n{client_id}"));
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

fn token_slot(auth: &AuthCfg) -> String {
    raw_token_slot(
        &canonical_token_url(&auth.token_url).unwrap_or_else(|_| auth.token_url.clone()),
        auth.client_id.trim(),
    )
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
    validate_token_config(auth)?;
    let mut v = serde_json::to_value(tokens).context("encode OAuth tokens")?;
    v["oauth_record_version"] = Value::from(1);
    v["token_url"] = Value::String(canonical_token_url(&auth.token_url)?);
    v["token_endpoint"] = Value::String(transport_token_url(&auth.token_url)?.to_string());
    v["client_id"] = Value::String(auth.client_id.trim().to_string());
    Ok(v.to_string())
}

struct StoredTokens {
    tokens: OAuthTokens,
    issuer: Option<String>,
    endpoint: Option<String>,
    client_id: Option<String>,
}

fn decode_tokens(text: &str) -> Option<StoredTokens> {
    let v: Value = serde_json::from_str(text).ok()?;
    let issuer = v["token_url"].as_str().map(str::to_string);
    let endpoint = (v["oauth_record_version"] == 1)
        .then(|| v["token_endpoint"].as_str())
        .flatten()
        .and_then(|endpoint| transport_token_url(endpoint).ok())
        .map(|endpoint| endpoint.to_string());
    let client_id = v["client_id"].as_str().map(str::to_string);
    let tokens: OAuthTokens = serde_json::from_value(v).ok()?;
    (!tokens.access_token.is_empty()).then_some(StoredTokens {
        tokens,
        issuer,
        endpoint,
        client_id,
    })
}

/// A secret kept in the keychain and a `0600` file at once (#868). Each
/// copy carries when it was saved, a read takes the newer, a save that
/// can't replace an older copy fails (or removes it), and a delete says
/// which copy it couldn't remove.
struct DualStore {
    file: std::path::PathBuf,
    user: String,
}

impl DualStore {
    fn entry(&self) -> Option<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, &self.user).ok()
    }

    fn keyring_text(&self) -> Option<String> {
        self.entry()?.get_password().ok()
    }

    /// (saved at, the value): new copies are `{"v", "saved_at"}`; an older
    /// plain copy counts as saved at 0.
    fn unwrap(text: &str) -> Option<(i128, String)> {
        let v: Value = serde_json::from_str(text).ok()?;
        match (v.get("saved_at").and_then(Value::as_i64), v.get("v")) {
            (Some(at), Some(inner)) => Some((at as i128, inner.to_string())),
            _ => Some((0, text.to_string())),
        }
    }

    fn save(&self, value: &str) -> Result<()> {
        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        let inner: Value = serde_json::from_str(value).unwrap_or(Value::String(value.into()));
        let wrapped = serde_json::json!({"v": inner, "saved_at": at}).to_string();
        let file_result = crate::config::write_secret_file(&self.file, &wrapped);
        let key_result = self
            .entry()
            .ok_or_else(|| anyhow!("no keychain"))
            .and_then(|e| e.set_password(&wrapped).map_err(|e| anyhow!("{e}")));
        match (file_result, key_result) {
            (Ok(()), Ok(())) => Ok(()),
            // One copy written: the other must not keep an older value.
            (Ok(()), Err(_)) => {
                if self.keyring_text().is_some()
                    && self.entry().is_none_or(|e| e.delete_credential().is_err())
                {
                    bail!("the keychain holds an older copy that could not be replaced or removed");
                }
                Ok(())
            }
            (Err(e), Ok(())) => {
                if self.file.exists() && std::fs::remove_file(&self.file).is_err() {
                    return Err(e.context(format!(
                        "{} holds an older copy that could not be replaced or removed",
                        self.file.display()
                    )));
                }
                Ok(())
            }
            (Err(e), Err(_)) => Err(e),
        }
    }

    /// The newer of the two copies, retaining its ordering for old-slot lookup.
    fn load_versioned(&self) -> Option<(i128, String)> {
        let file = crate::config::read_secret_file(&self.file).and_then(|t| Self::unwrap(&t));
        let key = self.keyring_text().and_then(|t| Self::unwrap(&t));
        match (file, key) {
            (Some(f), Some(k)) => Some(if k.0 > f.0 { k } else { f }),
            (Some(one), None) | (None, Some(one)) => Some(one),
            (None, None) => None,
        }
    }

    fn load(&self) -> Option<String> {
        self.load_versioned().map(|(_, value)| value)
    }

    /// Remove both copies; what could not be removed.
    fn delete(&self) -> Vec<String> {
        let mut left = Vec::new();
        if let Some(e) = self.entry() {
            let _ = e.delete_credential();
        }
        if self.keyring_text().is_some() {
            left.push("the keychain copy".to_string());
        }
        let _ = std::fs::remove_file(&self.file);
        if self.file.exists() {
            left.push(self.file.display().to_string());
        }
        left
    }
}

fn token_store(paths: &Paths, auth: &AuthCfg) -> DualStore {
    DualStore {
        file: oauth_tokens_file(paths, auth),
        user: oauth_keyring_user(paths, auth),
    }
}

pub fn save_oauth_tokens(paths: &Paths, auth: &AuthCfg, tokens: &OAuthTokens) -> Result<()> {
    token_store(paths, auth).save(&encode_tokens(auth, tokens)?)
}

fn read_stored(file: &std::path::Path, user: &str) -> Option<StoredTokens> {
    let store = DualStore {
        file: file.to_path_buf(),
        user: user.to_string(),
    };
    decode_tokens(&store.load()?)
}

/// Compatibility lookup locates old stores for diagnostics/logout, and any
/// versioned records retained under an old name. A raw issuer/hash never
/// authorizes a credential: old migration could fabricate both from config.
/// Arbitrary keyring-only old hashes cannot be enumerated.
fn compatible_token_stores(paths: &Paths, auth: &AuthCfg) -> Vec<DualStore> {
    let mut slots = std::collections::BTreeSet::from([
        token_slot(auth),
        raw_token_slot(&auth.token_url, &auth.client_id),
    ]);
    // A finite compatibility lookup for a slash-only edit to the old raw hash.
    // This cannot enumerate arbitrary historical scheme/host/port spellings.
    let raw = auth.token_url.trim();
    let (path, query) = raw
        .split_once('?')
        .map_or((raw, String::new()), |(path, query)| {
            (path, format!("?{query}"))
        });
    if let Some(without_slash) = path.strip_suffix('/') {
        slots.insert(raw_token_slot(
            &format!("{without_slash}{query}"),
            &auth.client_id,
        ));
    }
    slots.insert(raw_token_slot(&format!("{path}/{query}"), &auth.client_id));
    if let Ok(dir) = std::fs::read_dir(&paths.config_dir) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(slot) = name
                .strip_prefix("oauth_tokens-")
                .and_then(|s| s.strip_suffix(".json"))
            else {
                continue;
            };
            if slot.len() != 12 || !slot.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let Some(text) = crate::config::read_secret_file(&e.path())
                .and_then(|s| DualStore::unwrap(&s).map(|(_, v)| v))
            else {
                continue;
            };
            let Some(issuer) = decode_tokens(&text).and_then(|stored| stored.issuer) else {
                continue;
            };
            // The old slot proves the client id as well as the recorded issuer.
            if slot == raw_token_slot(&issuer, &auth.client_id)
                && canonical_token_url(&issuer).ok() == canonical_token_url(&auth.token_url).ok()
                && canonical_token_url(&issuer).is_ok()
            {
                slots.insert(slot.to_string());
            }
        }
    }
    slots
        .into_iter()
        .map(|slot| DualStore {
            file: paths.config_dir.join(format!("oauth_tokens-{slot}.json")),
            user: format!("oauth-tokens-{}-{slot}", paths.profile),
        })
        .collect()
}

/// Only records written by the exact-endpoint codec prove transport. Historical
/// migration could stamp both issuer and raw slot from the current config,
/// irrespective of where its profile-wide token set was actually obtained.
fn proven_stored_tokens(
    auth: &AuthCfg,
    stored: StoredTokens,
) -> Option<(OAuthTokens, Option<String>)> {
    if !issuer_matches(auth, stored.issuer.as_deref()) {
        return None;
    }
    let endpoint = stored.endpoint?;
    if stored.client_id.as_deref() != Some(auth.client_id.trim())
        || canonical_token_url(&endpoint).ok() != canonical_token_url(&auth.token_url).ok()
    {
        return None;
    }
    Some((stored.tokens, Some(endpoint)))
}

fn issuer_matches(auth: &AuthCfg, issuer: Option<&str>) -> bool {
    match (
        canonical_token_url(&auth.token_url),
        issuer.and_then(|i| canonical_token_url(i).ok()),
    ) {
        (Ok(expected), Some(actual)) => expected == actual,
        _ => false,
    }
}

fn load_with_issuer(paths: &Paths, auth: &AuthCfg) -> Option<(OAuthTokens, Option<String>)> {
    // Prefer the current slot: a refreshed/rotated login supersedes old aliases.
    let current = token_store(paths, auth);
    if let Some(found) = current.load().and_then(|s| decode_tokens(&s)) {
        return proven_stored_tokens(auth, found);
    }
    let newest = compatible_token_stores(paths, auth)
        .iter()
        .filter_map(|store| {
            let (at, value) = store.load_versioned()?;
            let found = decode_tokens(&value)?;
            proven_stored_tokens(auth, found).map(|found| (at, found))
        })
        .max_by_key(|(at, _)| *at);
    if let Some((_, found)) = newest {
        return Some(found);
    }
    // Profile-wide legacy copies do not bind a client or prove an exact
    // endpoint. Keep them untouched for diagnostics and explicit re-login.
    None
}

/// Unlabelled or mismatched legacy credentials are retained for explicit reset,
/// never used or silently relabelled from today's default-provider settings.
pub fn oauth_storage_warning(paths: &Paths, auth: &AuthCfg) -> Option<String> {
    if load_with_issuer(paths, auth).is_some() {
        return None;
    }
    let legacy = read_stored(&paths.oauth_tokens_file(), &legacy_keyring_user(paths)).is_some();
    let invalid = compatible_token_stores(paths, auth)
        .iter()
        .any(|s| s.load().is_some());
    (legacy || invalid).then(|| "Stored OAuth credentials have no proven matching issuer and exact token endpoint; sign in again. Legacy issuer labels and raw slot hashes could have been derived from configuration, not a token exchange. Older keyring-only logins with an unknown endpoint spelling cannot be discovered automatically.".into())
}

/// The token set of the provider with `auth`, only with proven issuer identity.
pub fn load_oauth_tokens(paths: &Paths, auth: &AuthCfg) -> Option<OAuthTokens> {
    load_with_issuer(paths, auth).map(|(tokens, _)| tokens)
}

/// Login guidance includes the actual named provider (safe as a shell argument).
pub fn login_command(name: &str) -> String {
    let name = if name.is_empty() {
        crate::config::DEFAULT_PROVIDER
    } else {
        name
    };
    let quoted = if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
    {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "'\"'\"'"))
    };
    format!("formless auth login --provider {quoted}")
}

pub fn provider_login_command(paths: &Paths, cfg: &ProviderCfg) -> String {
    let name = crate::Config::load_file_only(paths)
        .ok()
        .and_then(|config| {
            if token_slot(&config.provider.auth) == token_slot(&cfg.auth) {
                Some(crate::config::DEFAULT_PROVIDER.to_string())
            } else {
                config
                    .providers
                    .iter()
                    .find(|(_, p)| token_slot(&p.auth) == token_slot(&cfg.auth))
                    .map(|(n, _)| n.clone())
            }
        })
        .unwrap_or_else(|| crate::config::DEFAULT_PROVIDER.to_string());
    login_command(&name)
}

/// A JSON credential stored like the provider tokens (keychain + `0600`
/// file), under its own name (e.g. one per MCP server).
fn named_store(paths: &Paths, name: &str) -> DualStore {
    DualStore {
        file: paths.config_dir.join(format!("{name}.json")),
        user: format!("{name}-{}", paths.profile),
    }
}

pub fn save_named_secret(paths: &Paths, name: &str, value: &Value) -> Result<()> {
    named_store(paths, name).save(&value.to_string())
}

pub fn load_named_secret(paths: &Paths, name: &str) -> Option<Value> {
    serde_json::from_str(&named_store(paths, name).load()?).ok()
}

/// Delete a named secret; what could not be removed (#868).
pub fn delete_named_secret(paths: &Paths, name: &str) -> Vec<String> {
    named_store(paths, name).delete()
}

/// Delete a provider's stored tokens; what could not be removed (#868).
pub fn delete_oauth_tokens(paths: &Paths, auth: &AuthCfg) -> Vec<String> {
    let mut left = Vec::new();
    for store in compatible_token_stores(paths, auth) {
        left.extend(store.delete());
        let _ = std::fs::remove_file(store.file.with_extension("json.tmp"));
    }
    if read_stored(&paths.oauth_tokens_file(), &legacy_keyring_user(paths))
        .is_some_and(|stored| issuer_matches(auth, stored.issuer.as_deref()))
    {
        left.extend(
            DualStore {
                file: paths.oauth_tokens_file(),
                user: legacy_keyring_user(paths),
            }
            .delete(),
        );
    }
    left
}

/// Whether the OS keychain works here (`doctor`): false means secrets
/// live only in their `0600` files.
pub fn keychain_available() -> bool {
    let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, "formless-probe") else {
        return false;
    };
    match entry.get_password() {
        Ok(_) | Err(keyring::Error::NoEntry) => true,
        Err(_) => false,
    }
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
                warning: None,
            };
        }
        match load_oauth_tokens(paths, &cfg.auth) {
            Some(tokens) => AuthStatus {
                mode: cfg.auth.mode.clone(),
                credential_present: true,
                expires_at: tokens.expires_at,
                has_refresh_token: tokens.has_refresh_token(),
                scope: tokens.scope.clone(),
                warning: None,
            },
            None => AuthStatus {
                mode: cfg.auth.mode.clone(),
                credential_present: false,
                expires_at: None,
                has_refresh_token: false,
                scope: None,
                warning: oauth_storage_warning(paths, &cfg.auth),
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
            warning: None,
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
    canonical_token_url(&cfg.token_url)?;
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

fn sanitized_oauth_error(body: &[u8], submitted_secrets: &[&str]) -> String {
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
    crate::secrets::redact_known(&text, submitted_secrets)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect()
}

/// Revoke `token` at an RFC 7009 endpoint (blocking-safe async).
pub async fn revoke_token(
    revocation_url: &str,
    client_id: &str,
    token: &str,
    hint: &str,
) -> Result<()> {
    validate_endpoint("revocation_url", revocation_url)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("formless/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut request = client.post(revocation_url.trim()).form(&[
        ("token", token),
        ("token_type_hint", hint),
        ("client_id", client_id.trim()),
    ]);
    if let Ok(secret) = std::env::var("FORMLESS_OAUTH_CLIENT_SECRET") {
        if !secret.is_empty() {
            request = request.basic_auth(client_id.trim(), Some(secret));
        }
    }
    let status = request
        .send()
        .await
        .context("send revocation request")?
        .status();
    if !status.is_success() {
        bail!("the revocation endpoint returned {status}");
    }
    Ok(())
}

/// `formless auth logout` (#866): revoke at the provider when it has a
/// revocation endpoint, delete both local copies, and say plainly what
/// is still in force. Lines for the user.
pub fn logout_provider(paths: &Paths, cfg: &ProviderCfg) -> Vec<String> {
    let mut out = Vec::new();
    if !cfg.oauth() {
        out.push(
            "provider.auth.mode is api_key: there are no OAuth tokens to delete; the API key stays (`formless reset` removes it)."
                .into(),
        );
        if std::env::var_os("FORMLESS_API_KEY").is_some() {
            out.push("FORMLESS_API_KEY is set in this environment and is still used.".into());
        }
        return out;
    }
    match load_oauth_tokens(paths, &cfg.auth) {
        None => out.push("No OAuth tokens were stored for this provider.".into()),
        Some(tokens) => {
            if cfg.auth.revocation_url.is_empty() {
                out.push(
                    "The provider has no revocation endpoint (provider.auth.revocation_url): only the local copies were deleted; the refresh token stays valid at the provider until it expires."
                        .into(),
                );
            } else {
                let (token, hint) = match tokens.refresh_token.as_deref().filter(|t| !t.is_empty())
                {
                    Some(r) => (r.to_string(), "refresh_token"),
                    None => (tokens.access_token.clone(), "access_token"),
                };
                match run_blocking(revoke_token(&cfg.auth.revocation_url, &cfg.auth.client_id, &token, hint)) {
                    Ok(()) => out.push("Revoked the login at the provider.".into()),
                    Err(e) => out.push(format!(
                        "Could not revoke at the provider ({e:#}); the local copies are deleted anyway."
                    )),
                }
            }
            let left = delete_oauth_tokens(paths, &cfg.auth);
            if left.is_empty() {
                out.push("Deleted the stored OAuth tokens (keychain and file).".into());
            } else {
                out.push(format!(
                    "Could not delete: {} (remove it by hand).",
                    left.join(", ")
                ));
            }
        }
    }
    if std::env::var_os("FORMLESS_OAUTH_ACCESS_TOKEN").is_some() {
        out.push(
            "FORMLESS_OAUTH_ACCESS_TOKEN is set in this environment and is still used for every request."
                .into(),
        );
    }
    out
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
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("formless/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build OAuth HTTP client")?;
    let mut request = client.post(cfg.token_url.trim()).form(&params);
    let client_secret = std::env::var("FORMLESS_OAUTH_CLIENT_SECRET")
        .ok()
        .filter(|s| !s.is_empty());
    let basic_credential = client_secret.as_ref().map(|secret| {
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{secret}", cfg.client_id.trim()))
    });
    if let Some(secret) = &client_secret {
        // Confidential-client authentication stays out of config.toml.
        request = request.basic_auth(cfg.client_id.trim(), Some(secret));
    }
    let response = request.send().await.context("send OAuth token request")?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .context("read OAuth token response")?;
    if !status.is_success() {
        let mut known: Vec<&str> = params
            .iter()
            .filter(|(key, _)| {
                matches!(
                    key.as_str(),
                    "refresh_token"
                        | "code"
                        | "code_verifier"
                        | "client_secret"
                        | "client_assertion"
                        | "assertion"
                        | "password"
                        | "token"
                )
            })
            .map(|(_, value)| value.as_str())
            .collect();
        known.extend(client_secret.as_deref());
        known.extend(basic_credential.as_deref());
        let detail = sanitized_oauth_error(&body, &known);
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

    // Still valid: no lock needed.
    let (tokens, _) = stored_tokens(paths, cfg)?;
    if !tokens.is_expired(now_epoch()) {
        return Ok(tokens.access_token);
    }
    // Serialize refreshes in this process and across processes (the
    // daemon and a CLI, #865), then look again: another may have done it.
    let _guard = refresh_lock().lock().await;
    let _file = refresh_file_lock(paths, &token_slot(&cfg.auth)).await?;
    let (tokens, issuer) = stored_tokens(paths, cfg)?;
    if !tokens.is_expired(now_epoch()) {
        return Ok(tokens.access_token);
    }
    refresh_stored(paths, cfg, &tokens, issuer.as_deref()).await
}

/// A lock every process of this profile shares for redeeming one
/// rotating refresh token (`config_dir/.refresh-<slot>.lock`), held until
/// the returned file is dropped.
pub async fn refresh_file_lock(paths: &Paths, slot: &str) -> Result<std::fs::File> {
    let path = paths.config_dir.join(format!(".refresh-{slot}.lock"));
    tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        f.lock()
            .with_context(|| format!("lock {}", path.display()))?;
        Ok(f)
    })
    .await
    .context("refresh lock task")?
}

/// The stored token as it is, never refreshed (`doctor`, `auth status`):
/// a diagnostic must not redeem a rotating refresh token (#865).
pub fn peek_provider_token(paths: &Paths, cfg: &ProviderCfg) -> Result<String> {
    if cfg.auth.mode.eq_ignore_ascii_case("api_key") {
        return Ok(load_api_key(paths).unwrap_or_default());
    }
    if let Some(token) = env_access_token() {
        return Ok(token);
    }
    let (tokens, _) = stored_tokens(paths, cfg)?;
    if tokens.is_expired(now_epoch()) {
        bail!(
            "the stored access token has expired{}; login: `{}`",
            if tokens.has_refresh_token() {
                "; the daemon refreshes it on the next request"
            } else {
                // Caller gets provider-specific guidance below.
                " and no refresh token is stored"
            },
            provider_login_command(paths, cfg)
        );
    }
    Ok(tokens.access_token)
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
    let _file = refresh_file_lock(paths, &token_slot(&cfg.auth)).await?;
    let (tokens, issuer) = stored_tokens(paths, cfg)?;
    if tokens.access_token != rejected && !tokens.is_expired(now_epoch()) {
        return Ok(tokens.access_token);
    }
    refresh_stored(paths, cfg, &tokens, issuer.as_deref()).await
}

fn stored_tokens(paths: &Paths, cfg: &ProviderCfg) -> Result<(OAuthTokens, Option<String>)> {
    load_with_issuer(paths, &cfg.auth).ok_or_else(|| {
        anyhow!(
            "OAuth login required for provider: run `{}`{}",
            provider_login_command(paths, cfg),
            oauth_storage_warning(paths, &cfg.auth)
                .map(|w| format!("; {w}"))
                .unwrap_or_default()
        )
    })
}

async fn refresh_stored(
    paths: &Paths,
    cfg: &ProviderCfg,
    tokens: &OAuthTokens,
    issuer: Option<&str>,
) -> Result<String> {
    if !tokens.has_refresh_token() {
        bail!(
            "OAuth access token expired and no refresh_token is stored: run `{}`",
            provider_login_command(paths, cfg)
        );
    }
    // A refresh token only ever goes back to the endpoint that issued it.
    if !issuer_matches(&cfg.auth, issuer) {
        bail!(
            "stored OAuth token issuer is unproven or different; run `{}`",
            provider_login_command(paths, cfg)
        );
    }
    let mut captured_auth = cfg.auth.clone();
    captured_auth.token_url = issuer
        .context("missing proven OAuth token endpoint")?
        .to_string();
    let refreshed = refresh_tokens(&captured_auth, tokens)
        .await
        .with_context(|| {
            format!(
                "OAuth token refresh failed; run `{}` if this persists",
                provider_login_command(paths, cfg)
            )
        })?;
    // Persist before use so a rotated refresh token cannot be lost after the
    // provider has already invalidated its predecessor.
    save_oauth_tokens(paths, &captured_auth, &refreshed)
        .context("persist refreshed OAuth tokens")?;
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

#[cfg(test)]
mod error_redaction_tests {
    #[test]
    fn echoed_opaque_credential_is_masked_before_error_truncation() {
        let secret = "opaque-credential-phrase";
        let padding = "z".repeat(300 - "invalid_grant: ".len() - 4);
        let body = serde_json::json!({"error":"invalid_grant", "error_description":format!("{padding}{secret}")}).to_string();
        let clean = super::sanitized_oauth_error(body.as_bytes(), &[secret]);
        assert!(clean.starts_with("invalid_grant: "));
        assert_eq!(clean.chars().count(), 300);
        assert!(clean.ends_with("[RED"), "masking must precede the cutoff");
        assert!(!clean.contains("opaq"));
    }
}
