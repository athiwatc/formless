use super::*;
fn pair(model: Option<&str>, effort: Option<&str>) -> SessionModelSelection {
    SessionModelSelection {
        model: model.map(str::to_string),
        reasoning_effort: effort.map(str::to_string),
    }
}
#[test]
fn session_selection_cas_rejects_external_companion_change_and_preserves_unrelated_metadata() {
    let root = std::env::temp_dir().join(format!("fl-selection-cas-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("owned.db");
    {
        let a = Store::open(&file).unwrap();
        let b = Store::open(&file).unwrap();
        let id = a.create_session(None).unwrap().id;
        a.set_session_model(&id, Some("old".into()), Some("low".into()))
            .unwrap();
        let expected = pair(Some("old"), Some("low"));
        let desired = pair(Some("new"), Some("high"));
        b.set_session_model(&id, Some("old".into()), Some("medium".into()))
            .unwrap();
        assert!(a
            .compare_set_session_model(&id, &expected, &desired)
            .is_err());
        let row = b.get_session(&id).unwrap().unwrap();
        assert_eq!(row.model.as_deref(), Some("old"));
        assert_eq!(row.reasoning_effort.as_deref(), Some("medium"));
        b.rename_session(&id, "Keep this name").unwrap();
        let row = a
            .compare_set_session_model(&id, &pair(Some("old"), Some("medium")), &desired)
            .unwrap();
        assert_eq!(row.name, "Keep this name");
        assert_eq!(row.model, desired.model);
        assert_eq!(row.reasoning_effort, desired.reasoning_effort);
        assert_eq!(
            a.conn
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            67
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn session_selection_cas_two_writers_cannot_overwrite_each_others_reviewed_pair() {
    let root = std::env::temp_dir().join(format!("fl-selection-race-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("owned.db");
    let st = Store::open(&file).unwrap();
    let id = st.create_session(None).unwrap().id;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    let choices = [
        pair(Some("model-A"), Some("low")),
        pair(Some("role:reviewer"), Some("high")),
    ];
    for desired in choices.clone() {
        let st = Store::open(&file).unwrap();
        let id = id.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            st.compare_set_session_model(&id, &pair(None, None), &desired)
        }));
    }
    barrier.wait();
    let rows = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.iter().filter(|r| r.is_ok()).count(), 1);
    for error in rows.iter().filter_map(|r| r.as_ref().err()) {
        assert!(error.to_string().contains("changed"));
    }
    let row = st.get_session(&id).unwrap().unwrap();
    assert!(choices
        .iter()
        .any(|p| p.model == row.model && p.reasoning_effort == row.reasoning_effort));
    drop(st);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn session_selection_cas_nulls_noop_deleted_validation_and_outer_failure_fail_closed() {
    let st = Store::open_in_memory().unwrap();
    let id = st.create_session(None).unwrap().id;
    let empty = pair(None, None);
    let desired = pair(Some("role:reviewer"), Some("high"));
    let before = st.get_session(&id).unwrap().unwrap();
    let same = st.compare_set_session_model(&id, &empty, &empty).unwrap();
    assert_eq!(before.updated_at, same.updated_at);
    let result: Result<()> = st.transaction(|st| {
        st.compare_set_session_model(&id, &empty, &desired)?;
        Err(anyhow!("outer failure"))
    });
    assert!(result.is_err());
    assert!(st.get_session(&id).unwrap().unwrap().model.is_none());
    for bad in [
        pair(Some("auto"), None),
        pair(Some("role:"), None),
        pair(Some(" bad"), None),
        pair(Some("x\u{202e}"), None),
        pair(Some(&"x".repeat(513)), None),
        pair(None, Some(&"x".repeat(65))),
    ] {
        assert!(st.compare_set_session_model(&id, &empty, &bad).is_err());
    }
    st.soft_delete_session(&id).unwrap();
    assert!(st.compare_set_session_model(&id, &empty, &desired).is_err());
    assert!(st
        .compare_set_session_model("missing", &empty, &desired)
        .is_err());
}
