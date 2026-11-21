use super::*;
#[tokio::test]
async fn session_selection_cas_dispatch_is_atomic_metadata_only_even_with_auto_tests_enabled() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-selection-daemon-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.auto_test_models = true;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let id = daemon
        .with_store(|st| Ok(st.create_session(None)?.id))
        .unwrap();
    // A synchronous trap: any unintended model-test reservation fails without
    // network and emits an extra Note. The metadata API must never access it.
    let trap = paths.data_dir.join("model_tests.json");
    std::fs::create_dir(&trap).unwrap();
    let (tx, mut rx) = mpsc::channel(16);
    daemon.subs.locked().push(Subscriber {
        conn: 99,
        tx: crate::relay_buffers::ConnectionSender::from(tx),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: Default::default(),
    });
    let empty = formless_protocol::SessionModelSelection {
        model: None,
        reasoning_effort: None,
    };
    let desired = formless_protocol::SessionModelSelection {
        model: Some("new-model".into()),
        reasoning_effort: Some("high".into()),
    };
    let request = || Request::CompareSetSessionModel {
        id: id.clone(),
        expected: empty.clone(),
        desired: desired.clone(),
    };
    let reply = daemon.handle(request()).await.unwrap();
    assert_eq!(reply["model"], "new-model");
    assert_eq!(reply["reasoning_effort"], "high");
    assert!(matches!(
        rx.try_recv().unwrap(),
        ServerMessage::Event(Event::SessionUpdated { .. })
    ));
    assert!(rx.try_recv().is_err());
    assert!(daemon.handle(request()).await.is_err());
    assert!(rx.try_recv().is_err());
    assert!(trap.is_dir());
    assert!(daemon.runs.locked().is_empty());
    assert!(daemon
        .with_reader(|st| st.list_messages(&id))
        .unwrap()
        .is_empty());
    assert_eq!(daemon.with_reader(|st| st.queued_count(&id)).unwrap(), 0);
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}
