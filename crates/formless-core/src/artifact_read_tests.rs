//! Shared readers retain their different ID authority while rejecting registry
//! paths as file capabilities. All data and restored databases are disposable.
use super::*;

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
