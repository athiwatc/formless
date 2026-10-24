//! Undoing the newest Tool registry change (SPEC §7.2).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn the_newest_registry_change_can_be_undone() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-undo-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.save(&paths).unwrap();
    let id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[]).unwrap();
        store
            .create_tool_version(
                "echo_text",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                "function run(i){ return {a: 1}; }",
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
            .id
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let undo = || {
        let d = daemon.clone();
        async move { d.handle(Request::UndoRegistryChange).await }
    };
    assert!(undo().await.is_err(), "nothing recorded yet");

    // An update (here the user's code edit) is rolled back.
    daemon
        .handle(Request::UpdateToolCode {
            id: id.clone(),
            code: "function run(i){ return {a: 2}; }".into(),
        })
        .await
        .unwrap();
    let r = undo().await.unwrap();
    assert_eq!(r["undone"], "user updated echo_text v2");
    assert_eq!(r["tool"]["current_version"], 1);
    assert!(undo().await.is_err(), "one undo per change");

    // A retire is reversed.
    daemon
        .handle(Request::RetireTool { id: id.clone() })
        .await
        .unwrap();
    let r = undo().await.unwrap();
    assert_eq!(r["tool"]["retired"], false);

    // A new Tool (here an import) is retired, not purged.
    let bundle = daemon.handle(Request::ExportTool { id }).await.unwrap();
    let imported = daemon.handle(Request::ImportTool { bundle }).await.unwrap();
    assert_eq!(imported["name"], "echo_text_imported");
    let r = undo().await.unwrap();
    assert_eq!(r["undone"], "import created echo_text_imported v1");
    assert_eq!(r["tool"]["retired"], true);
    let store = Store::open(&paths.db_file()).unwrap();
    assert!(
        store
            .get_tool_by_name("echo_text_imported")
            .unwrap()
            .is_some(),
        "kept"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_is_reviewed_without_being_saved() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-prev-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.save(&paths).unwrap();
    let id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[]).unwrap();
        store
            .create_tool_version(
                "echo_text",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                "function run(i){\n  return {a: 1};\n}",
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
            .id
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let preview = |code: &str| {
        let (d, id, code) = (daemon.clone(), id.clone(), code.to_string());
        async move {
            d.handle(Request::PreviewToolCode { id, code })
                .await
                .unwrap()
        }
    };
    let good = preview("function run(i){\n  return {a: 2};\n}").await;
    assert_eq!(good["ok"], true, "{good}");
    let diff = good["diff"].as_str().unwrap();
    assert!(
        diff.contains("-   return {a: 1};") && diff.contains("+   return {a: 2};"),
        "{diff}"
    );

    let broken = preview("function run(i){ return {a: ").await;
    assert_eq!(
        (broken["ok"].clone(), broken["stage"].clone()),
        (serde_json::json!(false), serde_json::json!("syntax"))
    );
    let throws = preview("function run(i){ throw new Error('nope'); }").await;
    assert_eq!(throws["stage"], "smoke", "{throws}");
    assert!(throws["error"].as_str().unwrap().contains("nope"));

    let store = Store::open(&paths.db_file()).unwrap();
    let (info, _) = store.get_tool_current(&id).unwrap().unwrap();
    assert_eq!(info.current_version, 1, "a preview saves nothing");
    let _ = std::fs::remove_dir_all(dir);
}
