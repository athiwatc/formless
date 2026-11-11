//! Scheduler failure states use only private stores and no provider requests.
use super::*;

struct Fixture {
    daemon: Option<Arc<Daemon>>,
    root: std::path::PathBuf,
    events: mpsc::Receiver<ServerMessage>,
}

impl Fixture {
    fn new(env: &crate::credential_env::EnvGuard) -> Self {
        env.set("FORMLESS_BASE_URL", "http://127.0.0.1:9");
        env.set("FORMLESS_MODEL", "fixture-model");
        env.set("FORMLESS_API_KEY", "fixture-key");
        let root = std::env::temp_dir().join(format!("fl-job-state-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.naming.enabled = false;
        cfg.provider.max_retries = 0;
        cfg.provider.fallback_models.clear();
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let (tx, events) = mpsc::channel(128);
        daemon.subs.locked().push(Subscriber {
            conn: 948,
            tx: tx.into(),
            filter: Arc::new(StdMutex::new(EventFilter::default())),
            kick: CancellationToken::new(),
            gaps: Default::default(),
        });
        Self {
            daemon: Some(daemon),
            root,
            events,
        }
    }

    fn daemon(&self) -> &Arc<Daemon> {
        self.daemon
            .as_ref()
            .expect("fixture daemon is present until drop")
    }

    fn session(&self) -> String {
        self.daemon()
            .with_store(|st| Ok(st.create_session(None)?.id))
            .unwrap()
    }

    fn due(&self, target: Option<&str>, cron: Option<&str>) -> JobInfo {
        self.daemon()
            .with_store(|st| {
                st.create_job(
                    "fixture prompt",
                    target,
                    cron,
                    &(chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
                    true,
                    "user",
                )
            })
            .unwrap()
    }

    fn job(&self, id: &str) -> JobInfo {
        self.daemon()
            .with_store(|st| st.get_job(id))
            .unwrap()
            .unwrap()
    }

    fn assert_paused(&mut self, job: &JobInfo, action: &str, reason: &str) {
        let after = self.job(&job.id);
        assert!(!after.enabled);
        assert!(after.last_run.is_some());
        assert_eq!(
            after.next_run, job.next_run,
            "failure must retain the occurrence"
        );
        assert_eq!(after.session_id, job.session_id);
        assert!(after.last_status.starts_with("paused: "), "{after:?}");
        assert!(after.last_status.contains(reason), "{after:?}");
        let runs = self
            .daemon()
            .with_store(|st| st.job_runs(&job.id, 20))
            .unwrap();
        assert!(!runs.is_empty());
        assert!(runs
            .iter()
            .all(|r| r.status == "failed" && r.finished_at.is_some()));
        assert!(self
            .daemon()
            .with_store(|st| st.recent_audit(Some(action), 20))
            .unwrap()
            .iter()
            .any(|(_, a, detail, _)| a == action
                && detail.contains(&job.id)
                && detail.contains(reason)));
        let mut note = false;
        let mut changed = false;
        while let Ok(message) = self.events.try_recv() {
            match message {
                ServerMessage::Event(Event::Note {
                    session_id: None,
                    text,
                }) => {
                    note |= text.contains(&job.id) && text.contains(reason);
                }
                ServerMessage::Event(Event::JobsChanged) => changed = true,
                _ => {}
            }
        }
        assert!(
            note && changed,
            "scheduler failure must be visible to connected clients"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(
            Arc::strong_count(self.daemon()),
            1,
            "fixture spawned unexpected async work"
        );
        // Release the sole daemon (DB, readers, profile lock) before removal.
        drop(self.daemon.take());
        std::fs::remove_dir_all(&self.root)
            .expect("remove job-state fixture root after daemon shutdown");
    }
}

#[tokio::test]
async fn job_state_creation_checks_both_entry_points_and_current_target() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let current = f.session();
    let soft = f.session();
    let hard = f.session();
    f.daemon()
        .with_store(|st| {
            st.soft_delete_session(&soft)?;
            st.delete_session(&hard)?;
            Ok(())
        })
        .unwrap();
    for target in ["missing", "", &soft, &hard] {
        let error = f
            .daemon()
            .handle(Request::AddJob {
                prompt: "fixture".into(),
                session_id: Some(target.into()),
                cron: None,
                at: Some("in 1h".into()),
                overlap: None,
                deliver: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("job target Session"), "{error}");
        // schedule_run historically treats an empty target as unpinned.
        if !target.is_empty() {
            let error = f
                .daemon()
                .call_tool_direct(
                    &current,
                    "schedule_run",
                    json!({"prompt":"fixture", "at":"in 1h", "session_id":target}),
                )
                .await;
            assert!(error.contains("job target Session"), "{error}");
        }
        assert!(f
            .daemon()
            .with_store(|st| st.list_jobs())
            .unwrap()
            .is_empty());
    }
    let out = f
        .daemon()
        .call_tool_direct(
            &current,
            "schedule_run",
            json!({"prompt":"fixture", "at":"in 1h", "session_id":"current"}),
        )
        .await;
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        f.job(out["id"].as_str().unwrap()).session_id.as_deref(),
        Some(current.as_str())
    );
    let out = f
        .daemon()
        .handle(Request::AddJob {
            prompt: "fixture".into(),
            session_id: Some(current.clone()),
            cron: None,
            at: Some("in 1h".into()),
            overlap: None,
            deliver: None,
        })
        .await
        .unwrap();
    assert_eq!(out["session_id"], current);
    f.daemon()
        .with_store(|st| {
            st.soft_delete_session(&current)?;
            Ok(())
        })
        .unwrap();
    let error = f
        .daemon()
        .call_tool_direct(
            &current,
            "schedule_run",
            json!({"prompt":"fixture", "at":"in 1h", "session_id":"current"}),
        )
        .await;
    assert!(error.contains("job target Session"), "{error}");
    assert_eq!(f.daemon().with_store(|st| st.list_jobs()).unwrap().len(), 2);
}

#[tokio::test]
async fn job_state_deleted_target_never_falls_back_on_repeated_scheduled_or_manual_fire() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env);
    for soft in [false, true] {
        let sid = f.session();
        let job = f.due(Some(&sid), Some("* * * * *"));
        f.daemon()
            .with_store(|st| {
                if soft {
                    st.soft_delete_session(&sid)?;
                } else {
                    st.delete_session(&sid)?;
                }
                Ok(())
            })
            .unwrap();
        let before = f
            .daemon()
            .with_store(|st| st.list_sessions())
            .unwrap()
            .len();
        f.daemon().run_due_jobs().await;
        f.daemon().run_due_jobs().await;
        assert_eq!(
            f.daemon()
                .with_store(|st| st.job_runs(&job.id, 20))
                .unwrap()
                .len(),
            1,
            "second scheduler pass must leave a paused job alone"
        );
        f.assert_paused(&job, "job.target_missing", &sid);
        for _ in 0..2 {
            let out = f
                .daemon()
                .handle(Request::RunJobNow { id: job.id.clone() })
                .await
                .unwrap();
            assert!(
                out["status"].as_str().unwrap().starts_with("paused: "),
                "{out}"
            );
        }
        assert_eq!(
            f.daemon()
                .with_store(|st| st.list_sessions())
                .unwrap()
                .len(),
            before
        );
        f.assert_paused(&job, "job.target_missing", &sid);
    }
}

#[tokio::test]
async fn job_state_bad_or_uncomputable_recurrence_is_flagged_before_delivery() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env);
    for cron in ["invalid old cron", "0 0 31 2 *"] {
        let job = f.due(None, Some(cron));
        let before = f
            .daemon()
            .with_store(|st| st.list_sessions())
            .unwrap()
            .len();
        f.daemon().run_due_jobs().await;
        f.daemon().run_due_jobs().await;
        assert_eq!(
            f.daemon()
                .with_store(|st| st.list_sessions())
                .unwrap()
                .len(),
            before
        );
        f.assert_paused(&job, "job.reschedule_failed", "could not be rescheduled");
        assert_eq!(
            f.daemon()
                .with_store(|st| st.job_runs(&job.id, 20))
                .unwrap()
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn job_state_delivery_failure_retains_one_shot_and_recurring_occurrences() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env);
    let sid = f.session();
    let once = f.due(Some(&sid), None);
    let recurring = f.due(Some(&sid), Some("* * * * *"));
    // Private malformed config causes send admission to fail before provider IO.
    std::fs::write(
        f.daemon().paths.config_file(),
        b"[ intentionally malformed private fixture",
    )
    .unwrap();
    f.daemon().run_due_jobs().await;
    f.daemon().run_due_jobs().await;
    for job in [&once, &recurring] {
        // Drain per-job events independently by checking all notes below.
        let after = f.job(&job.id);
        assert!(!after.enabled);
        assert_eq!(after.next_run, job.next_run);
        assert!(
            after.last_status.contains("paused: prompt delivery failed"),
            "{after:?}"
        );
        let runs = f
            .daemon()
            .with_store(|st| st.job_runs(&job.id, 20))
            .unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "failed");
        assert!(runs[0].finished_at.is_some());
        assert!(f
            .daemon()
            .with_store(|st| st.recent_audit(Some("job.delivery_failed"), 10))
            .unwrap()
            .iter()
            .any(|(_, _, detail, _)| detail.contains(&job.id)));
    }
    let mut noted = std::collections::HashSet::new();
    while let Ok(message) = f.events.try_recv() {
        if let ServerMessage::Event(Event::Note { text, .. }) = message {
            for job in [&once, &recurring] {
                if text.contains(&job.id) && text.contains("delivery failed") {
                    noted.insert(job.id.clone());
                }
            }
        }
    }
    assert_eq!(noted.len(), 2);
    assert!(f
        .daemon()
        .with_store(|st| st.list_messages(&sid))
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn job_state_late_completion_cannot_erase_pause_but_normal_or_resumed_jobs_update() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let sid = f.session();
    let job = f.due(Some(&sid), Some("* * * * *"));
    let row = f
        .daemon()
        .with_store(|st| st.job_run_start(&job.id, Some(&sid), "schedule", "running", ""))
        .unwrap();
    f.daemon()
        .job_pending
        .locked()
        .insert(sid.clone(), (row.clone(), job.id.clone(), 0));
    let status = f.daemon().pause_job(
        &job,
        "job.target_missing",
        "target vanished",
        Some("schedule"),
    );
    f.daemon()
        .job_run_ended(&sid, "fixture-no-provider-run", "completed");
    assert_eq!(f.job(&job.id).last_status, status);
    assert!(f
        .daemon()
        .with_store(|st| st.job_runs(&job.id, 20))
        .unwrap()
        .iter()
        .any(|r| r.id == row && r.status == "completed"));
    f.daemon()
        .with_store(|st| {
            st.set_job_enabled(&job.id, true)?;
            st.job_ran_status(&job.id, "completed")
        })
        .unwrap();
    assert_eq!(f.job(&job.id).last_status, "completed");
    let once = f.due(None, None);
    f.daemon()
        .with_store(|st| {
            st.job_ran(&once.id, "started", None)?;
            st.job_ran_status(&once.id, "completed")
        })
        .unwrap();
    assert!(!f.job(&once.id).enabled);
    assert_eq!(f.job(&once.id).last_status, "completed");
}
