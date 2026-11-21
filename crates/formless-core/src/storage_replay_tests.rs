use super::*;
use std::collections::BTreeSet;

fn fixture(st: &Store) -> String {
    st.create_tool_version(
        "replay_target",
        "private replay fixture",
        "{}",
        "{}",
        "function run(i){return i}",
        &Capabilities::default(),
        64,
        "agent",
    )
    .unwrap()
    .0
    .id
}
fn record(st: &Store, tool: &str, input: &str, success: bool) {
    st.record_invocation(
        tool,
        1,
        None,
        None,
        None,
        if success { "success" } else { "failure" },
        if success { "" } else { "js" },
        input,
        "{}",
        "private fixture error",
        1,
        4096,
    )
    .unwrap();
}
fn candidate() -> CandidateData {
    CandidateData {
        code: "function run(i){return i}".into(),
        description: "private candidate".into(),
        input_schema: "{}".into(),
        output_schema: "{}".into(),
        capabilities: vec![],
        memory_mb: 64,
    }
}
#[test]
fn recent_pool_is_bounded_and_not_balanced_by_outcome() {
    let st = Store::open_in_memory().unwrap();
    let tool = fixture(&st);
    for index in 0..30 {
        record(&st, &tool, &json!({"i":index}).to_string(), false);
    }
    for index in 30..60 {
        record(&st, &tool, &json!({"i":index}).to_string(), true);
    }
    let (pool, source_count, _) = st.replay_pool(&tool, 10).unwrap();
    assert_eq!(pool.len(), 10);
    assert_eq!(
        source_count, 40,
        "a full pool only reads the ordinary recent window"
    );
    assert!(
        pool.iter().all(|group| group.item.recorded_success),
        "recent successes are not downsampled to balance failures"
    );
    let snapshot = st.create_replay_snapshot(&tool, 1, 10, 50).unwrap();
    assert_eq!(snapshot.generation.len(), 5);
    assert_eq!(snapshot.holdout.len(), 5);
    assert!(st.create_replay_snapshot(&tool, 1, 513, 50).is_err());
    assert!(st.create_replay_snapshot(&tool, 1, 10, 0).is_err());
}
#[test]
fn duplicate_heavy_history_reaches_older_distinct_inputs_without_bias() {
    let st = Store::open_in_memory().unwrap();
    let tool = fixture(&st);
    for input in 0..100 {
        record(
            &st,
            &tool,
            &json!({"older": input}).to_string(),
            input % 2 == 0,
        );
    }
    for input in 0..1000 {
        record(
            &st,
            &tool,
            &json!({"poll": input % 3}).to_string(),
            input % 2 == 0,
        );
    }
    let snapshot = st.create_replay_snapshot(&tool, 1, 200, 50).unwrap();
    assert_eq!(snapshot.source_count, 1100);
    assert_eq!(snapshot.excluded_count, 0);
    assert_eq!(snapshot.holdout.len(), 51);
    assert_eq!(snapshot.generation.len(), 52);
    let (pool, count, _) = st.replay_pool(&tool, 40).unwrap();
    assert_eq!(count, 1037);
    assert_eq!(pool.len(), 40);
    assert_eq!(pool[3].item.input, r#"{"older":99.0}"#);
    assert_eq!(pool[39].item.input, r#"{"older":63.0}"#);
    assert!(pool[..3].iter().all(|group| group.item.recorded_success));
}

#[test]
fn duplicate_heavy_scan_stays_bounded_and_uses_the_recency_index() {
    let st = Store::open_in_memory().unwrap();
    let tool = fixture(&st);
    let plan = st
        .conn
        .query_row(
            &format!("EXPLAIN QUERY PLAN {REPLAY_POOL_SQL}"),
            params![tool, MAX_INPUT as i64, MAX_SCAN_ROWS as i64],
            |row| row.get::<_, String>(3),
        )
        .unwrap();
    assert!(
        plan.contains("idx_inv_replay_recent") && !plan.contains("SCAN"),
        "{plan}"
    );
    assert_eq!(
        st.conn
            .prepare(&format!("EXPLAIN QUERY PLAN {REPLAY_POOL_SQL}"))
            .unwrap()
            .query_map(
                params![tool, MAX_INPUT as i64, MAX_SCAN_ROWS as i64],
                |row| { row.get::<_, String>(3) }
            )
            .unwrap()
            .count(),
        1,
        "the query does not sort the Tool's history"
    );
    record(&st, &tool, &json!({"old": true}).to_string(), true);
    let fill = |input: &str, count: usize, label: &str| {
        st.ex(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<?1)
             INSERT INTO invocations(id,tool_id,version,status,input,created_at)
             SELECT ?2 || n, ?3, 1, 'success', ?4, '' FROM seq",
            params![count as i64, label, tool, input],
        )
        .unwrap();
    };
    fill("0", MAX_SCAN_ROWS + 1, "repeat-");
    let (pool, count, excluded) = st.replay_pool(&tool, 2).unwrap();
    assert_eq!((pool.len(), count, excluded), (1, MAX_SCAN_ROWS, 0));

    let large = format!("\"{}\"", "a".repeat(MAX_INPUT - 2));
    let additional = MAX_EXTENSION_BYTES / large.len();
    fill(&large, additional + 9, "large-");
    let (pool, count, excluded) = st.replay_pool(&tool, 2).unwrap();
    assert_eq!((pool.len(), count, excluded), (1, additional + 8, 0));
}

#[test]
fn canonical_input_groups_do_not_leak_or_inflate_and_split_ignores_outcomes() {
    let st = Store::open_in_memory().unwrap();
    let tool = fixture(&st);
    for input in [r#"{"a":1,"b":[2]}"#, r#"{ "b":[2.0], "a":1.0 }"#] {
        for _ in 0..8 {
            record(&st, &tool, input, true);
        }
    }
    for index in 0..18 {
        record(
            &st,
            &tool,
            &json!({"unique":index}).to_string(),
            index % 2 == 0,
        );
    }
    let before = st.create_replay_snapshot(&tool, 1, 100, 50).unwrap();
    assert_eq!(before.generation.len() + before.holdout.len(), 19);
    let inputs = |items: &[ReplayItem]| {
        items
            .iter()
            .map(|i| i.input.clone())
            .collect::<BTreeSet<_>>()
    };
    assert!(inputs(&before.generation).is_disjoint(&inputs(&before.holdout)));
    assert!(before.provenance["groups"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["source_invocations"].as_array().unwrap().len() == 16));
    st.ex("UPDATE invocations SET status=CASE status WHEN 'success' THEN 'failure' ELSE 'success' END WHERE tool_id=?1",[&tool]).unwrap();
    let after = st.create_replay_snapshot(&tool, 1, 100, 50).unwrap();
    assert_eq!(inputs(&before.generation), inputs(&after.generation));
    assert_eq!(inputs(&before.holdout), inputs(&after.holdout));
    let huge = json!({"x":"x".repeat(MAX_INPUT+1)}).to_string();
    record(&st, &tool, &huge, false);
    record(&st, &tool, "not JSON", false);
    let excluded = st.create_replay_snapshot(&tool, 1, 100, 50).unwrap();
    assert_eq!(excluded.excluded_count, 2);
}
#[test]
fn persisted_evidence_survives_restart_pruning_and_cannot_be_forged_in_detail() {
    let root = std::env::temp_dir().join(format!("fl-replay-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("profile.db");
    let st = Store::open(&file).unwrap();
    let tool = fixture(&st);
    for index in 0..20 {
        record(
            &st,
            &tool,
            &json!({"input":index}).to_string(),
            index % 3 != 0,
        );
    }
    let snapshot = st.create_replay_snapshot(&tool, 1, 100, 50).unwrap();
    let attempt = st
        .create_attempt_with_replay_snapshot(&tool, 1, &candidate(), 0, &snapshot.id)
        .unwrap();
    st.discard_unbound_replay_snapshot(&snapshot.id).unwrap();
    assert!(
        st.attempt_replay_snapshot(&attempt).unwrap().is_some(),
        "unbound cleanup cannot delete bound proof"
    );
    let unused = st.create_replay_snapshot(&tool, 1, 100, 50).unwrap();
    st.discard_unbound_replay_snapshot(&unused.id).unwrap();
    assert!(st.replay_snapshot(&unused.id).unwrap().is_none());

    assert!(
        st.create_attempt_with_replay_snapshot(&tool, 1, &candidate(), 0, &snapshot.id)
            .is_err(),
        "one snapshot cannot be rebound to another candidate"
    );
    let legacy = st.create_attempt(&tool, 1, &candidate(), 0).unwrap();
    st.update_attempt(
        &legacy,
        "generated",
        &json!({"snapshot_id":snapshot.id,"provenance":snapshot.provenance}).to_string(),
        None,
        None,
        0,
        0,
    )
    .unwrap();
    assert!(st.attempt_replay_snapshot(&legacy).unwrap().is_none());
    assert!(st
        .ex(
            "UPDATE optimiser_replay_snapshots SET manifest='{}' WHERE id=?1",
            [&snapshot.id]
        )
        .is_err());
    assert!(st
        .ex(
            "UPDATE optimiser_attempt_replay SET snapshot_id='forged' WHERE attempt_id=?1",
            [&attempt]
        )
        .is_err());
    st.ex("DELETE FROM invocations WHERE tool_id=?1", [&tool])
        .unwrap();
    drop(st);
    let reopened = Store::open(&file).unwrap();
    let loaded = reopened.attempt_replay_snapshot(&attempt).unwrap().unwrap();
    assert_eq!(loaded.digest, snapshot.digest);
    assert_eq!(loaded.generation, snapshot.generation);
    assert_eq!(loaded.holdout, snapshot.holdout);
    assert!(reopened.attempt_replay_snapshot(&legacy).unwrap().is_none());
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn legacy_schema_64_upgrade_has_backup_and_never_invents_replay_provenance() {
    let root = std::env::temp_dir().join(format!("fl-replay-upgrade-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("profile.db");
    let old = super::super::schema_at(64).unwrap();
    old.execute_batch("INSERT INTO tools(id,name,name_lower,current_version,created_at,updated_at) VALUES('old-tool','old_tool','old_tool',1,'2020-01-01','2020-01-01'); INSERT INTO tool_versions(id,tool_id,version,input_schema,output_schema,code,created_at) VALUES('old-v','old-tool',1,'{}','{}','function run(i){return i}','2020-01-01'); INSERT INTO optimiser_attempts(id,tool_id,base_version,status,detail,created_at) VALUES('old-attempt','old-tool',1,'awaiting_approval','legacy score only','2020-01-01'); PRAGMA user_version=64;").unwrap();
    old.execute("VACUUM INTO ?1", [file.to_str().unwrap()])
        .unwrap();
    drop(old);
    let upgraded = Store::open(&file).unwrap();
    assert_eq!(
        upgraded
            .conn
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        super::super::SCHEMA_VERSION
    );
    assert!(upgraded
        .attempt_replay_snapshot("old-attempt")
        .unwrap()
        .is_none());
    assert_eq!(
        upgraded.get_attempt("old-attempt").unwrap().unwrap().status,
        "awaiting_approval"
    );
    let backup = super::super::newest_backup(&file).unwrap();
    let original =
        Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        original
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        64
    );
    assert_eq!(
        original
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='optimiser_replay_snapshots'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        original
            .query_row(
                "SELECT detail FROM optimiser_attempts WHERE id='old-attempt'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "legacy score only"
    );
    drop(original);
    drop(upgraded);
    std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn replay_config_defaults_and_invalid_statistical_settings_fail_closed() {
    let mut cfg = crate::config::OptimiserCfg::default();
    cfg.validate_replay().unwrap();
    assert_eq!(cfg.replay_min_holdout, 20);
    assert_eq!(cfg.replay_pool_limit, 200);
    cfg.replay_min_holdout = 0;
    assert!(cfg.validate_replay().is_err());
    cfg.replay_min_holdout = 20;
    cfg.replay_significance = f64::NAN;
    assert!(cfg.validate_replay().is_err());
    cfg.replay_significance = 0.5;
    assert!(cfg.validate_replay().is_err());
    cfg.replay_significance = 0.05;
    cfg.replay_pool_limit = usize::MAX;
    assert!(cfg.validate_replay().is_err());
}

#[test]
fn bounded_evidence_store_preserves_active_proof_and_reclaims_only_old_terminal_snapshots() {
    let st = Store::open_in_memory().unwrap();
    let tool = fixture(&st);
    record(&st, &tool, "{\"i\":1}", true);
    record(&st, &tool, "{\"i\":2}", false);
    let template = st.create_replay_snapshot(&tool, 1, 10, 50).unwrap();
    st.ex(
        "DELETE FROM optimiser_replay_snapshots WHERE id=?1",
        [&template.id],
    )
    .unwrap();
    let mut attempts = Vec::new();
    for _ in 0..MAX_SNAPSHOTS {
        // A genuine immutable fixture whose timestamp predates retention;
        // production has no UPDATE path for manifest data or bindings.
        let mut snapshot = template.clone();
        snapshot.id = uuid::Uuid::new_v4().to_string();
        snapshot.created_at = "2000-01-01T00:00:00Z".into();
        snapshot.digest.clear();
        let manifest = serde_json::to_string(&snapshot).unwrap();
        let digest = hash(manifest.as_bytes());
        st.ex("INSERT INTO optimiser_replay_snapshots(id,tool_id,base_version,created_at,manifest,digest) VALUES(?1,?2,?3,?4,?5,?6)",params![snapshot.id,snapshot.tool_id,snapshot.base_version,snapshot.created_at,manifest,digest]).unwrap();
        attempts.push(
            st.create_attempt_with_replay_snapshot(&tool, 1, &candidate(), 0, &snapshot.id)
                .unwrap(),
        );
    }
    assert!(st
        .create_replay_snapshot(&tool, 1, 10, 50)
        .unwrap_err()
        .to_string()
        .contains("capacity"));
    for attempt in &attempts {
        assert!(st.attempt_replay_snapshot(attempt).unwrap().is_some());
    }
    st.update_attempt(
        &attempts[0],
        "discarded",
        "paired test failed",
        None,
        None,
        0,
        0,
    )
    .unwrap();
    let replacement = st.create_replay_snapshot(&tool, 1, 10, 50).unwrap();
    assert!(
        st.attempt_replay_snapshot(&attempts[0]).unwrap().is_none(),
        "expired terminal proof is missing, never silently resampled"
    );
    assert!(st.replay_snapshot(&replacement.id).unwrap().is_some());
    for attempt in &attempts[1..] {
        assert!(
            st.attempt_replay_snapshot(attempt).unwrap().is_some(),
            "active evidence survives cleanup"
        );
    }
    assert_eq!(
        st.conn
            .query_row("SELECT count(*) FROM optimiser_replay_snapshots", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        MAX_SNAPSHOTS
    );
}

#[test]
fn mixed_outcome_duplicates_preserve_any_known_success_regression_guard() {
    let st = Store::open_in_memory().unwrap();
    let tool = fixture(&st);
    record(&st, &tool, r#"{"a":1,"b":[2]}"#, true);
    record(&st, &tool, r#"{ "b":[2.0], "a":1.0 }"#, false);
    st.ex(
        "UPDATE invocations SET tag='denied-capability' WHERE tool_id=?1 AND status='failure'",
        [&tool],
    )
    .unwrap();
    let (groups, source_count, excluded) = st.replay_pool(&tool, 10).unwrap();
    assert_eq!(source_count, 2);
    assert_eq!(excluded, 0);
    assert_eq!(groups.len(), 1, "equivalent inputs count once");
    assert!(groups[0].item.recorded_success, "an older successful observation must retain the zero-regression guard despite a newer failure");
    assert_eq!(groups[0].sources.len(), 2);
    assert!(!groups[0].latest_success);
    let context = st.replay_set(&tool).unwrap();
    assert_eq!(context.len(), 1);
    assert!(
        !context[0].recorded_success,
        "latest denied capability remains failure context despite prior success"
    );
    assert_eq!(context[0].tag, "denied-capability");
    let snapshot = st.create_replay_snapshot(&tool, 1, 10, 50).unwrap();
    let all: Vec<_> = snapshot
        .generation
        .iter()
        .chain(&snapshot.holdout)
        .collect();
    assert_eq!(
        all.len(),
        1,
        "a mixed-outcome group cannot enter both partitions"
    );
    assert!(all[0].recorded_success);
    assert_eq!(
        snapshot.provenance["groups"][0]["latest_recorded_success"], false,
        "frozen evidence keeps distinct current-failure context for generation"
    );
}
