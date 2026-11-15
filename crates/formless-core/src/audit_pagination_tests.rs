use super::*;
use std::collections::HashSet;

fn insert(store: &Store, id: &str, date: &str, action: &str, detail: &str) {
    store
        .conn
        .execute(
            "INSERT INTO audit(id,actor,action,detail,created_at) VALUES(?1,'user',?2,?3,?4)",
            params![id, action, detail, date],
        )
        .unwrap();
}

#[test]
fn audit_compound_cursor_reaches_all_ties_across_vacuum_and_mixed_dates() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.list_audit(Some("*"), None, 50).unwrap().is_empty());
    // Insert out of id order so the documented tie-order change is observable.
    for i in (0..120).rev() {
        insert(
            &store,
            &format!("tie-{i:03}"),
            "2026-01-02T00:00:00Z",
            "approve",
            &format!("ไทย 日本語 {i}"),
        );
    }
    insert(&store, "new", "2026-01-03T00:00:00Z", "approve", "new");
    insert(&store, "old", "2026-01-01T00:00:00Z", "approve", "old");
    let expected = store.list_audit(Some("*"), None, 500).unwrap();
    assert_eq!(expected[1].detail, "ไทย 日本語 119");
    let mut collected = Vec::new();
    let mut before = None;
    loop {
        let page = store.list_audit(Some("*"), before.as_deref(), 50).unwrap();
        if page.is_empty() {
            break;
        }
        before = page.last().unwrap().cursor.clone();
        collected.extend(page);
        store.conn.execute_batch("VACUUM").unwrap();
    }
    assert_eq!(collected, expected);
    assert_eq!(collected.len(), 122);
    assert_eq!(
        collected
            .iter()
            .map(|e| e.cursor.as_deref().unwrap())
            .collect::<HashSet<_>>()
            .len(),
        122
    );
    assert_eq!(
        store
            .list_audit(Some("*"), Some("2026-01-02T00:00:00Z"), 50)
            .unwrap()
            .iter()
            .map(|e| e.detail.as_str())
            .collect::<Vec<_>>(),
        ["old"]
    );
    assert_eq!(store.list_audit(Some("*"), None, 0).unwrap().len(), 1);
    for i in 0..510 {
        insert(&store, &format!("more-{i}"), "2025", "approve", "extra");
    }
    assert_eq!(
        store.list_audit(Some("*"), None, usize::MAX).unwrap().len(),
        500
    );
}

#[test]
fn audit_cursor_keeps_filters_and_unicode_fields_across_pages() {
    let store = Store::open_in_memory().unwrap();
    for (id, action, detail) in [
        ("a", "tool.approve", "ไทย"),
        ("b", "tool.run_unasked", "fetch"),
        ("c", "tool.reject", "FETCH 日本語"),
    ] {
        insert(&store, id, "same", action, detail);
    }
    for (filter, expected) in [
        (None, vec!["c", "a"]),
        (Some(""), vec!["c", "a"]),
        (Some("*"), vec!["c", "b", "a"]),
        (Some("run_unasked"), vec!["b"]),
        (Some(" fetch "), vec!["c", "b"]),
        (Some("-approve"), vec!["c", "b"]),
        (Some("USER"), vec!["c", "b", "a"]),
        (Some("no match"), vec![]),
    ] {
        let mut before = None;
        let mut ids = Vec::new();
        loop {
            let page = store.list_audit(filter, before.as_deref(), 1).unwrap();
            let Some(entry) = page.first() else {
                break;
            };
            assert_eq!(entry.actor, "user");
            let (_, id) = crate::audit_cursor::decode(entry.cursor.as_deref()).unwrap();
            ids.push(id.unwrap());
            before = entry.cursor.clone();
        }
        assert_eq!(ids, expected, "filter {filter:?}");
    }
}

#[test]
fn audit_cursor_protocol_round_trip_and_malformed_boundaries() {
    use base64::Engine;
    let cursor = crate::audit_cursor::encode("2026-01-01T00:00:00Z", "id ไทย:日本語");
    let request = formless_protocol::Request::ListAudit {
        filter: Some("*".into()),
        limit: Some(50),
        before: Some(cursor.clone()),
    };
    let request: formless_protocol::Request =
        serde_json::from_value(serde_json::to_value(request).unwrap()).unwrap();
    let formless_protocol::Request::ListAudit { before, .. } = request else {
        panic!("wrong request")
    };
    assert_eq!(
        crate::audit_cursor::decode(before.as_deref()).unwrap(),
        (
            Some("2026-01-01T00:00:00Z".into()),
            Some("id ไทย:日本語".into())
        )
    );
    // Responses from older daemons remain readable by a newer client.
    let legacy: formless_protocol::AuditEntry = serde_json::from_value(
        serde_json::json!({"actor":"user","action":"approve","detail":"ไทย","created_at":"same"}),
    )
    .unwrap();
    assert!(legacy.cursor.is_none());
    assert!(serde_json::to_value(legacy)
        .unwrap()
        .get("cursor")
        .is_none());
    let store = Store::open_in_memory().unwrap();
    let mut malformed = vec![
        "audit:v2:anything".to_string(),
        "audit:v1:".into(),
        "audit:v1:not base64!".into(),
        "audit:".into(),
    ];
    for payload in [
        "null",
        "[]",
        r#"["date"]"#,
        r#"["date",1]"#,
        r#"["date","id","extra"]"#,
        r#"["","id"]"#,
        r#"["date",""]"#,
    ] {
        malformed.push(format!(
            "audit:v1:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload)
        ));
    }
    for cursor in malformed {
        assert!(
            format!(
                "{:#}",
                store.list_audit(None, Some(&cursor), 50).unwrap_err()
            )
            .contains("invalid audit cursor"),
            "{cursor}"
        );
    }
}

#[tokio::test]
async fn audit_cursor_passes_through_daemon_list_audit_requests() {
    use std::sync::Arc;
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let fixture = Fixture(
        std::env::temp_dir().join(format!("formless-audit-cursor-{}", uuid::Uuid::new_v4())),
    );
    let paths = crate::Paths::from_dirs(fixture.0.join("data"), fixture.0.join("config")).unwrap();
    let mut cfg = crate::Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.save(&paths).unwrap();
    {
        let store = Store::open(&paths.db_file()).unwrap();
        for i in 0..120 {
            insert(
                &store,
                &format!("id-{i:03}"),
                "same",
                "cursor.fixture",
                &format!("ไทย {i}"),
            );
        }
    }
    let daemon = Arc::new(crate::daemon::Daemon::new(paths).unwrap());
    let mut before = None;
    let mut details = Vec::new();
    for expected_size in [50, 50, 20, 0] {
        let req = formless_protocol::Request::ListAudit {
            filter: Some("cursor.fixture".into()),
            before,
            limit: Some(50),
        };
        let req = serde_json::from_value(serde_json::to_value(req).unwrap()).unwrap();
        let rows: Vec<formless_protocol::AuditEntry> =
            serde_json::from_value(daemon.handle(req).await.unwrap()).unwrap();
        assert_eq!(rows.len(), expected_size);
        before = rows.last().and_then(|row| row.cursor.clone());
        details.extend(rows.into_iter().map(|row| row.detail));
    }
    assert_eq!(
        details,
        (0..120)
            .rev()
            .map(|i| format!("ไทย {i}"))
            .collect::<Vec<_>>()
    );
    let error = daemon
        .handle(formless_protocol::Request::ListAudit {
            filter: None,
            before: Some("audit:v1:bad!".into()),
            limit: None,
        })
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("invalid audit cursor"));
}
