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
fn session_and_audit_search_fold_greek_final_sigma() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let message = store.append_message(&session.id, "user", "ος").unwrap();
    let longer_word = store.append_message(&session.id, "user", "λόγος").unwrap();
    let hits = store.search_sessions("ΟΣ", 10).unwrap();
    assert!(hits.iter().any(|hit| hit.message_id == message.id));
    assert!(hits.iter().any(|hit| hit.message_id == longer_word.id));

    store.audit("tester", "greek_search", "ος").unwrap();
    let entries = store.list_audit(Some("ΟΣ"), None, 10).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].detail, "ος");
}

#[test]
fn search_reads_both_utf16_database_encodings_across_surrogate_boundaries() {
    for encoding in ["UTF-16le", "UTF-16be"] {
        let root =
            std::env::temp_dir().join(format!("fl-search-encoding-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("data.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA encoding='{encoding}'; CREATE TABLE encoding_fixture(value);"
        ))
        .unwrap();
        drop(conn);
        let store = Store::open(&db).unwrap();
        let session = store.create_session(None).unwrap();
        let text = format!("{}😀 needle λόγος ??", "a".repeat(8191));
        let message = store.append_message(&session.id, "user", &text).unwrap();
        for query in ["needle", "ΟΣ", "??"] {
            let hits = store.search_sessions(query, 10).unwrap();
            assert_eq!(hits[0].message_id, message.id, "{encoding}, {query}");
            assert!(
                hits[0].snippet.contains("😀 needle λόγος ??"),
                "{encoding}, {query}"
            );
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn unicode_verification_and_snippets_cross_storage_read_boundaries() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let text = format!("{}λόγος 文件目标", "a".repeat(16 * 1024 - 1));
    let message = store.append_message(&session.id, "user", &text).unwrap();
    let hits = store.search_sessions("ΟΣ", 10).unwrap();
    assert_eq!(hits[0].message_id, message.id);
    assert!(hits[0].snippet.contains("λόγος 文件目标"));
    let hits = store.search_sessions("文件 目标", 10).unwrap();
    assert_eq!(hits[0].message_id, message.id);
    assert!(hits[0].snippet.contains("文件目标"));
}

#[test]
fn literal_search_caps_live_candidates_before_reading_bodies() {
    let root = std::env::temp_dir().join(format!("fl-search-scope-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("data.db");
    let store = Store::open(&db).unwrap();
    let live = store.create_session(None).unwrap();
    let deleted = store.create_session(None).unwrap();
    let old = store.append_message(&live.id, "user", "?? older").unwrap();
    let excluded = store
        .append_message(&deleted.id, "user", "?? deleted")
        .unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE messages SET created_at='2000' WHERE id=?1",
        [&old.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE sessions SET deleted_at='2026' WHERE id=?1",
        [&deleted.id],
    )
    .unwrap();
    conn.execute(
        "WITH RECURSIVE seq(n) AS (SELECT 2 UNION ALL SELECT n+1 FROM seq WHERE n<10001)
         INSERT INTO messages(id, session_id, seq, role, content, created_at)
         SELECT 'literal-fixture-'||n, ?1, n, 'user', 'plain', '2026' FROM seq",
        [&live.id],
    )
    .unwrap();
    conn.execute(
        "UPDATE messages SET created_at='9999' WHERE id=?1",
        [&excluded.id],
    )
    .unwrap();
    let scoped = store.search_sessions_with_scope("??", 20).unwrap();
    assert!(scoped.hits.is_empty());
    assert_eq!(scoped.searched_latest, Some(10_000));
    conn.execute(
        "UPDATE messages SET content='?? included' WHERE id='literal-fixture-2'",
        [],
    )
    .unwrap();
    let scoped = store.search_sessions_with_scope("??", 20).unwrap();
    assert_eq!(scoped.hits.len(), 1);
    assert_eq!(scoped.hits[0].message_id, "literal-fixture-2");
    assert_eq!(store.search_sessions("??", 20).unwrap(), scoped.hits);
    assert_eq!(
        store
            .search_sessions_with_scope("included", 20)
            .unwrap()
            .searched_latest,
        None
    );
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn search_snippets_prefer_nearby_word_prefixes_and_keep_the_original_text() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let gap = "unrelated ".repeat(20);
    let message = format!(
        "Deploy early {gap} redeploy staging is an unrelated aside {gap} \
         Staging was\ndeployed for the cafe\u{301} group {gap} after"
    );
    store.append_message(&session.id, "user", &message).unwrap();
    for query in ["deploy stag", "deploy stag CAFÉ", "deploy stag cafe\u{301}"] {
        let result = &store.search_sessions(query, 1).unwrap()[0].snippet;
        assert!(result.contains("Staging was deployed for the cafe\u{301} group"));
        assert!(!result.contains("redeploy"), "{result}");
        assert!(result.starts_with('…') && result.ends_with('…'));
        assert!(result.chars().count() <= 102);
    }
    let single = &store.search_sessions("deploy", 1).unwrap()[0].snippet;
    assert_eq!(
        single,
        &format!("Deploy early {}unr…", "unrelated ".repeat(5))
    );
}

#[test]
fn multi_term_han_snippets_center_on_the_matching_passage() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let message = format!(
        "{}这里的目标内容说明了实际匹配段落{}",
        "前置背景".repeat(40),
        "后续材料".repeat(40)
    );
    store.append_message(&session.id, "user", &message).unwrap();

    let hits = store.search_sessions("目标 内容", 1).unwrap();
    let snippet = &hits[0].snippet;
    assert!(snippet.contains("目标内容"), "{snippet}");
    assert!(snippet.starts_with('…') && snippet.ends_with('…'));
    assert!(snippet.chars().count() <= 102);
    let before_match = snippet
        .split("目标内容")
        .next()
        .unwrap()
        .trim_start_matches('…');
    assert_eq!(before_match.chars().count(), 40, "{snippet}");
}

#[test]
fn overlapping_han_terms_keep_the_complete_matching_passage() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let long_term = format!("{}乙", "甲".repeat(50));
    let message = format!("{}，{}丙丁", "背景".repeat(60), long_term);
    store.append_message(&session.id, "user", &message).unwrap();

    let query = format!("{long_term} 乙");
    let hits = store.search_sessions(&query, 1).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains(&long_term), "{}", hits[0].snippet);
}

#[test]
fn search_snippet_ties_choose_the_shortest_then_earliest_span() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap();
    let gap = "遥".repeat(160);
    let message = format!(
        "alpha with extra words beta {gap} beta alpha EARLIEST {gap} \
         alpha beta LATER {gap}"
    );
    store.append_message(&session.id, "user", &message).unwrap();
    let hits = store.search_sessions("alpha beta", 10).unwrap();
    assert!(hits[0].snippet.contains("beta alpha EARLIEST"));
    assert!(!hits[0].snippet.contains("LATER"));
    assert!(hits[0].snippet.chars().count() <= 102);

    let message = format!("alpha {gap} beta and ??");
    let isolated = store.append_message(&session.id, "user", &message).unwrap();
    let hits = store.search_sessions("alpha beta", 10).unwrap();
    let snippet = &hits
        .iter()
        .find(|hit| hit.message_id == isolated.id)
        .unwrap()
        .snippet;
    assert!(snippet.contains("beta"));
    assert!(!snippet.contains("alpha"));
    assert!(snippet.chars().count() <= 102);
    assert!(store.search_sessions("??", 1).unwrap()[0]
        .snippet
        .ends_with("beta and ??"));
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

#[test]
fn tied_search_results_follow_recording_order_in_both_sql_paths() {
    let root = std::env::temp_dir().join(format!("fl-search-order-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("data.db");
    let store = Store::open(&db).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    let session = store.create_session(Some("ไทย 日本語".into())).unwrap().id;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let message = store
            .append_message(&session, "user", "ไทย needle ?? 日本語")
            .unwrap();
        conn.execute(
            "UPDATE messages SET created_at='2026-01-02T00:00:00Z' WHERE id=?1",
            [&message.id],
        )
        .unwrap();
        ids.push(message.id);
    }
    for query in ["needle", "??"] {
        let hits = store.search_sessions(query, 10).unwrap();
        assert_eq!(
            hits.iter().map(|h| &h.message_id).collect::<Vec<_>>(),
            ids.iter().rev().collect::<Vec<_>>()
        );
        for limit in [0, 1] {
            assert_eq!(
                store.search_sessions(query, limit).unwrap()[0].message_id,
                ids[2]
            );
        }
        assert!(hits.iter().all(|hit| hit.session_id == session
            && hit.session_name == "ไทย 日本語"
            && hit.role == "user"
            && hit.snippet.contains("日本語")));
    }
    // The timestamp key still dominates row insertion order in both paths.
    conn.execute(
        "UPDATE messages SET created_at='2026-01-03T00:00:00Z' WHERE id=?1",
        [&ids[0]],
    )
    .unwrap();
    conn.execute(
        "UPDATE messages SET created_at='2026-01-01T00:00:00Z' WHERE id=?1",
        [&ids[2]],
    )
    .unwrap();
    for query in ["needle", "??"] {
        assert_eq!(
            store
                .search_sessions(query, 10)
                .unwrap()
                .iter()
                .map(|h| &h.message_id)
                .collect::<Vec<_>>(),
            ids.iter().collect::<Vec<_>>()
        );
    }
    for _ in 0..205 {
        store
            .append_message(&session, "assistant", "ไทย needle ?? 日本語")
            .unwrap();
    }
    assert_eq!(
        store.search_sessions("needle", usize::MAX).unwrap().len(),
        200
    );
    assert_eq!(store.search_sessions("??", 201).unwrap().len(), 200);
    assert!(store.search_sessions("", 10).unwrap().is_empty());
    assert!(store
        .search_sessions("no_matching_token", 10)
        .unwrap()
        .is_empty());
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn search_relevance_still_precedes_newer_timestamps_and_rowids() {
    let root = std::env::temp_dir().join(format!("fl-search-score-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("data.db");
    let store = Store::open(&db).unwrap();
    let session = store.create_session(None).unwrap().id;
    let repeated = store
        .append_message(&session, "user", "needle needle needle needle needle")
        .unwrap();
    let newer = store
        .append_message(&session, "assistant", "needle and several unrelated words")
        .unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE messages SET created_at=?2 WHERE id=?1",
        rusqlite::params![repeated.id, "2026-01-01T00:00:00Z"],
    )
    .unwrap();
    conn.execute(
        "UPDATE messages SET created_at=?2 WHERE id=?1",
        rusqlite::params![newer.id, "2026-01-03T00:00:00Z"],
    )
    .unwrap();
    let rows: Vec<(String,f64)>=conn.prepare("SELECT m.id,bm25(messages_fts) FROM messages_fts f JOIN messages m ON m.rowid=f.rowid WHERE messages_fts MATCH '\"needle\"*' ORDER BY bm25(messages_fts),m.created_at DESC").unwrap().query_map([],|row|Ok((row.get(0)?,row.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert!(rows[0].1 < rows[1].1);
    assert_eq!(rows[0].0, repeated.id);
    assert_eq!(
        store.search_sessions("needle", 1).unwrap()[0].message_id,
        repeated.id
    );
    drop(conn);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn identifier_adjacency_is_a_prefix_ranking_tier_without_removing_scattered_hits() {
    for (query, adjacent, words) in [
        ("http_get_text", "http_get_text", "http get text"),
        (
            "provider.tool_mode",
            "provider.tool_mode",
            "provider tool mode",
        ),
        ("search_fts.rs", "search_fts.rs", "search fts rs"),
        ("src/read-file.rs", "src/read-file.rs", "src read file rs"),
        (
            "http_get_text",
            "httprequest_getter_textual",
            "http get text",
        ),
        ("café_mode", "cafe\u{301}_mode", "cafe mode"),
        ("cafe\u{301}_mode", "café_mode", "cafe mode"),
    ] {
        let root = std::env::temp_dir().join(format!("fl-search-adj-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("data.db");
        let store = Store::open(&db).unwrap();
        let session = store.create_session(Some("ไทย 日本語".into())).unwrap().id;
        let a = store.append_message(&session, "user", adjacent).unwrap();
        let scattered = words
            .split_whitespace()
            .map(|word| format!("{word} filler filler"))
            .collect::<Vec<_>>()
            .join(" ");
        let scattered = format!("{scattered} ").repeat(20);
        let b = store
            .append_message(&session, "assistant", &scattered)
            .unwrap();
        let old = store.search_sessions(words, 10).unwrap();
        assert_eq!(
            old[0].message_id, b.id,
            "fixture must favor scattered terms under raw BM25: {query}"
        );
        let hits = store.search_sessions(query, 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].message_id, a.id, "{query}");
        assert_eq!(hits[1].message_id, b.id);
        assert_eq!(store.search_sessions(query, 1).unwrap()[0].message_id, a.id);
        assert_eq!(
            hits[0].snippet, adjacent,
            "original Unicode snippet remains data"
        );
        assert_eq!(hits[0].session_name, "ไทย 日本語");
        assert_eq!(hits[0].role, "user");
        assert_eq!(hits[0].seq, a.seq);
        // All membership/fields except the query-dependent snippet agree
        // with the original prefix-AND path; only identifier rank changes.
        for hit in &hits {
            let prior = old
                .iter()
                .find(|prior| prior.message_id == hit.message_id)
                .unwrap();
            assert_eq!(
                (
                    &hit.session_id,
                    &hit.session_name,
                    hit.seq,
                    &hit.role,
                    &hit.created_at
                ),
                (
                    &prior.session_id,
                    &prior.session_name,
                    prior.seq,
                    &prior.role,
                    &prior.created_at
                )
            );
        }
        let conn = rusqlite::Connection::open(&db).unwrap();
        for query in [
            words,
            "http get text",
            "provider:tool:mode",
            "it's",
            "\"http_get_text\"",
            "needle",
        ] {
            let Some(fts) = fts_query(query) else {
                continue;
            };
            let expected:Vec<String>=conn.prepare("SELECT m.id FROM messages_fts f JOIN messages m ON m.rowid=f.rowid WHERE messages_fts MATCH ?1 ORDER BY bm25(messages_fts),m.created_at DESC,m.rowid DESC LIMIT 10").unwrap().query_map([fts],|row|row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
            assert_eq!(
                store
                    .search_sessions(query, 10)
                    .unwrap()
                    .into_iter()
                    .map(|hit| hit.message_id)
                    .collect::<Vec<_>>(),
                expected,
                "unchanged non-identifier query {query}"
            );
        }
        drop(conn);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
