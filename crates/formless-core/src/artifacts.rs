//! Artifact retention (SPEC §9.6, §15): stored Tool outputs are removed
//! past `artifacts.max_age_days` and, oldest first, beyond
//! `artifacts.max_total_mb`; deleting a Session deletes its artifacts.

use crate::config::ArtifactsCfg;
use crate::Store;
use anyhow::Result;

/// What a collection removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Collected {
    pub files: usize,
    pub bytes: u64,
}

/// Apply the retention caps: files and rows both go.
pub fn gc(store: &Store, cfg: &ArtifactsCfg) -> Result<Collected> {
    let all = store.list_artifacts()?;
    let cutoff = (cfg.max_age_days > 0).then(|| {
        (chrono::Utc::now() - chrono::Duration::days(cfg.max_age_days as i64)).to_rfc3339()
    });
    let cap = cfg.max_total_mb.saturating_mul(1_000_000);
    let mut total: u64 = all.iter().map(|a| a.2.max(0) as u64).sum();
    let mut doomed = Vec::new();
    let mut out = Collected::default();
    // Oldest first: past the age limit, or still over the size cap.
    for (id, path, bytes, created) in all {
        let too_old = cutoff.as_deref().is_some_and(|c| created.as_str() < c);
        let over_cap = cap > 0 && total > cap;
        if !(too_old || over_cap) {
            continue;
        }
        let bytes = bytes.max(0) as u64;
        total = total.saturating_sub(bytes);
        let _ = std::fs::remove_file(&path);
        out.files += 1;
        out.bytes += bytes;
        doomed.push(id);
    }
    store.delete_artifact_rows(&doomed)?;
    // Attachments among them are marked expired where they were sent (#858).
    store.mark_attachments_expired(&doomed)?;
    Ok(out)
}

/// What [`gc`] would remove now: (files, bytes); nothing is touched.
pub fn gc_preview(store: &Store, cfg: &ArtifactsCfg) -> Result<(usize, u64)> {
    let all = store.list_artifacts()?;
    let cutoff = (cfg.max_age_days > 0).then(|| {
        (chrono::Utc::now() - chrono::Duration::days(cfg.max_age_days as i64)).to_rfc3339()
    });
    let cap = cfg.max_total_mb.saturating_mul(1_000_000);
    let mut total: u64 = all.iter().map(|a| a.2.max(0) as u64).sum();
    let (mut files, mut bytes) = (0, 0);
    for (_, _, b, created) in all {
        let too_old = cutoff.as_deref().is_some_and(|c| created.as_str() < c);
        if too_old || (cap > 0 && total > cap) {
            let b = b.max(0) as u64;
            total = total.saturating_sub(b);
            files += 1;
            bytes += b;
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
    let known: std::collections::HashSet<std::path::PathBuf> = rows
        .iter()
        .map(|(_, p, _, _)| std::path::PathBuf::from(p))
        .collect();
    let mut out = Reconciled::default();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() || known.contains(&path) {
            continue;
        }
        let young = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_none_or(|age| age < grace);
        if young {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            out.orphan_files += 1;
            out.orphan_bytes += meta.len();
        }
    }
    for entry in std::fs::read_dir(emitting_dir(dir))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(meta) = entry.metadata() else { continue };
        let abandoned = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > EMIT_ABANDONED.max(grace));
        if meta.is_file() && abandoned && std::fs::remove_file(entry.path()).is_ok() {
            out.orphan_files += 1;
            out.orphan_bytes += meta.len();
        }
    }
    // A row is only dropped once its file has had time to appear (another
    // process may be replacing it right now).
    let cutoff = chrono::Duration::from_std(grace)
        .ok()
        .map(|g| (chrono::Utc::now() - g).to_rfc3339());
    let gone: Vec<String> = rows
        .into_iter()
        .filter(|(_, p, _, created)| {
            !std::path::Path::new(p).exists()
                && cutoff.as_deref().is_none_or(|c| created.as_str() < c)
        })
        .map(|(id, ..)| id)
        .collect();
    out.missing_rows = gone.len();
    store.delete_artifact_rows(&gone)?;
    Ok(out)
}

/// Delete a Session's artifact files and rows (call before the Session).
pub fn remove_for_session(store: &Store, session_id: &str) -> Result<()> {
    for path in store.session_artifact_paths(session_id)? {
        let _ = std::fs::remove_file(path);
    }
    store.delete_session_artifact_rows(session_id)
}

/// (count, bytes) of stored artifacts.
pub fn usage(store: &Store) -> Result<(usize, u64)> {
    let all = store.list_artifacts()?;
    Ok((all.len(), all.iter().map(|a| a.2.max(0) as u64).sum()))
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
