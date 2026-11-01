//! Real daemon cleanup entry points, with private data and no model dispatch.
use super::*;
struct Fixture {
    daemon: Option<Arc<Daemon>>,
    root: std::path::PathBuf,
    victim: std::path::PathBuf,
    session: String,
}
impl Fixture {
    fn new(env: &crate::credential_env::EnvGuard) -> Self {
        env.remove("FORMLESS_SECURE");
        #[cfg(unix)]
        let base = std::path::PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        let root = base.join(format!("fl-ad-{}", uuid::Uuid::new_v4().simple()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.base_url = "http://127.0.0.1:9".into();
        cfg.provider.auto_test_models = false;
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.daemon.delete_undo_secs = 0;
        cfg.save(&paths).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap().id;
        drop(store);
        std::fs::create_dir_all(paths.data_dir.join("artifacts")).unwrap();
        let victim = root.join("outside-victim");
        std::fs::write(&victim, b"private protected victim").unwrap();
        Self {
            daemon: Some(Arc::new(Daemon::new(paths).unwrap())),
            root,
            victim,
            session,
        }
    }
    fn daemon(&self) -> &Arc<Daemon> {
        self.daemon.as_ref().unwrap()
    }
    fn record(&self, path: &std::path::Path) -> String {
        self.daemon()
            .with_store(|st| {
                st.save_artifact(
                    Some(&self.session),
                    path.to_str().unwrap(),
                    "",
                    2_000_000_000,
                )
            })
            .unwrap()
    }
    fn hold(&self, id: &str, path: &std::path::Path) {
        self.daemon()
            .with_store(|st| {
                st.add_pending_attachment(
                    &self.session,
                    &json!({"id":id,"path":path,"name":"private","bytes":2_000_000_000}),
                )
            })
            .unwrap();
    }
    fn assert_victim(&self) {
        assert_eq!(
            std::fs::read(&self.victim).unwrap(),
            b"private protected victim"
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            daemon.shutdown.cancel();
            drop(daemon);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn detach_uses_owned_registry_path_and_never_held_metadata_path() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let owned = f.daemon().paths.data_dir.join("artifacts/owned");
    std::fs::write(&owned, b"owned").unwrap();
    let id = f.record(&owned);
    // A poisoned held path must not replace the real artifact row's identity.
    f.hold(&id, &f.victim);
    let result = f
        .daemon()
        .handle(Request::DetachFile {
            session_id: f.session.clone(),
            id: id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(result["removed"], true);
    f.assert_victim();
    assert!(!owned.exists());
    assert!(f
        .daemon()
        .with_reader(|st| st.get_artifact(&id))
        .unwrap()
        .is_none());
    let id = f.record(&f.victim);
    f.hold(&id, &f.victim);
    f.daemon()
        .handle(Request::DetachFile {
            session_id: f.session.clone(),
            id: id.clone(),
        })
        .await
        .unwrap();
    f.assert_victim();
    assert!(f
        .daemon()
        .with_reader(|st| st.get_artifact(&id))
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn public_session_delete_purges_rows_and_owned_file_but_keeps_foreign_victim() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    f.record(&f.victim);
    let owned = f.daemon().paths.data_dir.join("artifacts/owned");
    std::fs::write(&owned, b"owned").unwrap();
    f.record(&owned);
    let result = f
        .daemon()
        .handle(Request::DeleteSession {
            id: f.session.clone(),
        })
        .await
        .unwrap();
    assert_eq!(result["undo_secs"], 0);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while f
            .daemon()
            .with_reader(|st| st.get_session(&f.session))
            .unwrap()
            .is_some()
        {
            tokio::task::yield_now().await;
        }
        while Arc::strong_count(f.daemon()) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("zero-window Session purge did not finish");
    f.assert_victim();
    assert!(!owned.exists());
    assert!(f
        .daemon()
        .with_reader(|st| st.list_artifacts())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn actual_startup_gc_loop_and_pending_expiry_quarantine_foreign_paths() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let id = f.record(&f.victim);
    f.hold(&id, &f.victim);
    // A held metadata-only path (no artifact row) is not file authority either.
    f.hold("missing-registry-row", &f.victim);
    rusqlite::Connection::open(f.daemon().paths.db_file())
        .unwrap()
        .execute(
            "UPDATE pending_attachments SET created_at='2000-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    let task = tokio::spawn(f.daemon().clone().artifact_gc_loop());
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let done = f
                .daemon()
                .with_reader(|st| {
                    Ok(st.list_artifacts()?.is_empty()
                        && st.pending_attachments(&f.session)?.is_empty())
                })
                .unwrap();
            if done {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    task.abort();
    let _ = task.await;
    result.expect("startup collection and expiry did not run");
    f.assert_victim();
    assert_eq!(
        Arc::strong_count(f.daemon()),
        1,
        "GC task must release the daemon"
    );
}

#[cfg(unix)]
#[test]
fn actual_constructor_refuses_linked_artifact_namespaces_without_outside_cleanup() {
    use std::os::unix::fs::symlink;
    let env = crate::credential_env::EnvGuard::isolated();
    for mode in ["root", "uploads", "nested"] {
        let mut f = Fixture::new(&env);
        let paths = f.daemon().paths.clone();
        drop(f.daemon.take());
        let outside = f.root.join("outside");
        std::fs::create_dir_all(outside.join(".uploads")).unwrap();
        let sentinel = outside.join(".uploads/victim.part");
        std::fs::write(&sentinel, b"outside upload sentinel").unwrap();
        let artifacts = paths.data_dir.join("artifacts");
        match mode {
            "root" => {
                std::fs::remove_dir(&artifacts).unwrap();
                symlink(&outside, &artifacts).unwrap();
            }
            "uploads" => {
                symlink(outside.join(".uploads"), artifacts.join(".uploads")).unwrap();
            }
            _ => {
                std::fs::create_dir(artifacts.join(".uploads")).unwrap();
                symlink(outside.join(".uploads"), artifacts.join(".uploads/child")).unwrap();
            }
        }
        assert!(
            Daemon::new(paths).is_err(),
            "unsafe {mode} namespace must stop startup"
        );
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            b"outside upload sentinel"
        );
        f.assert_victim();
    }
}

#[test]
fn actual_constructor_cleans_regular_stale_parts_without_recursive_deletion() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env);
    let paths = f.daemon().paths.clone();
    drop(f.daemon.take());
    let uploads = paths.data_dir.join("artifacts/.uploads");
    std::fs::create_dir(&uploads).unwrap();
    let stale = uploads.join("stale.part");
    std::fs::write(&stale, b"unfinished private upload").unwrap();
    let daemon = Daemon::new(paths).unwrap();
    assert!(!stale.exists());
    assert!(
        uploads.is_dir(),
        "ordinary directory can remain for subsequent uploads"
    );
    f.assert_victim();
    drop(daemon);
}

#[tokio::test]
async fn artifact_lookup_failure_rolls_back_public_detach_without_losing_records() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let owned = f.daemon().paths.data_dir.join("artifacts/owned");
    std::fs::write(&owned, b"owned").unwrap();
    let id = f.record(&owned);
    f.hold(&id, &owned);
    let before = f
        .daemon()
        .with_reader(|st| st.pending_attachments(&f.session))
        .unwrap();
    // A real SQLite conversion failure must remain an error, not become a
    // supposedly missing row followed by unconditional metadata deletion.
    let connection = rusqlite::Connection::open(f.daemon().paths.db_file()).unwrap();
    connection
        .execute(
            "UPDATE artifacts SET bytes='invalid-counter' WHERE id=?1",
            [&id],
        )
        .unwrap();
    assert!(f
        .daemon()
        .handle(Request::DetachFile {
            session_id: f.session.clone(),
            id: id.clone()
        })
        .await
        .is_err());
    assert_eq!(
        f.daemon()
            .with_reader(|st| st.pending_attachments(&f.session))
            .unwrap(),
        before
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM artifacts WHERE id=?1", [&id], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        1
    );
    assert_eq!(std::fs::read(&owned).unwrap(), b"owned");
    f.assert_victim();
    drop(connection);
}
