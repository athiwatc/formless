//! The MAX sentinel is an in-memory watcher branch, not a TOML value.
//! IPC fixtures exercise the public foreground stay mode independently.
use super::*;

#[tokio::test]
async fn actual_idle_watcher_scans_stalls_when_cached_idle_close_is_disabled() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-idle-watcher-fixture");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env.remove(STAY_ENV);
    let root = std::env::temp_dir().join(format!("fl-progress-idle-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut config = Config::default();
    config.provider.base_url = "http://127.0.0.1:9".into();
    config.provider.model = "private-unused-provider".into();
    config.naming.enabled = false;
    config.skills.auto_create = false;
    config.daemon.idle_grace_secs = 3600;
    config.daemon.stall_warning_secs = 1;
    config.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    assert_eq!(daemon.config().unwrap().daemon.idle_grace_secs, 3600);
    // Keep the real saved file and its real ConfigStamp. Only this private
    // test changes the cached value, so no product setter or public hook exists.
    daemon
        .config
        .locked()
        .as_mut()
        .unwrap()
        .1
        .daemon
        .idle_grace_secs = u64::MAX;
    assert_eq!(daemon.config().unwrap().daemon.idle_grace_secs, u64::MAX);
    assert_eq!(
        Config::load_file_only(&paths)
            .unwrap()
            .daemon
            .idle_grace_secs,
        3600
    );
    let session = daemon
        .with_store(|store| Ok(store.create_session(None)?.id))
        .unwrap();
    let progress = Arc::new(crate::invocation_progress::InvocationProgress::default());
    progress.report(Some(0.5), "working", std::time::Instant::now());
    let handle = "private-idle-handle".to_string();
    let now = epoch_ms();
    let entry = Arc::new(PendingEntry {
        progress: progress.clone(),
        _pipeline: StdMutex::new(None),
        routing_task: None,
        taint_scope: daemon.taint_scope(&session, "private-idle-run"),
        tool_name: "private-progress-loop".into(),
        run_id: "private-idle-run".into(),
        started_ms: now.saturating_sub(2000),
        session_id: session.clone(),
        result: StdMutex::new(None),
        delivered: AtomicBool::new(false),
        abandoned: AtomicBool::new(false),
        stalled_noted: AtomicBool::new(false),
        activity: Arc::new(std::sync::atomic::AtomicU64::new(now.saturating_sub(2000))),
        notify: tokio::sync::Notify::new(),
        cancel: CancellationToken::new(),
        order: 0,
    });
    daemon.pending.locked().insert(handle.clone(), entry);
    let (tx, mut rx) = mpsc::channel(8);
    daemon.subs.locked().push(Subscriber {
        conn: 1,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    // Exercise the actual watcher and its real first one-second tick, rather
    // than calling the extracted scanner or assuming a sleep means it ran.
    let watcher = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.idle_watcher().await }
    });
    let observed = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
    daemon.shutdown.cancel();
    watcher.await.unwrap();
    let event = observed
        .expect("MAX idle-close setting must not skip the real stall scan")
        .expect("watcher event receiver closed");
    match event {
        ServerMessage::Event(Event::InvocationStalled {
            invocation_id,
            session_id,
            handle: actual,
            tool_name,
            last_fraction,
            last_label,
            ..
        }) => {
            assert_eq!(invocation_id.as_deref(), Some(progress.id.as_str()));
            assert_eq!(session_id, session);
            assert_eq!(actual, handle);
            assert_eq!(tool_name, "private-progress-loop");
            assert_eq!(last_fraction, Some(0.5));
            assert_eq!(last_label.as_deref(), Some("working"));
        }
        other => panic!("expected the real watcher's stall notice, got {other:?}"),
    }
    assert_eq!(daemon.config().unwrap().daemon.idle_grace_secs, u64::MAX);
    drop(daemon);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn live_tool_progress_flushes_without_another_host_call() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    let root = std::env::temp_dir().join(format!("fl-progress-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let session_id = "private-progress-session".to_owned();
    let run_id = "private-progress-run".to_owned();
    let ops = NestedOps {
        _pipeline: None,
        routing_task: None,
        _taint_scope: daemon.taint_scope(&session_id, &run_id),
        read_identity: None,
        read_input: Value::Null,
        daemon: daemon.clone(),
        tool_name: "progress-fixture".into(),
        kv_scope: "test:progress".into(),
        session_id,
        run_id,
        parent_inv: None,
        progress: Arc::new(Default::default()),
        activity: None,
        depth: 1,
        agent_depth: 0,
        agent_ok: false,
        script: None,
        cancel_flag: Arc::new(AtomicBool::new(false)),
        cancel: CancellationToken::new(),
        cfg,
        handle: tokio::runtime::Handle::current(),
        emitted: StdMutex::new(None),
        artifact_dir: root.join("artifacts"),
    };
    let (tx, mut rx) = mpsc::channel(8);
    daemon.subs.locked().push(Subscriber {
        conn: 1,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });

    // Seed the leading state without a wire event, then exercise the real
    // HostOps scheduling and the daemon's subscriber path.
    let leading = std::time::Instant::now() + std::time::Duration::from_millis(200);
    assert!(ops
        .progress
        .report(Some(0.1), "starting", leading)
        .emit
        .is_some());
    ops.progress(Some(0.9), "writing");
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    let event = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    match event {
        ServerMessage::Event(Event::ToolProgress {
            invocation_id,
            fraction,
            label,
            ..
        }) => {
            assert_eq!(invocation_id.as_deref(), Some(ops.progress.id.as_str()));
            assert_eq!(fraction, Some(0.9));
            assert_eq!(label, "writing");
        }
        other => panic!("expected the trailing ToolProgress, got {other:?}"),
    }
    drop(ops);
    drop(daemon);
    let _ = std::fs::remove_dir_all(root);
}
