//! Cost-aware selection over an already windowed, provider-scoped event sample.
//! Billing and event collection are deliberately separate from these pure rules.
use crate::{
    config::ProviderCfg, provider::Usage, routing::model_score, storage::ModelStat, Config,
};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Days, Utc};
use serde_json::{json, Value};

/// One model's eligible logical calls or unambiguous whole-role tasks.
/// `usd` is the total across ALL their physical attempts, or unknown if any
/// billable attempt lacks a complete cost basis. Neutral outcomes are excluded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stat {
    pub model: String,
    pub samples: i64,
    pub good: i64,
    pub total_ms: f64,
    pub total_tokens: Option<f64>,
    pub usd: Option<f64>,
}
impl Stat {
    pub fn good_rate(&self) -> f64 {
        if self.samples > 0 {
            self.good as f64 / self.samples as f64
        } else {
            0.0
        }
    }
    pub fn usd_per_success(&self) -> Option<f64> {
        self.usd
            .filter(|n| n.is_finite() && *n >= 0.0)
            .filter(|_| self.good > 0)
            .map(|n| n / self.good as f64)
    }
    fn avg_ms(&self) -> f64 {
        self.total_ms / self.samples.max(1) as f64
    }
    fn avg_tokens(&self) -> Option<f64> {
        self.total_tokens.map(|n| n / self.samples.max(1) as f64)
    }
    fn qualifies(&self, min: i64, bar: f64) -> bool {
        self.samples >= min
            && self.good >= 0
            && self.good <= self.samples
            && self.good_rate() >= bar
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    KnownUsd,
    UnpricedEffort,
    Explore,
    ColdStart,
    LegacyScore,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub model: String,
    pub reason: Reason,
    pub explanation: Value,
}

pub fn window_start(now: DateTime<Utc>, days: u32) -> Result<DateTime<Utc>> {
    if days == 0 {
        bail!("provider.routing_window_days must be positive");
    }
    now.checked_sub_days(Days::new(u64::from(days)))
        .context("provider.routing_window_days exceeds the supported date range")
}
fn validate(min: i64, bar: f64) -> Result<()> {
    if min < 1 {
        bail!("routing minimum samples must be positive");
    }
    if !bar.is_finite() || !(0.0..=1.0).contains(&bar) {
        bail!("routing quality bar must be finite and between zero and one");
    }
    Ok(())
}
/// A reported amount can be known without token counters. Repricing a partial
/// token report would fabricate absent counters as zero, so never do that.
pub fn usage_usd(cfg: &Config, provider: &str, model: &str, usage: &Usage) -> Option<f64> {
    if let Some(cost) = usage.cost.filter(|v| v.is_finite() && *v >= 0.0) {
        return Some(cost);
    }
    if usage.reported & 3 != 3 || usage.cost_unknown {
        return None;
    }
    let convert = |n: u64| i64::try_from(n).ok();
    cfg.cost_of_known(
        provider,
        model,
        convert(usage.prompt_tokens)?,
        convert(usage.completion_tokens)?,
        convert(usage.cached_tokens)?,
        convert(usage.cache_write_tokens)?,
        None,
        false,
    )
    .filter(|v| v.is_finite() && *v >= 0.0)
}
fn resolved_models(cfg: &ProviderCfg, models: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for name in models {
        let model = cfg.resolve_alias(name);
        if !model.is_empty() && !ProviderCfg::is_auto_model(&model) && !out.contains(&model) {
            out.push(model);
        }
    }
    out
}

fn routing_subset(cfg: &ProviderCfg, complex: bool) -> (&'static str, &[String]) {
    if complex {
        ("strong", &cfg.routing_strong_models)
    } else {
        ("fast", &cfg.routing_fast_models)
    }
}

pub fn routing_subset_warnings(cfg: &ProviderCfg) -> Vec<String> {
    let all = resolved_models(cfg, &cfg.routing_candidates());
    let mut warnings = Vec::new();
    for complex in [false, true] {
        let (kind, names) = routing_subset(cfg, complex);
        let setting = format!("provider.routing_{kind}_models");
        let mut matched = false;
        for name in names {
            if all.contains(&cfg.resolve_alias(name)) {
                matched = true;
            } else {
                warnings.push(format!(
                    "{setting} includes {name:?} outside the configured candidate list."
                ));
            }
        }
        if !names.is_empty() && !matched {
            warnings.push(match all.first() {
                Some(first) => format!("{setting} matches no candidates. Automatic routing for this subset will use the first candidate, {first:?}."),
                None => format!("{setting} matches no candidates. Configure routing_models or fallback_models before using auto routing."),
            });
        }
    }
    warnings
}

fn candidates(cfg: &ProviderCfg, complex: bool) -> (Vec<String>, Option<&'static str>) {
    let all = resolved_models(cfg, &cfg.routing_candidates());
    let (kind, names) = routing_subset(cfg, complex);
    if names.is_empty() {
        return (all, None);
    }
    let subset = resolved_models(cfg, names);
    let filtered = all
        .iter()
        .filter(|m| subset.contains(m))
        .cloned()
        .collect::<Vec<_>>();
    if filtered.is_empty() {
        (all.into_iter().take(1).collect(), Some(kind))
    } else {
        (filtered, None)
    }
}
fn best_legacy(candidates: &[String], stats: &[Stat]) -> Option<String> {
    let total = stats
        .iter()
        .filter(|s| candidates.contains(&s.model))
        .map(|s| s.samples.max(0))
        .sum();
    let max = stats
        .iter()
        .filter(|s| candidates.contains(&s.model))
        .map(Stat::avg_ms)
        .fold(0.0, f64::max);
    let mut best = None;
    for model in candidates {
        let stat = stats.iter().find(|s| s.model == *model).map(|s| ModelStat {
            model: model.clone(),
            successes: s.good,
            failures: (s.samples - s.good).max(0),
            total_ms: s.total_ms as i64,
        });
        let score = model_score(stat.as_ref(), total, max);
        if best.as_ref().is_none_or(|(_, old)| score > *old) {
            best = Some((model.clone(), score));
        }
    }
    best.map(|(m, _)| m)
}
fn choose(
    candidates: &[String],
    stats: &[Stat],
    min: i64,
    bar: f64,
    explore: bool,
    main: bool,
) -> Result<Option<Choice>> {
    validate(min, bar)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let stat = |m: &str| stats.iter().find(|s| s.model == m);
    let least = candidates
        .iter()
        .filter(|m| stat(m).map_or(0, |s| s.samples) < min)
        .min_by_key(|m| stat(m).map_or(0, |s| s.samples));
    let qualifiers = candidates
        .iter()
        .filter_map(|m| stat(m))
        .filter(|s| s.qualifies(min, bar))
        .collect::<Vec<_>>();
    let legacy = main.then(|| best_legacy(candidates, stats)).flatten();
    let exploring = if main {
        legacy
            .as_ref()
            .filter(|m| stat(m).map_or(0, |s| s.samples) < min)
    } else {
        least.filter(|_| explore)
    };
    let (picked, reason) = if let Some(m) = exploring {
        (m.clone(), Reason::Explore)
    } else if qualifiers.is_empty() {
        (
            legacy.unwrap_or_else(|| candidates[0].clone()),
            if main {
                Reason::LegacyScore
            } else {
                Reason::ColdStart
            },
        )
    } else if let Some(s) = qualifiers
        .iter()
        .filter(|s| s.usd_per_success().is_some())
        .copied()
        .min_by(|a, b| {
            a.usd_per_success()
                .unwrap()
                .total_cmp(&b.usd_per_success().unwrap())
        })
    {
        (s.model.clone(), Reason::KnownUsd)
    } else {
        let ms = qualifiers
            .iter()
            .map(|s| s.avg_ms())
            .fold(f64::MAX, f64::min)
            .max(1.0);
        let tokens = qualifiers
            .iter()
            .filter_map(|s| s.avg_tokens())
            .fold(f64::MAX, f64::min)
            .max(1.0);
        // Missing counters do not make a candidate cheaper. Use a common
        // latency-only basis until every qualifier has known token totals.
        let known_tokens = qualifiers.iter().all(|s| s.avg_tokens().is_some());
        let effort = |s: &Stat| {
            s.avg_ms() / ms
                + if known_tokens {
                    s.avg_tokens().unwrap() / tokens
                } else {
                    0.0
                }
        };
        let s = qualifiers
            .iter()
            .copied()
            .min_by(|a, b| effort(a).total_cmp(&effort(b)))
            .unwrap();
        (s.model.clone(), Reason::UnpricedEffort)
    };
    let rows=candidates.iter().map(|m|match stat(m){Some(s)=>json!({"model":m,"samples":s.samples,"good_rate":s.good_rate(),"qualified":s.qualifies(min,bar),"avg_ms":s.avg_ms(),"avg_tokens":s.avg_tokens(),"usd_per_success":s.usd_per_success()}),None=>json!({"model":m,"samples":0,"qualified":false,"usd_per_success":null})}).collect::<Vec<_>>();
    let rule = match reason {
        Reason::KnownUsd => "lowest complete USD per success among qualified known-cost models",
        Reason::UnpricedEffort => "no qualified complete USD basis: relative tokens plus latency",
        Reason::Explore => "exploring an under-sampled model",
        Reason::ColdStart => "no qualifier: first configured candidate",
        Reason::LegacyScore => "no qualifier: smoothed success, latency and exploration score",
    };
    Ok(Some(Choice {
        model: picked.clone(),
        reason,
        explanation: json!({"picked":picked,"reason":rule,"candidates":rows,"token_basis":if qualifiers.iter().all(|s|s.total_tokens.is_some()){"complete"}else{"unknown counters: shared latency-only fallback"}}),
    }))
}
pub fn select_auto(cfg: &ProviderCfg, stats: &[Stat], complex: bool) -> Result<Option<Choice>> {
    let (candidates, subset_fallback) = candidates(cfg, complex);
    let mut choice = choose(
        &candidates,
        stats,
        cfg.routing_min_samples,
        cfg.routing_quality_bar,
        false,
        true,
    )?;
    if let (Some(choice), Some(kind)) = (&mut choice, subset_fallback) {
        choice.explanation["subset_fallback"] = json!(kind);
        choice.explanation["reason"] = json!(format!(
            "The {kind} subset matched no candidates. Using the first configured candidate."
        ));
    }
    Ok(choice)
}
pub fn select_task(
    candidates: &[String],
    stats: &[Stat],
    min: i64,
    bar: f64,
    explore: bool,
) -> Result<Option<Choice>> {
    choose(candidates, stats, min, bar, explore, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stat(model: &str, usd: Option<f64>, ms: f64, tokens: f64) -> Stat {
        Stat {
            model: model.into(),
            samples: 10,
            good: 9,
            total_ms: ms * 10.0,
            total_tokens: Some(tokens * 10.0),
            usd,
        }
    }
    fn config() -> ProviderCfg {
        ProviderCfg {
            routing_models: vec!["premium".into(), "cheap".into()],
            ..Default::default()
        }
    }
    #[test]
    fn both_selectors_rank_qualified_usd_instead_of_unpriced_speed() {
        let cfg = config();
        let stats = [
            stat("premium", Some(9.0), 1.0, 10.0),
            stat("cheap", Some(0.9), 100.0, 1000.0),
        ];
        let main = select_auto(&cfg, &stats, false).unwrap().unwrap();
        let task = select_task(&cfg.routing_models, &stats, 5, 0.8, false)
            .unwrap()
            .unwrap();
        for choice in [main, task] {
            assert_eq!(choice.model, "cheap");
            assert_eq!(choice.reason, Reason::KnownUsd);
            assert!(choice.explanation["reason"]
                .as_str()
                .unwrap()
                .contains("USD"));
        }
    }
    #[test]
    fn unknown_is_not_free_and_zero_reported_cost_is_known() {
        let cfg = config();
        let mut stats = [
            stat("premium", Some(1.0), 100.0, 100.0),
            stat("cheap", None, 1.0, 1.0),
        ];
        assert_eq!(
            select_auto(&cfg, &stats, false).unwrap().unwrap().model,
            "premium"
        );
        stats[1].usd = Some(0.0);
        assert_eq!(
            select_auto(&cfg, &stats, false).unwrap().unwrap().model,
            "cheap"
        );
        stats[0].usd = None;
        stats[1].usd = None;
        let c = select_auto(&cfg, &stats, false).unwrap().unwrap();
        assert_eq!(c.model, "cheap");
        assert_eq!(c.reason, Reason::UnpricedEffort);
    }
    #[test]
    fn quality_subsets_aliases_exploration_and_order_remain_explicit() {
        let mut cfg = config();
        cfg.aliases.insert("fast".into(), "cheap".into());
        cfg.routing_fast_models = vec!["fast".into()];
        assert_eq!(
            select_auto(&cfg, &[], false).unwrap().unwrap().model,
            "cheap"
        );
        cfg.routing_fast_models.clear();
        let mut stats = [
            stat("premium", Some(0.0), 1.0, 1.0),
            stat("cheap", Some(1.0), 1.0, 1.0),
        ];
        stats[0].good = 1;
        assert_eq!(
            select_auto(&cfg, &stats, false).unwrap().unwrap().model,
            "cheap"
        );
        stats[0] = stat("premium", Some(1.0), 1.0, 1.0);
        assert_eq!(
            select_auto(&cfg, &stats, false).unwrap().unwrap().model,
            "premium"
        );
        let c = select_task(&cfg.routing_models, &[], 5, 0.8, true)
            .unwrap()
            .unwrap();
        assert_eq!(c.model, "premium");
        assert_eq!(c.reason, Reason::Explore);
    }

    #[test]
    fn unmatched_routing_subsets_are_reported_and_fall_back_to_one_candidate() {
        let mut cfg = config();
        cfg.fallback_models = vec!["fallback-only".into()];
        cfg.aliases.insert("quick".into(), "cheap".into());
        cfg.routing_fast_models = vec!["quick".into(), "typo".into()];
        cfg.routing_strong_models = vec!["fallback-only".into()];
        let warnings = routing_subset_warnings(&cfg);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings[0].contains("routing_fast_models") && warnings[0].contains("typo"));
        assert!(warnings[1].contains("fallback-only") && warnings[1].contains("candidate list"));
        assert!(warnings[2].contains("matches no candidates") && warnings[2].contains("premium"));
        let stats = [
            stat("premium", Some(20.0), 1.0, 1.0),
            stat("cheap", Some(1.0), 1.0, 1.0),
        ];
        let fast = select_auto(&cfg, &stats, false).unwrap().unwrap();
        assert_eq!(fast.model, "cheap");
        assert!(fast.explanation.get("subset_fallback").is_none());
        let strong = select_auto(&cfg, &stats, true).unwrap().unwrap();
        assert_eq!(strong.model, "premium");
        assert_eq!(strong.explanation["subset_fallback"], "strong");
        assert_eq!(
            strong.explanation["candidates"].as_array().unwrap().len(),
            1
        );
    }
    #[test]
    fn partial_tokens_stay_unknown_reported_usd_wins_and_cache_is_priced() {
        let mut cfg = Config::default();
        cfg.provider.prices.insert(
            "m".into(),
            crate::config::ModelPrice {
                input: 10.0,
                output: 20.0,
                cached_input: Some(1.0),
                cache_write_input: Some(5.0),
                retrieved_at: None,
            },
        );
        let mut u = Usage {
            prompt_tokens: 1000,
            completion_tokens: 100,
            cached_tokens: 500,
            reported: 1,
            ..Default::default()
        };
        assert_eq!(usage_usd(&cfg, "default", "m", &u), None);
        u.reported = 3;
        assert_eq!(usage_usd(&cfg, "default", "m", &u), Some(0.0075));
        u.cost_unknown = true;
        assert_eq!(usage_usd(&cfg, "default", "m", &u), None);
        u.cost = Some(0.012);
        assert_eq!(usage_usd(&cfg, "default", "m", &u), Some(0.012));
    }
    #[test]
    fn provider_price_and_endpoint_identity_are_not_merged_by_model_name() {
        let mut cfg = Config::default();
        let mut named = ProviderCfg {
            base_url: "https://user:private@example.invalid/native?key=private".into(),
            ..Default::default()
        };
        named.prices.insert(
            "m".into(),
            crate::config::ModelPrice {
                input: 2.0,
                output: 3.0,
                cached_input: None,
                cache_write_input: None,
                retrieved_at: None,
            },
        );
        let original = crate::tokens::provider_key("named", &named);
        assert!(!original.contains("private"));
        assert_ne!(original, crate::tokens::provider_key("other", &named));
        named.api_style = "responses".into();
        assert_ne!(original, crate::tokens::provider_key("named", &named));
        cfg.providers.insert("named".into(), named);
        let usage = Usage {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            reported: 3,
            ..Default::default()
        };
        assert_eq!(usage_usd(&cfg, "named", "m", &usage), Some(5.0));
        assert_eq!(usage_usd(&cfg, "default", "m", &usage), None);
    }
    #[test]
    fn missing_token_counters_do_not_get_a_zero_token_discount() {
        let mut cfg = config();
        cfg.routing_min_samples = 1;
        let known = stat("cheap", None, 50.0, 10000.0);
        let mut unknown = stat("premium", None, 60.0, 0.0);
        unknown.total_tokens = None;
        let selected = select_auto(&cfg, &[known, unknown], false)
            .unwrap()
            .unwrap();
        assert_eq!(selected.model, "cheap");
        assert_eq!(
            selected.explanation["token_basis"],
            "unknown counters: shared latency-only fallback"
        );
    }
    #[test]
    fn window_defaults_and_config_roundtrip_are_documented() {
        let mut cfg = Config::default();
        assert_eq!(cfg.provider.routing_window_days, 30);
        assert_eq!(cfg.provider.routing_min_samples, 5);
        assert_eq!(cfg.provider.routing_quality_bar, 0.8);
        for (key, value) in [
            ("provider.routing_window_days", json!(7)),
            ("provider.routing_min_samples", json!(3)),
            ("provider.routing_quality_bar", json!(0.9)),
        ] {
            cfg.set_key(key, value).unwrap();
            assert!(crate::config::setting_docs().contains_key(key));
        }
        let back: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.provider.routing_window_days, 7);
        let now = DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            window_start(now, 30).unwrap().to_rfc3339(),
            "2026-10-02T12:00:00+00:00"
        );
        assert!(window_start(now, 0).is_err());
        assert!(select_task(&[], &[], 0, 0.8, false).is_err());
    }
}
