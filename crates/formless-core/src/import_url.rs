//! Tool bundles fetched from an https URL or a GitHub gist (SPEC §7.8).
//! Fetching only previews: importing stays a separate, explicit request,
//! under the same never-trusted rules as a file import.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

/// Largest bundle we fetch.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
const FETCH_BUDGET: Duration = Duration::from_secs(20);
type ResolveHost = fn(&str, u16, Instant) -> Result<Vec<SocketAddr>, String>;

/// Where a bundle URL really points.
#[derive(Debug, PartialEq)]
pub enum Source {
    /// Fetch this URL and read it as a bundle.
    Direct(String),
    /// A gist page: read its files from the specified revision through the GitHub API.
    Gist {
        id: String,
        revision: Option<String>,
        file_anchor: Option<String>,
    },
}

/// Check the URL (https only) and map GitHub pages to what holds the file:
/// a gist page to its GitHub API selection and a GitHub blob page to its raw file.
pub fn resolve(url: &str) -> Result<Source> {
    resolve_with_label(url).map(|(source, _)| source)
}

fn display_url(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    let _ = url.set_password(None);
    let _ = url.set_username("");
    url.to_string()
}

fn resolve_with_label(url: &str) -> Result<(Source, String)> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url).context("invalid bundle URL")?;
    if parsed.scheme() != "https" {
        bail!("only https:// URLs can be imported");
    }
    let label = if !parsed.username().is_empty() || parsed.password().is_some() {
        display_url(&parsed)
    } else {
        url.to_string()
    };
    let host = parsed.host_str().unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(host);
    let path = parsed.path().strip_prefix('/').unwrap_or(parsed.path());
    // Rewritten targets only use the standard service port. Userinfo stays
    // with direct requests and is never carried to the API/raw authority.
    let source = match (parsed.port(), host) {
        (None, "gist.github.com" | "gist.githubusercontent.com") => {
            resolve_gist(&parsed, host, path, &label)?
        }
        (None, "github.com") if path.split('/').nth(2) == Some("blob") => {
            let parts: Vec<&str> = path.splitn(4, '/').collect();
            Source::Direct(format!(
                "https://raw.githubusercontent.com/{}/{}/{}",
                parts[0],
                parts[1],
                parts.get(3).unwrap_or(&"")
            ))
        }
        _ => Source::Direct(url.to_string()),
    };
    Ok((source, label))
}

fn resolve_gist(url: &reqwest::Url, host: &str, path: &str, label: &str) -> Result<Source> {
    let mut parts = path.trim_end_matches('/').split('/');
    let is_id = |part: &str| part.len() >= 20 && part.bytes().all(|c| c.is_ascii_hexdigit());
    let first = parts.next().unwrap_or_default();
    let id = if is_id(first) {
        first
    } else {
        parts
            .next()
            .filter(|part| is_id(part))
            .ok_or_else(|| anyhow!("no gist id in {label}"))?
    };
    let next = parts.next();
    if host == "gist.githubusercontent.com" && next == Some("raw") {
        return Ok(Source::Direct(display_url(url)));
    }
    let revision = match next {
        None => None,
        Some(sha)
            if (7..=40).contains(&sha.len())
                && sha.bytes().all(|c| c.is_ascii_hexdigit())
                && parts.next().is_none() =>
        {
            Some(sha.to_owned())
        }
        Some(_) => bail!("invalid gist revision in {label}"),
    };
    Ok(Source::Gist {
        id: id.to_owned(),
        revision,
        file_anchor: url
            .fragment()
            .filter(|fragment| fragment.starts_with("file-"))
            .map(str::to_owned),
    })
}

fn gist_file_anchor(name: &str) -> String {
    format!("file-{}", name.to_lowercase().replace('.', "-"))
}

/// A Tool bundle (`formless.tool/1`) or a registry export
/// (`formless.tools/1`); anything else is refused.
pub fn check_bundle(v: &Value) -> Result<()> {
    match v["format"].as_str() {
        Some("formless.tool/1") => Ok(()),
        Some("formless.tools/1") if v["tools"].is_array() => Ok(()),
        Some(other) => bail!("not a Tool bundle (format {other})"),
        None => bail!("not a Tool bundle (no \"format\": export one with `formless tools export`)"),
    }
}

enum GistChoices {
    Inline(String, Value),
    Raw(Vec<(String, String)>),
}

fn gist_choices(gist: &Value, file_anchor: Option<&str>) -> Result<GistChoices> {
    let files = gist["files"]
        .as_object()
        .ok_or_else(|| anyhow!("the gist has no files"))?;
    let mut names: Vec<_> = files.keys().collect();
    if let Some(anchor) = file_anchor {
        names.retain(|name| gist_file_anchor(name) == anchor);
        match names.len() {
            0 => bail!("no gist file matches #{anchor}"),
            1 => (),
            _ => bail!("more than one gist file matches #{anchor}"),
        }
    }
    names.sort();
    // A complete bundle wins over unrelated truncated files anywhere in the
    // gist. Truncated inline content is never treated as the complete file.
    for name in &names {
        let file = &files[name.as_str()];
        if file["truncated"] == true {
            continue;
        }
        if let Ok(bundle) = crate::bundle_json::parse(
            file["content"].as_str().unwrap_or_default().as_bytes(),
            name,
        ) {
            if check_bundle(&bundle).is_ok() {
                return Ok(GistChoices::Inline((*name).clone(), bundle));
            }
        }
    }
    let mut raw: Vec<_> = names
        .into_iter()
        .filter_map(|name| {
            let file = &files[name.as_str()];
            if file["truncated"] != true {
                return None;
            }
            file["raw_url"]
                .as_str()
                .map(|url| (name.clone(), url.to_string()))
        })
        .collect();
    let json = |name: &str| {
        name.rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("json"))
    };
    raw.sort_by(|a, b| (!json(&a.0)).cmp(&!json(&b.0)).then_with(|| a.0.cmp(&b.0)));
    if raw.is_empty() {
        if let Some(anchor) = file_anchor {
            bail!("gist file #{anchor} is not a Formless Tool bundle");
        }
        bail!("no file in the gist is a Formless Tool bundle");
    }
    Ok(GistChoices::Raw(raw))
}

/// Prefer a complete valid gist bundle; otherwise return the first raw
/// candidate (JSON filenames first), which still needs fetching/validation.
pub fn bundle_in_gist(gist: &Value) -> Result<(String, Option<Value>, Option<String>)> {
    match gist_choices(gist, None)? {
        GistChoices::Inline(name, bundle) => Ok((name, Some(bundle), None)),
        GistChoices::Raw(raw) => {
            let (name, url) = raw
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("no raw gist candidate"))?;
            Ok((name, None, Some(url)))
        }
    }
}

async fn fetch_gist_bundle<F, Fut>(
    gist: &Value,
    file_anchor: Option<&str>,
    mut fetch: F,
) -> Result<(String, Value)>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    match gist_choices(gist, file_anchor)? {
        GistChoices::Inline(name, bundle) => Ok((name, bundle)),
        GistChoices::Raw(raw) => {
            let mut last_error = None;
            for (name, url) in raw {
                match fetch(url).await.and_then(|bundle| {
                    check_bundle(&bundle)?;
                    Ok(bundle)
                }) {
                    Ok(bundle) => return Ok((name, bundle)),
                    Err(error) => last_error = Some(error),
                }
            }
            let error = last_error.unwrap_or_else(|| anyhow!("no raw gist candidate"));
            Err(match file_anchor {
                Some(anchor) => error.context(format!("cannot import gist file #{anchor}")),
                None => error.context(
                    "no file in the gist is a Formless Tool bundle after checking truncated files",
                ),
            })
        }
    }
}

#[derive(Debug)]
struct DownloadTooLarge(String);
impl std::fmt::Display for DownloadTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is larger than {} MB",
            self.0,
            MAX_BYTES / (1024 * 1024)
        )
    }
}
impl std::error::Error for DownloadTooLarge {}

fn gist_metadata_error(error: anyhow::Error) -> anyhow::Error {
    if error.is::<DownloadTooLarge>() {
        error.context(
            "gist metadata is too large for preview; download the selected bundle JSON from the Gist and import the saved local file with `formless tools import FILE`",
        )
    } else {
        error
    }
}

/// Fetch `url` as JSON, at most [`MAX_BYTES`], following up to 5
/// redirects itself: every hop must be https and resolve only to public
/// addresses, and the connection is pinned to what was checked, so a
/// bundle URL can't make the daemon reach its own network (#825).
async fn get_json_until(
    url: &str,
    allow_private: bool,
    deadline: Instant,
    resolve: ResolveHost,
) -> Result<Value> {
    use futures_util::StreamExt;
    let mut url = reqwest::Url::parse(url).context("invalid bundle URL")?;
    for _ in 0..=5 {
        let label = display_url(&url);
        let client = checked_client(&url, allow_private, deadline, resolve).await?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("timed out fetching {label}");
        }
        let resp = client
            .get(url.clone())
            .header(
                "User-Agent",
                concat!("formless/", env!("CARGO_PKG_VERSION")),
            )
            .timeout(remaining)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .with_context(|| format!("fetch {label}"))?;
        if resp.status().is_redirection() {
            let next = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("{label}: redirect without a Location"))?;
            url = url
                .join(next)
                .with_context(|| format!("bad redirect from {label}"))?;
            continue;
        }
        if !resp.status().is_success() {
            bail!("{label}: HTTP {}", resp.status());
        }
        if resp.content_length().unwrap_or(0) > MAX_BYTES as u64 {
            return Err(DownloadTooLarge(label).into());
        }
        // Read as a stream and stop at the cap, whatever the server says.
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(
                &chunk
                    .map_err(reqwest::Error::without_url)
                    .with_context(|| format!("read {label}"))?,
            );
            if body.len() > MAX_BYTES {
                return Err(DownloadTooLarge(label).into());
            }
        }
        return crate::bundle_json::parse(&body, &label);
    }
    bail!("{}: too many redirects", display_url(&url))
}

/// A client for one hop: https only, pinned to the host's checked
/// addresses, no redirects, no proxy.
async fn checked_client(
    url: &reqwest::Url,
    allow_private: bool,
    deadline: Instant,
    resolve: ResolveHost,
) -> Result<reqwest::Client> {
    if url.scheme() != "https" && !allow_private {
        bail!("only https:// URLs can be imported ({})", display_url(url));
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("{} has no host", display_url(url)))?
        .trim_matches(['[', ']'])
        .to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = if let Ok(ip) = host.parse::<IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        if Instant::now() >= deadline {
            bail!("timed out resolving {host}");
        }
        let name = host.clone();
        let lookup = tokio::task::spawn_blocking(move || resolve(&name, port, deadline));
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), lookup)
            .await
            .with_context(|| format!("timed out resolving {host}"))?
            .with_context(|| format!("cannot resolve {host}"))?
            .map_err(anyhow::Error::msg)?
    };
    if addrs.is_empty() {
        bail!("cannot resolve {host}");
    }
    if !allow_private {
        if let Some(bad) = addrs.iter().find(|a| formless_runtime::non_public(a.ip())) {
            bail!("{host} resolves to {}, a private or local address; bundles are only fetched from public hosts", bad.ip());
        }
    }
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve_to_addrs(&host, &addrs)
        .build()?)
}

/// Fetch a bundle for preview: `{source, file?, bundle}`.
pub async fn fetch(url: &str) -> Result<Value> {
    fetch_from(url, false).await
}

/// [`fetch`]; `allow_private` lets tests use a local server.
pub async fn fetch_from(url: &str, allow_private: bool) -> Result<Value> {
    fetch_from_until(
        url,
        allow_private,
        Instant::now() + FETCH_BUDGET,
        formless_runtime::resolve_host_until,
    )
    .await
}

async fn fetch_from_until(
    url: &str,
    allow_private: bool,
    deadline: Instant,
    resolve: ResolveHost,
) -> Result<Value> {
    let (source, label) = resolve_with_label(url)?;
    let (file, bundle) = match source {
        Source::Direct(u) => (
            None,
            get_json_until(&u, allow_private, deadline, resolve).await?,
        ),
        Source::Gist {
            id,
            revision,
            file_anchor,
        } => {
            let mut api = format!("https://api.github.com/gists/{id}");
            if let Some(sha) = revision {
                api.push('/');
                api.push_str(&sha);
            }
            let gist = get_json_until(&api, allow_private, deadline, resolve)
                .await
                .map_err(gist_metadata_error)?;
            // Every raw candidate uses the same checked, capped downloader.
            let (name, bundle) =
                fetch_gist_bundle(&gist, file_anchor.as_deref(), |raw| async move {
                    get_json_until(&raw, allow_private, deadline, resolve).await
                })
                .await?;
            (Some(name), bundle)
        }
    };
    check_bundle(&bundle)?;
    Ok(serde_json::json!({ "source": label, "file": file, "bundle": bundle }))
}

#[cfg(test)]
mod fetch_tests {
    use super::*;

    async fn get_json(url: &str, allow_private: bool) -> Result<Value> {
        get_json_until(
            url,
            allow_private,
            Instant::now() + FETCH_BUDGET,
            formless_runtime::resolve_host_until,
        )
        .await
    }

    #[tokio::test]
    async fn stalled_bundle_dns_expires_without_blocking_the_executor() {
        let resolve: ResolveHost = |host, _port, _deadline| {
            assert_eq!(host, "fixture.invalid");
            std::thread::sleep(Duration::from_millis(500));
            Err("fixture resolver finished after the deadline".into())
        };
        let started = Instant::now();
        let (result, heartbeat) = tokio::join!(
            fetch_from_until(
                "https://fixture.invalid/file.json",
                false,
                started + Duration::from_millis(100),
                resolve
            ),
            async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                started.elapsed()
            },
        );
        let error = result.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("timed out resolving fixture.invalid"),
            "{error:#}"
        );
        assert!(
            heartbeat < Duration::from_millis(350),
            "executor was blocked for {heartbeat:?}"
        );
    }

    #[tokio::test]
    async fn redirects_share_the_original_bundle_fetch_budget() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route(
                "/first",
                axum::routing::get(|| async {
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    axum::response::Redirect::temporary("/last")
                }),
            )
            .route(
                "/last",
                axum::routing::get(|| async {
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    axum::Json(serde_json::json!({"format":"formless.tool/1"}))
                }),
            );
        let peer = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let result = get_json_until(
            &format!("http://{address}/first"),
            true,
            Instant::now() + Duration::from_millis(200),
            formless_runtime::resolve_host_until,
        )
        .await;
        peer.abort();
        let error = result.unwrap_err();
        let last = format!("http://{address}/last");
        let message = error.to_string();
        // The shared budget can expire before the second request starts or
        // while reqwest is waiting for that hop.
        let expired_before_send = message == format!("timed out fetching {last}");
        let timed_out_during_request = (message == format!("fetch {last}")
            || message == format!("read {last}"))
            && error.chain().any(|source| {
                source
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(reqwest::Error::is_timeout)
            });
        assert!(expired_before_send || timed_out_during_request, "{error:#}");
    }

    #[test]
    fn source_labels_drop_userinfo_and_keep_ordinary_url_spelling() {
        let direct = "https://fixture-user:fixture-secret@example.invalid/t.json?version=2";
        let (source, label) = resolve_with_label(direct).unwrap();
        assert_eq!(source, Source::Direct(direct.into()));
        assert_eq!(label, "https://example.invalid/t.json?version=2");
        let (_, label) = resolve_with_label("HTTPS://Example.Invalid:443/t.json").unwrap();
        assert_eq!(label, "HTTPS://Example.Invalid:443/t.json");
    }

    #[tokio::test]
    async fn fetch_diagnostics_drop_userinfo_on_original_and_redirected_urls() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let destination = format!("http://redirect-name:redirect-secret@{addr}/denied");
        let app = axum::Router::new()
            .route(
                "/denied",
                axum::routing::get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
            )
            .route("/invalid", axum::routing::get(|| async { "{invalid" }))
            .route(
                "/redirect",
                axum::routing::get(move || async move {
                    axum::response::Redirect::temporary(&destination)
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (path, diagnostic_path, marker) in [
            ("denied", "denied", "HTTP 401"),
            ("invalid", "invalid", "not a JSON bundle"),
            ("redirect", "denied", "HTTP 401"),
        ] {
            let input = format!("http://fixture-name:fixture-secret@{addr}/{path}");
            let error = get_json(&input, true).await.unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains(&format!("http://{addr}/{diagnostic_path}")));
            assert!(message.contains(marker), "{message}");
            for credential in [
                "fixture-name",
                "fixture-secret",
                "redirect-name",
                "redirect-secret",
            ] {
                assert!(!message.contains(credential), "{message}");
            }
        }
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn connection_failure_keeps_userinfo_out_of_the_error_chain() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
        });
        let url = format!("http://fixture-name:fixture-secret@{addr}/closed");
        let (response, accepted) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(get_json(&url, true), server)
        })
        .await
        .expect("the request and fixture listener should finish");
        accepted.unwrap();
        let error = response.unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(&format!("http://{addr}/closed")));
        assert!(
            !message.contains("fixture-name") && !message.contains("fixture-secret"),
            "{message}"
        );
    }

    #[test]
    fn gist_prefers_complete_bundle_over_truncated_earlier_files() {
        let bundle = serde_json::json!({"format":"formless.tool/1","name":"fixture"});
        let gist = serde_json::json!({"files":{
            "README.md":{"truncated":true,"raw_url":"https://example.invalid/readme"},
            "z-tool.json":{"content":bundle.to_string()}
        }});
        assert_eq!(
            bundle_in_gist(&gist).unwrap(),
            ("z-tool.json".into(), Some(bundle), None)
        );
    }

    #[tokio::test]
    async fn anchored_gist_file_controls_selection_and_raw_fetch() {
        let bundle = |name| serde_json::json!({"format":"formless.tool/1","name":name});
        let gist = serde_json::json!({"files":{
            "A.json":{"content":bundle("first").to_string()},
            "Fetch.JSON":{"content":bundle("anchor").to_string()},
            "broken.txt":{"content":"not json"},
            "Data.json":{"truncated":true,"raw_url":"https://example.invalid/data"}
        }});
        let unexpected = |_| std::future::ready(Err(anyhow!("unexpected raw fetch")));
        let (name, value) = fetch_gist_bundle(&gist, Some("file-fetch-json"), unexpected)
            .await
            .unwrap();
        assert_eq!(name, "Fetch.JSON");
        assert_eq!(value, bundle("anchor"));

        let (name, value) = fetch_gist_bundle(&gist, Some("file-data-json"), |url| {
            assert_eq!(url, "https://example.invalid/data");
            std::future::ready(Ok(bundle("raw")))
        })
        .await
        .unwrap();
        assert_eq!(name, "Data.json");
        assert_eq!(value, bundle("raw"));

        for (anchor, message) in [
            ("file-broken-txt", "is not a Formless Tool bundle"),
            ("file-absent-json", "no gist file matches"),
        ] {
            let error = fetch_gist_bundle(&gist, Some(anchor), unexpected)
                .await
                .unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }

        let ambiguous = serde_json::json!({"files":{
            "Foo.Bar.json":{"content":bundle("dot").to_string()},
            "foo-bar.json":{"content":bundle("hyphen").to_string()}
        }});
        let error = fetch_gist_bundle(&ambiguous, Some("file-foo-bar-json"), unexpected)
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("more than one gist file matches"));
    }

    #[tokio::test]
    async fn complete_gist_bundle_needs_no_raw_fetch() {
        let bundle = serde_json::json!({"format":"formless.tool/1","name":"inline"});
        let gist = serde_json::json!({"files":{
            "README.md":{"truncated":true,"raw_url":"https://example.invalid/readme"},
            "tool.json":{"content":bundle.to_string()}
        }});
        let mut calls = Vec::new();
        let result = fetch_gist_bundle(&gist, None, |url| {
            calls.push(url);
            std::future::ready(Err(anyhow!("unexpected raw fetch")))
        })
        .await
        .unwrap();
        assert_eq!(result, ("tool.json".into(), bundle));
        assert!(calls.is_empty());
    }

    #[tokio::test]
    async fn raw_gist_candidates_prefer_json_and_skip_failed_nonbundles() {
        let bundle = serde_json::json!({"format":"formless.tool/1","name":"raw"});
        let gist = serde_json::json!({"files":{
            "README.md":{"truncated":true,"raw_url":"readme"},
            "b.json":{"truncated":true,"raw_url":"broken"},
            "c.JSON":{"truncated":true,"raw_url":"not-bundle"},
            "z.json":{"truncated":true,"raw_url":"bundle"}
        }});
        assert_eq!(bundle_in_gist(&gist).unwrap().0, "b.json");
        let mut calls = Vec::new();
        let result = fetch_gist_bundle(&gist, None, |url| {
            calls.push(url.clone());
            std::future::ready(match url.as_str() {
                "broken" => Err(anyhow!("not JSON")),
                "not-bundle" => Ok(serde_json::json!({"format":"unrelated/1"})),
                "bundle" => Ok(bundle.clone()),
                _ => Err(anyhow!("unexpected fallback")),
            })
        })
        .await
        .unwrap();
        assert_eq!(result, ("z.json".into(), bundle));
        assert_eq!(calls, ["broken", "not-bundle", "bundle"]);
    }

    #[tokio::test]
    async fn raw_gist_non_json_names_can_still_hold_bundles_and_exhaustion_is_clear() {
        let bundle = serde_json::json!({"format":"formless.tool/1"});
        let gist = serde_json::json!({"files":{
            "README.md":{"truncated":true,"raw_url":"valid"},
            "tool.json":{"truncated":true,"raw_url":"invalid"}
        }});
        let mut calls = Vec::new();
        let result = fetch_gist_bundle(&gist, None, |url| {
            calls.push(url.clone());
            std::future::ready(Ok(if url == "valid" {
                bundle.clone()
            } else {
                serde_json::json!({})
            }))
        })
        .await
        .unwrap();
        assert_eq!(result, ("README.md".into(), bundle));
        assert_eq!(calls, ["invalid", "valid"]);
        let error = fetch_gist_bundle(&gist, None, |_| {
            std::future::ready(Err(anyhow!("fixture unavailable")))
        })
        .await
        .unwrap_err();
        let detail = format!("{error:#}");
        assert!(detail.contains("no file in the gist") && detail.contains("fixture unavailable"));
    }

    #[tokio::test]
    async fn raw_gist_candidates_keep_checked_download_refusals() {
        let gist = serde_json::json!({"files":{"tool.json":{"truncated":true,"raw_url":"http://localhost:9/never-requested"}}});
        let error = fetch_gist_bundle(
            &gist,
            None,
            |url| async move { get_json(&url, false).await },
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("only https://"));
    }

    #[test]
    fn oversized_gist_guidance_avoids_the_raw_metadata_loop() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            resolve(&format!("https://gist.github.com/ada/{id}")).unwrap(),
            Source::Gist {
                id: id.into(),
                revision: None,
                file_anchor: None
            }
        );
        let raw = format!("https://gist.githubusercontent.com/ada/{id}/raw/abcdef/tool.json");
        assert_eq!(resolve(&raw).unwrap(), Source::Direct(raw));
        let error = gist_metadata_error(DownloadTooLarge("gist metadata".into()).into());
        let advice = error.to_string();
        assert!(
            advice.contains("download")
                && advice.contains("local file")
                && advice.contains("formless tools import FILE"),
            "{advice}"
        );
        assert!(!advice.contains("raw file URL directly"), "{advice}");
        assert!(format!("{error:#}").contains("larger than 2 MB"));
    }

    #[test]
    fn ordinary_gist_metadata_errors_are_not_mislabeled_as_oversized() {
        let error = gist_metadata_error(anyhow!("fixture metadata unavailable"));
        assert_eq!(error.to_string(), "fixture metadata unavailable");
    }

    #[tokio::test]
    async fn fetched_and_embedded_bundles_use_shared_encoding_errors() {
        let bundle = serde_json::json!({"format":"formless.tool/1","name":"fixture"});
        let body = format!("\u{feff}{bundle}");
        let app = axum::Router::new()
            .route(
                "/bom",
                axum::routing::get(move || {
                    let body = body.clone();
                    async move { body }
                }),
            )
            .route(
                "/le",
                axum::routing::get(|| async { vec![0xffu8, 0xfe, 123, 0] }),
            )
            .route(
                "/be",
                axum::routing::get(|| async { vec![0xfeu8, 0xff, 0, 123] }),
            )
            .route("/broken", axum::routing::get(|| async { "\u{feff}{bad" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (path, expected) in [
            ("bom", None),
            ("le", Some("UTF-16")),
            ("be", Some("UTF-16")),
            ("broken", Some("not a JSON bundle")),
        ] {
            let url = format!("http://{addr}/{path}");
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(2), get_json(&url, true))
                    .await
                    .unwrap();
            if let Some(expected) = expected {
                let error = result.unwrap_err().to_string();
                assert!(error.contains(&url) && error.contains(expected), "{error}");
            } else {
                assert_eq!(result.unwrap(), bundle);
            }
        }
        let gist =
            serde_json::json!({"files":{"bom.json":{"content":format!("\u{feff}{bundle}")}}});
        assert_eq!(
            bundle_in_gist(&gist).unwrap(),
            ("bom.json".into(), Some(bundle), None)
        );
        server.abort();
        let _ = server.await;
    }

    /// A bundle URL naming a local host is refused before anything is sent.
    #[tokio::test]
    async fn private_hosts_are_refused() {
        let e = fetch("https://localhost:9/bundle.json").await.unwrap_err();
        assert!(format!("{e:#}").contains("private or local"), "{e:#}");
    }

    /// A body with no length, past the cap, stops at the cap instead of
    /// filling memory (#825).
    #[tokio::test]
    async fn an_endless_body_stops_at_the_cap() {
        use futures_util::stream;
        let app = axum::Router::new().route(
            "/big",
            axum::routing::get(|| async {
                let chunks =
                    stream::iter((0..64).map(|_| Ok::<_, std::io::Error>(vec![b' '; 64 * 1024])));
                axum::body::Body::from_stream(chunks)
            }),
        );
        let app = app.route(
            "/declared",
            axum::routing::get(|| async { vec![b' '; MAX_BYTES + 1] }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for path in ["big", "declared"] {
            let e = get_json(&format!("http://{addr}/{path}"), true)
                .await
                .unwrap_err();
            assert!(format!("{e:#}").contains("larger than"), "{e:#}");
            let metadata_error = gist_metadata_error(e);
            assert!(metadata_error
                .to_string()
                .contains("gist metadata is too large"));
            assert!(metadata_error
                .to_string()
                .contains("formless tools import FILE"));
        }
    }
}
