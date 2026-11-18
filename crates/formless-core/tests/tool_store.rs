//! Tool key-value store (SPEC §7.4 `store`): per-Tool and shared
//! namespaces, Capability-gated, size-capped, wiped on purge.

use formless_core::{Paths, Store};
use formless_runtime::Capabilities;
use serde_json::{json, Value};
use std::sync::{Arc, Barrier};

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

#[test]
fn namespace_quota_counts_bytes_and_keeps_overwrite_and_delete_available() {
    for payload in ["a".repeat(65_534), "λ".repeat(32_767)] {
        let store = Store::open_in_memory().unwrap();
        let value = Value::String(payload);
        let key = "\0🦀".to_owned() + &"é".repeat(5);
        assert_eq!(key.len(), 15);
        for i in 0..79 {
            store
                .kv_set("shared:cache", &format!("{i}"), &value)
                .unwrap();
        }
        // The key is charged through its embedded NUL, and the same UTF-8
        // byte sizes give the same result for ASCII and multibyte JSON.
        let remaining = 5 * 1024 * 1024 - 79 * 65_536 - (10 + 69 * 2);
        let boundary = "x".repeat(remaining - key.len() - 2);
        let error = store
            .kv_set("shared:cache", &key, &json!(boundary.clone() + "x"))
            .unwrap_err();
        assert!(error.to_string().contains("namespace is full"));
        let boundary_value = json!(boundary);
        store.kv_set("shared:cache", &key, &boundary_value).unwrap();
        store.kv_set("shared:cache", "0", &value).unwrap();
        assert_eq!(store.kv_get("shared:cache", "0").unwrap(), Some(value));
        assert_eq!(
            store.kv_get("shared:cache", &key).unwrap(),
            Some(boundary_value)
        );
        assert!(store.kv_set("shared:cache", "extra", &Value::Null).is_err());
        assert!(store.kv_delete("shared:cache", &key).unwrap());
        store.kv_set("shared:cache", "extra", &Value::Null).unwrap();
    }

    let store = Store::open_in_memory().unwrap();
    let key = "🦀".repeat(64);
    store.kv_set("tool:a", &key, &Value::Null).unwrap();
    assert!(store
        .kv_set("tool:a", &(key + "a"), &Value::Null)
        .unwrap_err()
        .to_string()
        .contains("1-256 UTF-8 bytes"));
}

#[test]
fn profile_quota_is_atomic_across_writers_and_leaves_harness_updates_available() {
    let dir = std::env::temp_dir().join(format!("fl-kv-quota-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let database = dir.join("test.db");
    let store = Store::open(&database).unwrap();
    // Seed data as an earlier version could have stored it.
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute(
        "WITH RECURSIVE seed(i) AS (
             VALUES(0) UNION ALL SELECT i+1 FROM seed WHERE i<798
         )
         INSERT INTO tool_kv(namespace, key, value, updated_at)
         SELECT printf('shared:cache-%02d', i/64), printf('%03d', i), ?1, '' FROM seed",
        [json!("x".repeat(65_534)).to_string()],
    )
    .unwrap();
    drop(conn);

    let first = Store::open(&database).unwrap();
    let second = Store::open(&database).unwrap();
    let start = Arc::new(Barrier::new(2));
    let write = |writer: Store, namespace: &'static str| {
        let start = start.clone();
        std::thread::spawn(move || {
            let payload = json!("x".repeat(48 * 1024));
            start.wait();
            writer.kv_set(namespace, "cursor", &payload)
        })
    };
    let a = write(first, "tool:instance-a");
    let b = write(second, "shared:cache-extra");
    let results = [a.join().unwrap(), b.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let error = results.into_iter().find_map(Result::err).unwrap();
    assert!(error.to_string().contains("profile Tool store is full"));
    store
        .kv_set("harness:registry", "last_change", &json!("receipt"))
        .unwrap();
    assert!(store.kv_delete("shared:cache-00", "000").unwrap());
    store
        .kv_set("shared:cache-extra", "new", &json!("x".repeat(48 * 1024)))
        .unwrap();
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn storage_reports_payload_bytes_and_retains_unreferenced_shared_data() {
    let dir = std::env::temp_dir().join(format!("fl-kv-usage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let paths = Paths {
        data_dir: dir.clone(),
        config_dir: dir.join("config"),
        profile: "test-kv-usage".into(),
    };
    let store = Store::open(&paths.db_file()).unwrap();
    let cap = Capabilities::parse(&["store:retained".into()]).unwrap();
    let code = "function run(){return '🦀';}";
    let (tool, version) = store
        .create_tool_version("example", "fixture", "{}", "{}", code, &cap, 256, "agent")
        .unwrap();
    // Older versions still preserve references even if the current version
    // stopped declaring the shared namespace.
    store
        .create_tool_version(
            "example",
            "fixture",
            "{}",
            "{}",
            code,
            &Capabilities::default(),
            256,
            "agent",
        )
        .unwrap();
    let session = store.create_session(None).unwrap();
    let content = "ไทย\0🦀";
    store
        .append_message_meta(&session.id, "user", content, Some("{}".into()))
        .unwrap();
    let inv = store
        .start_invocation(&tool.id, version, None, None, None)
        .unwrap();
    store
        .finish_invocation(&inv, &tool.id, "success", "", "漢字", "ascii", 0, 10)
        .unwrap();
    let conn = rusqlite::Connection::open(paths.db_file()).unwrap();
    conn.execute(
        "INSERT INTO run_files(run_id,session_id,path,existed,kept,before,created_at)
         VALUES('run',?1,'example',1,1,?2,'')",
        rusqlite::params![session.id, &[0_u8, 0xff, 0][..]],
    )
    .unwrap();
    drop(conn);
    let raw = json!("🦀");
    store.kv_set("shared:retained", "payload", &raw).unwrap();
    store.kv_set("shared:unclaimed", "payload", &raw).unwrap();

    let content_bytes = store.content_bytes().unwrap();
    assert_eq!(content_bytes["messages"], content.len() + 2);
    assert_eq!(content_bytes["invocations"], "漢字".len() + "ascii".len());
    assert_eq!(content_bytes["tool_versions"], 2 * (code.len() + 4));
    assert_eq!(content_bytes["run_files"], 3);
    let report = || {
        formless_core::housekeeping::usage(&paths, &store).unwrap()["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "Tool store")
            .unwrap()
            .clone()
    };
    let row = report();
    let orphan_bytes = "shared:unclaimed".len() + "payload".len() + raw.to_string().len();
    assert_eq!(row["orphaned_namespaces"], 1);
    assert_eq!(row["orphaned_bytes"], orphan_bytes);
    assert_eq!(row["bytes"], 2 * orphan_bytes - 1);
    assert!(store.purge_tool(&tool.id).unwrap());
    assert_eq!(report()["orphaned_namespaces"], 2);
    assert_eq!(
        store.kv_get("shared:retained", "payload").unwrap(),
        Some(raw)
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
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
