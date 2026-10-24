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
        if let Ok(v) = serde_json::from_str::<Value>(f["content"].as_str().unwrap_or_default()) {
            if check_bundle(&v).is_ok() {
                return Ok((name.clone(), Some(v), None));
            }
        }
    }
    bail!("no file in the gist is a Formless Tool bundle")
}

async fn get_json(http: &reqwest::Client, url: &str) -> Result<Value> {
    let resp = http
        .get(url)
        .header(
            "User-Agent",
            concat!("formless/", env!("CARGO_PKG_VERSION")),
        )
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .with_context(|| format!("fetch {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: HTTP {}", resp.status());
    }
    if resp.content_length().unwrap_or(0) > MAX_BYTES as u64 {
        bail!("{url} is larger than {} MB", MAX_BYTES / (1024 * 1024));
    }
    let bytes = resp.bytes().await?;
    if bytes.len() > MAX_BYTES {
        bail!("{url} is larger than {} MB", MAX_BYTES / (1024 * 1024));
    }
    serde_json::from_slice(&bytes).with_context(|| format!("{url} is not JSON"))
}

/// Fetch a bundle for preview: `{source, file?, bundle}`.
pub async fn fetch(http: &reqwest::Client, url: &str) -> Result<Value> {
    let (file, bundle) = match resolve(url)? {
        Source::Direct(u) => (None, get_json(http, &u).await?),
        Source::Gist(id) => {
            let gist = get_json(http, &format!("https://api.github.com/gists/{id}")).await?;
            match bundle_in_gist(&gist)? {
                (name, Some(b), _) => (Some(name), b),
                (name, None, Some(raw)) => (Some(name), get_json(http, &raw).await?),
                _ => unreachable!("bundle_in_gist returns a bundle or a raw URL"),
            }
        }
    };
    check_bundle(&bundle)?;
    Ok(serde_json::json!({ "source": url.trim(), "file": file, "bundle": bundle }))
}
