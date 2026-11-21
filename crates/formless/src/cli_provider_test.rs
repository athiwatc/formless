//! Machine output for the two provider-test replies. Keep the allowlist nested
//! so additions to daemon diagnostics cannot silently expand this CLI report.
use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize)]
struct ModelReport {
    model: String,
    ok: bool,
    #[serde(default)]
    transient: bool,
    first_token_ms: Option<u64>,
    total_ms: Option<u64>,
    streaming: bool,
    reasoning: bool,
    tool_calling: bool,
    prompt_fallback: bool,
    error: Option<String>,
    tested_at: String,
    probe_usage: Option<ProbeUsage>,
}

#[derive(Deserialize, Serialize)]
struct ProbeUsage {
    requests_sent: u64,
    requests_with_reported_usage: u64,
    deadline_exceeded: bool,
    incomplete_transport: bool,
    usage_partial: bool,
    cost_partial: bool,
    reported_usage: Option<ReportedUsage>,
}

#[derive(Deserialize, Serialize)]
struct ReportedUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reported_cost: Option<f64>,
}

#[derive(Deserialize, Serialize)]
struct ModelsReport {
    provider: String,
    models: Vec<String>,
}

fn project<T: DeserializeOwned + Serialize>(value: Value) -> Result<String> {
    let report: T =
        serde_json::from_value(value).map_err(|_| anyhow!("invalid provider test reply"))?;
    Ok(serde_json::to_string_pretty(&report)?)
}

pub(super) fn model_json(value: Value) -> Result<String> {
    project::<ModelReport>(value)
}

pub(super) fn models_json(value: Value) -> Result<String> {
    project::<ModelsReport>(value)
}
