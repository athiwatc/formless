//! Tool key-value store (SPEC §7.4 `store`): per-Tool and shared
//! namespaces, Capability-gated, size-capped, wiped on purge.

use formless_core::{Paths, Store};
use formless_runtime::Capabilities;
use std::sync::Arc;

#[test]
fn store_capabilities_parse_and_are_not_sensitive() {
    let c = Capabilities::parse(&["store".into(), "store:cache".into()]).unwrap();
    assert!(c.store && c.store_shared == ["cache"]);
    assert!(!c.is_sensitive(), "no approval needed");
    assert_eq!(c.to_specs(), ["store", "store:cache"]);
    let own = Capabilities::parse(&["store".into()]).unwrap();
    assert!(c.covers(&own) && !own.covers(&c));
    assert!(Capabilities::parse(&["store:bad/ns".into()]).is_err());
}

#[test]
fn kv_storage_caps() {
    let store = Store::open_in_memory().unwrap();
    store
        .kv_set("tool:a", "cursor", &serde_json::json!({"page": 3}))
        .unwrap();
    store
        .kv_set("tool:a", "cursor2", &serde_json::json!(1))
        .unwrap();
    store
        .kv_set("tool:a", "other", &serde_json::json!("x"))
        .unwrap();
    assert_eq!(
        store.kv_get("tool:a", "cursor").unwrap(),
        Some(serde_json::json!({"page": 3}))
    );
    assert_eq!(
        store.kv_get("tool:b", "cursor").unwrap(),
        None,
        "namespaces are separate"
    );
    assert_eq!(
        store.kv_list("tool:a", "cur").unwrap(),
        ["cursor", "cursor2"]
    );
    let big = serde_json::json!("x".repeat(70 * 1024));
    assert!(store.kv_set("tool:a", "big", &big).is_err(), "value cap");
    assert!(store.kv_delete("tool:a", "other").unwrap());
    assert!(!store.kv_delete("tool:a", "other").unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_keep_state_between_invocations() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-kv-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-kv-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let create = |name: &str, caps: serde_json::Value, js: &str| {
        serde_json::json!({
            "name": name, "description": "counts", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": js, "capabilities": caps
        })
    };
    let counter = "function run(){ const n = (store.get('n') || 0) + 1; store.set('n', n); store.set('shared', n, {ns: 'team'}); return {n}; }";
    let out = daemon
        .call_tool_direct(
            &sid,
            "create_tool",
            create(
                "counter",
                serde_json::json!(["store", "store:team"]),
                counter,
            ),
        )
        .await;
    assert!(out.contains("\"created\""), "{out}");
    for want in [1, 2, 3] {
        let r = daemon
            .call_tool_direct(&sid, "counter", serde_json::json!({}))
            .await;
        assert!(r.contains(&format!("\"n\":{want}")), "{r}");
    }
    // Another Tool reads the shared namespace, not counter's own.
    let reader = "function run(){ return {own: store.get('n'), shared: store.get('shared', {ns: 'team'})}; }";
    let refused = daemon
        .call_tool_direct(
            &sid,
            "create_tool",
            create("reader", serde_json::json!(["store:team"]), reader),
        )
        .await;
    assert!(
        refused.contains("store not declared"),
        "own store needs the store capability: {refused}"
    );
    let peek = "function run(){ return {shared: store.get('shared', {ns: 'team'})}; }";
    daemon
        .call_tool_direct(
            &sid,
            "create_tool",
            create("peek", serde_json::json!(["store:team"]), peek),
        )
        .await;
    let r = daemon
        .call_tool_direct(&sid, "peek", serde_json::json!({}))
        .await;
    assert!(r.contains("\"shared\":3"), "{r}");

    // Purging the Tool wipes its own namespace.
    let id = store.get_tool_by_name("counter").unwrap().unwrap().0.id;
    assert!(store.kv_get(&format!("tool:{id}"), "n").unwrap().is_some());
    store.purge_tool(&id).unwrap();
    assert!(store.kv_get(&format!("tool:{id}"), "n").unwrap().is_none());
}
