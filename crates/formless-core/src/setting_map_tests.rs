//! Shared harness key resolution; no changes to consent or execution gates.
use super::*;

#[tokio::test]
async fn dotted_map_entry_snapshot_and_model_domain_match_the_writer() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "unused-private-map-fixture");
    let root = std::env::temp_dir().join(format!("fl-hmap-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
    Config::default().save(&paths).unwrap();
    let d = Arc::new(Daemon::new(paths.clone()).unwrap());
    let key = "provider.model_context_tokens.vendor/qwen2.5";
    let args = json!({"key":key,"value":1234});
    assert_eq!(
        d.harness_snapshot("models.set", &args).unwrap(),
        Value::Null
    );
    d.harness_apply("models.set", &args).await.unwrap();
    let before = d.harness_snapshot("models.set", &args).unwrap();
    assert_eq!(before, 1234);
    d.harness_apply("models.set", &json!({"key":key,"value":9876}))
        .await
        .unwrap();
    d.harness_apply("models.set", &json!({"key":key,"value":before}))
        .await
        .unwrap();
    assert_eq!(
        Config::load_file_only(&paths)
            .unwrap()
            .provider
            .model_context_tokens["vendor/qwen2.5"],
        1234
    );
    assert!(d.harness_apply("config.set", &args).await.is_err());
    let prices = json!({"key":"provider.prices.vendor/model.4.5", "value":{"input":1,"output":2}});
    assert!(d.harness_apply("models.set", &prices).await.is_err());
    d.harness_apply("config.set", &prices).await.unwrap();
    let original_price = d.harness_snapshot("config.set", &prices).unwrap();
    assert_eq!(original_price["input"], 1.0);
    d.harness_apply(
        "config.set",
        &json!({"key":prices["key"],"value":{"input":5,"output":6}}),
    )
    .await
    .unwrap();
    d.harness_apply(
        "config.set",
        &json!({"key":prices["key"],"value":original_price}),
    )
    .await
    .unwrap();
    assert_eq!(
        d.harness_snapshot("config.set", &prices).unwrap()["input"],
        1.0
    );
    d.harness_apply("config.set", &json!({"key":prices["key"],"value":null}))
        .await
        .unwrap();
    assert_eq!(
        d.harness_snapshot("config.set", &prices).unwrap(),
        Value::Null
    );
    let meta = json!({
        "key":"provider.model_meta.vendor/qwen2.5",
        "value":{"vision":false,"tools":true,"efforts":["low","high"]}
    });
    d.harness_apply("models.set", &meta).await.unwrap();
    assert_eq!(
        d.harness_snapshot("models.set", &meta).unwrap(),
        meta["value"]
    );
    assert_eq!(
        Config::load_file_only(&paths).unwrap().provider.model_meta["vendor/qwen2.5"],
        crate::config::ModelMetaCfg {
            vision: Some(false),
            tools: Some(true),
            efforts: Some(vec!["low".into(), "high".into()]),
        }
    );
    assert!(d
        .harness_apply(
            "models.set",
            &json!({"key":"provider.model.typo","value":"x"})
        )
        .await
        .is_err());
    drop(d);
    std::fs::remove_dir_all(root).unwrap();
}
