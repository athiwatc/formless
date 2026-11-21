//! Browsing and editing shared modules from the UIs (SPEC §7.7).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

#[test]
fn module_dependents_scan_current_active_code_with_literal_unicode_matching() {
    let root = std::env::temp_dir().join(format!("fl-module-dependents-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("registry.db");
    let store = Store::open(&db).unwrap();
    let name = "Módulo_日本語";
    assert!(store.module_dependents(name).unwrap().is_empty());

    let save = |tool, code| {
        store
            .create_tool_version(
                tool,
                "fixture",
                "{}",
                "{}",
                code,
                &Capabilities::default(),
                64,
                "user",
            )
            .unwrap()
            .0
    };
    for (tool, code) in [
        ("Zulu", "ReQuIrEmOdUlE('MÓDULO_日本語')"),
        ("alpha", "Modules.Load(\"módulo_日本語\")"),
        ("Bravo", "requireModule(`módulo_日本語`)"),
        ("d_comment", "// modules.load('módulo_日本語')"),
        ("e_space", "requireModule ('módulo_日本語')"),
        ("f_computed", "modules.load(prefix + 'módulo_日本語')"),
        ("g_different", "modules.load('módulo_日本語-other')"),
        ("h_stale", "requireModule('módulo_日本語')"),
    ] {
        save(tool, code);
    }
    save("h_stale", "function run() {}");
    let retired = save("i_retired", "modules.load('módulo_日本語')");
    store.set_tool_retired(&retired.id, true).unwrap();

    assert_eq!(
        store.module_dependents(name).unwrap(),
        ["alpha", "Bravo", "d_comment", "Zulu"]
    );
    assert!(store.module_dependents("unrelated").unwrap().is_empty());

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE tools SET current_version=999 WHERE id=?1",
        [&retired.id],
    )
    .unwrap();
    assert!(store.module_dependents(name).is_ok());
    conn.execute(
        "UPDATE tools SET current_version=999 WHERE name_lower='g_different'",
        [],
    )
    .unwrap();
    assert!(matches!(
        store
            .module_dependents(name)
            .unwrap_err()
            .downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::QueryReturnedNoRows)
    ));
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn modules_open_edit_roll_back_and_delete() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-mods-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.save(&paths).unwrap();
    {
        let store = Store::open(&paths.db_file()).unwrap();
        store
            .create_module_version(
                "text_utils",
                "helpers",
                "module.exports = { up: (s) => s.toUpperCase() };",
                "agent",
            )
            .unwrap();
        let caps = Capabilities::parse(&[]).unwrap();
        store
            .create_tool_version(
                "shout",
                "d",
                "{}",
                "{}",
                "function run(i){ return requireModule('text_utils').up(i.text); }",
                &caps,
                64,
                "agent",
            )
            .unwrap();
    }
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths).unwrap());
    let get = |d: Arc<formless_core::daemon::Daemon>| async move {
        d.handle(Request::GetModule {
            id: "text_utils".into(),
        })
        .await
    };
    let d = get(daemon.clone()).await.unwrap();
    assert_eq!(d["module"]["name"], "text_utils");
    assert_eq!(d["used_by"], serde_json::json!(["shout"]));
    assert_eq!(d["versions"].as_array().unwrap().len(), 1);
    let id = d["module"]["id"].as_str().unwrap().to_string();

    // Edits go through the same gate as create_module.
    let bad = daemon
        .handle(Request::UpdateModuleCode {
            id: id.clone(),
            code: "module.exports = {".into(),
        })
        .await;
    assert!(
        bad.unwrap_err().to_string().contains("syntax"),
        "a broken edit is refused"
    );
    let saved = daemon
        .handle(Request::UpdateModuleCode {
            id: id.clone(),
            code: "module.exports = { up: (s) => s.toUpperCase() + '!' };".into(),
        })
        .await
        .unwrap();
    assert_eq!(saved["version"], 2);
    let d = get(daemon.clone()).await.unwrap();
    assert_eq!(d["versions"][1]["author"], "user");

    // Rolling back makes v1's code current again, as v3.
    let rolled = daemon
        .handle(Request::RollbackModule {
            id: id.clone(),
            version: 1,
        })
        .await
        .unwrap();
    assert_eq!(rolled["version"], 3);
    let d = get(daemon.clone()).await.unwrap();
    assert_eq!(d["versions"][2]["code"], d["versions"][0]["code"]);

    daemon.handle(Request::DeleteModule { id }).await.unwrap();
    assert!(get(daemon.clone()).await.is_err(), "deleted");
    let _ = std::fs::remove_dir_all(dir);
}
