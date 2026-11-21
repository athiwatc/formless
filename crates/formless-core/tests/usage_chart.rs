//! Token use by day, model, Role or Session (SPEC §10.4 Metrics).

use formless_core::config::ModelPrice;
use formless_core::provider::Usage;
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn usage_groups_by_day_or_key_and_prices_it() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-usage-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-usage-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.provider.prices.insert(
        "cheap".into(),
        ModelPrice {
            input: 1.0,
            output: 2.0,
            cached_input: None,
            cache_write_input: Some(2.0),
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let a = store.create_session(Some("Alpha".into())).unwrap().id;
    let b = store.create_session(Some("Beta".into())).unwrap().id;
    let use_ = |p, c, cw| Usage {
        prompt_tokens: p,
        completion_tokens: c,
        cache_write_tokens: cw,
        ..Default::default()
    };
    store
        .record_usage(&a, "r1", "cheap", &use_(1_000_000, 0, 250_000))
        .unwrap();
    store
        .record_usage(&a, "r1", "pricey", &use_(10, 10, 3))
        .unwrap();
    store
        .record_usage(&b, "r2", "cheap", &use_(0, 1_000_000, 0))
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let by = |by: Option<&str>| Request::UsageByDay {
        since: None,
        by: by.map(Into::into),
    };
    let days = daemon.handle(by(None)).await.unwrap();
    let days = days.as_array().unwrap();
    assert_eq!(days.len(), 1, "all today");
    assert_eq!(days[0]["prompt_tokens"], 1_000_010);
    assert_eq!(days[0]["cache_write_tokens"], 250_003);
    assert!(days[0]["cost"].is_null(), "a model without a price");
    assert!(days[0].get("session_id").is_none());

    let models = daemon.handle(by(Some("model"))).await.unwrap();
    assert_eq!(models[0]["key"], "cheap");
    assert_eq!(models[0]["cache_write_tokens"], 250_000);
    assert_eq!(models[0]["cost"], 3.25);
    assert!(models[1]["cost"].is_null());

    let sessions = daemon.handle(by(Some("session"))).await.unwrap();
    let keys: Vec<&str> = sessions
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["Alpha", "Beta"]);
    let roles = daemon.handle(by(Some("role"))).await.unwrap();
    assert_eq!(roles[0]["key"], "main");

    let none = daemon
        .handle(Request::UsageByDay {
            since: Some("2999-01-01T00:00:00Z".into()),
            by: None,
        })
        .await
        .unwrap();
    assert!(none.as_array().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn session_usage_groups_keep_identity_when_names_match_or_disappear() {
    use serde_json::{json, Value};

    let root = std::env::temp_dir().join(format!("fl-usage-group-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.provider.base_url = "http://127.0.0.1:9/v1".into();
    cfg.provider.model = "priced".into();
    cfg.provider.prices.insert(
        "priced".into(),
        ModelPrice {
            input: 1.0,
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let first = store.create_session(Some("Work".into())).unwrap().id;
    let second = store.create_session(Some("Work".into())).unwrap().id;
    let deleted = store.create_session(Some("Gone".into())).unwrap().id;
    let same_label = store.create_session(Some(deleted.clone())).unwrap().id;

    let tokens = |prompt_tokens, completion_tokens, cost| Usage {
        prompt_tokens,
        completion_tokens,
        cost,
        ..Default::default()
    };
    for (session, provider, model, usage) in [
        (&first, "default", "priced", tokens(1_000_000, 0, None)),
        (&first, "other", "another-model", tokens(10, 20, Some(2.5))),
        (&second, "default", "unpriced", tokens(4, 5, None)),
        (&deleted, "default", "unpriced", tokens(6, 7, Some(1.0))),
        (&same_label, "default", "unpriced", tokens(8, 9, Some(2.0))),
    ] {
        store
            .record_usage_full(session, "record", provider, model, &usage, "run")
            .unwrap();
    }
    assert!(store.delete_session(&deleted).unwrap());

    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths).unwrap());
    let request = || Request::UsageByDay {
        since: None,
        by: Some("session".into()),
    };
    let expected = |id: &str, key: &str, prompt_tokens, completion_tokens, cost: Value| {
        json!({
            "session_id": id, "key": key, "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens, "cached_tokens": 0, "cache_write_tokens": 0, "cost": cost,
        })
    };
    let before = daemon.handle(request()).await.unwrap();
    assert_eq!(
        before,
        json!([
            expected(&first, "Work", 1_000_010, 20, json!(3.5)),
            expected(&same_label, &deleted, 8, 9, json!(2.0)),
            expected(&deleted, &deleted, 6, 7, json!(1.0)),
            expected(&second, "Work", 4, 5, Value::Null),
        ])
    );

    store.rename_session(&first, "Renamed").unwrap().unwrap();
    let after = daemon.handle(request()).await.unwrap();
    let mut changed_label = before;
    changed_label[0]["key"] = json!("Renamed");
    assert_eq!(after, changed_label);
    drop(daemon);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

/// Cache writes are priced at their own rate, both reporting styles give
/// the same tokens, and prefixed ids find their price (#804, #809).
#[test]
fn cache_writes_and_prefixes_are_priced() {
    let price = ModelPrice {
        input: 3.0,
        output: 15.0,
        cached_input: Some(0.3),
        cache_write_input: Some(3.75),
        ..Default::default()
    };
    // 1M prompt: 200k cached, 300k written, 500k fresh; 0 out.
    let usd = price.cost(1_000_000, 0, 200_000, 300_000);
    assert!(
        (usd - (0.5 * 3.0 + 0.2 * 0.3 + 0.3 * 3.75)).abs() < 1e-9,
        "{usd}"
    );
    // Anthropic puts cache tokens beside input_tokens; OpenAI-style inside.
    let beside = Usage::parse(&serde_json::json!({
        "input_tokens": 500, "cache_read_input_tokens": 200, "cache_creation_input_tokens": 300,
        "output_tokens": 10
    }))
    .unwrap();
    let inside = Usage::parse(&serde_json::json!({
        "prompt_tokens": 1000, "completion_tokens": 10,
        "prompt_tokens_details": {"cached_tokens": 200, "cache_write_tokens": 300}
    }))
    .unwrap();
    for u in [&beside, &inside] {
        assert_eq!(
            (u.prompt_tokens, u.cached_tokens, u.cache_write_tokens),
            (1000, 200, 300)
        );
    }
    let mut p = formless_core::config::ProviderCfg::default();
    p.prices.insert("claude-x".into(), price.clone());
    assert!(
        p.price_for("anthropic/claude-x").is_some(),
        "prefix dropped"
    );
    p.prices.insert("openai/gpt-y".into(), price);
    assert!(p.price_for("gpt-y").is_some(), "the other way too");
    assert!(p.price_for("nope").is_none());
}

/// The same model from two providers is priced from each one's table,
/// and usage groups by provider (#810).
#[tokio::test(flavor = "multi_thread")]
async fn each_provider_prices_its_own_usage() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-usage2-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-usage2-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    let at = |input: f64| ModelPrice {
        input,
        output: 0.0,
        ..Default::default()
    };
    cfg.provider.prices.insert("same".into(), at(10.0));
    let mut agg = formless_core::config::ProviderCfg {
        base_url: "http://127.0.0.1:9".into(),
        model: "same".into(),
        ..Default::default()
    };
    agg.prices.insert("same".into(), at(1.0));
    cfg.providers.insert("aggregator".into(), agg);
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap().id;
    let mtok = Usage {
        prompt_tokens: 1_000_000,
        ..Default::default()
    };
    store
        .record_usage_full(&s, "r", "default", "same", &mtok, "run")
        .unwrap();
    store
        .record_usage_full(&s, "r", "aggregator", "same", &mtok, "run")
        .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let v = daemon
        .handle(Request::UsageByDay {
            since: None,
            by: Some("provider".into()),
        })
        .await
        .unwrap();
    let rows = v.as_array().unwrap();
    let cost_of = |name: &str| {
        rows.iter()
            .find(|r| r["key"] == name || r[0] == name)
            .map(|r| r["cost"].as_f64().or(r[4].as_f64()).unwrap())
            .unwrap_or_else(|| panic!("no {name} row in {v}"))
    };
    assert!((cost_of("default") - 10.0).abs() < 1e-9);
    assert!((cost_of("aggregator") - 1.0).abs() < 1e-9);
}

/// OpenRouter-style `/models` pricing becomes dated per-million prices;
/// free models are left out (#809).
#[test]
fn listed_prices_are_parsed() {
    let body = serde_json::json!({"data": [
        {"id": "anthropic/claude-x", "pricing": {"prompt": "0.000003", "completion": "0.000015",
            "input_cache_read": "0.0000003", "input_cache_write": "0.00000375"}},
        {"id": "free/model", "pricing": {"prompt": "0", "completion": "0"}},
        {"id": "no-pricing"}
    ]});
    let prices = formless_core::provider::parse_model_prices(&body, "2026-10-31T00:00:00Z");
    assert_eq!(prices.len(), 1);
    let p = &prices["anthropic/claude-x"];
    assert!((p.input - 3.0).abs() < 1e-9 && (p.output - 15.0).abs() < 1e-9);
    assert!((p.cache_write_input.unwrap() - 3.75).abs() < 1e-9);
    assert_eq!(p.retrieved_at.as_deref(), Some("2026-10-31T00:00:00Z"));
}
