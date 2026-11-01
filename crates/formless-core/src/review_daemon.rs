//! Explicit owned-Run inverse edits and version-pinned Tool drafts. No review
//! route starts a Run, authenticates a provider or publishes a Tool version.
use super::*;
use crate::{
    hunk_review::{image_hash, ReviewDocument, MAX_REVIEW_BYTES},
    review_file_io::{ReviewImage, ReviewPath},
    review_journal::{Journal, Phase, Receipt},
    storage::ReviewFailure,
};
const TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const MAX_REVIEWS: usize = 16;
const MAX_ENTRY: usize = 8 * 1024 * 1024;
const MAX_STATE: usize = 32 * 1024 * 1024;
const MAX_CHANGES: usize = 128;
#[cfg(windows)]
pub(super) fn retained_owner(
    session: &str,
    run: &str,
    path: &str,
) -> crate::review_retained::RetainedOwner {
    use sha2::{Digest, Sha256};
    crate::review_retained::RetainedOwner {
        session_id: session.into(),
        run_id: run.into(),
        path_digest: format!("{:x}", Sha256::digest(path.as_bytes())),
    }
}
#[derive(Default)]
pub(super) struct ReviewState {
    entries: HashMap<String, Entry>,
}
#[derive(Clone)]
struct Entry {
    created: std::time::Instant,
    view: HunkReview,
    documents: BTreeMap<String, ReviewDocument>,
    images: BTreeMap<String, ReviewImage>,
    original_images: BTreeMap<String, ReviewImage>,
    rejected: BTreeMap<String, BTreeSet<String>>,
    draft_undo: BTreeMap<String, DraftUndo>,
    #[cfg(windows)]
    live_changes: BTreeSet<String>,
    #[cfg(windows)]
    retained: BTreeMap<String, crate::review_retained::RetainedToken>,
    #[cfg(windows)]
    missing_markers: BTreeMap<String, crate::review_retained::RetainedToken>,
}
#[derive(Clone)]
struct DraftUndo {
    file_id: String,
    hunk_id: String,
    before: ReviewImage,
    after: ReviewImage,
    previous: String,
}
impl Entry {
    fn bytes(&self) -> Result<usize> {
        #[cfg(windows)]
        let retained_metadata =
            (self.retained.len() + self.missing_markers.len()) * 64 + self.live_changes.len() * 36;
        #[cfg(not(windows))]
        let retained_metadata = 0;
        Ok(retained_metadata
            + serde_json::to_vec(&self.view)?.len()
            + self
                .documents
                .values()
                .map(ReviewDocument::retained_bytes)
                .sum::<usize>()
            + self
                .images
                .values()
                .map(ReviewImage::retained_bytes)
                .sum::<usize>()
            + self
                .original_images
                .values()
                .map(ReviewImage::retained_bytes)
                .sum::<usize>()
            + self
                .draft_undo
                .values()
                .map(|u| u.before.retained_bytes() + u.after.retained_bytes())
                .sum::<usize>())
    }
    fn file(&self, id: &str) -> Result<&ReviewFile> {
        self.view
            .files
            .iter()
            .find(|f| f.id == id)
            .context(ReviewFailure::Invalid("Unknown file in this review".into()))
    }
    fn file_mut(&mut self, id: &str) -> Result<&mut ReviewFile> {
        self.view
            .files
            .iter_mut()
            .find(|f| f.id == id)
            .context(ReviewFailure::Invalid("Unknown file in this review".into()))
    }
    fn decision(&self, file: &str, hunk: &str) -> Result<String> {
        self.file(file)?
            .hunks
            .iter()
            .find(|h| h.id == hunk)
            .map(|h| h.decision.clone())
            .context(ReviewFailure::Invalid("Unknown hunk in this review".into()))
    }
    fn set_decision(&mut self, file: &str, hunk: &str, decision: &str) -> Result<()> {
        self.file_mut(file)?
            .hunks
            .iter_mut()
            .find(|h| h.id == hunk)
            .context("Unknown review hunk")?
            .decision = decision.into();
        Ok(())
    }
    fn changed(&mut self, file: &str, image: ReviewImage) -> Result<()> {
        self.file_mut(file)?.after_hash = image.hash();
        if self.view.tool_id.is_some() {
            let bytes = image
                .bytes
                .as_ref()
                .context("Tool draft cannot be missing")?;
            self.view.draft_code = Some(String::from_utf8(bytes.clone())?);
            self.view.candidate_hash = Some(image.hash());
        }
        self.images.insert(file.into(), image);
        Ok(())
    }
}
impl ReviewState {
    fn prune(&mut self) {
        self.entries
            .retain(|_, entry| entry.created.elapsed() < TTL);
    }
    fn insert(&mut self, entry: Entry) -> Result<HunkReview> {
        self.prune();
        let size = entry.bytes()?;
        if size > MAX_ENTRY
            || self.entries.len() >= MAX_REVIEWS
            || self
                .entries
                .values()
                .try_fold(size, |sum, e| Ok::<_, anyhow::Error>(sum + e.bytes()?))?
                > MAX_STATE
        {
            return Err(ReviewFailure::Invalid("Interactive review capacity reached; existing undo reviews are retained. Close expired reviews and try again later.".into()).into());
        }
        let view = entry.view.clone();
        self.entries.insert(view.review_id.clone(), entry);
        Ok(view)
    }
    fn get_mut(&mut self, id: &str) -> Result<&mut Entry> {
        self.prune();
        self.entries.get_mut(id).context(ReviewFailure::Invalid("Review expired or the daemon restarted; inspect again. Run undo receipts remain private and recoverable.".into()))
    }
}
pub(super) fn error(error: anyhow::Error) -> anyhow::Error {
    match error.downcast_ref::<ReviewFailure>() {
        Some(ReviewFailure::NotFound) => {
            anyhow::Error::new(RequestFailure::not_found(error.to_string()))
        }
        Some(ReviewFailure::Invalid(_)) => {
            anyhow::Error::new(RequestFailure::invalid(error.to_string()))
        }
        None => error,
    }
}
fn invalid(message: impl Into<String>) -> anyhow::Error {
    ReviewFailure::Invalid(message.into()).into()
}
fn blank(session: &str) -> HunkReview {
    HunkReview{review_id:uuid::Uuid::new_v4().to_string(),session_id:session.into(),run_id:None,tool_id:None,base_version:None,original_candidate_hash:None,candidate_hash:None,draft_code:None,files:Vec::new(),undo_changes:Vec::new(),expires_at:(chrono::Utc::now()+chrono::Duration::minutes(30)).to_rfc3339(),warnings:vec!["Closing review does not cancel a file action already dispatched. Reopen /review to recover Run undo receipts; never automatically replay a write.".into()]}
}
fn excluded(path: &str, reason: String, before_hash: String, after_hash: String) -> ReviewFile {
    ReviewFile {
        id: uuid::Uuid::new_v4().to_string(),
        path: path.into(),
        status: "unavailable".into(),
        before_hash,
        after_hash,
        hunks: Vec::new(),
        exclusion_reason: Some(reason),
        warnings: Vec::new(),
    }
}
impl Daemon {
    fn review_live(&self, session: &str, idle: bool) -> Result<()> {
        self.with_reader(|st| {
            if st.get_session(session)?.is_none() || st.session_deleted(session)? {
                return Err(ReviewFailure::NotFound.into());
            }
            Ok(())
        })?;
        if idle {
            let (live, complete) = self.review_pending_status(Some(session));
            if self.runs.locked().contains_key(session)
                || live
                || self.with_reader(|st| st.review_session_busy(session, &complete))?
            {
                return Err(anyhow::Error::new(RequestFailure::busy("Session has an active Run or pending/background Invocation; wait or stop it before changing reviewed files")));
            }
        }
        Ok(())
    }
    fn review_policy(&self) -> Result<formless_runtime::sensitive_paths::PathPolicy> {
        let cfg = Config::load(&self.paths)?;
        crate::sensitive_reads::policy(&self.paths, &cfg.security, &self.sensitive_anchors)
            .map_err(invalid)
    }
    fn validate_review(&self, entry: &Entry, idle: bool) -> Result<()> {
        self.review_live(&entry.view.session_id, idle && entry.view.run_id.is_some())?;
        if let Some(run) = &entry.view.run_id {
            let recorded =
                self.with_reader(|st| st.recorded_run_review(&entry.view.session_id, Some(run)))?;
            if entry
                .view
                .files
                .iter()
                .any(|f| !recorded.files.iter().any(|r| r.path == f.path))
            {
                return Err(invalid(
                    "Recorded Run snapshots changed or expired; inspect again",
                ));
            }
        } else if let Some(tool) = &entry.view.tool_id {
            self.with_reader(|st| {
                st.review_tool_code(
                    tool,
                    entry
                        .view
                        .base_version
                        .context("Missing review Tool version")?,
                )
            })?;
        } else {
            return Err(invalid("Review has no owned source"));
        }
        Ok(())
    }
    #[cfg(windows)]
    pub(super) fn prune_retained_capture_leases_in(&self, store: &Store) {
        // Existing pending/background work can still mutate its source object.
        // Be conservative on unknown state; this map is independently bounded
        // by the retained store's fixed owner/manifest capacity.
        if self.review_pending_status(None).0 {
            return;
        }
        self.retained_capture_leases
            .locked()
            .retain(|(_, run, _), _| store.review_retained_run_active(run).unwrap_or(true));
    }
    #[cfg(windows)]
    pub(super) fn expire_retained_now(
        &self,
        retained: &crate::review_retained::RetainedStore,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        retained.expire(now)?;
        Ok(())
    }
    #[cfg(windows)]
    pub(super) fn sweep_retained_before_action(&self) {
        // Always Store -> capture-map. Drop BOTH before native expiry work;
        // fresh capture/review token leases protect objects across this gap.
        {
            let store = self.store.locked();
            self.prune_retained_capture_leases_in(&store);
        }
        if let Ok(Some(retained)) = crate::review_retained::RetainedStore::open(&self.paths, false)
        {
            if self.expire_retained_now(&retained).is_err() {
                tracing::warn!("Windows review retention cleanup deferred; unknown state retained");
            }
        }
    }
    fn read_review_image(
        &self,
        path: &str,
        policy: &formless_runtime::sensitive_paths::PathPolicy,
    ) -> Result<ReviewImage> {
        let access = ReviewPath::open(std::path::Path::new(path), policy)?;
        #[cfg(windows)]
        if let Some(store) = crate::review_retained::RetainedStore::open(&self.paths, false)
            .ok()
            .flatten()
        {
            return access.read_tracked(policy, &store);
        }
        access.read(policy)
    }
    fn validate_review_image(
        &self,
        entry: &Entry,
        file: &str,
        hash: &str,
        policy: &formless_runtime::sensitive_paths::PathPolicy,
    ) -> Result<ReviewImage> {
        let shown = entry.file(file)?;
        if shown.exclusion_reason.is_some() {
            return Err(invalid("This file is listed but not hunk-editable"));
        }
        let expected = entry
            .images
            .get(file)
            .context("Missing server-owned review image")?;
        if shown.after_hash != hash || expected.hash() != hash {
            return Err(invalid(
                "Reviewed content changed; inspect again before applying a decision",
            ));
        }
        if entry.view.run_id.is_some() {
            let current = self.read_review_image(&shown.path, policy)?;
            if &current != expected {
                return Err(invalid(
                    "File content, existence or mode changed since review; inspect again",
                ));
            }
        }
        Ok(expected.clone())
    }
    pub(super) fn inspect_run_review(
        &self,
        session: &str,
        run: Option<&str>,
    ) -> Result<HunkReview> {
        let _admission = self.forecast_admission.locked();
        let _settings = self.config_guard();
        self.review_live(session, true)?;
        let policy = self.review_policy()?;
        let inspection_store = self.store.locked();
        self.review_commit_idle(&inspection_store, session)?;
        let recorded = self.with_reader(|st| st.recorded_run_review(session, run))?;
        let mut state = self.hunk_reviews.locked();
        state.prune();
        #[cfg(windows)]
        self.with_store(|store| {
            self.prune_retained_capture_leases_in(store);
            Ok(())
        })?;
        for entry in state.entries.values() {
            if entry.view.session_id == session
                && entry.view.run_id.as_deref() == Some(recorded.run_id.as_str())
                && entry.view.files.len() == recorded.files.len()
                && entry
                    .view
                    .files
                    .iter()
                    .all(|file| file.exclusion_reason.is_none())
                && entry.images.iter().all(|(id, _)| {
                    self.validate_review_image(
                        entry,
                        id,
                        &entry
                            .file(id)
                            .map(|f| f.after_hash.clone())
                            .unwrap_or_default(),
                        &policy,
                    )
                    .is_ok()
                })
            {
                return Ok(entry.view.clone());
            }
        }
        let mut view = blank(session);
        view.run_id = Some(recorded.run_id.clone());
        let mut entry = Entry {
            created: std::time::Instant::now(),
            view,
            documents: BTreeMap::new(),
            images: BTreeMap::new(),
            original_images: BTreeMap::new(),
            rejected: BTreeMap::new(),
            draft_undo: BTreeMap::new(),
            #[cfg(windows)]
            live_changes: BTreeSet::new(),
            #[cfg(windows)]
            retained: BTreeMap::new(),
            #[cfg(windows)]
            missing_markers: BTreeMap::new(),
        };
        let mut retained = 0usize;
        for record in recorded.files {
            let before_hash = if !record.existed {
                image_hash(None)
            } else {
                record
                    .before
                    .as_deref()
                    .map(|bytes| image_hash(Some(bytes)))
                    .unwrap_or_default()
            };
            if let Some(reason) = record.exclusion_reason {
                entry
                    .view
                    .files
                    .push(excluded(&record.path, reason, before_hash, String::new()));
                continue;
            }
            #[cfg(windows)]
            if entry.images.len() >= 128 {
                entry.view.files.push(excluded(
                    &record.path,
                    "This review already retains its maximum of 128 Windows file objects".into(),
                    before_hash,
                    String::new(),
                ));
                continue;
            }
            let image = match self.read_review_image(&record.path, &policy) {
                Ok(image) => image,
                Err(error) => {
                    entry.view.files.push(excluded(
                        &record.path,
                        format!("File access refused: {error}"),
                        before_hash,
                        String::new(),
                    ));
                    continue;
                }
            };
            let original = record.before_metadata.as_deref().map(|metadata| {
                crate::review_file_io::decode_before_metadata(metadata, record.before.as_deref())
            });
            let original = match original {
                Some(Ok(original)) if original.bytes.is_some() != record.existed => {
                    entry.view.files.push(excluded(
                        &record.path,
                        "Recorded existence does not match the checksum-bound original image"
                            .into(),
                        before_hash,
                        image.hash(),
                    ));
                    continue;
                }
                Some(Ok(image)) => Some(image),
                Some(Err(error)) => {
                    entry.view.files.push(excluded(
                        &record.path,
                        format!("Invalid original metadata: {error}"),
                        before_hash,
                        image.hash(),
                    ));
                    continue;
                }
                None => None,
            };
            #[cfg(windows)]
            let (retained_token, missing_marker) = {
                let store = crate::review_retained::RetainedStore::open(&self.paths, false)
                    .ok()
                    .flatten();
                let token = if record.existed {
                    if let Some(store) = &store {
                        // A known private first-capture binding must never be
                        // bypassed by treating its verification error as legacy
                        // SQLite-only history. Missing metadata is deliberately
                        // not reconstructed: any existing private record rejects
                        // this incomplete comparison image.
                        let comparison = original.clone().unwrap_or_else(|| ReviewImage {
                            bytes: record.before.clone(),
                            ..Default::default()
                        });
                        match store.lookup(
                            &retained_owner(session, &recorded.run_id, &record.path),
                            &comparison,
                        ) {
                            Ok(token) => token,
                            Err(_) => {
                                entry.view.files.push(excluded(&record.path,
                                    "Private immutable first-capture binding cannot be verified; this file is read-only".into(), before_hash, image.hash()));
                                continue;
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let missing_marker = if !record.existed {
                    original.as_ref().and_then(|missing| {
                        store.as_ref().and_then(|store| {
                            store
                                .lookup_missing(
                                    &retained_owner(session, &recorded.run_id, &record.path),
                                    missing,
                                )
                                .ok()
                                .flatten()
                        })
                    })
                } else {
                    None
                };
                if !record.existed && missing_marker.is_none() {
                    entry.view.files.push(excluded(&record.path,
                        "Created Windows file has no private pre-Tool missing-state marker; database history alone cannot authorize removal".into(), before_hash, image.hash()));
                    continue;
                }
                if record.existed && image.bytes.is_none() && token.is_none() {
                    entry.view.files.push(excluded(&record.path,
                        "Deleted Windows file has no verified retained original object; historical opaque security state cannot be recreated".into(), before_hash, image.hash()));
                    continue;
                }
                if (!record.existed && image.bytes.is_some())
                    || (record.existed && image.bytes.is_none())
                {
                    let source_identity = if image.bytes.is_some() {
                        image.windows_identity.as_ref()
                    } else {
                        original
                            .as_ref()
                            .and_then(|image| image.windows_identity.as_ref())
                    };
                    let volume_supported = source_identity.is_some_and(|identity| {
                        store.as_ref().is_some_and(|store| {
                            store.supports_identity(identity).unwrap_or(false)
                                && ReviewPath::open(std::path::Path::new(&record.path), &policy)
                                    .and_then(|access| {
                                        access.namespace_target_matches(identity, &policy)
                                    })
                                    .unwrap_or(false)
                        })
                    });
                    if !volume_supported
                        || store
                            .as_ref()
                            .is_none_or(|store| store.ensure_namespace_capability().is_err())
                    {
                        entry.view.files.push(excluded(&record.path,
                            "Windows namespace recovery is unavailable on this volume; no inverse action was offered".into(), before_hash, image.hash()));
                        continue;
                    }
                }
                (token, missing_marker)
            };
            if original
                .as_ref()
                .is_some_and(|original| original.bytes.is_some() && !original.security_complete)
                && !(cfg!(windows)
                    && record.existed
                    && (image.bytes.is_some() && image.existing_write_restriction().is_none() || {
                        #[cfg(windows)]
                        {
                            retained_token.is_some()
                        }
                        #[cfg(not(windows))]
                        {
                            false
                        }
                    }))
            {
                entry.view.files.push(excluded(&record.path,
                    "Original bytes were captured without fully restorable metadata; this file remains read-only".into(), before_hash, image.hash()));
                continue;
            }
            if record.existed && image.bytes.is_none() {
                let reason = original.as_ref().map_or(Some("Deleted file has no recorded original metadata; whole-file restore remains separate."), |original| {
                    #[cfg(windows)]
                    if retained_token.is_some() { return original.ordinary_write_restriction(); }
                    original.write_restriction()
                });
                if let Some(reason) = reason {
                    entry.view.files.push(excluded(
                        &record.path,
                        reason.into(),
                        before_hash,
                        image.hash(),
                    ));
                    continue;
                }
            }
            let restriction = if image.bytes.is_some() {
                image.existing_write_restriction()
            } else {
                image.write_restriction()
            };
            if let Some(reason) = restriction {
                entry.view.files.push(excluded(
                    &record.path,
                    reason.into(),
                    before_hash,
                    image.hash(),
                ));
                continue;
            }
            retained += image.bytes.as_ref().map_or(0, Vec::len);
            if retained > 2 * 1024 * 1024 {
                entry.view.files.push(excluded(
                    &record.path,
                    "Run exceeds the 2 MiB current-image review budget.".into(),
                    before_hash,
                    image.hash(),
                ));
                continue;
            }
            let document = ReviewDocument::build(
                &record.path,
                if record.existed {
                    record.before.as_deref()
                } else {
                    None
                },
                image.bytes.as_deref(),
            );
            let shown = document.view().clone();
            if shown.exclusion_reason.is_none() {
                if let Some(original) = original {
                    entry.original_images.insert(shown.id.clone(), original);
                }
                #[cfg(windows)]
                if let Some(token) = retained_token {
                    entry.retained.insert(shown.id.clone(), token);
                }
                #[cfg(windows)]
                if let Some(marker) = missing_marker {
                    entry.missing_markers.insert(shown.id.clone(), marker);
                }
                entry.images.insert(shown.id.clone(), image);
                entry.documents.insert(shown.id.clone(), document);
            }
            entry.view.files.push(shown);
            if entry.bytes()? > MAX_ENTRY {
                return Err(invalid("Run diff exceeds the bounded interactive response limit; use the whole-file listing"));
            }
        }
        self.recover_review_changes(&mut entry, &policy)?;
        state.insert(entry)
    }
    fn recover_review_changes(
        &self,
        entry: &mut Entry,
        policy: &formless_runtime::sensitive_paths::PathPolicy,
    ) -> Result<()> {
        let Some(run) = entry.view.run_id.as_deref() else {
            return Ok(());
        };
        let Some(journal) = Journal::open(&self.paths, false)? else {
            return Ok(());
        };
        let records = journal.list(&entry.view.session_id, run)?;
        let mut virtual_images: BTreeMap<String, ReviewImage> = BTreeMap::new();
        let mut descriptors = Vec::new();
        for record in records.iter().rev() {
            let Some(file) = entry.view.files.iter().find(|f| f.path == record.path) else {
                entry.view.warnings.push(format!("Undo receipt {} no longer has an owned Run snapshot; no automatic write is allowed.",record.id));
                continue;
            };
            let image = if let Some(image) = virtual_images.get(&record.path) {
                image.clone()
            } else {
                match self.read_review_image(&record.path, policy) {
                    Ok(image) => image,
                    Err(_) => {
                        entry.view.warnings.push(format!("Undo receipt {} cannot safely inspect its recorded file; no automatic write is allowed.",record.id));
                        continue;
                    }
                }
            };
            #[cfg(windows)]
            {
                let (before, after) = record.images()?;
                if before.bytes.is_none() != after.bytes.is_none() {
                    let object = record.retained.as_ref().and_then(|token| {
                        crate::review_retained::RetainedStore::open(&self.paths, false)
                            .ok()
                            .flatten()
                            .and_then(|store| {
                                store
                                    .open_verified(
                                        token,
                                        &retained_owner(&entry.view.session_id, run, &record.path),
                                    )
                                    .ok()
                            })
                    });
                    let Some(object) = object else {
                        entry.view.warnings.push(format!("Namespace receipt {} has no verified retained original object; no automatic Undo is allowed", record.id));
                        continue;
                    };
                    entry.retained.insert(file.id.clone(), object.token());
                }
                if before.bytes.is_some() && after.bytes.is_some() {
                    entry.view.warnings.push(format!("Windows undo receipt {} retains recovery bytes, but this reopened review does not retain the original live file object. Automatic Undo is unavailable; no metadata is reconstructed.", record.id));
                    continue;
                }
            }
            let phase = record.observed_phase(&image)?;
            let undone = matches!(phase, Phase::Undone | Phase::Prepared);
            if phase == Phase::Applied {
                virtual_images.insert(record.path.clone(), record.images()?.0);
            } else if phase == Phase::Ambiguous {
                entry.view.warnings.push(format!("Undo receipt {} does not match a known before/after image; external or interrupted changes require explicit repair. No automatic overwrite is allowed.",record.id));
            }
            descriptors.push(ReviewChangeInfo {
                change_id: record.id.clone(),
                file_id: file.id.clone(),
                hunk_id: record.hunk_id.clone(),
                description: format!("Reject hunk in {}", serde_json::to_string(&record.path)?),
                undone,
            });
        }
        descriptors.reverse();
        entry.view.undo_changes = descriptors;
        Ok(())
    }
    pub(super) fn prepare_tool_review(
        &self,
        session: &str,
        tool: &str,
        version: i64,
        candidate: &str,
    ) -> Result<HunkReview> {
        let _admission = self.forecast_admission.locked();
        self.review_live(session, false)?;
        if candidate.len() > MAX_REVIEW_BYTES {
            return Err(invalid("Tool draft exceeds the 256 KiB review limit"));
        }
        let before = self.with_reader(|st| st.review_tool_code(tool, version))?;
        let path = format!("tool:{tool}/code.js");
        let document =
            ReviewDocument::build(&path, Some(before.as_bytes()), Some(candidate.as_bytes()));
        let file = document.view().clone();
        let mut view = blank(session);
        view.tool_id = Some(tool.into());
        view.base_version = Some(version);
        view.original_candidate_hash = Some(image_hash(Some(candidate.as_bytes())));
        view.candidate_hash = view.original_candidate_hash.clone();
        view.draft_code = Some(candidate.into());
        view.files.push(file.clone());
        view.warnings.push("Tool review changes only this server-owned draft. Ctrl+S still uses the normal save/smoke gate; draft undo expires with this review.".into());
        let entry = Entry {
            created: std::time::Instant::now(),
            view,
            documents: BTreeMap::from([(file.id.clone(), document)]),
            original_images: BTreeMap::new(),
            images: BTreeMap::from([(
                file.id.clone(),
                ReviewImage {
                    bytes: Some(candidate.as_bytes().to_vec()),
                    mode: None,
                    readonly: false,
                    ..Default::default()
                },
            )]),
            rejected: BTreeMap::new(),
            draft_undo: BTreeMap::new(),
            #[cfg(windows)]
            live_changes: BTreeSet::new(),
            #[cfg(windows)]
            retained: BTreeMap::new(),
            #[cfg(windows)]
            missing_markers: BTreeMap::new(),
        };
        self.hunk_reviews.locked().insert(entry)
    }
    fn audit_review_receipt_in(st: &Store, receipt: &Receipt, status: &str) -> Result<()> {
        let (before, after) = receipt.images()?;
        let detail = json!({"change_id":receipt.id,"session_id":receipt.session_id,"run_id":receipt.run_id,"file_id":receipt.file_id,"hunk_id":receipt.hunk_id,"path":receipt.path,"before_hash":before.hash(),"after_hash":after.hash(),"status":status});
        st.audit("user", "review.file", &detail.to_string())
    }
    fn review_pending_status(&self, session: Option<&str>) -> (bool, BTreeSet<String>) {
        let mut complete = BTreeSet::new();
        let mut live = false;
        for (handle, pending) in self
            .pending
            .locked()
            .iter()
            .filter(|(_, pending)| session.is_none_or(|session| pending.session_id == session))
        {
            if pending.result.locked().is_some() {
                complete.insert(handle.clone());
            } else {
                live = true;
            }
        }
        (live, complete)
    }
    fn review_commit_idle(&self, st: &Store, session: &str) -> Result<()> {
        let (live, complete) = self.review_pending_status(Some(session));
        if live || st.review_session_busy(session, &complete)? {
            return Err(anyhow::Error::new(RequestFailure::busy("Session still has pending/background Invocations; wait or stop them before file review")));
        }
        Ok(())
    }
    fn review_profile_commit_idle(&self, st: &Store) -> Result<()> {
        let (live, complete) = self.review_pending_status(None);
        // Do not acquire `runs` here: Run admission can hold that mutex before
        // acquiring Store. Committed Run statuses cover the entire profile.
        if live || st.review_profile_busy(&complete)? {
            return Err(anyhow::Error::new(RequestFailure::busy(
                "This profile still has an active Run or pending/background Invocation; wait or stop it before changing Run files",
            )));
        }
        Ok(())
    }
    fn apply_review_file(
        &self,
        mut receipt: Receipt,
        live: &ReviewImage,
        policy: &formless_runtime::sensitive_paths::PathPolicy,
    ) -> Result<(Receipt, Vec<String>, ReviewImage)> {
        // New Invocation starts and snapshot-before-write admission use this
        // same Store mutex. Keep the final known-work check through commit.
        #[cfg(windows)]
        self.sweep_retained_before_action();
        let store = self.store.locked();
        self.review_profile_commit_idle(&store)?;
        #[cfg(windows)]
        self.prune_retained_capture_leases_in(&store);
        let (before, after) = receipt.images()?;
        let access = ReviewPath::open(std::path::Path::new(&receipt.path), policy)?;
        #[cfg(windows)]
        let retained_store = if before.bytes.is_some() != after.bytes.is_some() {
            crate::review_retained::RetainedStore::open(&self.paths, true)?
        } else {
            // Retention is not a dependency of the accepted ordinary single-
            // link byte-only path. Its own no-follow/link-count checks remain.
            crate::review_retained::RetainedStore::open(&self.paths, false)
                .ok()
                .flatten()
        };
        #[cfg(windows)]
        let mut prepared_move = None;
        #[cfg(windows)]
        let mut prepared_remove = None;
        #[cfg(windows)]
        if before.bytes.is_some() != after.bytes.is_some() {
            let retained = retained_store
                .as_ref()
                .context("Retained object store unavailable")?;
            retained.ensure_namespace_capability()?;
            let owner = retained_owner(&receipt.session_id, &receipt.run_id, &receipt.path);
            if before.bytes.is_some() {
                let source = access.namespace_source(&before, policy, retained)?;
                if let Some(token) = receipt.retained.as_ref() {
                    let object = retained.open_verified(token, &owner)?;
                    if object.before() != &before {
                        return Err(invalid(
                            "Retained creation image changed; no namespace mutation was applied",
                        ));
                    }
                    object.verify_peer(&source)?;
                    receipt.retained = Some(object.token());
                    prepared_remove = Some((source, object));
                } else {
                    let prepared = retained.prepare_move(&owner, &source, &before)?;
                    receipt.retained = Some(prepared.token().clone());
                    prepared_move = Some((prepared, source));
                }
            } else {
                let object = retained.open_verified(
                    receipt
                        .retained
                        .as_ref()
                        .context("Deleted-file recovery has no retained object")?,
                    &owner,
                )?;
                object.require_retained_only()?;
                if object.before() != &after {
                    return Err(invalid(
                        "Retained original image does not match the inverse edit",
                    ));
                }
            }
        }
        let journal =
            Journal::open(&self.paths, true)?.context("Missing private review journal")?;
        journal.prepare(&mut receipt)?;
        Self::audit_review_receipt_in(&store, &receipt, "prepared").context(
            "Audit unavailable; no file was changed. The prepared private receipt is retained.",
        )?;
        #[cfg(windows)]
        let (before, after) = {
            let (mut before, mut after) = (before, after);
            if before.bytes.is_some() && after.bytes.is_some() {
                before.attach_witness(live)?;
                after.attach_witness(live)?;
            }
            (before, after)
        };
        #[cfg(not(windows))]
        let _ = live;
        #[cfg(windows)]
        let outcome = if let Some((prepared, source)) = prepared_move {
            retained_store
                .as_ref()
                .context("Retained store disappeared")?
                .commit_move(prepared, &source)
                .map(|token| {
                    receipt.retained = Some(token);
                    Vec::new()
                })
        } else if let Some((source, object)) = prepared_remove {
            access.remove_prepared(source, &before, &object, policy)
        } else {
            match &retained_store {
                Some(retained) if before.bytes.is_none() && after.bytes.is_some() => {
                    let object = retained.open_verified(
                        receipt
                            .retained
                            .as_ref()
                            .context("Missing retained inverse object")?,
                        &retained_owner(&receipt.session_id, &receipt.run_id, &receipt.path),
                    )?;
                    access.restore_retained(&object, &after, policy, retained)
                }
                Some(retained) => access.replace_tracked(&before, &after, policy, retained),
                None => access.replace(&before, &after, policy),
            }
        };
        #[cfg(not(windows))]
        let outcome = access.replace(&before, &after, policy);
        let observed = self.read_review_image(&receipt.path, policy);
        let mut warnings=match outcome {
            Ok(warnings)=>warnings,
            Err(error)=>match &observed {
                Ok(image) if image==&after=>vec![format!("File changed, but replacement reported an error: {error}. Private undo receipt {} is retained.",receipt.id)],
                Ok(image) if image==&before=>return Err(invalid(format!("No inverse edit was applied: {error}. Prepared private receipt {} is retained.",receipt.id))),
                _=>{let _=journal.set_phase(&receipt,Phase::Ambiguous);return Err(invalid(format!("File state may have changed during replacement. No automatic retry or overwrite is allowed; private recovery receipt {} is retained.",receipt.id)));}
            }
        };
        if observed.as_ref().map_or(true, |image| image != &after) {
            let _ = journal.set_phase(&receipt, Phase::Ambiguous);
            return Err(invalid(format!("File changed after replacement or cannot be verified. No automatic overwrite is allowed; private recovery receipt {} is retained.",receipt.id)));
        }
        match journal.set_phase(&receipt,Phase::Applied){Ok(updated)=>receipt=updated,Err(error)=>warnings.push(format!("File replacement completed, but its final receipt status was not confirmed: {error}. Reopen review to reconcile private receipt {}.",receipt.id))}
        if Self::audit_review_receipt_in(&store, &receipt, "applied").is_err() {
            warnings.push("File replacement completed, but the audit update failed; the private prepared undo receipt remains available.".into());
        }
        Ok((
            receipt,
            warnings,
            observed.context("Verified post-image unavailable")?,
        ))
    }
    pub(super) fn decide_review_hunk(
        &self,
        review: &str,
        file: &str,
        hunk: &str,
        expected_hash: &str,
        decision: &str,
    ) -> Result<ReviewDecisionResult> {
        if !matches!(decision, "accept" | "reject") {
            return Err(invalid("Review decision must be accept or reject"));
        }
        let _admission = self.forecast_admission.locked();
        let _settings = self.config_guard();
        let policy = self.review_policy()?;
        let mut state = self.hunk_reviews.locked();
        state.prune();
        let total = state
            .entries
            .values()
            .try_fold(0usize, |n, e| Ok::<_, anyhow::Error>(n + e.bytes()?))?;
        let entry = state.get_mut(review)?;
        self.validate_review(entry, true)?;
        if entry.view.run_id.is_some() {
            self.review_profile_commit_idle(&self.store.locked())?;
        }
        let before = self.validate_review_image(entry, file, expected_hash, &policy)?;
        let previous = entry.decision(file, hunk)?;
        let chosen = if decision == "accept" {
            "accepted"
        } else {
            "rejected"
        };
        if previous == chosen {
            return Ok(ReviewDecisionResult {
                review: entry.view.clone(),
                change_id: None,
                already_applied: true,
            });
        }
        if previous == "rejected" {
            return Err(invalid("Undo the rejection before accepting that hunk"));
        }
        let old_size = entry.bytes()?;
        let mut changed = entry.clone();
        if decision == "accept" {
            changed.set_decision(file, hunk, "accepted")?;
            *entry = changed;
            return Ok(ReviewDecisionResult {
                review: entry.view.clone(),
                change_id: None,
                already_applied: false,
            });
        }
        if entry.view.undo_changes.len() >= MAX_CHANGES {
            return Err(invalid(
                "This review reached its 128-action limit; undo remains available",
            ));
        }
        let ids = changed.rejected.entry(file.into()).or_default();
        ids.insert(hunk.into());
        let bytes = changed
            .documents
            .get(file)
            .context("Missing immutable review document")?
            .content_for_rejections(ids)?;
        if bytes
            .as_ref()
            .is_some_and(|bytes| bytes.len() > MAX_REVIEW_BYTES)
        {
            return Err(invalid(
                "This inverse would exceed the 256 KiB current-image review limit",
            ));
        }
        let mut after = if before.bytes.is_none() && bytes.is_some() && entry.view.run_id.is_some()
        {
            entry
                .original_images
                .get(file)
                .context("Original metadata unavailable for deleted-file restoration")?
                .clone()
        } else {
            before.clone()
        };
        after.bytes = bytes;
        // Removing a created file has no file mode. Its exact old mode remains
        // in the action's private pre-image for a later explicit Undo.
        let after = if after.bytes.is_none() {
            ReviewImage::missing()
        } else {
            after
        };
        changed.changed(file, after.clone())?;
        changed.set_decision(file, hunk, "rejected")?;
        let change_id = uuid::Uuid::new_v4().to_string();
        let mut record = None;
        if let Some(run) = entry.view.run_id.as_deref() {
            let mut receipt = Receipt::new(
                &entry.view.session_id,
                run,
                file,
                hunk,
                &entry.file(file)?.path,
                &previous,
                &before,
                &after,
            )?;
            #[cfg(windows)]
            let retained = if before.bytes.is_some() != after.bytes.is_some() {
                entry.retained.get(file).cloned()
            } else {
                None
            };
            #[cfg(not(windows))]
            let retained = None;
            receipt = receipt.bind_action(&change_id, retained)?;
            record = Some(receipt);
        } else {
            changed.draft_undo.insert(
                change_id.clone(),
                DraftUndo {
                    file_id: file.into(),
                    hunk_id: hunk.into(),
                    before: before.clone(),
                    after: after.clone(),
                    previous,
                },
            );
        }
        changed.view.undo_changes.push(ReviewChangeInfo {
            change_id: change_id.clone(),
            file_id: file.into(),
            hunk_id: hunk.into(),
            description: format!(
                "Reject hunk in {}",
                serde_json::to_string(&entry.file(file)?.path)?
            ),
            undone: false,
        });
        let new_size = changed.bytes()?;
        if new_size > MAX_ENTRY || total - old_size + new_size > MAX_STATE {
            return Err(invalid(
                "Review undo capacity reached; no file or draft was changed",
            ));
        }
        if let Some(receipt) = record {
            let (applied, warnings, observed) =
                self.apply_review_file(receipt, &before, &policy)?;
            #[cfg(not(windows))]
            let _ = (applied, observed);
            #[cfg(windows)]
            {
                changed.live_changes.insert(change_id.clone());
                if let Some(token) = applied.retained {
                    changed.retained.insert(file.into(), token);
                }
                // A restored target gains its own live attribute witness.
                changed.changed(file, observed)?;
            }
            changed.view.warnings.extend(warnings);
        }
        *entry = changed;
        Ok(ReviewDecisionResult {
            review: entry.view.clone(),
            change_id: Some(change_id),
            already_applied: false,
        })
    }
    pub(super) fn undo_review_change(
        &self,
        review: &str,
        change_id: &str,
        expected_hash: &str,
    ) -> Result<ReviewDecisionResult> {
        let _admission = self.forecast_admission.locked();
        let _settings = self.config_guard();
        let policy = self.review_policy()?;
        let mut state = self.hunk_reviews.locked();
        let entry = state.get_mut(review)?;
        self.validate_review(entry, true)?;
        if entry.view.run_id.is_some() {
            self.review_profile_commit_idle(&self.store.locked())?;
        }
        let descriptor = entry
            .view
            .undo_changes
            .iter()
            .find(|c| c.change_id == change_id)
            .cloned()
            .context(ReviewFailure::Invalid(
                "Unknown undo action in this review".into(),
            ))?;
        let current =
            self.validate_review_image(entry, &descriptor.file_id, expected_hash, &policy)?;
        if descriptor.undone {
            return Ok(ReviewDecisionResult {
                review: entry.view.clone(),
                change_id: Some(change_id.into()),
                already_applied: true,
            });
        }
        let mut changed = entry.clone();
        if let Some(run) = entry.view.run_id.as_deref() {
            #[cfg(windows)]
            self.sweep_retained_before_action();
            let store = self.store.locked();
            self.review_profile_commit_idle(&store)?;
            #[cfg(windows)]
            self.prune_retained_capture_leases_in(&store);
            let journal = Journal::open(&self.paths, false)?
                .context("Private undo receipt is unavailable")?;
            let receipt = journal.read(change_id)?;
            if receipt.session_id != entry.view.session_id
                || receipt.run_id != run
                || receipt.path != entry.file(&descriptor.file_id)?.path
            {
                return Err(invalid(
                    "Undo receipt does not belong to the exact owned Session, Run and file",
                ));
            }
            let (before, after) = receipt.images()?;
            if current == before {
                if journal.set_phase(&receipt, Phase::Undone).is_err() {
                    changed.view.warnings.push("The file already matches the pre-image, but final private undo status was not confirmed.".into());
                }
                changed
                    .view
                    .undo_changes
                    .iter_mut()
                    .find(|c| c.change_id == change_id)
                    .context("Undo descriptor disappeared")?
                    .undone = true;
                *entry = changed;
                return Ok(ReviewDecisionResult {
                    review: entry.view.clone(),
                    change_id: Some(change_id.into()),
                    already_applied: true,
                });
            }
            if current != after {
                return Err(invalid("Undo cannot overwrite an unknown image. Undo newer actions first or explicitly repair external changes."));
            }
            #[cfg(windows)]
            let (before, after) = {
                if before.bytes.is_some() && after.bytes.is_some() {
                    if !entry.live_changes.contains(change_id) {
                        return Err(invalid("Windows Undo requires this active review's retained file object; reopened receipts are recovery data only"));
                    }
                    let (mut before, mut after) = (before, after);
                    before.attach_witness(&current)?;
                    after.attach_witness(&current)?;
                    (before, after)
                } else {
                    (before, after)
                }
            };
            let access = ReviewPath::open(std::path::Path::new(&receipt.path), &policy)?;
            #[cfg(windows)]
            let outcome = {
                let retained_store = if before.bytes.is_some() != after.bytes.is_some() {
                    crate::review_retained::RetainedStore::open(&self.paths, false)?
                } else {
                    crate::review_retained::RetainedStore::open(&self.paths, false)
                        .ok()
                        .flatten()
                };
                match retained_store {
                    Some(retained) if before.bytes.is_some() != after.bytes.is_some() => {
                        retained.ensure_namespace_capability()?;
                        let object = retained.open_verified(
                            receipt
                                .retained
                                .as_ref()
                                .context("Namespace Undo has no retained object")?,
                            &retained_owner(&receipt.session_id, &receipt.run_id, &receipt.path),
                        )?;
                        changed
                            .retained
                            .insert(descriptor.file_id.clone(), object.token());
                        if before.bytes.is_some() {
                            access.restore_retained(&object, &before, &policy, &retained)
                        } else {
                            access.remove_retained(&after, &object, &policy, &retained)
                        }
                    }
                    Some(retained) => access.replace_tracked(&after, &before, &policy, &retained),
                    None => access.replace(&after, &before, &policy),
                }
            };
            #[cfg(not(windows))]
            let outcome = access.replace(&after, &before, &policy);
            let observed = self.read_review_image(&receipt.path, &policy);
            if observed.as_ref().map_or(true, |image| image != &before) {
                return Err(invalid(format!("Undo did not reach a verified pre-image and may have changed file state. No automatic retry; private receipt {change_id} is retained.")));
            }
            if let Err(error) = outcome {
                changed.view.warnings.push(format!(
                    "Undo reached the pre-image, but replacement reported an error: {error}"
                ));
            }
            if let Err(error) = journal.set_phase(&receipt, Phase::Undone) {
                changed.view.warnings.push(format!("Undo restored the exact pre-image, but its final private status was not confirmed: {error}"));
            }
            if Self::audit_review_receipt_in(&store, &receipt, "undone").is_err() {
                changed.view.warnings.push("Undo completed; its audit update failed, while the private receipt remains available.".into());
            }
            if changed
                .decision(&descriptor.file_id, &receipt.hunk_id)
                .is_err()
            {
                // Reopened reviews were based on the already-rejected image.
                // Restoring an older image must refresh their hunk ranges;
                // otherwise a later reject could silently drop the restored data.
                let document = changed
                    .documents
                    .get(&descriptor.file_id)
                    .context("Missing server-owned undo document")?
                    .rebased_after(before.bytes.as_deref());
                let mut shown = document.view().clone();
                shown.id = descriptor.file_id.clone();
                *changed.file_mut(&descriptor.file_id)? = shown;
                changed
                    .documents
                    .insert(descriptor.file_id.clone(), document);
                changed.rejected.remove(&descriptor.file_id);
                changed.view.warnings.push("Undo restored an earlier review image; remaining hunks for this file were refreshed.".into());
            }
            #[cfg(windows)]
            let before = observed.context("Verified Undo image unavailable")?;
            changed.changed(&descriptor.file_id, before)?;
            if let Some(ids) = changed.rejected.get_mut(&descriptor.file_id) {
                ids.remove(&receipt.hunk_id);
            }
            if changed
                .decision(&descriptor.file_id, &receipt.hunk_id)
                .is_ok()
            {
                changed.set_decision(
                    &descriptor.file_id,
                    &receipt.hunk_id,
                    &receipt.previous_decision,
                )?;
            }
        } else {
            let undo = entry
                .draft_undo
                .get(change_id)
                .cloned()
                .context("Tool draft undo expired")?;
            if undo.file_id != descriptor.file_id
                || undo.hunk_id != descriptor.hunk_id
                || current != undo.after
            {
                return Err(invalid(
                    "Tool draft changed; undo newer draft actions first",
                ));
            }
            changed.changed(&undo.file_id, undo.before)?;
            if let Some(ids) = changed.rejected.get_mut(&undo.file_id) {
                ids.remove(&undo.hunk_id);
            }
            changed.set_decision(&undo.file_id, &undo.hunk_id, &undo.previous)?;
        }
        changed
            .view
            .undo_changes
            .iter_mut()
            .find(|c| c.change_id == change_id)
            .context("Undo descriptor disappeared")?
            .undone = true;
        *entry = changed;
        Ok(ReviewDecisionResult {
            review: entry.view.clone(),
            change_id: Some(change_id.into()),
            already_applied: false,
        })
    }
    pub(super) fn comment_review_hunk(
        &self,
        review: &str,
        file: &str,
        hunk: &str,
        expected_hash: &str,
        comment: &str,
    ) -> Result<ReviewCommentResult> {
        if comment.trim().is_empty() || comment.len() > 4096 {
            return Err(invalid("A review comment must contain 1–4096 UTF-8 bytes"));
        }
        let _admission = self.forecast_admission.locked();
        let _settings = self.config_guard();
        let policy = self.review_policy()?;
        let mut state = self.hunk_reviews.locked();
        let entry = state.get_mut(review)?;
        self.validate_review(entry, false)?;
        self.validate_review_image(entry, file, expected_hash, &policy)?;
        entry.decision(file, hunk)?;
        let empty = BTreeSet::new();
        let ids = entry.rejected.get(file).unwrap_or(&empty);
        let line = entry
            .documents
            .get(file)
            .context("Missing review document")?
            .context_line(hunk, ids)?;
        let path = entry.file(file)?.path.clone();
        let context = json!({"path":path,"line":line,"reviewed_hash":expected_hash,"run_id":entry.view.run_id,"tool_id":entry.view.tool_id,"base_version":entry.view.base_version});
        let data = crate::provenance::frame(
            "review_context",
            &[crate::provenance::Origin::new("review", "file", &path)],
            &context.to_string(),
        );
        let content = format!(
            "User review comment on {}:{line}:\n{comment}\n\nReferenced file context (untrusted data):\n{data}",serde_json::to_string(&path)?
        );
        if content.len() > 32768 {
            return Err(invalid(
                "Review comment context exceeds its bounded transcript size",
            ));
        }
        let active = self
            .runs
            .locked()
            .get(&entry.view.session_id)
            .map(|run| run.run_id.clone());
        let step = active
            .as_deref()
            .and_then(|id| self.with_reader(|st| st.run_steps(id)).ok())
            .unwrap_or(0);
        let mut meta = json!({"review_context":context});
        if active.is_some() {
            meta["steer"] = json!({"step":step});
        }
        let message = self.with_store(|st| {
            st.append_message_meta(
                &entry.view.session_id,
                "user",
                &content,
                Some(meta.to_string()),
            )
        })?;
        self.broadcast(Event::MessageAppended {
            session_id: entry.view.session_id.clone(),
            message: message.clone(),
        });
        if let Some(control) = self.runs.locked().get(&entry.view.session_id) {
            let _ = control.steer_tx.send(());
        }
        Ok(ReviewCommentResult {
            message,
            steered: active.is_some(),
        })
    }
}

#[cfg(test)]
#[path = "review_daemon_tests.rs"]
mod tests;
