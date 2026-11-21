use super::*;

fn insert(store: &Store, id: &str, date: &str, status: &str, kind: &str) -> PromptProposalInfo {
    store.conn.execute("INSERT INTO prompt_proposals(id,kind,content,rationale,status,baseline_score,candidate_score,eval_total,est_tokens,created_at,applied_at) VALUES(?1,?2,?3,'เหตุผล',?4,0.4,0.9,7,13,?5,?6)",params![id,kind,format!(" {id} 日本語 "),status,date,if status=="applied" { Some(date) } else { None }]).unwrap();
    store.get_proposal(id).unwrap().unwrap()
}

#[test]
fn applied_proposal_read_preserves_mixed_status_fields_and_oldest_first_order() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.applied_proposals().unwrap().is_empty());
    assert_eq!(store.effective_system_prompt("BASE").unwrap(), "BASE");
    let newest = insert(&store, "newest", "003", "applied", "prompt");
    insert(&store, "proposed", "004", "proposed", "prompt");
    insert(&store, "measured", "002", "measured", "prompt");
    let oldest = insert(&store, "oldest", "001", "applied", "prompt");
    insert(&store, "rejected", "005", "rejected", "prompt");
    let memory = insert(&store, "memory", "002", "applied", "memory");
    let full = store.list_proposals().unwrap();
    assert_eq!(full.len(), 6);
    let previous: Vec<_> = full
        .into_iter()
        .filter(|p| p.status == "applied")
        .rev()
        .collect();
    let actual = store.applied_proposals().unwrap();
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(&previous).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value([oldest, memory, newest]).unwrap()
    );
    assert_eq!(
        store.effective_system_prompt("BASE").unwrap(),
        "BASE\n\nLearned guidance (v1): oldest 日本語\n\nLearned guidance (v2): newest 日本語"
    );
}

#[test]
fn applied_proposal_timestamp_ties_use_descending_rowid_even_with_another_index() {
    let store = Store::open_in_memory().unwrap();
    // IDs deliberately disagree with insertion order. Equal timestamps now have
    // an explicit policy; no universal equality to the old unspecified ties.
    let first = insert(&store, "z-first", "001", "applied", "prompt");
    insert(&store, "ignored", "001", "proposed", "prompt");
    let second = insert(&store, "a-second", "001", "applied", "prompt");
    let third = insert(&store, "m-third", "001", "applied", "prompt");
    let expected = serde_json::to_value([third, second, first]).unwrap();
    assert_eq!(
        serde_json::to_value(store.applied_proposals().unwrap()).unwrap(),
        expected
    );
    let prompt = "BASE\n\nLearned guidance (v1): m-third 日本語\n\nLearned guidance (v2): a-second 日本語\n\nLearned guidance (v3): z-first 日本語";
    assert_eq!(store.effective_system_prompt("BASE").unwrap(), prompt);
    store.conn.execute_batch("CREATE INDEX fixture_proposal_order ON prompt_proposals(status,created_at,id DESC); ANALYZE;").unwrap();
    assert_eq!(
        serde_json::to_value(store.applied_proposals().unwrap()).unwrap(),
        expected
    );
    assert_eq!(store.effective_system_prompt("BASE").unwrap(), prompt);
}

#[test]
fn applying_an_older_proposal_appends_after_newer_proposals() {
    let store = Store::open_in_memory().unwrap();
    let old = insert(
        &store,
        "created-first",
        "2000-01-01T00:00:00+00:00",
        "measured",
        "prompt",
    );
    let newer = insert(
        &store,
        "created-second",
        "2000-02-01T00:00:00+00:00",
        "applied",
        "prompt",
    );
    let legacy = insert(
        &store,
        "legacy",
        "2000-01-15T00:00:00+00:00",
        "applied",
        "prompt",
    );
    store
        .conn
        .execute(
            "UPDATE prompt_proposals SET applied_at=NULL WHERE id=?1",
            [&legacy.id],
        )
        .unwrap();

    let before = store.effective_system_prompt("BASE").unwrap();
    assert_eq!(before, "BASE\n\nLearned guidance (v1): legacy 日本語\n\nLearned guidance (v2): created-second 日本語");
    store
        .set_proposal_status(&old.id, "applied")
        .unwrap()
        .unwrap();
    let ids: Vec<_> = store
        .applied_proposals()
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(ids, [legacy.id, newer.id, old.id]);
    assert_eq!(
        store.effective_system_prompt("BASE").unwrap(),
        before + "\n\nLearned guidance (v3): created-first 日本語",
    );
}

#[test]
fn applied_proposal_read_skips_non_applied_corruption_but_propagates_selected_errors() {
    let store = Store::open_in_memory().unwrap();
    insert(&store, "applied", "001", "applied", "prompt");
    insert(&store, "ignored", "002", "rejected", "prompt");
    store
        .conn
        .execute(
            "UPDATE prompt_proposals SET content=x'FF',rationale=x'FF' WHERE id='ignored'",
            [],
        )
        .unwrap();
    assert!(store.list_proposals().is_err());
    assert_eq!(store.applied_proposals().unwrap().len(), 1);
    assert!(store
        .effective_system_prompt("BASE")
        .unwrap()
        .contains("applied 日本語"));
    for field in ["content", "rationale"] {
        store
            .conn
            .execute(
                &format!("UPDATE prompt_proposals SET {field}=x'FF' WHERE id='applied'"),
                [],
            )
            .unwrap();
        assert!(store.applied_proposals().is_err());
        assert!(store.effective_system_prompt("BASE").is_err());
        store
            .conn
            .execute(
                &format!("UPDATE prompt_proposals SET {field}='valid' WHERE id='applied'"),
                [],
            )
            .unwrap();
    }
    store
        .conn
        .execute_batch("DROP TABLE prompt_proposals")
        .unwrap();
    assert!(store.applied_proposals().is_err());
}
