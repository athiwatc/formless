//! Suggesting a fresh Session name from recent messages (SPEC §10.4).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn suggests_a_name_from_recent_messages_without_renaming() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-name-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-name-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    // No naming model: the heuristic names it.
    cfg.naming.model = "none".into();
    cfg.naming.auto_download = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(Some("Old topic".into())).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    let empty = daemon
        .handle(Request::SuggestSessionName { id: sid.clone() })
        .await
        .unwrap();
    assert!(empty["name"].is_null());

    for text in [
        "fix the login page",
        "now plan the quarterly budget",
        "add a travel line",
        "and the office rent",
        "total it up",
    ] {
        store.append_message(&sid, "user", text).unwrap();
        store.append_message(&sid, "assistant", "done").unwrap();
    }
    let r = daemon
        .handle(Request::SuggestSessionName { id: sid.clone() })
        .await
        .unwrap();
    let name = r["name"].as_str().unwrap();
    // From the recent messages, not the first one.
    assert!(name.to_lowercase().contains("budget"), "{name}");
    assert_eq!(store.get_session(&sid).unwrap().unwrap().name, "Old topic");
    let _ = std::fs::remove_dir_all(&dir);
}
