//! Disk use, clean-up and backups (SPEC §9.6): what Formless stores and
//! where, `formless gc`, and rotated database backups.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::{Paths, Store};

/// File snapshots (`/rewind`) older than this are dropped by `gc`.
const SNAPSHOT_DAYS: i64 = 30;
/// MCP server logs larger than this are cut to their last part by `gc`.
const MCP_LOG_KEEP: u64 = 1_000_000;
/// Pre-migration database copies kept by `gc` (newest first).
const MIGRATION_BACKUPS_KEEP: usize = 2;

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
fn migration_backups(paths: &Paths) -> Vec<PathBuf> {
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
        json!({"kind": "file snapshots", "bytes": inside["run_files"], "detail": "for /rewind, inside the database", "inside_db": true}),
        json!({"kind": "artifacts", "bytes": artifact_bytes, "detail": format!("{artifact_count} stored outputs")}),
        json!({"kind": "logs", "bytes": crate::logs::log_usage(paths) + mcp_logs, "detail": "daemon and MCP server logs"}),
        json!({"kind": "skills", "bytes": path_bytes(&paths.data_dir.join("skills")), "detail": "installed skill files"}),
        json!({"kind": "backups", "bytes": backups, "detail": "rotated and pre-migration database copies"}),
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
        let _ = crate::artifacts::reconcile(
            store,
            &paths.data_dir.join("artifacts"),
            std::time::Duration::from_secs(3600),
        );
    }
    rows.push(json!({"kind": "artifacts", "items": n, "bytes": bytes,
        "detail": format!("older than {} days or over {} MB (artifacts.*)", cfg.artifacts.max_age_days, cfg.artifacts.max_total_mb)}));
    // Old /rewind snapshots.
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(SNAPSHOT_DAYS)).to_rfc3339();
    let (n, bytes) = store.old_run_files(&cutoff)?;
    if !dry_run {
        store.delete_run_files_before(&cutoff)?;
    }
    rows.push(json!({"kind": "file snapshots", "items": n, "bytes": bytes,
        "detail": format!("older than {SNAPSHOT_DAYS} days (no /rewind past them)")}));
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
    // Old pre-migration copies.
    let old: Vec<PathBuf> = migration_backups(paths)
        .into_iter()
        .skip(MIGRATION_BACKUPS_KEEP)
        .collect();
    let bytes: u64 = old.iter().map(|p| path_bytes(p)).sum();
    if !dry_run {
        for p in &old {
            let _ = std::fs::remove_file(p);
        }
    }
    rows.push(
        json!({"kind": "migration backups", "items": old.len(), "bytes": bytes,
        "detail": format!("all but the newest {MIGRATION_BACKUPS_KEEP}")}),
    );
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
    let mut all = list_backups(paths);
    all.reverse(); // newest first
    for old in all.iter().skip(keep.max(1)) {
        let _ = std::fs::remove_file(old);
    }
    Ok(dest)
}

/// Rotated backups, oldest first.
pub fn list_backups(paths: &Paths) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(backup_dir(paths))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("formless-") && n.ends_with(".db"))
        })
        .collect();
    // Legacy names end in whole seconds: the literal Z otherwise sorts
    // after the fractional dot of a newer snapshot from the same second.
    found.sort_by_cached_key(|p| {
        let at = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("formless-"))
            .and_then(|n| n.strip_suffix(".db"))
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
    let has_sessions: bool = probe
        .query_row(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name='sessions'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(false);
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
    drop(probe);
    let db = paths.db_file();
    let dir = backup_dir(paths);
    std::fs::create_dir_all(&dir)?;
    let stamp = backup_stamp();
    let saved = dir.join(format!("before-restore-{stamp}.db"));
    if db.exists() {
        // Snapshot the current file without migrating it. A previous restore
        // may have left an older database whose migration fails; the safety
        // copy must still be usable to recover from that state.
        let current =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        current
            .execute("VACUUM INTO ?1", [saved.to_string_lossy().as_ref()])
            .with_context(|| format!("back up current database to {}", saved.display()))?;
    }
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    std::fs::copy(file, &db).with_context(|| format!("copy into {}", db.display()))?;
    // Opening migrates an older backup to this build's schema.
    Store::open(&db).with_context(|| {
        format!(
            "restored database migration failed; original database saved at {}",
            saved.display()
        )
    })?;
    Ok(saved)
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
            51
        );
        assert_eq!(
            conn.query_row("SELECT name FROM sessions", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "from broken backup"
        );
        drop(conn);
        // Recovery works through the public restore path even though the
        // current database still cannot be migrated.
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
            std::fs::write(&copy, "backup").unwrap();
            copies.push(copy);
        }
        assert_eq!(
            migration_backups(&paths),
            copies.iter().rev().cloned().collect::<Vec<_>>()
        );
        let cfg = Config::default();
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
        assert_eq!(backup_bytes(&before), 24);
        gc(&paths, &store, &cfg, true).unwrap();
        assert!(
            copies.iter().all(|p| p.exists()),
            "dry run preserves all copies"
        );
        gc(&paths, &store, &cfg, false).unwrap();
        assert!(!copies[0].exists() && !copies[1].exists());
        assert!(copies[2].exists() && copies[3].exists());
        assert_eq!(backup_bytes(&usage(&paths, &store).unwrap()), 12);
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
