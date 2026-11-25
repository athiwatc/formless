//! Deterministic interleaving of real prepasses: no provider or VM execution.
use super::*;

fn requested(events: &mut mpsc::Receiver<ServerMessage>) -> (String, Option<ApprovalBatch>) {
    while let Ok(message) = events.try_recv() {
        if let ServerMessage::Event(Event::ApprovalRequested {
            approval_id, batch, ..
        }) = message
        {
            return (approval_id, batch);
        }
    }
    panic!("polling the approval future must publish its prompt");
}

async fn approve(daemon: &Arc<Daemon>, id: String) {
    assert_eq!(
        daemon
            .handle(Request::ApprovalDecision {
                approval_id: id,
                approved: true,
                scope: ApprovalScope::Once,
                paths: Default::default(),
                remember_denial: false,
            })
            .await
            .unwrap()["resolved"],
        true
    );
}

#[test]
fn identity_uses_original_ordinal_and_id_even_for_duplicate_names_and_ids() {
    let held = |scope: &str| HeldCall {
        tool_name: "same".into(),
        capabilities: vec![scope.into()],
        new_capabilities: vec![scope.into()],
    };
    let batch = approval_batch::Batch::new(vec![
        (1, "duplicate".into(), held("first")),
        (4, "duplicate".into(), held("second")),
    ])
    .unwrap();
    let second = batch.for_call(4, "duplicate").unwrap();
    assert_eq!(second.index, 2);
    assert_eq!(second.calls[1].capabilities, ["second"]);
    assert_eq!(batch.for_call(1, "duplicate").unwrap().index, 1);
    assert!(batch.for_call(0, "duplicate").is_none());
    assert!(batch.for_call(4, "different-id").is_none());
}

#[tokio::test]
async fn interleaved_same_session_prepasses_and_direct_call_keep_owned_batches() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-batch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("project")).unwrap();
    let project = root.join("project").canonicalize().unwrap();
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.security.secure = true;
    cfg.security.trusted_paths.clear();
    cfg.tools.approval_summaries = false;
    cfg.save(&paths).unwrap();
    {
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let caps = Capabilities::parse(&["fs:write:$input.path".into()]).unwrap();
        let session = daemon
            .with_store(|st| {
                st.create_tool_version(
                    "same_writer",
                    "fixture",
                    "{\"type\":\"object\"}",
                    "{}",
                    "function run(i) { return true; }",
                    &caps,
                    64,
                    "user",
                )?;
                Ok(st.create_session(None)?.id)
            })
            .unwrap();
        let (tx, mut events) = mpsc::channel(64);
        daemon.subs.locked().push(Subscriber {
            conn: 969,
            tx: tx.into(),
            filter: Arc::new(StdMutex::new(EventFilter::default())),
            kick: CancellationToken::new(),
            gaps: BTreeMap::new(),
        });
        let call = |id: &str, file: &str| ToolCall {
            id: id.into(),
            name: "same_writer".into(),
            arguments: json!({"path":project.join(file)}).to_string(),
        };
        let a = [call("a-first", "a1"), call("a-second", "a2")];
        let b = [call("b-first", "b1"), call("b-second", "b2")];
        let ca = CancellationToken::new();
        let cb = CancellationToken::new();
        let cd = CancellationToken::new();
        let first = daemon.approve_calls(&cfg, &session, "same-run", &a, None, &ca);
        let second = daemon.approve_calls(&cfg, &session, "same-run", &b, None, &cb);
        tokio::pin!(first, second);
        assert!(futures_util::poll!(&mut first).is_pending());
        let (a1, ab1) = requested(&mut events);
        let ab1 = ab1.unwrap();
        assert!(futures_util::poll!(&mut second).is_pending());
        let (b1, bb1) = requested(&mut events);
        let bb1 = bb1.unwrap();
        assert_ne!(ab1.batch_id, bb1.batch_id);
        assert_eq!((ab1.index, bb1.index), (1, 1));
        let expected = |calls: &[ToolCall]| {
            calls
                .iter()
                .map(|call| {
                    let input: Value = serde_json::from_str(&call.arguments).unwrap();
                    HeldCall {
                        tool_name: "same_writer".into(),
                        capabilities: caps.bind(&input).unwrap().to_specs(),
                        new_capabilities: caps.bind(&input).unwrap().to_specs(),
                    }
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(ab1.calls, expected(&a));
        assert_eq!(bb1.calls, expected(&b));

        // A standalone registry call in the same Session cannot inherit either
        // batch, nor can its cancellation remove either prepass's context.
        let direct_call = call("direct", "direct");
        let direct_input: Value = serde_json::from_str(&direct_call.arguments).unwrap();
        let direct = daemon.clone().execute_tool_value(
            session.clone(),
            "same-run".into(),
            "same_writer".into(),
            direct_input,
            0,
            None,
            Arc::new(AtomicBool::new(false)),
            None,
            cd.clone(),
            cfg.clone(),
            false,
        );
        tokio::pin!(direct);
        assert!(futures_util::poll!(&mut direct).is_pending());
        let (_, direct_batch) = requested(&mut events);
        assert!(direct_batch.is_none());
        cd.cancel();
        assert!(direct.await.unwrap_err().contains("interrupted"));

        approve(&daemon, a1).await;
        assert!(futures_util::poll!(&mut first).is_pending());
        let (a2, ab2) = requested(&mut events);
        assert_eq!(ab2, Some(ApprovalBatch { index: 2, ..ab1 }));
        approve(&daemon, a2).await;
        assert!(first.await.unwrap().0.is_empty());

        // Completing A cannot remove B; its next same-name call must retain
        // B's original held list, ID and second position.
        approve(&daemon, b1).await;
        assert!(futures_util::poll!(&mut second).is_pending());
        let (b2, bb2) = requested(&mut events);
        assert_eq!(bb2, Some(ApprovalBatch { index: 2, ..bb1 }));
        approve(&daemon, b2).await;
        assert!(second.await.unwrap().0.is_empty());
        assert!(daemon.approvals.locked().is_empty());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cwd_change_during_approval_keeps_prompt_and_dispatch_on_prepared_scope() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-batch-cwd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    let a = root.join("a").canonicalize().unwrap();
    let b = root.join("b").canonicalize().unwrap();
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.security.secure = true;
    cfg.security.trusted_paths.clear();
    cfg.tools.approval_summaries = false;
    cfg.save(&paths).unwrap();
    {
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let caps = Capabilities::parse(&["fs:write:$input.path".into()]).unwrap();
        let session = daemon
            .with_store(|st| {
                st.create_tool_version(
                    "cwd_writer",
                    "fixture",
                    "{\"type\":\"object\"}",
                    "{}",
                    "function run(i) { fs.write(i.path, 'approved'); return true; }",
                    &caps,
                    64,
                    "user",
                )?;
                let session = st.create_session(None)?.id;
                st.set_session_cwd(&session, a.to_str())?;
                Ok(session)
            })
            .unwrap();
        let (tx, mut events) = mpsc::channel(64);
        daemon.subs.locked().push(Subscriber {
            conn: 969,
            tx: tx.into(),
            filter: Arc::new(StdMutex::new(EventFilter::default())),
            kick: CancellationToken::new(),
            gaps: BTreeMap::new(),
        });
        let calls = [
            ToolCall {
                id: "first".into(),
                name: "cwd_writer".into(),
                arguments: json!({"path":"first"}).to_string(),
            },
            ToolCall {
                id: "second".into(),
                name: "cwd_writer".into(),
                arguments: json!({"path":"second"}).to_string(),
            },
        ];
        let cancel = CancellationToken::new();
        let approval = daemon.approve_calls(&cfg, &session, "fixture", &calls, None, &cancel);
        tokio::pin!(approval);
        assert!(futures_util::poll!(&mut approval).is_pending());
        let (first, batch) = requested(&mut events);
        let batch = batch.unwrap();
        assert_eq!(
            batch.calls[1].capabilities,
            vec![format!("fs:write:{}", a.join("second").display())]
        );
        daemon
            .handle(Request::SetSessionCwd {
                id: session.clone(),
                cwd: b.to_str().map(str::to_string),
            })
            .await
            .unwrap();
        approve(&daemon, first).await;
        assert!(futures_util::poll!(&mut approval).is_pending());
        let (second, second_batch) = requested(&mut events);
        assert_eq!(second_batch, Some(ApprovalBatch { index: 2, ..batch }));
        approve(&daemon, second).await;
        let (denied, prepared) = approval.await.unwrap();
        assert!(denied.is_empty());
        for (call, leaf) in prepared.iter().zip(["first", "second"]) {
            let input: Value = serde_json::from_str(&call.arguments).unwrap();
            assert_eq!(input["path"], a.join(leaf).to_str().unwrap());
            let result = daemon
                .dispatch_call_inner(
                    &session,
                    "fixture",
                    call,
                    &cfg,
                    &Arc::new(AtomicBool::new(false)),
                    &cancel,
                    &denied,
                    0,
                )
                .await;
            assert_eq!(result, "true");
            assert_eq!(std::fs::read_to_string(a.join(leaf)).unwrap(), "approved");
            assert!(
                !b.join(leaf).exists(),
                "changed cwd must not retarget a preapproved call"
            );
        }
        while let Ok(message) = events.try_recv() {
            assert!(
                !matches!(
                    message,
                    ServerMessage::Event(Event::ApprovalRequested { .. })
                ),
                "prepared calls were already approved for their exact scope"
            );
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn skipped_calls_are_excluded_without_skipping_executable_approvals() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-step-approval-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("project")).unwrap();
    let project = root.join("project").canonicalize().unwrap();
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.security.secure = true;
    cfg.security.trusted_paths.clear();
    cfg.tools.approval_summaries = false;
    cfg.save(&paths).unwrap();
    {
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let caps = Capabilities::parse(&["fs:write:$input.path".into()]).unwrap();
        let session = daemon
            .with_store(|store| {
                store.create_tool_version(
                    "step_writer",
                    "fixture",
                    r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
                    "{}",
                    "function run(i) { return true; }",
                    &caps,
                    64,
                    "user",
                )?;
                let session = store.create_session(None)?.id;
                store.set_session_cwd(&session, project.to_str())?;
                Ok(session)
            })
            .unwrap();
        let (tx, mut events) = mpsc::channel(64);
        daemon.subs.locked().push(Subscriber {
            conn: 969,
            tx: tx.into(),
            filter: Arc::new(StdMutex::new(EventFilter::default())),
            kick: CancellationToken::new(),
            gaps: BTreeMap::new(),
        });
        let call = |id: &str, leaf: &str| ToolCall {
            id: id.into(),
            name: "step_writer".into(),
            arguments: json!({"path": project.join(leaf)}).to_string(),
        };
        let calls = [
            call("skipped", "skipped.txt"),
            call("first", "first.txt"),
            call("second", "second.txt"),
        ];
        let skipped = HashSet::from(["skipped".to_string()]);
        let cancel = CancellationToken::new();
        let approval =
            daemon.approve_calls(&cfg, &session, "fixture", &calls, Some(&skipped), &cancel);
        tokio::pin!(approval);
        assert!(futures_util::poll!(&mut approval).is_pending());
        let (first_id, first_batch) = requested(&mut events);
        let first_batch = first_batch.unwrap();
        assert_eq!(first_batch.index, 1);
        assert_eq!(
            first_batch
                .calls
                .iter()
                .map(|call| call.capabilities.clone())
                .collect::<Vec<_>>(),
            [
                vec![format!("fs:write:{}", project.join("first.txt").display())],
                vec![format!("fs:write:{}", project.join("second.txt").display())],
            ]
        );
        assert_eq!(
            first_batch
                .calls
                .iter()
                .map(|call| call.new_capabilities.clone())
                .collect::<Vec<_>>(),
            [
                vec![format!("fs:write:{}", project.join("first.txt").display())],
                vec![format!("fs:write:{}", project.join("second.txt").display())],
            ]
        );
        approve(&daemon, first_id).await;
        assert!(futures_util::poll!(&mut approval).is_pending());
        let (second_id, second_batch) = requested(&mut events);
        assert_eq!(
            second_batch,
            Some(ApprovalBatch {
                index: 2,
                ..first_batch
            })
        );
        approve(&daemon, second_id).await;
        let (denied, prepared) = approval.await.unwrap();
        assert!(denied.is_empty());
        assert_eq!(prepared.len(), calls.len());
        assert_eq!(prepared[0].arguments, calls[0].arguments);
        while let Ok(message) = events.try_recv() {
            assert!(
                !matches!(
                    message,
                    ServerMessage::Event(Event::ApprovalRequested { .. })
                ),
                "only executable calls require approval"
            );
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}
