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

/// Make disk and database agree (#609): a file in `dir` with no row and
/// older than `grace` (so an `emit` still writing is left alone) is
/// removed, and a row whose file is gone is dropped.
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
    let gone: Vec<String> = rows
        .into_iter()
        .filter(|(_, p, _, _)| !std::path::Path::new(p).exists())
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
        // Too young to touch: an emit may still be writing it.
        let r = reconcile(&store, &dir, std::time::Duration::from_secs(3600)).unwrap();
        assert_eq!((r.orphan_files, r.missing_rows), (0, 1));
        let r = reconcile(&store, &dir, std::time::Duration::ZERO).unwrap();
        assert_eq!((r.orphan_files, r.orphan_bytes), (1, 5));
        assert!(kept.exists() && !dir.join("orphan.txt").exists());
        let (n, bytes) = usage(&store).unwrap();
        assert_eq!((n, bytes), (1, 4), "usage matches the disk");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
