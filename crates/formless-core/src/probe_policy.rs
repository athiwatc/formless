//! One shared budget for a logical operational probe and all of its subcalls.
//! No task is spawned here: timeout drops the caller's current request future.
use crate::{config::ProviderCfg, provider::Usage};
use anyhow::{bail, Result};
use formless_runtime::LockExt;
use serde_json::{json, Value};
use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

const TIME: Duration = Duration::from_secs(30);
const SENDS: usize = 8;
const TOKENS: u64 = 128;
#[derive(Clone)]
pub(crate) struct ProbePolicy(Arc<Inner>);
struct Inner {
    deadline: tokio::time::Instant,
    sends: AtomicUsize,
    maximum: usize,
    tokens: u64,
    ceiling: AtomicU64,
    completion_field: Mutex<Option<bool>>,
    repaired_field: AtomicBool,
    timed_out: AtomicBool,
    incomplete_transport: AtomicBool,
    observed: Mutex<Observed>,
    model: Mutex<Option<ModelIdentity>>,
}

struct ModelIdentity {
    requested: String,
    mismatch: Option<String>,
}

fn same_model(requested: &str, reported: &str) -> bool {
    if requested == reported {
        return true;
    }
    let alias = if requested.starts_with("claude-") {
        requested.strip_suffix("-latest").unwrap_or(requested)
    } else {
        requested
    };
    let Some(suffix) = reported.strip_prefix(alias) else {
        return false;
    };
    if suffix == ":latest" && !requested.contains(':') {
        return true;
    }
    let Some(date) = suffix.strip_prefix('-') else {
        return false;
    };
    (date.len() == 10 && chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok())
        || (alias.starts_with("claude-")
            && date.len() == 8
            && chrono::NaiveDate::parse_from_str(date, "%Y%m%d").is_ok())
}
#[derive(Default)]
struct Observed {
    reports: usize,
    complete: usize,
    input_reports: usize,
    output_reports: usize,
    input: u64,
    output: u64,
    reasoning: u64,
    cached: u64,
    cache_write: u64,
    cost: f64,
    cost_reports: usize,
}
impl Default for ProbePolicy {
    fn default() -> Self {
        Self::new(TIME, SENDS, TOKENS)
    }
}
impl ProbePolicy {
    fn new(duration: Duration, sends: usize, tokens: u64) -> Self {
        Self(Arc::new(Inner {
            deadline: tokio::time::Instant::now() + duration,
            sends: AtomicUsize::new(0),
            maximum: sends,
            tokens,
            ceiling: AtomicU64::new(tokens),
            completion_field: Mutex::new(None),
            repaired_field: AtomicBool::new(false),
            timed_out: AtomicBool::new(false),
            incomplete_transport: AtomicBool::new(false),
            observed: Mutex::new(Observed::default()),
            model: Mutex::new(None),
        }))
    }

    /// Azure's URL identifies the deployment, while the reply names its
    /// backing model. Those identifiers describe different things.
    pub(crate) fn verify_model(&self, model: &str, base_url: &str) {
        let deployment = reqwest::Url::parse(base_url).ok().is_some_and(|url| {
            url.path()
                .trim_end_matches('/')
                .rsplit_once('/')
                .is_some_and(|(path, name)| path.ends_with("/openai/deployments") && name == model)
        });
        if !deployment {
            *self.0.model.locked() = Some(ModelIdentity {
                requested: model.into(),
                mismatch: None,
            });
        }
    }

    pub(crate) fn observe_model(&self, response: &Value) {
        let mut guard = self.0.model.locked();
        let Some(identity) = guard
            .as_mut()
            .filter(|identity| identity.mismatch.is_none())
        else {
            return;
        };
        let reports = [
            &response["model"],
            &response["response"]["model"],
            &response["message"]["model"],
        ];
        for reported in reports.into_iter().filter_map(Value::as_str).map(str::trim) {
            if !reported.is_empty() && !same_model(&identity.requested, reported) {
                identity.mismatch = Some(
                    reported
                        .chars()
                        .take(160)
                        .flat_map(char::escape_default)
                        .collect(),
                );
                break;
            }
        }
    }

    pub(crate) fn model_mismatch(&self) -> Option<String> {
        self.0
            .model
            .locked()
            .as_ref()
            .and_then(|model| model.mismatch.clone())
    }
    #[cfg(test)]
    pub(crate) fn limited(duration: Duration, sends: usize) -> Self {
        Self::new(duration, sends, TOKENS)
    }
    pub(crate) async fn run<T>(&self, work: impl Future<Output = T>) -> Result<T> {
        match tokio::time::timeout_at(self.0.deadline, work).await {
            Ok(value) => Ok(value),
            Err(_) => {
                self.0.timed_out.store(true, Ordering::SeqCst);
                Err(anyhow::anyhow!("model probe logical deadline exceeded"))
            }
        }
    }
    fn check(&self) -> Result<()> {
        if tokio::time::Instant::now() >= self.0.deadline {
            self.0.timed_out.store(true, Ordering::SeqCst);
            bail!("model probe logical deadline exceeded")
        }
        Ok(())
    }
    pub(crate) fn before_send(&self) -> Result<()> {
        self.check()?;
        self.0
            .sends
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |sent| {
                (sent < self.0.maximum).then_some(sent + 1)
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "model probe physical POST budget exhausted (maximum {})",
                    self.0.maximum
                )
            })?;
        Ok(())
    }
    pub(crate) fn retain_stricter_caps(&self, body: &Value) {
        for object in [Some(body), body.get("options")].into_iter().flatten() {
            for key in [
                "max_tokens",
                "max_completion_tokens",
                "max_output_tokens",
                "max_new_tokens",
                "num_predict",
            ] {
                if let Some(cap) = object
                    .get(key)
                    .and_then(Value::as_u64)
                    .filter(|cap| *cap > 0)
                {
                    self.0
                        .ceiling
                        .fetch_min(cap.min(self.0.tokens), Ordering::SeqCst);
                }
            }
        }
    }

    /// Runs after vendor fields and repairs on EVERY physical attempt. A
    /// rejected cap can switch spelling, but never disappear or increase.
    pub(crate) fn finalize(&self, body: &mut Value, cfg: &ProviderCfg, model: &str) -> Result<()> {
        self.check()?;
        self.retain_stricter_caps(body);
        let ceiling = self.0.ceiling.load(Ordering::SeqCst);
        let completion = if !cfg.anthropic() && !cfg.responses() {
            let mut field = self.0.completion_field.locked();
            *field.get_or_insert_with(|| {
                let model = model
                    .rsplit('/')
                    .next()
                    .unwrap_or(model)
                    .to_ascii_lowercase();
                (body.get("max_completion_tokens").is_some() && body.get("max_tokens").is_none())
                    || ["o1", "o3", "o4", "gpt-5"]
                        .iter()
                        .any(|prefix| model == *prefix || model.starts_with(&format!("{prefix}-")))
            })
        } else {
            false
        };
        let object = body
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("model probe body is not an object"))?;
        // Token ceilings apply per choice on Chat. Operational probes must not
        // inherit owner-configured multi-choice/search costs from ordinary Runs.
        if !cfg.anthropic() && !cfg.responses() && object.contains_key("n") {
            object.insert("n".into(), json!(1));
        } else {
            object.remove("n");
        }
        const MULTIPLIERS: &[&str] = &[
            "best_of",
            "num_return_sequences",
            "num_outputs",
            "num_generations",
            "candidate_count",
            "candidateCount",
            "num_beams",
            "beam_width",
            "use_beam_search",
        ];
        for key in MULTIPLIERS {
            object.remove(*key);
        }
        for key in [
            "max_tokens",
            "max_completion_tokens",
            "max_output_tokens",
            "thinking",
            "reasoning",
            "reasoning_effort",
        ] {
            object.remove(key);
        }
        let key = if cfg.anthropic() {
            "max_tokens"
        } else if cfg.responses() {
            "max_output_tokens"
        } else if completion {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        object.insert(key.into(), json!(ceiling));
        for key in ["max_new_tokens", "num_predict"] {
            if object.contains_key(key) {
                object.insert(key.into(), json!(ceiling));
            }
        }
        if let Some(options) = object.get_mut("options").and_then(Value::as_object_mut) {
            options.remove("n");
            for key in MULTIPLIERS {
                options.remove(*key);
            }
            options.insert("num_predict".into(), json!(ceiling));
            for key in [
                "max_tokens",
                "max_completion_tokens",
                "max_output_tokens",
                "max_new_tokens",
            ] {
                if options.contains_key(key) {
                    options.insert(key.into(), json!(ceiling));
                }
            }
            options.remove("thinking");
            options.remove("reasoning");
            options.remove("reasoning_effort");
            if options.contains_key("think") {
                options.insert("think".into(), json!(false));
            }
        }
        Ok(())
    }
    pub(crate) fn repair_token_field(&self, text: &str) -> bool {
        let mut field = self.0.completion_field.locked();
        let Some(completion) = *field else {
            return false;
        };
        let current = if completion {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        let next = if completion {
            "max_tokens"
        } else {
            "max_completion_tokens"
        };
        let lower = text.to_ascii_lowercase();
        let param = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| v["error"]["param"].as_str().map(str::to_string));
        if !lower.contains(current)
            || !["unsupported", "not supported", "unknown", "unrecognized"]
                .iter()
                .any(|word| lower.contains(word))
            || !(lower.contains(next) || param.as_deref() == Some(current))
        {
            return false;
        }
        if self.0.repaired_field.swap(true, Ordering::SeqCst) {
            return false;
        }
        *field = Some(!completion);
        true
    }
    /// This is an observer only. The provider's existing hook remains the
    /// sole billing commit; known counters are not confused with absent ones.
    pub(crate) fn observe(&self, usage: &Usage) {
        let mut sum = self.0.observed.locked();
        if usage.reported != 0 || usage.cost.is_some() {
            sum.reports += 1;
        }
        if usage.reported & 3 == 3 {
            sum.complete += 1;
        }
        if usage.reported & 1 != 0 {
            sum.input_reports += 1;
            sum.input = sum.input.saturating_add(usage.prompt_tokens);
        }
        if usage.reported & 2 != 0 {
            sum.output_reports += 1;
            sum.output = sum.output.saturating_add(usage.completion_tokens);
        }
        sum.reasoning = sum.reasoning.saturating_add(usage.reasoning_tokens);
        sum.cached = sum.cached.saturating_add(usage.cached_tokens);
        sum.cache_write = sum.cache_write.saturating_add(usage.cache_write_tokens);
        if let Some(cost) = usage.cost.filter(|cost| cost.is_finite() && *cost >= 0.0) {
            let total = sum.cost + cost;
            if total.is_finite() {
                sum.cost = total;
                sum.cost_reports += 1;
            }
        }
    }
    pub(crate) fn mark_transport_incomplete(&self) {
        self.0.incomplete_transport.store(true, Ordering::SeqCst);
    }
    pub(crate) fn report(&self) -> Value {
        let sent = self.0.sends.load(Ordering::SeqCst);
        let timed_out = self.0.timed_out.load(Ordering::SeqCst);
        let incomplete = self.0.incomplete_transport.load(Ordering::SeqCst);
        let sum = self.0.observed.locked();
        let mut reported = serde_json::Map::new();
        if sum.input_reports > 0 {
            reported.insert("prompt_tokens".into(), json!(sum.input));
        }
        if sum.output_reports > 0 {
            reported.insert("completion_tokens".into(), json!(sum.output));
        }
        if sum.reasoning > 0 {
            reported.insert("reasoning_tokens".into(), json!(sum.reasoning));
        }
        if sum.cached > 0 {
            reported.insert("cached_tokens".into(), json!(sum.cached));
        }
        if sum.cache_write > 0 {
            reported.insert("cache_write_tokens".into(), json!(sum.cache_write));
        }
        if sum.cost_reports > 0 {
            reported.insert("reported_cost".into(), json!(sum.cost));
        }
        json!({"requests_sent":sent,"requests_with_reported_usage":sum.reports,"deadline_exceeded":timed_out,"incomplete_transport":incomplete,"usage_partial":timed_out||incomplete||sum.complete<sent,"cost_partial":timed_out||incomplete||sum.cost_reports<sent,"reported_usage":if reported.is_empty(){Value::Null}else{Value::Object(reported)}})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_identity_accepts_documented_snapshot_and_deployment_names() {
        for (requested, reported, equal) in [
            ("gpt-4o", "gpt-4o-2024-08-06", true),
            ("gpt-4o-2024-08-06", "gpt-4o-2024-11-20", false),
            ("gpt-4o", "gpt-4o-mini-2024-07-18", false),
            ("gpt-4o", "gpt-4o-2024-21-45", false),
            (
                "claude-3-5-sonnet-latest",
                "claude-3-5-sonnet-20241022",
                true,
            ),
            ("qwen3", "qwen3:latest", true),
            ("qwen3:8b", "qwen3:latest", false),
        ] {
            assert_eq!(
                same_model(requested, reported),
                equal,
                "{requested} vs {reported}"
            );
        }
        let policy = ProbePolicy::default();
        policy.verify_model("daily", "https://example.test/openai/deployments/daily");
        policy.observe_model(&json!({"model":"gpt-4o-2024-08-06"}));
        assert!(policy.model_mismatch().is_none());
        policy.verify_model("daily", "https://example.test/v1");
        policy.observe_model(&json!({"response":{"model":"other"}}));
        assert_eq!(policy.model_mismatch().as_deref(), Some("other"));
    }
    #[test]
    fn finalization_overrules_hostile_fields_and_never_drops_a_rejected_cap() {
        let policy = ProbePolicy::default();
        let cfg = ProviderCfg::default();
        let mut body = json!({"max_tokens":999999,"max_completion_tokens":999999,"max_output_tokens":999999,"thinking":{"budget_tokens":90000},"reasoning_effort":"high","options":{"num_predict":-1}});
        policy.finalize(&mut body, &cfg, "plain").unwrap();
        assert_eq!(body["max_tokens"], 128);
        assert!(body.get("max_completion_tokens").is_none() && body.get("thinking").is_none());
        assert_eq!(body["options"]["num_predict"], 128);
        assert!(policy.repair_token_field("max_tokens is not supported; use max_completion_tokens"));
        policy.finalize(&mut body, &cfg, "plain").unwrap();
        assert_eq!(body["max_completion_tokens"], 128);
        assert!(body.get("max_tokens").is_none());
        assert!(
            !policy.repair_token_field("max_completion_tokens unsupported; max_tokens required")
        );
    }
    #[test]
    fn stricter_endpoint_caps_survive_hostile_vendor_overrides() {
        let policy = ProbePolicy::default();
        let cfg = ProviderCfg {
            api_style: "anthropic".into(),
            ..Default::default()
        };
        policy.retain_stricter_caps(&json!({"max_tokens":64}));
        let mut body =
            json!({"max_tokens":99999,"max_output_tokens":-1,"options":{"num_predict":99999}});
        policy.finalize(&mut body, &cfg, "claude-x").unwrap();
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["options"]["num_predict"], 64);
        assert!(body.get("max_output_tokens").is_none());
        let mut next = json!({"max_tokens":-1});
        policy.finalize(&mut next, &cfg, "claude-x").unwrap();
        assert_eq!(next["max_tokens"], 64);
    }
    #[test]
    fn expired_pre_send_deadline_reports_partial_without_http() {
        let policy = ProbePolicy::limited(Duration::ZERO, 8);
        assert!(policy
            .before_send()
            .unwrap_err()
            .to_string()
            .contains("logical deadline"));
        let report = policy.report();
        assert_eq!(report["requests_sent"], 0);
        assert_eq!(report["deadline_exceeded"], true);
        assert_eq!(report["usage_partial"], true);
        assert!(report["reported_usage"].is_null());
    }
    #[test]
    fn send_budget_and_usage_report_do_not_invent_unreported_zeroes() {
        let policy = ProbePolicy::limited(Duration::from_secs(1), 2);
        policy.before_send().unwrap();
        policy.before_send().unwrap();
        assert!(policy.before_send().is_err());
        policy.observe(&Usage {
            prompt_tokens: 4,
            reported: 1,
            ..Default::default()
        });
        let report = policy.report();
        assert_eq!(report["requests_sent"], 2);
        assert_eq!(report["usage_partial"], true);
        assert_eq!(report["reported_usage"]["prompt_tokens"], 4);
        assert!(report["reported_usage"].get("completion_tokens").is_none());
        assert!(report["reported_usage"].get("reported_cost").is_none());
    }
}
