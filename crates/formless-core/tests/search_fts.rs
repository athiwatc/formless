//! Session search uses an FTS5 index (SPEC §5.1).

#[path = "support/historical.rs"]
mod historical;

use formless_core::storage::fts_query;
use formless_core::Store;

#[test]
fn fts_queries_are_quoted_prefix_terms() {
    assert_eq!(
        fts_query("deploy staging").as_deref(),
        Some("\"deploy\"* \"staging\"*")
    );
    assert_eq!(
        fts_query("it's \"quoted\" OR NOT").as_deref(),
        Some("\"it\"* \"s\"* \"quoted\"* \"OR\"* \"NOT\"*")
    );
    assert_eq!(fts_query("?? --"), None);
}

#[test]
fn search_follows_inserts_edits_and_deletes() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let db = std::path::PathBuf::from(format!("/tmp/fl-fts-{short}/f.db"));
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = Store::open(&db).unwrap();
    let a = store.create_session(Some("alpha".into())).unwrap().id;
    let b = store.create_session(Some("beta".into())).unwrap().id;
    let m = store
        .append_message(&a, "user", "Please deploy the staging server tonight")
        .unwrap();
    store
        .append_message(&b, "assistant", "Deployment notes: staging uses blue/green")
        .unwrap();
    store
        .append_message(&b, "user", "unrelated chatter")
        .unwrap();

    let hits = store.search_sessions("deploy staging", 10).unwrap();
    let sessions: Vec<&str> = hits.iter().map(|h| h.session_name.as_str()).collect();
    assert_eq!(hits.len(), 2, "prefix terms: deploy* matches Deployment");
    assert!(sessions.contains(&"alpha") && sessions.contains(&"beta"));
    assert!(hits
        .iter()
        .all(|h| h.snippet.to_lowercase().contains("deploy")));

    // Edits re-index; the old words no longer match.
    store
        .edit_message(&a, &m.id, "Please restart the cache")
        .unwrap();
    assert!(store.search_sessions("tonight", 10).unwrap().is_empty());
    assert_eq!(store.search_sessions("restart", 10).unwrap().len(), 1);
    // Deleting a Session drops its messages from the index.
    store.delete_session(&b).unwrap();
    assert!(store.search_sessions("green", 10).unwrap().is_empty());
    // No word characters: substring fallback.
    assert!(store.search_sessions("??", 10).unwrap().is_empty());
}

#[test]
fn existing_messages_are_indexed_on_upgrade() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let db = std::path::PathBuf::from(format!("/tmp/fl-ftsm-{short}/f.db"));
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    {
        let old = historical::create(&db, 23);
        old.execute_batch(
            "INSERT INTO sessions(id,name,created_at,updated_at)
             VALUES('history-fts','Old Session','2020-01-01','2020-01-01');
             INSERT INTO messages(id,session_id,seq,role,content,created_at)
             VALUES('history-message','history-fts',1,'user','remember the quarterly invoice','2020-01-01');",
        )
        .unwrap();
    }
    let store = Store::open(&db).unwrap();
    assert_eq!(store.search_sessions("invoice", 10).unwrap().len(), 1);
}
