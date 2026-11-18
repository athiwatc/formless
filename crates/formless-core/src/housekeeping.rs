//! Disk use, clean-up and backups (SPEC §9.6): what Formless stores and
//! where, `formless gc`, and rotated database backups.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::{Paths, Store};

/// MCP server logs larger than this are cut to their last part by `gc`.
const MCP_LOG_KEEP: u64 = 1_000_000;
/// Pre-migration database copies kept by `gc` (newest first).
const MIGRATION_BACKUPS_KEEP: usize = 2;
/// Recovery points have independent retention from daily snapshots.
pub const RESTORE_BACKUPS_KEEP: usize = 7;

/// Bytes in a file or (recursively) a folder; 0 when missing.
pub fn path_bytes(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| path_bytes(&e.path())).sum())
            .unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// The database with its WAL and shared-memory files.
pub fn db_bytes(paths: &Paths) -> u64 {
    let db = paths.db_file();
    ["", "-wal", "-shm"]
        .iter()
        .map(|s| path_bytes(&PathBuf::from(format!("{}{s}", db.display()))))
        .sum()
}

/// Where rotated backups go.
pub fn backup_dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join("backups")
}

/// Copies made before schema migrations, newest first.
pub(crate) fn migration_backups(paths: &Paths) -> Vec<PathBuf> {
    let db = paths.db_file();
    let (Some(dir), Some(name)) = (db.parent(), db.file_name().and_then(|n| n.to_str())) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&format!("{name}.v")) && n.ends_with(".bak"))
        })
        .collect();
    // Schema prefixes have different widths (v9 sorts after v52). Compare
    // the timestamp after the version instead; malformed legacy names sort
    // oldest.
    found.sort_by_cached_key(|p| {
        let stamp = p
            .file_name()?
            .to_str()?
            .strip_prefix(&format!("{name}.v"))?
            .split_once('-')?
            .1
            .strip_suffix(".bak")?;
        parse_backup_stamp(stamp)
    });
    found.reverse();
    found
}

/// Disk use by kind: the database (and roughly what fills it), artifacts,
/// logs, skills and backups.
pub fn usage(paths: &Paths, store: &Store) -> Result<Value> {
    let (artifact_count, artifact_bytes) = crate::artifacts::usage(store)?;
    let inside = store.content_bytes()?;
    let (tool_store_bytes, orphaned_namespaces, orphaned_bytes) = store.tool_store_usage()?;
    let snapshots = store.run_file_usage()?;
    let mcp_logs = path_bytes(&paths.data_dir.join("mcp"));
    let backups = path_bytes(&backup_dir(paths))
        + migration_backups(paths)
            .iter()
            .map(|p| path_bytes(p))
            .sum::<u64>();
    let rows = vec![
        json!({"kind": "database", "bytes": db_bytes(paths), "detail": "formless.db (with its WAL)"}),
        json!({"kind": "sessions", "bytes": inside["messages"], "detail": "messages, inside the database", "inside_db": true}),
        json!({"kind": "invocation logs", "bytes": inside["invocations"], "detail": "Tool inputs and outputs, inside the database", "inside_db": true}),
        json!({"kind": "tool versions", "bytes": inside["tool_versions"], "detail": "every version's code, inside the database", "inside_db": true}),
        json!({"kind": "Tool store", "bytes": tool_store_bytes, "orphaned_namespaces": orphaned_namespaces, "orphaned_bytes": orphaned_bytes, "detail": format!("inside the database, {orphaned_namespaces} shared namespaces without a Tool version ({} retained)", human_bytes(orphaned_bytes as u64)), "inside_db": true}),
        json!({"kind": "file snapshots", "bytes": snapshots.retained_bytes.saturating_add(snapshots.after_state_bytes), "rows": snapshots.rows, "retained_files": snapshots.retained_files, "before_bytes": snapshots.before_bytes, "metadata_bytes": snapshots.metadata_bytes, "after_state_bytes": snapshots.after_state_bytes, "detail": format!("{} recorded paths, {} retained snapshots, inside the database", snapshots.rows, snapshots.retained_files), "inside_db": true}),
        json!({"kind": "artifacts", "bytes": artifact_bytes, "detail": format!("{artifact_count} stored outputs")}),
        json!({"kind": "logs", "bytes": crate::logs::log_usage(paths) + mcp_logs, "detail": "daemon and MCP server logs"}),
        json!({"kind": "skills", "bytes": path_bytes(&paths.data_dir.join("skills")), "detail": "installed skill files"}),
        json!({"kind": "backups", "bytes": backups, "detail": "daily, pre-migration and before-restore database copies", "before_restore": list_restore_backups(paths), "before_restore_keep": RESTORE_BACKUPS_KEEP}),
    ];
    let total = path_bytes(&paths.data_dir);
    Ok(
        json!({"data_dir": paths.data_dir.display().to_string(), "total_bytes": total, "kinds": rows}),
    )
}

/// What `gc` would free (dry run) or freed: one row per kind.
pub fn gc(paths: &Paths, store: &Store, cfg: &Config, dry_run: bool) -> Result<Value> {
    let mut rows = Vec::new();
    // Artifacts past their retention.
    let (n, bytes) = if dry_run {
        crate::artifacts::gc_preview(store, &cfg.artifacts)?
    } else {
        let c = crate::artifacts::gc(store, &cfg.artifacts)?;
        (c.files, c.bytes)
    };
    // Files the database lost track of (#609).
    if !dry_run {
        crate::artifacts::reconcile(
            store,
            &paths.data_dir.join("artifacts"),
            std::time::Duration::from_secs(3600),
        )?;
    }
    rows.push(json!({"kind": "artifacts", "items": n, "bytes": bytes,
        "detail": format!("older than {} days or over {} MB (artifacts.*)", cfg.artifacts.max_age_days, cfg.artifacts.max_total_mb)}));
    // Expiry releases recovery payload while preserving the latest path history.
    let snapshots = store.retain_run_files(&cfg.run_files, dry_run)?;
    rows.push(json!({"kind": "file snapshots", "items": snapshots.files, "runs": snapshots.runs,
        "bytes": snapshots.bytes, "pinned_bytes": snapshots.pinned_bytes,
        "remaining_bytes": snapshots.remaining_bytes, "over_budget_bytes": snapshots.over_budget_bytes,
        "detail": format!("keep {} Runs per Session, {} days, {} MiB (run_files.*); active/latest Runs protected; expired paths remain listed", cfg.run_files.keep_runs_per_session, cfg.run_files.max_age_days, cfg.run_files.max_total_mb)}));
    // MCP logs cut to their last part.
    let (mut n, mut bytes) = (0, 0u64);
    for e in std::fs::read_dir(paths.data_dir.join("mcp"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let p = e.path();
        let len = path_bytes(&p);
        if p.extension().is_some_and(|x| x == "log") && len > MCP_LOG_KEEP {
            n += 1;
            bytes += len - MCP_LOG_KEEP;
            if !dry_run {
                keep_tail(&p, MCP_LOG_KEEP)?;
            }
        }
    }
    rows.push(json!({"kind": "MCP logs", "items": n, "bytes": bytes,
        "detail": "cut to their last 1 MB"}));
    // Recovery copies are removed only after a newer one has actually passed
    // structural verification. An active failed-migration receipt pins its copy.
    let migrations = migration_backups(paths);
    let receipt_path = paths.db_file().with_extension("migration-backup.json");
    let pin_result: Result<Option<String>> = (|| {
        let bytes = match std::fs::read(&receipt_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("read pinned migration recovery receipt"),
        };
        let receipt: Value =
            serde_json::from_slice(&bytes).context("parse pinned migration recovery receipt")?;
        let name = receipt["backup_name"]
            .as_str()
            .context("migration recovery receipt has no backup_name")?;
        let prefix = format!(
            "{}.v",
            paths
                .db_file()
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        );
        if Path::new(name).file_name().and_then(|p| p.to_str()) != Some(name)
            || !name.starts_with(&prefix)
            || !name.ends_with(".bak")
        {
            return Err(anyhow!("invalid pinned migration recovery filename"));
        }
        Ok(Some(name.to_string()))
    })();
    let (pin, pin_error) = match pin_result {
        Ok(pin) => (pin, None),
        Err(error) => {
            tracing::warn!("migration retention blocked by recovery receipt: {error:#}");
            (
                None,
                Some(format!("recovery receipt unreadable or invalid: {error:#}")),
            )
        }
    };
    let candidates: Vec<_> = migrations
        .iter()
        .skip(MIGRATION_BACKUPS_KEEP)
        .filter(|p| p.file_name().and_then(|n| n.to_str()) != pin.as_deref())
        .cloned()
        .collect();
    let mut removed = Vec::new();
    let mut bytes = 0;
    let mut verification_error = pin_error;
    if !candidates.is_empty() && verification_error.is_none() {
        let newest = migrations
            .first()
            .expect("candidates require a newest copy");
        if dry_run {
            // A preview never writes verification receipts or guesses that an
            // unverified snapshot will pass. Show the gate instead.
            if crate::backup_verification::verified(paths, newest).is_some() {
                removed = candidates;
                bytes = removed.iter().map(|p| path_bytes(p)).sum();
            } else {
                verification_error = Some("newest copy needs verification".to_string());
            }
        } else {
            match crate::backup_verification::verify(paths, newest) {
                Ok(_) => {
                    for path in candidates {
                        let size = path_bytes(&path);
                        crate::backup_verification::remove_copy(paths, &path)?;
                        bytes += size;
                        tracing::info!("pruned verified migration backup {}", path.display());
                        store.audit(
                            "daemon",
                            "backup.prune",
                            &format!(
                                "{} ({size} bytes); newest {} verified",
                                path.display(),
                                newest.display()
                            ),
                        )?;
                        removed.push(path);
                    }
                }
                Err(error) => {
                    tracing::warn!("migration backup retention skipped: {error:#}");
                    verification_error = Some(format!("{error:#}"));
                }
            }
        }
    }
    rows.push(json!({"kind":"migration backups","items":removed.len(),"bytes":bytes,
        "detail":format!("keep newest {MIGRATION_BACKUPS_KEEP} and receipt-pinned recovery; verify newest before pruning"),"verification_error":verification_error}));
    // Free pages left behind in the database.
    let free = store.free_bytes()?;
    if !dry_run && free > 0 {
        store.vacuum()?;
    }
    rows.push(
        json!({"kind": "database free space", "items": 0, "bytes": free,
        "detail": "reclaimed with VACUUM"}),
    );
    let total: u64 = rows.iter().map(|r| r["bytes"].as_u64().unwrap_or(0)).sum();
    Ok(json!({"dry_run": dry_run, "total_bytes": total, "kinds": rows}))
}

/// Keep only the last `keep` bytes of a file (from a line start).
fn keep_tail(p: &Path, keep: u64) -> Result<()> {
    let data = std::fs::read(p)?;
    let start = data.len().saturating_sub(keep as usize);
    let start = data[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(start, |i| start + i + 1);
    std::fs::write(p, &data[start..])?;
    Ok(())
}

fn parse_backup_stamp(stamp: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(stamp.split('-').next()?, "%Y%m%dT%H%M%S%.fZ").ok()
}

/// Fixed-width fractional UTC time keeps filename order chronological;
/// a random suffix also distinguishes calls made within one clock tick.
fn backup_stamp() -> String {
    format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ"),
        uuid::Uuid::new_v4()
    )
}

/// A consistent copy of the database into `backups/`, keeping the newest
/// `keep` (SPEC §9.6). Returns the new file.
pub fn backup(paths: &Paths, store: &Store, keep: usize) -> Result<PathBuf> {
    let dir = backup_dir(paths);
    std::fs::create_dir_all(&dir)?;
    let stamp = backup_stamp();
    let dest = dir.join(format!("formless-{stamp}.db"));
    store.backup_into(&dest)?;
    crate::backup_verification::verify(paths, &dest)
        .context("verify new backup before retention")?;
    let mut all = list_backups(paths);
    all.reverse(); // newest first
                   // Always retain this verified destination, including after clock rollback
                   // or when an old file has a future timestamp in its name.
    let mut others_kept = 0usize;
    for old in &all {
        if old == &dest {
            continue;
        }
        if others_kept < keep.max(1) - 1 {
            others_kept += 1;
            continue;
        }
        crate::backup_verification::remove_copy(paths, old)?;
        tracing::info!("pruned daily backup {}", old.display());
        store.audit(
            "daemon",
            "backup.prune",
            &format!(
                "daily {}; newest {} verified",
                old.display(),
                dest.display()
            ),
        )?;
    }
    Ok(dest)
}

/// Daily snapshots, oldest first (independent from restore retention).
pub fn list_backups(paths: &Paths) -> Vec<PathBuf> {
    list_copies(paths, "formless-", ".db")
}

/// Before-restore recovery points, oldest first.
pub fn list_restore_backups(paths: &Paths) -> Vec<PathBuf> {
    let mut copies = list_copies(paths, "before-restore-", ".db");
    copies.extend(list_copies(paths, "formless.db.pre-restore-", ""));
    copies.sort_by_cached_key(|path| {
        let at = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|name| {
                name.strip_prefix("formless.db.pre-restore-").or_else(|| {
                    name.strip_prefix("before-restore-")
                        .and_then(|n| n.strip_suffix(".db"))
                })
            })
            .and_then(parse_backup_stamp);
        (at, path.clone())
    });
    copies
}

// New names have no extension, so require the *entire* generated stamp. A
// prefix match alone would mistake a selected snapshot's WAL/SHM for backups.
fn valid_recovery_stamp(stamp: &str) -> bool {
    let Some((timestamp, id)) = stamp.split_once('-') else {
        return false;
    };
    parse_backup_stamp(timestamp).is_some() && id.len() == 36 && uuid::Uuid::parse_str(id).is_ok()
}

fn list_copies(paths: &Paths, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(backup_dir(paths))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with(prefix)
                    && n.ends_with(suffix)
                    && (suffix != "" || n.strip_prefix(prefix).is_some_and(valid_recovery_stamp))
            })
        })
        .collect();
    found.sort_by_cached_key(|p| {
        let at = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(prefix))
            .and_then(|n| n.strip_suffix(suffix))
            .and_then(parse_backup_stamp);
        (at, p.clone())
    });
    found
}

/// When the newest rotated backup was taken.
pub fn last_backup(paths: &Paths) -> Option<(PathBuf, std::time::SystemTime)> {
    let last = list_backups(paths).pop()?;
    let at = std::fs::metadata(&last).and_then(|m| m.modified()).ok()?;
    Some((last, at))
}

/// Replace the database with `file` (the daemon must be stopped). The file
/// is checked first (a Formless database this build can open), and the
/// current database is backed up next to it. Returns that backup.
pub fn restore(paths: &Paths, file: &Path) -> Result<PathBuf> {
    restore_report(paths, file).map(|outcome| outcome.saved)
}

#[derive(Debug)]
pub struct RestoreReport {
    pub saved: PathBuf,
    /// A failed diagnostic never turns a completed atomic replacement into a
    /// purported failed restore. The caller must surface the explicit warning.
    pub references: std::result::Result<crate::restore_references::Report, String>,
}

impl RestoreReport {
    pub fn warnings(&self) -> Vec<String> {
        match &self.references {
            Ok(report) => report.warnings(),
            Err(error) => vec![format!("database restored, but file-reference check could not complete: {error}; file recovery is unverified")],
        }
    }
}

pub fn restore_report(paths: &Paths, file: &Path) -> Result<RestoreReport> {
    restore_with(paths, file, |stage, db| {
        std::fs::rename(stage, db).context("atomically replace the profile database")
    })
}

// The replace boundary permits deterministic failure testing without global
// hooks. Both guards stay alive until migration, checkpoint and replacement end.
fn restore_with(
    paths: &Paths,
    file: &Path,
    replace: impl FnOnce(&Path, &Path) -> Result<()>,
) -> Result<RestoreReport> {
    restore_with_check(paths, file, replace, crate::restore_references::inspect)
}

// The diagnostic boundary permits a deterministic post-commit failure without
// malformed source schemas, environment hooks or weakening staged verification.
fn restore_with_check(
    paths: &Paths,
    file: &Path,
    replace: impl FnOnce(&Path, &Path) -> Result<()>,
    inspect: impl FnOnce(&Path, &Paths) -> Result<crate::restore_references::Report>,
) -> Result<RestoreReport> {
    let _gate = crate::profile_lock::ProfileLock::launcher(paths, std::time::Duration::ZERO)?;
    let _owner = crate::profile_lock::ProfileLock::owner(paths)?;
    if crate::daemon::endpoint_occupied(paths)? {
        return Err(anyhow!(
            "a daemon is still reachable; stop it before restoring"
        ));
    }
    let probe =
        rusqlite::Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("cannot open {}", file.display()))?;
    let ok: String = probe
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .context("not a SQLite database")?;
    if ok != "ok" {
        return Err(anyhow!(
            "{} failed its integrity check: {ok}",
            file.display()
        ));
    }
    let version: i64 = probe.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let has_sessions: bool = probe.query_row(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name='sessions'",
        [],
        |r| r.get(0),
    )?;
    if !has_sessions {
        return Err(anyhow!("{} is not a Formless database", file.display()));
    }
    if version > crate::storage::SCHEMA_VERSION {
        return Err(anyhow!(
            "{} is schema v{version}, newer than this build (v{}); upgrade formless first",
            file.display(),
            crate::storage::SCHEMA_VERSION
        ));
    }
    let db = paths.db_file();
    let dir = backup_dir(paths);
    std::fs::create_dir_all(&dir)?;
    let saved = dir.join(format!("formless.db.pre-restore-{}", backup_stamp()));
    if db.exists() {
        // Never migrate the current file: it may itself need recovery.
        let current =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        snapshot_into(&current, &saved)
            .with_context(|| format!("back up current database to {}", saved.display()))?;
        #[cfg(unix)]
        std::fs::File::open(&dir)?.sync_all()?;
    }
    let stage =
        RestoreStage(db.with_file_name(format!(".formless-restore-{}.db", uuid::Uuid::new_v4())));
    let result = (|| -> Result<()> {
        // Capture the source WAL, including committed uncheckpointed rows.
        snapshot_into(&probe, &stage.0)?;
        drop(probe);
        // Close the selected snapshot before pruning: Windows may refuse to
        // delete an open old recovery point. Retention also applies on a failed
        // migration, so repeated failures cannot grow recovery copies forever.
        let all = list_restore_backups(paths);
        // Both naming generations share one bound. Preserve the new displaced
        // copy across clock rollback, and preserve a selected recovery source.
        let source = std::fs::canonicalize(file).ok();
        let protected: Vec<_> = all
            .iter()
            .filter(|p| {
                **p == saved
                    || source.as_ref().is_some_and(|source| {
                        std::fs::canonicalize(p).ok().as_ref() == Some(source)
                    })
            })
            .collect();
        let mut remaining = RESTORE_BACKUPS_KEEP.saturating_sub(protected.len());
        for old in all.iter().rev() {
            if protected.contains(&old) {
                continue;
            }
            if remaining > 0 {
                remaining -= 1;
                continue;
            }
            crate::backup_verification::remove_copy(paths, old)?;
        }
        #[cfg(unix)]
        std::fs::File::open(&dir)?.sync_all()?;
        drop(Store::open(&stage.0)?);
        let staged = rusqlite::Connection::open(&stage.0)?;
        let ok: String = staged.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if ok != "ok" {
            return Err(anyhow!("staged restore failed integrity check: {ok}"));
        }
        checkpoint(&staged)?;
        staged.pragma_update(None, "journal_mode", "DELETE")?;
        drop(staged);
        crate::backup_verification::inspect(&stage.0)
            .context("verify migrated restore stage before replacing the destination")?;
        sync_file(&stage.0)?;
        // Fold any old WAL into the old canonical file before removing its
        // sidecars. If replacement fails, that old file remains complete.
        if db.exists() {
            let current = rusqlite::Connection::open_with_flags(
                &db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
            )?;
            checkpoint(&current)?;
        }
        for suffix in ["-wal", "-shm"] {
            match std::fs::remove_file(format!("{}{suffix}", db.display())) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        replace(&stage.0, &db)?;
        #[cfg(unix)]
        std::fs::File::open(&paths.data_dir)?.sync_all()?;
        Ok(())
    })();
    result.with_context(|| {
        if saved.is_file() {
            format!("restore did not complete; recovery copy at {}",saved.display())
        } else {
            "restore did not complete; the destination had no database, so no displaced-database recovery copy was created".to_string()
        }
    })?;
    // Both ownership guards remain held while checking the restored rows.
    let references = inspect(&db, paths).map_err(|error| format!("{error:#}"));
    let outcome = RestoreReport { saved, references };
    for warning in outcome.warnings() {
        tracing::warn!("{warning}");
    }
    Ok(outcome)
}

fn snapshot_into(conn: &rusqlite::Connection, path: &Path) -> Result<()> {
    // SQLite permits an existing empty destination; reserve it privately before
    // the first database byte is written and never overwrite a prior snapshot.
    reserve_private(path)?;
    if let Err(error) = conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()]) {
        let _ = std::fs::remove_file(path);
        return Err(error.into());
    }
    sync_file(path)
}

fn sync_file(path: &Path) -> Result<()> {
    // FlushFileBuffers on Windows requires a writable handle.
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .sync_all()?;
    Ok(())
}

fn reserve_private(path: &Path) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    Ok(())
}

fn checkpoint(conn: &rusqlite::Connection) -> Result<()> {
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    if busy != 0 {
        return Err(anyhow!(
            "database is busy; close other database clients before restoring"
        ));
    }
    Ok(())
}

struct RestoreStage(PathBuf);
impl Drop for RestoreStage {
    fn drop(&mut self) {
        // Only this random staging namespace; preserve all canonical recovery
        // receipts and copies from ordinary failed migrations (#1208).
        if let (Some(parent), Some(stem)) =
            (self.0.parent(), self.0.file_name().and_then(|n| n.to_str()))
        {
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name == stem
                        || name.starts_with(&format!("{stem}-"))
                        || name.starts_with(&format!("{stem}.v"))
                        || entry.path() == self.0.with_extension("migration-backup.json")
                    {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

/// The daemon's own memory (RSS bytes), CPU % and CPU time, from `ps`
/// (none on Windows).
pub fn process_usage(pid: u32) -> Value {
    #[cfg(unix)]
    {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=,%cpu=,time=", "-p", &pid.to_string()])
            .output();
        if let Ok(o) = out {
            let text = String::from_utf8_lossy(&o.stdout);
            let f: Vec<&str> = text.split_whitespace().collect();
            if let [rss, cpu, time, ..] = f.as_slice() {
                return json!({
                    "memory_bytes": rss.parse::<u64>().ok().map(|k| k * 1024),
                    "cpu_percent": cpu.parse::<f64>().ok(),
                    "cpu_time": time,
                });
            }
        }
        json!({})
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        json!({})
    }
}

/// "1.2 MB".
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> (Self, Paths) {
            let base = std::env::temp_dir().join(format!("fl-restore-{}", uuid::Uuid::new_v4()));
            let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
            (Self(base), paths)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn seed_external_references(paths: &Paths, make_files: bool) -> Store {
        let store = Store::open(&paths.db_file()).unwrap();
        let artifact = paths.data_dir.join("artifacts/output.txt");
        store
            .save_artifact(None, artifact.to_str().unwrap(), "preview", 6)
            .unwrap();
        if make_files {
            std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
            std::fs::write(&artifact, "output").unwrap();
        }
        for version in [1, 2] {
            let directory = paths.data_dir.join(format!("skills/example/v{version}"));
            store
                .add_skill_version(
                    "example",
                    "fixture skill",
                    version,
                    directory.to_str().unwrap(),
                    "fixture",
                    "user",
                    false,
                )
                .unwrap();
            if make_files {
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(directory.join("SKILL.md"), "# Example").unwrap();
            }
        }
        let conn = rusqlite::Connection::open(paths.db_file()).unwrap();
        conn.execute("UPDATE skills SET retired=1, enabled=0", [])
            .unwrap();
        drop(conn);
        store
    }

    #[test]
    fn fresh_restore_reports_missing_artifacts_and_all_skill_versions_without_deleting_rows() {
        let (fixture, source) = Fixture::new();
        let store = seed_external_references(&source, false);
        let backup = fixture.0.join("backup.db");
        store.backup_into(&backup).unwrap();
        let expected_artifacts = store.list_artifacts().unwrap();
        drop(store);
        let target =
            Paths::from_dirs(fixture.0.join("new-data"), fixture.0.join("new-config")).unwrap();
        let outcome = restore_report(&target, &backup).unwrap();
        let report = outcome.references.as_ref().unwrap();
        assert_eq!(report.artifacts.total, 1);
        assert_eq!(report.artifacts.missing, 1);
        assert_eq!(report.artifacts.foreign, 1);
        assert_eq!(
            report.skill_versions.total, 2,
            "old and current disabled/retired versions are both checked"
        );
        assert_eq!(report.skill_versions.missing, 2);
        assert_eq!(report.skill_versions.foreign, 2);
        assert_eq!(outcome.warnings().len(), 2);
        let restored = Store::open(&target.db_file()).unwrap();
        assert_eq!(
            restored.list_artifacts().unwrap(),
            expected_artifacts,
            "missing rows and paths are recovery clues, not garbage"
        );
        let conn = rusqlite::Connection::open_with_flags(
            target.db_file(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let versions: i64 = conn
            .query_row("SELECT count(*) FROM skill_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(versions, 2);
        drop(conn);
        drop(restored);
    }

    #[test]
    fn existing_source_files_are_foreign_to_a_fresh_restore_and_local_files_pass() {
        let (fixture, source) = Fixture::new();
        let store = seed_external_references(&source, true);
        let backup = fixture.0.join("backup.db");
        store.backup_into(&backup).unwrap();
        drop(store);
        let target =
            Paths::from_dirs(fixture.0.join("new-data"), fixture.0.join("new-config")).unwrap();
        let foreign = restore_report(&target, &backup).unwrap();
        let report = foreign.references.as_ref().unwrap();
        assert_eq!(report.artifacts.missing, 0);
        assert_eq!(
            report.artifacts.foreign, 1,
            "existing old-profile files do not certify portability"
        );
        assert_eq!(report.skill_versions.missing, 0);
        assert_eq!(report.skill_versions.foreign, 2);
        assert_eq!(foreign.warnings().len(), 2);
        let local = restore_report(&source, &backup).unwrap();
        let report = local.references.as_ref().unwrap();
        assert_eq!(report.artifacts.total, 1);
        assert_eq!(report.skill_versions.total, 2);
        assert!(!report.artifacts.has_issues() && !report.skill_versions.has_issues());
        assert!(local.warnings().is_empty());
        // A diagnostic alone is read-only, including database bytes and rows.
        let before = std::fs::read(source.db_file()).unwrap();
        assert_eq!(
            crate::restore_references::inspect(&source.db_file(), &source).unwrap(),
            *report
        );
        assert_eq!(std::fs::read(source.db_file()).unwrap(), before);
        // An existing old-version directory is still incomplete without its
        // manifest, even when the latest version remains readable.
        std::fs::remove_file(source.data_dir.join("skills/example/v1/SKILL.md")).unwrap();
        let incomplete = restore_report(&source, &backup).unwrap();
        let report = incomplete.references.as_ref().unwrap();
        assert_eq!(report.skill_versions.total, 2);
        assert_eq!(report.skill_versions.missing, 1);
        assert_eq!(report.skill_versions.foreign, 0);
    }

    #[test]
    fn reference_diagnostic_failure_is_a_warning_after_a_committed_restore() {
        let (fixture, target) = Fixture::new();
        let original = Store::open(&target.db_file()).unwrap();
        original.create_session(Some("old".into())).unwrap();
        drop(original);
        let source = fixture.0.join("source.db");
        let store = Store::open(&source).unwrap();
        store.create_session(Some("restored".into())).unwrap();
        drop(store);
        let outcome = restore_with_check(
            &target,
            &source,
            |stage, db| {
                std::fs::rename(stage, db)?;
                Ok(())
            },
            |db, _| {
                let committed = rusqlite::Connection::open_with_flags(
                    db,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )?;
                let name: String =
                    committed.query_row("SELECT name FROM sessions", [], |row| row.get(0))?;
                assert_eq!(name, "restored", "injection runs only after commit");
                Err(anyhow!("injected reference diagnostic unavailable"))
            },
        )
        .unwrap();
        assert!(outcome.references.is_err());
        assert!(outcome.warnings()[0]
            .contains("database restored, but file-reference check could not complete"));
        assert!(outcome.saved.is_file());
        let restored = Store::open(&target.db_file()).unwrap();
        assert_eq!(restored.list_sessions().unwrap()[0].name, "restored");
        drop(restored);
    }

    #[test]
    fn restore_rejects_a_concurrent_owner_before_touching_database_or_backups() {
        let (_fixture, paths) = Fixture::new();
        std::fs::write(paths.db_file(), "unchanged").unwrap();
        let owner = crate::profile_lock::ProfileLock::owner(&paths).unwrap();
        let error = restore(&paths, &paths.data_dir.join("missing-source.db")).unwrap_err();
        assert!(error.to_string().contains("owned"));
        assert_eq!(
            std::fs::read_to_string(paths.db_file()).unwrap(),
            "unchanged"
        );
        assert!(!backup_dir(&paths).exists());
        drop(owner);
    }

    #[test]
    fn restore_stages_source_wal_and_keeps_original_on_injected_replace_failure() {
        let (fixture, paths) = Fixture::new();
        let original = Store::open(&paths.db_file()).unwrap();
        original.create_session(Some("original".into())).unwrap();
        drop(original);
        let source = fixture.0.join("source.db");
        let source_store = Store::open(&source).unwrap();
        source_store
            .create_session(Some("committed in source WAL".into()))
            .unwrap();
        assert!(path_bytes(&PathBuf::from(format!("{}-wal", source.display()))) > 0);
        let error = restore_with(&paths, &source, |stage, db| {
            assert_eq!(
                stage.parent(),
                db.parent(),
                "replacement stays on the same filesystem"
            );
            assert!(
                crate::profile_lock::ProfileLock::try_owner(&paths)?.is_none(),
                "ownership spans replacement"
            );
            let conn = rusqlite::Connection::open(stage)?;
            let name: String = conn.query_row("SELECT name FROM sessions", [], |r| r.get(0))?;
            assert_eq!(name, "committed in source WAL");
            Err(anyhow!("injected replacement failure"))
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("injected replacement failure"));
        let current = Store::open(&paths.db_file()).unwrap();
        assert_eq!(current.list_sessions().unwrap()[0].name, "original");
        drop(current);
        assert_eq!(list_restore_backups(&paths).len(), 1);
        assert!(
            std::fs::read_dir(&paths.data_dir)
                .unwrap()
                .flatten()
                .all(|e| !e
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".formless-restore-")),
            "staging database, sidecars and migration receipts are cleaned"
        );
        restore(&paths, &source).unwrap();
        let current = Store::open(&paths.db_file()).unwrap();
        assert_eq!(
            current.list_sessions().unwrap()[0].name,
            "committed in source WAL"
        );
        drop(current);
        drop(source_store);
    }

    #[test]
    fn restore_retention_is_seven_and_independent_of_daily_backups_and_gc() {
        let (fixture, paths) = Fixture::new();
        let current = Store::open(&paths.db_file()).unwrap();
        current.create_session(Some("original".into())).unwrap();
        let daily = backup(&paths, &current, 1).unwrap();
        let source = fixture.0.join("source.db");
        current.backup_into(&source).unwrap();
        drop(current);
        let mut copies = Vec::new();
        for _ in 0..9 {
            copies.push(restore(&paths, &source).unwrap());
        }
        assert_eq!(list_restore_backups(&paths), copies[2..]);
        assert!(!copies[0].exists() && !copies[1].exists());
        assert!(daily.is_file());
        let current = Store::open(&paths.db_file()).unwrap();
        backup(&paths, &current, 1).unwrap();
        assert!(!daily.exists());
        assert_eq!(list_backups(&paths).len(), 1);
        assert_eq!(list_restore_backups(&paths).len(), RESTORE_BACKUPS_KEEP);
        let report = usage(&paths, &current).unwrap();
        let row = report["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["kind"] == "backups")
            .unwrap();
        assert_eq!(
            row["before_restore"].as_array().unwrap().len(),
            RESTORE_BACKUPS_KEEP
        );
        let mut cfg = Config::default();
        cfg.daemon.backups_keep = 0;
        gc(&paths, &current, &cfg, false).unwrap();
        assert_eq!(list_restore_backups(&paths).len(), RESTORE_BACKUPS_KEEP);
        drop(current);
    }

    #[test]
    fn backups_rotate_and_restore_round_trips() {
        let base = std::env::temp_dir().join(format!("fl-hk-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        let s = store.create_session(Some("kept".into())).unwrap().id;
        let first = backup(&paths, &store, 2).unwrap();
        let original = std::fs::read(&first).unwrap();
        assert!(store
            .backup_into(&first)
            .unwrap_err()
            .to_string()
            .contains("already exists"));
        assert_eq!(
            std::fs::read(&first).unwrap(),
            original,
            "existing snapshots are untouched"
        );
        let second = backup(&paths, &store, 2).unwrap();
        assert_ne!(first, second);
        assert!(
            first.exists() && second.exists(),
            "rapid backups both survive"
        );
        let third = backup(&paths, &store, 2).unwrap();
        assert!(second < third, "filename order follows backup creation");
        let all = list_backups(&paths);
        assert_eq!(all.len(), 2, "rotated to backups_keep");
        assert!(!first.exists());
        // Changes after the backup are undone by restoring it.
        store.create_session(Some("later".into())).unwrap();
        // Retain real sidecars containing the later row, then put them back
        // after closing to simulate leftovers from the replaced database.
        let sidecars: Vec<_> = ["-wal", "-shm"]
            .into_iter()
            .map(|suffix| {
                let path = PathBuf::from(format!("{}{suffix}", paths.db_file().display()));
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        drop(store);
        for (path, bytes) in &sidecars {
            std::fs::write(path, bytes).unwrap();
        }
        let saved = restore(&paths, all.last().unwrap()).unwrap();
        assert!(saved.exists());
        assert!(
            sidecars.iter().all(|(path, _)| !path.exists()),
            "stale sidecars are removed"
        );
        let saved_again = restore(&paths, all.last().unwrap()).unwrap();
        assert_ne!(saved, saved_again);
        assert!(
            saved.exists() && saved_again.exists(),
            "restore safety copies survive"
        );
        let store = Store::open(&paths.db_file()).unwrap();
        let names: Vec<String> = store
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["kept"]);
        assert!(store.get_session(&s).unwrap().is_some());
        // Not a database: refused.
        let junk = base.join("junk.db");
        std::fs::write(&junk, "hello").unwrap();
        assert!(restore(&paths, &junk).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn restore_refuses_corrupt_foreign_and_newer_databases_without_touching_live_data() {
        let base = std::env::temp_dir().join(format!("fl-hk-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        store.create_session(Some("untouched".into())).unwrap();
        drop(store);
        let before = std::fs::read(paths.db_file()).unwrap();
        let refused = |file: &Path, reason: &str| {
            let error = restore(&paths, file).unwrap_err().to_string();
            assert!(error.contains(reason), "wrong refusal: {error}");
            assert_eq!(std::fs::read(paths.db_file()).unwrap(), before);
            assert!(
                !backup_dir(&paths).exists(),
                "refusal must not make a safety copy"
            );
        };

        let corrupt = base.join("corrupt.db");
        let conn = rusqlite::Connection::open(&corrupt).unwrap();
        conn.execute_batch("CREATE TABLE sessions(id TEXT); INSERT INTO sessions VALUES('bad');")
            .unwrap();
        let page: usize = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name='sessions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let size: usize = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap();
        drop(conn);
        let mut bytes = std::fs::read(&corrupt).unwrap();
        // Damage the leaf page's freeblock pointer while leaving SQLite's
        // header/schema readable: integrity_check returns a real corruption
        // report, rather than failing to open or parse the database.
        let offset = (page - 1) * size;
        bytes[offset + 1..offset + 3].copy_from_slice(&[0xff, 0xff]);
        std::fs::write(&corrupt, bytes).unwrap();
        refused(&corrupt, "failed its integrity check");

        let foreign = base.join("foreign.db");
        let conn = rusqlite::Connection::open(&foreign).unwrap();
        conn.execute_batch("CREATE TABLE unrelated(id TEXT);")
            .unwrap();
        drop(conn);
        refused(&foreign, "is not a Formless database");

        let newer = base.join("newer.db");
        let conn = rusqlite::Connection::open(&newer).unwrap();
        conn.execute_batch(&format!(
            "CREATE TABLE sessions(id TEXT); PRAGMA user_version={};",
            crate::storage::SCHEMA_VERSION + 1
        ))
        .unwrap();
        drop(conn);
        refused(&newer, "newer than this build");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn failed_restore_migration_keeps_a_restorable_safety_copy() {
        let base = std::env::temp_dir().join(format!("fl-hk-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        store.create_session(Some("original".into())).unwrap();
        drop(store);
        let old = base.join("old.db");
        let store = Store::open(&old).unwrap();
        store
            .create_session(Some("from broken backup".into()))
            .unwrap();
        drop(store);
        let conn = rusqlite::Connection::open(&old).unwrap();
        // Step 51 completes; step 52 fails because it needs run_usage.
        conn.execute_batch("DROP TABLE run_usage; PRAGMA user_version=50;")
            .unwrap();
        drop(conn);
        let error = restore(&paths, &old).unwrap_err();
        assert!(format!("{error:#}").contains("migrate database to v52"));
        let saved: Vec<_> = std::fs::read_dir(backup_dir(&paths))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(saved.len(), 1);
        assert!(error.to_string().contains(&saved[0].display().to_string()));
        let conn = rusqlite::Connection::open(paths.db_file()).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            crate::storage::SCHEMA_VERSION
        );
        assert_eq!(
            conn.query_row("SELECT name FROM sessions", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "original"
        );
        drop(conn);
        // The recovery point remains usable through the public restore path.
        let failed_copy = restore(&paths, &saved[0]).unwrap();
        assert!(failed_copy.exists());
        let store = Store::open(&paths.db_file()).unwrap();
        assert_eq!(store.list_sessions().unwrap()[0].name, "original");
        drop(store);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn rotated_backups_order_legacy_and_fractional_timestamps() {
        let base = std::env::temp_dir().join(format!("fl-hk-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        std::fs::create_dir_all(backup_dir(&paths)).unwrap();
        let names = [
            "formless-20260101T120000Z.db",
            "formless-20260101T120000.123000000Z-unique.db",
            "formless-20260101T120001Z.db",
        ];
        let expected: Vec<_> = names.iter().map(|n| backup_dir(&paths).join(n)).collect();
        for path in expected.iter().rev() {
            std::fs::write(path, "backup").unwrap();
        }
        assert_eq!(list_backups(&paths), expected);
        assert_eq!(last_backup(&paths).unwrap().0, expected[2]);
        // Rotation must keep the fractional snapshot ahead of its legacy
        // whole-second predecessor, even when both share a UTC second.
        std::fs::remove_file(&expected[2]).unwrap();
        assert_eq!(last_backup(&paths).unwrap().0, expected[1]);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn migration_gc_keeps_newest_timestamps_across_schema_widths() {
        let base = std::env::temp_dir().join(format!("fl-hk-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        let mut copies = Vec::new();
        for (version, day) in [(8, 1), (9, 2), (51, 3), (52, 4)] {
            let copy = paths
                .data_dir
                .join(format!("formless.db.v{version}-202601{day:02}T120000Z.bak"));
            // Build the actual migration prefix at its stamped version, rather
            // than restamping a current schema or using non-SQLite bytes.
            let historical = crate::storage::schema_at(version).unwrap();
            historical
                .pragma_update(None, "user_version", version)
                .unwrap();
            historical.execute("INSERT INTO sessions(id,name,created_at,updated_at) VALUES('known','historical fixture','','')",[]).unwrap();
            historical
                .execute("VACUUM INTO ?1", [copy.to_str().unwrap()])
                .unwrap();
            copies.push(copy);
        }
        assert_eq!(
            migration_backups(&paths),
            copies.iter().rev().cloned().collect::<Vec<_>>()
        );
        let cfg = Config::default();
        let sizes: Vec<_> = copies.iter().map(|copy| path_bytes(copy)).collect();
        crate::backup_verification::verify(&paths, copies.last().unwrap()).unwrap();
        let receipt_bytes = path_bytes(&backup_dir(&paths));
        let before = usage(&paths, &store).unwrap();
        let backup_bytes = |u: &Value| {
            u["kinds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["kind"] == "backups")
                .unwrap()["bytes"]
                .as_u64()
                .unwrap()
        };
        assert_eq!(
            backup_bytes(&before),
            sizes.iter().sum::<u64>() + receipt_bytes
        );
        let dry = gc(&paths, &store, &cfg, true).unwrap();
        let migration_row = |report: &Value| {
            report["kinds"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["kind"] == "migration backups")
                .unwrap()
                .clone()
        };
        assert_eq!(migration_row(&dry)["items"], 2);
        assert_eq!(migration_row(&dry)["bytes"], sizes[0] + sizes[1]);
        assert_eq!(
            path_bytes(&backup_dir(&paths)),
            receipt_bytes,
            "dry run never rewrites receipts"
        );
        assert!(
            copies.iter().all(|p| p.exists()),
            "dry run preserves all copies"
        );
        let removed = gc(&paths, &store, &cfg, false).unwrap();
        assert_eq!(migration_row(&removed)["items"], 2);
        assert_eq!(migration_row(&removed)["bytes"], sizes[0] + sizes[1]);
        assert!(!copies[0].exists() && !copies[1].exists());
        assert!(copies[2].exists() && copies[3].exists());
        assert_eq!(
            migration_backups(&paths),
            vec![copies[3].clone(), copies[2].clone()]
        );
        assert_eq!(
            backup_bytes(&usage(&paths, &store).unwrap()),
            sizes[2] + sizes[3] + path_bytes(&backup_dir(&paths))
        );
        drop(store);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn usage_and_gc_report_every_kind() {
        let base = std::env::temp_dir().join(format!("fl-hk-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        std::fs::create_dir_all(paths.data_dir.join("mcp")).unwrap();
        std::fs::write(paths.data_dir.join("mcp/x.log"), "line\n".repeat(300_000)).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        let u = usage(&paths, &store).unwrap();
        assert!(u["kinds"].as_array().unwrap().len() >= 8);
        let cfg = Config::default();
        let dry = gc(&paths, &store, &cfg, true).unwrap();
        let mcp = dry["kinds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "MCP logs")
            .unwrap()
            .clone();
        assert_eq!(mcp["items"], 1);
        assert_eq!(
            path_bytes(&paths.data_dir.join("mcp/x.log")),
            1_500_000,
            "dry run changes nothing"
        );
        gc(&paths, &store, &cfg, false).unwrap();
        assert!(path_bytes(&paths.data_dir.join("mcp/x.log")) <= MCP_LOG_KEEP);
        assert_eq!(human_bytes(1_500_000), "1.5 MB");
        let _ = std::fs::remove_dir_all(&base);
    }
}
