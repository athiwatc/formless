use super::*;

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-state67-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn item(content: &str) -> PlanItem {
    PlanItem {
        content: content.into(),
        status: "pending".into(),
    }
}

#[test]
fn genuine_schema66_upgrade_preserves_unknown_plan_and_recovery_and_replay_evidence() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let file = fixture.0.join("private.db");
    let old = schema_at(66).unwrap();
    let plan = serde_json::to_string(&vec![item("legacy task")]).unwrap();
    old.execute_batch("INSERT INTO sessions(id,name,created_at,updated_at) VALUES('s','private legacy','2026-01-01','2026-01-01');
        INSERT INTO messages(id,session_id,seq,role,content,created_at) VALUES('m1','s',1,'user','first','2026-01-01'),('m2','s',2,'assistant','last','2026-01-01');
        INSERT INTO runs(id,session_id,status,created_at,updated_at) VALUES('r','s','completed','2026-01-01','2026-01-01');
        INSERT INTO tools(id,name,name_lower,current_version,created_at,updated_at) VALUES('tool','private_tool','private_tool',1,'2026-01-01','2026-01-01');
        INSERT INTO tool_versions(id,tool_id,version,input_schema,output_schema,code,created_at) VALUES('version','tool',1,'{}','{}','function run(i){return i}','2026-01-01');
        INSERT INTO optimiser_attempts(id,tool_id,base_version,status,detail,created_at) VALUES('attempt','tool',1,'awaiting_approval','private fixture','2026-01-01');
        PRAGMA user_version=66;").unwrap();
    old.execute("UPDATE sessions SET plan=?1 WHERE id='s'", params![plan])
        .unwrap();
    let image = crate::review_file_io::ReviewImage {
        bytes: Some(b"original\r\n".to_vec()),
        security_complete: true,
        unix_metadata: None,
        ..Default::default()
    };
    let metadata = crate::review_file_io::encode_before_metadata(&image).unwrap();
    let after=json!({"version":1,"kind":"file","sha256":format!("{:x}",Sha256::digest(b"after")),"size":5,"metadata":metadata}).to_string();
    old.execute("INSERT INTO run_files(run_id,session_id,path,existed,kept,before,created_at,before_metadata,before_kind,after_state) VALUES('r','s','private.txt',1,1,?1,'2026-01-01',?2,'file',?3)",params![image.bytes.as_deref(),metadata,after]).unwrap();
    let proof = ReplaySnapshot {
        id: "proof".into(),
        digest: String::new(),
        tool_id: "tool".into(),
        base_version: 1,
        generation: vec![ReplayItem {
            input: "{\"x\":-1}".into(),
            error: "private failure".into(),
            tag: String::new(),
            recorded_success: false,
        }],
        holdout: vec![ReplayItem {
            input: "{\"x\":1}".into(),
            error: String::new(),
            tag: String::new(),
            recorded_success: true,
        }],
        source_count: 2,
        excluded_count: 0,
        created_at: "2026-01-01".into(),
        provenance: json!({"format":"optimiser-replay-v1"}),
    };
    let manifest = serde_json::to_string(&proof).unwrap();
    let digest = format!("{:x}", Sha256::digest(manifest.as_bytes()));
    old.execute("INSERT INTO optimiser_replay_snapshots(id,tool_id,base_version,created_at,manifest,digest) VALUES('proof','tool',1,'2026-01-01',?1,?2)",params![manifest,digest]).unwrap();
    old.execute(
        "INSERT INTO optimiser_attempt_replay(attempt_id,snapshot_id) VALUES('attempt','proof')",
        [],
    )
    .unwrap();
    old.execute("VACUUM INTO ?1", [file.to_str().unwrap()])
        .unwrap();
    drop(old);
    {
        let store = Store::open(&file).unwrap();
        assert_eq!(
            store
                .conn
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            67
        );
        assert_eq!(store.session_plan("s").unwrap(), vec![item("legacy task")]);
        let state = store.session_plan_state("s").unwrap().unwrap();
        assert_eq!(state.origin, "legacy");
        assert!(state.run_id.is_none() && state.updated_at.is_none());
        assert!(store.latest_recorded_run_recap("s").unwrap().is_none());
        assert_eq!(
            store
                .conn
                .query_row("SELECT COUNT(*) FROM session_plan_revisions", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0,
            "migration must not invent a revision boundary"
        );
        let row = store.run_file_record("s", "r", "private.txt").unwrap();
        assert_eq!(row.before, image.bytes);
        assert_eq!(row.before_metadata.as_deref(), Some(metadata.as_str()));
        assert_eq!(row.after_state.as_deref(), Some(after.as_str()));
        assert_eq!(
            store
                .attempt_replay_snapshot("attempt")
                .unwrap()
                .unwrap()
                .digest,
            digest
        );
        assert!(store
            .ex(
                "UPDATE optimiser_replay_snapshots SET manifest='tamper' WHERE id='proof'",
                []
            )
            .is_err());
        assert!(store.ex("UPDATE optimiser_attempt_replay SET snapshot_id='proof' WHERE attempt_id='attempt'",[]).is_err());
        let early = store.fork_session("s", None, Some(1)).unwrap().unwrap();
        assert!(
            store.session_plan(&early.id).unwrap().is_empty(),
            "unknown legacy history cannot be applied to an earlier cut"
        );
        let full = store.fork_session("s", None, None).unwrap().unwrap();
        assert_eq!(
            store.session_plan(&full.id).unwrap(),
            vec![item("legacy task")]
        );
        let state = store.session_plan_state(&full.id).unwrap().unwrap();
        assert_eq!(state.origin, "copied");
        assert!(state.run_id.is_none() && state.updated_at.is_none());
        let original = newest_backup(&file).unwrap();
        assert_eq!(
            crate::backup_verification::inspect(&original)
                .unwrap()
                .schema_version,
            66
        );
        let backup = fixture.0.join("upgraded67.db");
        store.backup_into(&backup).unwrap();
        let report = crate::backup_verification::inspect(&backup).unwrap();
        assert_eq!(report.schema_version, 67);
        assert_eq!(report.tables["run_files"], 1);
        assert_eq!(report.tables["optimiser_replay_snapshots"], 1);
    }
    let reopened = Store::open(&file).unwrap();
    assert_eq!(
        reopened
            .attempt_replay_snapshot("attempt")
            .unwrap()
            .unwrap()
            .digest,
        digest
    );
    assert_eq!(
        reopened
            .run_file_record("s", "r", "private.txt")
            .unwrap()
            .after_state
            .as_deref(),
        Some(after.as_str())
    );
    assert_eq!(
        reopened.session_plan_state("s").unwrap().unwrap().origin,
        "legacy"
    );
}

#[test]
fn retained_plan_history_is_not_expired_and_same_anchor_coalesces_without_losing_clears() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap().id;
    store
        .append_message(&session, "user", "first task")
        .unwrap();
    let run = store.create_run(&session).unwrap();
    store
        .set_run_plan(&session, &run, &[item("first plan")])
        .unwrap();
    store
        .append_message(&session, "assistant", "clear boundary")
        .unwrap();
    store.set_run_plan(&session, &run, &[]).unwrap();
    // More than the rejected 128-revision / 1 MiB retention proposal: every
    // retained transcript boundary must still support a historical fork.
    for index in 0..260 {
        store
            .append_message(&session, "assistant", "next boundary")
            .unwrap();
        let items = (0..20)
            .map(|step| item(&format!("{index:03}-{step:02}-{}", "x".repeat(190))))
            .collect::<Vec<_>>();
        store.set_run_plan(&session, &run, &items).unwrap();
    }
    let (count, bytes): (i64, i64) = store
        .conn
        .query_row(
            "SELECT COUNT(*),SUM(length(items)) FROM session_plan_revisions WHERE session_id=?1",
            params![session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 262);
    assert!(bytes > 1024 * 1024);
    store
        .set_run_plan(&session, &run, &[item("same anchor replacement")])
        .unwrap();
    store
        .set_run_plan(&session, &run, &[item("same anchor replacement")])
        .unwrap();
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM session_plan_revisions WHERE session_id=?1",
                params![session],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        count
    );
    let first = store
        .fork_session(&session, None, Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        store.session_plan(&first.id).unwrap(),
        vec![item("first plan")]
    );
    let clear = store
        .fork_session(&session, None, Some(2))
        .unwrap()
        .unwrap();
    assert!(store.session_plan(&clear.id).unwrap().is_empty());
    assert_eq!(
        store.session_plan_state(&clear.id).unwrap().unwrap().origin,
        "copied"
    );
    store.delete_session(&session).unwrap();
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM session_plan_revisions WHERE session_id=?1",
                params![session],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn exact_terminal_recaps_are_immutable_and_survive_restart_without_latest_fallback() {
    let fixture = Fixture::new();
    let file = fixture.0.join("private.db");
    let (session, old, latest, recap) = {
        let store = Store::open(&file).unwrap();
        let session = store.create_session(None).unwrap().id;
        store.append_message(&session, "user", "task").unwrap();
        let run = store.create_run(&session).unwrap();
        store.set_run_status(&run, "paused").unwrap();
        let recap = RunRecap {
            status: "paused".into(),
            steps: 4,
            duration_ms: 123,
            total_calls: Some(2),
            unfinished_calls: 1,
            used: vec!["private nested call".into()],
            built: vec!["created actual version".into()],
            ..Default::default()
        };
        store.record_run_recap(&session, &run, &recap).unwrap();
        store.record_run_recap(&session, &run, &recap).unwrap();
        let mut different = recap.clone();
        different.duration_ms += 1;
        assert!(store.record_run_recap(&session, &run, &different).is_err());
        assert!(store
            .ex(
                "UPDATE run_recaps SET recap='{}' WHERE run_id=?1",
                params![run]
            )
            .is_err());
        let foreign = store.create_session(None).unwrap().id;
        assert!(store.recorded_run_recap(&foreign, &run).is_err());
        store.set_run_status(&run, "running").unwrap();
        assert_eq!(
            store
                .recorded_run_recap(&session, &run)
                .unwrap()
                .unwrap()
                .recap,
            recap,
            "later Run-row mutation does not rewrite a recorded historical snapshot"
        );
        let latest = store.create_run(&session).unwrap();
        store.set_run_status(&latest, "completed").unwrap();
        assert!(
            store.latest_recorded_run_recap(&session).unwrap().is_none(),
            "latest ineligible/gap Run cannot fall back to an old eligible recap"
        );
        (session, run, latest, recap)
    };
    let store = Store::open(&file).unwrap();
    assert_eq!(
        store
            .recorded_run_recap(&session, &old)
            .unwrap()
            .unwrap()
            .recap,
        recap
    );
    assert!(store.latest_recorded_run_recap(&session).unwrap().is_none());
    let mut too_large = RunRecap {
        status: "completed".into(),
        steps: 4,
        ..Default::default()
    };
    too_large.files = vec!["x".repeat(64 * 1024)];
    assert!(store
        .record_run_recap(&session, &latest, &too_large)
        .is_err());
    store.delete_session(&session).unwrap();
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM run_recaps WHERE session_id=?1",
                params![session],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn portable_plan_history_maps_real_message_sequences_and_strips_foreign_run_authority() {
    let store = Store::open_in_memory().unwrap();
    let source = store.create_session(None).unwrap().id;
    store.append_message(&source, "user", "first").unwrap();
    let run = store.create_run(&source).unwrap();
    store
        .set_run_plan(&source, &run, &[item("first plan")])
        .unwrap();
    store
        .append_message(&source, "assistant", "omitted gap")
        .unwrap();
    store
        .append_message(&source, "assistant", "clear boundary")
        .unwrap();
    store.set_run_plan(&source, &run, &[]).unwrap();
    store
        .append_message(&source, "assistant", "second omitted gap")
        .unwrap();
    store
        .append_message(&source, "assistant", "latest boundary")
        .unwrap();
    store
        .set_run_plan(&source, &run, &[item("latest plan")])
        .unwrap();
    store
        .ex(
            "DELETE FROM messages WHERE session_id=?1 AND seq IN (2,4)",
            params![source],
        )
        .unwrap();
    let bundle = store.session_plan_bundle(&source).unwrap();
    let messages = store.list_messages(&source).unwrap();
    let portable = messages
        .iter()
        .map(|m| (m.role.clone(), m.content.clone(), m.meta.clone()))
        .collect::<Vec<_>>();
    let imported = store
        .import_session("imported", None, None, &portable, &[])
        .unwrap();
    store
        .import_session_plan_bundle(
            &imported.id,
            &bundle,
            &messages.iter().map(|m| m.seq).collect::<Vec<_>>(),
        )
        .unwrap();
    let state = store.session_plan_state(&imported.id).unwrap().unwrap();
    assert_eq!(state.origin, "imported");
    assert!(state.run_id.is_none() && state.updated_at.is_none());
    assert_eq!(
        store.session_plan(&imported.id).unwrap(),
        vec![item("latest plan")]
    );
    let first = store
        .fork_session(&imported.id, None, Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        store.session_plan(&first.id).unwrap(),
        vec![item("first plan")]
    );
    let clear = store
        .fork_session(&imported.id, None, Some(2))
        .unwrap()
        .unwrap();
    assert!(store.session_plan(&clear.id).unwrap().is_empty());
    let local_run = store.create_run(&imported.id).unwrap();
    assert!(
        store
            .import_session_plan_bundle(&imported.id, &bundle, &[1, 3, 5])
            .is_err(),
        "portable state cannot stomp a locally owned Session"
    );
    assert!(store
        .set_run_plan(&imported.id, &run, &[item("foreign write")])
        .is_err());
    store
        .set_run_plan(&imported.id, &local_run, &[item("local write")])
        .unwrap();
}

#[test]
fn history_only_import_is_rejected_without_hidden_revisions_or_partial_session_import() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(None).unwrap().id;
    store
        .append_message(&session, "user", "retained original")
        .unwrap();
    let history_only = json!({"plan_history":[{"anchor_seq":1,"items":[{"content":"hidden step","status":"pending"}]}]});
    let before = store.session_plan_bundle(&session).unwrap();
    let error = store
        .import_session_plan_bundle(&session, &history_only, &[1])
        .unwrap_err();
    assert!(error.to_string().contains("history-only"));
    assert_eq!(store.session_plan_bundle(&session).unwrap(), before);
    assert_eq!(store.list_messages(&session).unwrap().len(), 1);
    let fork = store.fork_session(&session, None, None).unwrap().unwrap();
    assert!(
        store.session_plan(&fork.id).unwrap().is_empty(),
        "a rejected import cannot resurrect a hidden checklist through a fork"
    );
    let sessions_before: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    let result: Result<()> = store.transaction(|st| {
        let imported = st.import_session(
            "must roll back",
            None,
            None,
            &[("user".into(), "portable message".into(), None)],
            &[],
        )?;
        st.import_session_plan_bundle(&imported.id, &history_only, &[1])?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        sessions_before
    );
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM session_plan_revisions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(store.session_plan_bundle(&session).unwrap(), before);
}
