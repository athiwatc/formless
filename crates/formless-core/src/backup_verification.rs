//! Offline structural verification. Never execute restored Tools or start workers.
use crate::{housekeeping, Paths};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub source: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub schema_version: i64,
    pub tables: BTreeMap<String, u64>,
    pub verified_at: String,
}

struct PrivateDir(PathBuf);
impl PrivateDir {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("formless-backup-check-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn has_live_wal(source: &Path) -> Result<bool> {
    let wal = PathBuf::from(format!("{}-wal", source.display()));
    match std::fs::metadata(&wal) {
        Ok(metadata) => Ok(metadata.len() > 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("inspect backup WAL"),
    }
}

fn fingerprint(file: &Path) -> Result<(String, u64)> {
    if !std::fs::metadata(file)?.is_file() {
        bail!("backup is not a regular file");
    }
    let mut input = std::fs::File::open(file)?;
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0u8; 65536];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        hash.update(&buffer[..read]);
    }
    Ok((format!("{:x}", hash.finalize()), bytes))
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
fn inventory(conn: &Connection) -> Result<BTreeMap<String, Vec<String>>> {
    let mut statement = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = BTreeMap::new();
    for name in names {
        let mut cols = conn.prepare(&format!("PRAGMA table_info({})", quote(&name)))?;
        let columns = cols
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        result.insert(name, columns);
    }
    Ok(result)
}
fn receipt_path(paths: &Paths, file: &Path) -> Result<PathBuf> {
    let source = std::fs::canonicalize(file)?;
    let key = format!(
        "{:x}",
        Sha256::digest(source.as_os_str().as_encoded_bytes())
    );
    Ok(housekeeping::backup_dir(paths)
        .join(".verified")
        .join(format!("{key}.json")))
}

/// A standalone SQLite snapshot only; a live database with a WAL is not a backup.
/// Inspect a private immutable copy and compare fingerprints again before receipt.
pub fn verify(paths: &Paths, file: &Path) -> Result<Verification> {
    let report = inspect(file)?;
    crate::config::write_secret_file(
        &receipt_path(paths, file)?,
        &serde_json::to_string_pretty(&report)?,
    )?;
    Ok(report)
}

/// Structural check without receipts, also used on restore's migrated stage.
pub(crate) fn inspect(file: &Path) -> Result<Verification> {
    let source = std::fs::canonicalize(file).context("locate backup")?;
    if !std::fs::metadata(&source)?.is_file() {
        bail!("backup is not a regular file");
    }
    if has_live_wal(&source)? {
        bail!("backup has a live WAL; take a standalone consistent snapshot first");
    }
    let temporary = PrivateDir::new()?;
    let copy = temporary.0.join("snapshot.db");
    std::fs::copy(&source, &copy)?;
    let (sha256, bytes) = fingerprint(&copy)?;
    let uri = reqwest::Url::from_file_path(&copy)
        .map_err(|_| anyhow!("invalid private snapshot path"))?;
    let conn = Connection::open_with_flags(
        format!("{uri}?immutable=1"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    let mut integrity = conn
        .prepare("PRAGMA integrity_check")
        .context("SQLite integrity_check")?;
    for result in integrity.query_map([], |r| r.get::<_, String>(0))? {
        let result = result.context("SQLite integrity_check")?;
        if result != "ok" {
            bail!("SQLite integrity_check failed: {result}");
        }
    }
    let foreign_keys: bool = conn
        .prepare("PRAGMA foreign_key_check")
        .context("SQLite foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    if foreign_keys {
        bail!("SQLite foreign_key_check failed");
    }
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let expected = inventory(&crate::storage::schema_at(schema_version)?)?;
    let actual = inventory(&conn)?;
    for (table, columns) in expected {
        let found = actual
            .get(&table)
            .with_context(|| format!("schema v{schema_version}: missing table {table}"))?;
        for column in columns {
            if !found.contains(&column) {
                bail!("schema v{schema_version}: missing column {table}.{column}");
            }
        }
    }
    let mut tables = BTreeMap::new();
    for name in actual.keys() {
        let count: u64 =
            conn.query_row(&format!("SELECT COUNT(*) FROM {}", quote(name)), [], |r| {
                r.get(0)
            })?;
        tables.insert(name.clone(), count);
    }
    if has_live_wal(&source)? {
        bail!("backup acquired a live WAL during verification");
    }
    if fingerprint(&source)? != (sha256.clone(), bytes) {
        bail!("backup changed during verification");
    }
    let report = Verification {
        source,
        sha256,
        bytes,
        schema_version,
        tables,
        verified_at: chrono::Utc::now().to_rfc3339(),
    };
    Ok(report)
}

/// Receipts are hints, never a blessing for subsequently changed bytes.
pub fn verified(paths: &Paths, file: &Path) -> Option<Verification> {
    let source = std::fs::canonicalize(file).ok()?;
    if has_live_wal(&source).ok()? {
        return None;
    }
    let receipt: Verification =
        serde_json::from_slice(&std::fs::read(receipt_path(paths, file).ok()?).ok()?).ok()?;
    if receipt.source != source
        || fingerprint(&source).ok()? != (receipt.sha256.clone(), receipt.bytes)
    {
        return None;
    }
    Some(receipt)
}

pub(crate) fn remove_copy(paths: &Paths, file: &Path) -> Result<()> {
    let receipt = receipt_path(paths, file).ok();
    std::fs::remove_file(file)?;
    if let Some(receipt) = receipt {
        let _ = std::fs::remove_file(receipt);
    }
    Ok(())
}

pub fn copies(paths: &Paths) -> Vec<PathBuf> {
    let mut files = housekeeping::list_backups(paths);
    files.extend(housekeeping::list_restore_backups(paths));
    files.extend(housekeeping::migration_backups(paths));
    files.sort_by_key(|p| {
        (
            std::fs::metadata(p).and_then(|m| m.modified()).ok(),
            p.clone(),
        )
    });
    files
}
pub fn list(paths: &Paths) -> Value {
    json!(copies(paths).iter().map(|file| json!({"file":file,"bytes":housekeeping::path_bytes(file),
        "modified_at":std::fs::metadata(file).and_then(|m|m.modified()).ok().map(chrono::DateTime::<chrono::Utc>::from),
        "verified_at":verified(paths,file).map(|r|r.verified_at)})).collect::<Vec<_>>())
}
pub fn newest_status(paths: &Paths) -> Value {
    copies(paths).last().map_or(Value::Null, |file| {
        let receipt = verified(paths,file);
        let verification_age = receipt.as_ref().and_then(|r|chrono::DateTime::parse_from_rfc3339(&r.verified_at).ok())
            .map(|at|(chrono::Utc::now().signed_duration_since(at).num_seconds()).max(0));
        json!({"file":file,"bytes":housekeeping::path_bytes(file),
            "age_seconds":std::fs::metadata(file).and_then(|m|m.modified()).ok().and_then(|at|at.elapsed().ok()).map(|age|age.as_secs()),
            "verified_at":receipt.map(|r|r.verified_at),"verification_age_seconds":verification_age})
    })
}

/// Print-only plan; never probes/starts/stops a daemon or opens its live DB.
pub fn restore_plan(paths: &Paths, source: &Path) -> Value {
    json!({"dry_run":true,"source":source,"destination":paths.db_file(),"scope":"database only; external paths are not rebased",
        "steps":["Stop the daemon and pause any login service; restore refuses reachable endpoints and concurrent owners",
        "Acquire exclusive profile ownership; verify the selected database",
        "Preserve the displaced database in backups/formless.db.pre-restore-<stamp> (seven recovery copies, including legacy names)",
        "Stage and migrate the selected snapshot; integrity-check and checkpoint it",
        "Atomically replace formless.db with no stale WAL/SHM; release ownership",
        "Start the daemon explicitly when ready; restore never starts providers or jobs"]})
}

/// A real migration + offline daemon construction and Session read, with zero
/// live configuration, credentials, serving, job loops, hooks or MCP startup.
pub async fn drill(paths: &Paths, file: &Path) -> Result<Value> {
    let report = verify(paths, file)?;
    let temporary = PrivateDir::new()?;
    let trial = Paths::from_dirs(temporary.0.join("data"), temporary.0.join("config"))?;
    let saved = temporary.0.join("verified.db");
    std::fs::copy(file, &saved)?;
    if fingerprint(&saved)? != (report.sha256.clone(), report.bytes) {
        bail!("backup changed before drill");
    }
    housekeeping::restore(&trial, &saved)?;
    let daemon = std::sync::Arc::new(crate::Daemon::for_restore_drill(trial.clone())?);
    let sessions = daemon
        .handle(formless_protocol::Request::ListSessions)
        .await?;
    let db = Connection::open_with_flags(trial.db_file(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != crate::storage::SCHEMA_VERSION {
        bail!("drill did not upgrade to current schema");
    }
    let expected: u64 = db.query_row(
        "SELECT COUNT(*) FROM sessions WHERE deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    if db.query_row("SELECT COUNT(*) FROM sessions", [], |row| {
        row.get::<_, u64>(0)
    })? != report.tables.get("sessions").copied().unwrap_or(0)
    {
        bail!("drill changed the total Session count");
    }
    let read = sessions
        .as_array()
        .context("offline daemon returned no Session list")?
        .len();
    if read as u64 != expected {
        bail!("drill Session count differs: expected {expected}, read {read}");
    }
    // Do not return transcript text; identifiers suffice for a scripted known-Session check.
    let ids: Vec<_> = sessions
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    Ok(
        json!({"passed":true,"source":file,"schema_version":version,"sessions":read,"session_ids":ids,
        "offline":true,"scope":"database only; external files and credentials were not restored"}),
    )
}
