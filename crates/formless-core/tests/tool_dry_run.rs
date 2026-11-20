//! Dry runs: a Tool run by hand without real side effects (SPEC §10.4).
#![cfg(unix)]

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_changes_nothing_and_says_what_it_would_have_done() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-dry-{short}"));
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("out.txt"), "old").unwrap();
    let work = work.canonicalize().unwrap();
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let w = work.display().to_string();
    let (id, session) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[
            format!("fs:read:{w}"),
            format!("fs:write:{w}"),
            "process:touch".into(),
        ])
        .unwrap();
        let code = format!(
            "function run(i){{ fs.write('{w}/out.txt', 'new'); const after = fs.read('{w}/out.txt'); \
             const r = process.run('touch', ['{w}/touched']); return {{after, code: r.code}}; }}"
        );
        let (info, _) = store
            .create_tool_version(
                "dry_demo",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                &code,
                &caps,
                64,
                "agent",
            )
            .unwrap();
        // Approved before, as a user would have: no prompt this time.
        store.set_approval(&info.id, &caps, 64).unwrap();
        (info.id, store.create_session(None).unwrap().id)
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let r = daemon
        .handle(Request::DryRunTool {
            id: id.clone(),
            input: serde_json::json!({}),
            session_id: session,
        })
        .await
        .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["result"]["after"], "new", "the run sees its own writes");
    let effects: Vec<String> = serde_json::from_value(r["effects"].clone()).unwrap();
    assert_eq!(effects.len(), 2, "{effects:?}");
    assert!(
        effects[0].starts_with("write 3 bytes to ") && effects[0].ends_with("out.txt"),
        "{effects:?}"
    );
    assert_eq!(effects[1], format!("run touch {w}/touched"));
    // Nothing happened on disk, and nothing was recorded.
    assert_eq!(
        std::fs::read_to_string(work.join("out.txt")).unwrap(),
        "old"
    );
    assert!(!work.join("touched").exists());
    let store = Store::open(&paths.db_file()).unwrap();
    assert_eq!(store.tool_metrics(&id, None).unwrap().total, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_store_merges_existing_pages_and_preserves_null_membership() {
    use serde_json::json;

    let dir = std::env::temp_dir().join(format!("formless-dry-store-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(dir.join("data"), dir.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let live = Store::open(&paths.db_file()).unwrap();
    let expected = Store::open_in_memory().unwrap();
    let caps = Capabilities::parse(&["store".into(), "store:preview-fixture".into()]).unwrap();
    let (tool, _) = live
        .create_tool_version(
            "dry_store",
            "Dry-run store membership",
            r#"{"type":"object"}"#,
            "{}",
            r#"function run() {
                for (let n = 0; n < 64; n++) store.delete('item-' + String(n).padStart(4, '0'));
                store.set('item-!', 'before');
                store.set('item-0080a', 'inside');
                store.set('item-z', 'after');
                store.set('preview-new', 'overlay');
                const set = [store.get('preview-new'), store.list('preview-')];
                const real = [store.delete('item-0080'), store.delete('item-0080'), store.get('item-0080')];
                const existingNull = [store.get('real-null'), store.list('real-'), store.delete('real-null')];
                store.set('set-null', null);
                store.set('set-undefined', undefined);
                const overlayNulls = [store.list('set-'), store.delete('set-null'), store.delete('set-undefined')];
                store.set('preview-new', 'shared', {ns:'preview-fixture'});
                const shared = [store.get('preview-new', {ns:'preview-fixture'}), store.list('preview-', {ns:'preview-fixture'})];
                return {keys:store.list('item-'), set, real, existingNull, overlayNulls, shared,
                    missing:store.delete('missing'), private:store.get('preview-new')};
            }"#,
            &caps,
            64,
            "agent",
        )
        .unwrap();
    live.set_approval(&tool.id, &caps, 64).unwrap();
    let namespace = format!("tool:{}", tool.id);
    for n in 0..1100 {
        let key = format!("item-{n:04}");
        live.kv_set(&namespace, &key, &json!(n)).unwrap();
        expected.kv_set(&namespace, &key, &json!(n)).unwrap();
    }
    live.kv_set(&namespace, "real-null", &serde_json::Value::Null)
        .unwrap();
    live.kv_set("shared:preview-fixture", "preview-original", &json!(true))
        .unwrap();
    for n in 0..64 {
        assert!(expected
            .kv_delete(&namespace, &format!("item-{n:04}"))
            .unwrap());
    }
    assert!(expected.kv_delete(&namespace, "item-0080").unwrap());
    for (key, value) in [
        ("item-!", "before"),
        ("item-0080a", "inside"),
        ("item-z", "after"),
    ] {
        expected.kv_set(&namespace, key, &json!(value)).unwrap();
    }
    let original = live.kv_list(&namespace, "item-").unwrap();
    let session_id = live.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths).unwrap());
    let reply = daemon
        .handle(Request::DryRunTool {
            id: tool.id,
            input: json!({}),
            session_id,
        })
        .await
        .unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    let result = &reply["result"];
    assert_eq!(
        result["keys"],
        json!(expected.kv_list(&namespace, "item-").unwrap())
    );
    assert_eq!(result["set"], json!(["overlay", ["preview-new"]]));
    assert_eq!(result["real"], json!([true, false, null]));
    assert_eq!(result["existingNull"], json!([null, ["real-null"], true]));
    assert_eq!(
        result["overlayNulls"],
        json!([["set-null", "set-undefined"], true, true])
    );
    assert_eq!(
        result["shared"],
        json!(["shared", ["preview-new", "preview-original"]])
    );
    assert_eq!(result["private"], "overlay");
    assert_eq!(result["missing"], false);
    assert_eq!(live.kv_list(&namespace, "item-").unwrap(), original);
    assert_eq!(
        live.kv_get(&namespace, "real-null").unwrap(),
        Some(serde_json::Value::Null)
    );
    assert!(live.kv_get(&namespace, "preview-new").unwrap().is_none());
    assert!(live
        .kv_get("shared:preview-fixture", "preview-new")
        .unwrap()
        .is_none());
    drop(daemon);
    drop(live);
    std::fs::remove_dir_all(dir).unwrap();
}
