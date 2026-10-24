//! Browsing and editing shared modules from the UIs (SPEC §7.7).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

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
