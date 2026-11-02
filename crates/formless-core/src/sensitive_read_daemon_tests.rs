//! Real registry/runtime approval fixtures: no provider, keyring or user files.
use super::*;

struct Fixture {
    daemon: Option<Arc<Daemon>>,
    root: std::path::PathBuf,
    paths: Paths,
    session: String,
    tool: ToolInfo,
}
impl Fixture {
    fn new(env: &crate::credential_env::EnvGuard, secure: bool) -> Self {
        env.remove("FORMLESS_SECURE");
        let root =
            std::env::temp_dir().join(format!("fl-sensitive-registry-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("project/.ssh")).unwrap();
        let project = root.join("project").canonicalize().unwrap();
        let mut cfg = Config::default();
        cfg.security.secure = secure;
        cfg.security.trusted_paths = vec![project.to_str().unwrap().into()];
        cfg.tools.approval_summaries = false;
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        assert_eq!(daemon.config().unwrap().security.secure, secure);
        let caps = Capabilities::parse(&[format!("fs:read:{}", project.display())]).unwrap();
        let (session, tool) = daemon
            .with_store(|st| {
                let session = st.create_session(None)?.id;
                let (tool, _) = st.create_tool_version(
                    "fixture_reader",
                    "Read the supplied file",
                    "{\"type\":\"object\"}",
                    "{}",
                    "function run(i) { return fs.read(i.path); }",
                    &caps,
                    64,
                    "user",
                )?;
                st.set_approval(&tool.id, &caps, 64)?;
                Ok((session, tool))
            })
            .unwrap();
        Self {
            daemon: Some(daemon),
            root,
            paths,
            session,
            tool,
        }
    }
    fn daemon(&self) -> &Arc<Daemon> {
        self.daemon.as_ref().unwrap()
    }
    fn events(&self) -> mpsc::Receiver<ServerMessage> {
        let (tx, rx) = mpsc::channel(64);
        self.daemon().subs.locked().push(Subscriber {
            conn: 708,
            tx: tx.into(),
            filter: Arc::new(StdMutex::new(EventFilter::default())),
            kick: CancellationToken::new(),
            gaps: BTreeMap::new(),
        });
        rx
    }
    fn launch(
        &self,
        path: &std::path::Path,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<std::result::Result<(Value, String), String>> {
        self.launch_controlled(path, cancel, Arc::new(AtomicBool::new(false)))
    }
    fn launch_controlled(
        &self,
        path: &std::path::Path,
        cancel: CancellationToken,
        cancel_flag: Arc<AtomicBool>,
    ) -> tokio::task::JoinHandle<std::result::Result<(Value, String), String>> {
        tokio::spawn(self.daemon().clone().execute_tool_value(
            self.session.clone(),
            "credential-fixture-run".into(),
            self.tool.name.clone(),
            json!({"path":path}),
            0,
            None,
            cancel_flag,
            None,
            cancel,
            self.daemon().config().unwrap(),
            false,
        ))
    }
    fn last_status(&self) -> (String, String, String) {
        let conn = rusqlite::Connection::open_with_flags(
            self.paths.db_file(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.query_row("SELECT status,tag,output FROM invocations WHERE tool_id=?1 ORDER BY rowid DESC LIMIT 1", [&self.tool.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.daemon.take());
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
async fn next_approval(rx: &mut mpsc::Receiver<ServerMessage>) -> Event {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(message) = rx.recv().await {
            if let ServerMessage::Event(event @ Event::ApprovalRequested { .. }) = message {
                return event;
            }
        }
        panic!("private event stream ended before approval")
    })
    .await
    .unwrap()
}
async fn completed(
    task: tokio::task::JoinHandle<std::result::Result<(Value, String), String>>,
) -> std::result::Result<(Value, String), String> {
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn exact_read_approval_overrides_secure_off_old_broad_grants_and_trust_without_widening_authority(
) {
    let env = crate::credential_env::EnvGuard::isolated();
    for secure in [false, true] {
        let f = Fixture::new(&env, secure);
        let normal = f.root.join("project/notes.txt");
        let key = f.root.join("project/.ssh/config");
        std::fs::write(&normal, "ordinary-project-control").unwrap();
        std::fs::write(&key, "private-credential-fixture").unwrap();
        let mut events = f.events();
        assert_eq!(
            completed(f.launch(&normal, CancellationToken::new()))
                .await
                .unwrap()
                .0,
            json!("ordinary-project-control")
        );
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(
                event,
                ServerMessage::Event(Event::ApprovalRequested { .. })
            ));
        }
        f.daemon()
            .record_data_origin(
                &f.session,
                "credential-fixture-run",
                "fixture_source",
                "framed_data",
                "private source fixture",
            )
            .unwrap();
        let task = f.launch(&key, CancellationToken::new());
        let Event::ApprovalRequested {
            approval_id,
            capabilities,
            path_options,
            context: Some(context),
            tool_name,
            code_preview,
            ..
        } = next_approval(&mut events).await
        else {
            panic!("no actual registry call context")
        };
        assert!(
            !task.is_finished(),
            "the runtime must wait before returning bytes"
        );
        let canonical = key.canonicalize().unwrap();
        assert_eq!(
            capabilities,
            vec![format!("fs:read-sensitive:{}", canonical.display())]
        );
        assert!(
            path_options.is_empty(),
            "exact credential consent cannot widen to a folder"
        );
        assert_eq!(tool_name, "fixture_reader");
        assert!(code_preview.unwrap().contains("fs.read"));
        assert_eq!(context.run_id.as_deref(), Some("credential-fixture-run"));
        assert!(context
            .sources
            .iter()
            .any(|source| source.tool == "fixture_source"));
        assert_eq!(
            context.call.unwrap().arguments["runtime_access"]["path"],
            json!(canonical)
        );
        f.daemon()
            .handle(Request::ApprovalDecision {
                approval_id,
                approved: true,
                scope: ApprovalScope::Always,
                paths: Default::default(),
                remember_denial: false,
            })
            .await
            .unwrap();
        assert_eq!(
            completed(task).await.unwrap().0,
            json!("private-credential-fixture")
        );
        let (grant, _) = f
            .daemon()
            .with_reader(|st| st.get_approval(&f.tool.id))
            .unwrap()
            .unwrap();
        assert_eq!(
            grant.sensitive_read,
            vec![canonical.to_str().unwrap().to_string()]
        );
        assert!(grant.net.is_none() && grant.process.is_none() && grant.fs_write.is_empty());
        assert_eq!(
            grant.fs_read,
            vec![f
                .root
                .join("project")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()]
        );
        assert_eq!(f.last_status().0, "success");
        let sibling = f.root.join("project/.ssh/other-config");
        std::fs::write(&sibling, "second-private-fixture").unwrap();
        let task = f.launch(&sibling, CancellationToken::new());
        let Event::ApprovalRequested { approval_id, .. } = next_approval(&mut events).await else {
            unreachable!()
        };
        f.daemon()
            .handle(Request::ApprovalDecision {
                approval_id,
                approved: false,
                scope: ApprovalScope::Once,
                paths: Default::default(),
                remember_denial: false,
            })
            .await
            .unwrap();
        assert!(completed(task).await.is_err());
        assert_eq!(
            f.last_status(),
            ("failure".into(), "denied-capability".into(), String::new())
        );
        assert!(f.daemon().approvals.locked().is_empty());
    }
}

#[tokio::test]
async fn cancelled_credential_consent_and_active_profile_reads_fail_before_bytes() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, false);
    let key = f.root.join("project/.ssh/config");
    std::fs::write(&key, "cancelled-private-fixture").unwrap();
    let mut events = f.events();
    let cancel = CancellationToken::new();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let task = f.launch_controlled(&key, cancel.clone(), cancel_flag.clone());
    let Event::ApprovalRequested { approval_id, .. } = next_approval(&mut events).await else {
        unreachable!()
    };
    assert!(f.daemon().pending_attention.locked().iter().any(|event| matches!(event, Event::ApprovalRequested { approval_id: id, .. } if id == &approval_id)));
    // InterruptRun uses this same ordering for the runtime flag and token.
    cancel_flag.store(true, Ordering::SeqCst);
    cancel.cancel();
    assert!(completed(task).await.is_err());
    let (status, tag, output) = f.last_status();
    assert_eq!(
        (status.as_str(), tag.as_str()),
        ("cancelled", "user-cancelled")
    );
    assert!(output.is_empty());
    assert!(f.daemon().approvals.locked().is_empty());
    assert!(f.daemon().approval_paths.locked().is_empty());
    assert!(f.daemon().approval_by.locked().is_empty());
    assert!(f.daemon().deny_always.locked().is_empty());
    assert!(!f.daemon().pending_attention.locked().iter().any(|event| matches!(event, Event::ApprovalRequested { approval_id: id, .. } if id == &approval_id)));
    // No sleep/yield: completion itself guarantees cleanup. A late decision
    // cannot resolve or recreate widening/denial metadata for the dead waiter.
    let late = f
        .daemon()
        .handle(Request::ApprovalDecision {
            approval_id,
            approved: false,
            scope: ApprovalScope::Always,
            paths: [(
                "fs:read-sensitive:cancelled".into(),
                f.root.display().to_string(),
            )]
            .into(),
            remember_denial: true,
        })
        .await
        .unwrap();
    assert_eq!(late["resolved"], false);
    assert!(f.daemon().approvals.locked().is_empty());
    assert!(f.daemon().approval_paths.locked().is_empty());
    assert!(f.daemon().approval_by.locked().is_empty());
    assert!(f.daemon().deny_always.locked().is_empty());
    let profile_key = f.paths.config_dir.join("api_key");
    std::fs::write(&profile_key, "private-profile-fixture").unwrap();
    // A broad legacy grant covering both project and profile cannot offer
    // this path for approval or bypass the unconditional profile boundary.
    f.daemon()
        .with_store(|st| {
            let caps = Capabilities::parse(&[format!("fs:read:{}", f.root.display())]).unwrap();
            st.create_tool_version(
                "fixture_reader",
                "Read supplied file",
                "{\"type\":\"object\"}",
                "{}",
                "function run(i) { return fs.read(i.path); }",
                &caps,
                64,
                "user",
            )?;
            st.set_approval(&f.tool.id, &caps, 64)
        })
        .unwrap();
    while events.try_recv().is_ok() {}
    assert!(completed(f.launch(&profile_key, CancellationToken::new()))
        .await
        .is_err());
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(
            event,
            ServerMessage::Event(Event::ApprovalRequested { .. })
        ));
    }
    assert_eq!(
        std::fs::read_to_string(profile_key).unwrap(),
        "private-profile-fixture"
    );
    assert!(f.last_status().2.is_empty());
}

#[test]
fn typed_extensions_roundtrip_and_are_owner_file_only() {
    let mut cfg: Config = toml::from_str("[security]\nsensitive_paths = [{kind='subtree',value='~/private'},{kind='basename_suffix',value='.p12'}]\n").unwrap();
    assert_eq!(cfg.security.sensitive_paths.len(), 2);
    let roundtrip: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(roundtrip.security, cfg.security);
    let before = cfg.security.clone();
    assert!(cfg.set_key("security.sensitive_paths", json!([])).is_err());
    assert_eq!(cfg.security, before);
    assert!(cfg
        .set_key("security", json!({"sensitive_paths":[]}))
        .is_err());
    assert_eq!(cfg.security, before);
}

#[tokio::test]
async fn default_and_named_external_key_files_are_hard_denied_by_path_without_reading_key_values() {
    struct ExternalDir(std::path::PathBuf);
    impl Drop for ExternalDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let env = crate::credential_env::EnvGuard::isolated();
    let external = ExternalDir(
        std::env::temp_dir().join(format!("fl-owned-external-key-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&external.0).unwrap();
    let external_root = external.0.canonicalize().unwrap();
    let default = external_root.join("creds.txt");
    let named = external_root.join("named-source.txt");
    let ordinary = external_root.join("notes.txt");
    let replacement = external_root.join("replacement.txt");
    for path in [&default, &named, &replacement] {
        std::fs::write(path, "private-owned-key-fixture").unwrap();
    }
    std::fs::write(&ordinary, "ordinary-project-control").unwrap();
    #[cfg(unix)]
    let original = external_root.join("physical-original.txt");
    #[cfg(unix)]
    std::fs::write(&original, "private-owned-key-fixture").unwrap();
    for secure in [false, true] {
        #[cfg(unix)]
        {
            std::fs::remove_file(&default).unwrap();
            std::os::unix::fs::symlink(&original, &default).unwrap();
        }
        env.set("FORMLESS_API_KEY_FILE", &default);
        // provider_key("my-custom") uses exactly this normalized variable.
        env.set("FORMLESS_API_KEY_MY_CUSTOM_FILE", &named);
        let f = Fixture::new(&env, secure);
        let mut cfg = f.daemon().config().unwrap();
        cfg.security
            .trusted_paths
            .push(external_root.to_str().unwrap().into());
        cfg.save(&f.paths).unwrap();
        let paths = crate::sensitive_reads::credential_file_paths().unwrap();
        assert!(paths.contains(&default) && paths.contains(&named));
        f.daemon()
            .with_store(|st| {
                let caps =
                    Capabilities::parse(&[format!("fs:read:{}", external_root.display())]).unwrap();
                st.create_tool_version(
                    "fixture_reader",
                    "Read supplied file",
                    "{\"type\":\"object\"}",
                    "{}",
                    "function run(i) { return fs.read(i.path); }",
                    &caps,
                    64,
                    "user",
                )?;
                let mut legacy = caps;
                legacy.sensitive_read = vec![
                    default.to_str().unwrap().into(),
                    named.to_str().unwrap().into(),
                ];
                st.set_approval(&f.tool.id, &legacy, 64)
            })
            .unwrap();
        let mut events = f.events();
        assert_eq!(
            completed(f.launch(&ordinary, CancellationToken::new()))
                .await
                .unwrap()
                .0,
            json!("ordinary-project-control")
        );
        for path in [&default, &named] {
            assert!(completed(f.launch(path, CancellationToken::new()))
                .await
                .is_err());
            assert!(f.last_status().2.is_empty());
        }
        // Current environment-selected identities and startup anchors both
        // remain protected, even after that selection changes or disappears.
        env.set("FORMLESS_API_KEY_FILE", &replacement);
        assert!(completed(f.launch(&replacement, CancellationToken::new()))
            .await
            .is_err());
        env.remove("FORMLESS_API_KEY_FILE");
        env.remove("FORMLESS_API_KEY_MY_CUSTOM_FILE");
        #[cfg(unix)]
        {
            std::fs::remove_file(&default).unwrap();
            std::os::unix::fs::symlink(&replacement, &default).unwrap();
            // Retain the physical startup destination and the configured
            // alias namespace after env removal and alias retargeting.
            for path in [&original, &replacement] {
                assert!(completed(f.launch(path, CancellationToken::new()))
                    .await
                    .is_err());
            }
        }
        for path in [&default, &named] {
            assert!(completed(f.launch(path, CancellationToken::new()))
                .await
                .is_err());
        }
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(
                event,
                ServerMessage::Event(Event::ApprovalRequested { .. })
            ));
        }
        assert!(f.daemon().approvals.locked().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answer_claimed_before_cancel_cleanup_is_acknowledged_and_applied_once() {
    type Gate = Arc<(StdMutex<bool>, std::sync::Condvar)>;
    struct ReleaseGate(Gate);
    impl Drop for ReleaseGate {
        fn drop(&mut self) {
            let (released, ready) = &*self.0;
            *released.locked() = true;
            ready.notify_all();
        }
    }
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, false);
    let key = f.root.join("project/.ssh/config");
    std::fs::write(&key, "inflight-private-fixture").unwrap();
    let cancel = CancellationToken::new();
    let flag = Arc::new(AtomicBool::new(false));
    let mut events = f.events();
    let task = f.launch_controlled(&key, cancel.clone(), flag.clone());
    let Event::ApprovalRequested { approval_id, .. } = next_approval(&mut events).await else {
        unreachable!()
    };
    let (claimed_tx, claimed_rx) = oneshot::channel();
    let (cleanup_tx, cleanup_rx) = oneshot::channel();
    let claimed_tx = StdMutex::new(Some(claimed_tx));
    let cleanup_tx = StdMutex::new(Some(cleanup_tx));
    let gate: Gate = Arc::new((StdMutex::new(false), std::sync::Condvar::new()));
    let release = ReleaseGate(gate.clone());
    let wanted = approval_id.clone();
    *f.daemon().approval_transition_test.locked() = Some(Arc::new(move |phase, id| {
        if id != wanted {
            return;
        }
        match phase {
            "answer-claimed" => {
                claimed_tx.locked().take().unwrap().send(()).unwrap();
                let (open, ready) = &*gate;
                let (released, timeout) = ready
                    .wait_timeout_while(
                        open.locked(),
                        std::time::Duration::from_secs(5),
                        |released| !*released,
                    )
                    .unwrap();
                assert!(
                    *released && !timeout.timed_out(),
                    "private answer gate was not released"
                );
            }
            "waiter-cleanup" => {
                if let Some(tx) = cleanup_tx.locked().take() {
                    let _ = tx.send(());
                }
            }
            _ => panic!("unexpected private approval transition"),
        }
    }));
    let daemon = f.daemon().clone();
    let answer_id = approval_id.clone();
    let requested_path = key.to_str().unwrap().to_string();
    let root = f.root.to_str().unwrap().to_string();
    let runtime = tokio::runtime::Handle::current();
    // Actual request handler on a blocking worker: its private gate never
    // blocks the async test's two-worker executor or another daemon's tests.
    let answer = tokio::task::spawn_blocking(move || {
        runtime.block_on(CLIENT.scope(
            (708, "private answer fixture".into()),
            daemon.handle(Request::ApprovalDecision {
                approval_id: answer_id,
                approved: false,
                scope: ApprovalScope::Always,
                paths: [(format!("fs:read-sensitive:{requested_path}"), root)].into(),
                remember_denial: true,
            }),
        ))
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), claimed_rx)
        .await
        .unwrap()
        .unwrap();
    flag.store(true, Ordering::SeqCst);
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), cleanup_rx)
        .await
        .unwrap()
        .unwrap();
    // The waiter has selected cancellation and reached cleanup, but the
    // answer holds its transaction lock before publishing any metadata.
    assert!(matches!(
        f.daemon().approvals.try_lock(),
        Err(std::sync::TryLockError::WouldBlock)
    ));
    assert!(!task.is_finished());
    assert!(f.daemon().approval_paths.locked().is_empty());
    assert!(f.daemon().approval_by.locked().is_empty());
    assert!(f.daemon().deny_always.locked().is_empty());
    drop(release);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), answer)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        result["resolved"], true,
        "the handler claimed the registry before cancellation cleanup"
    );
    assert!(completed(task).await.is_err());
    assert_eq!(
        f.last_status(),
        ("cancelled".into(), "user-cancelled".into(), String::new())
    );
    assert!(f.daemon().approvals.locked().is_empty());
    assert!(f.daemon().approval_paths.locked().is_empty());
    assert!(f.daemon().approval_by.locked().is_empty());
    assert!(f.daemon().deny_always.locked().is_empty());
    let denials = f.daemon().with_reader(|st| st.list_denials()).unwrap();
    assert_eq!(
        denials.len(),
        1,
        "the acknowledged explicit denial is applied once"
    );
    assert_eq!(
        denials[0].capability,
        format!("fs:read-sensitive:{}", key.display())
    );
    assert_eq!(denials[0].denied_by, "private answer fixture");
    assert!(!f.daemon().pending_attention.locked().iter().any(
        |event| matches!(event, Event::ApprovalRequested {approval_id:id, ..} if id == &approval_id)
    ));
    *f.daemon().approval_transition_test.locked() = None;
    assert_eq!(
        std::fs::read_to_string(key).unwrap(),
        "inflight-private-fixture"
    );
}

#[tokio::test]
async fn acknowledged_question_survives_both_ready_cancel_exactly_once() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, true);
    let mut events = f.events();
    let cancel = CancellationToken::new();
    let args = json!({"question":"Which fixture?", "options":["one", "two"]});
    let reply = f.daemon().ask_user_impl(&f.session, &args, &cancel);
    tokio::pin!(reply);
    assert!(futures_util::poll!(&mut reply).is_pending());
    let question_id = loop {
        if let ServerMessage::Event(Event::UserQuestion { question_id, .. }) =
            events.try_recv().unwrap()
        {
            break question_id;
        }
    };
    let answer = || Request::AnswerQuestion {
        question_id: question_id.clone(),
        answer: "one".into(),
        answers: Vec::new(),
    };
    assert_eq!(f.daemon().handle(answer()).await.unwrap()["answered"], true);
    cancel.cancel();
    let value: Value = serde_json::from_str(&reply.await.unwrap()).unwrap();
    assert_eq!(value, json!({"answer":"one"}));
    assert_eq!(
        f.daemon().handle(answer()).await.unwrap()["answered"],
        false
    );
    let mut closed = 0;
    while let Ok(message) = events.try_recv() {
        if matches!(message, ServerMessage::Event(Event::UserQuestionClosed {question_id:id}) if id == question_id)
        {
            closed += 1;
        }
    }
    assert_eq!(closed, 1);
    assert!(f.daemon().questions.locked().is_empty());
}

#[tokio::test]
async fn acknowledged_always_grant_survives_both_ready_cancel_without_dispatch() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, true);
    let mut events = f.events();
    let cancel = CancellationToken::new();
    let (info, ver) = f
        .daemon()
        .with_store(|st| st.get_tool_by_name(&f.tool.name))
        .unwrap()
        .unwrap();
    let caps = Capabilities::parse(&["process:fixture-no-execution".into()]).unwrap();
    let cfg = f.daemon().config().unwrap();
    let approval = f.daemon().ensure_approval(
        &cfg, &f.session, "fixture", &info, &ver, &caps, None, &cancel, None,
    );
    tokio::pin!(approval);
    assert!(futures_util::poll!(&mut approval).is_pending());
    let Event::ApprovalRequested { approval_id, .. } = next_approval(&mut events).await else {
        unreachable!()
    };
    let answer = || Request::ApprovalDecision {
        approval_id: approval_id.clone(),
        approved: true,
        scope: ApprovalScope::Always,
        paths: Default::default(),
        remember_denial: false,
    };
    assert_eq!(f.daemon().handle(answer()).await.unwrap()["resolved"], true);
    cancel.cancel();
    assert!(
        approval.await,
        "a delivered grant must be applied before caller interruption"
    );
    assert_eq!(
        f.daemon().handle(answer()).await.unwrap()["resolved"],
        false
    );
    let grant = f
        .daemon()
        .with_store(|st| st.get_approval(&info.id))
        .unwrap()
        .unwrap();
    assert!(grant.0.covers(&caps));
    assert!(f.daemon().approvals.locked().is_empty());
    assert!(f.daemon().approval_paths.locked().is_empty());
    assert!(f.daemon().approval_by.locked().is_empty());
    assert!(f.daemon().deny_always.locked().is_empty());
    let conn = rusqlite::Connection::open(f.paths.db_file()).unwrap();
    let grants: i64 = conn
        .query_row(
            "SELECT count(*) FROM audit WHERE action='tool.approve'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(grants, 1);
    let decisions: i64 = conn
        .query_row(
            "SELECT count(*) FROM audit WHERE action='approval.decision'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(decisions, 1);
    let mut answered = 0;
    while let Ok(message) = events.try_recv() {
        match message {
            ServerMessage::Event(Event::ApprovalAnswered {
                approval_id: id,
                approved,
                ..
            }) if id == approval_id => {
                assert!(approved);
                answered += 1;
            }
            ServerMessage::Event(Event::ApprovalClosed { approval_id: id })
                if id == approval_id =>
            {
                panic!("acknowledged grant cannot be cancelled closed")
            }
            _ => {}
        }
    }
    assert_eq!(answered, 1);
    let invocations: i64 = conn
        .query_row("SELECT count(*) FROM invocations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(invocations, 0);
}

#[tokio::test]
async fn hook_timeout_closes_unanswered_consent_without_denial_or_orphan() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env, true);
    let mut events = f.events();
    let caps = Capabilities::parse(&["process:fixture-no-execution".into()]).unwrap();
    let name = "fixture_hook_consent";
    f.daemon()
        .with_store(|st| {
            st.create_tool_version(
                name,
                "Never execute without consent",
                "{\"type\":\"object\"}",
                "{}",
                "function run(i) { return true; }",
                &caps,
                64,
                "user",
            )
        })
        .unwrap();
    let hook = crate::config::HookCfg {
        run_tool: Some(name.into()),
        timeout_secs: Some(1),
        ..Default::default()
    };
    let cfg = f.daemon().config().unwrap();
    let payload = json!({"event":"pre_tool"});
    let cancel = CancellationToken::new();
    let result = f
        .daemon()
        .run_hook(&hook, &payload, &f.session, "fixture", &cfg, &cancel);
    tokio::pin!(result);
    assert!(futures_util::poll!(&mut result).is_pending());
    let Event::ApprovalRequested { approval_id, .. } = next_approval(&mut events).await else {
        unreachable!()
    };
    // This is the product's configured deadline, not a scheduling sleep.
    let verdict = tokio::time::timeout(std::time::Duration::from_secs(5), result)
        .await
        .unwrap();
    assert_eq!(verdict, (false, format!("hook tool {name} timed out")));
    assert!(f.daemon().approvals.locked().is_empty());
    assert!(f.daemon().approval_paths.locked().is_empty());
    assert!(f.daemon().approval_by.locked().is_empty());
    assert!(f.daemon().deny_always.locked().is_empty());
    let late = f
        .daemon()
        .handle(Request::ApprovalDecision {
            approval_id: approval_id.clone(),
            approved: false,
            scope: ApprovalScope::Always,
            paths: Default::default(),
            remember_denial: true,
        })
        .await
        .unwrap();
    assert_eq!(late["resolved"], false);
    let mut closed = 0;
    while let Ok(message) = events.try_recv() {
        match message {
            ServerMessage::Event(Event::ApprovalClosed { approval_id: id })
                if id == approval_id =>
            {
                closed += 1
            }
            ServerMessage::Event(Event::ApprovalAnswered {
                approval_id: id, ..
            }) if id == approval_id => panic!("timeout is not a denial"),
            _ => {}
        }
    }
    assert_eq!(closed, 1);
    let conn = rusqlite::Connection::open(f.paths.db_file()).unwrap();
    let denied: i64 = conn.query_row("SELECT count(*) FROM audit WHERE action IN ('approval.decision','tool.deny','tool.deny_always')", [], |r| r.get(0)).unwrap();
    assert_eq!(denied, 0);
    let invocations: i64 = conn
        .query_row("SELECT count(*) FROM invocations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(invocations, 0);
}
