//! Tool bundles fetched from an https URL or a GitHub gist (SPEC §7.8).
//! Fetching only previews: importing stays a separate, explicit request,
//! under the same never-trusted rules as a file import.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

/// Largest bundle we fetch.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

/// Where a bundle URL really points.
#[derive(Debug, PartialEq)]
pub enum Source {
    /// Fetch this URL and read it as a bundle.
    Direct(String),
    /// A gist: read its files through the GitHub API.
    Gist(String),
}

/// Check the URL (https only) and map GitHub pages to what holds the file:
/// a gist page to its id, a `github.com/…/blob/…` page to its raw file.
pub fn resolve(url: &str) -> Result<Source> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| anyhow!("only https:// URLs can be imported"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = path.split(['?', '#']).next().unwrap_or_default();
    match host {
        "gist.github.com" | "gist.githubusercontent.com" => {
            let id = path
                .split('/')
                .filter(|s| !s.is_empty())
                .find(|s| s.len() >= 20 && s.chars().all(|c| c.is_ascii_hexdigit()))
                .ok_or_else(|| anyhow!("no gist id in {url}"))?;
            Ok(Source::Gist(id.to_string()))
        }
        "github.com" if path.split('/').nth(2) == Some("blob") => {
            let parts: Vec<&str> = path.splitn(4, '/').collect();
            Ok(Source::Direct(format!(
                "https://raw.githubusercontent.com/{}/{}/{}",
                parts[0],
                parts[1],
                parts.get(3).unwrap_or(&"")
            )))
        }
        _ => Ok(Source::Direct(url.to_string())),
    }
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

/// The first file of a gist (GitHub API response) that is a Tool bundle,
/// as (file name, bundle, raw URL when the API truncated its content).
pub fn bundle_in_gist(gist: &Value) -> Result<(String, Option<Value>, Option<String>)> {
    let files = gist["files"]
        .as_object()
        .ok_or_else(|| anyhow!("the gist has no files"))?;
    let mut names: Vec<&String> = files.keys().collect();
    names.sort();
    for name in names {
        let f = &files[name];
        if f["truncated"] == true {
            if let Some(raw) = f["raw_url"].as_str() {
                return Ok((name.clone(), None, Some(raw.to_string())));
            }
        }
        if let Ok(v) =
            crate::bundle_json::parse(f["content"].as_str().unwrap_or_default().as_bytes(), name)
        {
            if check_bundle(&v).is_ok() {
                return Ok((name.clone(), Some(v), None));
            }
        }
    }
    bail!("no file in the gist is a Formless Tool bundle")
}

/// Fetch `url` as JSON, at most [`MAX_BYTES`], following up to 5
/// redirects itself: every hop must be https and resolve only to public
/// addresses, and the connection is pinned to what was checked, so a
/// bundle URL can't make the daemon reach its own network (#825).
async fn get_json(url: &str, allow_private: bool) -> Result<Value> {
    use futures_util::StreamExt;
    let mut url = reqwest::Url::parse(url).with_context(|| format!("bad URL {url}"))?;
    for _ in 0..=5 {
        let client = checked_client(&url, allow_private)?;
        let resp = client
            .get(url.clone())
            .header(
                "User-Agent",
                concat!("formless/", env!("CARGO_PKG_VERSION")),
            )
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .with_context(|| format!("fetch {url}"))?;
        if resp.status().is_redirection() {
            let next = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("{url}: redirect without a Location"))?;
            url = url
                .join(next)
                .with_context(|| format!("bad redirect from {url}"))?;
            continue;
        }
        if !resp.status().is_success() {
            bail!("{url}: HTTP {}", resp.status());
        }
        if resp.content_length().unwrap_or(0) > MAX_BYTES as u64 {
            bail!("{url} is larger than {} MB", MAX_BYTES / (1024 * 1024));
        }
        // Read as a stream and stop at the cap, whatever the server says.
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.with_context(|| format!("read {url}"))?);
            if body.len() > MAX_BYTES {
                bail!("{url} is larger than {} MB", MAX_BYTES / (1024 * 1024));
            }
        }
        return crate::bundle_json::parse(&body, url.as_str());
    }
    bail!("{url}: too many redirects")
}

/// A client for one hop: https only, pinned to the host's checked
/// addresses, no redirects, no proxy.
fn checked_client(url: &reqwest::Url, allow_private: bool) -> Result<reqwest::Client> {
    use std::net::ToSocketAddrs;
    if url.scheme() != "https" && !allow_private {
        bail!("only https:// URLs can be imported ({url})");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("{url} has no host"))?
        .trim_matches(['[', ']'])
        .to_string();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {host}"))?
        .collect();
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
    let (file, bundle) = match resolve(url)? {
        Source::Direct(u) => (None, get_json(&u, allow_private).await?),
        Source::Gist(id) => {
            let gist =
                get_json(&format!("https://api.github.com/gists/{id}"), allow_private).await?;
            match bundle_in_gist(&gist)? {
                (name, Some(b), _) => (Some(name), b),
                // The raw URL is checked like any other (#825).
                (name, None, Some(raw)) => (Some(name), get_json(&raw, allow_private).await?),
                _ => unreachable!("bundle_in_gist returns a bundle or a raw URL"),
            }
        }
    };
    check_bundle(&bundle)?;
    Ok(serde_json::json!({ "source": url.trim(), "file": file, "bundle": bundle }))
}

#[cfg(test)]
mod fetch_tests {
    use super::*;

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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let e = get_json(&format!("http://{addr}/big"), true)
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("larger than"), "{e:#}");
    }
}
