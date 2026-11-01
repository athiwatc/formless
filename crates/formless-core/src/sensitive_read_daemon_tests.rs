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
            tx,
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
