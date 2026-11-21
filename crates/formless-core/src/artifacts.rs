//! Artifact retention (SPEC §9.6, §15): stored Tool outputs are removed
//! past `artifacts.max_age_days` and, oldest first, beyond
//! `artifacts.max_total_mb`; deleting a Session deletes its artifacts.

use crate::config::ArtifactsCfg;
use crate::Store;
use anyhow::{Context, Result};

#[cfg(test)]
#[path = "artifact_delete_tests.rs"]
mod deletion_tests;

/// What a collection removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Collected {
    pub files: usize,
    pub bytes: u64,
}

type OwnedRow = (String, String, u64, String);
struct Inventory {
    root: Option<crate::artifact_delete::Root>,
    owned: Vec<OwnedRow>,
    rejected: Vec<String>,
}
fn verified_root(
    path: Option<&std::path::Path>,
    open: impl FnOnce(&std::path::Path) -> std::io::Result<crate::artifact_delete::Root>,
) -> Result<Option<crate::artifact_delete::Root>> {
    let path = path.context("artifact directory authority unavailable; records retained")?;
    match open(path) {
        Ok(root) => Ok(Some(root)),
        Err(error) if crate::artifact_delete::rejection(&error).is_some() => Ok(None),
        Err(error) => {
            Err(error).context("artifact directory verification is uncertain; records retained")
        }
    }
}
fn inspect(
    root: &Option<crate::artifact_delete::Root>,
    path: &str,
) -> Result<Option<crate::artifact_delete::VerifiedMetadata>> {
    let Some(root) = root else {
        return Ok(None);
    };
    match root.metadata(std::path::Path::new(path)) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if crate::artifact_delete::rejection(&error).is_some() => Ok(None),
        Err(error) => {
            Err(error).context("artifact file verification is uncertain; records retained")
        }
    }
}
fn remove_verified(root: &Option<crate::artifact_delete::Root>, path: &str) -> Result<bool> {
    let Some(root) = root else {
        return Ok(false);
    };
    match root.remove(std::path::Path::new(path)) {
        Ok(()) => Ok(true),
        Err(error) if crate::artifact_delete::rejection(&error).is_some() => Ok(false),
        Err(error) => Err(error).context("artifact deletion failed; records retained"),
    }
}
fn inventory(
    store: &Store,
    open: impl FnOnce(&std::path::Path) -> std::io::Result<crate::artifact_delete::Root>,
) -> Result<Inventory> {
    let rows = store.list_artifacts()?;
    if rows.is_empty() {
        return Ok(Inventory {
            root: None,
            owned: vec![],
            rejected: vec![],
        });
    }
    let root = verified_root(store.artifact_root.as_deref(), open)?;
    let mut owned = Vec::new();
    let mut rejected = Vec::new();
    for (id, path, _, created) in rows {
        match inspect(&root, &path)? {
            Some(info) => owned.push((id, path, info.len(), created)),
            None => rejected.push(id),
        }
    }
    Ok(Inventory {
        root,
        owned,
        rejected,
    })
}
/// Apply retention only after the entire inventory is verified. Unknown I/O
/// is not proof of absence and never commits row/attachment expiry changes.
pub fn gc(store: &Store, cfg: &ArtifactsCfg) -> Result<Collected> {
    gc_with_open(store, cfg, crate::artifact_delete::Root::open)
}
fn gc_with_open(
    store: &Store,
    cfg: &ArtifactsCfg,
    open: impl FnOnce(&std::path::Path) -> std::io::Result<crate::artifact_delete::Root>,
) -> Result<Collected> {
    let Inventory {
        root,
        owned,
        rejected,
    } = inventory(store, open)?;
    let cutoff = (cfg.max_age_days > 0).then(|| {
        (chrono::Utc::now() - chrono::Duration::days(cfg.max_age_days as i64)).to_rfc3339()
    });
    let cap = cfg.max_total_mb.saturating_mul(1_000_000);
    let mut total = owned.iter().fold(0u64, |sum, a| sum.saturating_add(a.2));
    let mut doomed = rejected;
    let mut out = Collected::default();
    for (id, path, bytes, created) in owned {
        if !(cutoff.as_deref().is_some_and(|c| created.as_str() < c) || (cap > 0 && total > cap)) {
            continue;
        }
        total = total.saturating_sub(bytes);
        if remove_verified(&root, &path)? {
            out.files += 1;
            out.bytes = out.bytes.saturating_add(bytes);
        }
        doomed.push(id);
    }
    store.transaction(|st| {
        st.delete_artifact_rows(&doomed)?;
        st.mark_attachments_expired(&doomed)
    })?;
    Ok(out)
}
/// An uncertain inventory returns an error, never a zero-byte estimate.
pub fn gc_preview(store: &Store, cfg: &ArtifactsCfg) -> Result<(usize, u64)> {
    gc_preview_with_open(store, cfg, crate::artifact_delete::Root::open)
}
fn gc_preview_with_open(
    store: &Store,
    cfg: &ArtifactsCfg,
    open: impl FnOnce(&std::path::Path) -> std::io::Result<crate::artifact_delete::Root>,
) -> Result<(usize, u64)> {
    let Inventory { owned, .. } = inventory(store, open)?;
    let cutoff = (cfg.max_age_days > 0).then(|| {
        (chrono::Utc::now() - chrono::Duration::days(cfg.max_age_days as i64)).to_rfc3339()
    });
    let cap = cfg.max_total_mb.saturating_mul(1_000_000);
    let mut total = owned.iter().fold(0u64, |sum, a| sum.saturating_add(a.2));
    let (mut files, mut bytes) = (0, 0u64);
    for (_, _, n, created) in owned {
        if cutoff.as_deref().is_some_and(|c| created.as_str() < c) || (cap > 0 && total > cap) {
            total = total.saturating_sub(n);
            files += 1;
            bytes = bytes.saturating_add(n);
        }
    }
    Ok((files, bytes))
}

/// What [`reconcile`] fixed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Files with no row, removed, and their bytes.
    pub orphan_files: usize,
    pub orphan_bytes: u64,
    /// Rows whose file was gone, dropped.
    pub missing_rows: usize,
}

/// Where `emit` writes while its Invocation runs; the file moves into the
/// artifacts folder when it is registered (#1105).
pub fn emitting_dir(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("emitting")
}

/// Move a finished emit file out of [`emitting_dir`] into `dir`.
pub fn settle_emit(dir: &std::path::Path, path: &std::path::Path) -> std::path::PathBuf {
    let Some(name) = path.file_name() else {
        return path.to_path_buf();
    };
    let dest = dir.join(name);
    match std::fs::rename(path, &dest) {
        Ok(()) => dest,
        Err(_) => path.to_path_buf(),
    }
}

/// An unregistered emit file this old belongs to no live Invocation.
const EMIT_ABANDONED: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// Make disk and database agree (#609): a file in `dir` with no row and
/// older than `grace` is removed, and a row whose file is gone, and was
/// made longer than `grace` ago, is dropped. Files an `emit` is still
/// writing live in [`emitting_dir`] and are left alone unless abandoned
/// for a week (#1105).
pub fn reconcile(
    store: &Store,
    dir: &std::path::Path,
    grace: std::time::Duration,
) -> Result<Reconciled> {
    let rows = store.list_artifacts()?;
    let root = verified_root(Some(dir), crate::artifact_delete::Root::open)?;
    let mut known = std::collections::HashSet::new();
    // Protect actual registered files, including valid short/case/hardlink
    // aliases. Uncertain rows abort before orphan classification/deletion.
    for (_, path, _, _) in &rows {
        if let Some(info) = inspect(&root, path)? {
            known.insert(info.identity);
        }
    }
    let mut out = Reconciled::default();
    fn entries(
        root: &Option<crate::artifact_delete::Root>,
        path: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>> {
        let Some(root) = root else {
            return Ok(vec![]);
        };
        match root.entries(path) {
            Ok(entries) => Ok(entries),
            Err(error) if crate::artifact_delete::rejection(&error).is_some() => Ok(vec![]),
            Err(error) => Err(error).context("artifact enumeration uncertain; records retained"),
        }
    }
    for path in entries(&root, std::path::Path::new(""))? {
        let text = path
            .to_str()
            .context("artifact filename encoding is uncertain; records retained")?;
        let Some(meta) = inspect(&root, text)? else {
            continue;
        };
        if known.contains(&meta.identity) {
            continue;
        }
        let young = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_none_or(|age| age < grace);
        if !young && remove_verified(&root, text)? {
            out.orphan_files += 1;
            out.orphan_bytes = out.orphan_bytes.saturating_add(meta.len());
        }
    }
    for path in entries(&root, std::path::Path::new("emitting"))? {
        let text = path
            .to_str()
            .context("artifact filename encoding is uncertain; records retained")?;
        let Some(meta) = inspect(&root, text)? else {
            continue;
        };
        if known.contains(&meta.identity) {
            continue;
        }
        let abandoned = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > EMIT_ABANDONED.max(grace));
        if abandoned && remove_verified(&root, text)? {
            out.orphan_files += 1;
            out.orphan_bytes = out.orphan_bytes.saturating_add(meta.len());
        }
    }
    // A row is only dropped once its file has had time to appear (another
    // process may be replacing it right now).
    let cutoff = chrono::Duration::from_std(grace)
        .ok()
        .map(|g| (chrono::Utc::now() - g).to_rfc3339());
    let mut gone = Vec::new();
    for (id, path, _, created) in rows {
        if inspect(&root, &path)?.is_none()
            && cutoff.as_deref().is_none_or(|c| created.as_str() < c)
        {
            gone.push(id);
        }
    }
    out.missing_rows = gone.len();
    store.delete_artifact_rows(&gone)?;
    Ok(out)
}

/// Delete a Session's artifact files and rows (call before the Session).
pub fn remove_for_session(store: &Store, session_id: &str) -> Result<()> {
    let family = store.topic_session_family(session_id)?;
    let survivors = store
        .list_topics()?
        .into_iter()
        .filter(|topic| {
            !matches!(
                topic.status,
                formless_protocol::TopicStatus::PendingApproval
                    | formless_protocol::TopicStatus::Rejected
            )
        })
        .filter_map(|topic| topic.topic_session_id)
        .filter(|session| !family.contains(session))
        .collect::<Vec<_>>();
    let mut doomed = Vec::new();
    let mut retained = std::collections::HashSet::new();
    for (id, path) in store.session_artifact_records(session_id)? {
        let mut keep = false;
        for session in &survivors {
            if store.topic_artifact_access(&id, session, None, Some(session_id))? {
                keep = true;
                break;
            }
        }
        if keep {
            retained.insert(path);
        } else {
            doomed.push((id, path));
        }
    }
    let paths = doomed
        .iter()
        .map(|(_, path)| path)
        .filter(|path| !retained.contains(*path))
        .collect::<Vec<_>>();
    if !paths.is_empty() {
        let root = verified_root(
            store.artifact_root.as_deref(),
            crate::artifact_delete::Root::open,
        )?;
        for path in &paths {
            inspect(&root, path)?;
        }
        for path in &paths {
            remove_verified(&root, path)?;
        }
    }
    store.delete_artifact_rows(&doomed.into_iter().map(|(id, _)| id).collect::<Vec<_>>())
}

/// Delete one recorded artifact; metadata from held/imported JSON is never
/// authority. Unknown verification/deletion errors preserve the durable row.
pub(crate) fn remove_record(store: &Store, id: &str) -> Result<()> {
    if let Some((_, path, _)) = store.get_artifact(id)? {
        let root = verified_root(
            store.artifact_root.as_deref(),
            crate::artifact_delete::Root::open,
        )?;
        inspect(&root, &path)?;
        remove_verified(&root, &path)?;
    }
    store.delete_artifact_rows(&[id.to_string()])
}

/// (count, bytes) of stored artifacts.
pub fn usage(store: &Store) -> Result<(usize, u64)> {
    let all = store.list_artifacts()?;
    Ok((
        all.len(),
        all.iter()
            .fold(0u64, |sum, a| sum.saturating_add(a.2.max(0) as u64)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphans_both_ways_are_reconciled() {
        let dir = std::env::temp_dir().join(format!("fl-recon-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open_in_memory().unwrap();
        let kept = dir.join("kept.txt");
        std::fs::write(&kept, "keep").unwrap();
        store
            .save_artifact(None, &kept.display().to_string(), "", 4)
            .unwrap();
        store
            .save_artifact(None, &dir.join("gone.txt").display().to_string(), "", 9)
            .unwrap();
        std::fs::write(dir.join("orphan.txt"), "12345").unwrap();
        // Too young to touch: a file or row may still be on its way.
        let r = reconcile(&store, &dir, std::time::Duration::from_secs(3600)).unwrap();
        assert_eq!((r.orphan_files, r.missing_rows), (0, 0));
        let r = reconcile(&store, &dir, std::time::Duration::ZERO).unwrap();
        assert_eq!((r.orphan_files, r.orphan_bytes, r.missing_rows), (1, 5, 1));
        assert!(kept.exists() && !dir.join("orphan.txt").exists());
        let (n, bytes) = usage(&store).unwrap();
        assert_eq!((n, bytes), (1, 4), "usage matches the disk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An emit idle for over the grace keeps its early output: it is
    /// written outside the reconciled folder until registered (#1105).
    #[test]
    fn a_live_emit_file_survives_reconcile() {
        let dir = std::env::temp_dir().join(format!("fl-emit-{}", uuid::Uuid::new_v4()));
        let emitting = emitting_dir(&dir);
        std::fs::create_dir_all(&emitting).unwrap();
        let store = Store::open_in_memory().unwrap();
        let file = emitting.join("emit-x.txt");
        std::fs::write(&file, "early ").unwrap();
        // An hour quiet (simulated by a zero grace), then reconcile.
        reconcile(&store, &dir, std::time::Duration::ZERO).unwrap();
        assert!(file.exists(), "a live emit is left alone");
        std::fs::write(&file, "early late").unwrap();
        let settled = settle_emit(&dir, &file);
        assert_eq!(settled.parent(), Some(dir.as_path()));
        assert_eq!(std::fs::read_to_string(settled).unwrap(), "early late");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
