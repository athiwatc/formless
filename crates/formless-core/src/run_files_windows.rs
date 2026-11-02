//! Whole-file namespace rewind reuses the retained-object review protocol.
use super::*;
use crate::review_file_io::{decode_before_metadata, ReviewImage, ReviewPath};
use crate::review_journal::{Journal, Phase, Receipt};
use crate::review_retained::RetainedStore;
use crate::storage::StoredRunFile;
use formless_runtime::sensitive_paths::PathPolicy;
use std::path::Path;

impl Daemon {
    pub(super) fn capture_windows_run_file_witness(&self, run: &str, path: &Path) {
        let capture = (|| -> Result<_> {
            let policy = self.review_policy()?;
            let access = ReviewPath::open(path, &policy)?;
            let retained = RetainedStore::open(&self.paths, false)?;
            match retained {
                Some(retained) => access.read_whole_file_tracked(&policy, &retained),
                None => access.read_whole_file(&policy),
            }
        })();
        let key = (run.into(), path.to_string_lossy().into_owned());
        let mut proofs = self.windows_run_file_witnesses.locked();
        let now = std::time::Instant::now();
        proofs.retain(|_, (_, deadline)| *deadline > now);
        // Remove an older post-image's capability even when the next capture
        // fails or becomes missing. Never reuse authority from an earlier write.
        proofs.remove(&key);
        if let Ok(mut image) = capture {
            if image.bytes.is_some() && proofs.len() < crate::review_retained::MAX_RECORDS {
                image.bytes = None;
                image.security = None;
                proofs.insert(
                    key,
                    (
                        image,
                        now + std::time::Duration::from_secs(
                            crate::review_retained::RETENTION_SECONDS,
                        ),
                    ),
                );
            }
        }
    }
    pub(super) fn attach_windows_run_witness(
        &self,
        run: &str,
        path: &str,
        live: &mut ReviewImage,
    ) -> Result<()> {
        let mut proofs = self.windows_run_file_witnesses.locked();
        let now = std::time::Instant::now();
        proofs.retain(|_, (_, deadline)| *deadline > now);
        let (proof, deadline) = proofs
            .get(&(run.into(), path.into()))
            .context("Live Run post-image object proof unavailable; recovery is read-only")?;
        if *deadline <= std::time::Instant::now() {
            anyhow::bail!("Live Run post-image object proof expired");
        }
        live.attach_witness(proof)
    }
    pub(super) fn capture_windows_created_directory(&self, run: &str, path: &Path) {
        let capture = (|| -> Result<_> {
            let policy = self.review_policy()?;
            ReviewPath::open(path, &policy)?.capture_windows_directory(&policy)
        })();
        if let Ok(proof) = capture {
            let mut proofs = self.windows_run_directory_proofs.locked();
            proofs.retain(|_, proof| proof.fresh());
            if proofs.len() < crate::review_retained::MAX_RECORDS {
                proofs.insert((run.into(), path.to_string_lossy().into_owned()), proof);
            }
        }
    }
    pub(super) fn verify_windows_created_directory(
        &self,
        run: &str,
        path: &Path,
        policy: &PathPolicy,
    ) -> Result<()> {
        let mut proofs = self.windows_run_directory_proofs.locked();
        proofs.retain(|_, proof| proof.fresh());
        let proof = proofs
            .get(&(run.into(), path.to_string_lossy().into_owned()))
            .context("Live proof of positive directory creation unavailable")?;
        ReviewPath::open(path, policy)?.verify_windows_directory(proof, policy, false)
    }
    pub(super) fn remove_windows_created_directory(
        &self,
        run: &str,
        path: &Path,
        policy: &PathPolicy,
    ) -> Result<()> {
        let mut proofs = self.windows_run_directory_proofs.locked();
        proofs.retain(|_, proof| proof.fresh());
        let key = (run.into(), path.to_string_lossy().into_owned());
        let proof = proofs
            .get(&key)
            .context("Live proof of positive directory creation unavailable")?;
        ReviewPath::open(path, policy)?.verify_windows_directory(proof, policy, true)?;
        proofs.remove(&key);
        Ok(())
    }
    pub(super) fn windows_namespace_reason(
        &self,
        session: &str,
        run: &str,
        row: &StoredRunFile,
    ) -> Result<()> {
        let policy = self.review_policy()?;
        let access = ReviewPath::open(Path::new(&row.path), &policy)?;
        if super::run_files_daemon::entry_kind(Path::new(&row.path)) == "directory" {
            return self.verify_windows_created_directory(run, Path::new(&row.path), &policy);
        }
        let retained = RetainedStore::open(&self.paths, false)?
            .context("Live retained-object recovery authority unavailable")?;
        let before = decode_before_metadata(
            row.before_metadata
                .as_deref()
                .context("Original metadata unavailable")?,
            row.before.as_deref(),
        )?;
        let owner = super::review_daemon::retained_owner(session, run, &row.path);
        let mut live = access.read_whole_file_tracked(&policy, &retained)?;
        if before.bytes.is_some() == live.bytes.is_some() {
            return Ok(());
        }
        retained.ensure_namespace_capability()?;
        if before.bytes.is_none() {
            self.attach_windows_run_witness(run, &row.path, &mut live)?;
            retained
                .lookup_missing(&owner, &before)?
                .context("Private first-absence proof unavailable")?;
            let _ = access.namespace_source_whole(&live, &policy, &retained)?;
        } else {
            let token = retained
                .lookup(&owner, &before)?
                .context("Original retained object unavailable or expired")?;
            let object = retained.open_verified(&token, &owner)?;
            object.require_retained_only()?;
            if !access.namespace_target_matches(
                before
                    .windows_identity
                    .as_ref()
                    .context("Original identity unavailable")?,
                &policy,
            )? {
                anyhow::bail!("Retained object and target must share a volume");
            }
        }
        Ok(())
    }

    /// Admission, settings and Store locks are held by the caller throughout.
    pub(super) fn rewind_windows_namespace(
        &self,
        session: &str,
        run: &str,
        path: &str,
        live: &ReviewImage,
        desired: &ReviewImage,
        policy: &PathPolicy,
        store: &Store,
    ) -> Result<()> {
        let retained = RetainedStore::open(&self.paths, false)?
            .context("Live retained-object recovery authority unavailable")?;
        retained.ensure_namespace_capability()?;
        let owner = super::review_daemon::retained_owner(session, run, path);
        let access = ReviewPath::open(Path::new(path), policy)?;
        if access.read_whole_file_tracked(policy, &retained)? != *live {
            anyhow::bail!("Changed since the Run");
        }
        let mut live = live.clone();
        if live.bytes.is_some() {
            self.attach_windows_run_witness(run, path, &mut live)?;
        }
        let live = &live;
        let mut receipt = Receipt::new_whole_file(session, run, path, live, desired)?;
        let mut prepared = None;
        let mut source = None;
        if live.bytes.is_some() && desired.bytes.is_none() {
            retained
                .lookup_missing(&owner, desired)?
                .context("Private first-absence proof unavailable")?;
            let file = access.namespace_source_whole(live, policy, &retained)?;
            let intent = retained.prepare_move(&owner, &file, live)?;
            receipt.retained = Some(intent.token().clone());
            prepared = Some(intent);
            source = Some(file);
        } else if live.bytes.is_none() && desired.bytes.is_some() {
            let token = retained
                .lookup(&owner, desired)?
                .context("Original retained object unavailable or expired")?;
            let object = retained.open_verified(&token, &owner)?;
            object.require_retained_only()?;
            if object.before() != desired {
                anyhow::bail!("Retained original image differs");
            }
            receipt.retained = Some(token);
        } else {
            anyhow::bail!("Expected an existence-changing rewind");
        }
        let journal =
            Journal::open(&self.paths, true)?.context("Private whole-file journal unavailable")?;
        journal.prepare(&mut receipt)?;
        store.audit("user", "rewind.file", &json!({"change_id":receipt.id,"session_id":session,"run_id":run,"path":path,"before_hash":live.hash(),"after_hash":desired.hash(),"status":"prepared"}).to_string()).context("Audit unavailable; prepared recovery receipt retained; no rewind applied")?;
        let outcome = if let Some(intent) = prepared {
            retained
                .commit_move(
                    intent,
                    source.as_ref().context("Prepared source unavailable")?,
                )
                .map(|_| ())
        } else {
            let object = retained.open_verified(
                receipt
                    .retained
                    .as_ref()
                    .context("Retained original unavailable")?,
                &owner,
            )?;
            access
                .restore_retained_whole(&object, desired, policy, &retained)
                .map(|_| ())
        };
        drop(source);
        match access.read_whole_file_tracked(policy, &retained) {
            Ok(current) if current == *desired => {
                if let Err(error) = outcome {
                    tracing::warn!("rewind applied with reported native error: {error:#}");
                }
                let _ = journal.set_phase(&receipt, Phase::Applied);
                Ok(())
            }
            Ok(current) if current == *live => anyhow::bail!(
                "No rewind applied: {}; prepared receipt {} retained",
                outcome
                    .err()
                    .map_or_else(|| "verification failed".into(), |e| e.to_string()),
                receipt.id
            ),
            _ => {
                let _ = journal.set_phase(&receipt, Phase::Ambiguous);
                anyhow::bail!(
                    "Rewind state is ambiguous; no automatic retry; recovery receipt {} retained",
                    receipt.id
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        daemon: Arc<Daemon>,
        root: std::path::PathBuf,
        session: String,
        run: String,
    }
    impl Fixture {
        fn new(env: &crate::credential_env::EnvGuard) -> Self {
            env.set("FORMLESS_API_KEY", "private-windows-run-files");
            env.remove("FORMLESS_BASE_URL");
            env.remove("FORMLESS_MODEL");
            let root =
                std::env::temp_dir().join(format!("fl-win-run-files-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
            let mut cfg = Config::default();
            cfg.provider.base_url = "http://127.0.0.1:9".into();
            cfg.provider.model = "private-offline-fixture".into();
            cfg.naming.enabled = false;
            cfg.skills.auto_create = false;
            cfg.save(&paths).unwrap();
            let daemon = Arc::new(Daemon::new(paths).unwrap());
            let (session, run) = daemon
                .with_store(|st| {
                    let session = st.create_session(None)?.id;
                    let run = st.create_run(&session)?;
                    Ok((session, run))
                })
                .unwrap();
            Self {
                daemon,
                root,
                session,
                run,
            }
        }
        fn post(&self, path: &Path) {
            self.daemon
                .record_run_file_post_image(&self.session, &self.run, path);
        }
        async fn finish(&self) {
            self.daemon
                .finish_run_tail(&self.session, &self.run, "completed", None)
                .await;
        }
        fn rewind(&self) -> (Vec<String>, Vec<Value>) {
            self.daemon
                .rewind_run_files(&self.session, &self.run, None)
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn actual_finish_retains_original_object_for_large_deleted_file() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("deleted.txt");
        let bytes = vec![b'a'; 300 * 1024];
        std::fs::write(&path, &bytes).unwrap();
        let policy = f.daemon.review_policy().unwrap();
        let original = ReviewPath::open(&path, &policy)
            .unwrap()
            .read_whole_file(&policy)
            .unwrap();
        f.daemon.snapshot_before_change(&f.session, &f.run, &path);
        std::fs::remove_file(&path).unwrap();
        f.post(&path);
        f.finish().await;
        assert!(
            !f.daemon.retained_capture_leases.locked().is_empty(),
            "actual terminal status must retain verified whole-file authority"
        );
        let retained = RetainedStore::open(&f.daemon.paths, false)
            .unwrap()
            .unwrap();
        assert_eq!(retained.expire(u64::MAX).unwrap(), 0);
        let result = f.rewind();
        assert_eq!(result.0.len(), 1, "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let restored = ReviewPath::open(&path, &policy)
            .unwrap()
            .read_whole_file_tracked(&policy, &retained)
            .unwrap();
        assert_eq!(restored.windows_identity, original.windows_identity);
        assert_eq!(restored.security, original.security);
    }

    #[tokio::test]
    async fn actual_finish_created_file_uses_private_absence_marker() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("created.txt");
        f.daemon.snapshot_before_change(&f.session, &f.run, &path);
        std::fs::write(&path, vec![b'c'; 300 * 1024]).unwrap();
        f.post(&path);
        f.finish().await;
        let result = f.rewind();
        assert_eq!(result.0.len(), 1, "{result:?}");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn replacement_object_with_identical_bytes_cannot_be_removed() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("created.txt");
        f.daemon.snapshot_before_change(&f.session, &f.run, &path);
        std::fs::write(&path, b"same bytes").unwrap();
        f.post(&path);
        f.finish().await;
        std::fs::rename(&path, f.root.join("original-object.txt")).unwrap();
        std::fs::write(&path, b"same bytes").unwrap();
        let result = f.rewind();
        assert!(result.0.is_empty(), "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"same bytes");
    }

    #[tokio::test]
    async fn positive_directory_creation_requires_live_object_and_empty_directory() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("created-dir");
        std::fs::create_dir(&path).unwrap();
        f.daemon
            .snapshot_created_directory(&f.session, &f.run, &path);
        f.finish().await;
        std::fs::write(path.join("external.txt"), b"keep").unwrap();
        let result = f.rewind();
        assert!(result.0.is_empty());
        assert_eq!(std::fs::read(path.join("external.txt")).unwrap(), b"keep");
        std::fs::remove_file(path.join("external.txt")).unwrap();
        let result = f.rewind();
        assert_eq!(result.0.len(), 1, "{result:?}");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn replaced_directory_and_lost_live_proof_remain_read_only() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("created-dir");
        std::fs::create_dir(&path).unwrap();
        f.daemon
            .snapshot_created_directory(&f.session, &f.run, &path);
        f.finish().await;
        std::fs::rename(&path, f.root.join("old-dir")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(f.rewind().0.is_empty());
        assert!(path.is_dir());
        f.daemon.windows_run_directory_proofs.locked().clear();
        assert!(f.rewind().0.is_empty());
        assert!(path.is_dir());
    }

    #[tokio::test]
    async fn changed_attributes_are_not_repaired_to_make_namespace_rewind_succeed() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("created.txt");
        f.daemon.snapshot_before_change(&f.session, &f.run, &path);
        std::fs::write(&path, b"private").unwrap();
        f.post(&path);
        f.finish().await;
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert!(f.rewind().0.is_empty());
        assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
        assert_eq!(std::fs::read(&path).unwrap(), b"private");
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
    }
    #[tokio::test]
    async fn later_acl_change_is_preserved_and_never_reset_by_rewind() {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Security::{
            Authorization::{SetSecurityInfo, SE_FILE_OBJECT},
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        };
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("acl-created.txt");
        f.daemon.snapshot_before_change(&f.session, &f.run, &path);
        std::fs::write(&path, b"private fixture").unwrap();
        f.post(&path);
        f.finish().await;
        // Test-only external ACL mutation. Production namespace paths never
        // request WRITE_DAC and never invoke a security-descriptor setter.
        let file = std::fs::OpenOptions::new()
            .access_mode(0x0004_0000)
            .share_mode(7)
            .open(&path)
            .unwrap();
        let result = unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert_eq!(result, 0);
        drop(file);
        let policy = f.daemon.review_policy().unwrap();
        let changed = ReviewPath::open(&path, &policy)
            .unwrap()
            .read_whole_file(&policy)
            .unwrap();
        let result = f.rewind();
        assert!(result.0.is_empty(), "{result:?}");
        let current = ReviewPath::open(&path, &policy)
            .unwrap()
            .read_whole_file(&policy)
            .unwrap();
        assert_eq!(current.security, changed.security);
        assert_eq!(current.bytes, changed.bytes);
    }
    #[tokio::test]
    async fn private_absence_marker_without_live_created_object_proof_is_read_only() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("created.txt");
        f.daemon.snapshot_before_change(&f.session, &f.run, &path);
        std::fs::write(&path, b"keep this object").unwrap();
        f.post(&path);
        f.finish().await;
        // Simulates loss of process-local authority on restart. Database and
        // the private absence marker remain, but cannot recreate a live pin.
        f.daemon.windows_run_file_witnesses.locked().clear();
        let result = f.rewind();
        assert!(result.0.is_empty(), "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"keep this object");
    }
    struct RuntimeCapture {
        daemon: Arc<Daemon>,
        session: String,
        run: String,
    }
    impl formless_runtime::HostOps for RuntimeCapture {
        fn path_policy(
            &self,
        ) -> std::result::Result<formless_runtime::sensitive_paths::PathPolicy, String> {
            self.daemon
                .review_policy()
                .map_err(|error| error.to_string())
        }
        fn file_changing_resolved(&self, requested: &Path, resolved: &Path, _operation: &str) {
            self.daemon.snapshot_before_change_resolved(
                &self.session,
                &self.run,
                requested,
                resolved,
            );
        }
        fn file_changed(&self, requested: &Path, resolved: &Path, _operation: &str) {
            let recorded = if formless_runtime::caps::same_path_spelling(requested, resolved) {
                resolved
            } else {
                requested
            };
            self.daemon
                .record_run_file_post_image(&self.session, &self.run, recorded);
        }
        fn directory_created(&self, path: &Path) {
            self.daemon
                .snapshot_created_directory(&self.session, &self.run, path);
        }
    }

    #[tokio::test]
    async fn runtime_ordinary_drive_spelling_preserves_namespace_rewind_authority() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let canonical = f.root.to_str().unwrap();
        let ordinary = canonical
            .strip_prefix(r"\\?\")
            .expect("canonical Windows fixture must use a verbatim drive prefix");
        let root = std::path::PathBuf::from(ordinary);
        let deleted = root.join("deleted.txt");
        std::fs::write(&deleted, vec![b'd'; 300 * 1024]).unwrap();
        let policy = f.daemon.review_policy().unwrap();
        let original = ReviewPath::open(&deleted, &policy)
            .unwrap()
            .read_whole_file(&policy)
            .unwrap();
        let ops = Arc::new(RuntimeCapture {
            daemon: f.daemon.clone(),
            session: f.session.clone(),
            run: f.run.clone(),
        });
        let request = formless_runtime::ExecRequest {
            code: "function run(i){ fs.write(i.root+'/created.txt','c'.repeat(300*1024)); fs.mkdir(i.root+'/created-dir'); fs.remove(i.root+'/deleted.txt'); return true; }".into(),
            input: json!({"root":ordinary}),
            caps: formless_runtime::Capabilities::parse(&[format!("fs:write:{}",root.display()),format!("fs:read:{}",root.display())]).unwrap(),
            memory_mb:256,mode:formless_runtime::HostMode::Live,output_schema:None,overlay:None,activity:None,modules:Default::default(),cancel:Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        formless_runtime::execute(&request, ops).unwrap();
        f.finish().await;
        let files = f.daemon.run_file_infos(&f.session, &f.run, false).unwrap();
        assert_eq!(files.len(), 3, "{files:?}");
        assert!(files.iter().all(|file| file.revertible), "{files:?}");
        let result = f.rewind();
        assert_eq!(result.0.len(), 3, "{result:?}");
        assert!(!root.join("created.txt").exists());
        assert!(!root.join("created-dir").exists());
        assert_eq!(std::fs::read(&deleted).unwrap(), vec![b'd'; 300 * 1024]);
        let retained = RetainedStore::open(&f.daemon.paths, false)
            .unwrap()
            .unwrap();
        let restored = ReviewPath::open(&deleted, &policy)
            .unwrap()
            .read_whole_file_tracked(&policy, &retained)
            .unwrap();
        assert_eq!(restored.windows_identity, original.windows_identity);
        assert_eq!(restored.security, original.security);
    }
}
