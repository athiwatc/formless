//! Measured model auto-selection (SPEC §4.1, v2).
//!
//! Selection is based only on outcomes this daemon has recorded for each
//! candidate (successes, failures, latency), smoothed toward a neutral
//! prior so small samples do not dominate, with a small exploration
//! bonus for under-sampled models. Optional user-labelled fast/strong
//! subsets constrain simple vs complex tasks. No vendor/model-name
//! rankings are hard-coded.

use crate::config::ProviderCfg;
use crate::storage::ModelStat;
use std::collections::HashMap;

/// A task is complex when its estimated context reaches the configured
/// threshold or it has many tools available.
pub fn is_complex_task(context_chars: usize, tool_count: usize, threshold_chars: usize) -> bool {
    context_chars >= threshold_chars.max(1) || tool_count > 12
}

/// Score one candidate. Higher is better.
pub fn model_score(stat: Option<&ModelStat>, total_samples: i64, max_avg_ms: f64) -> f64 {
    let (successes, samples, avg_ms) = match stat {
        Some(s) => (s.successes, s.samples(), s.avg_ms().unwrap_or(0.0)),
        None => (0, 0, 0.0),
    };
    // Beta(4,4)-style smoothing around 0.5.
    let smoothed = (successes as f64 + 4.0) / (samples as f64 + 8.0);
    let latency_penalty = if samples > 0 && max_avg_ms > 0.0 {
        0.15 * (avg_ms / max_avg_ms).clamp(0.0, 1.0)
    } else {
        0.075
    };
    let exploration = 0.08 * ((total_samples as f64 + 2.0).ln() / (samples as f64 + 1.0)).sqrt();
    smoothed - latency_penalty + exploration
}

/// Choose a model for `model = "auto"`, or `None` when no candidates are
/// configured. Ties keep configured candidate order.
pub fn select_model(cfg: &ProviderCfg, stats: &[ModelStat], complex: bool) -> Option<String> {
    let candidates = cfg.routing_candidates();
    if candidates.is_empty() {
        return None;
    }
    let subset_names: &[String] = if complex {
        &cfg.routing_strong_models
    } else {
        &cfg.routing_fast_models
    };
    let filtered: Vec<String> = candidates
        .iter()
        .filter(|c| subset_names.is_empty() || subset_names.iter().any(|s| s.trim() == c.as_str()))
        .cloned()
        .collect();
    let eligible = if filtered.is_empty() {
        candidates
    } else {
        filtered
    };

    let by_model: HashMap<&str, &ModelStat> = stats.iter().map(|s| (s.model.as_str(), s)).collect();
    let total_samples: i64 = eligible
        .iter()
        .map(|c| by_model.get(c.as_str()).map(|s| s.samples()).unwrap_or(0))
        .sum();
    let max_avg_ms = eligible
        .iter()
        .filter_map(|c| by_model.get(c.as_str()).and_then(|s| s.avg_ms()))
        .fold(0.0f64, f64::max);

    // Manual fold so configured order wins exact ties.
    let mut best: Option<(String, f64)> = None;
    for candidate in eligible {
        let score = model_score(
            by_model.get(candidate.as_str()).copied(),
            total_samples,
            max_avg_ms,
        );
        if best.as_ref().is_none_or(|(_, b)| score > *b) {
            best = Some((candidate, score));
        }
    }
    best.map(|(m, _)| m)
}
