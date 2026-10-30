//! OAuth 2.1 for remote MCP servers (SPEC §7.9, MCP authorization):
//! protected-resource metadata → authorization-server metadata → dynamic
//! client registration (when offered) → authorization code + PKCE on a
//! loopback callback, with the `resource` indicator. Tokens are stored per
//! server like the provider's (keychain + `0600` file), refreshed with
//! rotation, and never shown to the model or Tools.

use anyhow::{anyhow, bail, Context, Result};
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

/// What a login stores: the tokens plus what refreshing, revoking and
/// logging in again need, and which server address it is for (#866).
fn record(tokens: &OAuthTokens, auth: &AuthCfg, about: &LoginAbout) -> Value {
    json!({
        "tokens": tokens,
        "client_id": auth.client_id,
        "token_url": auth.token_url,
        "resource": auth.extra_token_params.get("resource"),
        "server_url": about.server_url,
        "revocation_url": about.revocation_url,
        "redirect_port": about.redirect_port,
    })
}

/// Facts about a login kept beside its tokens.
#[derive(Default, Clone)]
struct LoginAbout {
    server_url: String,
    revocation_url: Option<String>,
    redirect_port: Option<u16>,
}

fn about_from_record(rec: &Value) -> LoginAbout {
    LoginAbout {
        server_url: rec["server_url"].as_str().unwrap_or_default().to_string(),
        revocation_url: rec["revocation_url"].as_str().map(str::to_string),
        redirect_port: rec["redirect_port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok()),
    }
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

/// Log out of a server: revoke at its authorization server when it named
/// a revocation endpoint, then delete both local copies. Lines for the
/// user (#866).
pub fn logout(paths: &Paths, server: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(rec) = load_named_secret(paths, &secret_name(server)) {
        let about = about_from_record(&rec);
        let tokens: Option<OAuthTokens> = serde_json::from_value(rec["tokens"].clone()).ok();
        match (about.revocation_url.as_deref(), tokens) {
            (Some(url), Some(t)) => {
                let (token, hint) = match t.refresh_token.as_deref().filter(|r| !r.is_empty()) {
                    Some(r) => (r.to_string(), "refresh_token"),
                    None => (t.access_token.clone(), "access_token"),
                };
                let client_id = rec["client_id"].as_str().unwrap_or_default().to_string();
                let revoke = crate::auth::revoke_token(url, &client_id, &token, hint);
                let done = match tokio::runtime::Handle::try_current() {
                    Ok(_) => Err(anyhow!("revocation skipped inside the daemon runtime")),
                    Err(_) => tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(anyhow::Error::from)
                        .and_then(|rt| rt.block_on(revoke)),
                };
                out.push(match done {
                    Ok(()) => {
                        format!("Revoked the login for '{server}' at its authorization server.")
                    }
                    Err(e) => format!(
                        "Could not revoke the login for '{server}' ({e:#}); local copies deleted."
                    ),
                });
            }
            _ => out.push(format!(
                "'{server}' named no revocation endpoint: only the local copies were deleted."
            )),
        }
    }
    let left = delete_named_secret(paths, &secret_name(server));
    if !left.is_empty() {
        out.push(format!("Could not delete: {}.", left.join(", ")));
    }
    out
}

/// A current access token for the server at `server_url`, refreshed (and
/// the rotated refresh token stored) when it has expired. None when not
/// logged in. A login made for another address is refused, so a server
/// re-added under an old name never gets another server's token (#866).
pub async fn bearer(paths: &Paths, server: &str, server_url: &str) -> Result<Option<String>> {
    let Some(rec) = load_named_secret(paths, &secret_name(server)) else {
        return Ok(None);
    };
    let about = about_from_record(&rec);
    if !about.server_url.is_empty() && about.server_url != server_url {
        bail!(
            "the stored login for MCP server '{server}' was made for {}, not {server_url}: run `formless mcp login {server}`",
            about.server_url
        );
    }
    let tokens: OAuthTokens = serde_json::from_value(rec["tokens"].clone())?;
    if !tokens.is_expired(now_epoch()) {
        return Ok(Some(tokens.access_token));
    }
    refresh(paths, server).await.map(Some)
}

fn refresh_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Refresh the stored tokens now (e.g. after a 401). Serialised within
/// this process and across processes; a token another caller already
/// replaced is used as is (#865).
pub async fn refresh(paths: &Paths, server: &str) -> Result<String> {
    let before = load_named_secret(paths, &secret_name(server))
        .and_then(|r| r["tokens"]["access_token"].as_str().map(str::to_string));
    let _guard = refresh_lock().lock().await;
    let _file = crate::auth::refresh_file_lock(paths, &format!("mcp-{server}")).await?;
    let rec = load_named_secret(paths, &secret_name(server)).with_context(|| {
        format!("MCP server '{server}' needs authorization: run `formless mcp login {server}`")
    })?;
    let tokens: OAuthTokens = serde_json::from_value(rec["tokens"].clone())?;
    if before.as_deref() != Some(tokens.access_token.as_str()) && !tokens.is_expired(now_epoch()) {
        return Ok(tokens.access_token);
    }
    let auth = auth_from_record(&rec);
    let fresh = refresh_tokens(&auth, &tokens).await.with_context(|| {
        format!("refresh MCP login for '{server}' (run `formless mcp login {server}` again)")
    })?;
    save_named_secret(
        paths,
        &secret_name(server),
        &record(&fresh, &auth, &about_from_record(&rec)),
    )?;
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
pub fn login<F>(paths: &Paths, server: &str, cfg: &McpServerCfg, mut on_url: F) -> Result<()>
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
    // A client registered by an earlier login for this server and token
    // endpoint is reused (same callback port), not registered again (#866).
    let earlier = load_named_secret(paths, &secret_name(server)).filter(|r| {
        r["token_url"].as_str() == Some(token_url.as_str())
            && r["server_url"].as_str() == Some(cfg.url.as_str())
    });
    let reuse = earlier.as_ref().and_then(|r| {
        Some((
            r["client_id"]
                .as_str()
                .filter(|c| !c.is_empty())?
                .to_string(),
            about_from_record(r).redirect_port?,
        ))
    });
    let mut about = LoginAbout {
        server_url: cfg.url.clone(),
        revocation_url: field("revocation_endpoint"),
        redirect_port: None,
    };
    match login_flow(
        &client,
        &resource,
        &authorize_url,
        &token_url,
        &scopes,
        configured_client.clone(),
        registration.clone(),
        reuse.clone(),
        server,
        &mut about,
        &mut on_url,
    ) {
        Ok((tokens, used)) => {
            save_named_secret(paths, &secret_name(server), &record(&tokens, &used, &about))
        }
        // The old callback port is taken: register afresh on a new one.
        Err(e) if reuse.is_some() && format!("{e:#}").contains("bind OAuth callback") => {
            let (tokens, used) = login_flow(
                &client,
                &resource,
                &authorize_url,
                &token_url,
                &scopes,
                configured_client,
                registration,
                None,
                server,
                &mut about,
                &mut on_url,
            )?;
            save_named_secret(paths, &secret_name(server), &record(&tokens, &used, &about))
        }
        Err(e) => Err(e),
    }
}

/// One browser login: with `reuse` (client id, callback port) the earlier
/// registration is used; otherwise a configured client id, else dynamic
/// registration.
#[allow(clippy::too_many_arguments)]
fn login_flow<F: FnMut(&str)>(
    client: &reqwest::blocking::Client,
    resource: &str,
    authorize_url: &str,
    token_url: &str,
    scopes: &str,
    configured_client: Option<String>,
    registration: Option<String>,
    reuse: Option<(String, u16)>,
    server: &str,
    about: &mut LoginAbout,
    on_url: F,
) -> Result<(OAuthTokens, AuthCfg)> {
    let mut used = AuthCfg::default();
    let port = reuse.as_ref().map_or(0, |(_, p)| *p);
    let mut seen_port = None;
    let tokens = run_code_flow(
        port,
        |redirect_uri| {
            seen_port = reqwest::Url::parse(redirect_uri)
                .ok()
                .and_then(|u| u.port());
            let client_id = match (reuse.map(|(c, _)| c).or(configured_client), registration) {
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
                authorize_url: authorize_url.to_string(),
                token_url: token_url.to_string(),
                client_id,
                scopes: scopes.to_string(),
                ..Default::default()
            };
            // RFC 8707: tokens are bound to this server.
            auth.extra_authorize_params
                .insert("resource".into(), resource.to_string());
            auth.extra_token_params
                .insert("resource".into(), resource.to_string());
            used = auth.clone();
            Ok(auth)
        },
        on_url,
        std::time::Duration::from_secs(300),
    )?;
    about.redirect_port = seen_port;
    Ok((tokens, used))
}
