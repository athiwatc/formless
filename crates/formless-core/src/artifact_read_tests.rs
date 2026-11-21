//! Shared readers retain their different ID authority while rejecting registry
//! paths as file capabilities. All data and restored databases are disposable.
use super::*;

#[cfg(unix)]
#[path = "topic_orchestrator_tests.rs"]
mod topic_orchestrator_tests;

#[path = "topic_interaction_tests.rs"]
mod topic_interaction_tests;

struct Fixture {
    daemon: Option<Arc<Daemon>>,
    root: std::path::PathBuf,
    paths: Paths,
    session: String,
}
impl Fixture {
    fn new(env: &crate::credential_env::EnvGuard) -> Self {
        env.remove("FORMLESS_SECURE");
        // Unix socket fixtures must fit native sockaddr_un even when TMPDIR is
        // a long validation path. Drop removes this entire private fixture.
        #[cfg(unix)]
        let base = std::path::PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        let root = base.join(format!("fl-ar-{}", uuid::Uuid::new_v4().simple()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let st = Store::open(&paths.db_file()).unwrap();
        let session = st.create_session(None).unwrap().id;
        drop(st);
        std::fs::create_dir_all(paths.data_dir.join("artifacts/nested")).unwrap();
        Self {
            daemon: Some(Arc::new(Daemon::new(paths.clone()).unwrap())),
            root,
            paths,
            session,
        }
    }
    fn daemon(&self) -> &Arc<Daemon> {
        self.daemon.as_ref().unwrap()
    }
    fn record(&self, path: &std::path::Path, bytes: i64, owner: Option<&str>) -> String {
        self.daemon()
            .with_store(|st| st.save_artifact(owner, path.to_str().unwrap(), "", bytes))
            .unwrap()
    }
    async fn assert_denied(&self, id: &str) {
        let agent = self
            .daemon()
            .read_artifact_impl(&self.session, &json!({"id":id}))
            .unwrap_err();
        let client = self
            .daemon()
            .handle(Request::GetArtifact {
                id: id.into(),
                offset: None,
                length: Some(1),
            })
            .await
            .unwrap_err();
        for error in [agent, client] {
            let text = format!("{error:#}");
            assert!(!text.contains("PRIVATE_SENTINEL"));
            assert!(
                !text.contains(self.root.to_str().unwrap()),
                "no private registry path: {text}"
            );
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.daemon.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn topic_approval_preserves_source_fork_and_topic_attachment_access_without_foreign_grants() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let file = f.paths.data_dir.join("artifacts/nested/topic.txt");
    std::fs::write(&file, b"topic attachment").unwrap();
    let id = f.record(&file, 16, Some(&f.session));
    let unrelated = f.record(&file, 16, Some(&f.session));
    let foreign_session = f
        .daemon()
        .with_store(|st| Ok(st.create_session(None)?.id))
        .unwrap();
    let foreign = f.record(&file, 16, Some(&foreign_session));
    let (topic, fork) = f.daemon().with_store(|st| {
        let message = st.append_message_meta(&f.session, "user", "Inspect attached file", Some(json!({
            "attachments":[{"id":id,"path":file,"bytes":16,"mime":"text/plain"},{"id":foreign}]
        }).to_string()))?;
        let fork = st.fork_session(&f.session, None, None)?.unwrap();
        let proposal = TopicProposalRequest {
            parent_topic_id: None,
            source_session_id: f.session.clone(), message_ids: vec![message.id], features: vec![],
            name: "Attachment".into(), workspace: Some(std::env::current_dir()?), outline: vec![],
        };
        let topic = st.propose_topic(&proposal, WorkspaceKind::Git, Some(&"a".repeat(40)))?;
        Ok((st.approve_topic(&topic.id)?, fork))
    }).unwrap();
    let topic_session = topic.topic_session_id.unwrap();
    let page: Value = serde_json::from_str(
        &f.daemon()
            .read_artifact_impl(&topic_session, &json!({"id":id}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page["text"], "topic attachment");
    let access = crate::attachment_access::AttachmentAccess::profile(&f.paths);
    let attachment =
        json!({"id":id,"path":file,"bytes":16,"_source_session":topic_session,"_source_seq":1});
    assert_eq!(access.read(&attachment, 100).unwrap(), b"topic attachment");
    let mut fork_attachment = attachment.clone();
    fork_attachment["_source_session"] = json!(fork.id);
    assert_eq!(
        access.read(&fork_attachment, 100).unwrap(),
        b"topic attachment"
    );
    let mut wrong_seq = attachment.clone();
    wrong_seq["_source_seq"] = json!(2);
    assert!(access.read(&wrong_seq, 100).is_err());
    assert!(f
        .daemon()
        .read_artifact_impl(&topic_session, &json!({"id":unrelated}))
        .is_err());
    assert!(f
        .daemon()
        .read_artifact_impl(&topic_session, &json!({"id":foreign}))
        .is_err());
    f.daemon()
        .with_reader(|st| {
            assert_eq!(
                st.get_artifact(&id)?.unwrap().0.as_deref(),
                Some(f.session.as_str())
            );
            let snapshot = crate::attachment_access::AttachmentAccess::snapshot(
                st,
                &[json!({"_attachments":[attachment,fork_attachment]})],
            )?
            .unwrap();
            assert_eq!(
                snapshot.read(&attachment, 100).unwrap(),
                b"topic attachment"
            );
            assert_eq!(
                snapshot.read(&fork_attachment, 100).unwrap(),
                b"topic attachment"
            );
            assert_eq!(
                st.get_artifact(&unrelated)?.unwrap().0.as_deref(),
                Some(f.session.as_str())
            );
            assert_eq!(
                st.get_artifact(&foreign)?.unwrap().0.as_deref(),
                Some(foreign_session.as_str())
            );
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn text_pages_preserve_both_reader_shapes_defaults_caps_and_grep() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let file = f.paths.data_dir.join("artifacts/nested/pages.txt");
    for bytes in [
        b"".as_slice(),
        b"ASCII\r\nlast\n",
        "ไทย 日本語 e\u{301} 🦀".as_bytes(),
        b"a\xffb\xf0\x9f\x92",
    ] {
        std::fs::write(&file, bytes).unwrap();
        let id = f.record(&file, bytes.len() as i64, Some(&f.session));
        let text = String::from_utf8_lossy(bytes);
        let total = text.chars().count();
        for offset in [0, 1, total, total + 1, usize::MAX] {
            for length in [0, 1, 3, usize::MAX] {
                let client = f
                    .daemon()
                    .handle(Request::GetArtifact {
                        id: id.clone(),
                        offset: Some(offset),
                        length: Some(length),
                    })
                    .await
                    .unwrap();
                let selected: String = text
                    .chars()
                    .skip(offset)
                    .take(length.min(1_000_000))
                    .collect();
                assert_eq!(
                    client,
                    json!({"id":id,"session_id":f.session,"bytes":bytes.len(),"total_chars":total,"offset":offset,"end":offset+selected.chars().count(),"text":selected})
                );
                let agent: Value = serde_json::from_str(
                    &f.daemon()
                        .read_artifact_impl(
                            &f.session,
                            &json!({"id":id,"offset":offset,"length":length}),
                        )
                        .unwrap(),
                )
                .unwrap();
                let selected: String = text
                    .chars()
                    .skip(offset)
                    .take(length.min(100_000))
                    .collect();
                assert_eq!(
                    agent,
                    json!({"id":id,"total_chars":total,"offset":offset,"end":(offset+selected.chars().count()).min(total),"text":selected})
                );
            }
        }
        let empty_grep: Value = serde_json::from_str(
            &f.daemon()
                .read_artifact_impl(&f.session, &json!({"id":id,"grep":""}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(empty_grep["text"], text.as_ref());
        assert!(empty_grep.get("matches").is_none());
    }
    let source = "NeedLE\r\nİSTANBUL\nΟΣ\nος\n日本語\n".to_string()
        + &"Needle🦀".repeat(600)
        + "\n"
        + &"needle\n".repeat(205);
    std::fs::write(&file, &source).unwrap();
    let id = f.record(&file, source.len() as i64, Some(&f.session));
    for query in ["NEEDLE", "İ", "i\u{307}", "ΟΣ", "Σ", "日本", "missing"] {
        let needle = query.to_lowercase();
        let old: Vec<Value> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&needle))
            .take(200)
            .map(|(i, line)| json!({"line":i+1,"text":line.chars().take(500).collect::<String>()}))
            .collect();
        let got: Value = serde_json::from_str(
            &f.daemon()
                .read_artifact_impl(&f.session, &json!({"id":id,"grep":query}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(got, json!({"id":id,"matches":old}));
    }
    let long = "x".repeat(1_000_010);
    std::fs::write(&file, &long).unwrap();
    let id = f.record(&file, long.len() as i64, Some(&f.session));
    for (length, client_len, agent_len) in [
        (None, 200_000, 20_000),
        (Some(usize::MAX), 1_000_000, 100_000),
    ] {
        let client = f
            .daemon()
            .handle(Request::GetArtifact {
                id: id.clone(),
                offset: None,
                length,
            })
            .await
            .unwrap();
        let args = if let Some(length) = length {
            json!({"id":id,"length":length})
        } else {
            json!({"id":id})
        };
        let agent: Value =
            serde_json::from_str(&f.daemon().read_artifact_impl(&f.session, &args).unwrap())
                .unwrap();
        for (value, selected) in [(client, client_len), (agent, agent_len)] {
            assert_eq!(value["text"].as_str().unwrap().len(), selected);
            assert_eq!(value["end"], selected);
            assert_eq!(value["total_chars"], long.len());
        }
    }
}

#[tokio::test]
async fn shared_readers_preserve_utf8_grep_and_exact_session_vs_global_client_authority() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let text = "αβ\nNeedle 🦀\nlast";
    let file = f.paths.data_dir.join("artifacts/nested/owned.txt");
    std::fs::write(&file, text).unwrap();
    let id = f.record(&file, text.len() as i64, Some(&f.session));
    let read: Value = serde_json::from_str(
        &f.daemon()
            .read_artifact_impl(&f.session, &json!({"id":id,"offset":1,"length":4}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        read["text"],
        text.chars().skip(1).take(4).collect::<String>()
    );
    assert_eq!(read["total_chars"], text.chars().count());
    assert_eq!(read["end"], 5);
    let grep: Value = serde_json::from_str(
        &f.daemon()
            .read_artifact_impl(&f.session, &json!({"id":id,"grep":"NEEDLE"}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(grep["matches"], json!([{"line":2,"text":"Needle 🦀"}]));
    let client = f
        .daemon()
        .handle(Request::GetArtifact {
            id: id.clone(),
            offset: Some(1),
            length: Some(4),
        })
        .await
        .unwrap();
    assert_eq!(client["text"], read["text"]);
    assert_eq!(client["total_chars"], read["total_chars"]);
    assert_eq!(client["end"], read["end"]);
    assert!(!client.to_string().contains(file.to_str().unwrap()));
    let foreign = f
        .daemon()
        .with_store(|st| Ok(st.fork_session(&f.session, None, None)?.unwrap().id))
        .unwrap();
    assert!(
        f.daemon()
            .read_artifact_impl(&foreign, &json!({"id":id}))
            .is_err(),
        "fork must not expand agent authority"
    );
    for owner in [Some(foreign.as_str()), None] {
        let id = f.record(&file, text.len() as i64, owner);
        assert!(f
            .daemon()
            .read_artifact_impl(&f.session, &json!({"id":id}))
            .is_err());
        let result = f
            .daemon()
            .handle(Request::GetArtifact {
                id,
                offset: None,
                length: None,
            })
            .await
            .unwrap();
        assert_eq!(
            result["text"], text,
            "authorized client ID remains profile-wide"
        );
    }
}

#[tokio::test]
async fn restored_registry_cannot_authorize_outside_files_and_invalid_owned_targets() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env);
    let outside = f.root.join("private-credential.txt");
    let sentinel = "PRIVATE_SENTINEL_NOT_AN_ARTIFACT";
    std::fs::write(&outside, sentinel).unwrap();
    let id = f.record(&outside, sentinel.len() as i64, Some(&f.session));
    let saved = f.root.join("foreign-profile.db");
    f.daemon().with_store(|st| st.backup_into(&saved)).unwrap();
    drop(f.daemon.take());
    crate::housekeeping::restore(&f.paths, &saved).unwrap();
    f.daemon = Some(Arc::new(Daemon::new(f.paths.clone()).unwrap()));
    assert_eq!(
        f.daemon()
            .with_reader(|st| st.get_artifact(&id))
            .unwrap()
            .unwrap()
            .1,
        outside.to_str().unwrap()
    );
    f.assert_denied(&id).await;
    let directory = f.paths.data_dir.join("artifacts");
    let regular = directory.join("changed.txt");
    std::fs::write(&regular, b"changed").unwrap();
    for (path, size) in [
        (&directory, 0),
        (&regular, -1),
        (&regular, 1),
        (&regular, 100),
    ] {
        let id = f.record(path, size, Some(&f.session));
        f.assert_denied(&id).await;
    }
    let sparse = directory.join("too-large.txt");
    let size = crate::attachment_access::MAX_ARTIFACT_TEXT_BYTES as u64 + 1;
    std::fs::File::create(&sparse)
        .unwrap()
        .set_len(size)
        .unwrap();
    let id = f.record(&sparse, size as i64, Some(&f.session));
    f.assert_denied(&id).await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = directory.join("link.txt");
        symlink(&outside, &link).unwrap();
        let id = f.record(&link, sentinel.len() as i64, Some(&f.session));
        f.assert_denied(&id).await;
        let linked_parent = directory.join("linked-dir");
        symlink(&f.root, &linked_parent).unwrap();
        let id = f.record(
            &linked_parent.join("private-credential.txt"),
            sentinel.len() as i64,
            Some(&f.session),
        );
        f.assert_denied(&id).await;
        let socket = directory.join("nonregular.sock");
        let _socket = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let id = f.record(&socket, 0, Some(&f.session));
        f.assert_denied(&id).await;
    }
    assert_eq!(std::fs::read_to_string(outside).unwrap(), sentinel);
}

fn topic_fixture_proposal(
    f: &Fixture,
    workspace: Option<std::path::PathBuf>,
) -> TopicProposalRequest {
    let message = f
        .daemon()
        .with_store(|st| st.append_message(&f.session, "user", "Process X and Y"))
        .unwrap();
    TopicProposalRequest {
        parent_topic_id: None,
        source_session_id: f.session.clone(),
        message_ids: vec![message.id],
        features: vec![],
        name: "Topic fixture".into(),
        workspace,
        outline: vec![],
    }
}

#[test]
fn topic_workspace_without_git_executable_is_directory() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let proposal = topic_fixture_proposal(&f, Some(f.root.clone()));
    let topic = f
        .daemon()
        .propose_topic_with_git(&proposal, f.root.join("missing-git").as_os_str())
        .unwrap();
    assert_eq!(topic.workspace_kind, WorkspaceKind::Directory);
    assert_eq!(topic.workspace, Some(f.root.canonicalize().unwrap()));
    assert!(topic.base_revision.is_none());
}

#[test]
fn topic_attachment_provenance_follows_nested_moves_and_topic_forks() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let file = f.paths.data_dir.join("artifacts/nested/inherited.txt");
    std::fs::write(&file, b"inherited").unwrap();
    let id = f.record(&file, 9, Some(&f.session));
    let unrelated = f.record(&file, 9, Some(&f.session));
    let (child, fork) = f
        .daemon()
        .with_store(|st| {
            let message = st.append_message_meta(
                &f.session,
                "user",
                "Inspect",
                Some(json!({"attachments":[{"id":id,"path":file,"bytes":9}]}).to_string()),
            )?;
            let mut proposal = TopicProposalRequest {
                parent_topic_id: None,
                source_session_id: f.session.clone(),
                message_ids: vec![message.id],
                features: vec![],
                name: "Parent".into(),
                workspace: Some(f.root.clone()),
                outline: vec![],
            };
            let parent = st.propose_topic(&proposal, WorkspaceKind::Directory, None)?;
            let parent = st.approve_topic(&parent.id)?;
            let session = parent.topic_session_id.unwrap();
            let fork = st.fork_session(&session, None, None)?.unwrap();
            proposal.source_session_id = session;
            proposal.name = "Child".into();
            let child = st.propose_topic(&proposal, WorkspaceKind::Directory, None)?;
            Ok((
                st.approve_topic(&child.id)?.topic_session_id.unwrap(),
                fork.id,
            ))
        })
        .unwrap();
    let access = crate::attachment_access::AttachmentAccess::profile(&f.paths);
    for session in [child, fork] {
        let content = f
            .daemon()
            .read_artifact_impl(&session, &json!({"id":id}))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&content).unwrap()["text"],
            "inherited"
        );
        let attachment =
            json!({"id":id,"path":file,"bytes":9,"_source_session":session,"_source_seq":1});
        assert_eq!(access.read(&attachment, 100).unwrap(), b"inherited");
        f.daemon()
            .with_reader(|st| {
                let snapshot = crate::attachment_access::AttachmentAccess::snapshot(
                    st,
                    &[json!({"_attachments":[attachment]})],
                )?
                .unwrap();
                assert_eq!(snapshot.read(&attachment, 100).unwrap(), b"inherited");
                Ok(())
            })
            .unwrap();
        assert!(f
            .daemon()
            .read_artifact_impl(&session, &json!({"id":unrelated}))
            .is_err());
        let mut wrong = attachment;
        wrong["_source_seq"] = json!(2);
        assert!(access.read(&wrong, 100).is_err());
    }
}

#[tokio::test]
async fn topic_parent_purge_stops_descendant_native_run_before_removal() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let mut proposal = topic_fixture_proposal(&f, None);
    let topic = f.daemon().propose_topic_request(&proposal).unwrap();
    let topic = f
        .daemon()
        .with_store(|st| st.approve_topic(&topic.id))
        .unwrap();
    let parent = topic.topic_session_id.unwrap();
    proposal.source_session_id = parent.clone();
    let child = f.daemon().propose_topic_request(&proposal).unwrap();
    let child = f
        .daemon()
        .with_store(|st| st.approve_topic(&child.id))
        .unwrap();
    let session = child.topic_session_id.unwrap();
    let cancel = CancellationToken::new();
    let flag = Arc::new(AtomicBool::new(false));
    let (steer_tx, _) = mpsc::unbounded_channel();
    f.daemon().runs.locked().insert(
        session.clone(),
        RunCtl {
            run_id: "descendant-run".into(),
            cancel: cancel.clone(),
            cancel_flag: flag.clone(),
            steer_tx,
            steering: Arc::new(Default::default()),
            retry: Arc::new(provider_retry_daemon::ProviderRetry::default()),
        },
    );
    let daemon = f.daemon().clone();
    let observed = cancel.clone();
    let child = session.clone();
    let stopped = tokio::spawn(async move {
        observed.cancelled().await;
        daemon.runs.locked().remove(&child);
    });
    f.daemon().purge_session(&parent).await.unwrap();
    stopped.await.unwrap();
    assert!(cancel.is_cancelled());
    assert!(flag.load(Ordering::SeqCst));
    assert!(f
        .daemon()
        .with_reader(|st| st.get_session(&session))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn topic_surviving_source_purge_retains_moved_attachment_bytes() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let file = f.paths.data_dir.join("artifacts/nested/retained.txt");
    std::fs::write(&file, b"retained attachment").unwrap();
    let id = f.record(&file, 19, Some(&f.session));
    let discarded = f.paths.data_dir.join("artifacts/nested/discarded.txt");
    std::fs::write(&discarded, b"discarded").unwrap();
    let discarded_id = f.record(&discarded, 9, Some(&f.session));
    f.daemon()
        .with_store(|st| {
            let unrelated = st.create_session(None)?;
            assert!(!st.session_references_artifact(&id, &unrelated.id, &f.session)?);
            st.append_message_meta(
                &unrelated.id,
                "user",
                "Malformed metadata",
                Some("{".into()),
            )?;
            assert!(!st.session_references_artifact(&id, &unrelated.id, &f.session)?);
            st.append_message_meta(
                &unrelated.id,
                "user",
                "Forged reference",
                Some(json!({"attachments":[{"id":id,"path":file,"bytes":19}]}).to_string()),
            )?;
            assert!(!st.session_references_artifact(&id, &unrelated.id, &f.session)?);
            Ok(())
        })
        .unwrap();
    let (topic, sibling, ordinary_fork) = f
        .daemon()
        .with_store(|st| {
            let message = st.append_message_meta(
                &f.session,
                "user",
                "Keep this",
                Some(json!({"attachments":[{"id":id,"path":file,"bytes":19}]}).to_string()),
            )?;
            let fork = st.fork_session(&f.session, None, None)?.unwrap();
            let ordinary_fork = st.fork_session(&f.session, None, None)?.unwrap();
            let topic = st.propose_topic(
                &TopicProposalRequest {
                    parent_topic_id: None,
                    source_session_id: f.session.clone(),
                    message_ids: vec![message.id],
                    features: vec![],
                    name: "Independent".into(),
                    workspace: Some(f.root.clone()),
                    outline: vec![],
                },
                WorkspaceKind::Directory,
                None,
            )?;
            let topic = st.approve_topic(&topic.id)?;
            let sibling = st.propose_topic(
                &TopicProposalRequest {
                    parent_topic_id: None,
                    source_session_id: fork.id.clone(),
                    message_ids: vec![st.list_messages(&fork.id)?[0].id.clone()],
                    features: vec![],
                    name: "Another reference".into(),
                    workspace: Some(f.root.clone()),
                    outline: vec![],
                },
                WorkspaceKind::Directory,
                None,
            )?;
            Ok((topic, st.approve_topic(&sibling.id)?, ordinary_fork))
        })
        .unwrap();
    let session = topic.topic_session_id.unwrap();
    f.daemon().purge_session(&f.session).await.unwrap();
    let result: Value = serde_json::from_str(
        &f.daemon()
            .read_artifact_impl(&session, &json!({"id":id}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["text"], "retained attachment");
    let access = crate::attachment_access::AttachmentAccess::profile(&f.paths);
    assert_eq!(
        access
            .read(
                &json!({"id":id,"path":file,"bytes":19,"_source_session":session,"_source_seq":1}),
                100
            )
            .unwrap(),
        b"retained attachment"
    );
    assert!(!discarded.exists());
    assert!(f
        .daemon()
        .with_reader(|st| st.get_artifact(&discarded_id))
        .unwrap()
        .is_none());
    f.daemon().purge_session(&session).await.unwrap();
    assert!(file.exists());
    let sibling_session = sibling.topic_session_id.unwrap();
    assert!(f
        .daemon()
        .read_artifact_impl(&sibling_session, &json!({"id":id}))
        .unwrap()
        .contains("retained attachment"));
    f.daemon().purge_session(&sibling_session).await.unwrap();
    assert!(file.exists());
    assert_eq!(access.read(&json!({"id":id,"path":file,"bytes":19,"_source_session":ordinary_fork.id,"_source_seq":1}),100).unwrap(),b"retained attachment");
    f.daemon().purge_session(&ordinary_fork.id).await.unwrap();
    assert!(!file.exists());
    assert!(f
        .daemon()
        .with_reader(|st| st.get_artifact(&id))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn topic_fixture_workspace_classification_and_start_failure() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let ping = f.daemon().handle(Request::Ping).await.unwrap();
    assert!(ping["features"]
        .as_array()
        .unwrap()
        .contains(&json!("topic_orchestration_v2")));
    let proposal = topic_fixture_proposal(&f, None);
    let topic = f.daemon().propose_topic_request(&proposal).unwrap();
    assert_eq!(topic.workspace_kind, WorkspaceKind::Scratch);
    assert!(topic.workspace.is_none());
    assert!(topic.base_revision.is_none());
    assert!(!f
        .paths
        .data_dir
        .join("topic-workspaces")
        .join(&topic.id)
        .exists());
    assert!(f
        .daemon()
        .handle(Request::StartTopicWorkflow {
            topic_id: topic.id.clone()
        })
        .await
        .is_err());
    let approved: TopicInfo = serde_json::from_value(
        f.daemon()
            .handle(Request::ApproveTopic {
                approval: TopicApprovalRequest {
                    topic_id: topic.id.clone(),
                },
            })
            .await
            .unwrap(),
    )
    .unwrap();
    let workspace = approved.workspace.clone().unwrap();
    assert_eq!(
        workspace,
        f.paths.data_dir.join("topic-workspaces").join(&topic.id)
    );
    assert!(workspace.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&workspace).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        f.daemon()
            .with_store(|st| st.approve_topic(&topic.id))
            .unwrap()
            .workspace,
        Some(workspace)
    );
    let session = approved.topic_session_id.unwrap();
    f.daemon()
        .with_reader(|st| st.ensure_topic_chat_writable(&session))
        .unwrap();
    let message_result = f
        .daemon()
        .handle(Request::SendMessage {
            session_id: session.clone(),
            content: "Steer this topic".into(),
            mode: SendMode::Send,
            client_msg_id: None,
        })
        .await;
    if let Err(error) = message_result {
        assert!(!error.to_string().contains("read-only"));
    }
    for _ in 0..2 {
        let error = f
            .daemon()
            .handle(Request::StartTopicWorkflow {
                topic_id: topic.id.clone(),
            })
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("read-only"));
        let failed = f
            .daemon()
            .with_reader(|st| st.get_topic(&topic.id))
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, TopicStatus::Failed);
        assert_eq!(failed.features[0].status, TopicFeatureStatus::Failed);
        assert!(failed.features[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("orchestrator start failed"));
        assert!(failed.attempts.is_empty());
        let reply = f
            .daemon()
            .handle(Request::RetryTopicFeature {
                retry: TopicFeatureRetryRequest {
                    topic_id: topic.id.clone(),
                    feature_id: failed.features[0].id.clone(),
                },
            })
            .await
            .unwrap();
        assert_eq!(reply["status"], "approved");
        assert!(reply["attempts"].as_array().unwrap().is_empty());
    }
    let directory = f.root.join("directory");
    std::fs::create_dir(&directory).unwrap();
    let topic = f
        .daemon()
        .propose_topic_request(&topic_fixture_proposal(&f, Some(directory.clone())))
        .unwrap();
    assert_eq!(topic.workspace_kind, WorkspaceKind::Directory);
    assert!(topic.base_revision.is_none());
    let directory_topic = f
        .daemon()
        .with_store(|st| st.approve_topic(&topic.id))
        .unwrap();
    assert_eq!(
        directory_topic.workspace,
        Some(directory.canonicalize().unwrap())
    );

    let repo = f.root.join("git");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "--quiet"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "--allow-empty",
        "--quiet",
        "-m",
        "Base",
    ]);
    let base = git(&["rev-parse", "HEAD"]);
    let git_proposal = topic_fixture_proposal(&f, Some(repo.clone()));
    let git_topic = f.daemon().propose_topic_request(&git_proposal).unwrap();
    assert_eq!(git_topic.workspace_kind, WorkspaceKind::Git);
    assert_eq!(git_topic.base_revision.as_deref(), Some(base.as_str()));
    std::fs::write(repo.join("dirty"), "dirty").unwrap();
    assert!(f
        .daemon()
        .propose_topic_request(&git_proposal)
        .unwrap_err()
        .to_string()
        .contains("clean"));
    for path in [
        f.root.join("missing"),
        "relative".into(),
        repo.join("dirty"),
    ] {
        assert!(f
            .daemon()
            .propose_topic_request(&topic_fixture_proposal(&f, Some(path)))
            .is_err());
    }
}

#[tokio::test]
async fn topic_fixture_scoped_mcp_results_conversations_files_and_cancellation() {
    use sha2::{Digest, Sha256};
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let pending = f
        .daemon()
        .propose_topic_request(&topic_fixture_proposal(&f, None))
        .unwrap();
    let topic = f
        .daemon()
        .with_store(|st| st.approve_topic(&pending.id))
        .unwrap();
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let token = "fixture capability".repeat(3);
    let input = topic.workspace.as_ref().unwrap().join("input.txt");
    std::fs::write(&input, b"input data").unwrap();
    let inputs = vec![TopicArtifact::File {
        path: input.clone(),
        sha256: format!("{:x}", Sha256::digest(b"input data")),
    }];
    let attempt = f
        .daemon()
        .with_store(|st| {
            st.begin_topic_attempt(
                &topic.id,
                &topic.features[0].id,
                "Scan",
                StepAccess::ReadOnly,
                HarnessKind::Claude,
                None,
                None,
                &inputs,
                &token,
                topic.workspace.as_ref().unwrap(),
            )
        })
        .unwrap();
    let call = |operation| Request::TopicMcp {
        request: TopicMcpRequest {
            attempt_id: attempt.id.clone(),
            capability_token: token.clone(),
            operation,
        },
    };
    assert!(f
        .daemon()
        .handle(Request::TopicMcp {
            request: TopicMcpRequest {
                attempt_id: attempt.id.clone(),
                capability_token: "wrong".into(),
                operation: TopicMcpOperation::ListConversations
            }
        })
        .await
        .is_err());
    let conversations = f
        .daemon()
        .handle(call(TopicMcpOperation::ListConversations))
        .await
        .unwrap();
    assert!(conversations
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == attempt.conversation_session_id));
    assert!(!conversations
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == f.session));
    assert!(f
        .daemon()
        .handle(call(TopicMcpOperation::ReadConversation {
            session_id: f.session.clone()
        }))
        .await
        .is_err());
    f.daemon()
        .handle(Request::SetTopicContextAccess {
            topic_id: topic.id.clone(),
            allow: true,
        })
        .await
        .unwrap();
    let conversation = f
        .daemon()
        .handle(call(TopicMcpOperation::ReadConversation {
            session_id: f.session.clone(),
        }))
        .await
        .unwrap();
    assert_eq!(conversation["session"]["id"], f.session);
    assert!(f
        .daemon()
        .handle(call(TopicMcpOperation::ReadConversation {
            session_id: "missing".into()
        }))
        .await
        .is_err());
    let message = f
        .daemon()
        .handle(call(TopicMcpOperation::AppendConversation {
            content: "Scanning".into(),
        }))
        .await
        .unwrap();
    assert_eq!(message["session_id"], attempt.conversation_session_id);
    let artifact = f
        .daemon()
        .handle(call(TopicMcpOperation::ReadArtifact { index: 0 }))
        .await
        .unwrap();
    assert_eq!(artifact["base64"], "aW5wdXQgZGF0YQ==");
    assert!(f
        .daemon()
        .handle(call(TopicMcpOperation::ReadArtifact { index: 1 }))
        .await
        .is_err());
    std::fs::write(&input, b"changed").unwrap();
    assert!(f
        .daemon()
        .handle(call(TopicMcpOperation::ReadArtifact { index: 0 }))
        .await
        .is_err());
    for request in [
        Request::SendMessage {
            session_id: attempt.conversation_session_id.clone(),
            content: "No".into(),
            mode: SendMode::Send,
            client_msg_id: None,
        },
        Request::EditMessage {
            session_id: attempt.conversation_session_id.clone(),
            message_id: message["id"].as_str().unwrap().into(),
            content: "No".into(),
        },
        Request::ForkSession {
            id: attempt.conversation_session_id.clone(),
            name: None,
            up_to_seq: None,
        },
    ] {
        assert!(f
            .daemon()
            .handle(request)
            .await
            .unwrap_err()
            .to_string()
            .contains("read-only"));
    }
    let running = f
        .daemon()
        .with_reader(|st| st.get_topic(&topic.id))
        .unwrap()
        .unwrap();
    assert!(f
        .daemon()
        .handle(Request::StartTopicWorkflow {
            topic_id: topic.id.clone()
        })
        .await
        .is_err());
    assert_eq!(
        f.daemon()
            .with_reader(|st| st.get_topic(&topic.id))
            .unwrap()
            .unwrap(),
        running
    );

    let output = f
        .paths
        .data_dir
        .join("topic-attempts")
        .join(&attempt.id)
        .join("outputs/report.txt");
    std::fs::write(&output, b"report").unwrap();
    let hash = format!("{:x}", Sha256::digest(b"report"));
    for (path, sha256) in [
        (input.clone(), hash.clone()),
        (output.clone(), "wrong".into()),
    ] {
        assert!(f
            .daemon()
            .handle(call(TopicMcpOperation::ReportResult {
                status: TopicAttemptStatus::Succeeded,
                summary: "Report".into(),
                outputs: vec![TopicArtifact::File { path, sha256 }]
            }))
            .await
            .is_err());
    }
    let outputs = vec![
        TopicArtifact::File {
            path: output,
            sha256: hash,
        },
        TopicArtifact::Text {
            title: "Summary".into(),
            content: "Done".into(),
        },
    ];
    let report: TopicAttemptInfo = serde_json::from_value(
        f.daemon()
            .handle(call(TopicMcpOperation::ReportResult {
                status: TopicAttemptStatus::Succeeded,
                summary: "Done".into(),
                outputs: outputs.clone(),
            }))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(report.outputs, outputs);
    assert_eq!(report.summary.as_deref(), Some("Done"));
    assert!(f
        .daemon()
        .handle(call(TopicMcpOperation::AppendConversation {
            content: "stale".into()
        }))
        .await
        .is_err());
    let second = f
        .daemon()
        .with_store(|st| {
            st.begin_topic_attempt(
                &topic.id,
                &topic.features[0].id,
                "Accounting",
                StepAccess::Write,
                HarnessKind::Codex,
                None,
                None,
                &[],
                &token,
                topic.workspace.as_ref().unwrap(),
            )
        })
        .unwrap();
    let cancelled: TopicInfo = serde_json::from_value(
        f.daemon()
            .handle(Request::CancelTopic {
                topic_id: topic.id.clone(),
            })
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cancelled.status, TopicStatus::Cancelled);
    assert_eq!(
        cancelled
            .attempts
            .iter()
            .find(|a| a.id == second.id)
            .unwrap()
            .status,
        TopicAttemptStatus::Cancelled
    );
    assert!(f
        .daemon()
        .with_store(|st| st.authenticate_topic_attempt(&second.id, &token))
        .is_err());
}
