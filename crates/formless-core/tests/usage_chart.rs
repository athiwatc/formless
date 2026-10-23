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
        },
    );
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let a = store.create_session(Some("Alpha".into())).unwrap().id;
    let b = store.create_session(Some("Beta".into())).unwrap().id;
    let use_ = |p, c| Usage {
        prompt_tokens: p,
        completion_tokens: c,
        ..Default::default()
    };
    store
        .record_usage(&a, "r1", "cheap", &use_(1_000_000, 0))
        .unwrap();
    store
        .record_usage(&a, "r1", "pricey", &use_(10, 10))
        .unwrap();
    store
        .record_usage(&b, "r2", "cheap", &use_(0, 1_000_000))
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
    assert!(days[0]["cost"].is_null(), "a model without a price");

    let models = daemon.handle(by(Some("model"))).await.unwrap();
    assert_eq!(models[0]["key"], "cheap");
    assert_eq!(models[0]["cost"], 3.0);
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
