use super::*;

fn old_snapshot(store: &Store) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for info in store.list_modules()? {
        if !info.retired {
            if let Some((_, code)) = store.get_module_by_name(&info.name)? {
                out.push((info.name, code));
            }
        }
    }
    Ok(out)
}

#[test]
fn module_snapshot_matches_ordered_current_versions_and_retirement() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.list_module_code().unwrap().is_empty());
    let before = store.modules_fingerprint().unwrap();
    let (z, _) = store
        .create_module_version("Zulu", "description", "old z", "agent")
        .unwrap();
    store
        .create_module_version("Zulu", "updated", "new z", "agent")
        .unwrap();
    let (retired, _) = store
        .create_module_version("retired", "description", "ignored", "agent")
        .unwrap();
    store
        .conn
        .execute("UPDATE modules SET retired=1 WHERE id=?1", [&retired.id])
        .unwrap();
    store
        .create_module_version("Alpha", "description", "alpha", "agent")
        .unwrap();
    store
        .create_module_version("ไทย", "คำอธิบาย", "日本語\nไทย", "agent")
        .unwrap();
    store
        .create_module_version("Éclair", "description", "unicode fold", "agent")
        .unwrap();
    let fingerprint = store.modules_fingerprint().unwrap();
    assert_ne!(before, fingerprint);
    let expected = old_snapshot(&store).unwrap();
    assert_eq!(
        expected,
        vec![
            ("Alpha".into(), "alpha".into()),
            ("Zulu".into(), "new z".into()),
            ("Éclair".into(), "unicode fold".into()),
            ("ไทย".into(), "日本語\nไทย".into())
        ]
    );
    assert_eq!(store.list_module_code().unwrap(), expected);
    assert_eq!(store.modules_fingerprint().unwrap(), fingerprint);
    store
        .conn
        .execute("UPDATE modules SET current_version=1 WHERE id=?1", [&z.id])
        .unwrap();
    assert_eq!(
        store.list_module_code().unwrap(),
        old_snapshot(&store).unwrap()
    );
    assert_eq!(store.list_module_code().unwrap()[1].1, "old z");
}

#[test]
fn module_snapshot_keeps_metadata_skip_and_id_conversion_errors() {
    for field in [
        "name",
        "description",
        "current_version",
        "retired",
        "created_at",
        "updated_at",
    ] {
        let store = Store::open_in_memory().unwrap();
        store
            .create_module_version("valid", "description", "valid code", "agent")
            .unwrap();
        let (bad, _) = store
            .create_module_version("bad", "description", "unused", "agent")
            .unwrap();
        store
            .conn
            .execute(
                &format!("UPDATE modules SET {field}=x'FF' WHERE id=?1"),
                [&bad.id],
            )
            .unwrap();
        // Even a missing version for skipped metadata must not become an error.
        store
            .conn
            .execute("DELETE FROM module_versions WHERE module_id=?1", [&bad.id])
            .unwrap();
        assert_eq!(
            store.list_module_code().unwrap(),
            old_snapshot(&store).unwrap(),
            "{field}"
        );
        assert_eq!(
            store.list_module_code().unwrap(),
            vec![("valid".into(), "valid code".into())]
        );
    }
    let store = Store::open_in_memory().unwrap();
    store.conn.execute("INSERT INTO modules(id,name,name_lower,created_at,updated_at) VALUES(x'FF','bad','bad','fixture','fixture')", []).unwrap();
    assert!(old_snapshot(&store).is_err());
    assert!(store.list_module_code().is_err());
}

#[test]
fn module_snapshot_keeps_missing_and_unreadable_current_code_as_errors() {
    let store = Store::open_in_memory().unwrap();
    let (m, _) = store
        .create_module_version("current", "description", "code", "agent")
        .unwrap();
    store
        .conn
        .execute(
            "UPDATE module_versions SET code=x'FF' WHERE module_id=?1",
            [&m.id],
        )
        .unwrap();
    assert!(old_snapshot(&store).is_err());
    assert!(store.list_module_code().is_err());
    store
        .conn
        .execute("UPDATE modules SET retired=1 WHERE id=?1", [&m.id])
        .unwrap();
    assert!(store.list_module_code().unwrap().is_empty());
    store
        .conn
        .execute("DELETE FROM module_versions WHERE module_id=?1", [&m.id])
        .unwrap();
    assert!(store.list_module_code().unwrap().is_empty());
    store
        .conn
        .execute("UPDATE modules SET retired=0 WHERE id=?1", [&m.id])
        .unwrap();
    assert!(old_snapshot(&store)
        .unwrap_err()
        .downcast_ref::<rusqlite::Error>()
        .is_some_and(|e| matches!(e, rusqlite::Error::QueryReturnedNoRows)));
    assert!(store
        .list_module_code()
        .unwrap_err()
        .downcast_ref::<rusqlite::Error>()
        .is_some_and(|e| matches!(e, rusqlite::Error::QueryReturnedNoRows)));
}
