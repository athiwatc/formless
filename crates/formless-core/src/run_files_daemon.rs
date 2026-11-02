//! Whole-file recovery uses owned records, attributable post-images and anchored
//! compare-and-swap I/O. It never guesses authority from an old before-image.
use super::*;
use crate::review_file_io::{
    decode_before_metadata, encode_before_metadata, ReviewImage, ReviewPath,
};
use crate::review_journal::{Journal, Phase, Receipt};
use crate::storage::StoredRunFile;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AfterState {
    version: u8,
    kind: String,
    sha256: Option<String>,
    size: Option<u64>,
    metadata: Option<String>,
    directory_identity: Option<(u64, u64)>,
    reason: Option<String>,
}
impl AfterState {
    fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            version: 1,
            kind: "unsupported".into(),
            sha256: None,
            size: None,
            metadata: None,
            directory_identity: None,
            reason: Some(reason.into()),
        }
    }
    fn from_image(image: &ReviewImage) -> Result<Self> {
        use sha2::{Digest, Sha256};
        let metadata = encode_before_metadata(image)?;
        if metadata.len() > 6 * 1024 {
            anyhow::bail!("After-state security metadata exceeds the recovery proof limit");
        }
        Ok(Self {
            version: 1,
            kind: if image.bytes.is_some() {
                "file"
            } else {
                "missing"
            }
            .into(),
            sha256: image
                .bytes
                .as_ref()
                .map(|bytes| format!("{:x}", Sha256::digest(bytes))),
            size: image.bytes.as_ref().map(|bytes| bytes.len() as u64),
            metadata: Some(metadata),
            directory_identity: None,
            reason: None,
        })
    }
}

pub(super) fn entry_kind(path: &Path) -> &'static str {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => "symlink",
        Ok(metadata) if metadata.is_file() => "file",
        Ok(metadata) if metadata.is_dir() => "directory",
        Ok(_) => "other",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        Err(_) => "unknown",
    }
}

impl Daemon {
    fn read_run_file_image(
        &self,
        path: &Path,
        policy: &formless_runtime::sensitive_paths::PathPolicy,
    ) -> Result<ReviewImage> {
        let access = ReviewPath::open(path, policy)?;
        #[cfg(windows)]
        if let Some(retained) =
            crate::review_retained::RetainedStore::open(&self.paths, false)?.as_ref()
        {
            return access.read_whole_file_tracked(policy, retained);
        }
        access.read_whole_file(policy)
    }
    fn observe_run_file(&self, path: &Path) -> Result<AfterState> {
        let policy = self.review_policy()?;
        let access = ReviewPath::open(path, &policy)?;
        if entry_kind(path) == "directory" {
            return Ok(AfterState {
                version: 1,
                kind: "directory".into(),
                sha256: None,
                size: None,
                metadata: None,
                directory_identity: Some(access.directory_identity(&policy)?),
                reason: None,
            });
        }
        // Single-link, ordinary metadata and a final no-follow entry are required.
        AfterState::from_image(&self.read_run_file_image(path, &policy)?)
    }

    pub(super) fn mark_run_untracked_writes(&self, session: &str, run: &str) {
        if !run.is_empty() {
            if let Err(error) = self.with_store(|st| st.mark_run_untracked_writes(session, run)) {
                tracing::warn!("could not retain Run write-coverage receipt: {error:#}");
            }
        }
    }

    pub(super) fn snapshot_before_change_resolved(
        &self,
        session: &str,
        run: &str,
        requested: &Path,
        resolved: &Path,
    ) {
        if formless_runtime::caps::same_path_spelling(requested, resolved) {
            self.snapshot_before_change(session, run, resolved);
            return;
        }
        let (Some(shown), Ok(cfg)) = (requested.to_str(), self.config()) else {
            return;
        };
        // A canonical target does not authorize restoring through an alias that
        // may later point elsewhere. Keep its requested spelling and explain it.
        let _ = self.with_store(|st| st.snapshot_run_file_capped(session, run, shown, true, None, None,
            "symlink", Some("Changes through a symlink or path alias are not revertible; the resolved target was not recorded against this route"), &cfg.run_files).map(|_| ()));
    }

    pub(super) fn snapshot_created_directory(&self, session: &str, run: &str, path: &Path) {
        let (Some(shown), Ok(cfg)) = (path.to_str(), self.config()) else {
            return;
        };
        // Called only after create_dir succeeds for this exact name, never when
        // another creator won the race. No preexisting directory is claimed.
        let missing = encode_before_metadata(&ReviewImage::missing()).ok();
        let admitted = self.with_store(|st| {
            st.snapshot_run_file_capped(
                session,
                run,
                shown,
                false,
                None,
                missing.as_deref(),
                "missing",
                None,
                &cfg.run_files,
            )
        });
        if admitted.is_ok_and(|admission| admission.kept) {
            #[cfg(windows)]
            self.capture_windows_created_directory(run, path);
            self.record_run_file_post_image(session, run, path);
        }
    }

    pub(super) fn record_run_file_post_image(&self, session: &str, run: &str, path: &Path) {
        if run.is_empty() {
            return;
        }
        let Some(shown) = path.to_str() else {
            return;
        };
        // Cap admission bounds the live proof map as well as retained bodies.
        let eligible = self
            .with_reader(|st| {
                if st.run_status(run)?.as_deref() != Some("running") {
                    return Ok(false);
                }
                Ok(st.run_file_records(session, run)?.iter().any(|row| {
                    row.path == shown
                        && row.kept
                        && row.before_kind.is_some()
                        && row.after_state.is_none()
                }))
            })
            .unwrap_or(false);
        if !eligible {
            return;
        }
        // Pin the Windows object before token capture: reopening a same-ID
        // pathname later cannot resurrect authority after object replacement.
        #[cfg(windows)]
        self.capture_windows_run_file_witness(run, path);
        let state = self
            .observe_run_file(path)
            .unwrap_or_else(|error| AfterState::unsupported(error.to_string()));
        if let Ok(token) = serde_json::to_string(&state) {
            self.run_file_post_images
                .locked()
                .insert((run.into(), shown.into()), token);
        }
    }

    pub(super) fn finish_run_file_capture(&self, session: &str, run: &str) {
        // Admission and Store lock order matches review commit. Capture finishes
        // while the Run is still active; no subsequent Run may acquire its proof.
        let _admission = self.forecast_admission.locked();
        let store = self.store.locked();
        let Ok(rows) = store.run_file_records(session, run) else {
            return;
        };
        let mut proofs = self.run_file_post_images.locked();
        let (live, completed) = self.review_pending_status(Some(session));
        let pending = live
            || store
                .review_session_busy(session, &completed)
                .unwrap_or(true);
        for row in rows {
            let proof = proofs.remove(&(run.into(), row.path.clone()));
            if row.before_kind.is_none() || row.after_state.is_some() || !row.kept {
                continue;
            }
            let state = if pending {
                AfterState::unsupported("Background Invocations were still active at Run end; after-state attribution is unavailable")
            } else if let Some(proof) = proof {
                match serde_json::from_str::<AfterState>(&proof) {
                    Ok(previous) if previous.kind == "unsupported" => previous,
                    Ok(previous) => match self.observe_run_file(Path::new(&row.path)) {
                        Ok(current) if current == previous => current,
                        _ => AfterState::unsupported("Changed since the Run's last tracked write; after-state attribution is unavailable"),
                    },
                    Err(_) => AfterState::unsupported("Invalid tracked post-image; no recovery authority"),
                }
            } else {
                AfterState::unsupported("No successful tracked write supplied an after-state; recovery proof is unavailable")
            };
            if let Ok(token) = serde_json::to_string(&state) {
                if let Err(error) = store.set_run_file_after_state(session, run, &row.path, &token)
                {
                    tracing::warn!("could not freeze Run file after-state: {error:#}");
                }
            }
        }
        proofs.retain(|(owner, _), _| owner != run);
    }

    fn run_file_revert_reason(&self, row: &StoredRunFile) -> Option<String> {
        if !row.kept {
            return Some(
                row.not_kept_reason
                    .clone()
                    .unwrap_or_else(|| "Its earlier content was not kept".into()),
            );
        }
        let token = row.after_state.as_ref()?;
        if row.before_metadata.is_none() {
            return Some("Original security metadata was not kept; recovery is read-only".into());
        }
        let state: AfterState = match serde_json::from_str::<AfterState>(token) {
            Ok(state) if state.version == 1 => state,
            _ => return Some("The recorded after-state is unsupported or invalid".into()),
        };
        if let Some(reason) = &state.reason {
            return Some(reason.clone());
        }
        match self.observe_run_file(Path::new(&row.path)) {
            Ok(current) if current == state => None,
            _ => Some(
                "Changed since the Run; current content, size, type or metadata differs".into(),
            ),
        }
    }

    pub(super) fn run_file_infos(
        &self,
        session: &str,
        run: &str,
        diffs: bool,
    ) -> Result<Vec<RunFileInfo>> {
        let rows = self.with_reader(|st| st.run_file_records(session, run))?;
        let mut files = Vec::with_capacity(rows.len());
        let mut diff_budget = 1024 * 1024usize;
        for header in rows {
            // Load one bounded image at a time; historical Runs can have far
            // more files than the current capture cap. Never collect their BLOBs.
            let row = self.with_reader(|st| st.run_file_record(session, run, &header.path))?;
            let kind = entry_kind(Path::new(&row.path)).to_owned();
            let mut reason = self.run_file_revert_reason(&row);
            if row.after_state.is_none() && reason.is_none() {
                reason = Some("No verified Run-end after-state is available; legacy or unfinished snapshots are read-only".into());
            }
            if reason.is_none()
                && kind == "directory"
                && (row.existed || row.before_kind.as_deref() != Some("missing"))
            {
                reason =
                    Some("Only directories positively created by this Run can be removed".into());
            }
            #[cfg(windows)]
            if reason.is_none() && (kind == "directory" || row.existed != (kind != "missing")) {
                reason = self
                    .windows_namespace_reason(session, run, &row)
                    .err()
                    .map(|error| error.to_string());
            }
            #[cfg(windows)]
            if reason.is_none() && kind == "file" {
                reason = (|| -> Result<()> {
                    let policy = self.review_policy()?;
                    let mut live = self.read_run_file_image(Path::new(&row.path), &policy)?;
                    self.attach_windows_run_witness(run, &row.path, &mut live)
                })()
                .err()
                .map(|error| error.to_string());
            }
            let now = if kind == "file" {
                self.review_policy().ok().and_then(|policy| {
                    self.read_run_file_image(Path::new(&row.path), &policy)
                        .ok()?
                        .bytes
                })
            } else {
                None
            };
            let status = match (row.existed, kind.as_str()) {
                (false, "missing") => "unchanged",
                (false, _) => "created",
                (true, "missing") => "removed",
                (true, "file") if row.kept && row.before == now => "unchanged",
                _ => "modified",
            };
            let diff = if diffs && row.kept && status != "unchanged" && kind != "directory" {
                if diff_budget == 0
                    || row
                        .before
                        .as_ref()
                        .map_or(0, Vec::len)
                        .saturating_add(now.as_ref().map_or(0, Vec::len))
                        > 256 * 1024
                {
                    Some("(diff omitted: bounded listing limit; whole-file recovery is still available when verified)".into())
                } else {
                    let mut diff = match (
                        row.before.as_deref().map(std::str::from_utf8).transpose(),
                        now.as_deref().map(std::str::from_utf8).transpose(),
                    ) {
                        (Ok(a), Ok(b)) => {
                            crate::diff::context_diff(a.unwrap_or(""), b.unwrap_or("")).join("\n")
                        }
                        _ => "(binary file)".into(),
                    };
                    if diff.len() > diff_budget {
                        diff = "(diff omitted: aggregate listing limit)".into();
                        diff_budget = 0;
                    } else {
                        diff_budget -= diff.len();
                    }
                    Some(diff)
                }
            } else {
                None
            };
            files.push(RunFileInfo {
                path: row.path,
                status: status.into(),
                revertible: reason.is_none(),
                diff,
                kind,
                revert_reason: reason,
            });
        }
        Ok(files)
    }

    pub(super) fn rewind_run_files(
        &self,
        session: &str,
        run: &str,
        path: Option<&str>,
    ) -> Result<(Vec<String>, Vec<Value>)> {
        let _admission = self.forecast_admission.locked();
        let _settings = self.config_guard();
        let policy = self.review_policy()?;
        // Keep the mutex through every mutation: Run/Invocation admission uses
        // this lock too. This deliberately excludes the entire active profile.
        let store = self.store.locked();
        self.review_profile_commit_idle(&store)?;
        let rows = store.run_file_records(session, run)?;
        let picked: Vec<_> = rows
            .into_iter()
            .filter(|row| {
                path.is_none_or(|p| {
                    row.path == p
                        || row.path.ends_with(&format!("/{p}"))
                        || row.path.ends_with(&format!("\\{p}"))
                })
            })
            .collect();
        if path.is_some() && picked.is_empty() {
            anyhow::bail!("that Run did not change the requested path");
        }
        let (mut restored, mut skipped) = (Vec::new(), Vec::new());
        // Deeper paths first, so newly created directories are empty after their
        // children have been recovered. No recursive deletion is ever attempted.
        let mut picked = picked;
        picked.sort_by_key(|row| std::cmp::Reverse(Path::new(&row.path).components().count()));
        for row in picked {
            let outcome = (|| -> Result<()> {
                let row = store.run_file_record(session, run, &row.path)?;
                if !row.kept {
                    anyhow::bail!(
                        "{}",
                        row.not_kept_reason
                            .as_deref()
                            .unwrap_or("Its earlier content was not kept")
                    );
                }
                let token = row.after_state.as_deref().context("No verified Run-end after-state is available; legacy or unfinished snapshots are read-only")?;
                let state: AfterState = serde_json::from_str(token)?;
                if state.version != 1 {
                    anyhow::bail!("Unsupported after-state proof");
                }
                if let Some(reason) = &state.reason {
                    anyhow::bail!("{reason}");
                }
                let observed = self
                    .observe_run_file(Path::new(&row.path))
                    .context("Changed since the Run; the path type or state cannot be verified")?;
                if observed != state {
                    anyhow::bail!(
                        "Changed since the Run; current content, size, type or metadata differs"
                    );
                }
                let access = ReviewPath::open(Path::new(&row.path), &policy)?;
                if state.kind == "directory" {
                    if row.existed || row.before_kind.as_deref() != Some("missing") {
                        anyhow::bail!(
                            "Only directories positively created by this Run can be removed"
                        );
                    }
                    store.audit("user","rewind.directory",&json!({"session_id":session,"run_id":run,"path":row.path,"status":"prepared"}).to_string()).context("Audit unavailable; directory was not removed")?;
                    #[cfg(windows)]
                    self.remove_windows_created_directory(run, Path::new(&row.path), &policy)?;
                    #[cfg(not(windows))]
                    access.remove_created_directory(
                        state
                            .directory_identity
                            .context("Directory identity proof is missing")?,
                        &policy,
                    )?;
                } else {
                    if !matches!(state.kind.as_str(), "file" | "missing") {
                        anyhow::bail!("Unsupported path type");
                    }
                    let live = self.read_run_file_image(Path::new(&row.path), &policy)?;
                    #[cfg(windows)]
                    let live = {
                        let mut live = live;
                        if live.bytes.is_some() {
                            self.attach_windows_run_witness(run, &row.path, &mut live)?;
                        }
                        live
                    };
                    if AfterState::from_image(&live)? != state {
                        anyhow::bail!("Changed since the Run");
                    }
                    let metadata = row.before_metadata.as_deref().context(
                        "Original security metadata was not kept; recovery is read-only",
                    )?;
                    let mut desired = decode_before_metadata(metadata, row.before.as_deref())?;
                    #[cfg(windows)]
                    let namespace = desired.bytes.is_some() != live.bytes.is_some();
                    #[cfg(not(windows))]
                    let namespace = false;
                    if namespace {
                        #[cfg(windows)]
                        self.rewind_windows_namespace(
                            session, run, &row.path, &live, &desired, &policy, &store,
                        )?;
                    } else {
                        #[cfg(windows)]
                        desired.attach_witness(&live)?;
                        #[cfg(not(windows))]
                        let _ = &mut desired;
                        let journal = Journal::open(&self.paths, true)?
                            .context("Private whole-file recovery journal unavailable")?;
                        let mut receipt =
                            Receipt::new_whole_file(session, run, &row.path, &live, &desired)?;
                        journal.prepare(&mut receipt)?;
                        let audit = json!({"change_id":receipt.id,"session_id":session,"run_id":run,"path":row.path,"before_hash":live.hash(),"after_hash":desired.hash(),"status":"prepared"});
                        store.audit("user","rewind.file",&audit.to_string()).context("Audit unavailable; no rewind was applied; private prepared receipt retained")?;
                        #[cfg(windows)]
                        let outcome = match crate::review_retained::RetainedStore::open(
                            &self.paths,
                            false,
                        )? {
                            Some(retained) => access
                                .replace_whole_file_tracked(&live, &desired, &policy, &retained),
                            None => access.replace_whole_file(&live, &desired, &policy),
                        };
                        #[cfg(not(windows))]
                        let outcome = access.replace_whole_file(&live, &desired, &policy);
                        match self.read_run_file_image(Path::new(&row.path), &policy) {
                            Ok(current) if current == desired => {
                                if let Err(error) = &outcome {
                                    tracing::warn!("rewind applied but replacement reported an error; private receipt {}: {error:#}",receipt.id);
                                }
                                if let Err(error) = journal.set_phase(&receipt, Phase::Applied) {
                                    tracing::warn!(
                                    "rewind applied but final receipt update failed {}: {error:#}",
                                    receipt.id
                                );
                                }
                            }
                            Ok(current) if current == live => {
                                anyhow::bail!("No rewind was applied: {}; prepared private recovery receipt {} retained",outcome.err().map_or_else(|| "post-image verification failed".into(),|error|error.to_string()),receipt.id);
                            }
                            _ => {
                                let _ = journal.set_phase(&receipt, Phase::Ambiguous);
                                anyhow::bail!("File state may have changed during rewind; no automatic retry or overwrite is allowed; private recovery receipt {} retained",receipt.id);
                            }
                        }
                    }
                }
                store.forget_run_file(run, &row.path)?;
                Ok(())
            })();
            match outcome {
                Ok(()) => restored.push(row.path),
                Err(error) => skipped.push(json!({"path":row.path,"reason":error.to_string()})),
            }
        }
        Ok((restored, skipped))
    }
}

#[cfg(all(test, unix))]
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
            env.set("FORMLESS_API_KEY", "private-run-files-fixture");
            env.remove("FORMLESS_BASE_URL");
            env.remove("FORMLESS_MODEL");
            let root = std::env::temp_dir().join(format!("fl-run-files-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
            let mut cfg = Config::default();
            cfg.provider.base_url = "http://127.0.0.1:9".into();
            cfg.provider.model = "private-local-fixture".into();
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
        fn write(&self, path: &Path, body: &[u8]) {
            self.daemon
                .snapshot_before_change(&self.session, &self.run, path);
            std::fs::write(path, body).unwrap();
            self.daemon
                .record_run_file_post_image(&self.session, &self.run, path);
        }
        fn finish(&self) {
            self.daemon
                .finish_run_file_capture(&self.session, &self.run);
            self.daemon
                .with_store(|st| st.set_run_status(&self.run, "completed"))
                .unwrap();
        }
        async fn rewind(&self) -> Value {
            self.daemon
                .handle(Request::RewindRun {
                    session_id: self.session.clone(),
                    run_id: Some(self.run.clone()),
                    path: None,
                })
                .await
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn rewind_uses_frozen_hash_and_size_and_preserves_later_edits() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("owned.txt");
        std::fs::write(&path, b"before").unwrap();
        f.write(&path, b"Run output");
        f.finish();
        let rows = f
            .daemon
            .with_reader(|st| st.run_file_records(&f.session, &f.run))
            .unwrap();
        let state: AfterState =
            serde_json::from_str(rows[0].after_state.as_deref().unwrap()).unwrap();
        assert_eq!(state.size, Some(10));
        use sha2::{Digest, Sha256};
        assert_eq!(
            state.sha256,
            Some(format!("{:x}", Sha256::digest(b"Run output")))
        );
        std::fs::write(&path, b"user edits").unwrap();
        let result = f.rewind().await;
        assert!(result["restored"].as_array().unwrap().is_empty());
        assert!(result["skipped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("Changed since the Run"));
        assert_eq!(std::fs::read(&path).unwrap(), b"user edits");
    }

    #[tokio::test]
    async fn whole_file_rewind_retains_the_existing_two_mib_limit_and_mode() {
        use std::os::unix::fs::PermissionsExt;
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("large.txt");
        let original = vec![b'a'; 300 * 1024];
        std::fs::write(&path, &original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        f.write(&path, &vec![b'b'; 300 * 1024]);
        f.finish();
        let result = f.rewind().await;
        assert_eq!(result["restored"].as_array().unwrap().len(), 1, "{result}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let again = f.rewind().await;
        assert_eq!(again["skipped"][0]["reason"], "Already rewound");
        assert_eq!(f.daemon.files_run(&f.session, None).unwrap(), f.run);
    }

    #[tokio::test]
    async fn changed_file_type_never_recursively_deletes_a_later_directory() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("created");
        f.write(&path, b"created by Run");
        f.finish();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("later.txt"), b"keep this").unwrap();
        let result = f.rewind().await;
        assert!(result["restored"].as_array().unwrap().is_empty());
        assert_eq!(std::fs::read(path.join("later.txt")).unwrap(), b"keep this");
    }

    #[tokio::test]
    async fn new_directories_remove_only_when_empty_and_the_same_object() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let empty = f.root.join("empty");
        let occupied = f.root.join("occupied");
        for path in [&empty, &occupied] {
            std::fs::create_dir(path).unwrap();
            f.daemon
                .snapshot_created_directory(&f.session, &f.run, path);
        }
        f.finish();
        std::fs::write(occupied.join("new.txt"), b"untracked child").unwrap();
        let result = f.rewind().await;
        assert!(!empty.exists(), "{result}");
        assert_eq!(
            std::fs::read(occupied.join("new.txt")).unwrap(),
            b"untracked child"
        );
        assert_eq!(result["skipped"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn foreign_run_and_another_sessions_active_run_are_refused() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("owned.txt");
        std::fs::write(&path, b"before").unwrap();
        f.write(&path, b"after");
        f.finish();
        let other = f.daemon.with_store(|st| st.create_session(None)).unwrap();
        for request in [
            Request::RunFiles {
                session_id: other.id.clone(),
                run_id: Some(f.run.clone()),
            },
            Request::RewindRun {
                session_id: other.id.clone(),
                run_id: Some(f.run.clone()),
                path: None,
            },
        ] {
            assert!(f.daemon.handle(request).await.is_err());
        }
        f.daemon.with_store(|st| st.create_run(&other.id)).unwrap();
        let result = f
            .daemon
            .handle(Request::RewindRun {
                session_id: f.session.clone(),
                run_id: Some(f.run.clone()),
                path: None,
            })
            .await
            .unwrap_err();
        assert!(result.to_string().contains("active Run"), "{result:#}");
        assert_eq!(std::fs::read(&path).unwrap(), b"after");
    }

    #[tokio::test]
    async fn changes_between_write_and_run_end_are_never_adopted_as_run_output() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("owned.txt");
        std::fs::write(&path, b"before").unwrap();
        f.write(&path, b"Run output");
        std::fs::write(&path, b"external actor before Run end").unwrap();
        f.finish();
        let result = f.rewind().await;
        assert!(result["restored"].as_array().unwrap().is_empty());
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"external actor before Run end"
        );
    }

    #[tokio::test]
    async fn alias_and_legacy_snapshots_have_specific_read_only_reasons() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let target = f.root.join("target.txt");
        let alias = f.root.join("link.txt");
        std::fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        f.daemon
            .snapshot_before_change_resolved(&f.session, &f.run, &alias, &target);
        let legacy = f.root.join("legacy.txt");
        std::fs::write(&legacy, b"legacy current").unwrap();
        f.daemon
            .with_store(|st| {
                st.snapshot_run_file(
                    &f.session,
                    &f.run,
                    legacy.to_str().unwrap(),
                    true,
                    Some(b"legacy before"),
                )
            })
            .unwrap();
        f.finish();
        let result = f.rewind().await;
        assert_eq!(result["skipped"].as_array().unwrap().len(), 2);
        assert!(result["skipped"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("symlink"));
        assert!(result["skipped"][1]["reason"]
            .as_str()
            .unwrap()
            .contains("No verified Run-end"));
        assert_eq!(std::fs::read(&target).unwrap(), b"target");
        assert_eq!(std::fs::read(&legacy).unwrap(), b"legacy current");
    }

    #[tokio::test]
    async fn process_capability_invocation_marks_coverage_even_without_a_file_callback() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let caps = formless_runtime::Capabilities::parse(&["process:echo".into()]).unwrap();
        f.daemon
            .with_store(|st| {
                st.create_tool_version(
                    "opaque-cap",
                    "fixture",
                    "{}",
                    "{}",
                    "function run(){ return {}; }",
                    &caps,
                    256,
                    "agent",
                )
            })
            .unwrap();
        f.daemon
            .clone()
            .execute_tool_value(
                f.session.clone(),
                f.run.clone(),
                "opaque-cap".into(),
                json!({}),
                0,
                None,
                Arc::new(AtomicBool::new(false)),
                None,
                CancellationToken::new(),
                Config::load(&f.daemon.paths).unwrap(),
                true,
            )
            .await
            .unwrap();
        f.finish();
        let result = f
            .daemon
            .handle(Request::RunFiles {
                session_id: f.session.clone(),
                run_id: None,
            })
            .await
            .unwrap();
        assert_eq!(result["run_id"], f.run);
        assert_eq!(result["untracked_writes_possible"], true);
        assert!(result["files"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn runtime_file_hooks_keep_before_and_post_capture_keys_consistent() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("runtime.txt");
        std::fs::write(&path, b"before runtime").unwrap();
        let caps =
            formless_runtime::Capabilities::parse(&[format!("fs:write:{}", f.root.display())])
                .unwrap();
        f.daemon
            .with_store(|st| {
                st.create_tool_version(
                    "runtime-write",
                    "fixture",
                    "{}",
                    "{}",
                    "function run(i){ fs.write(i.path,'runtime output'); return {}; }",
                    &caps,
                    256,
                    "agent",
                )
            })
            .unwrap();
        f.daemon
            .clone()
            .execute_tool_value(
                f.session.clone(),
                f.run.clone(),
                "runtime-write".into(),
                json!({"path":path}),
                0,
                None,
                Arc::new(AtomicBool::new(false)),
                None,
                CancellationToken::new(),
                Config::load(&f.daemon.paths).unwrap(),
                true,
            )
            .await
            .unwrap();
        f.finish();
        let rows = f
            .daemon
            .with_reader(|st| st.run_file_records(&f.session, &f.run))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, path.to_str().unwrap());
        assert_eq!(rows[0].before_kind.as_deref(), Some("file"));
        assert!(rows[0].kept);
        let state: AfterState =
            serde_json::from_str(rows[0].after_state.as_deref().unwrap()).unwrap();
        assert_eq!(state.kind, "file");
        assert!(state.reason.is_none());
        let result = f.rewind().await;
        assert_eq!(result["restored"].as_array().unwrap().len(), 1, "{result}");
        assert_eq!(std::fs::read(path).unwrap(), b"before runtime");
    }

    #[tokio::test]
    async fn pending_background_invocation_prevents_attribution_and_rewind() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        let path = f.root.join("owned.txt");
        std::fs::write(&path, b"before").unwrap();
        f.write(&path, b"Run output");
        f.daemon
            .with_store(|st| {
                let (tool, _) = st.create_tool_version(
                    "background",
                    "fixture",
                    "{}",
                    "{}",
                    "function run(){return {};}",
                    &formless_runtime::Capabilities::default(),
                    256,
                    "agent",
                )?;
                st.start_invocation(&tool.id, 1, Some(&f.session), Some(&f.run), None, "{}")
            })
            .unwrap();
        f.finish();
        let rows = f
            .daemon
            .with_reader(|st| st.run_file_records(&f.session, &f.run))
            .unwrap();
        let state: AfterState =
            serde_json::from_str(rows[0].after_state.as_deref().unwrap()).unwrap();
        assert!(state.reason.unwrap().contains("Background Invocations"));
        assert!(f
            .daemon
            .rewind_run_files(&f.session, &f.run, None)
            .unwrap_err()
            .to_string()
            .contains("pending/background"));
        assert_eq!(std::fs::read(&path).unwrap(), b"Run output");
    }

    #[tokio::test]
    async fn delete_session_rpc_purges_recovery_rows_after_its_undo_window() {
        let env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new(&env);
        f.write(&f.root.join("created.txt"), b"Run output");
        f.finish();
        let mut cfg = Config::load(&f.daemon.paths).unwrap();
        cfg.daemon.delete_undo_secs = 0;
        cfg.save(&f.daemon.paths).unwrap();
        assert_eq!(
            f.daemon.with_reader(|st| st.run_file_usage()).unwrap().rows,
            1
        );
        let result = f
            .daemon
            .handle(Request::DeleteSession {
                id: f.session.clone(),
            })
            .await
            .unwrap();
        assert_eq!(result["deleted"], true);
        assert_eq!(result["undo_secs"], 0);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if f.daemon.with_reader(|st| st.run_file_usage()).unwrap().rows == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(f
            .daemon
            .with_reader(|st| st.get_session(&f.session))
            .unwrap()
            .is_none());
    }
}
