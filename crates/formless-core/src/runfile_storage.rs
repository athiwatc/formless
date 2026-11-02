//! Owned Run recovery records, bounded admission, and paths-preserving expiry.
use super::*;
use crate::config::RunFilesCfg;
use std::collections::{BTreeMap, BTreeSet};

const MAX_BEFORE_BYTES: usize = 2 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 128 * 1024;
const MAX_AFTER_BYTES: usize = 8 * 1024;
pub const EXPIRED_REASON: &str = "Snapshot expired by retention policy";

/// Metadata-only projection. Recovery bodies are loaded one path at a time.
#[derive(Clone, Debug)]
pub struct RunFileRecord {
    pub path: String,
    pub existed: bool,
    pub kept: bool,
    pub before_bytes: u64,
    pub metadata_bytes: u64,
    pub before_kind: Option<String>,
    pub after_state: Option<String>,
    pub not_kept_reason: Option<String>,
}
#[derive(Clone, Debug)]
pub struct StoredRunFile {
    pub path: String,
    pub existed: bool,
    pub kept: bool,
    pub before: Option<Vec<u8>>,
    pub before_metadata: Option<String>,
    pub before_kind: Option<String>,
    pub after_state: Option<String>,
    pub not_kept_reason: Option<String>,
}
#[derive(Clone, Debug)]
pub struct RunFileAdmission {
    pub inserted: bool,
    pub kept: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RunFileUsage {
    pub rows: u64,
    pub retained_files: u64,
    pub before_bytes: u64,
    pub metadata_bytes: u64,
    pub after_state_bytes: u64,
    /// Stored recovery payload, including descriptor metadata, excluding paths.
    pub retained_bytes: u64,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RunFileRetention {
    pub runs: usize,
    pub files: u64,
    pub bytes: u64,
    pub pinned_bytes: u64,
    pub remaining_bytes: u64,
    pub over_budget_bytes: u64,
}
impl Store {
    /// No caller may turn a foreign or malformed snapshot into filesystem authority.
    pub fn validate_run_file_owner(&self, session: &str, run: &str) -> Result<()> {
        let owned: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs r JOIN sessions s ON s.id=r.session_id
             WHERE r.id=?1 AND r.session_id=?2 AND s.deleted_at IS NULL)
             AND NOT EXISTS(SELECT 1 FROM run_files WHERE run_id=?1 AND session_id<>?2)",
            params![run, session],
            |r| r.get(0),
        )?;
        if !owned {
            return Err(review_storage::ReviewFailure::NotFound.into());
        }
        Ok(())
    }

    pub fn run_file_records(&self, session: &str, run: &str) -> Result<Vec<RunFileRecord>> {
        self.read_transaction(|st| {
            st.validate_run_file_owner(session, run)?;
            let (count,bytes):(u64,u64)=st.conn.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(path AS BLOB))+MIN(COALESCE(length(CAST(after_state AS BLOB)),0),8192)+MIN(COALESCE(length(CAST(not_kept_reason AS BLOB)),0),2048)+MIN(COALESCE(length(CAST(before_kind AS BLOB)),0),32)),0) FROM run_files WHERE run_id=?1",params![run],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if count>10_000 || bytes>16*1024*1024 {
                return Err(anyhow!("Run file listing exceeds the 10000-path or 16 MiB metadata limit ({count} recorded paths); no paths were omitted or older Run selected"));
            }
            let mut query=st.ps("SELECT path,existed,kept,COALESCE(length(before),0),COALESCE(length(CAST(before_metadata AS BLOB)),0),substr(before_kind,1,32),CASE WHEN length(CAST(after_state AS BLOB))<=8192 THEN after_state ELSE NULL END,substr(not_kept_reason,1,2048) FROM run_files WHERE run_id=?1 ORDER BY rowid")?;
            let records=query.query_map(params![run],|r|Ok(RunFileRecord {
                path:r.get(0)?,existed:r.get(1)?,kept:r.get(2)?,before_bytes:r.get(3)?,metadata_bytes:r.get(4)?,before_kind:r.get(5)?,after_state:r.get(6)?,not_kept_reason:r.get(7)?,
            }))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(records)
        })
    }

    /// Load at most one bounded recovery body. A legacy oversized image remains
    /// listed with its real byte count but never masquerades as a kept empty file.
    pub fn run_file_record(&self, session: &str, run: &str, path: &str) -> Result<StoredRunFile> {
        self.read_transaction(|st| {
            st.validate_run_file_owner(session,run)?;
            let sizes:Option<(u64,u64)>=st.conn.query_row("SELECT COALESCE(length(before),0),COALESCE(length(CAST(before_metadata AS BLOB)),0) FROM run_files WHERE run_id=?1 AND path=?2",params![run,path],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let (body,metadata)=sizes.context("That Run did not record this path")?;
            let mut row=st.conn.query_row("SELECT existed,kept,CASE WHEN length(before)<=2097152 THEN before ELSE NULL END,CASE WHEN length(CAST(before_metadata AS BLOB))<=131072 THEN before_metadata ELSE NULL END,substr(before_kind,1,32),CASE WHEN length(CAST(after_state AS BLOB))<=8192 THEN after_state ELSE NULL END,substr(not_kept_reason,1,2048) FROM run_files WHERE run_id=?1 AND path=?2",params![run,path],|r|Ok(StoredRunFile {
                path:path.into(),existed:r.get(0)?,kept:r.get(1)?,before:r.get(2)?,before_metadata:r.get(3)?,before_kind:r.get(4)?,after_state:r.get(5)?,not_kept_reason:r.get(6)?,
            }))?;
            if body>MAX_BEFORE_BYTES as u64 || metadata>MAX_METADATA_BYTES as u64 {
                row.kept=false;row.before=None;row.before_metadata=None;
                row.not_kept_reason=Some("Recorded recovery payload exceeds the per-file read limit".into());
            }
            Ok(row)
        })
    }

    /// Admission and aggregate accounting share one immediate write transaction.
    /// Count includes kept creations with no body. Repeated callbacks never replace
    /// first-capture evidence or gain a fresh quota allocation.
    #[allow(clippy::too_many_arguments)]
    pub fn snapshot_run_file_capped(
        &self,
        session: &str,
        run: &str,
        path: &str,
        existed: bool,
        before: Option<&[u8]>,
        metadata: Option<&str>,
        before_kind: &str,
        reason: Option<&str>,
        cfg: &RunFilesCfg,
    ) -> Result<RunFileAdmission> {
        if !matches!(
            before_kind,
            "missing" | "file" | "directory" | "symlink" | "other" | "unknown"
        ) {
            return Err(anyhow!("Unknown Run file kind"));
        }
        self.transaction(|st| {
            st.validate_run_file_owner(session, run)?;
            let existing: Option<(bool, Option<String>)> = st.conn.query_row(
                "SELECT kept,not_kept_reason FROM run_files WHERE run_id=?1 AND path=?2",
                params![run,path], |row| Ok((row.get(0)?,row.get(1)?)),
            ).optional()?;
            if let Some((kept, reason)) = existing {
                return Ok(RunFileAdmission { inserted:false, kept, reason });
            }
            let (files, bytes):(u64,u64) = st.conn.query_row(
                "SELECT COALESCE(SUM(kept),0),COALESCE(SUM(COALESCE(length(before),0)+COALESCE(length(CAST(before_metadata AS BLOB)),0)),0) FROM run_files WHERE run_id=?1",
                params![run], |row| Ok((row.get(0)?,row.get(1)?)),
            )?;
            let payload = (before.map_or(0, <[u8]>::len) as u64)
                .saturating_add(metadata.map_or(0, str::len) as u64);
            let mut exclusion = reason.map(str::to_owned);
            if exclusion.is_none() {
                exclusion = if before.is_some_and(|body| body.len() > MAX_BEFORE_BYTES) {
                    Some("File exceeds the 2 MiB snapshot limit".into())
                } else if metadata.is_some_and(|body| body.len() > MAX_METADATA_BYTES) {
                    Some("File security metadata exceeds the snapshot limit".into())
                } else if existed && before.is_none() {
                    Some("The pre-Run file content was not kept".into())
                } else if cfg.max_files_per_run == 0 || files >= cfg.max_files_per_run as u64 {
                    Some("Run snapshot file count limit reached".into())
                } else if cfg.max_bytes_per_run == 0 || bytes.saturating_add(payload) > cfg.max_bytes_per_run {
                    Some("Run snapshot byte limit reached".into())
                } else { None };
            }
            let kept = exclusion.is_none();
            st.ex("INSERT INTO run_files(run_id,session_id,path,existed,kept,before,created_at,before_metadata,before_kind,not_kept_reason)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![run,session,path,existed,kept,if kept {before} else {None},now(),if kept {metadata} else {None},before_kind,exclusion])?;
            Ok(RunFileAdmission { inserted:true, kept, reason:exclusion })
        })
    }

    /// Only a new typed capture can acquire a final token. Historical NULL kind
    /// is intentionally never backfilled from today's filesystem state.
    pub fn set_run_file_after_state(
        &self,
        session: &str,
        run: &str,
        path: &str,
        token: &str,
    ) -> Result<bool> {
        if token.len() > MAX_AFTER_BYTES || !serde_json::from_str::<Value>(token)?.is_object() {
            return Err(anyhow!("Invalid or oversized Run file after-state"));
        }
        self.transaction(|st| {
            st.validate_run_file_owner(session,run)?;
            Ok(st.ex("UPDATE run_files SET after_state=?4 WHERE run_id=?1 AND session_id=?2 AND path=?3 AND after_state IS NULL AND before_kind IS NOT NULL",
                params![run,session,path,token])? > 0)
        })
    }
    pub fn mark_run_untracked_writes(&self, session: &str, run: &str) -> Result<()> {
        self.transaction(|st| {
            st.validate_run_file_owner(session, run)?;
            st.ex(
                "UPDATE runs SET untracked_writes_possible=1 WHERE id=?1",
                params![run],
            )?;
            Ok(())
        })
    }
    pub fn run_untracked_writes_possible(&self, session: &str, run: &str) -> Result<bool> {
        self.read_transaction(|st| {
            st.validate_run_file_owner(session, run)?;
            Ok(st.conn.query_row(
                "SELECT untracked_writes_possible FROM runs WHERE id=?1",
                params![run],
                |r| r.get(0),
            )?)
        })
    }
    pub fn run_file_usage(&self) -> Result<RunFileUsage> {
        let mut usage = self.conn.query_row(
            "SELECT COUNT(*),COALESCE(SUM(kept),0),
            COALESCE(SUM(length(before)),0),COALESCE(SUM(length(CAST(before_metadata AS BLOB))),0),
            COALESCE(SUM(length(CAST(after_state AS BLOB))),0) FROM run_files",
            [],
            |r| {
                Ok(RunFileUsage {
                    rows: r.get(0)?,
                    retained_files: r.get(1)?,
                    before_bytes: r.get(2)?,
                    metadata_bytes: r.get(3)?,
                    after_state_bytes: r.get(4)?,
                    retained_bytes: 0,
                })
            },
        )?;
        usage.retained_bytes = usage.before_bytes.saturating_add(usage.metadata_bytes);
        Ok(usage)
    }

    /// Preview and apply use the same transaction and plan. Payload is expired as
    /// a whole Run; paths and after tokens remain as explicit read-only evidence.
    /// Pinned latest/active Runs can exceed the configured profile budget, which
    /// is reported rather than silently destroying the last recovery point.
    pub fn retain_run_files(&self, cfg: &RunFilesCfg, dry_run: bool) -> Result<RunFileRetention> {
        self.transaction(|st| {
            let cutoff = chrono::Utc::now().checked_sub_signed(chrono::Duration::days(
                cfg.max_age_days.min(3_650_000) as i64)).unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);
            let mut query=st.ps("SELECT f.run_id,f.session_id,COALESCE(r.created_at,MIN(f.created_at)),
                COUNT(*),COALESCE(SUM(COALESCE(length(f.before),0)+COALESCE(length(CAST(f.before_metadata AS BLOB)),0)),0),
                EXISTS(SELECT 1 FROM runs a WHERE a.id=f.run_id AND a.status='running') OR
                EXISTS(SELECT 1 FROM invocations i WHERE i.run_id=f.run_id AND i.status IN ('running','pending')),
                COALESCE(r.rowid,0),COALESCE(SUM(f.kept),0)
                FROM run_files f LEFT JOIN runs r ON r.id=f.run_id
                GROUP BY f.run_id,f.session_id ORDER BY 3 DESC,7 DESC,f.run_id DESC")?;
            let rows=query.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,u64>(3)?,r.get::<_,u64>(4)?,r.get::<_,bool>(5)?,r.get::<_,i64>(6)?,r.get::<_,u64>(7)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(query);
            let mut seen=BTreeMap::<String,usize>::new();
            let mut latest=BTreeMap::<String,String>::new();
            let mut plan=BTreeSet::new();
            let mut total=0u64;
            let mut report=RunFileRetention::default();
            for (run,session,created,_,bytes,active,_,kept) in &rows {
                total=total.saturating_add(*bytes);
                let rank=seen.entry(session.clone()).or_default();
                *rank+=1;
                let first=!latest.contains_key(session);
                latest.entry(session.clone()).or_insert_with(||run.clone());
                let pinned=*active || first;
                if pinned {report.pinned_bytes=report.pinned_bytes.saturating_add(*bytes);continue;}
                let old=chrono::DateTime::parse_from_rfc3339(created).map(|at|at.with_timezone(&chrono::Utc)<cutoff).unwrap_or(true);
                if (*kept>0 || *bytes>0) && (*rank>cfg.keep_runs_per_session || cfg.max_age_days==0 || old) {plan.insert(run.clone());}
            }
            let mut remaining=total;
            for (run,_,_,_,bytes,_,_,_) in &rows {if plan.contains(run){remaining=remaining.saturating_sub(*bytes);}}
            let limit=cfg.max_total_mb.saturating_mul(1024*1024);
            for (run,session,_,_,bytes,active,_,kept) in rows.iter().rev() {
                if remaining<=limit {break;}
                // Determine the latest recorded Run using the same deterministic
                // ordering, even when its payload was already expired/rewound.
                let latest_run=latest.get(session);
                if *active || latest_run==Some(run) || plan.contains(run) || (*kept==0 && *bytes==0) {continue;}
                plan.insert(run.clone());remaining=remaining.saturating_sub(*bytes);
            }
            for (run,_,_,_,bytes,_,_,kept) in &rows {
                if !plan.contains(run){continue;}
                report.runs+=1;report.files=report.files.saturating_add(*kept);report.bytes=report.bytes.saturating_add(*bytes);
                if !dry_run {st.ex("UPDATE run_files SET kept=0,before=NULL,before_metadata=NULL,not_kept_reason=?2 WHERE run_id=?1 AND (kept=1 OR before IS NOT NULL OR before_metadata IS NOT NULL)",params![run,EXPIRED_REASON])?;}
            }
            report.remaining_bytes=remaining;report.over_budget_bytes=remaining.saturating_sub(limit);
            Ok(report)
        })
    }
}

#[cfg(test)]
#[path = "runfile_storage_tests.rs"]
mod tests;
