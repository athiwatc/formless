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
