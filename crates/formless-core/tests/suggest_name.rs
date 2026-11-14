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

/// A heuristic name is provisional: the model's name replaces it once the
/// model is installed, a model name is final, and a user's name is never
/// touched (#871).
#[test]
fn heuristic_names_are_provisional_and_user_names_final() {
    use formless_core::storage::DEFAULT_SESSION_NAME;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let db = std::path::PathBuf::from(format!("/tmp/fl-provisional-{short}.db"));
    let store = Store::open(&db).unwrap();

    // Downloaded after the first Run: the heuristic title is replaced.
    let a = store.create_session(None).unwrap().id;
    assert_eq!(
        store.get_session(&a).unwrap().unwrap().name,
        DEFAULT_SESSION_NAME
    );
    assert_eq!(
        store.provisionally_named_sessions(10).unwrap(),
        vec![a.clone()]
    );
    let s = store
        .auto_name_session(&a, "can you write me a tool…", "heuristic")
        .unwrap();
    assert_eq!(s.unwrap().name, "can you write me a tool…");
    assert!(store.name_is_provisional(&a).unwrap());
    assert_eq!(
        store.provisionally_named_sessions(10).unwrap(),
        vec![a.clone()]
    );
    let s = store
        .auto_name_session(&a, "CSV to JSON Tool", "model")
        .unwrap();
    assert_eq!(s.unwrap().name, "CSV to JSON Tool");
    assert!(!store.name_is_provisional(&a).unwrap());
    assert!(
        store
            .auto_name_session(&a, "Other", "model")
            .unwrap()
            .is_none(),
        "model names are final"
    );
    assert!(store
        .auto_name_session(&a, "Other", "heuristic")
        .unwrap()
        .is_none());
    assert!(store.provisionally_named_sessions(10).unwrap().is_empty());

    // Renamed by the user before the download: untouched.
    let b = store.create_session(None).unwrap().id;
    store.auto_name_session(&b, "hi", "heuristic").unwrap();
    store.rename_session(&b, "My budget").unwrap();
    assert!(!store.name_is_provisional(&b).unwrap());
    assert!(store
        .auto_name_session(&b, "Budget Planning", "model")
        .unwrap()
        .is_none());
    assert_eq!(store.get_session(&b).unwrap().unwrap().name, "My budget");

    // The same heuristic name again is not a change.
    let c = store.create_session(None).unwrap().id;
    assert!(store
        .auto_name_session(&c, "hi", "heuristic")
        .unwrap()
        .is_some());
    assert!(store
        .auto_name_session(&c, "hi", "heuristic")
        .unwrap()
        .is_none());
    assert!(store
        .auto_name_session(&c, "convert this csv", "heuristic")
        .unwrap()
        .is_some());
    let _ = std::fs::remove_file(&db);
}

#[test]
fn provisional_batch_matches_live_default_or_heuristic_eligibility() {
    use formless_core::storage::DEFAULT_SESSION_NAME;
    let root = std::env::temp_dir().join(format!("fl-provisional-batch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    struct Owned(std::path::PathBuf);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _owned = Owned(root.clone());
    let db = root.join("fixture.db");
    let store = Store::open(&db).unwrap();
    let raw = rusqlite::Connection::open(&db).unwrap();
    let mut eligible = Vec::new();
    for (name, source, deleted, archived, stamp, include) in [
        (
            DEFAULT_SESSION_NAME,
            None,
            false,
            false,
            "2026-01-01T00:00:01Z",
            true,
        ),
        (
            "ไทย heuristic",
            Some("heuristic"),
            false,
            true,
            "2026-01-01T00:00:02Z",
            true,
        ),
        (
            DEFAULT_SESSION_NAME,
            Some("model"),
            false,
            false,
            "2026-01-01T00:00:03Z",
            true,
        ),
        (
            "User name",
            None,
            false,
            false,
            "2026-01-01T00:00:04Z",
            false,
        ),
        (
            "Model name",
            Some("model"),
            false,
            false,
            "2026-01-01T00:00:05Z",
            false,
        ),
        (
            DEFAULT_SESSION_NAME,
            None,
            true,
            false,
            "2026-01-01T00:00:06Z",
            false,
        ),
        (
            "Deleted heuristic",
            Some("heuristic"),
            true,
            false,
            "2026-01-01T00:00:07Z",
            false,
        ),
    ] {
        let session = store.create_session(Some(name.into())).unwrap();
        raw.execute("UPDATE sessions SET name_source=?2, deleted_at=?3, archived=?4, updated_at=?5 WHERE id=?1",
            rusqlite::params![session.id, source, if deleted {Some("2026-01-02")}else{None}, archived, stamp]).unwrap();
        if include {
            eligible.push(session.id);
        }
    }
    eligible.reverse();
    assert_eq!(store.provisionally_named_sessions(10).unwrap(), eligible);
    assert_eq!(
        store.provisionally_named_sessions(1).unwrap(),
        eligible[..1]
    );
    assert!(store.provisionally_named_sessions(0).unwrap().is_empty());
}
