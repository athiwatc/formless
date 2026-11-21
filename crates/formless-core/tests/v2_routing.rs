//! v2 model auto-selection (SPEC §4.1/§17): measured routing policy and
//! persisted outcome statistics. No hard-coded model rankings.

use formless_core::config::ProviderCfg;
use formless_core::routing::{is_complex_task, model_score};
use formless_core::routing_policy::{select_auto, Stat};
use formless_core::storage::ModelStat;
use formless_core::{Paths, Store};

fn stat(model: &str, successes: i64, failures: i64, total_ms: i64) -> ModelStat {
    ModelStat {
        model: model.into(),
        successes,
        failures,
        total_ms,
    }
}

fn cfg(models: Vec<&str>) -> ProviderCfg {
    ProviderCfg {
        model: "auto".into(),
        routing_models: models.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn selected(cfg: &ProviderCfg, stats: &[ModelStat], complex: bool) -> Option<String> {
    let current: Vec<_> = stats
        .iter()
        .map(|stat| Stat {
            model: stat.model.clone(),
            samples: stat.samples(),
            good: stat.successes,
            total_ms: stat.total_ms as f64,
            ..Stat::default()
        })
        .collect();
    select_auto(cfg, &current, complex)
        .unwrap()
        .map(|choice| choice.model)
}

#[test]
fn routing_learns_from_measured_outcomes() {
    let c = cfg(vec!["a", "b"]);
    // No data: configured order wins (neutral prior + tie-break).
    assert_eq!(selected(&c, &[], false).as_deref(), Some("a"));
    // A reliably successful model beats an unexplored one eventually.
    let stats = [stat("a", 1, 9, 10_000)];
    assert_eq!(selected(&c, &stats, false).as_deref(), Some("b"));
    // Strong measured success wins over exploration.
    let stats = [stat("a", 90, 10, 100_000), stat("b", 1, 9, 10_000)];
    assert_eq!(selected(&c, &stats, false).as_deref(), Some("a"));
    // Latency counts: equal success, much slower loses.
    let stats = [stat("a", 50, 0, 500_000), stat("b", 49, 1, 5_000)];
    let fast_score = model_score(Some(&stats[1]), 100, 10_000.0);
    let slow_score = model_score(Some(&stats[0]), 100, 10_000.0);
    assert!(fast_score > slow_score, "{fast_score} vs {slow_score}");
}

#[test]
fn routing_respects_fast_and_strong_subsets() {
    let mut c = cfg(vec!["fast", "strong", "mid"]);
    c.routing_fast_models = vec!["fast".into()];
    c.routing_strong_models = vec!["strong".into()];
    // Even with terrible stats, subset membership constrains the choice.
    let stats = [stat("fast", 0, 20, 1_000), stat("strong", 20, 0, 1_000)];
    assert_eq!(selected(&c, &stats, false).as_deref(), Some("fast"));
    assert_eq!(selected(&c, &stats, true).as_deref(), Some("strong"));
    // Candidates can come from fallback_models when routing_models is empty.
    let c2 = ProviderCfg {
        fallback_models: vec!["only".into()],
        ..Default::default()
    };
    assert_eq!(selected(&c2, &[], false).as_deref(), Some("only"));
    assert!(selected(&ProviderCfg::default(), &[], false).is_none());
    assert!(is_complex_task(20_000, 1, 20_000));
    assert!(is_complex_task(10, 13, 20_000));
    assert!(!is_complex_task(10, 2, 20_000));
}

#[test]
fn model_outcomes_persist_and_accumulate() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let paths = Paths {
        data_dir: format!("/tmp/fl-routing-{short}/d").into(),
        config_dir: format!("/tmp/fl-routing-{short}/c").into(),
        profile: format!("test-routing-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    store.record_model_outcome("good", true, 100).unwrap();
    store.record_model_outcome("good", true, 300).unwrap();
    store.record_model_outcome("good", false, 50).unwrap();
    store.record_model_outcome("auto", true, 1).unwrap();
    let stats = store.list_model_stats().unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].successes, 2);
    assert_eq!(stats[0].failures, 1);
    assert_eq!(stats[0].total_ms, 450);
    assert_eq!(stats[0].success_rate(), Some(2.0 / 3.0));
    assert_eq!(stats[0].avg_ms(), Some(150.0));
}
