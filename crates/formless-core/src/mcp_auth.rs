//! OAuth 2.1 for remote MCP servers (SPEC §7.9, MCP authorization):
//! protected-resource metadata → authorization-server metadata → dynamic
//! client registration (when offered) → authorization code + PKCE on a
//! loopback callback, with the `resource` indicator. Tokens are stored per
//! server like the provider's (keychain + `0600` file), refreshed with
//! rotation, and never shown to the model or Tools.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::auth::{
    delete_named_secret, load_named_secret, now_epoch, refresh_tokens, run_code_flow,
    save_named_secret, OAuthTokens,
};
use crate::config::{AuthCfg, McpServerCfg};
use crate::Paths;

fn secret_name(server: &str) -> String {
    format!("mcp-oauth-{server}")
}

/// What a login stores: the tokens plus what refreshing them needs.
fn record(tokens: &OAuthTokens, auth: &AuthCfg) -> Value {
    json!({
        "tokens": tokens,
        "client_id": auth.client_id,
        "token_url": auth.token_url,
        "resource": auth.extra_token_params.get("resource"),
    })
}

fn auth_from_record(rec: &Value) -> AuthCfg {
    let mut auth = AuthCfg {
        mode: "oauth".into(),
        client_id: rec["client_id"].as_str().unwrap_or_default().into(),
        token_url: rec["token_url"].as_str().unwrap_or_default().into(),
        ..Default::default()
    };
    if let Some(r) = rec["resource"].as_str() {
        auth.extra_token_params.insert("resource".into(), r.into());
    }
    auth
}

/// Whether a server has stored OAuth credentials.
pub fn has_login(paths: &Paths, server: &str) -> bool {
    load_named_secret(paths, &secret_name(server)).is_some()
}

pub fn logout(paths: &Paths, server: &str) {
    delete_named_secret(paths, &secret_name(server));
}

/// A current access token for the server, refreshed (and the rotated
/// refresh token stored) when it has expired. None when not logged in.
pub async fn bearer(paths: &Paths, server: &str) -> Result<Option<String>> {
    let Some(rec) = load_named_secret(paths, &secret_name(server)) else {
        return Ok(None);
    };
    let tokens: OAuthTokens = serde_json::from_value(rec["tokens"].clone())?;
    if !tokens.is_expired(now_epoch()) {
        return Ok(Some(tokens.access_token));
    }
    refresh(paths, server).await.map(Some)
}

/// Refresh the stored tokens now (e.g. after a 401).
pub async fn refresh(paths: &Paths, server: &str) -> Result<String> {
    let rec = load_named_secret(paths, &secret_name(server)).with_context(|| {
        format!("MCP server '{server}' needs authorization: run `formless mcp login {server}`")
    })?;
    let tokens: OAuthTokens = serde_json::from_value(rec["tokens"].clone())?;
    let auth = auth_from_record(&rec);
    let fresh = refresh_tokens(&auth, &tokens).await.with_context(|| {
        format!("refresh MCP login for '{server}' (run `formless mcp login {server}` again)")
    })?;
    save_named_secret(paths, &secret_name(server), &record(&fresh, &auth))?;
    Ok(fresh.access_token)
}

/// `WWW-Authenticate: Bearer resource_metadata="…"` → that URL.
fn resource_metadata_hint(header: &str) -> Option<String> {
    let at = header.find("resource_metadata=")? + "resource_metadata=".len();
    let rest = header[at..].trim_start_matches('"');
    Some(rest.split(['"', ',']).next()?.trim().to_string())
}

fn get_json(client: &reqwest::blocking::Client, url: &str) -> Result<Value> {
    let resp = client
        .get(url)
        .header("accept", "application/json")
        .send()?
        .error_for_status()?;
    Ok(resp.json()?)
}

/// Discover the server's authorization setup (blocking; run outside Tokio).
fn discover(client: &reqwest::blocking::Client, server_url: &str) -> Result<(String, Value)> {
    let url = reqwest::Url::parse(server_url)?;
    // 1. Ask the server: a 401 names its protected-resource metadata.
    let probe = client
        .post(server_url)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}))
        .send()?;
    let hint = probe
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .and_then(resource_metadata_hint);
    let origin = url.origin().ascii_serialization();
    let pr_url = hint.unwrap_or_else(|| format!("{origin}/.well-known/oauth-protected-resource"));
    let pr = get_json(client, &pr_url).context("fetch protected-resource metadata")?;
    let resource = pr["resource"].as_str().unwrap_or(server_url).to_string();
    let issuer = pr["authorization_servers"][0]
        .as_str()
        .context("protected-resource metadata names no authorization server")?;
    // 2. Authorization-server metadata (RFC 8414, else OpenID discovery).
    let is = reqwest::Url::parse(issuer)?;
    let path = is.path().trim_end_matches('/');
    let base = is.origin().ascii_serialization();
    let candidates = [
        format!("{base}/.well-known/oauth-authorization-server{path}"),
        format!("{base}/.well-known/openid-configuration{path}"),
        format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        ),
    ];
    let meta = candidates
        .iter()
        .find_map(|u| get_json(client, u).ok())
        .context("fetch authorization-server metadata")?;
    Ok((resource, meta))
}

/// Log in to a remote MCP server: discovery, registration if needed, then
/// the browser step (`on_url` gets the URL to open on the user's machine).
/// Blocking; call it outside any Tokio runtime.
pub fn login<F>(paths: &Paths, server: &str, cfg: &McpServerCfg, on_url: F) -> Result<()>
where
    F: FnMut(&str),
{
    if cfg.url.is_empty() {
        return Err(anyhow!(
            "MCP server '{server}' has no url; OAuth applies to http servers"
        ));
    }
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let (resource, meta) = discover(&client, &cfg.url)?;
    let field = |k: &str| meta[k].as_str().map(str::to_string);
    let authorize_url = field("authorization_endpoint").context("no authorization_endpoint")?;
    let token_url = field("token_endpoint").context("no token_endpoint")?;
    let scopes = meta["scopes_supported"]
        .as_array()
        .map(|s| {
            s.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    let configured_client = cfg.oauth_client_id.clone();
    let registration = field("registration_endpoint");
    let mut used = AuthCfg::default();
    let tokens = run_code_flow(
        0,
        |redirect_uri| {
            let client_id = match (configured_client, registration) {
                (Some(id), _) if !id.is_empty() => id,
                (_, Some(endpoint)) => {
                    // Dynamic client registration (RFC 7591): a public client.
                    let reg: Value = client
                        .post(&endpoint)
                        .json(&json!({
                            "client_name": "Formless",
                            "redirect_uris": [redirect_uri],
                            "grant_types": ["authorization_code", "refresh_token"],
                            "response_types": ["code"],
                            "token_endpoint_auth_method": "none",
                        }))
                        .send()?
                        .error_for_status()?
                        .json()?;
                    reg["client_id"]
                        .as_str()
                        .context("registration returned no client_id")?
                        .to_string()
                }
                _ => {
                    return Err(anyhow!(
                    "the server offers no client registration; set oauth_client_id for '{server}'"
                ))
                }
            };
            let mut auth = AuthCfg {
                mode: "oauth".into(),
                authorize_url: authorize_url.clone(),
                token_url: token_url.clone(),
                client_id,
                scopes: scopes.clone(),
                ..Default::default()
            };
            // RFC 8707: tokens are bound to this server.
            auth.extra_authorize_params
                .insert("resource".into(), resource.clone());
            auth.extra_token_params
                .insert("resource".into(), resource.clone());
            used = auth.clone();
            Ok(auth)
        },
        on_url,
        std::time::Duration::from_secs(300),
    )?;
    save_named_secret(paths, &secret_name(server), &record(&tokens, &used))
}
