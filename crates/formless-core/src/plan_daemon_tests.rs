use super::*;

struct Fixture {
    daemon: Arc<Daemon>,
    root: std::path::PathBuf,
    session: String,
    run: String,
    cfg: Config,
}
impl Fixture {
    fn new(env: &crate::credential_env::EnvGuard) -> Self {
        env.set("FORMLESS_API_KEY", "private-plan-owner-fixture");
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");
        let root = std::env::temp_dir().join(format!("fl-plan-daemon-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = "http://127.0.0.1:9".into();
        cfg.provider.model = "private-plan-model".into();
        cfg.provider
            .model_context_tokens
            .insert(cfg.provider.model.clone(), 100_000);
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let (session, run) = daemon
            .with_store(|store| {
                let session = store.create_session(None)?.id;
                store.append_message(&session, "user", "original task")?;
                let run = store.create_run(&session)?;
                Ok((session, run))
            })
            .unwrap();
        Self::control(&daemon, &session, &run);
        Self {
            daemon,
            root,
            session,
            run,
            cfg,
        }
    }
    fn control(daemon: &Daemon, session: &str, run: &str) {
        let (steer_tx, _) = mpsc::unbounded_channel();
        daemon.runs.locked().insert(
            session.into(),
            RunCtl {
                run_id: run.into(),
                cancel: CancellationToken::new(),
                cancel_flag: Arc::new(AtomicBool::new(false)),
                steer_tx,
                steering: Arc::new(Default::default()),
                retry: Arc::new(provider_retry_daemon::ProviderRetry::default()),
            },
        );
    }
    fn update(&self, text: &str) -> Result<String> {
        self.daemon.update_plan_impl(
            &self.session,
            &self.run,
            0,
            &json!({"items":[{"content":text,"status":"in_progress"}]}),
        )
    }
    fn snapshot(&self) -> (Vec<PlanItem>, Option<formless_protocol::PlanState>) {
        self.daemon
            .with_reader(|store| {
                Ok((
                    store.session_plan(&self.session)?,
                    store.session_plan_state(&self.session)?,
                ))
            })
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn subscribe(daemon: &Daemon) -> mpsc::Receiver<ServerMessage> {
    let (tx, rx) = mpsc::channel(64);
    daemon.subs.locked().push(Subscriber {
        conn: 1,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    rx
}
fn plan_payload(context: &[Value]) -> Value {
    let frame: Value = serde_json::from_str(
        context[0]["content"]
            .as_str()
            .unwrap()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(frame["source_tool"], "session_plan");
    serde_json::from_str(frame["content"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn invalid_subagent_and_stale_plan_writes_leave_the_owned_plan_unchanged() {
    let env = crate::credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env);
    let mut rx = subscribe(&fixture.daemon);
    fixture.update("owned step").unwrap();
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Event(Event::PlanUpdated {
            plan_state: Some(_),
            ..
        })
    ));
    let before = fixture.snapshot();
    let multiple = json!({"items":[{"content":"one","status":"in_progress"},{"content":"two","status":"in_progress"}]});
    assert!(fixture
        .daemon
        .update_plan_impl(&fixture.session, &fixture.run, 0, &multiple)
        .unwrap_err()
        .to_string()
        .contains("at most one"));
    let valid = json!({"items":[{"content":"sub-agent suggestion","status":"pending"}]});
    assert!(fixture
        .daemon
        .update_plan_impl(&fixture.session, &fixture.run, 1, &valid)
        .unwrap_err()
        .to_string()
        .contains("main agent"));
    assert_eq!(fixture.snapshot(), before);
    assert!(
        rx.try_recv().is_err(),
        "rejected writes cannot publish a replacement plan"
    );
    fixture
        .daemon
        .runs
        .locked()
        .get(&fixture.session)
        .unwrap()
        .cancel_flag
        .store(true, Ordering::SeqCst);
    assert!(fixture.update("cancelled main step").is_err());
    assert_eq!(fixture.snapshot(), before);
    fixture
        .daemon
        .runs
        .locked()
        .get(&fixture.session)
        .unwrap()
        .cancel_flag
        .store(false, Ordering::SeqCst);
    fixture
        .daemon
        .with_store(|store| store.create_run(&fixture.session))
        .unwrap();
    assert!(fixture.update("stale background step").is_err());
    assert_eq!(fixture.snapshot().0, before.0);
    assert!(rx.try_recv().is_err());
    fixture.daemon.runs.locked().clear();
    assert!(fixture
        .update("no active main agent")
        .unwrap_err()
        .to_string()
        .contains("current active"));
}

#[tokio::test]
async fn terminal_plan_state_is_published_without_rewriting_item_progress() {
    let env = crate::credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env);
    fixture.update("unfinished step").unwrap();
    let mut rx = subscribe(&fixture.daemon);
    for status in ["paused", "interrupted", "failed"] {
        fixture
            .daemon
            .record_run_status(&fixture.session, &fixture.run, status);
        let event = rx
            .try_recv()
            .expect("durable terminal status must publish plan lifecycle");
        match event {
            ServerMessage::Event(Event::PlanUpdated {
                items,
                plan_state: Some(state),
                ..
            }) => {
                assert_eq!(items[0].status, "in_progress");
                assert_eq!(state.run_status.as_deref(), Some(status));
                assert!(!state.previous_task);
            }
            other => panic!("unexpected lifecycle {other:?}"),
        }
    }
}

#[tokio::test]
async fn unrelated_admission_and_resume_freeze_the_quoted_plan_snapshot_then_publish_handoff() {
    let env = crate::credential_env::EnvGuard::isolated();
    for resume in [false, true] {
        let fixture = Fixture::new(&env);
        fixture.update("recorded checklist step").unwrap();
        fixture.daemon.record_run_status(
            &fixture.session,
            &fixture.run,
            if resume { "paused" } else { "completed" },
        );
        fixture.daemon.runs.locked().clear();
        let input = ForecastInput {
            session_id: Some(fixture.session.clone()),
            draft: if resume {
                String::new()
            } else {
                "unrelated new task".into()
            },
            resume,
            ..Default::default()
        };
        let quote = fixture
            .daemon
            .with_reader(|store| crate::forecast::estimate_store(store, &fixture.cfg, &input))
            .unwrap();
        let (_, new_run, admission) = fixture
            .daemon
            .admit_action(&fixture.session, &fixture.cfg, &input, |store| {
                if resume {
                    store.set_run_status(&fixture.run, "resumed")?;
                    store.append_message(
                        &fixture.session,
                        "system-note",
                        &crate::forecast::resume_note("paused"),
                    )?;
                } else {
                    store.append_message(&fixture.session, "user", "unrelated new task")?;
                }
                Ok(())
            })
            .unwrap();
        assert_ne!(new_run, fixture.run);
        let first = admission.first.locked();
        let (context, defs, _, _, _) = first.as_ref().unwrap();
        let payload = plan_payload(context);
        assert_eq!(payload["state"]["run_id"], fixture.run);
        assert_eq!(payload["state"]["run_status"],if resume{"resumed"}else{"completed"},"the frozen snapshot must explicitly describe its recorded owner, not an active new-task checklist");
        assert!(context[0]["content"]
            .as_str()
            .unwrap()
            .contains("resume only when relevant"));
        let provider = Provider::with_key(
            fixture.cfg.provider.clone(),
            String::new(),
            fixture.daemon.http.clone(),
        )
        .resolved_dispatch(&fixture.cfg.provider.model);
        let ratio = fixture
            .daemon
            .with_reader(|store| store.token_ratio(&quote.basis.provider_key, &quote.basis.model))
            .unwrap();
        assert_eq!(
            crate::tokens::calibrated(
                provider.prompt_tokens(context, defs, &fixture.cfg.provider.model),
                ratio
            ),
            quote.basis.prompt_tokens,
            "plan metadata must not drift between quoted and frozen first request bodies"
        );
        drop(first);
        let mut rx = subscribe(&fixture.daemon);
        fixture.daemon.publish_session_plan(&fixture.session);
        match rx.try_recv().unwrap() {
            ServerMessage::Event(Event::PlanUpdated {
                items,
                plan_state: Some(state),
                ..
            }) => {
                assert_eq!(items[0].content, "recorded checklist step");
                assert_eq!(state.previous_task, !resume);
                if resume {
                    assert_eq!(state.run_id.as_deref(), Some(new_run.as_str()));
                    assert_eq!(state.origin, "resumed");
                    assert_eq!(state.run_status.as_deref(), Some("running"));
                } else {
                    assert_eq!(state.run_id.as_deref(), Some(fixture.run.as_str()));
                }
            }
            other => panic!("unexpected admitted plan {other:?}"),
        }
    }
}

#[tokio::test]
async fn steering_ack_names_its_actual_run_and_queue_ack_does_not_guess_a_future_run() {
    let env = crate::credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env);
    let steered = fixture
        .daemon
        .send_message(
            fixture.session.clone(),
            "steering detail".into(),
            SendMode::Steer,
        )
        .await
        .unwrap();
    assert_eq!(steered["steered"], true);
    assert_eq!(steered["run_id"], fixture.run);
    let queued = fixture
        .daemon
        .send_message(
            fixture.session.clone(),
            "future task".into(),
            SendMode::Queue,
        )
        .await
        .unwrap();
    assert_eq!(queued["queued"], true);
    assert!(queued.get("run_id").is_none());
    assert_eq!(
        fixture
            .daemon
            .with_reader(|store| store.last_run(&fixture.session))
            .unwrap()
            .unwrap()
            .0,
        fixture.run
    );
}

fn completed_recap(fixture: &Fixture, status: &str) -> RunRecap {
    fixture
        .daemon
        .with_store(|store| {
            for _ in 0..4 {
                store.bump_run_steps(&fixture.run)?;
            }
            store.set_run_status(&fixture.run, status)
        })
        .unwrap();
    RunRecap {
        status: status.into(),
        steps: 4,
        duration_ms: 0,
        total_calls: Some(0),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_snapshot_cannot_sample_idle_after_reading_before_recap_persistence() {
    let env = crate::credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env);
    let recap = completed_recap(&fixture, "completed");
    let (read_tx, read_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let release_rx = Arc::new(StdMutex::new(release_rx));
    *fixture.daemon.session_snapshot_test.locked() = Some(Arc::new(move || {
        read_tx.send(()).unwrap();
        release_rx.locked().recv().unwrap();
    }));
    let runtime = tokio::runtime::Handle::current();
    let getter = std::thread::spawn({
        let daemon = fixture.daemon.clone();
        let session = fixture.session.clone();
        move || {
            runtime
                .block_on(daemon.handle(Request::GetSession { id: session }))
                .unwrap()
        }
    });
    read_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("snapshot reached its SQL/control boundary");
    let (blocked_tx, blocked_rx) = std::sync::mpsc::sync_channel(1);
    let writer = std::thread::spawn({
        let daemon = fixture.daemon.clone();
        let session = fixture.session.clone();
        let run = fixture.run.clone();
        move || {
            // The snapshot must exclude a terminal writer until its running bit is
            // sampled. This handshake is deterministic, without timing/polling away
            // a mixed snapshot or assuming a task ran after an arbitrary sleep.
            let blocked = matches!(
                daemon.store.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            );
            blocked_tx.send(blocked).unwrap();
            daemon
                .with_store(|store| store.record_run_recap(&session, &run, &recap))
                .unwrap();
            daemon.runs.locked().remove(&session);
        }
    });
    let blocked = blocked_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    release_tx.send(()).unwrap();
    let before = getter.join().unwrap();
    writer.join().unwrap();
    *fixture.daemon.session_snapshot_test.locked() = None;
    assert!(
        blocked,
        "GetSession must retain the writer boundary through its running sample"
    );
    assert_eq!(before["running"], true);
    assert!(before["last_recap"].is_null());
    let after = fixture
        .daemon
        .handle(Request::GetSession {
            id: fixture.session.clone(),
        })
        .await
        .unwrap();
    assert_eq!(after["running"], false);
    assert_eq!(after["last_recap"]["run_id"], fixture.run);
    assert_eq!(after["last_recap"]["recap"]["steps"], 4);
}

#[tokio::test]
async fn abandon_waits_past_control_release_until_terminal_publication() {
    let env = crate::credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env);
    let recap = completed_recap(&fixture, "paused");
    let publication = fixture
        .daemon
        .begin_run_publication(&fixture.session, &fixture.run);
    let request = || Request::AbandonRun {
        session_id: fixture.session.clone(),
    };
    assert!(fixture
        .daemon
        .handle(request())
        .await
        .unwrap_err()
        .to_string()
        .contains("finishing"));
    fixture
        .daemon
        .with_store(|store| store.record_run_recap(&fixture.session, &fixture.run, &recap))
        .unwrap();
    fixture.daemon.runs.locked().remove(&fixture.session);
    // Capture is durable, but RunFinished has not been published yet. This is
    // the gap an active-RunCtl-only guard would fail to cover.
    assert!(fixture
        .daemon
        .handle(request())
        .await
        .unwrap_err()
        .to_string()
        .contains("finishing"));
    let snapshot = fixture
        .daemon
        .handle(Request::GetSession {
            id: fixture.session.clone(),
        })
        .await
        .unwrap();
    assert_eq!(snapshot["running"], true);
    assert_eq!(snapshot["last_recap"]["recap"]["status"], "paused");
    let mut rx = subscribe(&fixture.daemon);
    fixture.daemon.broadcast(Event::RunFinished {
        session_id: fixture.session.clone(),
        run_id: fixture.run.clone(),
        status: "paused".into(),
    });
    drop(publication);
    assert_eq!(
        fixture.daemon.handle(request()).await.unwrap()["abandoned"],
        true
    );
    assert!(
        matches!(rx.try_recv().unwrap(),ServerMessage::Event(Event::RunFinished{status,..}) if status=="paused")
    );
    assert!(
        rx.try_recv().is_err(),
        "a stale paused finish cannot follow the Abandon acknowledgement"
    );
    assert_eq!(
        fixture
            .daemon
            .with_reader(|store| store.run_status(&fixture.run))
            .unwrap()
            .as_deref(),
        Some("abandoned")
    );
    assert_eq!(
        fixture
            .daemon
            .with_reader(|store| store.recorded_run_recap(&fixture.session, &fixture.run))
            .unwrap()
            .unwrap()
            .recap
            .status,
        "paused",
        "the earlier terminal recap remains an immutable historical snapshot"
    );
}

#[tokio::test]
async fn publication_unwind_releases_only_its_run_gate() {
    let env = crate::credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env);
    completed_recap(&fixture, "paused");
    fixture.daemon.runs.locked().remove(&fixture.session);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _publication = fixture
            .daemon
            .begin_run_publication(&fixture.session, &fixture.run);
        assert!(fixture.daemon.session_is_publishing(&fixture.session));
        panic!("private publication fixture unwind");
    }));
    assert!(unwound.is_err());
    assert!(!fixture.daemon.session_is_publishing(&fixture.session));
    assert_eq!(
        fixture
            .daemon
            .handle(Request::AbandonRun {
                session_id: fixture.session.clone()
            })
            .await
            .unwrap()["abandoned"],
        true
    );
}
