//! Server-owned replay provenance. Candidate JSON and attempt presentation can
//! neither create nor attach evidence. Raw held-out inputs stay in this table.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) const SCHEMA: &str = r#"
CREATE TABLE optimiser_replay_snapshots (
 id TEXT PRIMARY KEY, tool_id TEXT NOT NULL REFERENCES tools(id) ON DELETE CASCADE,
 base_version INTEGER NOT NULL, created_at TEXT NOT NULL,
 manifest TEXT NOT NULL, digest TEXT NOT NULL
);
CREATE TABLE optimiser_attempt_replay (
 attempt_id TEXT PRIMARY KEY REFERENCES optimiser_attempts(id) ON DELETE CASCADE,
 snapshot_id TEXT NOT NULL UNIQUE REFERENCES optimiser_replay_snapshots(id) ON DELETE CASCADE
);
CREATE TRIGGER immutable_optimiser_replay BEFORE UPDATE ON optimiser_replay_snapshots
 BEGIN SELECT RAISE(ABORT,'Replay evidence is immutable'); END;
CREATE TRIGGER immutable_optimiser_replay_binding BEFORE UPDATE ON optimiser_attempt_replay
 BEGIN SELECT RAISE(ABORT,'Replay evidence binding is immutable'); END;
CREATE INDEX idx_optimiser_replay_created ON optimiser_replay_snapshots(created_at);
"#;
const MAX_INPUT: usize = 256 * 1024;
const MAX_SCAN_ROWS: usize = 8192;
const MAX_EXTENSION_BYTES: usize = 32 * 1024 * 1024;
const REPLAY_POOL_SQL: &str = "SELECT id,
    CASE WHEN length(CAST(input AS BLOB))<=?2 THEN input ELSE NULL END,
    substr(error,1,2048), substr(tag,1,128), status
    FROM invocations
    WHERE tool_id=?1 AND status IN ('success','failure') ORDER BY rowid DESC LIMIT ?3";
const MAX_MANIFEST: usize = 8 * 1024 * 1024;
const MAX_SNAPSHOTS: i64 = 64;
const MAX_STORED_BYTES: i64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplaySnapshot {
    pub id: String,
    pub digest: String,
    pub tool_id: String,
    pub base_version: i64,
    pub generation: Vec<ReplayItem>,
    pub holdout: Vec<ReplayItem>,
    pub source_count: usize,
    pub excluded_count: usize,
    pub created_at: String,
    /// IDs/digests/partition metadata only; safe for attempt presentation.
    pub provenance: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Group {
    pub(super) item: ReplayItem,
    pub(super) latest_success: bool,
    hash: String,
    sources: Vec<String>,
}
fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn canonical(value: &mut Value) {
    match value {
        Value::Number(number) => {
            // JS receives IEEE754 numbers. Treat 1 and 1.0 (and integers that
            // round to the same JS number) as the same input group.
            if let Some(number) = number.as_f64().and_then(serde_json::Number::from_f64) {
                *value = Value::Number(number);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(canonical),
        Value::Object(values) => values.values_mut().for_each(canonical),
        _ => {}
    }
}
impl Store {
    pub(super) fn replay_pool(
        &self,
        tool_id: &str,
        limit: usize,
    ) -> Result<(Vec<Group>, usize, usize)> {
        if !(2..=512).contains(&limit) {
            return Err(anyhow!("optimiser.replay_pool_limit must be 2..=512"));
        }
        // Keep the ordinary recent window so duplicates there can still add
        // their outcome evidence. If it is short of distinct inputs, keep
        // scanning the same newest-first index within fixed row and byte caps.
        let recent = limit * 4;
        let mut statement = self.ps(REPLAY_POOL_SQL)?;
        let mut rows = statement.query(params![tool_id, MAX_INPUT as i64, MAX_SCAN_ROWS as i64])?;
        let mut groups = BTreeMap::<String, Group>::new();
        let mut order = Vec::new();
        let mut source_count = 0;
        let mut excluded = 0;
        let mut bytes = 0;
        let mut extension_bytes = 0;
        while source_count < recent || groups.len() < limit {
            let Some(row) = rows.next()? else { break };
            let input: Option<String> = row.get(1)?;
            if source_count >= recent {
                extension_bytes += input.as_ref().map_or(0, String::len);
                if extension_bytes > MAX_EXTENSION_BYTES {
                    break;
                }
            }
            source_count += 1;
            let Some(input) = input else {
                excluded += 1;
                continue;
            };
            let mut parsed: Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(_) => {
                    excluded += 1;
                    continue;
                }
            };
            canonical(&mut parsed);
            let input = serde_json::to_string(&parsed)?;
            let digest = hash(input.as_bytes());
            let success = row.get::<_, String>(4)? == "success";
            if let Some(group) = groups.get_mut(&digest) {
                // One input group counts once even with conflicting recorded
                // outcomes. Preserve ANY represented known success for the
                // zero-regression guard; recency still supplies error/tag
                // context and neither selection nor split depends on outcome.
                group.item.recorded_success |= success;
                group.sources.push(row.get(0)?);
                continue;
            }
            if groups.len() >= limit || bytes + input.len() > MAX_MANIFEST / 2 {
                excluded += 1;
                continue;
            }
            bytes += input.len();
            order.push(digest.clone());
            groups.insert(
                digest.clone(),
                Group {
                    hash: digest,
                    latest_success: success,
                    sources: vec![row.get(0)?],
                    item: ReplayItem {
                        input,
                        error: row.get(2)?,
                        tag: row.get(3)?,
                        recorded_success: success,
                    },
                },
            );
        }
        Ok((
            order
                .into_iter()
                .filter_map(|key| groups.remove(&key))
                .collect(),
            source_count,
            excluded,
        ))
    }

    pub fn create_replay_snapshot(
        &self,
        tool_id: &str,
        base_version: i64,
        pool_limit: usize,
        holdout_percent: u32,
    ) -> Result<ReplaySnapshot> {
        if !(10..=90).contains(&holdout_percent) {
            return Err(anyhow!("optimiser.replay_holdout_percent must be 10..=90"));
        }
        self.transaction(|st| {
            let current = st.get_tool_current(tool_id)?.context("Tool no longer exists")?;
            if current.1.version != base_version { return Err(anyhow!("Tool changed before replay evidence capture")); }
            st.prune_replay_snapshots()?;
            let (count, bytes): (i64,i64) = st.conn.query_row("SELECT count(*),coalesce(sum(length(CAST(manifest AS BLOB))),0) FROM optimiser_replay_snapshots", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
            if count >= MAX_SNAPSHOTS { return Err(anyhow!("Replay evidence capacity is full; active evidence was preserved")); }
            let (mut groups, source_count, excluded_count) = st.replay_pool(tool_id, pool_limit)?;
            // Hash rank depends on identity and input only, never outcome/error.
            groups.sort_by_key(|group| hash(format!("{tool_id}:{base_version}:{}",group.hash)));
            let holdout_count = groups.len() * holdout_percent as usize / 100;
            let mut generation = Vec::new();
            let mut holdout = Vec::new();
            let mut provenance = Vec::new();
            for (index, group) in groups.into_iter().enumerate() {
                let held_out = index < holdout_count;
                provenance.push(json!({"input_digest":group.hash,"source_invocations":group.sources,"latest_recorded_success":group.latest_success,"partition":if held_out {"holdout"}else{"generation"}}));
                if held_out { holdout.push(group.item); } else { generation.push(group.item); }
            }
            let mut snapshot = ReplaySnapshot { id:uuid::Uuid::new_v4().to_string(), digest:String::new(),tool_id:tool_id.into(),base_version,generation,holdout,source_count,excluded_count,created_at:now(),provenance:json!({"format":"optimiser-replay-v1","selection":"recent_terminal_outcome_independent","pool_limit":pool_limit,"holdout_percent":holdout_percent,"groups":provenance}) };
            let manifest = serde_json::to_string(&snapshot)?;
            if manifest.len() > MAX_MANIFEST || bytes + manifest.len() as i64 > MAX_STORED_BYTES { return Err(anyhow!("Replay evidence byte capacity is full")); }
            snapshot.digest = hash(manifest.as_bytes());
            st.ex("INSERT INTO optimiser_replay_snapshots(id,tool_id,base_version,created_at,manifest,digest) VALUES(?1,?2,?3,?4,?5,?6)",params![snapshot.id,tool_id,base_version,snapshot.created_at,manifest,snapshot.digest])?;
            Ok(snapshot)
        })
    }

    fn prune_replay_snapshots(&self) -> Result<()> {
        // Never expire a generated/replaying/approval-pending attempt. Bounded
        // capacity refuses new work rather than discarding its active proof.
        self.ex("DELETE FROM optimiser_replay_snapshots WHERE created_at < strftime('%Y-%m-%dT%H:%M:%SZ','now','-7 days') AND NOT EXISTS (SELECT 1 FROM optimiser_attempt_replay b JOIN optimiser_attempts a ON a.id=b.attempt_id WHERE b.snapshot_id=optimiser_replay_snapshots.id AND a.status NOT IN ('promoted','rejected','discarded','failed','cancelled'))", [])?;
        Ok(())
    }

    fn replay_snapshot(&self, id: &str) -> Result<Option<ReplaySnapshot>> {
        let row: Option<(String,String,String,i64)> = self.conn.query_row("SELECT manifest,digest,tool_id,base_version FROM optimiser_replay_snapshots WHERE id=?1",params![id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let Some((manifest, digest, tool_id, base_version)) = row else {
            return Ok(None);
        };
        if manifest.len() > MAX_MANIFEST || hash(manifest.as_bytes()) != digest {
            return Err(anyhow!("Replay evidence integrity mismatch"));
        }
        let mut snapshot: ReplaySnapshot = serde_json::from_str(&manifest)?;
        if snapshot.id != id
            || snapshot.tool_id != tool_id
            || snapshot.base_version != base_version
            || !snapshot.digest.is_empty()
        {
            return Err(anyhow!("Replay evidence identity mismatch"));
        }
        snapshot.digest = digest;
        Ok(Some(snapshot))
    }

    pub fn create_attempt_with_replay_snapshot(
        &self,
        tool_id: &str,
        base_version: i64,
        candidate: &CandidateData,
        est_tokens: i64,
        snapshot_id: &str,
    ) -> Result<String> {
        self.transaction(|st| {
            let snapshot = st
                .replay_snapshot(snapshot_id)?
                .context("Replay evidence is missing")?;
            if snapshot.tool_id != tool_id || snapshot.base_version != base_version {
                return Err(anyhow!("Replay evidence belongs to another Tool/version"));
            }
            let id = st.create_attempt(tool_id, base_version, candidate, est_tokens)?;
            st.ex(
                "INSERT INTO optimiser_attempt_replay(attempt_id,snapshot_id) VALUES(?1,?2)",
                params![id, snapshot_id],
            )?;
            Ok(id)
        })
    }
    /// Release a capture that never reached candidate creation. A bound
    /// attempt's evidence cannot be removed by generation cleanup.
    pub fn discard_unbound_replay_snapshot(&self, snapshot_id: &str) -> Result<()> {
        self.ex("DELETE FROM optimiser_replay_snapshots WHERE id=?1 AND NOT EXISTS (SELECT 1 FROM optimiser_attempt_replay WHERE snapshot_id=?1)", params![snapshot_id])?;
        Ok(())
    }

    pub fn attempt_replay_snapshot(&self, attempt_id: &str) -> Result<Option<ReplaySnapshot>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT snapshot_id FROM optimiser_attempt_replay WHERE attempt_id=?1",
                params![attempt_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(snapshot_id) = id else {
            return Ok(None);
        };
        let snapshot = self
            .replay_snapshot(&snapshot_id)?
            .context("Bound replay evidence is missing")?;
        let attempt = self
            .get_attempt(attempt_id)?
            .context("Replay attempt is missing")?;
        if snapshot.tool_id != attempt.tool_id || snapshot.base_version != attempt.base_version {
            return Err(anyhow!("Attempt replay evidence identity mismatch"));
        }
        Ok(Some(snapshot))
    }
}

#[cfg(test)]
#[path = "storage_replay_tests.rs"]
mod tests;
