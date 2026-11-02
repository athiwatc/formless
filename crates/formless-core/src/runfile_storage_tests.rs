use super::*;

fn fixture() -> (Store, String, String) {
    let st = Store::open_in_memory().unwrap();
    let session = st.create_session(None).unwrap().id;
    let run = st.create_run(&session).unwrap();
    (st, session, run)
}
fn capture(
    st: &Store,
    session: &str,
    run: &str,
    path: &str,
    body: &[u8],
    cfg: &RunFilesCfg,
) -> RunFileAdmission {
    st.snapshot_run_file_capped(
        session,
        run,
        path,
        true,
        Some(body),
        None,
        "file",
        None,
        cfg,
    )
    .unwrap()
}
fn completed(st: &Store, session: &str, date: &str, body: &[u8]) -> String {
    let run = st.create_run(session).unwrap();
    capture(
        st,
        session,
        &run,
        "private.txt",
        body,
        &RunFilesCfg::default(),
    );
    st.set_run_status(&run, "completed").unwrap();
    st.ex(
        "UPDATE runs SET created_at=?2 WHERE id=?1",
        params![run, date],
    )
    .unwrap();
    run
}
fn no_expiry() -> RunFilesCfg {
    RunFilesCfg {
        keep_runs_per_session: usize::MAX,
        max_age_days: 3_650_000,
        max_total_mb: u64::MAX,
        ..Default::default()
    }
}

#[test]
fn count_byte_and_zero_caps_preserve_paths_and_immutable_first_capture() {
    let (st, s, r) = fixture();
    let cfg = RunFilesCfg {
        max_files_per_run: 2,
        max_bytes_per_run: 5,
        ..Default::default()
    };
    let first = st
        .snapshot_run_file_capped(
            &s,
            &r,
            "a",
            true,
            Some(b"abc"),
            Some("de"),
            "file",
            None,
            &cfg,
        )
        .unwrap();
    assert!(first.inserted && first.kept);
    let duplicate = st
        .snapshot_run_file_capped(&s, &r, "a", false, None, None, "missing", None, &cfg)
        .unwrap();
    assert!(!duplicate.inserted && duplicate.kept);
    let bytes = capture(&st, &s, &r, "byte-overflow", b"x", &cfg);
    assert!(!bytes.kept && bytes.reason.as_deref() == Some("Run snapshot byte limit reached"));
    let creation = st
        .snapshot_run_file_capped(
            &s,
            &r,
            "created-dir",
            false,
            None,
            None,
            "missing",
            None,
            &cfg,
        )
        .unwrap();
    assert!(
        creation.kept,
        "a zero-byte creation fits exactly at the byte boundary"
    );
    let count = st
        .snapshot_run_file_capped(
            &s,
            &r,
            "count-overflow",
            false,
            None,
            None,
            "missing",
            None,
            &cfg,
        )
        .unwrap();
    assert!(
        !count.kept && count.reason.as_deref() == Some("Run snapshot file count limit reached")
    );
    let rows = st.run_file_records(&s, &r).unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        st.run_file_record(&s, &r, "a").unwrap().before.as_deref(),
        Some(b"abc".as_slice())
    );
    assert_eq!(
        st.run_file_record(&s, &r, "a")
            .unwrap()
            .before_metadata
            .as_deref(),
        Some("de")
    );
    assert_eq!((rows[1].before_bytes, rows[1].metadata_bytes), (0, 0));
    assert_eq!(st.run_file_usage().unwrap().retained_bytes, 5);
    for cfg in [
        RunFilesCfg {
            max_files_per_run: 0,
            ..Default::default()
        },
        RunFilesCfg {
            max_bytes_per_run: 0,
            ..Default::default()
        },
    ] {
        let run = st.create_run(&s).unwrap();
        let admission = st
            .snapshot_run_file_capped(
                &s,
                &run,
                "empty-creation",
                false,
                None,
                None,
                "missing",
                None,
                &cfg,
            )
            .unwrap();
        assert!(admission.inserted && !admission.kept && admission.reason.is_some());
        assert_eq!(st.run_file_records(&s, &run).unwrap().len(), 1);
    }
}

#[test]
fn owned_apis_reject_foreign_deleted_and_mismatched_rows_without_mutation() {
    let (st, s, r) = fixture();
    let foreign = st.create_session(None).unwrap().id;
    capture(&st, &s, &r, "a", b"before", &RunFilesCfg::default());
    assert!(st.run_file_records(&foreign, &r).is_err());
    assert!(st
        .snapshot_run_file_capped(
            &foreign,
            &r,
            "evil",
            false,
            None,
            None,
            "missing",
            None,
            &RunFilesCfg::default()
        )
        .is_err());
    assert!(st
        .set_run_file_after_state(&foreign, &r, "a", r#"{"version":1}"#)
        .is_err());
    assert!(st.mark_run_untracked_writes(&foreign, &r).is_err());
    assert!(!st.run_untracked_writes_possible(&s, &r).unwrap());
    assert_eq!(st.run_file_usage().unwrap().rows, 1);
    st.ex(
        "UPDATE run_files SET session_id=?2 WHERE run_id=?1",
        params![r, foreign],
    )
    .unwrap();
    assert!(st.run_file_records(&s, &r).is_err());
    assert!(st.mark_run_untracked_writes(&s, &r).is_err());
    st.ex(
        "UPDATE run_files SET session_id=?2 WHERE run_id=?1",
        params![r, s],
    )
    .unwrap();
    st.soft_delete_session(&s).unwrap();
    assert!(st.run_file_records(&s, &r).is_err());
    st.restore_session(&s).unwrap();
    assert_eq!(st.run_file_records(&s, &r).unwrap().len(), 1);
}

#[test]
fn final_tokens_are_first_write_only_bounded_and_never_backfill_legacy() {
    let (st, s, r) = fixture();
    st.snapshot_run_file(&s, &r, "legacy", true, Some(b"before"))
        .unwrap();
    assert!(!st
        .set_run_file_after_state(&s, &r, "legacy", r#"{"version":1}"#)
        .unwrap());
    capture(&st, &s, &r, "typed", b"before", &RunFilesCfg::default());
    let token = r#"{"version":1,"kind":"file","sha256":"private","size":6}"#;
    assert!(st.set_run_file_after_state(&s, &r, "typed", token).unwrap());
    assert!(!st
        .set_run_file_after_state(&s, &r, "typed", r#"{"version":2}"#)
        .unwrap());
    assert!(st.set_run_file_after_state(&s, &r, "typed", "[]").is_err());
    assert!(st
        .set_run_file_after_state(&s, &r, "typed", &json!({"x":"x".repeat(8192)}).to_string())
        .is_err());
    assert_eq!(
        st.run_file_records(&s, &r).unwrap()[1]
            .after_state
            .as_deref(),
        Some(token)
    );
}

#[test]
fn count_retention_keeps_paths_and_preview_matches_apply_then_is_idempotent() {
    let (st, s, unused) = fixture();
    st.set_run_status(&unused, "completed").unwrap();
    let a = completed(&st, &s, "2026-01-01T00:00:00Z", b"aaa");
    let b = completed(&st, &s, "2026-01-02T00:00:00Z", b"bbbb");
    let c = completed(&st, &s, "2026-01-03T00:00:00Z", b"ccccc");
    let cfg = RunFilesCfg {
        keep_runs_per_session: 2,
        ..no_expiry()
    };
    let preview = st.retain_run_files(&cfg, true).unwrap();
    assert_eq!((preview.runs, preview.files, preview.bytes), (1, 1, 3));
    assert_eq!(st.run_file_usage().unwrap().before_bytes, 12);
    assert!(st.run_file_records(&s, &a).unwrap()[0].kept);
    let applied = st.retain_run_files(&cfg, false).unwrap();
    assert_eq!(
        serde_json::to_value(preview).unwrap(),
        serde_json::to_value(applied).unwrap()
    );
    let expired = st.run_file_records(&s, &a).unwrap();
    assert_eq!(expired.len(), 1);
    assert!(!expired[0].kept && expired[0].before_bytes == 0 && expired[0].metadata_bytes == 0);
    assert_eq!(expired[0].not_kept_reason.as_deref(), Some(EXPIRED_REASON));
    assert!(st.run_file_records(&s, &b).unwrap()[0].kept);
    assert_eq!(st.last_run_with_files(&s).unwrap(), Some(c));
    assert_eq!(st.run_file_usage().unwrap().rows, 3);
    assert_eq!(st.retain_run_files(&cfg, false).unwrap().bytes, 0);
}

#[test]
fn age_and_total_retention_each_work_independently_and_pin_active_latest() {
    let (st, s, unused) = fixture();
    st.set_run_status(&unused, "completed").unwrap();
    let old = completed(&st, &s, "2000-01-01T00:00:00Z", b"old");
    let recent = completed(&st, &s, &chrono::Utc::now().to_rfc3339(), b"latest");
    let age = st
        .retain_run_files(
            &RunFilesCfg {
                max_age_days: 30,
                ..no_expiry()
            },
            false,
        )
        .unwrap();
    assert_eq!((age.runs, age.bytes), (1, 3));
    assert_eq!(
        st.run_file_records(&s, &old).unwrap()[0]
            .not_kept_reason
            .as_deref(),
        Some(EXPIRED_REASON)
    );
    assert!(st.run_file_records(&s, &recent).unwrap()[0].kept);

    let (st, s, unused) = fixture();
    st.set_run_status(&unused, "completed").unwrap();
    let a = completed(&st, &s, "2026-01-01T00:00:00Z", &vec![b'a'; 600_000]);
    let b = completed(&st, &s, "2026-01-02T00:00:00Z", &vec![b'b'; 600_000]);
    let c = completed(&st, &s, "2026-01-03T00:00:00Z", b"latest");
    let total = st
        .retain_run_files(
            &RunFilesCfg {
                max_total_mb: 1,
                ..no_expiry()
            },
            false,
        )
        .unwrap();
    assert_eq!((total.runs, total.bytes), (1, 600_000));
    assert!(!st.run_file_records(&s, &a).unwrap()[0].kept);
    assert!(st.run_file_records(&s, &b).unwrap()[0].kept);
    st.set_run_status(&b, "running").unwrap();
    let zero = st
        .retain_run_files(
            &RunFilesCfg {
                keep_runs_per_session: 0,
                max_age_days: 0,
                max_total_mb: 0,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!(zero.bytes, 0);
    assert_eq!(
        (
            zero.pinned_bytes,
            zero.remaining_bytes,
            zero.over_budget_bytes
        ),
        (600_006, 600_006, 600_006)
    );
    assert!(st.run_file_records(&s, &c).unwrap()[0].kept);
}

#[test]
fn coverage_only_and_rewound_runs_remain_latest_across_reopen() {
    let root = std::env::temp_dir().join(format!("fl-runfiles-history-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("private.db");
    let (session, latest) = {
        let st = Store::open(&file).unwrap();
        let s = st.create_session(None).unwrap().id;
        completed(&st, &s, "2026-01-01T00:00:00Z", b"old");
        let r = completed(&st, &s, "2026-01-02T00:00:00Z", b"latest");
        st.forget_run_file(&r, "private.txt").unwrap();
        assert_eq!(st.last_run_with_files(&s).unwrap(), Some(r.clone()));
        assert_eq!(
            st.run_file_records(&s, &r).unwrap()[0]
                .not_kept_reason
                .as_deref(),
            Some("Already rewound")
        );
        let coverage = st.create_run(&s).unwrap();
        st.mark_run_untracked_writes(&s, &coverage).unwrap();
        st.mark_run_untracked_writes(&s, &coverage).unwrap();
        st.set_run_status(&coverage, "completed").unwrap();
        (s, coverage)
    };
    let st = Store::open(&file).unwrap();
    assert_eq!(
        st.last_run_with_files(&session).unwrap(),
        Some(latest.clone())
    );
    assert!(st.run_untracked_writes_possible(&session, &latest).unwrap());
    assert!(st.run_file_records(&session, &latest).unwrap().is_empty());
    drop(st);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn hard_delete_and_expired_soft_delete_remove_rows_but_undo_preserves_them() {
    let (st, s, r) = fixture();
    let other = st.create_session(None).unwrap().id;
    let other_run = st.create_run(&other).unwrap();
    capture(&st, &s, &r, "a", b"original", &RunFilesCfg::default());
    capture(
        &st,
        &other,
        &other_run,
        "b",
        b"unrelated",
        &RunFilesCfg::default(),
    );
    st.soft_delete_session(&s).unwrap();
    assert_eq!(st.run_file_usage().unwrap().rows, 2);
    st.restore_session(&s).unwrap();
    assert!(st.run_file_records(&s, &r).unwrap()[0].kept);
    st.soft_delete_session(&s).unwrap();
    st.ex(
        "UPDATE sessions SET deleted_at='2000-01-01T00:00:00Z' WHERE id=?1",
        params![s],
    )
    .unwrap();
    for id in st.sessions_deleted_before("2001-01-01T00:00:00Z").unwrap() {
        st.delete_session(&id).unwrap();
    }
    assert_eq!(st.run_file_usage().unwrap().rows, 1);
    assert!(st.run_file_records(&other, &other_run).unwrap()[0].kept);
    st.delete_session(&other).unwrap();
    assert_eq!(st.run_file_usage().unwrap().rows, 0);
    // Clean up orphan rows even if the Session itself was already lost.
    st.snapshot_run_file(
        "orphan-session",
        "orphan-run",
        "orphan",
        true,
        Some(b"orphan"),
    )
    .unwrap();
    assert!(!st.delete_session("orphan-session").unwrap());
    assert_eq!(st.run_file_usage().unwrap().before_bytes, 0);
}

#[test]
fn snapshot_usage_counts_metadata_as_bytes_and_stays_truthful_after_expiry() {
    let (st, s, r) = fixture();
    st.snapshot_run_file_capped(
        &s,
        &r,
        "private",
        true,
        Some(b"abc"),
        Some("日"),
        "file",
        None,
        &RunFilesCfg::default(),
    )
    .unwrap();
    let token = r#"{"version":1}"#;
    st.set_run_file_after_state(&s, &r, "private", token)
        .unwrap();
    let usage = st.run_file_usage().unwrap();
    assert_eq!(
        (
            usage.rows,
            usage.retained_files,
            usage.before_bytes,
            usage.metadata_bytes,
            usage.retained_bytes,
            usage.after_state_bytes
        ),
        (1, 1, 3, 3, 6, token.len() as u64)
    );
    st.set_run_status(&r, "completed").unwrap();
    completed(&st, &s, "9999-01-01T00:00:00Z", b"new");
    st.retain_run_files(
        &RunFilesCfg {
            keep_runs_per_session: 0,
            ..no_expiry()
        },
        false,
    )
    .unwrap();
    let usage = st.run_file_usage().unwrap();
    assert_eq!(
        (
            usage.rows,
            usage.retained_files,
            usage.before_bytes,
            usage.metadata_bytes,
            usage.after_state_bytes
        ),
        (2, 1, 3, 0, token.len() as u64)
    );
}

#[test]
fn genuine_schema65_upgrade_and_backup_preserve_originals_and_immutable_replay_proof() {
    use sha2::{Digest, Sha256};
    let root = std::env::temp_dir().join(format!("fl-runfiles-v65-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("private.db");
    let historical = root.join("historical65.db");
    let original = b"before\r\n\xe6\x97\xa5\n";
    let before_metadata = "legacy descriptor metadata must stay exact";
    let snapshot = ReplaySnapshot {
        id: "proof".into(),
        digest: String::new(),
        tool_id: "tool".into(),
        base_version: 1,
        generation: vec![ReplayItem {
            input: r#"{"x":-1}"#.into(),
            error: "private failure".into(),
            tag: String::new(),
            recorded_success: false,
        }],
        holdout: vec![ReplayItem {
            input: r#"{"x":1}"#.into(),
            error: String::new(),
            tag: String::new(),
            recorded_success: true,
        }],
        source_count: 2,
        excluded_count: 0,
        created_at: "2026-01-01T00:00:00Z".into(),
        provenance: json!({"format":"optimiser-replay-v1","selection":"recent_terminal_outcome_independent"}),
    };
    let manifest = serde_json::to_string(&snapshot).unwrap();
    let digest = format!("{:x}", Sha256::digest(manifest.as_bytes()));
    let old = schema_at(65).unwrap();
    assert_eq!(old.query_row("SELECT COUNT(*) FROM pragma_table_info('run_files') WHERE name IN ('before_kind','after_state','not_kept_reason')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    old.execute_batch("INSERT INTO sessions(id,name,created_at,updated_at) VALUES('session','private fixture','2026-01-01','2026-01-01');
        INSERT INTO runs(id,session_id,status,created_at,updated_at) VALUES('run','session','completed','2026-01-01','2026-01-01');
        INSERT INTO tools(id,name,name_lower,current_version,created_at,updated_at) VALUES('tool','private_tool','private_tool',1,'2026-01-01','2026-01-01');
        INSERT INTO tool_versions(id,tool_id,version,input_schema,output_schema,code,created_at) VALUES('version','tool',1,'{}','{}','function run(i){return i}','2026-01-01');
        INSERT INTO optimiser_attempts(id,tool_id,base_version,status,detail,created_at) VALUES('attempt','tool',1,'awaiting_approval','private fixture','2026-01-01');
        PRAGMA user_version=65;").unwrap();
    old.execute("INSERT INTO run_files(run_id,session_id,path,existed,kept,before,created_at,before_metadata) VALUES('run','session','private.txt',1,1,?1,'2026-01-01',?2)",params![original.as_slice(),before_metadata]).unwrap();
    old.execute("INSERT INTO optimiser_replay_snapshots(id,tool_id,base_version,created_at,manifest,digest) VALUES('proof','tool',1,'2026-01-01',?1,?2)",params![manifest,digest]).unwrap();
    old.execute(
        "INSERT INTO optimiser_attempt_replay(attempt_id,snapshot_id) VALUES('attempt','proof')",
        [],
    )
    .unwrap();
    old.execute("VACUUM INTO ?1", [historical.to_str().unwrap()])
        .unwrap();
    old.execute("VACUUM INTO ?1", [file.to_str().unwrap()])
        .unwrap();
    drop(old);
    assert_eq!(
        crate::backup_verification::inspect(&historical)
            .unwrap()
            .schema_version,
        65
    );
    {
        let st = Store::open(&file).unwrap();
        assert_eq!(
            st.conn
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            66
        );
        let rows = vec![st.run_file_record("session", "run", "private.txt").unwrap()];
        assert_eq!(rows[0].before.as_deref(), Some(original.as_slice()));
        assert_eq!(rows[0].before_metadata.as_deref(), Some(before_metadata));
        assert!(rows[0].before_kind.is_none() && rows[0].after_state.is_none());
        assert!(!st
            .set_run_file_after_state("session", "run", "private.txt", r#"{"version":1}"#)
            .unwrap());
        assert!(
            !st.snapshot_run_file_capped(
                "session",
                "run",
                "private.txt",
                false,
                None,
                None,
                "missing",
                None,
                &RunFilesCfg::default()
            )
            .unwrap()
            .inserted
        );
        let proof = st.attempt_replay_snapshot("attempt").unwrap().unwrap();
        assert_eq!(proof.digest, digest);
        assert_eq!(proof.holdout[0].input, r#"{"x":1}"#);
        assert!(st
            .ex(
                "UPDATE optimiser_replay_snapshots SET manifest='tamper' WHERE id='proof'",
                []
            )
            .is_err());
        assert!(st.ex("UPDATE optimiser_attempt_replay SET snapshot_id='proof' WHERE attempt_id='attempt'",[]).is_err());
        let backup = newest_backup(&file).unwrap();
        assert_eq!(
            crate::backup_verification::inspect(&backup)
                .unwrap()
                .schema_version,
            65
        );
        let upgraded = root.join("upgraded66.db");
        st.backup_into(&upgraded).unwrap();
        let verified = crate::backup_verification::inspect(&upgraded).unwrap();
        assert_eq!(verified.schema_version, 66);
        assert_eq!(verified.tables["run_files"], 1);
        assert_eq!(verified.tables["optimiser_replay_snapshots"], 1);
    }
    let st = Store::open(&file).unwrap();
    assert_eq!(
        st.run_file_record("session", "run", "private.txt")
            .unwrap()
            .before
            .as_deref(),
        Some(original.as_slice())
    );
    assert_eq!(
        st.attempt_replay_snapshot("attempt")
            .unwrap()
            .unwrap()
            .digest,
        digest
    );
    drop(st);
    // Verify again after reopen: original backup never acquired new columns.
    assert_eq!(
        crate::backup_verification::inspect(&historical)
            .unwrap()
            .schema_version,
        65
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_connections_cannot_overallocate_run_capture_budget() {
    let root = std::env::temp_dir().join(format!("fl-runfiles-race-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("private.db");
    let st = Store::open(&file).unwrap();
    let session = st.create_session(None).unwrap().id;
    let run = st.create_run(&session).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers = (0..2)
        .map(|index| {
            let path = file.clone();
            let s = session.clone();
            let r = run.clone();
            let ready = barrier.clone();
            std::thread::spawn(move || {
                let st = Store::open(&path).unwrap();
                ready.wait();
                capture(
                    &st,
                    &s,
                    &r,
                    &format!("private-{index}"),
                    b"12345",
                    &RunFilesCfg {
                        max_files_per_run: 1,
                        max_bytes_per_run: 5,
                        ..Default::default()
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    let admissions = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(admissions.iter().filter(|a| a.kept).count(), 1);
    assert!(admissions.iter().all(|a| a.inserted));
    let usage = st.run_file_usage().unwrap();
    assert_eq!(
        (usage.rows, usage.retained_files, usage.retained_bytes),
        (2, 1, 5)
    );
    drop(st);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_bulk_listing_reads_only_metadata_and_single_payload_load_is_bounded() {
    let (st, s, r) = fixture();
    // A legitimate historical Run exceeds the new whole-Run budget. Keep all
    // 500 paths without materializing their combined 31.25 MiB of before bytes.
    st.ex(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<500)
        INSERT INTO run_files(run_id,session_id,path,existed,kept,before,created_at)
        SELECT ?1,?2,'legacy-'||i,1,1,zeroblob(65536),'2026-01-01' FROM n",
        params![r, s],
    )
    .unwrap();
    let headers = st.run_file_records(&s, &r).unwrap();
    assert_eq!(headers.len(), 500);
    assert!(headers.iter().all(|header| header.before_bytes == 65536));
    assert_eq!(
        st.run_file_record(&s, &r, "legacy-1")
            .unwrap()
            .before
            .unwrap()
            .len(),
        65536
    );
    st.ex("INSERT INTO run_files(run_id,session_id,path,existed,kept,before,created_at) VALUES(?1,?2,'oversized',1,1,zeroblob(2097153),'2026-01-01')",params![r,s]).unwrap();
    let oversized = st.run_file_record(&s, &r, "oversized").unwrap();
    assert!(!oversized.kept && oversized.before.is_none());
    assert_eq!(
        oversized.not_kept_reason.as_deref(),
        Some("Recorded recovery payload exceeds the per-file read limit")
    );
    assert_eq!(st.run_file_records(&s, &r).unwrap().len(), 501);
}

#[test]
fn latest_active_run_does_not_accidentally_pin_the_previous_completed_run() {
    let (st, s, unused) = fixture();
    st.set_run_status(&unused, "completed").unwrap();
    let old = completed(&st, &s, "2000-01-01T00:00:00Z", b"old");
    let latest = completed(&st, &s, "2000-01-02T00:00:00Z", b"active");
    st.set_run_status(&latest, "running").unwrap();
    let report = st
        .retain_run_files(
            &RunFilesCfg {
                keep_runs_per_session: 0,
                max_age_days: 0,
                max_total_mb: 0,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    assert_eq!((report.runs, report.bytes, report.pinned_bytes), (1, 3, 6));
    assert!(!st.run_file_records(&s, &old).unwrap()[0].kept);
    assert!(st.run_file_records(&s, &latest).unwrap()[0].kept);
}

#[test]
fn recovery_settings_default_for_old_config_and_are_hot_settable_with_zero_limits() {
    let mut cfg: crate::Config = toml::from_str("").unwrap();
    assert_eq!(cfg.run_files.max_files_per_run, 128);
    assert_eq!(cfg.run_files.max_bytes_per_run, 16 * 1024 * 1024);
    assert_eq!(cfg.run_files.keep_runs_per_session, 10);
    assert_eq!(cfg.run_files.max_age_days, 30);
    assert_eq!(cfg.run_files.max_total_mb, 256);
    for key in [
        "run_files.max_files_per_run",
        "run_files.max_bytes_per_run",
        "run_files.keep_runs_per_session",
        "run_files.max_age_days",
        "run_files.max_total_mb",
    ] {
        cfg.set_key(key, json!(0)).unwrap();
        assert!(
            crate::config::setting_docs().contains_key(key),
            "missing docs for {key}"
        );
    }
    let cfg: crate::Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(
        (
            cfg.run_files.max_files_per_run,
            cfg.run_files.max_bytes_per_run,
            cfg.run_files.keep_runs_per_session,
            cfg.run_files.max_age_days,
            cfg.run_files.max_total_mb
        ),
        (0, 0, 0, 0, 0)
    );
    assert!(
        cfg.run_files.max_bytes_per_run == 0,
        "zero remains an explicit zero cap after reload"
    );
}

#[test]
fn existing_gc_and_storage_reports_match_retained_payload_and_paths() {
    let root = std::env::temp_dir().join(format!("fl-runfiles-gc-{}", uuid::Uuid::new_v4()));
    let paths = crate::Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    let (st, s, unused) = fixture();
    st.set_run_status(&unused, "completed").unwrap();
    completed(&st, &s, "2000-01-01T00:00:00Z", b"old");
    completed(&st, &s, "2000-01-02T00:00:00Z", b"latest");
    let mut cfg = crate::Config::default();
    cfg.run_files = RunFilesCfg {
        keep_runs_per_session: 0,
        max_age_days: 0,
        max_total_mb: 0,
        ..Default::default()
    };
    let snapshot_row = |report: &Value| {
        report["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "file snapshots")
            .unwrap()
            .clone()
    };
    let before = snapshot_row(&crate::housekeeping::usage(&paths, &st).unwrap());
    assert_eq!(before["rows"], 2);
    assert_eq!(before["bytes"], 9);
    let preview = snapshot_row(&crate::housekeeping::gc(&paths, &st, &cfg, true).unwrap());
    assert_eq!(preview["items"], 1);
    assert_eq!(preview["bytes"], 3);
    assert_eq!(preview["over_budget_bytes"], 6);
    assert_eq!(st.run_file_usage().unwrap().before_bytes, 9);
    let applied = snapshot_row(&crate::housekeeping::gc(&paths, &st, &cfg, false).unwrap());
    assert_eq!(preview, applied);
    let after = snapshot_row(&crate::housekeeping::usage(&paths, &st).unwrap());
    assert_eq!(after["rows"], 2);
    assert_eq!(after["retained_files"], 1);
    assert_eq!(after["bytes"], 6);
    drop(st);
    std::fs::remove_dir_all(root).unwrap();
}
