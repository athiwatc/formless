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
        Ok(serde_json::to_vec(&self.view)?.len()
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
            let current =
                ReviewPath::open(std::path::Path::new(&shown.path), policy)?.read(policy)?;
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
            let image = match ReviewPath::open(std::path::Path::new(&record.path), &policy)
                .and_then(|p| p.read(&policy))
            {
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
            if original
                .as_ref()
                .is_some_and(|original| original.bytes.is_some() && !original.security_complete)
            {
                entry.view.files.push(excluded(&record.path,
                    "Original bytes were captured without fully restorable metadata; this file remains read-only".into(), before_hash, image.hash()));
                continue;
            }
            if record.existed && image.bytes.is_none() {
                let reason = original.as_ref().map_or(Some("Deleted file has no recorded original metadata; whole-file restore remains separate."), |original| original.write_restriction());
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
            if let Some(reason) = image.write_restriction() {
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
                match ReviewPath::open(std::path::Path::new(&record.path), policy)
                    .and_then(|p| p.read(policy))
                {
                    Ok(image) => image,
                    Err(_) => {
                        entry.view.warnings.push(format!("Undo receipt {} cannot safely inspect its recorded file; no automatic write is allowed.",record.id));
                        continue;
                    }
                }
            };
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
        policy: &formless_runtime::sensitive_paths::PathPolicy,
    ) -> Result<(Receipt, Vec<String>)> {
        // New Invocation starts and snapshot-before-write admission use this
        // same Store mutex. Keep the final known-work check through commit.
        let store = self.store.locked();
        self.review_profile_commit_idle(&store)?;
        let journal =
            Journal::open(&self.paths, true)?.context("Missing private review journal")?;
        journal.prepare(&mut receipt)?;
        Self::audit_review_receipt_in(&store, &receipt, "prepared").context(
            "Audit unavailable; no file was changed. The prepared private receipt is retained.",
        )?;
        let (before, after) = receipt.images()?;
        let access = ReviewPath::open(std::path::Path::new(&receipt.path), policy)?;
        let outcome = access.replace(&before, &after, policy);
        let observed = access.read(policy);
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
        Ok((receipt, warnings))
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
            receipt.id = change_id.clone();
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
            let (_, warnings) = self.apply_review_file(receipt, &policy)?;
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
            let store = self.store.locked();
            self.review_profile_commit_idle(&store)?;
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
            let access = ReviewPath::open(std::path::Path::new(&receipt.path), &policy)?;
            let outcome = access.replace(&after, &before, &policy);
            let observed = access.read(&policy);
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
