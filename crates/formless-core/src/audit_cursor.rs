//! Opaque audit pagination boundaries use durable entry ids, never SQLite rowids.
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

const PREFIX: &str = "audit:v1:";

pub(crate) fn encode(created_at: &str, id: &str) -> String {
    let payload = serde_json::to_vec(&(created_at, id)).expect("strings serialize");
    format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(payload))
}

/// Plain timestamps retain the legacy strict-before boundary. New cursor
/// versions and malformed payloads must fail, rather than becoming timestamps.
pub(crate) fn decode(before: Option<&str>) -> Result<(Option<String>, Option<String>)> {
    let Some(before) = before else {
        return Ok((None, None));
    };
    let Some(payload) = before.strip_prefix(PREFIX) else {
        if before.starts_with("audit:") {
            bail!("invalid audit cursor: unsupported version");
        }
        return Ok((Some(before.into()), None));
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("invalid audit cursor: expected base64url payload")?;
    let (created_at, id): (String, String) = serde_json::from_slice(&bytes)
        .context("invalid audit cursor: expected timestamp and entry id")?;
    if created_at.is_empty() || id.is_empty() {
        bail!("invalid audit cursor: timestamp and entry id must be nonempty");
    }
    Ok((Some(created_at), Some(id)))
}
