use anyhow::{anyhow, Context, Result};
use formless_protocol::{
    MessageInfo, ModuleInfo, PromptProposalInfo, SearchHit, SessionInfo, SubagentInfo,
    TemplateInfo, ToolInfo, ToolMetricsInfo, ToolVersionInfo, TopicAttemptInfo, TopicFeatureInfo,
    TopicInfo,
};
use formless_runtime::Capabilities;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[cfg(test)]
#[path = "job_schedule_tests.rs"]
mod job_schedule_tests;

#[cfg(test)]
#[path = "pin_batch_tests.rs"]
mod pin_batch_tests;

#[cfg(test)]
#[path = "run_id_lookup_tests.rs"]
mod run_id_lookup_tests;

#[path = "short_ids.rs"]
mod short_ids;
use short_ids::insert_short_id;

/// A usage bucket's key, provider, model, prompt, completion, cache read and
/// write tokens, reported cost, and whether the cost is explicitly unknown.
pub type GroupedUsage = (
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    Option<f64>,
    bool,
);

#[path = "storage_forecast.rs"]
mod forecast_storage;

#[path = "forecast_snapshot.rs"]
mod forecast_snapshot;

#[path = "pipeline_storage.rs"]
mod pipeline_storage;

#[path = "optimiser_attempt_schema.rs"]
mod optimiser_attempt_schema;

#[path = "storage_replay.rs"]
mod replay_storage;
pub use replay_storage::ReplaySnapshot;

#[path = "runfile_storage.rs"]
mod runfile_storage;
pub use runfile_storage::{
    RunFileAdmission, RunFileRecord, RunFileRetention, RunFileUsage, StoredRunFile,
};

#[path = "run_state_storage.rs"]
mod run_state_storage;

#[path = "topic_storage.rs"]
mod topic_storage;

#[cfg(test)]
#[path = "topic_storage_tests.rs"]
mod topic_storage_tests;

#[path = "attachment_expiry.rs"]
mod attachment_expiry;

#[path = "recap_storage.rs"]
mod recap_storage;
pub(crate) use recap_storage::RecapRunFacts;

#[path = "forecast_report_storage.rs"]
mod forecast_report_storage;
pub(crate) use forecast_report_storage::ForecastMetricInput;

pub struct Store {
    conn: Connection,
    /// Bumped whenever stored messages change other than by appending
    /// (edit/truncate, delete), so [`ContextCache`]s know to rebuild.
    rewrites: std::sync::atomic::AtomicU64,
    // Connection drops before this private directory; source files are never
    // held by a forecast reader or touched by close-time optimize.
    _forecast_snapshot: Option<forecast_snapshot::SnapshotDir>,
    pub(super) artifact_root: Option<PathBuf>,
    description_notifications: std::sync::Mutex<(Vec<formless_protocol::DescriptionReview>, bool)>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ReplayItem {
    pub input: String,
    pub error: String,
    pub tag: String,
    pub recorded_success: bool,
}

#[derive(Debug, Clone)]
pub struct CandidateData {
    pub code: String,
    pub description: String,
    pub input_schema: String,
    pub output_schema: String,
    pub capabilities: Vec<String>,
    pub memory_mb: u32,
}

#[derive(Debug, Clone)]
pub struct AttemptRecord {
    pub tool_id: String,
    pub base_version: i64,
    pub status: String,
    pub candidate: CandidateData,
}

/// Newest `*.run_unasked` audit records kept (approvals-off mode logs one
/// per sensitive call).
pub const UNASKED_KEEP: i64 = 2000;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
#[test]
fn tool_list_reports_the_latest_retained_start_independently_of_metadata() {
    let store = Store::open_in_memory().unwrap();
    let caps = Capabilities::parse(&[]).unwrap();
    let (tool, version) = store
        .create_tool_version(
            "usage_fixture",
            "usage fixture",
            "{}",
            "{}",
            "",
            &caps,
            64,
            "user",
        )
        .unwrap();
    let (unused, _) = store
        .create_tool_version("unused_fixture", "", "{}", "{}", "", &caps, 64, "user")
        .unwrap();
    let running = store
        .start_invocation(&tool.id, version, None, None, None, "{}")
        .unwrap();
    let older = store
        .record_invocation(
            &tool.id, version, None, None, None, "denied", "", "{}", "", "", 0, 20,
        )
        .unwrap();
    let latest = "2026-01-02T03:04:05+00:00";
    for (id, stamp) in [(&running, latest), (&older, "2026-01-01T03:04:05+00:00")] {
        store
            .conn
            .execute(
                "UPDATE invocations SET created_at=?2 WHERE id=?1",
                params![id, stamp],
            )
            .unwrap();
    }
    let rows = store.list_tools(true).unwrap();
    let used = rows.iter().find(|row| row.id == tool.id).unwrap();
    assert_eq!(used.updated_at, tool.updated_at);
    assert_eq!(used.invocations, 2);
    assert_eq!(used.last_used_at.as_deref(), Some(latest));
    assert_eq!(
        store
            .tool_last_used()
            .unwrap()
            .get(&tool.id)
            .map(String::as_str),
        Some(latest)
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.id == unused.id)
            .unwrap()
            .last_used_at,
        None
    );

    let (revised, revised_version) = store
        .create_tool_version(
            "usage_fixture",
            "updated usage fixture",
            "{}",
            "{}",
            "function run() { return {}; }",
            &caps,
            64,
            "user",
        )
        .unwrap();
    assert_eq!(revised.id, tool.id);
    assert!(revised_version > version);
    assert_eq!(revised.last_used_at.as_deref(), Some(latest));
    let rolled_back = store.rollback_tool(&tool.id, version).unwrap().unwrap();
    assert_eq!(rolled_back.current_version, version);
    assert_eq!(rolled_back.last_used_at.as_deref(), Some(latest));

    store.set_needs_attention(&tool.id, true).unwrap();
    let retired = store.set_tool_retired(&tool.id, true).unwrap().unwrap();
    assert!(retired.retired && retired.needs_attention);
    assert_eq!(retired.last_used_at.as_deref(), Some(latest));
    assert_eq!(
        store
            .get_tool_current(&tool.id)
            .unwrap()
            .unwrap()
            .0
            .last_used_at
            .as_deref(),
        Some(latest)
    );
    store
        .finish_invocation(&running, &tool.id, "success", "", "", "", 1, 0)
        .unwrap();
    let pruned = store
        .list_tools(true)
        .unwrap()
        .into_iter()
        .find(|row| row.id == tool.id)
        .unwrap();
    assert_eq!(pruned.invocations, 0);
    assert_eq!(pruned.last_used_at, None);
}

const RUN_INSPECT_EXACT: &str = "SELECT id,session_id,status,error,steps,created_at,updated_at,
    start_message_seq,end_message_seq,
    (SELECT COUNT(*) FROM invocations WHERE run_id=runs.id)
    FROM runs WHERE id=?1";
const RUN_ID_PREFIX_RANGE: &str = "SELECT id FROM runs WHERE id>=?1 AND id<?2 ORDER BY id LIMIT 2";
const RUN_ID_PREFIX_TAIL: &str = "SELECT id FROM runs WHERE id>=?1 ORDER BY id LIMIT 2";
const RUN_ID_PREFIX_OTHER_ENCODING: &str =
    "SELECT id FROM runs WHERE substr(id,1,length(?1))=?1 LIMIT 2";

/// The exclusive upper bound for a literal prefix in SQLite's BINARY
/// ordering on Formless's UTF-8 databases. A prefix made entirely of the
/// maximum scalar has no upper bound.
fn run_id_prefix_end(prefix: &str) -> Option<String> {
    let mut upper = prefix.trim_end_matches(char::MAX).to_owned();
    let last = upper.pop()?;
    let next = if last == '\u{d7ff}' {
        '\u{e000}'
    } else {
        char::from_u32(last as u32 + 1)?
    };
    upper.push(next);
    Some(upper)
}

fn make_snippet(content: &str, needle_lower: &str) -> String {
    let flat: String = content
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let chars: Vec<char> = flat.chars().collect();
    // Found in a folded copy that keeps each char's original index, so
    // the snippet centres on the match whatever case folding or Unicode
    // normal form did to the length (#789).
    let found = crate::fuzzy::find_folded(&flat, needle_lower).unwrap_or(0..0);
    let start = found.start.saturating_sub(40);
    let end = (found.end + 60).min(chars.len());
    let core: String = chars[start..end].iter().collect();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        core,
        if end < chars.len() { "…" } else { "" }
    )
}

/// For multi-term searches, fit as many distinct word-prefix matches as
/// possible into 100 original characters, plus ellipses. Match from the
/// start of alphanumeric words with the app's accent-insensitive folding.
/// Combining marks stay with the preceding word. SQLite still decides which
/// rows match, using its own tokenizer.
fn multi_term_snippet(content: &str, terms: &HashMap<String, usize>) -> String {
    const BUDGET: usize = 100;
    let max_term_bytes = terms.keys().map(String::len).max().unwrap_or(0);
    let mut counts = vec![0; terms.len()];
    let mut covered = 0;
    let mut window: VecDeque<(usize, usize, Vec<usize>)> = VecDeque::new();
    let mut folded = String::new();
    let mut best = ((0, Reverse(0), Reverse(0)), 0, 0, 0);
    let mut word_start = None;
    let chars = content.char_indices().chain([(content.len(), '\0')]);
    for (end_char, (end_byte, ch)) in chars.enumerate() {
        if ch.is_alphanumeric() {
            word_start.get_or_insert((end_char, end_byte));
            continue;
        }
        if unicode_normalization::char::is_combining_mark(ch) {
            continue;
        }
        let Some((start_char, start_byte)) = word_start.take() else {
            continue;
        };
        if end_char - start_char > BUDGET {
            continue;
        }
        folded.clear();
        let mut matched = Vec::new();
        for key in crate::fuzzy::fold_chars(&content[start_byte..end_byte]) {
            if folded.len() + key.len_utf8() > max_term_bytes {
                break;
            }
            folded.push(key);
            if let Some(&id) = terms.get(&folded) {
                matched.push(id);
                covered += usize::from(counts[id] == 0);
                counts[id] += 1;
            }
        }
        if matched.is_empty() {
            continue;
        }
        window.push_back((start_char, start_byte, matched));
        // Remove out-of-budget words and any redundant leading matches to
        // find the shortest span with this coverage ending at this word.
        while window.front().is_some_and(|(start, _, ids)| {
            end_char - start > BUDGET || ids.iter().all(|&id| counts[id] > 1)
        }) {
            let (_, _, ids) = window.pop_front().unwrap();
            for id in ids {
                counts[id] -= 1;
                covered -= usize::from(counts[id] == 0);
            }
        }
        if let Some(&(start, from, _)) = window.front() {
            let width = end_char - start;
            let score = (covered, Reverse(width), Reverse(start));
            if score > best.0 {
                best = (score, from, end_byte, width);
            }
        }
    }
    let (_, from, to, width) = best;
    let mut start = from;
    let mut after = BUDGET - width;
    for (index, _) in content[..from].char_indices().rev().take(after.min(40)) {
        start = index;
        after -= 1;
    }
    let end = content[to..]
        .char_indices()
        .nth(after)
        .map_or(content.len(), |(index, _)| to + index);
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        content[start..end].replace('\n', " "),
        if end < content.len() { "…" } else { "" }
    )
}

/// Name of a Session nobody has named yet; auto-naming replaces it.
pub const DEFAULT_SESSION_NAME: &str = "Untitled";

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY, name TEXT NOT NULL,
  model TEXT, reasoning_effort TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON messages(session_id, seq);
CREATE TABLE IF NOT EXISTS runs (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  status TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS checkpoints (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  summary TEXT NOT NULL, up_to_seq INTEGER NOT NULL, created_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS audit (
  id TEXT PRIMARY KEY, actor TEXT NOT NULL, action TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL);
";

const SCHEMA_V2: &str = "
CREATE TABLE IF NOT EXISTS tools (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, name_lower TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL DEFAULT '', retired INTEGER NOT NULL DEFAULT 0,
  current_version INTEGER NOT NULL DEFAULT 0, needs_attention INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tool_versions (
  id TEXT PRIMARY KEY, tool_id TEXT NOT NULL REFERENCES tools(id) ON DELETE CASCADE,
  version INTEGER NOT NULL, input_schema TEXT NOT NULL, output_schema TEXT NOT NULL,
  code TEXT NOT NULL, capabilities TEXT NOT NULL DEFAULT '[]', memory_mb INTEGER NOT NULL DEFAULT 256,
  author TEXT NOT NULL DEFAULT 'agent', created_at TEXT NOT NULL, UNIQUE(tool_id, version));
CREATE TABLE IF NOT EXISTS invocations (
  id TEXT PRIMARY KEY, tool_id TEXT NOT NULL REFERENCES tools(id) ON DELETE CASCADE,
  version INTEGER NOT NULL, session_id TEXT, run_id TEXT, parent_id TEXT,
  status TEXT NOT NULL, tag TEXT NOT NULL DEFAULT '', input TEXT NOT NULL DEFAULT '',
  output TEXT NOT NULL DEFAULT '', error TEXT NOT NULL DEFAULT '', duration_ms INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_inv_tool ON invocations(tool_id, version, created_at);
CREATE TABLE IF NOT EXISTS approvals (
  tool_id TEXT PRIMARY KEY REFERENCES tools(id) ON DELETE CASCADE,
  capabilities TEXT NOT NULL DEFAULT '[]', memory_mb INTEGER NOT NULL DEFAULT 256, approved_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS artifacts (
  id TEXT PRIMARY KEY, session_id TEXT, path TEXT NOT NULL, preview TEXT NOT NULL DEFAULT '',
  bytes INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);
";

const SCHEMA_V3: &str = "
CREATE TABLE IF NOT EXISTS optimiser_attempts (
  id TEXT PRIMARY KEY, tool_id TEXT NOT NULL REFERENCES tools(id) ON DELETE CASCADE,
  base_version INTEGER NOT NULL, status TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '',
  candidate_code TEXT NOT NULL DEFAULT '', candidate_description TEXT NOT NULL DEFAULT '',
  candidate_input_schema TEXT NOT NULL DEFAULT '{}', candidate_output_schema TEXT NOT NULL DEFAULT '{}',
  candidate_capabilities TEXT NOT NULL DEFAULT '[]', candidate_memory_mb INTEGER NOT NULL DEFAULT 256,
  candidate_score REAL, baseline_score REAL, replay_total INTEGER NOT NULL DEFAULT 0,
  regressions INTEGER NOT NULL DEFAULT 0, est_tokens INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_attempts_tool ON optimiser_attempts(tool_id, created_at);
";

const SCHEMA_V4: &str = "
CREATE TABLE IF NOT EXISTS modules (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, name_lower TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL DEFAULT '', retired INTEGER NOT NULL DEFAULT 0,
  current_version INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS module_versions (
  id TEXT PRIMARY KEY, module_id TEXT NOT NULL REFERENCES modules(id) ON DELETE CASCADE,
  version INTEGER NOT NULL, code TEXT NOT NULL,
  author TEXT NOT NULL DEFAULT 'agent', created_at TEXT NOT NULL, UNIQUE(module_id, version));
";

const SCHEMA_V5: &str = "
CREATE TABLE IF NOT EXISTS prompt_proposals (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, content TEXT NOT NULL,
  rationale TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'proposed',
  baseline_score REAL, candidate_score REAL, eval_total INTEGER NOT NULL DEFAULT 0,
  est_tokens INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, applied_at TEXT);
CREATE INDEX IF NOT EXISTS idx_proposals_status ON prompt_proposals(status, created_at);
";

const SCHEMA_V6: &str = "
CREATE TABLE IF NOT EXISTS model_stats (
  model TEXT PRIMARY KEY, successes INTEGER NOT NULL DEFAULT 0,
  failures INTEGER NOT NULL DEFAULT 0, total_ms INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL);
";

const SCHEMA_V7: &str = "
CREATE TABLE IF NOT EXISTS session_tools (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  tool_id TEXT NOT NULL REFERENCES tools(id) ON DELETE CASCADE,
  source TEXT NOT NULL DEFAULT 'found', loaded_at TEXT NOT NULL,
  PRIMARY KEY (session_id, tool_id));
";

const SCHEMA_V8: &str = "
CREATE TABLE IF NOT EXISTS queued_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  content TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_queued_session ON queued_messages(session_id, id);
";

const SCHEMA_V9: &str = "
CREATE TABLE IF NOT EXISTS pending_handles (
  handle TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  tool_name TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
  created_at TEXT NOT NULL);
";

const SCHEMA_V10: &str = "
CREATE TABLE IF NOT EXISTS subagents (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL, task TEXT NOT NULL, depth INTEGER NOT NULL,
  status TEXT NOT NULL, steps INTEGER NOT NULL DEFAULT 0, result TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_subagents_session ON subagents(session_id, created_at);
CREATE TABLE IF NOT EXISTS subagent_messages (
  subagent_id TEXT NOT NULL REFERENCES subagents(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL, message TEXT NOT NULL,
  PRIMARY KEY (subagent_id, seq));
";

const SCHEMA_V11: &str = "
CREATE TABLE IF NOT EXISTS run_usage (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL, model TEXT NOT NULL,
  prompt_tokens INTEGER NOT NULL, completion_tokens INTEGER NOT NULL,
  reasoning_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
  created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_run_usage_session ON run_usage(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_run_usage_run ON run_usage(run_id);
";

/// Metadata-only Skill projection for internal callers that do not use metrics.
#[derive(Debug, PartialEq)]
pub(crate) struct SkillSummary {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub retired: bool,
}

/// Tool definitions as a Session last listed them (pinned for the prompt
/// cache until that Session refreshes them).
/// Columns for [`Store::row_skill`]: the row plus usage metrics.
const SKILL_COLS: &str = "s.*,
  (SELECT COUNT(*) FROM skill_loads l WHERE l.skill_id=s.id) AS loads,
  (SELECT COUNT(DISTINCT NULLIF(l.run_id,'')) FROM skill_loads l WHERE l.skill_id=s.id) AS runs,
  (SELECT COUNT(DISTINCT l.run_id) FROM skill_loads l JOIN runs r ON r.id=l.run_id
     WHERE l.skill_id=s.id AND r.status='completed' AND r.followup='') AS runs_completed";

const SCHEMA_V16: &str = "
CREATE TABLE IF NOT EXISTS skills (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, description TEXT NOT NULL,
  current_version INTEGER NOT NULL, enabled INTEGER NOT NULL DEFAULT 0,
  source TEXT NOT NULL DEFAULT '', author TEXT NOT NULL DEFAULT 'user',
  has_scripts INTEGER NOT NULL DEFAULT 0, retired INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS skill_versions (
  skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
  version INTEGER NOT NULL, path TEXT NOT NULL, created_at TEXT NOT NULL,
  PRIMARY KEY (skill_id, version));
CREATE TABLE IF NOT EXISTS skill_loads (
  skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
  session_id TEXT, run_id TEXT, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_skill_loads ON skill_loads(skill_id, created_at);
";

/// User memory (SPEC §7.11).
const SCHEMA_V17: &str = "
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY, text TEXT NOT NULL, dedup_key TEXT NOT NULL,
  scope TEXT NOT NULL DEFAULT 'global', author TEXT NOT NULL DEFAULT 'agent',
  session_id TEXT, enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_used TEXT);
CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_dedup ON memories(scope, dedup_key);
";

/// Tool key-value store (SPEC §7.4 `store`).
const SCHEMA_V23: &str = "
CREATE TABLE IF NOT EXISTS tool_kv (
  namespace TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
  updated_at TEXT NOT NULL, PRIMARY KEY (namespace, key));
";

/// Full-text index over message content (SPEC §5.1 search), kept in step
/// with `messages` by triggers and backfilled once.
/// Orchestration scripts (SPEC §7.12). Nested Invocations link to theirs
/// through `parent_id` = "script:<id>".
const SCHEMA_V28: &str = "
CREATE TABLE IF NOT EXISTS scripts (
  id TEXT PRIMARY KEY, session_id TEXT, run_id TEXT, hash TEXT NOT NULL,
  source TEXT NOT NULL, uses TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL,
  error TEXT NOT NULL DEFAULT '', duration_ms INTEGER NOT NULL DEFAULT 0,
  saved_tool TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_scripts_hash ON scripts(hash, session_id);
";

const SCHEMA_V24: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
  content, content='messages', content_rowid='rowid',
  tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS messages_fts_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
END;
CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
END;
CREATE TRIGGER IF NOT EXISTS messages_fts_au AFTER UPDATE OF content ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
  INSERT INTO messages_fts(rowid, content) VALUES (new.rowid, new.content);
END;
INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');
";

/// Largest stored value (JSON bytes) and namespace total.
pub const KV_MAX_VALUE: usize = 64 * 1024;
pub const KV_MAX_NAMESPACE: i64 = 5 * 1024 * 1024;

/// Token use of one model: (model, prompt, completion, cached,
/// provider-reported USD when every request reported one).
/// (provider, model, prompt, completion, cached, cache-write, reported cost, unknown native cost).
/// Separate cost-basis groups preserve reported-cost precedence in mixed rows.
pub type ModelUsage = (String, String, i64, i64, i64, i64, Option<f64>, bool);

/// Scheduled jobs (SPEC §9.7).
const SCHEMA_V22: &str = "
CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY, prompt TEXT NOT NULL, session_id TEXT,
  cron TEXT, next_run TEXT, last_run TEXT, last_status TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 1, approved INTEGER NOT NULL DEFAULT 0,
  author TEXT NOT NULL DEFAULT 'user', created_at TEXT NOT NULL);
";

/// Built-in calls, without payloads (SPEC §8.8).
const SCHEMA_V20: &str = "
CREATE TABLE IF NOT EXISTS builtin_calls (
  name TEXT NOT NULL, session_id TEXT, run_id TEXT, ok INTEGER NOT NULL,
  kind TEXT NOT NULL DEFAULT '', duration_ms INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_builtin_calls ON builtin_calls(name, created_at);
CREATE INDEX IF NOT EXISTS idx_builtin_calls_run ON builtin_calls(run_id);
";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelStat {
    pub model: String,
    pub successes: i64,
    pub failures: i64,
    pub total_ms: i64,
}

impl ModelStat {
    pub fn samples(&self) -> i64 {
        self.successes + self.failures
    }
    pub fn success_rate(&self) -> Option<f64> {
        let n = self.samples();
        (n > 0).then_some(self.successes as f64 / n as f64)
    }
    pub fn avg_ms(&self) -> Option<f64> {
        let n = self.samples();
        (n > 0).then_some(self.total_ms as f64 / n as f64)
    }
}

/// Per-Session summary columns for the Sessions list. They live on the
/// Session row itself (`message_count`, `preview`, `last_run_status`,
/// schema v39) and triggers on `messages`/`runs` keep them true, so the
/// list is one scan instead of three correlated subqueries per Session.
const SESSION_SUMMARY: &str = "s.*";

// These indexes are rebuilt from existing rows and change no record layout.
// Install them on writable opens without accepting a new schema stamp.
const DERIVED_INDEXES: &str = "
    CREATE INDEX IF NOT EXISTS idx_checkpoints_session ON checkpoints(session_id, up_to_seq);
    CREATE INDEX IF NOT EXISTS idx_run_usage_created ON run_usage(created_at);
    CREATE INDEX IF NOT EXISTS idx_inv_replay_recent ON invocations(tool_id)
        WHERE status IN ('success','failure');
";

const LATEST_CHECKPOINT_SQL: &str = "SELECT id, summary, up_to_seq FROM checkpoints
     WHERE session_id=?1 ORDER BY up_to_seq DESC, rowid DESC LIMIT 1";

fn usage_by_model_sql(bounded: bool) -> String {
    let filter = if bounded {
        "WHERE created_at >= ?1"
    } else {
        ""
    };
    format!(
        "SELECT model, SUM(prompt_tokens), SUM(completion_tokens), SUM(cached_tokens)
         FROM run_usage {filter} GROUP BY model"
    )
}

fn grouped_usage_sql(by: &str, bounded: bool) -> String {
    let key = match by {
        "model" => "u.model",
        "provider" => "CASE u.provider WHEN '' THEN 'default' ELSE u.provider END",
        "purpose" => "u.source",
        "session" => "u.session_id",
        "role" => {
            "COALESCE((SELECT t.role FROM task_stats t WHERE t.run_id=u.run_id LIMIT 1), 'main')"
        }
        _ => "substr(u.created_at, 1, 10)",
    };
    let (label, order) = if by == "session" {
        (
            "COALESCE((SELECT s.name FROM sessions s WHERE s.id=u.session_id), u.session_id)",
            // Keep alphabetical labels on token-count ties. The ID
            // makes buckets with an identical label stay contiguous.
            "10, 1",
        )
    } else {
        ("NULL", "1")
    };
    let filter = if bounded {
        "WHERE u.created_at >= ?1"
    } else {
        ""
    };
    format!(
        "SELECT {key}, u.provider, u.model, SUM(u.prompt_tokens), SUM(u.completion_tokens),
           SUM(u.cached_tokens), SUM(u.cache_write_tokens),
           CASE WHEN COUNT(u.cost)=COUNT(*) THEN SUM(u.cost) END, MAX(u.cost_unknown), {label}
         FROM run_usage u
         {filter}
         GROUP BY 1, 2, 3, (u.cost IS NULL), u.cost_unknown ORDER BY {order}"
    )
}

#[cfg(test)]
#[test]
fn checkpoint_and_usage_range_queries_use_their_indexes() {
    let store = Store::open_in_memory().unwrap();
    let session = store.create_session(Some("fixture".into())).unwrap().id;
    store
        .conn
        .execute(
            "WITH RECURSIVE fixture(n) AS (
           SELECT 1 UNION ALL SELECT n + 1 FROM fixture WHERE n < 2000
         )
         INSERT INTO run_usage (session_id, run_id, model, prompt_tokens, completion_tokens,
                                reasoning_tokens, cached_tokens, created_at, source, provider)
         SELECT ?1, 'fixture', 'model', 1, 2, 0, 0,
                printf('2026-01-%02dT00:00:00Z', n % 28 + 1), 'run', ''
         FROM fixture",
            params![session],
        )
        .unwrap();
    store
        .conn
        .execute(
            "WITH RECURSIVE fixture(n) AS (
           SELECT 1 UNION ALL SELECT n + 1 FROM fixture WHERE n < 200
         )
         INSERT INTO checkpoints (id, session_id, summary, up_to_seq, created_at)
         SELECT printf('fixture-%d', n), ?1, 'old', n, '2026-01-01T00:00:00Z' FROM fixture",
            params![session],
        )
        .unwrap();
    let newest = store.save_checkpoint(&session, "newest", 200).unwrap();
    let plan = |sql: &str, param: &str| {
        let mut stmt = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let plan = stmt
            .query_map([param], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        plan.join("\n")
    };
    let checkpoint_plan = plan(LATEST_CHECKPOINT_SQL, &session);
    assert!(
        checkpoint_plan.contains("USING INDEX idx_checkpoints_session"),
        "{checkpoint_plan}"
    );
    assert!(
        !checkpoint_plan.contains("TEMP B-TREE"),
        "{checkpoint_plan}"
    );
    let latest = store.latest_checkpoint_record(&session).unwrap().unwrap();
    assert_eq!(latest, (newest, "newest".into(), 200));

    let since = "2026-01-28T00:00:00Z";
    let check_range = |sql: &str| {
        let usage_plan = plan(sql, since);
        assert!(
            usage_plan.contains("USING INDEX idx_run_usage_created (created_at>?)"),
            "{usage_plan}"
        );
    };
    check_range(&usage_by_model_sql(true));
    for group in ["model", "provider", "purpose", "session", "role", "day"] {
        check_range(&grouped_usage_sql(group, true));
    }
    assert_eq!(
        store.usage_by_model(None).unwrap()["model"],
        (2000, 4000, 0)
    );
    assert_eq!(
        store.usage_by_model(Some(since)).unwrap()["model"],
        (71, 142, 0)
    );
    assert_eq!(
        store.usage_grouped(Some(since), "model").unwrap(),
        [(
            "model".into(),
            "".into(),
            "model".into(),
            71,
            142,
            0,
            0,
            None,
            false
        )]
    );
}

fn run_duration_ms(created: &str, updated: &str) -> i64 {
    match (
        chrono::DateTime::parse_from_rfc3339(created),
        chrono::DateTime::parse_from_rfc3339(updated),
    ) {
        (Ok(start), Ok(end)) => (end - start).num_milliseconds().max(0),
        _ => 0,
    }
}

/// Schema version this build writes (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 76;

#[path = "topic_context.rs"]
mod topic_context;

#[path = "topic_schema_v72.rs"]
mod topic_schema_v72;

#[cfg(test)]
#[path = "topic_schema_v72_tests.rs"]
mod topic_schema_v72_tests;

const SCHEMA_V71: &str = "
CREATE TABLE IF NOT EXISTS profile_metadata (key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS topics (
 id TEXT PRIMARY KEY,name TEXT NOT NULL,parent_topic_id TEXT REFERENCES topics(id) ON DELETE CASCADE,
 workspace TEXT,workspace_kind TEXT NOT NULL,
 base_revision TEXT,outline TEXT NOT NULL DEFAULT '[]',
 source_session_id TEXT NOT NULL,
 topic_session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE,status TEXT NOT NULL,
 redirect_message_id TEXT,rejection_reason TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_topics_status_updated ON topics(status,updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_topics_parent ON topics(parent_topic_id);
CREATE TABLE IF NOT EXISTS topic_repo_locks (
 workspace TEXT PRIMARY KEY,topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,acquired_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS topic_source_messages (
 topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,source_session_id TEXT NOT NULL,
 message_id TEXT NOT NULL,source_seq INTEGER NOT NULL,source_content TEXT NOT NULL,
 source_content_hash TEXT NOT NULL,moved_at TEXT,PRIMARY KEY(topic_id,message_id));
CREATE TABLE IF NOT EXISTS topic_features (
 id TEXT PRIMARY KEY,topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
 source_message_id TEXT NOT NULL,source_seq INTEGER NOT NULL,request TEXT NOT NULL,status TEXT NOT NULL,
 active_attempt_id TEXT,outputs TEXT NOT NULL DEFAULT '[]',attempt_count INTEGER NOT NULL DEFAULT 0,
 detail TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_topic_features_status ON topic_features(topic_id,status,source_seq);
CREATE TABLE IF NOT EXISTS topic_attempts (
 id TEXT PRIMARY KEY,topic_id TEXT NOT NULL REFERENCES topics(id) ON DELETE CASCADE,
 feature_id TEXT NOT NULL REFERENCES topic_features(id) ON DELETE CASCADE,step_title TEXT NOT NULL,
 access TEXT NOT NULL,harness_kind TEXT NOT NULL,model TEXT,acp_agent TEXT,inputs TEXT NOT NULL DEFAULT '[]',
 outputs TEXT NOT NULL DEFAULT '[]',summary TEXT,status TEXT NOT NULL,
 conversation_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 capability_hash TEXT NOT NULL,worktree_path TEXT,outputs_dir TEXT,harness_session_id TEXT,child_pid INTEGER,
 detail TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_topic_attempts_feature ON topic_attempts(feature_id,created_at);
";

fn migrate_v71(c: &Connection) -> Result<()> {
    add_column(c, "sessions", "parent_session_id TEXT")?;
    add_column(c, "sessions", "session_kind TEXT NOT NULL DEFAULT 'chat'")?;
    add_column(c, "sessions", "topic_id TEXT")?;
    add_column(c, "sessions", "topic_attempt_id TEXT")?;
    add_column(c, "sessions", "harness_kind TEXT")?;
    c.execute_batch("CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id, session_kind);")?;
    c.execute_batch(SCHEMA_V71)?;
    Ok(())
}

fn unsupported_preview_schema(version: i64) -> bool {
    matches!(version, 68 | 69)
}

/// Add a column, tolerating only "it already exists" (a step that was
/// partly applied by an older build); every other error is real.
fn add_column(conn: &Connection, table: &str, column_def: &str) -> Result<()> {
    match conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column_def}"), []) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column name") => Ok(()),
        Err(e) => Err(e).with_context(|| format!("add column {table}.{column_def}")),
    }
}

/// Indexes the query plans were missing (found with `EXPLAIN QUERY PLAN`
/// against a real database): `list_sessions` scanned `runs` once per
/// Session row, `attribute_step_tokens`/`run_metrics_for` scanned
/// `invocations` per call, and the daemon's orphan sweep scanned it at
/// startup.
const SCHEMA_V39_INDEXES: &str = "
CREATE INDEX IF NOT EXISTS idx_runs_session ON runs(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_inv_run ON invocations(run_id);
CREATE INDEX IF NOT EXISTS idx_inv_session ON invocations(session_id, tool_id);
CREATE INDEX IF NOT EXISTS idx_inv_running ON invocations(tool_id) WHERE status='running';
CREATE INDEX IF NOT EXISTS idx_artifacts_session ON artifacts(session_id);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit(created_at);
";

/// Triggers that keep `sessions.message_count`, `sessions.preview` and
/// `sessions.last_run_status` true, so the Sessions list reads them
/// instead of counting messages per row (measured 0.47 ms -> 0.07 ms for
/// 27 Sessions on the reference database; a message append pays ~0.07 ms
/// for the trigger, which the write-side PRAGMA wins back several times
/// over).
const SCHEMA_V39_TRIGGERS: &str = "
CREATE TRIGGER IF NOT EXISTS sessions_msg_ai AFTER INSERT ON messages BEGIN
  UPDATE sessions SET message_count = message_count + 1,
    preview = CASE WHEN new.role='user' THEN substr(new.content,1,2000) ELSE preview END
  WHERE id = new.session_id;
END;
CREATE TRIGGER IF NOT EXISTS sessions_msg_ad AFTER DELETE ON messages BEGIN
  UPDATE sessions SET message_count = message_count - 1,
    preview = CASE
      WHEN old.role='user' AND NOT EXISTS (
        SELECT 1 FROM messages WHERE session_id=old.session_id AND role='user' AND seq > old.seq)
      THEN (SELECT substr(content,1,2000) FROM messages WHERE session_id=old.session_id AND role='user' ORDER BY seq DESC LIMIT 1)
      ELSE preview END
  WHERE id = old.session_id;
END;
CREATE TRIGGER IF NOT EXISTS sessions_msg_au AFTER UPDATE OF content ON messages BEGIN
  UPDATE sessions SET
    preview = (SELECT substr(content,1,2000) FROM messages WHERE session_id=old.session_id AND role='user' ORDER BY seq DESC LIMIT 1)
  WHERE id = old.session_id AND old.role='user';
END;
CREATE TRIGGER IF NOT EXISTS sessions_run_ai AFTER INSERT ON runs BEGIN
  UPDATE sessions SET last_run_status = new.status WHERE id = new.session_id;
END;
CREATE TRIGGER IF NOT EXISTS sessions_run_au AFTER UPDATE OF status ON runs BEGIN
  UPDATE sessions SET last_run_status = (
    SELECT status FROM runs WHERE session_id=new.session_id ORDER BY created_at DESC, rowid DESC LIMIT 1)
  WHERE id = new.session_id;
END;
";

/// v39: indexes, then the denormalised Session summary backfilled from the
/// existing rows before its triggers take over.
fn migrate_v39(c: &Connection) -> Result<()> {
    c.execute_batch(SCHEMA_V39_INDEXES)?;
    add_column(c, "sessions", "message_count INTEGER NOT NULL DEFAULT 0")?;
    add_column(c, "sessions", "preview TEXT")?;
    add_column(c, "sessions", "last_run_status TEXT")?;
    c.execute_batch(
        "UPDATE sessions SET
           message_count = (SELECT COUNT(*) FROM messages m WHERE m.session_id=sessions.id),
           preview = (SELECT substr(content,1,2000) FROM messages m
                        WHERE m.session_id=sessions.id AND m.role='user'
                        ORDER BY m.seq DESC LIMIT 1),
           last_run_status = (SELECT status FROM runs r WHERE r.session_id=sessions.id
                                ORDER BY r.created_at DESC, r.rowid DESC LIMIT 1);",
    )?;
    c.execute_batch(SCHEMA_V39_TRIGGERS)?;
    Ok(())
}

type MigrationStep = fn(&Connection) -> Result<()>;

/// Migration steps by the version they produce.
const MIGRATIONS: [(i64, MigrationStep); 74] = [
    (1, |c| Ok(c.execute_batch(SCHEMA_V1)?)),
    (2, |c| {
        add_column(c, "messages", "meta TEXT")?;
        Ok(c.execute_batch(SCHEMA_V2)?)
    }),
    (3, |c| {
        add_column(c, "tools", "cooldown_until TEXT")?;
        add_column(
            c,
            "tools",
            "consecutive_failures INTEGER NOT NULL DEFAULT 0",
        )?;
        add_column(c, "invocations", "retried INTEGER NOT NULL DEFAULT 0")?;
        Ok(c.execute_batch(SCHEMA_V3)?)
    }),
    (4, |c| Ok(c.execute_batch(SCHEMA_V4)?)),
    (5, |c| Ok(c.execute_batch(SCHEMA_V5)?)),
    (6, |c| Ok(c.execute_batch(SCHEMA_V6)?)),
    (7, |c| Ok(c.execute_batch(SCHEMA_V7)?)),
    (8, |c| Ok(c.execute_batch(SCHEMA_V8)?)),
    (9, |c| Ok(c.execute_batch(SCHEMA_V9)?)),
    (10, |c| Ok(c.execute_batch(SCHEMA_V10)?)),
    (11, |c| Ok(c.execute_batch(SCHEMA_V11)?)),
    (12, |c| {
        add_column(c, "session_tools", "pinned_version INTEGER")?;
        add_column(c, "session_tools", "pinned_def TEXT")
    }),
    (13, |c| add_column(c, "sessions", "system_prompt TEXT")),
    (14, |c| {
        add_column(c, "session_tools", "unload INTEGER NOT NULL DEFAULT 0")
    }),
    (15, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS mcp_server_grants (server TEXT PRIMARY KEY, approved_at TEXT NOT NULL);",
        )?)
    }),
    (16, |c| Ok(c.execute_batch(SCHEMA_V16)?)),
    (17, |c| Ok(c.execute_batch(SCHEMA_V17)?)),
    (18, |c| {
        add_column(c, "memories", "proposal_id TEXT")?;
        short_ids::migrate_applied_memories(c, uuid::Uuid::new_v4)
    }),
    // Run-level metrics (SPEC §8.7).
    (19, |c| {
        add_column(c, "runs", "steps INTEGER NOT NULL DEFAULT 0")?;
        add_column(c, "runs", "followup TEXT NOT NULL DEFAULT ''")
    }),
    (20, |c| Ok(c.execute_batch(SCHEMA_V20)?)),
    // Provider tokens attributed to Invocations (SPEC §8.1 metric 3).
    (21, |c| {
        add_column(c, "invocations", "tokens INTEGER NOT NULL DEFAULT 0")
    }),
    (22, |c| Ok(c.execute_batch(SCHEMA_V22)?)),
    (23, |c| Ok(c.execute_batch(SCHEMA_V23)?)),
    (24, |c| Ok(c.execute_batch(SCHEMA_V24)?)),
    (25, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS optimiser_queue (tool_id TEXT PRIMARY KEY, queued_at TEXT NOT NULL);",
        )?)
    }),
    // Why a Run stopped (paused provider error, crash, interrupt).
    (26, |c| {
        add_column(c, "runs", "error TEXT NOT NULL DEFAULT ''")
    }),
    // Session delete with an undo window (SPEC §5.1).
    (27, |c| add_column(c, "sessions", "deleted_at TEXT")),
    (28, |c| Ok(c.execute_batch(SCHEMA_V28)?)),
    // Which role/model a sub-agent ran on (SPEC §4.5).
    (29, |c| {
        add_column(c, "subagents", "model TEXT NOT NULL DEFAULT ''")
    }),
    // Per-task routing stats for roles (SPEC §4.6).
    (30, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_stats (
               id TEXT PRIMARY KEY, role TEXT NOT NULL, model TEXT NOT NULL,
               session_id TEXT, run_id TEXT, task_hash TEXT NOT NULL,
               ms INTEGER NOT NULL, tokens INTEGER NOT NULL, turns INTEGER NOT NULL,
               outcome TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_task_stats ON task_stats(role, model);",
        )?)
    }),
    // Agent changes to the harness, with what they replaced (SPEC §7.13).
    (31, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS harness_changes (
               id TEXT PRIMARY KEY, op TEXT NOT NULL, target TEXT NOT NULL,
               before TEXT NOT NULL, after TEXT NOT NULL, tool TEXT NOT NULL,
               reason TEXT NOT NULL, undone INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);",
        )?)
    }),
    // A working directory per Session (SPEC §5.1).
    (32, |c| add_column(c, "sessions", "cwd TEXT")),
    // Pinned Sessions (SPEC §10.4).
    (33, |c| {
        add_column(c, "sessions", "pinned INTEGER NOT NULL DEFAULT 0")
    }),
    // Archived Sessions (SPEC §10.4).
    (34, |c| {
        add_column(c, "sessions", "archived INTEGER NOT NULL DEFAULT 0")
    }), // Session tags, a JSON array (SPEC §10.4).
    (35, |c| {
        add_column(c, "sessions", "tags TEXT NOT NULL DEFAULT '[]'")
    }),
    // Per-Session standing instructions (SPEC §5.1).
    (36, |c| add_column(c, "sessions", "instructions TEXT")),
    // Session templates (SPEC §5.1).
    (37, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS session_templates (
               name TEXT PRIMARY KEY, model TEXT, reasoning_effort TEXT,
               instructions TEXT, cwd TEXT, first_message TEXT, created_at TEXT NOT NULL);",
        )?)
    }),
    // Model outcomes over time, for ranged Metrics (SPEC §10.4).
    (38, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS model_events (
               model TEXT NOT NULL, ok INTEGER NOT NULL, ms INTEGER NOT NULL, created_at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_model_events ON model_events(created_at);",
        )?)
    }),
    // Storage performance: the indexes the query plans asked for, and the
    // Session summary denormalised onto the Session row (SPEC §9.4).
    (39, migrate_v39),
    // A short human title for each Tool (SPEC §7.2).
    (40, |c| {
        add_column(c, "tools", "title TEXT NOT NULL DEFAULT ''")
    }),
    // Topic tags on each Tool, a JSON array (SPEC §7.2).
    (41, |c| {
        add_column(c, "tools", "tags TEXT NOT NULL DEFAULT '[]'")
    }),
    // Tools the user pinned for the agent (SPEC §7.1).
    (42, |c| {
        add_column(c, "tools", "user_pinned INTEGER NOT NULL DEFAULT 0")
    }),
    // Tools the user switched off without retiring them (SPEC §7.2).
    (43, |c| {
        add_column(c, "tools", "disabled INTEGER NOT NULL DEFAULT 0")
    }),
    // A Tool's own model use per Invocation: tokens, priced USD, and
    // whether any of it had no price (SPEC §10.4).
    (44, |c| {
        add_column(c, "invocations", "model_tokens INTEGER NOT NULL DEFAULT 0")?;
        add_column(c, "invocations", "model_cost REAL NOT NULL DEFAULT 0")?;
        add_column(
            c,
            "invocations",
            "model_unpriced INTEGER NOT NULL DEFAULT 0",
        )
    }),
    // Which client answered each stored approval (SPEC §10.3).
    (45, |c| {
        add_column(c, "approvals", "granted_by TEXT NOT NULL DEFAULT ''")
    }),
    // Provider-reported cost and cache writes per request (SPEC §10.6).
    (46, |c| {
        add_column(c, "run_usage", "cost REAL")?;
        add_column(
            c,
            "run_usage",
            "cache_write_tokens INTEGER NOT NULL DEFAULT 0",
        )
    }),
    // Capabilities the user always denies (SPEC §10.3).
    (47, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS denials (
               capability TEXT PRIMARY KEY, tool_name TEXT NOT NULL,
               denied_by TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL);",
        )?)
    }),
    // Jobs: overlap policy, delivery targets and a run history (SPEC §9.7).
    (48, |c| {
        add_column(c, "jobs", "overlap TEXT NOT NULL DEFAULT 'skip'")?;
        add_column(c, "jobs", "deliver TEXT")?;
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS job_runs (
               id TEXT PRIMARY KEY, job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
               session_id TEXT, trigger TEXT NOT NULL, status TEXT NOT NULL,
               detail TEXT NOT NULL DEFAULT '', cost REAL,
               started_at TEXT NOT NULL, finished_at TEXT);
             CREATE INDEX IF NOT EXISTS idx_job_runs ON job_runs(job_id, started_at);",
        )?)
    }),
    // Why a skill was retired; which memories a Run's prompt carried
    // (SPEC §7.10, §7.11).
    (49, |c| {
        add_column(c, "skills", "retire_reason TEXT")?;
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS run_memories (
               run_id TEXT NOT NULL, memory_id TEXT NOT NULL,
               PRIMARY KEY (run_id, memory_id));",
        )?)
    }),
    // The user's own verdict on a Run's answer (👍/👎, SPEC §8.7).
    (50, |c| {
        add_column(c, "runs", "rating TEXT NOT NULL DEFAULT ''")?;
        add_column(c, "runs", "rating_comment TEXT")
    }),
    // Files a Run's Tools changed, with what they held before (SPEC §5.7).
    (51, |c| {
        add_column(c, "sessions", "plan TEXT")?;
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS run_files (
               run_id TEXT NOT NULL, session_id TEXT NOT NULL, path TEXT NOT NULL,
               existed INTEGER NOT NULL, kept INTEGER NOT NULL, before BLOB,
               created_at TEXT NOT NULL,
               PRIMARY KEY (run_id, path));
             CREATE INDEX IF NOT EXISTS idx_run_files_session ON run_files(session_id, created_at);",
        )?)
    }),
    // Nested model spend (llm.complete, sub-agents) is tagged so it counts
    // toward usage and budgets but not the context estimate.
    (52, |c| {
        add_column(c, "run_usage", "source TEXT NOT NULL DEFAULT 'run'")
    }),
    // Attachments waiting for a Session's next message survive a restart (#857).
    (53, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS pending_attachments (
               session_id TEXT NOT NULL, att TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_pending_att_session ON pending_attachments(session_id);",
        )?)
    }),
    // Which provider served each request, for its own prices (#810); ''
    // for rows from before (priced as the default provider's).
    // Which provider served each request, for its own prices (#810; '' for
    // older rows, priced as the default's). The table loses its Session
    // foreign key: the daemon's own calls (compaction, the optimiser…)
    // often have no Session but still count (#806).
    (54, |c| {
        add_column(c, "run_usage", "provider TEXT NOT NULL DEFAULT ''")?;
        Ok(c.execute_batch(
            "CREATE TABLE run_usage_new (
               session_id TEXT NOT NULL, run_id TEXT NOT NULL, model TEXT NOT NULL,
               prompt_tokens INTEGER NOT NULL, completion_tokens INTEGER NOT NULL,
               reasoning_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
               created_at TEXT NOT NULL, cache_write_tokens INTEGER NOT NULL DEFAULT 0,
               cost REAL, source TEXT NOT NULL DEFAULT 'run', provider TEXT NOT NULL DEFAULT '');
             INSERT INTO run_usage_new(session_id, run_id, model, prompt_tokens, completion_tokens,
               reasoning_tokens, cached_tokens, created_at, cache_write_tokens, cost, source, provider)
               SELECT session_id, run_id, model, prompt_tokens, completion_tokens,
               reasoning_tokens, cached_tokens, created_at, cache_write_tokens, cost, source, provider
               FROM run_usage;
             DROP TABLE run_usage;
             ALTER TABLE run_usage_new RENAME TO run_usage;
             CREATE INDEX IF NOT EXISTS idx_run_usage_session ON run_usage(session_id, created_at);
             CREATE INDEX IF NOT EXISTS idx_run_usage_run ON run_usage(run_id);",
        )?)
    }),
    // A grant names the code it was given for (#704); older grants get
    // theirs the first time they are used.
    (55, |c| add_column(c, "approvals", "code_hash TEXT")),
    // Where an automatic name came from (#871): 'heuristic' names are
    // provisional and the naming model replaces them; NULL is the default
    // name, a user's name, or an earlier version's.
    (56, |c| add_column(c, "sessions", "name_source TEXT")),
    // MCP chain suggestions the agent or user dismissed (#771).
    (57, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE IF NOT EXISTS dismissed_mcp_chains (
               first TEXT NOT NULL, second TEXT NOT NULL, dismissed_at TEXT NOT NULL,
               PRIMARY KEY (first, second));",
        )?)
    }),
    (58, |c| {
        Ok(c.execute_batch(
            "CREATE TABLE token_calibration (
                provider_key TEXT NOT NULL, model TEXT NOT NULL,
                ratio REAL NOT NULL, samples INTEGER NOT NULL,
                PRIMARY KEY(provider_key, model));",
        )?)
    }),
    (59, |c| {
        c.execute_batch("CREATE TABLE run_forecasts (run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE, forecast TEXT NOT NULL); CREATE TRIGGER immutable_run_forecast BEFORE UPDATE ON run_forecasts BEGIN SELECT RAISE(ABORT, 'Run forecast is immutable'); END; CREATE TABLE forecast_request_counts(run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE, source TEXT NOT NULL, expected INTEGER NOT NULL, PRIMARY KEY(run_id,source));")?;
        add_column(c, "run_usage", "usage_complete INTEGER NOT NULL DEFAULT 0")
    }),
    (60, |c| {
        // Deliberately no parent foreign key: deleting a parent must preserve
        // both children and their historical parent identity.
        add_column(c, "sessions", "forked_from TEXT")?;
        add_column(c, "sessions", "fork_point_seq INTEGER CHECK((forked_from IS NULL AND fork_point_seq IS NULL) OR (forked_from IS NOT NULL AND fork_point_seq IS NOT NULL AND fork_point_seq>=0))")?;
        c.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_sessions_forked_from ON sessions(forked_from);",
        )?;
        Ok(())
    }),
    (61, |c| {
        // No backfill from timestamps or message sequence guesses. Deleted
        // messages must not cascade-delete the billing history of old Runs.
        add_column(c, "runs", "start_message_id TEXT")?;
        add_column(
            c,
            "runs",
            "start_message_seq INTEGER CHECK(start_message_seq IS NULL OR start_message_seq>=0)",
        )?;
        add_column(c, "runs", "end_message_id TEXT")?;
        add_column(
            c,
            "runs",
            "end_message_seq INTEGER CHECK(end_message_seq IS NULL OR end_message_seq>=0)",
        )
    }),
    (62, |c| {
        add_column(
            c,
            "run_usage",
            "cost_unknown INTEGER NOT NULL DEFAULT 0 CHECK(cost_unknown IN (0,1))",
        )
    }),
    (63, |c| add_column(c, "run_files", "before_metadata TEXT")),
    (64, |c| Ok(c.execute_batch(routing_storage::SCHEMA)?)),
    (65, |c| Ok(c.execute_batch(replay_storage::SCHEMA)?)),
    (66, |c| {
        add_column(c, "run_files", "before_kind TEXT")?;
        add_column(c, "run_files", "after_state TEXT")?;
        add_column(c, "run_files", "not_kept_reason TEXT")?;
        add_column(c, "runs", "untracked_writes_possible INTEGER NOT NULL DEFAULT 0 CHECK(untracked_writes_possible IN (0,1))")
    }),
    (67, |c| {
        add_column(c, "sessions", "plan_state TEXT")?;
        Ok(c.execute_batch(run_state_storage::SCHEMA)?)
    }),
    // Versions 68 and 69 were unpublished preview schemas.
    (70, |c| {
        add_column(c, "queued_messages", "position INTEGER")?;
        add_column(c, "queued_messages", "revision INTEGER NOT NULL DEFAULT 0")?;
        Ok(c.execute_batch("CREATE INDEX IF NOT EXISTS idx_queued_order ON queued_messages(session_id,COALESCE(position,id),id)")?)
    }),
    (71, migrate_v71),
    (72, topic_schema_v72::migrate),
    (73, |c| {
        add_column(c,"topics","allow_external_context INTEGER NOT NULL DEFAULT 0 CHECK(allow_external_context IN (0,1))")
    }),
    (74, |c| add_column(c, "messages", "generated_by TEXT")),
    (75, |c| {
        add_column(c, "topics", "permission_rules TEXT NOT NULL DEFAULT '[]'")
    }),
    (76, |c| {
        add_column(
            c,
            "topics",
            r#"limits TEXT NOT NULL DEFAULT '{"max_retries_per_step":3,"max_attempts_per_topic":20}'"#,
        )
    }),
];

/// Empty schema at a stamped historical version, for offline backup inspection.
/// Applies the same ordered migration prefix without opening/upgrading a backup.
pub(crate) fn schema_at(version: i64) -> Result<Connection> {
    if !(1..=SCHEMA_VERSION).contains(&version) || unsupported_preview_schema(version) {
        return Err(anyhow!("unsupported backup schema v{version}"));
    }
    let conn = Connection::open_in_memory()?;
    for (target, step) in MIGRATIONS {
        if target <= version {
            step(&conn)?;
        }
    }
    Ok(conn)
}

/// Bring the schema to [`SCHEMA_VERSION`] (SPEC §9.4). An existing
/// database is backed up first; each step runs in its own transaction
/// together with its version bump, so a failure leaves the database at
/// the last completed version.
fn migrate(conn: &Connection, file: Option<&Path>) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if unsupported_preview_schema(version) {
        return Err(anyhow!("database schema v{version} is an unsupported preview schema (this build writes v{SCHEMA_VERSION})"));
    }
    if version > SCHEMA_VERSION {
        return Err(anyhow!(
            "database schema v{version} is newer than this build (v{SCHEMA_VERSION}); upgrade formless"
        ));
    }
    let backup = if version > 0 && version < SCHEMA_VERSION {
        file.map(|f| backup_before_migration(conn, f, version))
            .transpose()?
    } else {
        None
    };
    let result: Result<()> = (|| {
        for (target, step) in MIGRATIONS {
            if version < target {
                let tx = conn.unchecked_transaction()?;
                step(&tx).with_context(|| format!("migrate database to v{target}"))?;
                tx.execute_batch(&format!("PRAGMA user_version={target};"))?;
                tx.commit()?;
            }
        }
        Ok(())
    })();
    if let (Some(file), Some(backup)) = (file, backup.as_ref()) {
        if result.is_err() {
            // Earlier steps may have committed. Remember this exact failed
            // state so the next attempt reuses the original recovery point.
            if let Err(error) = remember_migration_backup(conn, file, backup) {
                tracing::warn!(%error, "could not remember migration backup; it remains preserved");
            }
        } else {
            let _ = std::fs::remove_file(migration_receipt_path(file));
        }
    }
    result?;
    conn.execute_batch(DERIVED_INDEXES)
        .context("create derived database indexes")
}

/// Whether `e` says the database file itself is damaged (corrupt, or not a
/// database at all), and how.
fn corrupt_detail(e: &anyhow::Error) -> Option<String> {
    e.chain()
        .find_map(|c| match c.downcast_ref::<rusqlite::Error>()? {
            rusqlite::Error::SqliteFailure(f, msg)
                if matches!(
                    f.code,
                    rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                ) =>
            {
                Some(msg.clone().unwrap_or_else(|| f.to_string()))
            }
            _ => None,
        })
}

/// The newest backup of `file`: a pre-migration copy beside it or a daily
/// one in `backups/`.
pub fn newest_backup(file: &Path) -> Option<PathBuf> {
    let dir = file.parent()?;
    let name = file.file_name()?.to_string_lossy().into_owned();
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for d in [dir.to_path_buf(), dir.join("backups")] {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let ours = (n.starts_with(&name) && n.ends_with(".bak"))
                || (n.starts_with("formless-") && n.ends_with(".db"));
            if let (true, Ok(m)) = (ours, e.metadata().and_then(|m| m.modified())) {
                found.push((m, e.path()));
            }
        }
    }
    found.sort();
    found.pop().map(|(_, p)| p)
}

/// The error for a damaged database: what is wrong, that nothing was
/// replaced, and which backup to restore (§16).
fn damaged(file: &Path, detail: &str) -> anyhow::Error {
    let next = match newest_backup(file) {
        Some(b) => format!(
            "Stop the daemon and restore the newest backup: formless restore {}",
            b.display()
        ),
        None => {
            "No backup was found next to it; keep the file for recovery (sqlite3 .recover).".into()
        }
    };
    anyhow!(
        "the database {} is damaged ({detail}). Formless stopped and left it untouched. {next}",
        file.display()
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MigrationBackupReceipt {
    state_sha256: String,
    backup_name: String,
    backup_sha256: String,
}

fn migration_receipt_path(file: &Path) -> PathBuf {
    file.with_extension("migration-backup.json")
}

fn file_sha256(file: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut input = std::fs::File::open(file)?;
    let mut hash = Sha256::new();
    // Allocate directly on the heap; Box::new([0; N]) can still construct a
    // large temporary on the Windows main-thread stack in debug builds.
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// A checkpoint makes the source fingerprint independent of WAL teardown
/// when this connection closes. Busy readers disable reuse rather than
/// allowing a receipt to describe only part of the database.
fn migration_state_sha256(conn: &Connection, file: &Path) -> Result<Option<String>> {
    let before: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    if busy != 0 {
        return Ok(None);
    }
    let hash = file_sha256(file)?;
    let after: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
    // Another connection committing during the checkpoint/hash must not
    // make a receipt bless a fingerprint of only the main file.
    let wal = PathBuf::from(format!("{}-wal", file.display()));
    if before != after || std::fs::metadata(wal).is_ok_and(|m| m.len() != 0) {
        return Ok(None);
    }
    Ok(Some(hash))
}

fn remember_migration_backup(conn: &Connection, file: &Path, backup: &Path) -> Result<()> {
    let Some(state_sha256) = migration_state_sha256(conn, file)? else {
        return Ok(());
    };
    let receipt = MigrationBackupReceipt {
        state_sha256,
        backup_name: backup
            .file_name()
            .context("backup has no filename")?
            .to_str()
            .context("backup filename is not UTF-8")?
            .to_string(),
        backup_sha256: file_sha256(backup)?,
    };
    let destination = migration_receipt_path(file);
    let temporary = destination.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    use std::io::Write;
    let written = (|| -> Result<()> {
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        output.write_all(&serde_json::to_vec(&receipt)?)?;
        output.sync_all()?;
        std::fs::rename(&temporary, &destination)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

fn reusable_migration_backup(conn: &Connection, file: &Path) -> Option<PathBuf> {
    let receipt: MigrationBackupReceipt =
        serde_json::from_slice(&std::fs::read(migration_receipt_path(file)).ok()?).ok()?;
    let name = file.file_name()?.to_str()?;
    // A receipt can only name one of this database's adjacent snapshots.
    let filename = Path::new(&receipt.backup_name);
    if filename.file_name()?.to_str()? != receipt.backup_name
        || !receipt.backup_name.starts_with(&format!("{name}.v"))
        || !receipt.backup_name.ends_with(".bak")
    {
        return None;
    }
    let backup = file.with_file_name(&receipt.backup_name);
    if !std::fs::symlink_metadata(&backup)
        .ok()?
        .file_type()
        .is_file()
        || migration_state_sha256(conn, file).ok()?? != receipt.state_sha256
        || file_sha256(&backup).ok()? != receipt.backup_sha256
    {
        return None;
    }
    Some(backup)
}

/// Consistent copy (WAL included) next to the database, named after the
/// version it holds and the time it was taken.
fn backup_before_migration(conn: &Connection, file: &Path, version: i64) -> Result<PathBuf> {
    if let Some(backup) = reusable_migration_backup(conn, file) {
        return Ok(backup);
    }
    let stamp = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ"),
        uuid::Uuid::new_v4()
    );
    let name = format!(
        "{}.v{version}-{stamp}.bak",
        file.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("formless.db")
    );
    let dest = file.with_file_name(name);
    conn.execute("VACUUM INTO ?1", params![dest.to_string_lossy()])
        .with_context(|| format!("back up database to {}", dest.display()))?;
    // Record the initial state too, so an interrupted attempt before its
    // first committed step can reuse the snapshot on restart.
    if let Err(error) = remember_migration_backup(conn, file, &dest) {
        tracing::warn!(%error, "could not remember migration backup; it remains preserved");
    }
    Ok(dest)
}

/// Settings every connection gets. `synchronous=NORMAL` is SQLite's
/// recommendation under WAL: a commit survives an application crash (only a
/// power cut can lose the last few) and, unlike the `FULL` default, needs no
/// fsync — 0.10 ms per commit becomes 0.03 ms, and the daemon commits
/// several times per agent step. The rest are latency settings: a busy
/// timeout instead of an instant `SQLITE_BUSY` when another process (the
/// CLI, another connection of this daemon) holds the write lock, memory for temp
/// B-trees, a 32 MiB page cache, a 256 MiB read map, and a 64 MiB cap so a
/// long Run cannot leave an unbounded `-wal` file behind.
const PRAGMAS: &str = "\
PRAGMA journal_mode=WAL;\
PRAGMA synchronous=NORMAL;\
PRAGMA foreign_keys=ON;\
PRAGMA busy_timeout=5000;\
PRAGMA temp_store=MEMORY;\
PRAGMA cache_size=-32000;\
PRAGMA mmap_size=268435456;\
PRAGMA journal_size_limit=67108864;";

/// A read-only connection: the same memory and locking settings, minus the
/// ones that need write access.
const READER_PRAGMAS: &str = "\
PRAGMA query_only=ON;\
PRAGMA foreign_keys=ON;\
PRAGMA busy_timeout=5000;\
PRAGMA temp_store=MEMORY;\
PRAGMA cache_size=-32000;\
PRAGMA mmap_size=268435456;";

/// In-memory databases (tests) get the settings that do not involve a file.
const MEMORY_PRAGMAS: &str = "\
PRAGMA foreign_keys=ON;\
PRAGMA temp_store=MEMORY;\
PRAGMA cache_size=-32000;";

/// Keep the newest `retention` finished Invocations of a Tool. The old
/// `rowid NOT IN (… ORDER BY rowid DESC LIMIT n)` form made SQLite read the
/// Tool's whole index range on every call; finding the cutoff rowid once
/// and deleting the prefix below it touches the rows that go away only.
/// `running` rows are never pruned: the daemon needs them to mark a crashed
/// invocation interrupted.
fn prune_invocations(tx: &Connection, tool_id: &str, retention: usize) -> Result<()> {
    if retention == 0 {
        tx.execute(
            "DELETE FROM invocations WHERE tool_id=?1 AND status!='running'",
            params![tool_id],
        )?;
        return Ok(());
    }
    let cutoff: i64 = tx.query_row(
        "SELECT COALESCE(MIN(rowid),0) FROM (SELECT rowid FROM invocations \
         WHERE tool_id=?1 AND status!='running' ORDER BY rowid DESC LIMIT ?2)",
        params![tool_id, retention as i64],
        |r| r.get(0),
    )?;
    if cutoff > 0 {
        tx.execute(
            "DELETE FROM invocations WHERE tool_id=?1 AND status!='running' AND rowid < ?2",
            params![tool_id, cutoff],
        )?;
    }
    Ok(())
}

/// Prepared statements kept per connection: the agent loop and the TUI's
/// polling re-run the same few dozen statements, so re-parsing them on
/// every call is pure overhead.
const STMT_CACHE: usize = 128;

impl Drop for Store {
    /// `PRAGMA optimize` is SQLite's documented close-time hook: it runs
    /// ANALYZE only on tables whose query patterns changed since the last
    /// run, so the planner keeps good statistics without a full ANALYZE.
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("PRAGMA optimize;");
    }
}

impl Store {
    pub fn open(file: &Path) -> Result<Self> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // An existing file that isn't a SQLite database is reported, never
        // replaced by a fresh one (SPEC §16).
        if std::fs::metadata(file).is_ok_and(|m| m.len() > 0) {
            let mut head = [0u8; 16];
            let read = std::fs::File::open(file)
                .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head));
            if read.is_err() || &head != b"SQLite format 3\0" {
                return Err(damaged(file, "its header is not a SQLite database's"));
            }
        }
        let opened = (|| -> Result<Connection> {
            let conn =
                Connection::open(file).with_context(|| format!("open db {}", file.display()))?;
            conn.execute_batch(PRAGMAS)?;
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
                r.get::<_, i64>(0)
            })?;
            conn.set_prepared_statement_cache_capacity(STMT_CACHE);
            migrate(&conn, Some(file))?;
            Ok(conn)
        })();
        let conn = opened.map_err(|e| match corrupt_detail(&e) {
            Some(detail) => damaged(file, &detail),
            None => e,
        })?;
        Ok(Self {
            conn,
            rewrites: Default::default(),
            _forecast_snapshot: None,
            artifact_root: file.parent().map(|parent| parent.join("artifacts")),
            description_notifications: Default::default(),
        })
    }

    /// `PRAGMA quick_check`: every page and index readable. Run once when
    /// the daemon starts (§16); a failure is reported like an unreadable
    /// file.
    pub fn check_integrity(&self, file: &Path) -> Result<()> {
        let mut stmt = self.conn.prepare("PRAGMA quick_check")?;
        let found: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| damaged(file, &e.to_string()))?
            .collect::<rusqlite::Result<_>>()
            .map_err(|e| damaged(file, &e.to_string()))?;
        if found.len() == 1 && found[0] == "ok" {
            return Ok(());
        }
        let detail: Vec<&str> = found.iter().take(3).map(String::as_str).collect();
        Err(damaged(file, &detail.join("; ")))
    }

    /// Private snapshot reader; no SQLite connection ever opens the source.
    /// Busy/unsupported native locking fails closed instead of changing WAL/SHM.
    pub fn open_forecast_reader(file: &Path) -> Result<Self> {
        let absolute = std::path::absolute(file)?;
        let directory = absolute
            .parent()
            .context("forecast database has no parent")?
            .to_path_buf();
        let paths = crate::Paths {
            data_dir: directory.clone(),
            config_dir: directory,
            profile: "forecast-snapshot".into(),
        };
        // This compatibility wrapper protects the data tree. Callers with real
        // profile Paths can additionally freeze its separate configuration tree.
        forecast_snapshot::open_file(&paths, &absolute)?.context("forecast database does not exist")
    }

    /// None means genuinely absent: no profile/lock/directory is created.
    pub fn open_forecast_snapshot(paths: &crate::Paths) -> Result<Option<Self>> {
        forecast_snapshot::open(paths)
    }

    /// An extra connection for reads only (the database must already be
    /// migrated). WAL lets it read while the main connection writes.
    pub fn open_reader(file: &Path) -> Result<Self> {
        let conn = Connection::open(file).with_context(|| format!("open db {}", file.display()))?;
        conn.execute_batch(READER_PRAGMAS)?;
        conn.set_prepared_statement_cache_capacity(STMT_CACHE);
        Ok(Self {
            conn,
            rewrites: Default::default(),
            _forecast_snapshot: None,
            artifact_root: file.parent().map(|parent| parent.join("artifacts")),
            description_notifications: Default::default(),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(MEMORY_PRAGMAS)?;
        conn.set_prepared_statement_cache_capacity(STMT_CACHE);
        migrate(&conn, None)?;
        Ok(Self {
            conn,
            rewrites: Default::default(),
            _forecast_snapshot: None,
            artifact_root: None,
            description_notifications: Default::default(),
        })
    }

    /// `execute` through the connection's statement cache.
    fn ex(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
        Ok(self.conn.prepare_cached(sql)?.execute(params)?)
    }

    #[cfg(test)]
    pub(crate) fn inject_topic_human_answer_receipt_commit_failure_for_test(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE topic_answer_commit_parent (id TEXT PRIMARY KEY);
             CREATE TABLE topic_answer_commit_child (
                 parent_id TEXT NOT NULL REFERENCES topic_answer_commit_parent(id)
                     DEFERRABLE INITIALLY DEFERRED
             );
             CREATE TRIGGER topic_answer_commit_failure
             AFTER INSERT ON messages
             WHEN NEW.generated_by = 'topic_human_answer'
             BEGIN
                 INSERT INTO topic_answer_commit_child(parent_id) VALUES ('missing');
             END;",
        )?;
        Ok(())
    }

    /// `prepare` through the connection's statement cache.
    fn ps(&self, sql: &str) -> Result<rusqlite::CachedStatement<'_>> {
        Ok(self.conn.prepare_cached(sql)?)
    }

    fn row_session(r: &rusqlite::Row) -> rusqlite::Result<SessionInfo> {
        Ok(SessionInfo {
            id: r.get("id")?,
            forked_from: r.get("forked_from")?,
            fork_point_seq: r.get("fork_point_seq")?,
            name: r.get("name")?,
            model: r.get("model")?,
            reasoning_effort: r.get("reasoning_effort")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            cwd: r.get("cwd")?,
            // Only the list query adds these.
            message_count: r.get("message_count").unwrap_or(0),
            preview: r
                .get::<_, Option<String>>("preview")
                .ok()
                .flatten()
                .map(|p| {
                    let first = p.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
                    let mut out: String = first.trim().chars().take(80).collect();
                    if first.trim().chars().count() > 80 {
                        out.push('…');
                    }
                    out
                }),
            last_run_status: r.get("last_run_status").ok().flatten(),
            pinned: r.get::<_, i64>("pinned").unwrap_or(0) != 0,
            archived: r.get::<_, i64>("archived").unwrap_or(0) != 0,
            tags: r
                .get::<_, String>("tags")
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default(),
            instructions: r.get("instructions").ok().flatten(),
        })
    }
    fn row_msg(r: &rusqlite::Row) -> rusqlite::Result<MessageInfo> {
        Ok(MessageInfo {
            generated_by: r
                .as_ref()
                .column_index("generated_by")
                .ok()
                .map(|index| r.get::<_, Option<String>>(index))
                .transpose()?
                .flatten(),
            id: r.get("id")?,
            session_id: r.get("session_id")?,
            seq: r.get("seq")?,
            role: r.get("role")?,
            content: r.get("content")?,
            created_at: r.get("created_at")?,
            meta: r.get("meta").ok().flatten(),
        })
    }

    pub fn create_session(&self, name: Option<String>) -> Result<SessionInfo> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        let name = name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SESSION_NAME.into());
        self.ex(
            "INSERT INTO sessions(id,name,created_at,updated_at) VALUES(?1,?2,?3,?3)",
            params![id, name, t],
        )?;
        self.get_session(&id)?.context("session vanished")
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {SESSION_SUMMARY} FROM sessions s WHERE deleted_at IS NULL ORDER BY archived, pinned DESC, updated_at DESC"
            ))?;
        let rows = stmt
            .query_map([], Self::row_session)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<SessionInfo>> {
        let mut stmt = self.ps("SELECT * FROM sessions WHERE id=?1")?;
        let mut rows = stmt.query_map(params![id], Self::row_session)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn rename_session(&self, id: &str, name: &str) -> Result<Option<SessionInfo>> {
        self.ex(
            "UPDATE sessions SET name=?2, updated_at=?3, name_source=NULL WHERE id=?1",
            params![id, name, now()],
        )?;
        self.get_session(id)
    }

    /// Auto-naming: rename only while the Session still has the default
    /// name or a provisional (heuristic) one, so a user rename that raced
    /// the namer always wins and a model name is final. `source` is
    /// "heuristic" or "model" (#871). Leaves `updated_at` alone (naming is
    /// not activity). Returns the Session when the rename changed it.
    pub fn auto_name_session(
        &self,
        id: &str,
        name: &str,
        source: &str,
    ) -> Result<Option<SessionInfo>> {
        let n = self.ex(
            "UPDATE sessions SET name=?2, name_source=?4
             WHERE id=?1 AND (name=?3 OR name_source='heuristic')
               AND NOT (name=?2 AND name_source IS ?4)",
            params![id, name, DEFAULT_SESSION_NAME, source],
        )?;
        if n == 0 {
            return Ok(None);
        }
        self.get_session(id)
    }

    /// Whether the Session's name may still be set automatically: the
    /// default name, or a provisional heuristic one.
    pub fn name_is_provisional(&self, id: &str) -> Result<bool> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT name=?2 OR name_source IS 'heuristic' FROM sessions WHERE id=?1",
                params![id, DEFAULT_SESSION_NAME],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// Sessions with a provisional name, most recent first: what the naming
    /// model renames once it is installed (#871).
    pub fn provisionally_named_sessions(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.ps("SELECT id FROM sessions WHERE deleted_at IS NULL
             AND (name=?2 OR name_source='heuristic') ORDER BY updated_at DESC LIMIT ?1")?;
        let ids = stmt
            .query_map(params![limit as i64, DEFAULT_SESSION_NAME], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(ids)
    }

    /// Normalised tags: trimmed, lowercase, without `#`, unique, sorted.
    pub fn set_session_tags(&self, id: &str, tags: &[String]) -> Result<Option<SessionInfo>> {
        let mut clean: Vec<String> = tags
            .iter()
            .map(|t| t.trim().trim_start_matches('#').to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        clean.sort();
        clean.dedup();
        self.ex(
            "UPDATE sessions SET tags=?2 WHERE id=?1",
            params![id, serde_json::to_string(&clean)?],
        )?;
        self.get_session(id)
    }

    /// Exact role pins, including archived and soft-deleted Sessions which
    /// can still be restored. Do not use the paginated visible Session list.
    pub fn role_references(&self, role: &str) -> Result<Vec<String>> {
        let mut stmt = self.ps(
            "SELECT 'Session', id, COALESCE(NULLIF(name,''),id) FROM sessions WHERE model=?1
             UNION ALL SELECT 'template', name, name FROM session_templates WHERE model=?1
             ORDER BY 1,2",
        )?;
        let rows = stmt.query_map(params![format!("role:{role}")], |r| {
            let kind: String = r.get(0)?;
            let id: String = r.get(1)?;
            let label: String = r.get(2)?;
            Ok(if kind == "Session" {
                format!("Session {label:?} ({id})")
            } else {
                format!("template {label:?}")
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn save_template(&self, t: &TemplateInfo) -> Result<()> {
        self.ex(
            "INSERT OR REPLACE INTO session_templates
               (name, model, reasoning_effort, instructions, cwd, first_message, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                t.name,
                t.model,
                t.reasoning_effort,
                t.instructions,
                t.cwd,
                t.first_message,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn list_templates(&self) -> Result<Vec<TemplateInfo>> {
        let mut stmt = self.ps(
            "SELECT name, model, reasoning_effort, instructions, cwd, first_message
             FROM session_templates ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(TemplateInfo {
                name: r.get(0)?,
                model: r.get(1)?,
                reasoning_effort: r.get(2)?,
                instructions: r.get(3)?,
                cwd: r.get(4)?,
                first_message: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Whether a template of that name existed.
    pub fn delete_template(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM session_templates WHERE name=?1", params![name])?
            > 0)
    }

    /// Blank instructions clear them.
    pub fn set_session_instructions(
        &self,
        id: &str,
        instructions: Option<&str>,
    ) -> Result<Option<SessionInfo>> {
        let text = instructions.map(str::trim).filter(|t| !t.is_empty());
        self.ex(
            "UPDATE sessions SET instructions=?2 WHERE id=?1",
            params![id, text],
        )?;
        self.get_session(id)
    }

    /// Archiving doesn't count as activity either.
    pub fn set_session_archived(&self, id: &str, archived: bool) -> Result<Option<SessionInfo>> {
        self.ex(
            "UPDATE sessions SET archived=?2 WHERE id=?1",
            params![id, archived as i64],
        )?;
        self.get_session(id)
    }

    /// Pinning doesn't count as activity: `updated_at` stays.
    pub fn set_session_pinned(&self, id: &str, pinned: bool) -> Result<Option<SessionInfo>> {
        self.ex(
            "UPDATE sessions SET pinned=?2 WHERE id=?1",
            params![id, pinned as i64],
        )?;
        self.get_session(id)
    }

    pub fn set_session_cwd(&self, id: &str, cwd: Option<&str>) -> Result<Option<SessionInfo>> {
        self.ex(
            "UPDATE sessions SET cwd=?2, updated_at=?3 WHERE id=?1",
            params![id, cwd, now()],
        )?;
        self.get_session(id)
    }

    pub fn set_session_model(
        &self,
        id: &str,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<Option<SessionInfo>> {
        self.ex(
            "UPDATE sessions SET model=?2, reasoning_effort=?3, updated_at=?4 WHERE id=?1",
            params![id, model, effort, now()],
        )?;
        self.get_session(id)
    }

    fn note_rewrite(&self) {
        self.rewrites
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Hide a Session for its undo window (SPEC §5.1).
    pub fn soft_delete_session(&self, id: &str) -> Result<bool> {
        Ok(self.ex(
            "UPDATE sessions SET deleted_at=?2 WHERE id=?1 AND deleted_at IS NULL",
            params![id, now()],
        )? > 0)
    }

    /// Undo a soft delete still in its window.
    pub fn restore_session(&self, id: &str) -> Result<Option<SessionInfo>> {
        if self.ex(
            "UPDATE sessions SET deleted_at=NULL WHERE id=?1 AND deleted_at IS NOT NULL",
            params![id],
        )? == 0
        {
            return Ok(None);
        }
        self.get_session(id)
    }

    /// Whether a Session is soft-deleted (waiting out its undo window).
    pub fn session_deleted(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT deleted_at IS NOT NULL FROM sessions WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap_or(false))
    }

    /// Soft-deleted Sessions whose window ended before `cutoff` (RFC 3339).
    pub fn sessions_deleted_before(&self, cutoff: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM sessions WHERE deleted_at IS NOT NULL AND deleted_at <= ?1")?;
        let rows = stmt
            .query_map(params![cutoff], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub(crate) fn topic_session_family(&self, id: &str) -> Result<Vec<String>> {
        let descendants = self
            .conn
            .prepare(
                "WITH RECURSIVE family(id) AS (SELECT ?1 UNION
                 SELECT s.id FROM sessions s JOIN family f ON s.parent_session_id=f.id
                 WHERE s.session_kind='topic_attempt' OR EXISTS(
                   SELECT 1 FROM topics child JOIN topics parent ON child.parent_topic_id=parent.id
                   WHERE child.topic_session_id=s.id AND parent.topic_session_id=f.id))
                 SELECT id FROM family",
            )?
            .query_map([id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(descendants)
    }

    pub fn delete_session(&self, id: &str) -> Result<bool> {
        self.note_rewrite();
        self.transaction(|st| {
            let descendants=st.topic_session_family(id)?;
            let mut deleted=false;
            for session_id in descendants {
                st.ex("DELETE FROM topics WHERE source_session_id=?1 AND status IN ('pending_approval','rejected')",params![session_id])?;
                st.ex("DELETE FROM run_files WHERE session_id=?1 OR run_id IN (SELECT id FROM runs WHERE session_id=?1)", params![session_id])?;
                st.ex("DELETE FROM run_recaps WHERE session_id=?1 OR run_id IN (SELECT id FROM runs WHERE session_id=?1)",params![session_id])?;
                st.ex("DELETE FROM session_plan_revisions WHERE session_id=?1",params![session_id])?;
                let changed=st.ex("DELETE FROM sessions WHERE id=?1",params![session_id])?>0;
                deleted|=session_id==id && changed;
            }
            Ok(deleted)
        })
    }

    pub(crate) fn read_transaction<T>(&self, action: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        if !self.conn.is_autocommit() {
            return action(self);
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Deferred,
        )?;
        let result = action(self)?;
        tx.commit()?;
        Ok(result)
    }

    /// Validate and consume a Run's inputs under one SQLite write boundary.
    pub(crate) fn transaction<T>(&self, action: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        struct Guard<'a> {
            conn: &'a Connection,
            rollback: String,
            commit: String,
            done: bool,
        }
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                if !self.done {
                    let _ = self.conn.execute_batch(&self.rollback);
                }
            }
        }
        let outer = self.conn.is_autocommit();
        let name = format!("fl_{}", uuid::Uuid::new_v4().simple());
        let (begin, commit, rollback) = if outer {
            (
                "BEGIN IMMEDIATE".to_string(),
                "COMMIT".to_string(),
                "ROLLBACK".to_string(),
            )
        } else {
            (
                format!("SAVEPOINT {name}"),
                format!("RELEASE {name}"),
                format!("ROLLBACK TO {name}; RELEASE {name}"),
            )
        };
        self.conn.execute_batch(&begin)?;
        let mut guard = Guard {
            conn: &self.conn,
            rollback,
            commit,
            done: false,
        };
        let result = action(self)?;
        self.conn.execute_batch(&guard.commit)?;
        guard.done = true;
        Ok(result)
    }
    pub(crate) fn preview_copy(&self) -> Result<Self> {
        let mut copy = Self::open_in_memory()?;
        copy.artifact_root = self.artifact_root.clone();
        {
            let backup = rusqlite::backup::Backup::new(&self.conn, &mut copy.conn)?;
            backup.run_to_completion(128, std::time::Duration::ZERO, None)?;
        }
        Ok(copy)
    }
    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
    ) -> Result<MessageInfo> {
        self.append_message_meta(session_id, role, content, None)
    }

    pub fn append_message_meta(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        meta: Option<String>,
    ) -> Result<MessageInfo> {
        self.append_message_generated(session_id, role, content, meta, None)
    }
    pub(crate) fn append_message_generated(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        meta: Option<String>,
        generated_by: Option<&str>,
    ) -> Result<MessageInfo> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        // One write transaction, taken up front (IMMEDIATE): the next seq is
        // computed inside the INSERT itself, so two writers can never pick
        // the same one, and the session's timestamp moves with the message.
        self.transaction(|st| {
        let tx=&st.conn;
        let seq: i64 = tx
            .prepare_cached(
                "INSERT INTO messages(id,session_id,seq,role,content,created_at,meta,generated_by) \
                 SELECT ?1, ?2, COALESCE(MAX(seq),0)+1, ?3, ?4, ?5, ?6, ?7 FROM messages WHERE session_id=?2 \
                 RETURNING seq",
            )?
            .query_row(params![id, session_id, role, content, t, meta,generated_by], |r| r.get(0))?;
        tx.prepare_cached("UPDATE sessions SET updated_at=?2 WHERE id=?1")?
            .execute(params![session_id, t])?;
        Ok(MessageInfo {
            generated_by:generated_by.map(str::to_owned),
            id,
            session_id: session_id.into(),
            seq,
            role: role.into(),
            content: content.into(),
            created_at: t,
            meta,
        })
        })
    }

    pub fn list_messages(&self, session_id: &str) -> Result<Vec<MessageInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM messages WHERE session_id=?1 ORDER BY seq")?;
        let rows = stmt
            .query_map(params![session_id], Self::row_msg)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Newest transcript sequence without decoding any message bodies.
    pub(crate) fn last_message_seq(&self, session_id: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM messages WHERE session_id=?1",
            params![session_id],
            |row| row.get(0),
        )?)
    }

    /// The same newest user row as reversing a Session transcript, including
    /// empty content. Compare and retry need its identity, sequence, and text.
    pub(crate) fn last_user_message(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, i64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, seq, content FROM messages WHERE session_id=?1 AND role='user'
             ORDER BY seq DESC, rowid DESC LIMIT 1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    /// Suggest-name prompt: the latest four nonblank user rows, in transcript
    /// order. Step through a single SQL snapshot rather than materializing
    /// unrelated messages or retaining unbounded user text.
    pub(crate) fn recent_user_naming_input(&self, session_id: &str) -> Result<String> {
        const LIMIT: usize = 4;
        const MAX_CHARS: usize = 2000;
        let mut stmt = self.ps(
            "SELECT content FROM messages WHERE session_id=?1 AND role='user'
             ORDER BY seq DESC, rowid DESC",
        )?;
        let mut rows = stmt.query(params![session_id])?;
        let mut recent = Vec::with_capacity(LIMIT);
        while recent.len() < LIMIT {
            let Some(row) = rows.next()? else { break };
            let text = row.get_ref(0)?.as_str()?;
            if !text.trim().is_empty() {
                recent.push(text.chars().take(MAX_CHARS).collect::<String>());
            }
        }
        recent.reverse();
        Ok(recent.join("\n").chars().take(MAX_CHARS).collect())
    }

    /// One Session-scoped message, using the same row/metadata conversion as
    /// transcript reads. Unrelated rows are deliberately not decoded here.
    pub fn get_message(&self, session_id: &str, message_id: &str) -> Result<Option<MessageInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM messages WHERE session_id=?1 AND id=?2",
                params![session_id, message_id],
                Self::row_msg,
            )
            .optional()?)
    }

    pub(crate) fn topic_suggestion_source(
        &self,
        session_id: &str,
    ) -> Result<Option<crate::main_topic_suggestions::SuggestionSource>> {
        use crate::main_topic_suggestions::SuggestionSource;
        let active: Option<(String, Option<String>)> = self
            .ps("SELECT session_kind, topic_id FROM sessions WHERE id=?1 AND deleted_at IS NULL")?
            .query_row(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match active {
            Some((kind, Some(id))) if kind == "topic" => Ok(Some(SuggestionSource::Topic(id))),
            Some((kind, _)) if kind == "main" && self.is_main_chat(session_id)? => {
                Ok(Some(SuggestionSource::Main))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn recent_topic_messages(&self, session_id: &str) -> Result<Vec<Value>> {
        let mut stmt = self.ps(
            "SELECT id, seq, CASE WHEN length(CAST(content AS BLOB)) <= 4096 THEN content END
             FROM messages WHERE session_id=?1 AND role='user' AND generated_by IS NULL
             ORDER BY seq DESC LIMIT 8",
        )?;
        let mut rows = stmt
            .query_map(params![session_id], |row| {
                let content = row.get::<_, Option<String>>(2)?;
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "seq": row.get::<_, i64>(1)?,
                    "content_omitted": content.is_none(),
                    "content": content,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Fork source prefix, decoded within the existing fork transaction.
    fn list_messages_up_to(&self, session_id: &str, limit: i64) -> Result<Vec<MessageInfo>> {
        let mut statement = self
            .conn
            .prepare("SELECT * FROM messages WHERE session_id=?1 AND seq<=?2 ORDER BY seq")?;
        let rows = statement
            .query_map(params![session_id, limit], Self::row_msg)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Messages with `seq > after`, in order (incremental context builds).
    pub fn list_messages_after(&self, session_id: &str, after: i64) -> Result<Vec<MessageInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM messages WHERE session_id=?1 AND seq>?2 ORDER BY seq")?;
        let rows = stmt
            .query_map(params![session_id, after], Self::row_msg)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Total characters of a Session's message contents, without loading
    /// them (routing's task-size signal).
    pub fn session_content_chars(&self, session_id: &str) -> Result<usize> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(content)),0) FROM messages WHERE session_id=?1",
            params![session_id],
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Sum of top-level `attachments` array lengths for one Session.
    ///
    /// Selects only `meta` and steps one row at a time, so context usage
    /// does not materialize transcript content. Unreadable metadata and
    /// malformed JSON add nothing, matching `row_msg`. Any role counts.
    pub(crate) fn attachment_entry_count(&self, session_id: &str) -> Result<usize> {
        let mut statement =
            self.ps("SELECT meta FROM messages WHERE session_id=?1 AND meta IS NOT NULL")?;
        let mut rows = statement.query(params![session_id])?;
        let mut total = 0usize;
        while let Some(row) = rows.next()? {
            // Same conversion as `row_msg`: unreadable metadata is absence.
            let meta: Option<String> = row.get("meta").ok().flatten();
            if let Some(meta) = meta {
                if let Ok(value) = serde_json::from_str::<Value>(&meta) {
                    total += value["attachments"].as_array().map_or(0, Vec::len);
                }
            }
        }
        Ok(total)
    }

    /// Auto-naming text from user `content` in `seq` order. The scan ends at
    /// the first nontrivial row and keeps at most the first nonempty trimmed
    /// fallback plus that row.
    pub(crate) fn user_naming_text(&self, session_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT content FROM messages WHERE session_id=?1 AND role='user' ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        crate::namer::try_naming_text_from_contents(rows).map_err(Into::into)
    }

    pub fn list_checkpoints(&self, session_id: &str) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.ps(
            "SELECT summary, up_to_seq FROM checkpoints WHERE session_id=?1 ORDER BY up_to_seq",
        )?;
        let rows = stmt
            .query_map(params![session_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn import_session(
        &self,
        name: &str,
        model: Option<String>,
        reasoning_effort: Option<String>,
        messages: &[(String, String, Option<String>)],
        checkpoints: &[(String, i64)],
    ) -> Result<SessionInfo> {
        let mut s = self.create_session(Some(name.to_string()))?;
        if model.is_some() || reasoning_effort.is_some() {
            s = self
                .set_session_model(&s.id, model, reasoning_effort)?
                .unwrap_or(s);
        }
        for (role, content, meta) in messages {
            self.append_message_meta(
                &s.id,
                role,
                content,
                sanitize_imported_message_meta(meta.as_deref()),
            )?;
        }
        for (summary, up_to) in checkpoints {
            self.save_checkpoint(&s.id, summary, *up_to)?;
        }
        self.get_session(&s.id).map(|o| o.unwrap_or(s))
    }

    /// Fork a session: copy its messages (optionally only up to `up_to_seq`)
    /// and checkpoints into a new independent session.
    pub fn fork_session(
        &self,
        id: &str,
        name: Option<String>,
        up_to_seq: Option<i64>,
    ) -> Result<Option<SessionInfo>> {
        self.ensure_topic_chat_writable(id)?;
        self.transaction(|st| st.fork_session_locked(id, name, up_to_seq))
    }

    fn fork_session_locked(
        &self,
        id: &str,
        name: Option<String>,
        up_to_seq: Option<i64>,
    ) -> Result<Option<SessionInfo>> {
        let Some(orig) = self.get_session(id)? else {
            return Ok(None);
        };
        if self.session_deleted(id)? {
            return Ok(None);
        }
        // Decode only the requested prefix, inside this same transaction.
        // Conversion errors in excluded later rows no longer abort a bounded
        // fork; included rows still use the ordinary row_msg contract.
        let messages = match up_to_seq {
            Some(limit) => self.list_messages_up_to(id, limit)?,
            None => self.list_messages(id)?,
        };
        let point = messages.last().map(|message| message.seq).unwrap_or(0);
        let checkpoints: Vec<(String, String, i64)> = self.ps(
            "SELECT id,summary,up_to_seq FROM checkpoints WHERE session_id=?1 AND up_to_seq<=?2 ORDER BY up_to_seq,rowid"
        )?.query_map(params![id,point], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let name = name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{} (fork)", orig.name));
        let fork = self.create_session(Some(name))?;
        self.set_session_model(&fork.id, orig.model, orig.reasoning_effort)?;
        for message in messages {
            self.ex("INSERT INTO messages(id,session_id,seq,role,content,created_at,meta,generated_by) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)", params![uuid::Uuid::new_v4().to_string(),fork.id,message.seq,message.role,message.content,message.created_at,message.meta,message.generated_by])?;
        }
        let complete = up_to_seq.is_none()
            || !self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND seq>?2)",
                params![id, point],
                |r| r.get::<_, bool>(0),
            )?;
        self.copy_plan_at(id, &fork.id, point, complete)?;
        for (source_id, summary, up_to) in checkpoints {
            let copied_id = self.save_checkpoint(&fork.id, &summary, up_to)?;
            // The summary's evidence can outlive the original message rows.
            // Rebind it to this independent checkpoint ID, not the parent's
            // session/run key, and never pull evidence from a later cut.
            for origin in self.data_origins(id, &format!("checkpoint:{source_id}"))? {
                self.record_data_origin(&fork.id, &format!("checkpoint:{copied_id}"), &origin)?;
            }
        }
        self.ex(
            "UPDATE sessions SET forked_from=?2,fork_point_seq=?3,updated_at=?4 WHERE id=?1",
            params![fork.id, id, point, now()],
        )?;
        // The fork works in the same directory, to the same instructions.
        if orig.instructions.is_some() {
            self.set_session_instructions(&fork.id, orig.instructions.as_deref())?;
        }
        match orig.cwd.as_deref() {
            Some(cwd) => self.set_session_cwd(&fork.id, Some(cwd)),
            None => self.get_session(&fork.id),
        }
    }

    /// Case-insensitive substring search across all sessions' messages.
    pub fn search_sessions(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 200) as i64;
        // FTS5 (bm25, then newest): every word as a prefix term. A query
        // with no word characters falls back to a substring scan.
        let fts = fts_query(query);
        let phrase = identifier_prefix_phrase(query);
        let sql = if phrase.is_some() {
            "SELECT m.session_id, s.name, m.id, m.seq, m.role, m.content, m.created_at \
             FROM messages_fts f JOIN messages m ON m.rowid = f.rowid \
             JOIN sessions s ON s.id = m.session_id AND s.deleted_at IS NULL \
             WHERE messages_fts MATCH ?1 \
             ORDER BY CASE WHEN m.rowid IN \
               (SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?3) \
               THEN 0 ELSE 1 END, bm25(messages_fts), m.created_at DESC, m.rowid DESC LIMIT ?2"
        } else if fts.is_some() {
            "SELECT m.session_id, s.name, m.id, m.seq, m.role, m.content, m.created_at \
             FROM messages_fts f JOIN messages m ON m.rowid = f.rowid \
             JOIN sessions s ON s.id = m.session_id AND s.deleted_at IS NULL \
             WHERE messages_fts MATCH ?1 \
             ORDER BY bm25(messages_fts), m.created_at DESC, m.rowid DESC LIMIT ?2"
        } else {
            "SELECT m.session_id, s.name, m.id, m.seq, m.role, m.content, m.created_at \
             FROM messages m JOIN sessions s ON s.id = m.session_id AND s.deleted_at IS NULL \
             WHERE instr(lower(m.content), lower(?1)) > 0 \
             ORDER BY m.created_at DESC, m.rowid DESC LIMIT ?2"
        };
        let mut stmt = self.ps(sql)?;
        let arg = fts.unwrap_or_else(|| query.to_string());
        let mut args = vec![rusqlite::types::Value::from(arg), limit.into()];
        if let Some(phrase) = phrase {
            args.push(phrase.into());
        }
        let mut rows = stmt.query(rusqlite::params_from_iter(args))?;
        let needle = search_words(query).next().unwrap_or(query).to_lowercase();
        let mut terms = HashMap::new();
        for word in search_words(query) {
            let next = terms.len();
            terms
                .entry(crate::fuzzy::fold_chars(word).collect())
                .or_insert(next);
        }
        let mut hits = Vec::new();
        while let Some(row) = rows.next()? {
            let content = row.get_ref(5)?.as_str()?;
            hits.push(SearchHit {
                session_id: row.get(0)?,
                session_name: row.get(1)?,
                message_id: row.get(2)?,
                seq: row.get(3)?,
                role: row.get(4)?,
                snippet: if terms.len() > 1 {
                    multi_term_snippet(content, &terms)
                } else {
                    make_snippet(content, &needle)
                },
                created_at: row.get(6)?,
            });
        }
        Ok(hits)
    }

    /// Edit a user message in place and delete everything after it
    /// (later messages and checkpoints that covered them), for edit-and-retry.
    /// Returns the edited message, or `None` if not found / not a user message.
    pub fn edit_message(
        &self,
        session_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<Option<MessageInfo>> {
        self.ensure_topic_chat_writable(session_id)?;
        let found = self.get_message(session_id, message_id)?;
        let Some(mut msg) = found else {
            return Ok(None);
        };
        if msg.role != "user" {
            return Ok(None);
        }
        self.note_rewrite();
        // One edit is one commit: four separate ones cost four fsyncs (or
        // four WAL commits) for a single user action.
        self.transaction(|st| {
            let tx = &st.conn;
            tx.execute(
                "UPDATE messages SET content=?3 WHERE session_id=?1 AND id=?2",
                params![session_id, message_id, content],
            )?;
            tx.execute(
                "DELETE FROM messages WHERE session_id=?1 AND seq>?2",
                params![session_id, msg.seq],
            )?;
            tx.execute(
                "DELETE FROM checkpoints WHERE session_id=?1 AND up_to_seq>=?2",
                params![session_id, msg.seq],
            )?;
            tx.execute(
                "UPDATE sessions SET updated_at=?2 WHERE id=?1",
                params![session_id, now()],
            )?;
            st.truncate_plan_revisions(session_id, msg.seq)?;
            msg.content = content.to_string();
            Ok(Some(msg))
        })
    }

    pub fn create_run(&self, session_id: &str) -> Result<String> {
        self.transaction(|st|{
        let topic:bool=st.conn.query_row("SELECT EXISTS(SELECT 1 FROM topics WHERE topic_session_id=?1)",[session_id],|row|row.get(0))?;
        if topic {st.abandon_resumable_runs(session_id)?;}
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        st.ex(
            "INSERT INTO runs(id,session_id,status,created_at,updated_at,start_message_id,start_message_seq) VALUES(?1,?2,'running',?3,?3,(SELECT id FROM messages WHERE session_id=?2 ORDER BY seq DESC,rowid DESC LIMIT 1),(SELECT COALESCE(MAX(seq),0) FROM messages WHERE session_id=?2))",
            params![id, session_id, t],
        )?;
        Ok(id)
        })
    }
    pub(crate) fn abandon_resumable_runs(&self, session: &str) -> Result<usize> {
        Ok(self.conn.execute("UPDATE runs SET status='abandoned',updated_at=?2 WHERE session_id=?1 AND status IN ('paused','interrupted')",params![session,now()])?)
    }
    pub fn run_status(&self, run_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT status FROM runs WHERE id=?1",
                params![run_id],
                |r| r.get::<_, String>(0),
            )
            .ok())
    }
    /// Record a built-in call: outcome and a short kind, never its input
    /// or output.
    pub fn record_builtin_call(
        &self,
        name: &str,
        session_id: &str,
        run_id: &str,
        ok: bool,
        kind: &str,
        duration_ms: i64,
    ) -> Result<()> {
        self.ex(
            "INSERT INTO builtin_calls(name,session_id,run_id,ok,kind,duration_ms,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![name, session_id, run_id, ok, kind, duration_ms, now()],
        )?;
        Ok(())
    }

    /// Whether this Run already had a `find_tools` call that found nothing.
    pub fn run_had_empty_find(&self, run_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM builtin_calls WHERE run_id=?1 AND name='find_tools' AND kind='empty')",
            params![run_id],
            |r| r.get(0),
        )?)
    }

    /// Per built-in: calls, failures, and counts per kind.
    pub fn builtin_stats(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<formless_protocol::BuiltinStatInfo>> {
        let mut stmt = self.ps(
            "SELECT name, kind, COUNT(*), SUM(ok=0), AVG(duration_ms) FROM builtin_calls
             WHERE ?1 IS NULL OR created_at >= ?1 GROUP BY name, kind ORDER BY name, kind",
        )?;
        let mut out: Vec<formless_protocol::BuiltinStatInfo> = Vec::new();
        let rows = stmt.query_map(params![since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })?;
        for row in rows {
            let (name, kind, calls, failures, avg_ms) = row?;
            let s = match out.last_mut() {
                Some(s) if s.name == name => s,
                _ => {
                    out.push(formless_protocol::BuiltinStatInfo {
                        name: name.clone(),
                        ..Default::default()
                    });
                    out.last_mut().expect("just pushed")
                }
            };
            s.total_ms += avg_ms * calls as f64;
            s.calls += calls;
            s.failures += failures;
            if !kind.is_empty() {
                s.kinds.insert(kind, calls);
            }
        }
        for s in &mut out {
            s.avg_ms = if s.calls > 0 {
                s.total_ms / s.calls as f64
            } else {
                0.0
            };
        }
        Ok(out)
    }

    /// Split a step's tokens evenly over the tool calls it made: each
    /// top-level Invocation the Run started since `since` gets one share
    /// (built-in calls absorb theirs).
    pub fn attribute_step_tokens(
        &self,
        run_id: &str,
        since: &str,
        tokens: i64,
        calls: usize,
    ) -> Result<()> {
        if tokens <= 0 || calls == 0 {
            return Ok(());
        }
        let share = tokens / calls as i64;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE invocations SET tokens=tokens+?3 WHERE run_id=?1 AND parent_id IS NULL AND created_at>=?2",
            params![run_id, since, share],
        )?;
        // A run_script call's share is split over the Tools it called.
        tx.execute(
            "UPDATE invocations SET tokens=tokens+?3/(SELECT count(*) FROM invocations s WHERE s.parent_id=invocations.parent_id) \
             WHERE run_id=?1 AND parent_id LIKE 'script:%' AND created_at>=?2",
            params![run_id, since, share],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record one finished `run_script` (SPEC §7.12).
    #[allow(clippy::too_many_arguments)]
    pub fn record_script(
        &self,
        id: &str,
        session_id: &str,
        run_id: &str,
        hash: &str,
        source: &str,
        uses: &[String],
        status: &str,
        error: &str,
        duration_ms: i64,
        saved_tool: &str,
    ) -> Result<()> {
        let changed = self.ex(
            "INSERT INTO scripts(id,session_id,run_id,hash,source,uses,status,error,duration_ms,saved_tool,created_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(id) DO UPDATE SET status=excluded.status,error=excluded.error,duration_ms=excluded.duration_ms,saved_tool=excluded.saved_tool
             WHERE scripts.session_id IS excluded.session_id AND scripts.run_id IS excluded.run_id
             AND scripts.hash=excluded.hash AND scripts.source=excluded.source AND scripts.uses=excluded.uses
             AND scripts.status='running' AND excluded.status<>'running'",
            params![
                id,
                session_id,
                run_id,
                hash,
                source,
                serde_json::to_string(uses)?,
                status,
                error,
                duration_ms,
                saved_tool,
                now()
            ],
        )?;
        if changed != 1 {
            return Err(anyhow::anyhow!(
                "Script completion does not match its original execution identity"
            ));
        }
        Ok(())
    }

    /// Distinct Sessions that ran a script with this hash successfully.
    pub fn script_sessions(&self, hash: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT count(DISTINCT session_id) FROM scripts WHERE hash=?1 AND status='success'",
            params![hash],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    /// Scripts run successfully in at least `threshold` Sessions and never
    /// saved as a Tool, most widespread first: (sessions, uses, latest source).
    pub fn recurring_scripts(
        &self,
        threshold: usize,
        limit: usize,
    ) -> Result<Vec<(usize, Vec<String>, String)>> {
        let mut stmt = self.ps("SELECT eligible.n, latest.uses, latest.source \
             FROM (SELECT hash, count(DISTINCT session_id) AS n \
                   FROM scripts WHERE status='success' GROUP BY hash \
                   HAVING n>=?1 AND max(saved_tool)='' ORDER BY n DESC LIMIT ?2) eligible \
             JOIN scripts latest ON latest.rowid=( \
                 SELECT rowid FROM scripts candidate WHERE candidate.hash=eligible.hash \
                 ORDER BY created_at DESC, rowid DESC LIMIT 1) \
             ORDER BY eligible.n DESC")?;
        let rows = stmt.query_map(params![threshold as i64, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)? as usize,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (n, uses, source) = row?;
            out.push((n, serde_json::from_str(&uses).unwrap_or_default(), source));
        }
        Ok(out)
    }

    /// Tokens a Tool spent itself (`llm.complete` inside it).
    /// Charge an Invocation for its Tool's own model use (`llm`, `agent`):
    /// tokens, and USD when the model is priced.
    pub fn add_invocation_model_use(
        &self,
        inv_id: &str,
        tokens: i64,
        cost: Option<f64>,
    ) -> Result<()> {
        self.ex(
            "UPDATE invocations SET model_tokens=model_tokens+?2, model_cost=model_cost+COALESCE(?3,0), \
             model_unpriced=MAX(model_unpriced, ?3 IS NULL) WHERE id=?1",
            params![inv_id, tokens, cost],
        )?;
        Ok(())
    }

    pub fn add_invocation_tokens(&self, inv_id: &str, tokens: i64) -> Result<()> {
        self.ex(
            "UPDATE invocations SET tokens=tokens+?2 WHERE id=?1",
            params![inv_id, tokens],
        )?;
        Ok(())
    }

    fn row_job(r: &rusqlite::Row) -> rusqlite::Result<formless_protocol::JobInfo> {
        Ok(formless_protocol::JobInfo {
            id: r.get("id")?,
            prompt: r.get("prompt")?,
            session_id: r.get("session_id")?,
            cron: r.get("cron")?,
            next_run: r.get("next_run")?,
            last_run: r.get("last_run")?,
            last_status: r.get("last_status")?,
            enabled: r.get::<_, i64>("enabled")? != 0,
            approved: r.get::<_, i64>("approved")? != 0,
            author: r.get("author")?,
            created_at: r.get("created_at")?,
            overlap: r.get("overlap")?,
            deliver: r
                .get::<_, Option<String>>("deliver")?
                .and_then(|d| serde_json::from_str(&d).ok()),
        })
    }

    /// A job's overlap policy and delivery target (SPEC §9.7).
    pub fn set_job_options(
        &self,
        id: &str,
        overlap: Option<&str>,
        deliver: Option<&serde_json::Value>,
    ) -> Result<()> {
        if let Some(o) = overlap {
            self.ex("UPDATE jobs SET overlap=?2 WHERE id=?1", params![id, o])?;
        }
        if let Some(d) = deliver {
            // `{}` (or null) clears it.
            let empty = d.is_null() || d.as_object().is_some_and(|o| o.is_empty());
            let d = (!empty).then(|| d.to_string());
            self.ex("UPDATE jobs SET deliver=?2 WHERE id=?1", params![id, d])?;
        }
        Ok(())
    }

    /// Start a row of a job's history; returns its id.
    pub fn job_run_start(
        &self,
        job_id: &str,
        session_id: Option<&str>,
        trigger: &str,
        status: &str,
        detail: &str,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let finished = (status != "running").then(now);
        self.ex(
            "INSERT INTO job_runs(id,job_id,session_id,trigger,status,detail,started_at,finished_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, job_id, session_id, trigger, status, detail, now(), finished],
        )?;
        Ok(id)
    }

    pub fn job_run_finish(
        &self,
        id: &str,
        status: &str,
        detail: &str,
        cost: Option<f64>,
    ) -> Result<()> {
        self.ex(
            "UPDATE job_runs SET status=?2, detail=CASE WHEN ?3='' THEN detail ELSE ?3 END, cost=?4, finished_at=?5 WHERE id=?1",
            params![id, status, detail, cost, now()],
        )?;
        Ok(())
    }

    /// A job's latest runs, newest first.
    pub fn job_runs(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<formless_protocol::JobRunInfo>> {
        let mut stmt = self.ps(
            "SELECT id, job_id, session_id, trigger, status, detail, cost, started_at, finished_at
             FROM job_runs WHERE job_id=?1 ORDER BY started_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![job_id, limit as i64], |r| {
                Ok(formless_protocol::JobRunInfo {
                    id: r.get(0)?,
                    job_id: r.get(1)?,
                    session_id: r.get(2)?,
                    trigger: r.get(3)?,
                    status: r.get(4)?,
                    detail: r.get(5)?,
                    cost: r.get(6)?,
                    started_at: r.get(7)?,
                    finished_at: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// How many messages wait in a Session's queue.
    pub fn queued_count(&self, session_id: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM queued_messages WHERE session_id=?1",
            params![session_id],
            |r| r.get::<_, i64>(0),
        )? as usize)
    }

    pub fn create_job(
        &self,
        prompt: &str,
        session_id: Option<&str>,
        cron: Option<&str>,
        next_run: &str,
        approved: bool,
        author: &str,
    ) -> Result<formless_protocol::JobInfo> {
        let created = now();
        let id = insert_short_id('j', "job", uuid::Uuid::new_v4, |id| {
            self.ex(
                "INSERT INTO jobs(id,prompt,session_id,cron,next_run,enabled,approved,author,created_at) VALUES(?1,?2,?3,?4,?5,?6,?6,?7,?8)
                 ON CONFLICT(id) DO NOTHING",
                params![id, prompt, session_id, cron, next_run, approved, author, created],
            )
        })?;
        self.get_job(&id)?.context("job vanished")
    }

    pub fn get_job(&self, id: &str) -> Result<Option<formless_protocol::JobInfo>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM jobs WHERE id=?1", params![id], Self::row_job)
            .ok())
    }

    pub fn list_jobs(&self) -> Result<Vec<formless_protocol::JobInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM jobs ORDER BY created_at, rowid")?;
        let rows = stmt
            .query_map([], Self::row_job)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Enabled, approved jobs whose time has come.
    pub fn due_jobs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<formless_protocol::JobInfo>> {
        Ok(self
            .list_jobs()?
            .into_iter()
            .filter(|j| j.enabled && j.approved)
            .filter(|j| {
                j.next_run
                    .as_deref()
                    .and_then(|n| chrono::DateTime::parse_from_rfc3339(n).ok())
                    .is_some_and(|n| n <= now)
            })
            .collect())
    }

    /// The user enables (which also approves) or disables a job.
    pub fn set_job_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        self.set_job_enabled_with_clock(id, enabled, chrono::Local::now)
    }

    #[cfg(test)]
    fn set_job_enabled_at(
        &self,
        id: &str,
        enabled: bool,
        now: chrono::DateTime<chrono::Local>,
    ) -> Result<bool> {
        self.set_job_enabled_with_clock(id, enabled, || now)
    }

    fn set_job_enabled_with_clock(
        &self,
        id: &str,
        enabled: bool,
        clock: impl FnOnce() -> chrono::DateTime<chrono::Local>,
    ) -> Result<bool> {
        self.transaction(|st| {
            let Some(job) = st.get_job(id)? else { return Ok(false); };
            if !enabled {
                return Ok(st.ex("UPDATE jobs SET enabled=0 WHERE id=?1", params![id])? > 0);
            }
            // Repeated enable must not discard legitimate downtime catch-up.
            if job.enabled && job.approved { return Ok(true); }
            // Read time after acquiring the SQLite writer boundary; waiting
            // for a busy writer must not leave the next slot in the past.
            let now = clock();
            let next = match job.cron.as_deref() {
                Some(cron) => crate::jobs::first_run(Some(cron), None, now)?,
                None => {
                    let error = "one-shot job has expired or completed; use jobs run ID explicitly or create a new schedule";
                    let at = job.next_run.as_deref().context(error)?;
                    crate::jobs::parse_at(at, now).context(error)?
                }
            };
            Ok(st.ex("UPDATE jobs SET enabled=1, approved=1, next_run=?2 WHERE id=?1", params![id, next.to_rfc3339()])? > 0)
        })
    }

    /// Record a job's run; `next_run` None ends a one-shot job.
    pub fn job_ran(&self, id: &str, status: &str, next_run: Option<&str>) -> Result<()> {
        self.ex(
            "UPDATE jobs SET last_run=?2, last_status=?3, next_run=?4,
               enabled=CASE WHEN ?4 IS NULL THEN 0 ELSE enabled END WHERE id=?1",
            params![id, now(), status, next_run],
        )?;
        Ok(())
    }

    /// Pause a scheduler failure without consuming its due occurrence.
    pub(crate) fn pause_job(&self, id: &str, status: &str) -> Result<()> {
        self.ex(
            "UPDATE jobs SET enabled=0, last_run=?2, last_status=?3 WHERE id=?1",
            params![id, now(), status],
        )?;
        Ok(())
    }

    /// A firing's Run ended: its outcome becomes the job's last status.
    pub fn job_ran_status(&self, id: &str, status: &str) -> Result<()> {
        self.ex(
            "UPDATE jobs SET last_status=?2 WHERE id=?1
             AND NOT (enabled=0 AND last_status LIKE 'paused: %')",
            params![id, status],
        )?;
        Ok(())
    }

    pub fn delete_job(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM jobs WHERE id=?1", params![id])?
            > 0)
    }

    /// Whether any job is waiting to run (keeps the daemon up when the
    /// user enabled `daemon.keep_alive_for_jobs`).
    pub fn has_active_jobs(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs WHERE enabled=1 AND approved=1 AND next_run IS NOT NULL)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Queue a Tool for the optimiser (persisted; no duplicates).
    pub fn enqueue_optimise(&self, tool_id: &str) -> Result<()> {
        self.ex(
            "INSERT OR IGNORE INTO optimiser_queue(tool_id, queued_at) VALUES(?1, ?2)",
            params![tool_id, now()],
        )?;
        Ok(())
    }

    /// Queued Tools, oldest first.
    pub fn optimise_queue(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tool_id FROM optimiser_queue ORDER BY queued_at, rowid")?;
        let rows = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn dequeue_optimise(&self, tool_id: &str) -> Result<()> {
        self.ex(
            "DELETE FROM optimiser_queue WHERE tool_id=?1",
            params![tool_id],
        )?;
        Ok(())
    }

    /// Whether a Tool is in use: an Invocation still running, or used by
    /// one of `active_runs` (SPEC §8.3: the optimiser waits only for those).
    pub fn tool_busy(&self, tool_id: &str, active_runs: &[String]) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM invocations WHERE tool_id=?1
               AND (status='running' OR run_id IN (SELECT value FROM json_each(?2))))",
            params![tool_id, serde_json::to_string(active_runs)?],
            |r| r.get(0),
        )?)
    }

    /// Token use for a Run (`run_id`) or a whole Session, per model, and
    /// the steps: (model, prompt, completion, cached) rows plus steps.
    pub fn usage_rows(
        &self,
        session_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<(Vec<ModelUsage>, i64)> {
        let mut stmt = self.ps(
            "SELECT provider, model, SUM(prompt_tokens), SUM(completion_tokens), SUM(cached_tokens),
               SUM(cache_write_tokens), CASE WHEN COUNT(cost)=COUNT(*) THEN SUM(cost) END, MAX(cost_unknown) FROM run_usage
             WHERE (?1 IS NULL OR session_id=?1) AND (?2 IS NULL OR run_id=?2) GROUP BY provider, model, (cost IS NULL), cost_unknown",
        )?;
        let rows = stmt
            .query_map(params![session_id, run_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let steps: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(steps),0) FROM runs WHERE (?1 IS NULL OR session_id=?1) AND (?2 IS NULL OR id=?2)",
            params![session_id, run_id],
            |r| r.get(0),
        )?;
        Ok((rows, steps))
    }

    /// Error of the immediately preceding Run only when it was explicitly
    /// resumed. A fresh message after a paused/completed Run resets guidance.
    pub fn previous_resumed_run_error(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT CASE WHEN status='resumed' THEN error END FROM runs
             WHERE session_id=?1 AND rowid<(SELECT rowid FROM runs WHERE id=?2 AND session_id=?1)
             ORDER BY rowid DESC LIMIT 1",
                params![session_id, run_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Record why a Run stopped.
    pub fn run_error(&self, run_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT error FROM runs WHERE id=?1", params![run_id], |r| {
                r.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten())
    }

    pub fn set_run_error(&self, run_id: &str, error: &str) -> Result<()> {
        self.ex(
            "UPDATE runs SET error=?2 WHERE id=?1",
            params![run_id, error],
        )?;
        Ok(())
    }

    /// Read the live Session and latest Run status with strict SQL errors.
    /// `None` means the Session is unavailable, not that it has no Run.
    pub(crate) fn idle_step_off_status(&self, session_id: &str) -> Result<Option<bool>> {
        Ok(self
            .conn
            .query_row(
                "SELECT (SELECT status FROM runs WHERE session_id=sessions.id
                     ORDER BY created_at DESC, rowid DESC LIMIT 1)
               FROM sessions WHERE id=?1 AND deleted_at IS NULL",
                params![session_id],
                |row| {
                    let status = row.get::<_, Option<String>>(0)?;
                    Ok(matches!(
                        status.as_deref(),
                        None | Some("completed" | "abandoned" | "failed")
                    ))
                },
            )
            .optional()?)
    }

    /// The Session's last Run when it stopped unfinished (interrupted or
    /// paused): (id, status, reason).
    pub fn unfinished_last_run(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, status, error FROM runs WHERE session_id=?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok()
            .filter(|(_, s, _): &(String, String, String)| s == "interrupted" || s == "paused"))
    }

    /// Record one finished sub-agent task for routing (SPEC §4.6).
    #[allow(clippy::too_many_arguments)]
    pub fn record_task(
        &self,
        role: &str,
        model: &str,
        session_id: &str,
        run_id: &str,
        task_hash: &str,
        ms: i64,
        tokens: i64,
        turns: i64,
        outcome: &str,
    ) -> Result<()> {
        self.record_task_with_id(
            &uuid::Uuid::new_v4().to_string(),
            role,
            model,
            session_id,
            run_id,
            task_hash,
            ms,
            tokens,
            turns,
            outcome,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_task_with_id(
        &self,
        id: &str,
        role: &str,
        model: &str,
        session_id: &str,
        run_id: &str,
        task_hash: &str,
        ms: i64,
        tokens: i64,
        turns: i64,
        outcome: &str,
    ) -> Result<()> {
        self.ex(
            "INSERT INTO task_stats(id,role,model,session_id,run_id,task_hash,ms,tokens,turns,outcome,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![id, role, model, session_id, run_id, task_hash, ms, tokens, turns, outcome, now()],
        )?;
        Ok(())
    }

    /// The same task started again in the same Run: the earlier result was
    /// not good enough, so it no longer counts as good.
    pub fn mark_task_respawned(&self, role: &str, run_id: &str, task_hash: &str) -> Result<()> {
        self.ex(
            "UPDATE task_stats SET outcome='respawned' WHERE role=?1 AND run_id=?2 AND task_hash=?3 AND outcome='completed'",
            params![role, run_id, task_hash],
        )?;
        Ok(())
    }

    /// Per-model task stats of a role.
    pub fn task_stats(&self, role: &str) -> Result<Vec<crate::routing::TaskStat>> {
        let mut stmt = self.ps(
            "SELECT model, COUNT(*), SUM(outcome='completed'), AVG(ms), AVG(tokens) FROM task_stats WHERE role=?1 GROUP BY model",
        )?;
        let rows = stmt
            .query_map(params![role], |r| {
                Ok(crate::routing::TaskStat {
                    model: r.get(0)?,
                    samples: r.get(1)?,
                    good: r.get(2)?,
                    avg_ms: r.get(3)?,
                    avg_tokens: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Log an agent change to the harness (SPEC §7.13).
    pub fn record_harness_change(
        &self,
        op: &str,
        target: &str,
        before: &serde_json::Value,
        after: &serde_json::Value,
        tool: &str,
        reason: &str,
    ) -> Result<String> {
        let (before, after, created) = (before.to_string(), after.to_string(), now());
        insert_short_id('h', "harness change", uuid::Uuid::new_v4, |id| {
            self.ex(
                "INSERT INTO harness_changes(id,op,target,before,after,tool,reason,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(id) DO NOTHING",
                params![id, op, target, before, after, tool, reason, created],
            )
        })
    }

    /// Harness changes, newest first.
    pub fn harness_changes(&self, limit: usize) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self.ps(
            "SELECT id, op, target, before, after, tool, reason, undone, created_at FROM harness_changes
             ORDER BY created_at DESC, rowid DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                let parse = |s: String| serde_json::from_str(&s).unwrap_or(serde_json::Value::Null);
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?, "op": r.get::<_, String>(1)?,
                    "target": r.get::<_, String>(2)?, "before": parse(r.get(3)?),
                    "after": parse(r.get(4)?), "tool": r.get::<_, String>(5)?,
                    "reason": r.get::<_, String>(6)?, "undone": r.get::<_, i64>(7)? != 0,
                    "created_at": r.get::<_, String>(8)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn mark_harness_undone(&self, id: &str) -> Result<()> {
        self.ex(
            "UPDATE harness_changes SET undone=1 WHERE id=?1",
            params![id],
        )?;
        Ok(())
    }

    /// One model call made in a Run (a step).
    pub fn run_steps(&self, run_id: &str) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT steps FROM runs WHERE id=?1", params![run_id], |r| {
                r.get(0)
            })
            .unwrap_or(0))
    }

    pub fn bump_run_steps(&self, run_id: &str) -> Result<()> {
        self.conn
            .execute("UPDATE runs SET steps=steps+1 WHERE id=?1", params![run_id])?;
        Ok(())
    }

    /// Mark a Session's latest finished Run as not having met the user's
    /// goal: `edited` (the user edited and re-ran) or `corrected` (the next
    /// message reads like "that's wrong"). The first follow-up wins.
    pub fn mark_last_run_followup(&self, session_id: &str, followup: &str) -> Result<()> {
        self.ex(
            "UPDATE runs SET followup=?2 WHERE id=(SELECT id FROM runs WHERE session_id=?1
               ORDER BY created_at DESC, rowid DESC LIMIT 1) AND followup=''",
            params![session_id, followup],
        )?;
        // A primary role's task that the user corrected was not good
        // (SPEC §4.6).
        self.ex(
            "UPDATE task_stats SET outcome=?2 WHERE run_id=(SELECT id FROM runs WHERE session_id=?1
               ORDER BY created_at DESC, rowid DESC LIMIT 1) AND outcome='completed'",
            params![session_id, followup],
        )?;
        Ok(())
    }

    /// Record what `path` held before a Run first changed it: `existed`
    /// false for a new file; `before` its bytes when `kept` (small enough
    /// and a regular file). Later changes in the same Run keep the first.
    pub fn snapshot_run_file(
        &self,
        session_id: &str,
        run_id: &str,
        path: &str,
        existed: bool,
        before: Option<&[u8]>,
    ) -> Result<()> {
        self.snapshot_run_file_with_metadata(session_id, run_id, path, existed, before, None)
    }

    /// Bytes and their descriptor-bound metadata are a single immutable first
    /// snapshot. In particular, a later call must never fill a legacy NULL with
    /// permissions observed after the original change.
    pub(crate) fn snapshot_run_file_with_metadata(
        &self,
        session_id: &str,
        run_id: &str,
        path: &str,
        existed: bool,
        before: Option<&[u8]>,
        metadata: Option<&str>,
    ) -> Result<()> {
        self.ex(
            "INSERT OR IGNORE INTO run_files(run_id,session_id,path,existed,kept,before,created_at,before_metadata)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                run_id,
                session_id,
                path,
                existed,
                !existed || before.is_some(),
                before,
                now(),
                metadata
            ],
        )?;
        Ok(())
    }

    /// The files a Run changed: (path, existed before, revertible, bytes
    /// before), in the order first touched.
    #[allow(clippy::type_complexity)]
    pub fn run_files(&self, run_id: &str) -> Result<Vec<(String, bool, bool, Option<Vec<u8>>)>> {
        let mut stmt = self.ps(
            "SELECT path, existed, kept, before FROM run_files WHERE run_id=?1 ORDER BY rowid",
        )?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The latest Run with recorded paths or a durable untracked-write caveat.
    /// Paths survive expiry and rewind, so this never falls back to older edits.
    pub fn last_run_with_files(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row(
            "SELECT r.id FROM runs r JOIN sessions s ON s.id=r.session_id
             WHERE r.session_id=?1 AND s.deleted_at IS NULL AND
             (r.untracked_writes_possible=1 OR EXISTS(SELECT 1 FROM run_files f WHERE f.run_id=r.id))
             ORDER BY r.created_at DESC,r.rowid DESC LIMIT 1",
            params![session_id], |r| r.get(0),
        ).optional()?)
    }

    /// Finished calls in a Run: (name, count, failed), including nested
    /// Invocations and built-ins. Unfinished calls are counted separately.
    pub fn run_calls(&self, run_id: &str) -> Result<Vec<(String, i64, i64)>> {
        let mut stmt = self.ps("SELECT name, COUNT(*), SUM(failed) FROM (
               SELECT t.name AS name, (i.status <> 'success') AS failed
                 FROM invocations i JOIN tools t ON t.id=i.tool_id
                WHERE i.run_id=?1 AND i.status NOT IN ('running','pending')
               UNION ALL
               SELECT name, (ok=0) FROM builtin_calls WHERE run_id=?1)
             GROUP BY name ORDER BY COUNT(*) DESC, name")?;
        let rows = stmt
            .query_map(params![run_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The authoritative item list, with legacy metadata kept separately.
    pub fn session_plan(&self, session_id: &str) -> Result<Vec<formless_protocol::PlanItem>> {
        self.session_plan_snapshot(session_id)
            .map(|(items, _)| items)
    }

    /// Legacy fixture/import compatibility. Live agents must supply their Run.
    pub fn set_session_plan(
        &self,
        session_id: &str,
        items: &[formless_protocol::PlanItem],
    ) -> Result<()> {
        self.set_legacy_plan(session_id, items)
    }

    /// Roughly what fills the database, in bytes of stored text/blobs.
    pub fn content_bytes(&self) -> Result<serde_json::Value> {
        let sum = |sql: &str| -> Result<i64> {
            Ok(self
                .conn
                .query_row(sql, [], |r| r.get::<_, Option<i64>>(0))?
                .unwrap_or(0))
        };
        Ok(serde_json::json!({
            "messages": sum("SELECT SUM(length(content) + COALESCE(length(meta),0)) FROM messages")?,
            "invocations": sum("SELECT SUM(length(input) + length(output) + length(error)) FROM invocations")?,
            "tool_versions": sum("SELECT SUM(length(code) + length(input_schema) + length(output_schema)) FROM tool_versions")?,
            "run_files": sum("SELECT SUM(COALESCE(length(before),0)) FROM run_files")?,
        }))
    }

    /// Unused pages VACUUM would give back.
    pub fn free_bytes(&self) -> Result<u64> {
        let free: i64 = self
            .conn
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        let size: i64 = self.conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok((free * size).max(0) as u64)
    }

    pub fn vacuum(&self) -> Result<()> {
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// A consistent copy of the whole database (WAL included) at `dest`.
    /// Existing snapshots are never removed or replaced.
    pub fn backup_into(&self, dest: &Path) -> Result<()> {
        if dest.exists() {
            return Err(anyhow!(
                "backup destination already exists: {}",
                dest.display()
            ));
        }
        self.conn
            .execute("VACUUM INTO ?1", params![dest.to_string_lossy()])
            .with_context(|| format!("back up database to {}", dest.display()))?;
        Ok(())
    }

    /// Drop restoration authority after rewind, retaining the path as a tombstone.
    /// Internal compatibility wrapper; live routes validate ownership first.
    pub fn forget_run_file(&self, run_id: &str, path: &str) -> Result<()> {
        self.ex(
            "UPDATE run_files SET kept=0,before=NULL,before_metadata=NULL,not_kept_reason='Already rewound' WHERE run_id=?1 AND path=?2",
            params![run_id, path],
        )?;
        Ok(())
    }

    /// Rate a Session's latest finished Run (or `run_id`): "good", "bad"
    /// or "" to clear, with an optional comment. A bad rating counts as
    /// the `rated_bad` outcome for Run metrics and the primary role's
    /// routing stats (SPEC §8.7, §4.6). Returns the rated Run's id.
    pub fn rate_run(
        &self,
        session_id: &str,
        run_id: Option<&str>,
        rating: &str,
        comment: Option<&str>,
    ) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        let run_id: Option<String> = match run_id {
            Some(r) => Some(r.to_string()),
            None => self
                .conn
                .query_row(
                    "SELECT id FROM runs WHERE session_id=?1 AND status!='running'
                     ORDER BY created_at DESC, rowid DESC LIMIT 1",
                    params![session_id],
                    |r| r.get(0),
                )
                .optional()?,
        };
        let Some(run_id) = run_id else {
            return Ok(None);
        };
        let n = self.ex(
            "UPDATE runs SET rating=?3, rating_comment=?4 WHERE id=?1 AND session_id=?2",
            params![
                run_id,
                session_id,
                rating,
                comment.filter(|c| !c.trim().is_empty())
            ],
        )?;
        if n == 0 {
            return Ok(None);
        }
        if rating == "bad" {
            self.ex(
                "UPDATE task_stats SET outcome='rated_bad' WHERE run_id=?1 AND outcome='completed'",
                params![run_id],
            )?;
        } else {
            self.ex(
                "UPDATE task_stats SET outcome='completed' WHERE run_id=?1 AND outcome='rated_bad'",
                params![run_id],
            )?;
        }
        Ok(Some(run_id))
    }

    /// Per-Run metrics, newest first.
    pub fn record_forecast(
        &self,
        run_id: &str,
        forecast: &formless_protocol::RunForecast,
    ) -> Result<()> {
        self.ex(
            "INSERT INTO run_forecasts(run_id,forecast) VALUES(?1,?2)",
            params![run_id, serde_json::to_string(forecast)?],
        )?;
        Ok(())
    }
    pub fn recorded_forecasts(&self) -> Result<Vec<(String, formless_protocol::RunForecast)>> {
        self.recorded_forecasts_limited(2000)
    }
    pub(crate) fn recorded_forecasts_limited(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, formless_protocol::RunForecast)>> {
        if !self.has_forecast_history()? {
            return Ok(Vec::new());
        }
        let mut stmt =
            self.ps("SELECT run_id,forecast FROM run_forecasts ORDER BY rowid DESC LIMIT ?1")?;
        let rows = stmt.query_map(params![limit.min(2000) as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.map(|r| {
            let (id, json) = r?;
            Ok((id, serde_json::from_str(&json)?))
        })
        .collect()
    }
    pub fn record_forecast_request(&self, run_id: &str, source: &str) -> Result<()> {
        if run_id.is_empty() {
            return Ok(());
        }
        self.ex("INSERT INTO forecast_request_counts(run_id,source,expected) SELECT ?1,?2,1 WHERE EXISTS(SELECT 1 FROM runs WHERE id=?1) ON CONFLICT(run_id,source) DO UPDATE SET expected=expected+1",params![run_id,source])?;
        Ok(())
    }
    pub fn forecast_usage_complete(&self, run_id: &str) -> Result<bool> {
        let (expected,main):(i64,i64)=self.conn.query_row("SELECT COALESCE(SUM(expected),0),COALESCE(SUM(CASE WHEN source='run' THEN expected ELSE 0 END),0) FROM forecast_request_counts WHERE run_id=?1",params![run_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let (rows, known): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),COALESCE(SUM(usage_complete),0) FROM run_usage WHERE run_id=?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(
            forecast_report_storage::covered_requests(expected, rows, known)
                && main >= self.run_steps(run_id)?,
        )
    }

    /// Check coverage for a branch's allocated Runs in one statement.
    /// Each aggregate streams one Run's indexed rows. Packing each pair keeps
    /// one scan per child table and avoids sorting the matched history.
    fn forecast_usage_complete_for_runs(&self, run_ids: &[String]) -> Result<bool> {
        if run_ids.is_empty() {
            return Ok(true);
        }
        let ids = serde_json::to_string(run_ids)?;
        let mut stmt = self.ps("SELECT
                 (SELECT json_array(
                     COALESCE(SUM(expected),0),
                     COALESCE(SUM(CASE WHEN source='run' THEN expected ELSE 0 END),0))
                  FROM forecast_request_counts WHERE run_id=w.value),
                 (SELECT json_array(COUNT(*),COALESCE(SUM(usage_complete),0))
                  FROM run_usage WHERE run_id=w.value),
                 r.steps
             FROM json_each(?1) AS w LEFT JOIN runs AS r ON r.id=w.value")?;
        let mut rows = stmt.query(params![ids])?;
        let mut complete = true;
        while let Some(row) = rows.next()? {
            let read_counts = |column| -> Result<(i64, i64)> {
                Ok(serde_json::from_str(row.get_ref(column)?.as_str()?)?)
            };
            let (expected, main) = read_counts(0)?;
            let (recorded, covered) = read_counts(1)?;
            // Match run_steps' zero fallback for a missing or invalid value.
            let steps: i64 = row.get(2).unwrap_or_default();
            complete &=
                expected > 0 && expected == recorded && expected == covered && main >= steps;
        }
        Ok(complete)
    }

    pub fn forecast_models(&self, run_id: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .ps("SELECT DISTINCT provider,model FROM run_usage WHERE run_id=?1 AND source='run'")?;
        let rows = stmt.query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn count_runs_for_session(&self, session_id: &str) -> Result<i64> {
        Ok(self
            .ps("SELECT COUNT(*) FROM runs WHERE session_id=?1")?
            .query_row(params![session_id], |r| r.get(0))?)
    }

    pub fn run_metrics(&self, limit: usize) -> Result<Vec<formless_protocol::RunMetricsInfo>> {
        self.run_metrics_for(None, limit, None)
    }

    /// Resolve literal prefixes, preferring an exact identity.
    pub fn inspect_run(&self, query: &str) -> Result<Option<serde_json::Value>> {
        if query.is_empty() {
            let any_run: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE id IS NOT NULL)",
                [],
                |row| row.get(0),
            )?;
            anyhow::ensure!(!any_run, "ambiguous Run id");
            return Ok(None);
        }
        self.read_transaction(|st| {
            if let Some(exact) = st.inspect_exact_run(query)? {
                return Ok(Some(exact));
            }
            let encoding: String = st.ps("PRAGMA encoding")?.query_row([], |row| row.get(0))?;
            let (sql, upper) = match encoding.as_str() {
                "UTF-8" => match run_id_prefix_end(query) {
                    Some(upper) => (RUN_ID_PREFIX_RANGE, Some(upper)),
                    None => (RUN_ID_PREFIX_TAIL, None),
                },
                // Restored databases can use UTF-16, whose binary byte order
                // differs from the scalar ordering used for the upper bound.
                _ => (RUN_ID_PREFIX_OTHER_ENCODING, None),
            };
            let mut stmt = st.ps(sql)?;
            let binds = rusqlite::params_from_iter(std::iter::once(query).chain(upper.as_deref()));
            let mut ids = stmt.query(binds)?;
            let Some(first) = ids.next()? else {
                return Ok(None);
            };
            let id: String = first.get(0)?;
            anyhow::ensure!(ids.next()?.is_none(), "ambiguous Run id");
            st.inspect_exact_run(&id)
        })
    }

    pub(crate) fn inspect_exact_run(&self, id: &str) -> Result<Option<serde_json::Value>> {
        let Some(mut run) = self.ps(RUN_INSPECT_EXACT)?.query_row(params![id], |r| Ok(serde_json::json!({
            "run_id":r.get::<_,String>(0)?, "session_id":r.get::<_,String>(1)?,
            "status":r.get::<_,String>(2)?, "error":r.get::<_,Option<String>>(3)?,
            "steps":r.get::<_,i64>(4)?, "created_at":r.get::<_,String>(5)?, "updated_at":r.get::<_,String>(6)?,
            "start_message_seq":r.get::<_,Option<i64>>(7)?, "end_message_seq":r.get::<_,Option<i64>>(8)?,
            "tool_calls":r.get::<_,i64>(9)?
        }))).optional()? else {
            return Ok(None);
        };
        let usage = self.run_usage(id)?;
        // Preserve the signed SQLite totals already returned by GetRun.
        run["prompt_tokens"] = serde_json::json!(usage.prompt_tokens as i64);
        run["completion_tokens"] = serde_json::json!(usage.completion_tokens as i64);
        run["cached_tokens"] = serde_json::json!(usage.cached_tokens);
        run["cache_write_tokens"] = serde_json::json!(usage.cache_write_tokens);
        run["reasoning_tokens"] = serde_json::json!(usage.reasoning_tokens);
        Ok(Some(run))
    }

    pub(crate) fn run_used_tool(&self, run_id: &str, tool_id: &str) -> Result<bool> {
        Ok(self.ps("SELECT EXISTS(SELECT 1 FROM invocations WHERE run_id=?1 AND tool_id=?2 AND status='success')")?
            .query_row(params![run_id, tool_id], |row| row.get(0))?)
    }

    pub fn run_metrics_for(
        &self,
        session_id: Option<&str>,
        limit: usize,
        since: Option<&str>,
    ) -> Result<Vec<formless_protocol::RunMetricsInfo>> {
        self.run_metrics_matching(session_id, limit, since, None)
    }

    /// One exact Run, independent of pagination or other Sessions' activity.
    /// Routing completion must not search an unrelated recent-results page.
    pub(crate) fn run_metric(
        &self,
        run_id: &str,
    ) -> Result<Option<formless_protocol::RunMetricsInfo>> {
        Ok(self
            .run_metrics_matching(None, 1, None, Some(run_id))?
            .into_iter()
            .next())
    }

    fn run_metrics_matching(
        &self,
        session_id: Option<&str>,
        limit: usize,
        since: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<formless_protocol::RunMetricsInfo>> {
        let mut stmt = self.ps(
            "SELECT r.id, r.session_id, r.status, r.followup, r.steps,
               (SELECT COUNT(*) FROM invocations i WHERE i.run_id=r.id),
               (SELECT COALESCE(SUM(prompt_tokens),0) FROM run_usage u WHERE u.run_id=r.id),
               (SELECT COALESCE(SUM(completion_tokens),0) FROM run_usage u WHERE u.run_id=r.id),
               r.created_at, r.updated_at, r.rating
             FROM runs r WHERE (?2 IS NULL OR r.session_id=?2) AND (?3 IS NULL OR r.created_at >= ?3)
               AND (?4 IS NULL OR r.id=?4)
             ORDER BY r.created_at DESC, r.rowid DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64, session_id, since, run_id], |r| {
                let (created, updated): (String, String) = (r.get(8)?, r.get(9)?);
                let duration_ms = run_duration_ms(&created, &updated);
                let status: String = r.get(2)?;
                let followup: String = r.get(3)?;
                let rating: String = r.get(10)?;
                Ok(formless_protocol::RunMetricsInfo {
                    run_id: r.get(0)?,
                    session_id: r.get(1)?,
                    // The user's explicit 👎 outranks inferred follow-ups.
                    outcome: if rating == "bad" {
                        "rated_bad".into()
                    } else if followup.is_empty() {
                        status.clone()
                    } else {
                        followup
                    },
                    rating,
                    status,
                    steps: r.get(4)?,
                    tool_calls: r.get(5)?,
                    prompt_tokens: r.get(6)?,
                    completion_tokens: r.get(7)?,
                    duration_ms,
                    created_at: created,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn set_run_status(&self, run_id: &str, status: &str) -> Result<()> {
        self.ex(
            "UPDATE runs SET status=?2, updated_at=?3,
             end_message_id=CASE WHEN end_message_seq IS NULL AND status='running' AND ?2 IN ('completed','paused','interrupted','failed','abandoned') THEN (SELECT id FROM messages WHERE session_id=runs.session_id ORDER BY seq DESC,rowid DESC LIMIT 1) ELSE end_message_id END,
             end_message_seq=CASE WHEN end_message_seq IS NULL AND status='running' AND ?2 IN ('completed','paused','interrupted','failed','abandoned') THEN (SELECT COALESCE(MAX(seq),0) FROM messages WHERE session_id=runs.session_id) ELSE end_message_seq END WHERE id=?1",
            params![run_id, status, now()],
        )?;
        Ok(())
    }
    pub fn create_subagent(
        &self,
        session_id: &str,
        run_id: &str,
        task: &str,
        depth: u32,
        model: &str,
    ) -> Result<SubagentInfo> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        self.ex(
            "INSERT INTO subagents(id,session_id,run_id,task,depth,status,created_at,updated_at,model) VALUES(?1,?2,?3,?4,?5,'running',?6,?6,?7)",
            params![id, session_id, run_id, task, depth, t, model],
        )?;
        self.get_subagent(&id)?.context("sub-agent vanished")
    }

    fn row_subagent(r: &rusqlite::Row) -> rusqlite::Result<SubagentInfo> {
        Ok(SubagentInfo {
            model: r.get("model")?,
            id: r.get("id")?,
            session_id: r.get("session_id")?,
            run_id: r.get("run_id")?,
            task: r.get("task")?,
            depth: r.get("depth")?,
            status: r.get("status")?,
            steps: r.get("steps")?,
            result: r.get("result")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
        })
    }

    pub fn get_subagent(&self, id: &str) -> Result<Option<SubagentInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM subagents WHERE id=?1",
                params![id],
                Self::row_subagent,
            )
            .ok())
    }

    pub fn list_subagents(&self, session_id: &str) -> Result<Vec<SubagentInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM subagents WHERE session_id=?1 ORDER BY created_at, rowid")?;
        let rows = stmt
            .query_map(params![session_id], Self::row_subagent)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Append one provider-shaped message to a sub-agent's transcript.
    pub fn append_subagent_message(&self, subagent_id: &str, message: &Value) -> Result<()> {
        self.ex(
            "INSERT INTO subagent_messages(subagent_id,seq,message) VALUES(?1,(SELECT COALESCE(MAX(seq),0)+1 FROM subagent_messages WHERE subagent_id=?1),?2)",
            params![subagent_id, message.to_string()],
        )?;
        Ok(())
    }

    pub fn subagent_messages(&self, subagent_id: &str) -> Result<Vec<Value>> {
        let mut stmt = self
            .conn
            .prepare("SELECT message FROM subagent_messages WHERE subagent_id=?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![subagent_id], |r| r.get::<_, String>(0))?;
        let mut messages = Vec::new();
        for row in rows {
            // Drop each raw string after parsing; the returned parsed transcript
            // remains complete. Row errors propagate; only malformed JSON skips.
            let raw = row?;
            if let Ok(message) = serde_json::from_str(&raw) {
                messages.push(message);
            }
        }
        Ok(messages)
    }

    pub fn set_subagent_steps(&self, id: &str, steps: i64) -> Result<()> {
        self.ex(
            "UPDATE subagents SET steps=?2, updated_at=?3 WHERE id=?1",
            params![id, steps, now()],
        )?;
        Ok(())
    }

    pub fn finish_subagent(&self, id: &str, status: &str, result: &str) -> Result<SubagentInfo> {
        self.ex(
            "UPDATE subagents SET status=?2, result=?3, updated_at=?4 WHERE id=?1",
            params![id, status, result, now()],
        )?;
        self.get_subagent(id)?.context("sub-agent vanished")
    }

    /// Record a Pending Invocation handle the agent was given (§6.2), so it
    /// can still be answered after a daemon restart.
    pub fn add_pending_handle(&self, handle: &str, session_id: &str, tool: &str) -> Result<()> {
        self.ex(
            "INSERT OR REPLACE INTO pending_handles(handle,session_id,tool_name,status,created_at) VALUES(?1,?2,?3,'pending',?4)",
            params![handle, session_id, tool, now()],
        )?;
        Ok(())
    }

    /// The handle's result was delivered (or dropped): forget it.
    pub fn remove_pending_handle(&self, handle: &str) -> Result<()> {
        self.ex(
            "DELETE FROM pending_handles WHERE handle=?1",
            params![handle],
        )?;
        Ok(())
    }

    /// (tool name, status) of a recorded handle.
    pub fn pending_handle(&self, handle: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT tool_name, status FROM pending_handles WHERE handle=?1",
                params![handle],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok())
    }

    /// Recorded handle visible only within its owning Session.
    pub fn session_pending_handle(
        &self,
        session: &str,
        handle: &str,
    ) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT tool_name,status FROM pending_handles WHERE handle=?1 AND session_id=?2",
                params![handle, session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    /// On daemon startup: every handle still pending lost its in-memory
    /// work. Marks them interrupted and returns (handle, session, tool).
    pub fn interrupt_pending_handles(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.ps(
            "SELECT handle, session_id, tool_name FROM pending_handles WHERE status='pending'",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        self.ex(
            "UPDATE pending_handles SET status='interrupted' WHERE status='pending'",
            [],
        )?;
        Ok(rows)
    }

    /// Queue a message to start its own Run after the current one (§5.3).
    pub fn enqueue_message(&self, session_id: &str, content: &str) -> Result<()> {
        self.ex(
            "INSERT INTO queued_messages(session_id,content,created_at) VALUES(?1,?2,?3)",
            params![session_id, content, now()],
        )?;
        Ok(())
    }

    /// Remove and return the Session's oldest queued message.
    pub fn pop_queued_message(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self.ps(
            "DELETE FROM queued_messages WHERE id=(SELECT id FROM queued_messages WHERE session_id=?1
               ORDER BY COALESCE(position,id),id LIMIT 1) RETURNING content",
        )?.query_row([session_id], |row| row.get(0)).optional()?)
    }

    pub fn list_queued_messages(&self, session_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT content FROM queued_messages WHERE session_id=?1 ORDER BY COALESCE(position,id),id")?;
        let rows = stmt
            .query_map(params![session_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    /// The Session's queued messages with their ids, in send order.
    pub fn queued_items(&self, session_id: &str) -> Result<Vec<formless_protocol::QueuedMessage>> {
        let mut stmt =
            self.ps("SELECT id, content, revision FROM queued_messages WHERE session_id=?1 ORDER BY COALESCE(position,id),id")?;
        let rows = stmt
            .query_map(params![session_id], |r| {
                Ok(formless_protocol::QueuedMessage {
                    id: r.get(0)?,
                    content: r.get(1)?,
                    revision: Some(r.get(2)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Replace a queued message's text, or remove it (`None`). False when
    /// it is no longer queued (already sent).
    pub fn edit_queued(&self, session_id: &str, id: i64, content: Option<&str>) -> Result<bool> {
        let n = match content {
            Some(c) => self.ex(
                "UPDATE queued_messages SET content=?3, revision=revision+1 WHERE id=?1 AND session_id=?2",
                params![id, session_id, c],
            )?,
            None => self.ex(
                "DELETE FROM queued_messages WHERE id=?1 AND session_id=?2",
                params![id, session_id],
            )?,
        };
        Ok(n > 0)
    }

    /// A revision binds editable text and position. Reject a stale draft
    /// atomically even when there was no event on the editing connection.
    pub fn edit_queued_if_current(
        &self,
        session_id: &str,
        id: i64,
        revision: i64,
        content: &str,
    ) -> Result<bool> {
        Ok(self.ex(
            "UPDATE queued_messages SET content=?4, revision=revision+1 WHERE session_id=?1 AND id=?2 AND revision=?3",
            params![session_id, id, revision, content],
        )? > 0)
    }

    /// Move a queued message one place without changing its identity or text.
    pub fn move_queued(&self, session_id: &str, id: i64, up: bool) -> Result<Option<i64>> {
        self.transaction(|st| {
            let Some(position) = st.ps("SELECT COALESCE(position,id) FROM queued_messages WHERE session_id=?1 AND id=?2")?
                .query_row(params![session_id, id], |row| row.get::<_, i64>(0)).optional()? else {
                return Ok(None);
            };
            let neighbor = if up {
                "SELECT id, COALESCE(position,id) FROM queued_messages WHERE session_id=?1 AND COALESCE(position,id)<?2 ORDER BY COALESCE(position,id) DESC,id DESC LIMIT 1"
            } else {
                "SELECT id, COALESCE(position,id) FROM queued_messages WHERE session_id=?1 AND COALESCE(position,id)>?2 ORDER BY COALESCE(position,id),id LIMIT 1"
            };
            let Some((other, other_position)) = st.ps(neighbor)?
                .query_row(params![session_id, position], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))).optional()? else {
                return Ok(None);
            };
            st.ex(
                "UPDATE queued_messages SET position=CASE id WHEN ?2 THEN ?4 ELSE ?5 END, revision=revision+1
                 WHERE session_id=?1 AND id IN (?2,?3)",
                params![session_id, id, other, other_position, position],
            )?;
            Ok(Some(id))
        })
    }

    pub fn clear_queued_messages(&self, session_id: &str) -> Result<()> {
        self.ex(
            "DELETE FROM queued_messages WHERE session_id=?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// The Session's most recent Run as (run id, status).
    pub fn last_run(&self, session_id: &str) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, status FROM runs WHERE session_id=?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok())
    }

    /// Forget the Anthropic thinking stored on a Session's tool-call turns
    /// (the endpoint rejected it): it is not replayed again.
    pub fn drop_thinking_blocks(&self, session_id: &str) -> Result<usize> {
        // Collect before any writes, in the original sequence order. Keep id
        // conversion strict and meta conversion-to-missing like row_msg.
        // Unused message columns (including corrupt content) are no longer
        // decoded; this deliberately narrows the corrupt-row error surface.
        let messages = {
            let mut statement = self
                .conn
                .prepare("SELECT id, meta FROM messages WHERE session_id=?1 ORDER BY seq")?;
            let rows = statement
                .query_map(params![session_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1).ok().flatten(),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut changed = 0;
        for (id, metadata) in messages {
            let Some(mut meta) = metadata
                .as_deref()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
            else {
                continue;
            };
            if meta
                .as_object_mut()
                .and_then(|o| o.remove("thinking_blocks"))
                .is_some()
            {
                changed += self.ex(
                    "UPDATE messages SET meta=?3 WHERE session_id=?1 AND id=?2",
                    params![session_id, id, meta.to_string()],
                )?;
            }
        }
        if changed > 0 {
            self.note_rewrite();
        }
        Ok(changed)
    }

    /// Tool calls of the latest assistant tool-call turn that never got a
    /// result (the Run stopped mid-batch), as (call id, tool name).
    pub fn dangling_tool_calls(&self, session_id: &str) -> Result<Vec<(String, String)>> {
        let msgs = self.list_messages(session_id)?;
        let Some(pos) = msgs.iter().rposition(|m| {
            m.role == "assistant"
                && m.meta
                    .as_deref()
                    .is_some_and(|meta| meta.contains("\"tool_calls\""))
        }) else {
            return Ok(Vec::new());
        };
        let meta: Value = serde_json::from_str(msgs[pos].meta.as_deref().unwrap_or("{}"))?;
        let answered: std::collections::HashSet<String> = msgs[pos + 1..]
            .iter()
            .filter(|m| m.role == "tool")
            .filter_map(|m| {
                let meta: Value = serde_json::from_str(m.meta.as_deref()?).ok()?;
                meta["tool_call_id"].as_str().map(str::to_string)
            })
            .collect();
        Ok(meta["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                let id = c["id"].as_str()?;
                (!answered.contains(id)).then(|| {
                    (
                        id.to_string(),
                        c["function"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
            })
            .collect())
    }

    /// On daemon startup, runs left 'running' were interrupted by a crash (SPEC §9.5).
    pub fn mark_orphan_runs_interrupted(&self) -> Result<usize> {
        // In-flight invocations: outcome unknown, never re-executed (§9.5).
        let _ = self.ex(
            "UPDATE invocations SET status='interrupted', tag='interrupted' WHERE status='running'",
            [],
        );
        let _ = self.ex(
            "UPDATE subagents SET status='interrupted', updated_at=?1 WHERE status='running'",
            params![now()],
        );
        self.ex("UPDATE scripts SET status='interrupted',error='the daemon stopped while this script was running' WHERE status='running'", [])?;
        self.ex(
            "UPDATE runs SET status='interrupted', error='the daemon stopped while this Run was going', updated_at=?1 WHERE status='running'",
            params![now()],
        )
    }

    pub fn save_checkpoint(
        &self,
        session_id: &str,
        summary: &str,
        up_to_seq: i64,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        // Read metadata only, one row at a time: compaction must not reload
        // arbitrarily large historical Tool payloads or accumulate origins.
        let mut origins = std::collections::BTreeSet::new();
        if let Some((previous, _, _)) = self.latest_checkpoint_record(session_id)? {
            origins.extend(self.data_origins(session_id, &format!("checkpoint:{previous}"))?);
        }
        let mut statement = self.ps("SELECT meta FROM messages WHERE session_id=?1 AND seq<=?2 AND meta IS NOT NULL ORDER BY seq")?;
        let rows =
            statement.query_map(params![session_id, up_to_seq], |r| r.get::<_, String>(0))?;
        for row in rows {
            let meta: Value = serde_json::from_str(&row?).unwrap_or(Value::Null);
            let found: Vec<crate::provenance::Origin> =
                serde_json::from_value(meta["data_origins"].clone()).unwrap_or_default();
            for origin in found.into_iter().take(crate::provenance::MAX_ORIGINS) {
                if origins.len() < crate::provenance::MAX_ORIGINS {
                    origins.insert(origin);
                }
            }
        }
        for origin in origins {
            self.record_data_origin(session_id, &format!("checkpoint:{id}"), &origin)?;
        }
        self.ex(
            "INSERT INTO checkpoints(id,session_id,summary,up_to_seq,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![id, session_id, summary, up_to_seq, now()],
        )?;
        Ok(id)
    }
    pub fn latest_checkpoint(&self, session_id: &str) -> Result<Option<(String, i64)>> {
        Ok(self
            .latest_checkpoint_record(session_id)?
            .map(|(_, summary, up_to)| (summary, up_to)))
    }

    /// The immutable row ID identifies a summary generation. Transcript
    /// sequence numbers can be reused after editing or retrying a turn.
    pub(crate) fn latest_checkpoint_record(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, String, i64)>> {
        Ok(self
            .conn
            .query_row(LATEST_CHECKPOINT_SQL, params![session_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?)
    }

    // --- Tools (SPEC §7, §8) ---

    pub(crate) fn tool_info(&self, id: &str) -> Result<Option<ToolInfo>> {
        let mut stmt = self.ps("SELECT * FROM tools WHERE id=?1")?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok((
                r.get::<_, String>("id")?,
                r.get::<_, String>("name")?,
                r.get::<_, String>("description")?,
                r.get::<_, i64>("retired")?,
                r.get::<_, i64>("current_version")?,
                r.get::<_, i64>("needs_attention")?,
                r.get::<_, String>("created_at")?,
                r.get::<_, String>("updated_at")?,
                r.get::<_, String>("title")?,
                r.get::<_, String>("tags")?,
                r.get::<_, i64>("user_pinned")?,
                r.get::<_, i64>("disabled")?,
            ))
        })?;
        let Some(row) = rows.next() else {
            return Ok(None);
        };
        let (
            id,
            name,
            description,
            retired,
            current_version,
            needs_attention,
            created_at,
            updated_at,
            title,
            tags,
            pinned,
            disabled,
        ) = row?;
        drop(rows);
        drop(stmt);
        let (capabilities, memory_mb, author) = self
            .conn
            .query_row(
                "SELECT capabilities, memory_mb, author FROM tool_versions WHERE tool_id=?1 AND version=?2",
                params![id, current_version],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map(|(c, m, a)| {
                (
                    serde_json::from_str::<Vec<String>>(&c).unwrap_or_default(),
                    m as u32,
                    a,
                )
            })
            .unwrap_or_default();
        let (metrics, last_used_at) = self.tool_metrics_with_last_use(&id, None)?;
        let mut recent: Vec<char> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT status FROM invocations WHERE tool_id=?1 AND status IN ('success','failure')
                 ORDER BY created_at DESC, rowid DESC LIMIT 40",
            )?;
            let rows = stmt
                .query_map(params![id], |r| r.get::<_, String>(0))?
                .map(|s| s.map(|s| if s == "success" { 's' } else { 'f' }))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        recent.reverse();
        Ok(Some(ToolInfo {
            id,
            name,
            title,
            tags: serde_json::from_str(&tags).unwrap_or_default(),
            pinned: pinned != 0,
            disabled: disabled != 0,
            description: crate::descriptions::screen(&description).text,
            retired: retired != 0,
            current_version,
            capabilities,
            memory_mb,
            invocations: metrics.total,
            success_rate: metrics.success_rate,
            needs_attention: needs_attention != 0,
            created_at,
            updated_at,
            last_used_at,
            recent: recent.into_iter().collect(),
            author,
        }))
    }

    /// Compare the reviewed generation while holding the very write boundary
    /// that commits its replacement; smoke checks happen before this boundary.
    pub(crate) fn with_current_tool<T>(
        &self,
        id: &str,
        expected: i64,
        action: impl FnOnce(&Self) -> Result<T>,
    ) -> Result<T> {
        self.transaction(|st| {
            let actual = st
                .get_tool_current(id)?
                .map(|(info, _)| info.current_version);
            if actual != Some(expected) {
                return Err(anyhow::Error::new(
                    crate::descriptions::DescriptionConflict { expected, actual },
                ));
            }
            action(st)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_tool_version(
        &self,
        name: &str,
        description: &str,
        input_schema: &str,
        output_schema: &str,
        code: &str,
        caps: &Capabilities,
        memory_mb: u32,
        author: &str,
    ) -> Result<(ToolInfo, i64)> {
        self.create_tool_version_with_schema_policy(
            name,
            description,
            input_schema,
            output_schema,
            code,
            caps,
            memory_mb,
            author,
            crate::tool_compatibility::SchemaWritePolicy::Agent {
                allow_breaking: false,
            },
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn create_tool_version_with_schema_policy(
        &self,
        name: &str,
        description: &str,
        input_schema: &str,
        output_schema: &str,
        code: &str,
        caps: &Capabilities,
        memory_mb: u32,
        author: &str,
        policy: crate::tool_compatibility::SchemaWritePolicy,
    ) -> Result<(ToolInfo, i64)> {
        self.create_tool_version_with_schema_report(
            name,
            description,
            input_schema,
            output_schema,
            code,
            caps,
            memory_mb,
            author,
            policy,
        )
        .map(|(info, version, _)| (info, version))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn create_tool_version_with_schema_report(
        &self,
        name: &str,
        description: &str,
        input_schema: &str,
        output_schema: &str,
        code: &str,
        caps: &Capabilities,
        memory_mb: u32,
        author: &str,
        policy: crate::tool_compatibility::SchemaWritePolicy,
    ) -> Result<(ToolInfo, i64, crate::tool_compatibility::ToolSchemaReport)> {
        // Look-up and insert under one write lock: two connections (the
        // daemon and a CLI import) creating one name must serialise into
        // versions of one Tool, not race on the unique name (SPEC §16).
        if !self.conn.is_autocommit() {
            return self.create_tool_version_locked(
                name,
                description,
                input_schema,
                output_schema,
                code,
                caps,
                memory_mb,
                author,
                policy,
            );
        }
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let out = self.create_tool_version_locked(
            name,
            description,
            input_schema,
            output_schema,
            code,
            caps,
            memory_mb,
            author,
            policy,
        );
        match &out {
            Ok(_) => self.conn.execute_batch("COMMIT")?,
            Err(_) => {
                let _ = self.conn.execute_batch("ROLLBACK");
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn create_tool_version_locked(
        &self,
        name: &str,
        description: &str,
        input_schema: &str,
        output_schema: &str,
        code: &str,
        caps: &Capabilities,
        memory_mb: u32,
        author: &str,
        policy: crate::tool_compatibility::SchemaWritePolicy,
    ) -> Result<(ToolInfo, i64, crate::tool_compatibility::ToolSchemaReport)> {
        let mut compatibility = self.schema_report_for_name(name, input_schema, output_schema)?;
        if matches!(
            policy,
            crate::tool_compatibility::SchemaWritePolicy::Agent {
                allow_breaking: false
            }
        ) && compatibility.requires_acknowledgement()
        {
            return Err(crate::tool_compatibility::SchemaChangeBlocked(compatibility).into());
        }
        let before = self
            .get_tool_by_name(name)?
            .map(|(info, _)| info.description);
        let screened = crate::descriptions::screen(description);
        let raw_description = description;
        let description = screened.text.as_str();
        let lower = name.to_lowercase();
        let existing: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT id, retired FROM tools WHERE name_lower=?1",
                params![lower],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok();
        let t = now();
        let (tool_id, version) = match existing {
            Some((_, 1)) => {
                return Err(anyhow::anyhow!(
                    "a retired tool named '{name}' exists; restore or purge it first"
                ));
            }
            Some((tid, _)) => {
                let v: i64 = self.conn.query_row(
                    "SELECT COALESCE(MAX(version),0)+1 FROM tool_versions WHERE tool_id=?1",
                    params![tid],
                    |r| r.get(0),
                )?;
                (tid, v)
            }
            None => {
                let tid = uuid::Uuid::new_v4().to_string();
                self.ex(
                    "INSERT INTO tools(id,name,name_lower,description,current_version,created_at,updated_at) VALUES(?1,?2,?3,?4,0,?5,?5)",
                    params![tid, name, lower, description, t],
                )?;
                (tid, 1)
            }
        };
        self.ex(
            "INSERT INTO tool_versions(id,tool_id,version,input_schema,output_schema,code,capabilities,memory_mb,author,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![uuid::Uuid::new_v4().to_string(), tool_id, version, input_schema, output_schema, code, serde_json::to_string(&caps.to_specs())?, memory_mb, author, t],
        )?;
        self.ex(
            "UPDATE tools SET description=?2, current_version=?3, updated_at=?4 WHERE id=?1",
            params![tool_id, description, version, t],
        )?;
        if author != "builtin" {
            self.record_description_review(
                if author == "mcp" { "mcp" } else { "tool" },
                name,
                version,
                caps.mcp.first().map(String::as_str).unwrap_or(author),
                before.as_deref(),
                raw_description,
            )?;
        }
        compatibility.tool_id = tool_id.clone();
        Ok((
            self.tool_info(&tool_id)?.context("tool vanished")?,
            version,
            compatibility,
        ))
    }

    pub fn get_tool_by_name(&self, name: &str) -> Result<Option<(ToolInfo, ToolVersionInfo)>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM tools WHERE name_lower=?1",
                params![name.to_lowercase()],
                |r| r.get(0),
            )
            .ok();
        match id {
            Some(id) => self.get_tool_current(&id),
            None => Ok(None),
        }
    }

    pub fn get_tool_current(&self, id: &str) -> Result<Option<(ToolInfo, ToolVersionInfo)>> {
        let Some(info) = self.tool_info(id)? else {
            return Ok(None);
        };
        let v = self.conn.query_row(
            "SELECT version,author,created_at,capabilities,memory_mb,input_schema,output_schema,code FROM tool_versions WHERE tool_id=?1 AND version=?2",
            params![id, info.current_version],
            |r| {
                Ok(ToolVersionInfo {
                    version: r.get(0)?,
                    author: r.get(1)?,
                    created_at: r.get(2)?,
                    capabilities: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                    memory_mb: r.get::<_, i64>(4)? as u32,
                    input_schema: r.get(5)?,
                    output_schema: r.get(6)?,
                    code: r.get(7)?,
                })
            },
        )?;
        Ok(Some((info, v)))
    }

    pub(crate) fn current_input_schemas_for(
        &self,
        tool_ids: &[&str],
    ) -> Result<HashMap<String, (i64, String)>> {
        let mut schemas = HashMap::new();
        for chunk in tool_ids.chunks(128) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT t.id, v.version, v.input_schema FROM tools AS t
                 JOIN tool_versions AS v ON v.tool_id=t.id AND v.version=t.current_version
                 WHERE t.retired=0 AND t.disabled=0 AND t.id IN ({placeholders})"
            );
            let mut stmt = self.ps(&sql)?;
            let rows =
                stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
            for row in rows {
                let (id, version, schema) = row?;
                schemas.insert(id, (version, schema));
            }
        }
        Ok(schemas)
    }

    pub fn list_tools(&self, include_retired: bool) -> Result<Vec<ToolInfo>> {
        let ids: Vec<String> = {
            let sql = if include_retired {
                "SELECT id FROM tools ORDER BY name_lower"
            } else {
                "SELECT id FROM tools WHERE retired=0 ORDER BY name_lower"
            };
            let mut stmt = self.ps(sql)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(info) = self.tool_info(&id)? {
                out.push(info);
            }
        }
        Ok(out)
    }

    fn module_row(r: &rusqlite::Row) -> rusqlite::Result<ModuleInfo> {
        Ok(ModuleInfo {
            id: r.get(0)?,
            name: r.get(1)?,
            description: crate::descriptions::screen(&r.get::<_, String>(2)?).text,
            current_version: r.get(3)?,
            retired: r.get::<_, i64>(4)? != 0,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
        })
    }

    fn module_info(&self, id: &str) -> Result<Option<ModuleInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id,name,description,current_version,retired,created_at,updated_at FROM modules WHERE id=?1",
                params![id],
                Self::module_row,
            )
            .ok())
    }

    pub fn create_module_version(
        &self,
        name: &str,
        description: &str,
        code: &str,
        author: &str,
    ) -> Result<(ModuleInfo, i64)> {
        self.transaction(|st| st.create_module_version_locked(name, description, code, author))
    }

    fn create_module_version_locked(
        &self,
        name: &str,
        description: &str,
        code: &str,
        author: &str,
    ) -> Result<(ModuleInfo, i64)> {
        let before = self
            .get_module_by_name(name)?
            .map(|(info, _)| info.description);
        let screened = crate::descriptions::screen(description);
        let raw_description = description;
        let description = screened.text.as_str();
        let lower = name.to_lowercase();
        let existing: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT id, retired FROM modules WHERE name_lower=?1",
                params![lower],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .ok();
        let t = now();
        let (module_id, version) = match existing {
            Some((_, 1)) => {
                return Err(anyhow::anyhow!(
                    "a retired module named '{name}' exists; restore or purge it first"
                ))
            }
            Some((mid, _)) => {
                let v: i64 = self.conn.query_row(
                    "SELECT COALESCE(MAX(version),0)+1 FROM module_versions WHERE module_id=?1",
                    params![mid],
                    |r| r.get(0),
                )?;
                (mid, v)
            }
            None => {
                let mid = uuid::Uuid::new_v4().to_string();
                self.ex(
                    "INSERT INTO modules(id,name,name_lower,description,current_version,created_at,updated_at) VALUES(?1,?2,?3,?4,0,?5,?5)",
                    params![mid, name, lower, description, t],
                )?;
                (mid, 1)
            }
        };
        self.ex(
            "INSERT INTO module_versions(id,module_id,version,code,author,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![uuid::Uuid::new_v4().to_string(), module_id, version, code, author, t],
        )?;
        self.ex(
            "UPDATE modules SET description=?2, current_version=?3, updated_at=?4 WHERE id=?1",
            params![module_id, description, version, t],
        )?;
        self.record_description_review(
            "module",
            name,
            version,
            author,
            before.as_deref(),
            raw_description,
        )?;
        Ok((
            self.module_info(&module_id)?.context("module vanished")?,
            version,
        ))
    }

    pub fn get_module_by_name(&self, name: &str) -> Result<Option<(ModuleInfo, String)>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM modules WHERE name_lower=?1",
                params![name.to_lowercase()],
                |r| r.get(0),
            )
            .ok();
        let Some(id) = id else { return Ok(None) };
        let Some(info) = self.module_info(&id)? else {
            return Ok(None);
        };
        let code: String = self.conn.query_row(
            "SELECT code FROM module_versions WHERE module_id=?1 AND version=?2",
            params![id, info.current_version],
            |r| r.get(0),
        )?;
        Ok(Some((info, code)))
    }

    /// Every version of a module as (version, code, author, created_at).
    pub fn module_versions(&self, module_id: &str) -> Result<Vec<(i64, String, String, String)>> {
        let mut stmt = self.ps(
            "SELECT version, code, author, created_at FROM module_versions WHERE module_id=?1 ORDER BY version",
        )?;
        let rows = stmt
            .query_map(params![module_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Agent read projection: all metadata, only the selected code body.
    /// Unrequested code is intentionally absent; selected code still decodes
    /// as required text, preserving its conversion failures.
    pub(crate) fn module_versions_for_read(
        &self,
        module_id: &str,
        selected: i64,
    ) -> Result<Vec<(i64, Option<String>, String, String)>> {
        let mut stmt = self.ps("SELECT version, CASE WHEN version=?2 THEN code ELSE NULL END, author, created_at FROM module_versions WHERE module_id=?1 ORDER BY version")?;
        let rows = stmt
            .query_map(params![module_id, selected], |row| {
                let version: i64 = row.get(0)?;
                let code = if version == selected {
                    Some(row.get::<_, String>(1)?)
                } else {
                    None
                };
                Ok((version, code, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Active Tools whose current code requires the module (by name).
    pub fn module_dependents(&self, name: &str) -> Result<Vec<String>> {
        let lower = name.to_lowercase();
        let patterns = ["requiremodule", "modules.load"]
            .map(|function| ["\"", "'", "`"].map(|q| format!("{function}({q}{lower}{q}")));
        let mut stmt = self.ps("SELECT t.name, v.code FROM tools AS t
             LEFT JOIN tool_versions AS v ON v.tool_id=t.id AND v.version=t.current_version
             WHERE t.retired=0 ORDER BY t.name_lower")?;
        let tools = stmt.query_map([], |r| {
            let name: String = r.get(0)?;
            // Keep reporting a missing current version, even for a Tool
            // that would not have matched the module reference.
            let code: String = r
                .get::<_, Option<String>>(1)?
                .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
            Ok((name, code))
        })?;
        let mut out = Vec::new();
        for tool in tools {
            let (name, code) = tool?;
            let code = code.to_lowercase();
            if patterns
                .iter()
                .flatten()
                .any(|pattern| code.contains(pattern))
            {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// A module by id, or else by name.
    pub fn find_module(&self, id_or_name: &str) -> Result<Option<ModuleInfo>> {
        if let Some(m) = self.module_info(id_or_name)? {
            return Ok(Some(m));
        }
        Ok(self.get_module_by_name(id_or_name)?.map(|(m, _)| m))
    }

    /// Delete a module with all its versions.
    pub fn delete_module(&self, id: &str) -> Result<()> {
        self.ex(
            "DELETE FROM module_versions WHERE module_id=?1",
            params![id],
        )?;
        self.ex("DELETE FROM modules WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn list_modules(&self) -> Result<Vec<ModuleInfo>> {
        let ids: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM modules ORDER BY name_lower")?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut out = Vec::new();
        for id in ids {
            if let Some(info) = self.module_info(&id)? {
                out.push(info);
            }
        }
        Ok(out)
    }

    /// Current code of every live module, for the runtime's `requireModule`.
    /// Changes whenever any module is created, versioned, rolled back or
    /// retired: lets callers cache [`Store::list_module_code`].
    pub fn modules_fingerprint(&self) -> Result<String> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) || ':' || COALESCE(SUM(current_version),0) || ':' || COALESCE(SUM(retired),0) || ':' || COALESCE(MAX(updated_at),'') || ':' || (SELECT COUNT(*) FROM module_versions) FROM modules",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn list_module_code(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.ps(
            "SELECT m.id,m.name,m.description,m.current_version,m.retired,m.created_at,m.updated_at,v.code
             FROM modules m LEFT JOIN module_versions v ON v.module_id=m.id AND v.version=m.current_version
             ORDER BY m.name_lower",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            // The former id-list read propagated id conversion failures, while
            // module_info skipped unreadable metadata. Keep those distinct.
            let _: String = row.get(0)?;
            let Ok(info) = Self::module_row(row) else {
                continue;
            };
            if info.retired {
                continue;
            }
            // LEFT JOIN keeps a missing current version visible as an error,
            // rather than silently omitting an otherwise readable live module.
            if matches!(row.get_ref(7)?, rusqlite::types::ValueRef::Null) {
                return Err(rusqlite::Error::QueryReturnedNoRows.into());
            }
            out.push((info.name, row.get(7)?));
        }
        Ok(out)
    }

    fn proposal_row(r: &rusqlite::Row) -> rusqlite::Result<PromptProposalInfo> {
        Ok(PromptProposalInfo {
            id: r.get(0)?,
            kind: r.get(1)?,
            content: r.get(2)?,
            rationale: r.get(3)?,
            status: r.get(4)?,
            baseline_score: r.get(5)?,
            candidate_score: r.get(6)?,
            eval_total: r.get(7)?,
            est_tokens: r.get(8)?,
            created_at: r.get(9)?,
            applied_at: r.get(10)?,
        })
    }

    pub fn create_proposal(
        &self,
        kind: &str,
        content: &str,
        rationale: &str,
        est_tokens: i64,
    ) -> Result<PromptProposalInfo> {
        let id = uuid::Uuid::new_v4().to_string();
        self.ex(
            "INSERT INTO prompt_proposals(id,kind,content,rationale,status,est_tokens,created_at) VALUES(?1,?2,?3,?4,'proposed',?5,?6)",
            params![id, kind, content, rationale, est_tokens, now()],
        )?;
        self.get_proposal(&id)?.context("proposal vanished")
    }

    pub fn get_proposal(&self, id: &str) -> Result<Option<PromptProposalInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id,kind,content,rationale,status,baseline_score,candidate_score,eval_total,est_tokens,created_at,applied_at FROM prompt_proposals WHERE id=?1",
                params![id],
                Self::proposal_row,
            )
            .ok())
    }

    pub fn list_proposals(&self) -> Result<Vec<PromptProposalInfo>> {
        let mut stmt = self.ps(
            "SELECT id,kind,content,rationale,status,baseline_score,candidate_score,eval_total,est_tokens,created_at,applied_at FROM prompt_proposals ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], Self::proposal_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn applied_proposals(&self) -> Result<Vec<PromptProposalInfo>> {
        // Append guidance in application order. Legacy rows without an
        // application timestamp use their creation time and existing tie order.
        let mut stmt = self.ps(
            "SELECT id,kind,content,rationale,status,baseline_score,candidate_score,eval_total,est_tokens,created_at,applied_at FROM prompt_proposals WHERE status='applied' ORDER BY COALESCE(applied_at,created_at) ASC, rowid DESC",
        )?;
        let rows = stmt
            .query_map([], Self::proposal_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn update_proposal_eval(
        &self,
        id: &str,
        baseline: f64,
        candidate: f64,
        total: i64,
    ) -> Result<Option<PromptProposalInfo>> {
        self.ex(
            "UPDATE prompt_proposals SET status='measured', baseline_score=?2, candidate_score=?3, eval_total=?4 WHERE id=?1 AND status IN ('proposed','measured')",
            params![id, baseline, candidate, total],
        )?;
        self.get_proposal(id)
    }

    /// Status transitions: apply (only from measured), reject, roll back
    /// (only from applied). Returns the updated proposal if it applied.
    pub fn set_proposal_status(
        &self,
        id: &str,
        status: &str,
    ) -> Result<Option<PromptProposalInfo>> {
        let (from, applied) = match status {
            "applied" => ("status='measured'", true),
            "rejected" => ("status IN ('proposed','measured')", false),
            "rolled_back" => ("status='applied'", false),
            other => return Err(anyhow::anyhow!("unknown proposal status '{other}'")),
        };
        let changed = if applied {
            self.ex(
                &format!("UPDATE prompt_proposals SET status='applied', applied_at=?2 WHERE id=?1 AND {from}"),
                params![id, now()],
            )?
        } else {
            self.ex(
                &format!("UPDATE prompt_proposals SET status=?2 WHERE id=?1 AND {from}"),
                params![id, status],
            )?
        };
        if changed == 0 {
            return Ok(None);
        }
        let p = self.get_proposal(id)?;
        // A memory proposal lives in the memory store (SPEC §7.11).
        if let Some(p) = p.as_ref().filter(|p| p.kind == "memory") {
            match status {
                "applied" => {
                    let (m, _) = self.add_memory(&p.content, "global", "optimiser", None)?;
                    self.ex(
                        "UPDATE memories SET proposal_id=?2 WHERE id=?1",
                        params![m.id, p.id],
                    )?;
                }
                "rolled_back" => {
                    self.conn
                        .execute("DELETE FROM memories WHERE proposal_id=?1", params![p.id])?;
                }
                _ => {}
            }
        }
        Ok(p)
    }

    /// Effective system prompt: frozen bootstrap + applied guidance
    /// proposals as versioned sections (first applied first). Memory proposals
    /// are in the memory store instead.
    pub fn effective_system_prompt(&self, base: &str) -> Result<String> {
        let applied: Vec<_> = self
            .applied_proposals()?
            .into_iter()
            .filter(|p| p.kind != "memory")
            .collect();
        let mut out = base.to_string();
        for (i, p) in applied.iter().enumerate() {
            out.push_str(&format!(
                "\n\nLearned guidance (v{}): {}",
                i + 1,
                p.content.trim()
            ));
        }
        Ok(out)
    }

    pub fn record_model_outcome(&self, model: &str, ok: bool, elapsed_ms: i64) -> Result<()> {
        if model.trim().is_empty() || model.eq_ignore_ascii_case("auto") {
            return Ok(());
        }
        self.ex(
            "INSERT INTO model_stats(model,successes,failures,total_ms,updated_at) VALUES(?1,?2,?3,?4,?5) \
             ON CONFLICT(model) DO UPDATE SET successes=successes+excluded.successes, failures=failures+excluded.failures, total_ms=total_ms+excluded.total_ms, updated_at=excluded.updated_at",
            params![
                model,
                if ok { 1 } else { 0 },
                if ok { 0 } else { 1 },
                elapsed_ms.max(0),
                now()
            ],
        )?;
        self.ex(
            "INSERT INTO model_events(model, ok, ms, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![model, ok as i64, elapsed_ms.max(0), now()],
        )?;
        Ok(())
    }

    /// Model outcomes since `since` (from the event log, kept since v38).
    pub fn model_stats_since(&self, since: &str) -> Result<Vec<ModelStat>> {
        let mut stmt = self.ps("SELECT model, SUM(ok), SUM(ok=0), SUM(ms) FROM model_events
             WHERE created_at >= ?1 GROUP BY model ORDER BY model")?;
        let rows = stmt
            .query_map(params![since], |r| {
                Ok(ModelStat {
                    model: r.get(0)?,
                    successes: r.get(1)?,
                    failures: r.get(2)?,
                    total_ms: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Each Tool's calls and success rate since `since`: (tool id, calls,
    /// success rate over successes and failures).
    pub fn tool_metrics_since(&self, since: &str) -> Result<Vec<(String, i64, Option<f64>)>> {
        let mut stmt = self.ps(
            "SELECT tool_id, COUNT(*), SUM(status='success'), SUM(status='failure')
             FROM invocations WHERE created_at >= ?1 GROUP BY tool_id",
        )?;
        let rows = stmt
            .query_map(params![since], |r| {
                let (s, f): (i64, i64) = (r.get(2)?, r.get(3)?);
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    (s + f > 0).then(|| s as f64 / (s + f) as f64),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_model_stats(&self) -> Result<Vec<ModelStat>> {
        let mut stmt = self
            .conn
            .prepare("SELECT model,successes,failures,total_ms FROM model_stats ORDER BY model")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ModelStat {
                    model: r.get(0)?,
                    successes: r.get(1)?,
                    failures: r.get(2)?,
                    total_ms: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn tool_detail(&self, id: &str) -> Result<Option<formless_protocol::ToolDetail>> {
        let Some(tool) = self.tool_info(id)? else {
            return Ok(None);
        };
        let versions = {
            let mut stmt = self.ps(
                "SELECT version,author,created_at,capabilities,memory_mb,input_schema,output_schema,code FROM tool_versions WHERE tool_id=?1 ORDER BY version",
            )?;
            let rows = stmt
                .query_map(params![id], |r| {
                    Ok(ToolVersionInfo {
                        version: r.get(0)?,
                        author: r.get(1)?,
                        created_at: r.get(2)?,
                        capabilities: serde_json::from_str(&r.get::<_, String>(3)?)
                            .unwrap_or_default(),
                        memory_mb: r.get::<_, i64>(4)? as u32,
                        input_schema: r.get(5)?,
                        output_schema: r.get(6)?,
                        code: r.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let metrics = self.tool_metrics(id, None)?;
        let optimiser_attempts = self.list_attempts(id, 20)?;
        let version_metrics = versions
            .iter()
            .map(|v| Ok((v.version, self.tool_metrics(id, Some(v.version))?)))
            .collect::<Result<Vec<_>>>()?;
        let approval =
            self.get_approval(id)?
                .map(|(caps, memory_mb)| formless_protocol::ApprovalGrant {
                    capabilities: caps.to_specs(),
                    memory_mb,
                });
        // Each field capped: a detail view, not the full record.
        let cap = |s: String| -> String {
            if s.len() <= 20_000 {
                return s;
            }
            let mut end = 20_000;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}… (cut)", &s[..end])
        };
        let recent_calls = {
            let mut stmt = self.ps(
                "SELECT id, version, session_id, status, input, output, error, duration_ms, created_at
                 FROM invocations WHERE tool_id=?1 ORDER BY created_at DESC, rowid DESC LIMIT 20",
            )?;
            let rows = stmt
                .query_map(params![id], |r| {
                    Ok(formless_protocol::ToolCallInfo {
                        id: r.get(0)?,
                        version: r.get(1)?,
                        session_id: r.get(2)?,
                        status: r.get(3)?,
                        input: cap(r.get(4)?),
                        output: cap(r.get(5)?),
                        error: cap(r.get(6)?),
                        duration_ms: r.get(7)?,
                        created_at: r.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let attention =
            self.attention_reasons(&tool, &metrics, &optimiser_attempts, &recent_calls)?;
        let example = self.tool_example(id)?;
        Ok(Some(formless_protocol::ToolDetail {
            description_review: crate::descriptions::screen(&tool.description),
            description_reviews: self.description_reviews(Some("tool"), Some(&tool.name), 20)?,
            tool,
            versions,
            metrics,
            optimiser_attempts,
            version_metrics,
            approval,
            recent_calls,
            attention,
            example,
            listing_tokens: 0,
            uses: Vec::new(),
            modules: Vec::new(),
            used_by: Vec::new(),
        }))
    }

    /// The latest successful Invocation of a Tool, as its example call
    /// (inputs and outputs are redacted when recorded).
    pub fn tool_example(&self, id: &str) -> Result<Option<formless_protocol::ToolExample>> {
        use rusqlite::OptionalExtension;
        const MAX: usize = 4_000;
        let row = self
            .conn
            .query_row(
                "SELECT version, input, output, created_at FROM invocations
                 WHERE tool_id=?1 AND status='success'
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![id],
                |r| {
                    Ok(formless_protocol::ToolExample {
                        version: r.get(0)?,
                        input: r.get(1)?,
                        output: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row.map(|mut ex| {
            for field in [&mut ex.input, &mut ex.output] {
                if field.len() > MAX {
                    let mut end = MAX;
                    while !field.is_char_boundary(end) {
                        end -= 1;
                    }
                    field.truncate(end);
                    field.push_str("… (cut)");
                }
            }
            ex
        }))
    }

    /// Why a Tool needs attention: the optimiser giving up (what sets the
    /// flag), and live signals: a low success rate, a failure streak, a
    /// spike of invalid inputs, or a recent rollback.
    fn attention_reasons(
        &self,
        tool: &ToolInfo,
        m: &ToolMetricsInfo,
        attempts: &[formless_protocol::OptimiserAttemptInfo],
        calls: &[formless_protocol::ToolCallInfo],
    ) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if tool.needs_attention {
            let (_, consec) = self.get_opt_state(&tool.id)?;
            let last = attempts
                .iter()
                .find(|a| a.status == "discarded" || a.status == "failed")
                .map(|a| format!(" (last: {})", a.detail))
                .unwrap_or_default();
            out.push(format!(
                "the optimiser gave up after {consec} failed candidate fixes{last}"
            ));
        }
        if let Some(rate) = m.success_rate.filter(|r| *r < 0.5 && m.total >= 5) {
            out.push(format!(
                "only {:.0}% of {} calls succeed",
                rate * 100.0,
                m.total
            ));
        }
        let streak = tool.recent.chars().rev().take_while(|c| *c == 'f').count();
        if streak >= 3 {
            let error = calls
                .iter()
                .find(|c| c.status == "failure" && !c.error.is_empty())
                .map(|c| {
                    let e: String = c
                        .error
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect();
                    format!(": {e}")
                })
                .unwrap_or_default();
            out.push(format!("the last {streak} calls failed{error}"));
        }
        if m.total >= 5 && m.validation_failures * 5 >= m.total {
            out.push(format!(
                "{} of {} calls had invalid input (the schema may not match how the agent calls it)",
                m.validation_failures, m.total
            ));
        }
        let week_ago = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();
        let rollback: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT detail, created_at FROM audit WHERE action='tool.rollback'
                 AND detail LIKE ?1 || ' v%' AND created_at >= ?2
                 ORDER BY created_at DESC LIMIT 1",
                params![tool.name, week_ago],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some((detail, at)) = rollback {
            let version = detail.rsplit(' ').next().unwrap_or_default();
            out.push(format!(
                "rolled back to {version} on {}",
                at.get(..10).unwrap_or(&at)
            ));
        }
        Ok(out)
    }

    /// Mark Tools as loaded into a Session's function list (SPEC §7.1).
    /// `source` is `found`, `created` or `filler` (the most-used Tools
    /// frozen in when the Session first listed a large registry).
    pub fn load_session_tools(
        &self,
        session_id: &str,
        tool_ids: &[String],
        source: &str,
    ) -> Result<()> {
        for id in tool_ids {
            self.ex(
                // Loading again revives a Tool the Session had unloaded.
                "INSERT INTO session_tools(session_id, tool_id, source, loaded_at) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(session_id, tool_id) DO UPDATE SET source=excluded.source, loaded_at=excluded.loaded_at, unload=0 \
                 WHERE session_tools.source='unloaded'",
                params![session_id, id, source, now()],
            )?;
        }
        Ok(())
    }

    /// Has this Session frozen its most-used filler Tools yet?
    /// Mark Tools to drop from a Session's listing at its next compaction.
    /// Returns how many were marked.
    pub fn request_unload(&self, session_id: &str, tool_ids: &[String]) -> Result<usize> {
        let mut n = 0;
        for id in tool_ids {
            self.ex(
                "INSERT OR IGNORE INTO session_tools(session_id, tool_id, source, loaded_at) VALUES (?1, ?2, 'called', ?3)",
                params![session_id, id, now()],
            )?;
            n += self.ex(
                "UPDATE session_tools SET unload=1 WHERE session_id=?1 AND tool_id=?2 AND source!='created'",
                params![session_id, id],
            )?;
        }
        Ok(n)
    }

    /// At a compaction: apply requested unloads, then keep at most
    /// `max_loaded` listed Tools (oldest found/filler/called first; a
    /// Session's own creations stay). Returns how many were unloaded.
    pub fn apply_unloads(&self, session_id: &str, max_loaded: usize) -> Result<usize> {
        let mut n = self.ex(
            "UPDATE session_tools SET source='unloaded', unload=0, pinned_def=NULL, pinned_version=NULL WHERE session_id=?1 AND unload=1",
            params![session_id],
        )?;
        let loaded: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM session_tools WHERE session_id=?1 AND source!='unloaded'",
            params![session_id],
            |r| r.get(0),
        )?;
        let excess = (loaded as usize).saturating_sub(max_loaded.max(1));
        if excess > 0 {
            n += self.ex(
                "UPDATE session_tools SET source='unloaded', pinned_def=NULL, pinned_version=NULL WHERE rowid IN (
                   SELECT rowid FROM session_tools WHERE session_id=?1 AND source NOT IN ('unloaded','created')
                   ORDER BY loaded_at LIMIT ?2)",
                params![session_id, excess as i64],
            )?;
        }
        Ok(n)
    }

    /// Forget the frozen most-used "filler" picks, so the next listing
    /// chooses them again from current usage (called at compaction).
    pub fn reset_filler(&self, session_id: &str) -> Result<()> {
        self.ex(
            "DELETE FROM session_tools WHERE session_id=?1 AND source='filler'",
            params![session_id],
        )?;
        Ok(())
    }

    pub fn session_has_filler(&self, session_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_tools WHERE session_id=?1 AND source='filler')",
            params![session_id],
            |r| r.get(0),
        )?)
    }

    /// Tools a Session has loaded: found or created there, or called there.
    /// The system prompt a Session runs with (snapshot), if taken yet.
    pub fn session_system_prompt(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT system_prompt FROM sessions WHERE id=?1",
                params![session_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten())
    }

    pub fn set_session_system_prompt(&self, session_id: &str, prompt: &str) -> Result<()> {
        self.ex(
            "UPDATE sessions SET system_prompt=?2 WHERE id=?1",
            params![session_id, prompt],
        )?;
        Ok(())
    }

    /// (version, definition JSON) a Session listed for a Tool, if pinned.
    pub fn pinned_definition(
        &self,
        session_id: &str,
        tool_id: &str,
    ) -> Result<Option<(i64, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT pinned_version, pinned_def FROM session_tools WHERE session_id=?1 AND tool_id=?2 AND pinned_def IS NOT NULL",
                params![session_id, tool_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok())
    }

    /// Per-listing active snapshots for only the shown IDs. Decode errors in
    /// an individual pin remain missing, like pinned_definition. SQL/iteration
    /// errors propagate so the caller can retain the legacy per-Tool path.
    pub(crate) fn pinned_definitions_for(
        &self,
        session_id: &str,
        tool_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, (i64, String)>> {
        let mut pins = std::collections::HashMap::new();
        // Stay below SQLite's conservative parameter limit; no query for [].
        for chunk in tool_ids.chunks(128) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT tool_id, pinned_version, pinned_def FROM session_tools
                 WHERE session_id=? AND tool_id IN ({placeholders}) AND pinned_def IS NOT NULL"
            );
            let mut stmt = self.ps(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(
                    std::iter::once(session_id).chain(chunk.iter().copied()),
                ),
                |row| {
                    let id: String = row.get(0)?;
                    let pin = row
                        .get::<_, i64>(1)
                        .and_then(|version| {
                            row.get::<_, String>(2)
                                .map(|definition| (version, definition))
                        })
                        .ok();
                    Ok((id, pin))
                },
            )?;
            for row in rows {
                let (id, pin) = row?;
                if let Some(pin) = pin {
                    pins.insert(id, pin);
                }
            }
        }
        Ok(pins)
    }

    /// Pinned definitions of a Session's Tools that are no longer active
    /// (retired, e.g. an MCP server dropped them), as (tool id, definition).
    pub fn pinned_inactive(&self, session_id: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.ps(
            "SELECT st.tool_id, st.pinned_def FROM session_tools st JOIN tools t ON t.id = st.tool_id
             WHERE st.session_id=?1 AND st.pinned_def IS NOT NULL AND st.source!='unloaded' AND (t.retired=1 OR t.disabled=1)",
        )?;
        let rows = stmt
            .query_map(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn pin_definition(
        &self,
        session_id: &str,
        tool_id: &str,
        version: i64,
        def: &str,
    ) -> Result<()> {
        self.ex(
            "INSERT OR IGNORE INTO session_tools(session_id, tool_id, source, loaded_at) VALUES (?1, ?2, 'called', ?3)",
            params![session_id, tool_id, now()],
        )?;
        self.ex(
            "UPDATE session_tools SET pinned_version=?3, pinned_def=?4 WHERE session_id=?1 AND tool_id=?2",
            params![session_id, tool_id, version, def],
        )?;
        Ok(())
    }

    /// Drop pins so the next listing shows current definitions: the given
    /// Tools, or all of the Session's (`None`). Returns (tool id, version
    /// that was pinned) for each dropped pin.
    pub fn unpin_definitions(
        &self,
        session_id: &str,
        tool_ids: Option<&[String]>,
    ) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.ps(
            "SELECT tool_id, pinned_version FROM session_tools WHERE session_id=?1 AND pinned_def IS NOT NULL",
        )?;
        let pinned: Vec<(String, i64)> = stmt
            .query_map(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let dropped: Vec<(String, i64)> = pinned
            .into_iter()
            .filter(|(id, _)| tool_ids.is_none_or(|ids| ids.contains(id)))
            .collect();
        for (id, _) in &dropped {
            self.ex(
                "UPDATE session_tools SET pinned_version=NULL, pinned_def=NULL WHERE session_id=?1 AND tool_id=?2",
                params![session_id, id],
            )?;
        }
        Ok(dropped)
    }

    pub fn session_tool_ids(&self, session_id: &str) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.ps(
            "SELECT tool_id FROM session_tools WHERE session_id=?1 AND source!='unloaded'
             UNION SELECT DISTINCT tool_id FROM invocations WHERE session_id=?1
               AND tool_id NOT IN (SELECT tool_id FROM session_tools WHERE session_id=?1 AND source='unloaded')",
        )?;
        let rows = stmt
            .query_map(params![session_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Most recent failed Invocations of a Tool: (version, tag, input, error).
    pub fn recent_failures(
        &self,
        tool_id: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, String, String)>> {
        let mut stmt =
            self.ps("SELECT version, tag, input, error FROM invocations
             WHERE tool_id=?1 AND status='failure' ORDER BY created_at DESC, rowid DESC LIMIT ?2")?;
        let rows = stmt
            .query_map(params![tool_id, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// A Tool's human title (SPEC §7.2); empty clears it.
    pub fn set_tool_title(&self, id: &str, title: &str) -> Result<Option<ToolInfo>> {
        self.ex("UPDATE tools SET title=?2 WHERE id=?1", params![id, title])?;
        self.tool_info(id)
    }

    /// Switch a Tool off or on for the agent (SPEC §7.2).
    pub fn set_tool_disabled(&self, id: &str, disabled: bool) -> Result<Option<ToolInfo>> {
        self.ex(
            "UPDATE tools SET disabled=?2 WHERE id=?1",
            params![id, disabled as i64],
        )?;
        self.tool_info(id)
    }

    /// Pin a Tool for the agent, or unpin it (SPEC §7.1).
    pub fn set_tool_pinned(&self, id: &str, pinned: bool) -> Result<Option<ToolInfo>> {
        self.ex(
            "UPDATE tools SET user_pinned=?2 WHERE id=?1",
            params![id, pinned as i64],
        )?;
        self.tool_info(id)
    }

    /// A Tool's tags (SPEC §7.2), already normalised.
    pub fn set_tool_tags(&self, id: &str, tags: &[String]) -> Result<Option<ToolInfo>> {
        self.ex(
            "UPDATE tools SET tags=?2 WHERE id=?1",
            params![id, serde_json::to_string(tags)?],
        )?;
        self.tool_info(id)
    }

    pub fn set_tool_retired(&self, id: &str, retired: bool) -> Result<Option<ToolInfo>> {
        self.ex(
            "UPDATE tools SET retired=?2, updated_at=?3 WHERE id=?1",
            params![id, retired as i64, now()],
        )?;
        self.tool_info(id)
    }

    pub fn kv_get(&self, namespace: &str, key: &str) -> Result<Option<Value>> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM tool_kv WHERE namespace=?1 AND key=?2",
                params![namespace, key],
                |r| r.get(0),
            )
            .ok();
        Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
    }

    /// Store a JSON value, within the size caps.
    pub fn kv_set(&self, namespace: &str, key: &str, value: &Value) -> Result<()> {
        if key.is_empty() || key.len() > 256 {
            anyhow::bail!("store keys must be 1-256 characters");
        }
        let text = value.to_string();
        if text.len() > KV_MAX_VALUE {
            anyhow::bail!("store value is larger than {} KB", KV_MAX_VALUE / 1024);
        }
        let used: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(length(value)+length(key)),0) FROM tool_kv WHERE namespace=?1 AND key!=?2",
            params![namespace, key],
            |r| r.get(0),
        )?;
        if used + (text.len() + key.len()) as i64 > KV_MAX_NAMESPACE {
            anyhow::bail!(
                "store namespace is full ({} MB); delete keys first",
                KV_MAX_NAMESPACE / 1024 / 1024
            );
        }
        self.ex(
            "INSERT INTO tool_kv(namespace,key,value,updated_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(namespace,key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![namespace, key, text, now()],
        )?;
        Ok(())
    }

    pub fn kv_delete(&self, namespace: &str, key: &str) -> Result<bool> {
        Ok(self.ex(
            "DELETE FROM tool_kv WHERE namespace=?1 AND key=?2",
            params![namespace, key],
        )? > 0)
    }

    pub fn kv_contains(&self, namespace: &str, key: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tool_kv WHERE namespace=?1 AND key=?2)",
            params![namespace, key],
            |row| row.get(0),
        )?)
    }

    /// Keys under the prefix, sorted (at most 1,000).
    pub fn kv_list(&self, namespace: &str, prefix: &str) -> Result<Vec<String>> {
        self.kv_list_page(namespace, prefix, None)
    }

    pub(crate) fn kv_list_page(
        &self,
        namespace: &str,
        prefix: &str,
        after: Option<&str>,
    ) -> Result<Vec<String>> {
        let sql = if after.is_some() {
            "SELECT key FROM tool_kv WHERE namespace=?1 AND key>?3 AND substr(key,1,length(?2))=?2 ORDER BY key LIMIT 1000"
        } else {
            "SELECT key FROM tool_kv WHERE namespace=?1 AND substr(key,1,length(?2))=?2 ORDER BY key LIMIT 1000"
        };
        let mut stmt = self.ps(sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter([namespace, prefix].into_iter().chain(after)),
                |r| r.get(0),
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn purge_tool(&self, id: &str) -> Result<bool> {
        // The Tool's own store goes with it.
        self.ex(
            "DELETE FROM tool_kv WHERE namespace=?1",
            params![format!("tool:{id}")],
        )?;
        Ok(self
            .conn
            .execute("DELETE FROM tools WHERE id=?1", params![id])?
            > 0)
    }

    pub fn rollback_tool(&self, id: &str, version: i64) -> Result<Option<ToolInfo>> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM tool_versions WHERE tool_id=?1 AND version=?2",
                params![id, version],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .unwrap_or(false);
        if !exists {
            return Ok(None);
        }
        self.ex(
            "UPDATE tools SET current_version=?2, updated_at=?3 WHERE id=?1",
            params![id, version, now()],
        )?;
        self.tool_info(id)
    }

    /// Create a `running` invocation row at execution start so a daemon
    /// crash can mark it interrupted (SPEC §9.5). Returns invocation id.
    pub fn start_invocation(
        &self,
        tool_id: &str,
        version: i64,
        session_id: Option<&str>,
        run_id: Option<&str>,
        parent_id: Option<&str>,
        input: &str,
    ) -> Result<String> {
        self.start_invocation_with_id(
            &uuid::Uuid::new_v4().to_string(),
            tool_id,
            version,
            session_id,
            run_id,
            parent_id,
            input,
        )
    }

    /// The daemon allocates identity before a threshold-zero pending handoff, so
    /// lifecycle events and the eventual running row use the same unique ID.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_invocation_with_id(
        &self,
        id: &str,
        tool_id: &str,
        version: i64,
        session_id: Option<&str>,
        run_id: Option<&str>,
        parent_id: Option<&str>,
        input: &str,
    ) -> Result<String> {
        uuid::Uuid::parse_str(id)?;
        let tx = self.conn.unchecked_transaction()?;
        if let Some(run) = run_id {
            let _ = tx.execute(
                "UPDATE invocations SET retried=1 WHERE rowid=(SELECT MAX(rowid) FROM invocations WHERE tool_id=?1 AND run_id=?2 AND status='success')",
                params![tool_id, run],
            );
        }
        tx.execute(
            "INSERT INTO invocations(id,tool_id,version,session_id,run_id,parent_id,status,tag,input,output,error,duration_ms,created_at) VALUES(?1,?2,?3,?4,?5,?6,'running','',?7,'','',0,?8)",
            params![id, tool_id, version, session_id, run_id, parent_id, input, now()],
        )?;
        tx.commit()?;
        Ok(id.to_owned())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn finish_invocation(
        &self,
        id: &str,
        tool_id: &str,
        status: &str,
        tag: &str,
        output: &str,
        error: &str,
        duration_ms: i64,
        retention: usize,
    ) -> Result<()> {
        // Stored text goes through the one secret scanner (#709).
        let (output, error) = (
            crate::secrets::redact(output).0,
            crate::secrets::redact(error).0,
        );
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE invocations SET status=?2, tag=?3, output=?4, error=?5, duration_ms=?6 WHERE id=?1",
            params![id, status, tag, output, error, duration_ms],
        )?;
        prune_invocations(&tx, tool_id, retention)?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_invocation_tag(&self, id: &str, tag: &str) -> Result<()> {
        self.ex(
            "UPDATE invocations SET tag=?2 WHERE id=?1",
            params![id, tag],
        )?;
        Ok(())
    }

    pub fn invocation_status(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT status FROM invocations WHERE id=?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .ok())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_invocation(
        &self,
        tool_id: &str,
        version: i64,
        session_id: Option<&str>,
        run_id: Option<&str>,
        parent_id: Option<&str>,
        status: &str,
        tag: &str,
        input: &str,
        output: &str,
        error: &str,
        duration_ms: i64,
        retention: usize,
    ) -> Result<String> {
        // Retry signal (SPEC §8.1): a new invocation of the same Tool in the
        // same Run immediately after a success marks that success retried.
        let tx = self.conn.unchecked_transaction()?;
        if let Some(run) = run_id {
            let _ = tx.execute(
                "UPDATE invocations SET retried=1 WHERE rowid=(SELECT MAX(rowid) FROM invocations WHERE tool_id=?1 AND run_id=?2 AND status='success')",
                params![tool_id, run],
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        // Stored text goes through the one secret scanner (#709).
        let (input, output, error) = (
            crate::secrets::redact(input).0,
            crate::secrets::redact(output).0,
            crate::secrets::redact(error).0,
        );
        tx.execute(
            "INSERT INTO invocations(id,tool_id,version,session_id,run_id,parent_id,status,tag,input,output,error,duration_ms,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![id, tool_id, version, session_id, run_id, parent_id, status, tag, input, output, error, duration_ms, now()],
        )?;
        prune_invocations(&tx, tool_id, retention)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn tool_metrics(&self, tool_id: &str, version: Option<i64>) -> Result<ToolMetricsInfo> {
        self.tool_metrics_with_last_use(tool_id, version)
            .map(|(metrics, _)| metrics)
    }

    fn tool_metrics_with_last_use(
        &self,
        tool_id: &str,
        version: Option<i64>,
    ) -> Result<(ToolMetricsInfo, Option<String>)> {
        // Reuse the existing per-Tool counts scan instead of making an extra
        // query for every Tool in a list. Match tool_last_used's retained MAX.
        let (total, success, failure, denied, cancelled, validation, retried, last_used_at) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(status='success'),0), COALESCE(SUM(status='failure'),0),
                    COALESCE(SUM(status='denied'),0), COALESCE(SUM(status='cancelled'),0),
                    COALESCE(SUM(tag='validation'),0), COALESCE(SUM(retried),0), MAX(created_at)
             FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2)",
            params![tool_id, version],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get::<_, i64>(6)?, r.get::<_, Option<String>>(7)?)),
        )?;
        // Cost per Success (§8.1 metric 3): everything spent, failures
        // included, over the successes it bought.
        let (all_ms, all_tokens): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(duration_ms),0), COALESCE(SUM(tokens),0) FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2)",
            params![tool_id, version],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let per_success = |v: i64| (success > 0).then(|| v as f64 / success as f64);
        // The Tool's own model use, per call (§10.4).
        let (model_tokens, model_cost, unpriced): (i64, f64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(model_tokens),0), COALESCE(SUM(model_cost),0), COALESCE(MAX(model_unpriced),0) \
             FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2)",
            params![tool_id, version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let model_used = model_tokens > 0 && total > 0;
        let mut durations: Vec<i64> = {
            let mut stmt = self.ps("SELECT duration_ms FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2) AND status='success' ORDER BY duration_ms")?;
            let rows = stmt
                .query_map(params![tool_id, version], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        durations.sort_unstable();
        let pct = |p: f64| -> Option<f64> {
            if durations.is_empty() {
                None
            } else {
                let idx = ((durations.len() as f64 - 1.0) * p).round() as usize;
                Some(durations[idx] as f64)
            }
        };
        let sessions: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT session_id) FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2)",
            params![tool_id, version],
            |r| r.get(0),
        )?;
        let denom = success + failure;
        let metrics = ToolMetricsInfo {
            total,
            success,
            failure,
            denied,
            cancelled,
            validation_failures: validation,
            success_rate: if denom > 0 {
                Some(success as f64 / denom as f64)
            } else {
                None
            },
            retry_rate: if success > 0 {
                Some(retried as f64 / success as f64)
            } else {
                None
            },
            p50_ms: pct(0.50),
            p95_ms: pct(0.95),
            sessions,
            ms_per_success: per_success(all_ms),
            tokens_per_success: per_success(all_tokens),
            model_tokens_per_call: model_used.then(|| model_tokens as f64 / total as f64),
            model_cost_per_call: (model_used && unpriced == 0).then(|| model_cost / total as f64),
        };
        Ok((metrics, last_used_at))
    }

    /// Remove one Capability spec from a Tool's grant (or the whole grant
    /// with `None`). Returns whether anything was revoked.
    pub fn revoke_approval(&self, tool_id: &str, spec: Option<&str>) -> Result<bool> {
        let Some(spec) = spec else {
            return Ok(self
                .conn
                .execute("DELETE FROM approvals WHERE tool_id=?1", params![tool_id])?
                > 0);
        };
        let Some((caps, mem)) = self.get_approval(tool_id)? else {
            return Ok(false);
        };
        let specs = caps.to_specs();
        if !specs.iter().any(|s| s == spec) {
            return Ok(false);
        }
        let rest: Vec<String> = specs.into_iter().filter(|s| s != spec).collect();
        if rest.is_empty() {
            self.conn
                .execute("DELETE FROM approvals WHERE tool_id=?1", params![tool_id])?;
        } else {
            let caps = Capabilities::parse(&rest).map_err(|e| anyhow::anyhow!(e))?;
            self.set_approval(tool_id, &caps, mem)?;
        }
        Ok(true)
    }

    pub fn get_approval(&self, tool_id: &str) -> Result<Option<(Capabilities, u32)>> {
        let row = self.conn.query_row(
            "SELECT capabilities, memory_mb FROM approvals WHERE tool_id=?1",
            params![tool_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        );
        match row {
            Ok((specs, mem)) => {
                let specs: Vec<String> = serde_json::from_str(&specs).unwrap_or_default();
                Ok(Some((
                    Capabilities::parse(&specs).unwrap_or_default(),
                    mem as u32,
                )))
            }
            Err(_) => Ok(None),
        }
    }

    /// Store a grant and the client that answered it.
    pub fn set_approval_by(
        &self,
        tool_id: &str,
        caps: &Capabilities,
        memory_mb: u32,
        by: &str,
    ) -> Result<()> {
        self.set_approval(tool_id, caps, memory_mb)?;
        self.ex(
            "UPDATE approvals SET granted_by=?2 WHERE tool_id=?1",
            params![tool_id, by],
        )?;
        Ok(())
    }

    /// Every stored grant: (tool id, tool name, Capabilities, approved at,
    /// granted by, last Invocation).
    #[allow(clippy::type_complexity)]
    pub fn list_approvals(
        &self,
    ) -> Result<Vec<(String, String, Vec<String>, String, String, Option<String>)>> {
        let mut stmt = self.ps(
            "SELECT a.tool_id, t.name, a.capabilities, a.approved_at, a.granted_by,
                    (SELECT MAX(created_at) FROM invocations i WHERE i.tool_id=a.tool_id)
             FROM approvals a JOIN tools t ON t.id=a.tool_id ORDER BY t.name_lower",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    serde_json::from_str::<Vec<String>>(&r.get::<_, String>(2)?)
                        .unwrap_or_default(),
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// A Tool's latest Invocation time.
    pub fn tool_last_invocation(&self, tool_id: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row(
            "SELECT MAX(created_at) FROM invocations WHERE tool_id=?1",
            params![tool_id],
            |r| r.get(0),
        )?)
    }

    /// Store a grant for the Tool's current version: it holds only while
    /// that code runs (#704).
    pub fn set_approval(&self, tool_id: &str, caps: &Capabilities, memory_mb: u32) -> Result<()> {
        let hash = self
            .get_tool_current(tool_id)?
            .map(|(_, v)| crate::daemon::code_hash(&v));
        self.ex(
            "INSERT INTO approvals(tool_id,capabilities,memory_mb,approved_at,code_hash) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(tool_id) DO UPDATE SET capabilities=excluded.capabilities, memory_mb=excluded.memory_mb, approved_at=excluded.approved_at, code_hash=excluded.code_hash",
            params![tool_id, serde_json::to_string(&caps.to_specs())?, memory_mb, now(), hash],
        )?;
        Ok(())
    }

    /// The code fingerprint a stored grant was given for (None: a grant
    /// from before fingerprints, or no grant).
    pub fn approval_code_hash(&self, tool_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT code_hash FROM approvals WHERE tool_id=?1",
                params![tool_id],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten())
    }

    /// Record the fingerprint of a grant that predates them.
    pub fn set_approval_code_hash(&self, tool_id: &str, hash: &str) -> Result<()> {
        self.ex(
            "UPDATE approvals SET code_hash=?2 WHERE tool_id=?1",
            params![tool_id, hash],
        )?;
        Ok(())
    }

    pub fn next_skill_version(&self, name: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(v.version),0)+1 FROM skill_versions v JOIN skills s ON s.id=v.skill_id WHERE s.name=?1",
            params![name],
            |r| r.get(0),
        )?)
    }

    /// Record an installed skill version as current. A new install (or new
    /// version) is disabled until the user enables it.
    #[allow(clippy::too_many_arguments)]
    pub fn add_skill_version(
        &self,
        name: &str,
        description: &str,
        version: i64,
        path: &str,
        source: &str,
        author: &str,
        has_scripts: bool,
    ) -> Result<formless_protocol::SkillInfo> {
        self.transaction(|st| {
            st.add_skill_version_locked(
                name,
                description,
                version,
                path,
                source,
                author,
                has_scripts,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn add_skill_version_locked(
        &self,
        name: &str,
        description: &str,
        version: i64,
        path: &str,
        source: &str,
        author: &str,
        has_scripts: bool,
    ) -> Result<formless_protocol::SkillInfo> {
        let before = self.get_skill(name)?.map(|info| info.description);
        let screened = crate::descriptions::screen(description);
        let raw_description = description;
        let description = screened.text.as_str();
        let t = now();
        self.ex(
            "INSERT INTO skills(id,name,description,current_version,enabled,source,author,has_scripts,created_at,updated_at)
             VALUES(?1,?2,?3,?4,0,?5,?6,?7,?8,?8)
             ON CONFLICT(name) DO UPDATE SET description=excluded.description, current_version=excluded.current_version,
               enabled=0, source=excluded.source, author=excluded.author, has_scripts=excluded.has_scripts,
               retired=0, retire_reason=NULL, updated_at=excluded.updated_at",
            params![uuid::Uuid::new_v4().to_string(), name, description, version, source, author, has_scripts, t],
        )?;
        self.ex(
            "INSERT OR REPLACE INTO skill_versions(skill_id,version,path,created_at) SELECT id,?2,?3,?4 FROM skills WHERE name=?1",
            params![name, version, path, t],
        )?;
        self.record_description_review(
            "skill",
            name,
            version,
            source,
            before.as_deref(),
            raw_description,
        )?;
        self.get_skill(name)?.context("skill vanished")
    }

    fn row_skill(r: &rusqlite::Row) -> rusqlite::Result<formless_protocol::SkillInfo> {
        Ok(formless_protocol::SkillInfo {
            description_review: crate::descriptions::screen(&r.get::<_, String>("description")?),
            description_reviews: Vec::new(),
            name: r.get("name")?,
            description: crate::descriptions::screen(&r.get::<_, String>("description")?).text,
            version: r.get("current_version")?,
            enabled: r.get::<_, i64>("enabled")? != 0,
            source: r.get("source")?,
            author: r.get("author")?,
            has_scripts: r.get::<_, i64>("has_scripts")? != 0,
            retired: r.get::<_, i64>("retired")? != 0,
            loads: r.get("loads")?,
            runs: r.get("runs")?,
            runs_completed: r.get("runs_completed")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            retire_reason: r.get("retire_reason")?,
        })
    }

    /// Avoid unused history aggregates; malformed omitted fields are not decoded.
    pub(crate) fn skill_summaries(&self) -> Result<Vec<SkillSummary>> {
        let mut stmt =
            self.ps("SELECT name,description,enabled,retired FROM skills ORDER BY name")?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SkillSummary {
                    name: row.get(0)?,
                    description: crate::descriptions::screen(&row.get::<_, String>(1)?).text,
                    enabled: row.get::<_, i64>(2)? != 0,
                    retired: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Export still reads each Skill through get_skill; only enumeration is lean.
    pub(crate) fn skill_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.ps("SELECT name FROM skills ORDER BY name")?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn list_skills(&self) -> Result<Vec<formless_protocol::SkillInfo>> {
        let mut stmt = self.ps(&format!(
            "SELECT {SKILL_COLS} FROM skills s ORDER BY s.name"
        ))?;
        let rows = stmt
            .query_map([], Self::row_skill)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn get_skill(&self, name: &str) -> Result<Option<formless_protocol::SkillInfo>> {
        let mut info = self
            .conn
            .query_row(
                &format!("SELECT {SKILL_COLS} FROM skills s WHERE s.name=?1"),
                params![name],
                Self::row_skill,
            )
            .optional()?;
        if let Some(info) = &mut info {
            info.description_reviews = self.description_reviews(Some("skill"), Some(name), 20)?;
            if let Some(review) = info.description_reviews.iter().find(|review| {
                review.version == info.version && review.after.text == info.description
            }) {
                info.description_review = review.after.clone();
            }
        }
        Ok(info)
    }

    /// Installed folder of the skill's current version.
    pub fn skill_path(&self, name: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT v.path FROM skills s JOIN skill_versions v ON v.skill_id=s.id AND v.version=s.current_version WHERE s.name=?1",
                params![name],
                |r| r.get(0),
            )
            .ok())
    }

    /// Enabling also restores a retired skill.
    pub fn set_skill_enabled(&self, name: &str, enabled: bool) -> Result<bool> {
        Ok(self.ex(
            "UPDATE skills SET enabled=?2, retired=CASE WHEN ?2 THEN 0 ELSE retired END,
               retire_reason=CASE WHEN ?2 THEN NULL ELSE retire_reason END, updated_at=?3 WHERE name=?1",
            params![name, enabled, now()],
        )? > 0)
    }

    /// Record a skill load. An empty Run ID represents a manual load.
    pub fn record_skill_load(&self, name: &str, session_id: &str, run_id: &str) -> Result<()> {
        self.ex(
            "INSERT INTO skill_loads(skill_id, session_id, run_id, created_at) SELECT id, ?2, NULLIF(?3,''), ?4 FROM skills WHERE name=?1",
            params![name, session_id, run_id, now()],
        )?;
        Ok(())
    }

    /// Retire auto-created skills that are unused (no load since `cutoff`,
    /// counting from their latest version) or unhelpful (Runs that loaded
    /// them complete less often than `min_rate`, once there are `min_runs`).
    /// Returns the retired names; they stay restorable.
    pub fn retire_auto_skills(
        &self,
        cutoff: Option<&str>,
        min_rate: f64,
        min_runs: i64,
    ) -> Result<Vec<String>> {
        let mut retired = Vec::new();
        for s in self.list_skills()? {
            if s.author != "auto" || s.retired {
                continue;
            }
            let idle = match cutoff {
                Some(c) => {
                    s.updated_at.as_str() < c
                        && !self.conn.query_row(
                            "SELECT EXISTS(SELECT 1 FROM skill_loads l JOIN skills k ON k.id=l.skill_id WHERE k.name=?1 AND l.created_at>=?2)",
                            params![s.name, c],
                            |r| r.get::<_, bool>(0),
                        )?
                }
                None => false,
            };
            let unhelpful =
                s.runs >= min_runs.max(1) && (s.runs_completed as f64) < min_rate * s.runs as f64;
            if idle || unhelpful {
                let reason = if unhelpful {
                    format!(
                        "unhelpful: {} of {} Runs that loaded it completed",
                        s.runs_completed, s.runs
                    )
                } else {
                    "unused: not loaded since it was last changed".to_string()
                };
                self.ex(
                    "UPDATE skills SET retired=1, enabled=0, retire_reason=?3, updated_at=?2 WHERE name=?1",
                    params![s.name, now(), reason],
                )?;
                retired.push(s.name);
            }
        }
        Ok(retired)
    }

    /// Registry Tools a Run called, in order, with input and status.
    pub fn run_invocations(&self, run_id: &str) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.ps(
            "SELECT t.name, i.input, i.status FROM invocations i JOIN tools t ON t.id=i.tool_id
             WHERE i.run_id=?1 ORDER BY i.created_at, i.rowid",
        )?;
        let rows = stmt
            .query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Names only for pattern detection. Input/status conversion is intentionally
    /// reserved for full invocation reads used by distillation.
    fn run_tool_names(&self, run_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.ps(
            "SELECT t.name FROM invocations i JOIN tools t ON t.id=i.tool_id
             WHERE i.run_id=?1 ORDER BY i.created_at, i.rowid",
        )?;
        let rows = stmt
            .query_map(params![run_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Whether a completed Run in another Session called the same Tools in
    /// the same order (a repeated pattern worth a skill).
    pub fn tool_sequence_seen_elsewhere(&self, session_id: &str, seq: &[String]) -> Result<bool> {
        let mut stmt = self.ps(
            "SELECT r.id FROM runs r WHERE r.session_id!=?1 AND r.status='completed'
               AND EXISTS(SELECT 1 FROM invocations i WHERE i.run_id=r.id)
             ORDER BY r.created_at DESC LIMIT 500",
        )?;
        let runs: Vec<String> = stmt
            .query_map(params![session_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for run in runs {
            let other = self.run_tool_names(&run)?;
            if other == seq {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether a Run loaded any skill (then it was already guided by one).
    pub fn run_loaded_skill(&self, run_id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM skill_loads WHERE run_id=?1)",
            params![run_id],
            |r| r.get(0),
        )?)
    }

    /// Delete a skill's rows; returns its version folders to remove.
    pub fn remove_skill(&self, name: &str) -> Result<Vec<String>> {
        let mut stmt = self.ps(
            "SELECT v.path FROM skill_versions v JOIN skills s ON s.id=v.skill_id WHERE s.name=?1",
        )?;
        let paths: Vec<String> = stmt
            .query_map(params![name], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        self.conn
            .execute("DELETE FROM skills WHERE name=?1", params![name])?;
        Ok(paths)
    }

    fn row_memory(r: &rusqlite::Row) -> rusqlite::Result<formless_protocol::MemoryInfo> {
        Ok(formless_protocol::MemoryInfo {
            id: r.get("id")?,
            text: r.get("text")?,
            scope: r.get("scope")?,
            author: r.get("author")?,
            session_id: r.get("session_id")?,
            enabled: r.get::<_, i64>("enabled")? != 0,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            last_used: r.get("last_used")?,
        })
    }

    /// Save a memory note; a duplicate (same scope, same text ignoring
    /// case and spacing) refreshes the existing note instead. Returns the
    /// note and whether it is new.
    pub fn add_memory(
        &self,
        text: &str,
        scope: &str,
        author: &str,
        session_id: Option<&str>,
    ) -> Result<(formless_protocol::MemoryInfo, bool)> {
        if author == "optimiser" {
            crate::memory::validate_len(text, crate::memory::MAX_OPTIMISER_CHARS)?;
            crate::memory::validate_scope(scope)?;
        } else {
            crate::memory::validate(text, scope)?;
        }
        let key = crate::memory::dedup_key(text);
        let t = now();
        self.transaction(|st| {
            let existing: Option<String> = st
                .conn
                .query_row(
                    "SELECT id FROM memories WHERE scope=?1 AND dedup_key=?2",
                    params![scope, key],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                st.ex(
                    "UPDATE memories SET enabled=1, updated_at=?2 WHERE id=?1",
                    params![id, t],
                )?;
                return Ok((st.get_memory(&id)?.context("memory vanished")?, false));
            }
            let text = crate::memory::clean(text);
            let id = insert_short_id('m', "memory", uuid::Uuid::new_v4, |id| {
                st.ex(
                    "INSERT INTO memories(id,text,dedup_key,scope,author,session_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)
                     ON CONFLICT(id) DO NOTHING",
                    params![id, text, key, scope, author, session_id, t],
                )
            })?;
            Ok((st.get_memory(&id)?.context("memory vanished")?, true))
        })
    }

    pub fn get_memory(&self, id: &str) -> Result<Option<formless_protocol::MemoryInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM memories WHERE id=?1",
                params![id],
                Self::row_memory,
            )
            .ok())
    }

    pub fn list_memories(&self) -> Result<Vec<formless_protocol::MemoryInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM memories ORDER BY scope, created_at, rowid")?;
        let rows = stmt
            .query_map([], Self::row_memory)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Edit a note's text, scope or enabled flag.
    pub fn update_memory(
        &self,
        id: &str,
        text: Option<&str>,
        scope: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<Option<formless_protocol::MemoryInfo>> {
        let Some(cur) = self.get_memory(id)? else {
            return Ok(None);
        };
        let text = text.unwrap_or(&cur.text);
        let scope = scope.unwrap_or(&cur.scope);
        crate::memory::validate(text, scope)?;
        self.ex(
            "UPDATE memories SET text=?2, dedup_key=?3, scope=?4, enabled=?5, updated_at=?6 WHERE id=?1",
            params![
                id,
                crate::memory::clean(text),
                crate::memory::dedup_key(text),
                scope,
                enabled.unwrap_or(cur.enabled),
                now()
            ],
        )?;
        self.get_memory(id)
    }

    pub fn delete_memory(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM memories WHERE id=?1", params![id])?
            > 0)
    }

    /// Which memories a Run's prompt carried.
    pub fn set_run_memories(&self, run_id: &str, ids: &[String]) -> Result<()> {
        for id in ids {
            self.ex(
                "INSERT OR IGNORE INTO run_memories(run_id, memory_id) VALUES(?1,?2)",
                params![run_id, id],
            )?;
        }
        Ok(())
    }

    pub fn run_memories(&self, run_id: &str) -> Result<Vec<formless_protocol::MemoryInfo>> {
        Ok(self
            .list_memories()?
            .into_iter()
            .filter(|m| {
                self.conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM run_memories WHERE run_id=?1 AND memory_id=?2)",
                        params![run_id, m.id],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
            })
            .collect())
    }

    /// Mark the enabled notes as used (a Session's prompt included them).
    pub fn touch_memories(&self) -> Result<()> {
        self.ex(
            "UPDATE memories SET last_used=?1 WHERE enabled=1",
            params![now()],
        )?;
        Ok(())
    }

    /// Whether the user has approved using this MCP server at all.
    pub fn mcp_server_approved(&self, server: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM mcp_server_grants WHERE server=?1)",
            params![server],
            |r| r.get(0),
        )?)
    }

    pub fn approve_mcp_server(&self, server: &str) -> Result<()> {
        self.ex(
            "INSERT OR REPLACE INTO mcp_server_grants(server, approved_at) VALUES(?1, ?2)",
            params![server, now()],
        )?;
        Ok(())
    }

    /// Always deny this Capability spec, for every Tool (SPEC §10.3).
    pub fn add_denial(&self, capability: &str, tool_name: &str, by: &str) -> Result<()> {
        self.ex(
            "INSERT OR REPLACE INTO denials(capability,tool_name,denied_by,created_at) VALUES(?1,?2,?3,?4)",
            params![capability, tool_name, by, now()],
        )?;
        Ok(())
    }

    pub fn list_denials(&self) -> Result<Vec<formless_protocol::DenialInfo>> {
        let mut stmt = self.ps(
            "SELECT capability, tool_name, denied_by, created_at FROM denials ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(formless_protocol::DenialInfo {
                    capability: r.get(0)?,
                    tool_name: r.get(1)?,
                    denied_by: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Forget one remembered denial, or (None) all; returns how many.
    pub fn forget_denial(&self, capability: Option<&str>) -> Result<usize> {
        Ok(self.ex(
            "DELETE FROM denials WHERE ?1 IS NULL OR capability = ?1",
            params![capability],
        )?)
    }

    /// Pairs of raw MCP tools the agent called back to back (top-level
    /// calls, nothing between them) since `since`, with the number of
    /// Runs each pair occurred in, most frequent first.
    pub fn mcp_call_pairs(&self, since: &str) -> Result<Vec<((String, String), i64)>> {
        let mut stmt = self.ps(
            "SELECT i.run_id, t.name FROM invocations i JOIN tools t ON t.id = i.tool_id
             WHERE i.parent_id IS NULL AND i.run_id IS NOT NULL AND i.created_at >= ?1
               AND i.status = 'success'
             ORDER BY i.run_id, i.created_at, i.rowid",
        )?;
        let rows = stmt
            .query_map(params![since], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut runs: std::collections::HashMap<(String, String), std::collections::HashSet<&str>> =
            Default::default();
        for w in rows.windows(2) {
            let ((run_a, a), (run_b, b)) = (&w[0], &w[1]);
            if run_a == run_b
                && a != b
                && a.starts_with(crate::mcp::PREFIX)
                && b.starts_with(crate::mcp::PREFIX)
            {
                runs.entry((a.clone(), b.clone()))
                    .or_default()
                    .insert(run_a);
            }
        }
        let mut out: Vec<_> = runs
            .into_iter()
            .map(|(pair, r)| (pair, r.len() as i64))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Stop suggesting the chain `first` → `second` (#771).
    pub fn dismiss_mcp_chain(&self, first: &str, second: &str) -> Result<()> {
        self.ex(
            "INSERT OR REPLACE INTO dismissed_mcp_chains (first, second, dismissed_at) VALUES (?1, ?2, ?3)",
            params![first, second, now()],
        )?;
        Ok(())
    }

    pub fn dismissed_mcp_chains(&self) -> Result<std::collections::HashSet<(String, String)>> {
        let mut stmt = self.ps("SELECT first, second FROM dismissed_mcp_chains")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The output of the latest successful Invocation of the Tool called
    /// `name` with exactly this (redacted) input, if one was recorded.
    pub fn recorded_output(&self, name: &str, input: &str) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT i.output FROM invocations i JOIN tools t ON t.id = i.tool_id
                 WHERE t.name = ?1 AND i.input = ?2 AND i.status = 'success'
                 ORDER BY i.created_at DESC LIMIT 1",
                params![name, input],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Raw MCP tools (`mcp__*`) that a live, non-MCP Tool has called, with
    /// those wrapper Tools' names (sorted): known from nested Invocations,
    /// nothing is declared (SPEC §7.1).
    pub fn mcp_wrappers(&self) -> Result<std::collections::HashMap<String, Vec<String>>> {
        let mut stmt = self.ps("SELECT DISTINCT ct.name, pt.name FROM invocations c
             JOIN invocations p ON p.id = c.parent_id
             JOIN tools ct ON ct.id = c.tool_id
             JOIN tools pt ON pt.id = p.tool_id
             WHERE ct.name LIKE 'mcp\\_\\_%' ESCAPE '\\'
               AND pt.name NOT LIKE 'mcp\\_\\_%' ESCAPE '\\'
               AND pt.retired = 0 AND pt.disabled = 0
             ORDER BY 1, 2")?;
        let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (raw, wrapper) = row?;
            out.entry(raw).or_default().push(wrapper);
        }
        Ok(out)
    }

    /// Persist one comparable successful physical request observation. Missing
    /// usage is never a zero-token training sample.
    pub fn observe_prompt_tokens(
        &self,
        provider: &str,
        model: &str,
        raw: usize,
        reported: u64,
    ) -> Result<()> {
        if raw == 0 || reported == 0 {
            return Ok(());
        }
        let sample = (reported as f64 / raw as f64)
            .clamp(crate::tokens::MIN_RATIO, crate::tokens::MAX_RATIO);
        self.ex(
            "INSERT INTO token_calibration(provider_key,model,ratio,samples) VALUES(?1,?2,?3,1)
             ON CONFLICT(provider_key,model) DO UPDATE SET
             ratio=MAX(?4,MIN(?5,token_calibration.ratio*(1-?6)+?7*?6)), samples=token_calibration.samples+1",
            params![provider, model, 1.0 + crate::tokens::ALPHA * (sample - 1.0), crate::tokens::MIN_RATIO, crate::tokens::MAX_RATIO, crate::tokens::ALPHA, sample],
        )?;
        Ok(())
    }

    pub fn token_ratio(&self, provider: &str, model: &str) -> Result<f64> {
        use rusqlite::OptionalExtension;
        let ratio: Option<f64> = self
            .conn
            .query_row(
                "SELECT ratio FROM token_calibration WHERE provider_key=?1 AND model=?2",
                params![provider, model],
                |r| r.get(0),
            )
            .optional()?;
        Ok(ratio
            .filter(|r| r.is_finite())
            .unwrap_or(1.0)
            .clamp(crate::tokens::MIN_RATIO, crate::tokens::MAX_RATIO))
    }

    /// Record one provider request's reported token usage (one Run step).
    pub fn record_usage(
        &self,
        session_id: &str,
        run_id: &str,
        model: &str,
        usage: &crate::provider::Usage,
    ) -> Result<()> {
        self.record_usage_from(session_id, run_id, model, usage, "run")
    }

    /// Record a provider request made on a Run's behalf: `source` is "run"
    /// for its own steps, "llm" for `llm.complete`, "agent" for sub-agents.
    pub fn record_usage_from(
        &self,
        session_id: &str,
        run_id: &str,
        model: &str,
        usage: &crate::provider::Usage,
        source: &str,
    ) -> Result<()> {
        self.record_usage_full(session_id, run_id, "", model, usage, source)
    }

    /// [`Self::record_usage_from`] naming the provider that served it, so
    /// its own prices apply (#810).
    pub fn record_usage_full(
        &self,
        session_id: &str,
        run_id: &str,
        provider: &str,
        model: &str,
        usage: &crate::provider::Usage,
        source: &str,
    ) -> Result<()> {
        self.ex(
            "INSERT INTO run_usage(session_id,run_id,model,prompt_tokens,completion_tokens,reasoning_tokens,cached_tokens,cache_write_tokens,cost,created_at,source,provider,usage_complete,cost_unknown) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                session_id,
                run_id,
                model,
                usage.prompt_tokens as i64,
                usage.completion_tokens as i64,
                usage.reasoning_tokens as i64,
                usage.cached_tokens as i64,
                usage.cache_write_tokens as i64,
                usage.cost,
                now(),
                source,
                provider,
                i64::from(usage.reported==3),
                i64::from(usage.cost_unknown && usage.cost.is_none())
            ],
        )?;
        Ok(())
    }

    /// Tokens the daemon's own calls of `source_prefix` (e.g. `optimiser`)
    /// reported today, UTC (#806).
    pub fn internal_tokens_today(&self, source_prefix: &str) -> Result<i64> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(prompt_tokens + completion_tokens),0) FROM run_usage
             WHERE created_at >= ?1 AND substr(source,1,length(?2))=?2",
            params![today, source_prefix],
            |r| r.get(0),
        )?)
    }

    /// Hold an attachment for the Session's next user message.
    pub fn add_pending_attachment(&self, session_id: &str, att: &Value) -> Result<()> {
        self.ex(
            "INSERT INTO pending_attachments(session_id, att, created_at) VALUES(?1,?2,?3)",
            params![session_id, att.to_string(), now()],
        )?;
        Ok(())
    }

    /// The Session's held attachments, oldest first.
    pub fn pending_attachments(&self, session_id: &str) -> Result<Vec<Value>> {
        let mut stmt =
            self.ps("SELECT att FROM pending_attachments WHERE session_id=?1 ORDER BY rowid")?;
        let rows = stmt
            .query_map(params![session_id], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok().and_then(|t| serde_json::from_str(&t).ok()))
            .collect();
        Ok(rows)
    }

    /// Strict bounded metadata read: never silently omits corrupt rows.
    /// The SQL projection avoids loading an oversized individual metadata value.
    pub fn pending_attachment_inventory(&self, session_id: &str) -> Result<Vec<Value>> {
        let max_rows = formless_protocol::PENDING_ATTACHMENT_MAX_ENTRIES;
        let max_bytes = formless_protocol::PENDING_ATTACHMENT_METADATA_BYTES;
        let mut stmt=self.ps("SELECT length(CAST(att AS BLOB)), CASE WHEN length(CAST(att AS BLOB))<=?2 THEN att ELSE NULL END FROM pending_attachments WHERE session_id=?1 ORDER BY rowid LIMIT ?3")?;
        let rows = stmt.query_map(
            params![session_id, max_bytes as i64, (max_rows + 1) as i64],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )?;
        let mut out = Vec::new();
        let mut bytes = 0usize;
        for row in rows {
            let (size, text) = row?;
            if out.len() >= max_rows || size < 0 || size as u64 > max_bytes as u64 {
                return Err(anyhow!(
                    "pending attachment inventory exceeds metadata bounds"
                ));
            }
            let text = text.context("pending attachment metadata unavailable")?;
            bytes = bytes.saturating_add(text.len());
            if bytes > max_bytes {
                return Err(anyhow!(
                    "pending attachment inventory exceeds metadata bounds"
                ));
            }
            let value: Value =
                serde_json::from_str(&text).context("pending attachment metadata is corrupt")?;
            if !value.is_object() {
                return Err(anyhow!("pending attachment metadata is not a record"));
            }
            out.push(value);
        }
        Ok(out)
    }

    /// Take the Session's held attachments (they go with its next message).
    pub fn take_pending_attachments(&self, session_id: &str) -> Result<Vec<Value>> {
        let out = self.pending_attachments(session_id)?;
        self.ex(
            "DELETE FROM pending_attachments WHERE session_id=?1",
            params![session_id],
        )?;
        Ok(out)
    }

    /// Drop the held attachment whose id (or, for a referenced file, path)
    /// is `key`; returns it.
    pub fn remove_pending_attachment(&self, session_id: &str, key: &str) -> Result<Option<Value>> {
        let rows: Vec<(i64, String)> = {
            let mut stmt = self.ps(
                "SELECT rowid, att FROM pending_attachments WHERE session_id=?1 ORDER BY rowid",
            )?;
            let rows = stmt
                .query_map(params![session_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            rows
        };
        for (rowid, text) in rows {
            let Ok(att) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if att["id"].as_str() == Some(key) || att["path"].as_str() == Some(key) {
                self.ex(
                    "DELETE FROM pending_attachments WHERE rowid=?1",
                    params![rowid],
                )?;
                return Ok(Some(att));
            }
        }
        Ok(None)
    }

    /// Held attachments never sent and older than `before` (RFC 3339),
    /// taken off the queue and returned so their files can go.
    pub fn expire_pending_attachments(&self, before: &str) -> Result<Vec<Value>> {
        let rows: Vec<Value> = {
            let mut stmt = self.ps("SELECT att FROM pending_attachments WHERE created_at < ?1")?;
            let rows = stmt
                .query_map(params![before], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok().and_then(|t| serde_json::from_str(&t).ok()))
                .collect();
            rows
        };
        self.ex(
            "DELETE FROM pending_attachments WHERE created_at < ?1",
            params![before],
        )?;
        Ok(rows)
    }

    /// Summed (prompt, completion, cached) tokens per model.
    pub fn usage_by_model(
        &self,
        since: Option<&str>,
    ) -> Result<std::collections::HashMap<String, (i64, i64, i64)>> {
        let mut stmt = self.ps(&usage_by_model_sql(since.is_some()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(since), |r| {
                Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Token use since `since` grouped by `by` ("model", "provider",
    /// "purpose", "role", the immutable "session" ID, else the UTC day),
    /// then by provider and model for pricing.
    pub fn usage_grouped(&self, since: Option<&str>, by: &str) -> Result<Vec<GroupedUsage>> {
        self.query_usage_groups(since, by, |usage, _| usage)
    }

    pub(crate) fn usage_grouped_with_labels(
        &self,
        since: Option<&str>,
        by: &str,
    ) -> Result<Vec<(GroupedUsage, Option<String>)>> {
        self.query_usage_groups(since, by, |usage, label| (usage, label))
    }

    fn query_usage_groups<T>(
        &self,
        since: Option<&str>,
        by: &str,
        project: impl Fn(GroupedUsage, Option<String>) -> T,
    ) -> Result<Vec<T>> {
        let mut stmt = self.ps(&grouped_usage_sql(by, since.is_some()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(since), |r| {
                let usage = (
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                );
                Ok(project(usage, r.get(9)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Summed usage of a Run's provider requests.
    pub fn run_usage(&self, run_id: &str) -> Result<crate::provider::Usage> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(reasoning_tokens),0), COALESCE(SUM(cached_tokens),0), COALESCE(SUM(cache_write_tokens),0),COALESCE(MAX(cost_unknown),0) FROM run_usage WHERE run_id=?1",
            params![run_id],
            |r| {
                Ok(crate::provider::Usage {
                    prompt_tokens: r.get::<_, i64>(0)? as u64,
                    completion_tokens: r.get::<_, i64>(1)? as u64,
                    reasoning_tokens: r.get::<_, i64>(2)? as u64,
                    cached_tokens: r.get::<_, i64>(3)? as u64,
                    cache_write_tokens: r.get::<_, i64>(4)? as u64,
                    cost_unknown:r.get(5)?,
                    ..Default::default()
                })
            },
        )?)
    }

    /// The Session's latest real prompt size, if one was reported since its
    /// latest compaction checkpoint (older ones describe a longer context).
    pub fn last_prompt_tokens(&self, session_id: &str) -> Result<Option<u64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT prompt_tokens FROM run_usage WHERE session_id=?1 AND source='run' \
                 AND created_at > COALESCE((SELECT MAX(created_at) FROM checkpoints WHERE session_id=?1), '') \
                 ORDER BY rowid DESC LIMIT 1",
                params![session_id],
                |r| r.get::<_, i64>(0),
            )
            .ok()
            .map(|n| n as u64))
    }

    /// (id, path, bytes, created_at) of every artifact, oldest first.
    pub fn list_artifacts(&self) -> Result<Vec<(String, String, i64, String)>> {
        let mut stmt = self
            .ps("SELECT id, path, bytes, created_at FROM artifacts ORDER BY created_at, rowid")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// (session id, path, bytes) of one artifact.
    pub fn get_artifact(&self, id: &str) -> Result<Option<(Option<String>, String, i64)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT session_id, path, bytes FROM artifacts WHERE id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
    }

    /// Paths of a Session's artifacts (to delete with it).
    pub(crate) fn session_artifact_records(
        &self,
        session_id: &str,
    ) -> Result<Vec<(String, String)>> {
        Ok(self
            .conn
            .prepare("SELECT id,path FROM artifacts WHERE session_id=?1")?
            .query_map([session_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn orphaned_artifact_records(&self) -> Result<Vec<(String, String)>> {
        Ok(self.conn.prepare("SELECT id,session_id FROM artifacts WHERE session_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM sessions WHERE sessions.id=artifacts.session_id)")?
            .query_map([],|row|Ok((row.get(0)?,row.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn artifact_reference_sessions(&self) -> Result<Vec<String>> {
        Ok(self
            .conn
            .prepare("SELECT id FROM sessions")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn session_references_artifact(
        &self,
        id: &str,
        session: &str,
        owner: &str,
    ) -> Result<bool> {
        crate::attachment_access::session_references_artifact(&self.conn, id, session, owner)
    }

    /// Paths of a Session's artifacts (to delete with it).
    pub fn session_artifact_paths(&self, session_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM artifacts WHERE session_id=?1")?;
        let rows = stmt
            .query_map(params![session_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn delete_artifact_rows(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            self.conn
                .execute("DELETE FROM artifacts WHERE id=?1", params![id])?;
        }
        Ok(())
    }

    /// Mark the attachments with these artifact ids as expired in the
    /// messages that carry them, so every client and the model's history
    /// say so instead of pointing at a removed file (#858).
    pub fn delete_session_artifact_rows(&self, session_id: &str) -> Result<()> {
        self.ex(
            "DELETE FROM artifacts WHERE session_id=?1",
            params![session_id],
        )?;
        Ok(())
    }

    pub fn save_artifact(
        &self,
        session_id: Option<&str>,
        path: &str,
        preview: &str,
        bytes: i64,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.ex(
            "INSERT INTO artifacts(id,session_id,path,preview,bytes,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![id, session_id, path, preview, bytes, now()],
        )?;
        Ok(id)
    }

    // --- Optimiser state (SPEC §8) ---

    /// Success rate over the last `window` executed invocations
    /// (success+failure only). Returns (sample, rate).
    pub fn windowed_success(&self, tool_id: &str, window: usize) -> Result<(i64, Option<f64>)> {
        let (sample, succ): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(status='success'),0) FROM (SELECT status FROM invocations WHERE tool_id=?1 AND status IN ('success','failure') ORDER BY rowid DESC LIMIT ?2)",
            params![tool_id, window as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((
            sample,
            if sample > 0 {
                Some(succ as f64 / sample as f64)
            } else {
                None
            },
        ))
    }

    /// Bounded recent terminal inputs, selected without balancing outcomes.
    pub fn replay_set(&self, tool_id: &str) -> Result<Vec<ReplayItem>> {
        Ok(self
            .replay_pool(tool_id, 200)?
            .0
            .into_iter()
            .map(|group| {
                // This legacy view supplies current failure context, not the
                // frozen holdout's any-known-success regression policy.
                let mut item = group.item;
                item.recorded_success = group.latest_success;
                item
            })
            .collect())
    }

    pub fn create_attempt(
        &self,
        tool_id: &str,
        base_version: i64,
        cand: &CandidateData,
        est_tokens: i64,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.ex(
            "INSERT INTO optimiser_attempts(id,tool_id,base_version,status,candidate_code,candidate_description,candidate_input_schema,candidate_output_schema,candidate_capabilities,candidate_memory_mb,est_tokens,created_at) VALUES(?1,?2,?3,'generated',?4,?5,?6,?7,?8,?9,?10,?11)",
            params![id, tool_id, base_version, cand.code, cand.description, cand.input_schema, cand.output_schema, serde_json::to_string(&cand.capabilities)?, cand.memory_mb, est_tokens, now()],
        )?;
        let detail = self.attempt_schema_detail(&id, "generated candidate")?;
        self.ex(
            "UPDATE optimiser_attempts SET detail=?2 WHERE id=?1",
            params![id, detail],
        )?;
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_attempt(
        &self,
        id: &str,
        status: &str,
        detail: &str,
        candidate_score: Option<f64>,
        baseline_score: Option<f64>,
        replay_total: i64,
        regressions: i64,
    ) -> Result<()> {
        let detail = self.attempt_schema_detail(id, detail)?;
        self.ex(
            "UPDATE optimiser_attempts SET status=?2, detail=?3, candidate_score=?4, baseline_score=?5, replay_total=?6, regressions=?7 WHERE id=?1",
            params![id, status, detail, candidate_score, baseline_score, replay_total, regressions],
        )?;
        Ok(())
    }

    pub fn get_attempt(&self, id: &str) -> Result<Option<AttemptRecord>> {
        let row = self.conn.query_row(
            "SELECT tool_id,base_version,status,candidate_code,candidate_description,candidate_input_schema,candidate_output_schema,candidate_capabilities,candidate_memory_mb FROM optimiser_attempts WHERE id=?1",
            params![id],
            |r| {
                Ok(AttemptRecord {
                    tool_id: r.get(0)?,
                    base_version: r.get(1)?,
                    status: r.get(2)?,
                    candidate: CandidateData {
                        code: r.get(3)?,
                        description: r.get(4)?,
                        input_schema: r.get(5)?,
                        output_schema: r.get(6)?,
                        capabilities: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                        memory_mb: r.get::<_, i64>(8)? as u32,
                    },
                })
            },
        );
        Ok(row.ok())
    }

    pub fn list_attempts(
        &self,
        tool_id: &str,
        limit: i64,
    ) -> Result<Vec<formless_protocol::OptimiserAttemptInfo>> {
        let mut stmt = self.ps(
            "SELECT id,base_version,status,detail,candidate_score,baseline_score,replay_total,regressions,created_at FROM optimiser_attempts WHERE tool_id=?1 ORDER BY rowid DESC LIMIT ?2",
        )?;
        let mut rows = stmt
            .query_map(params![tool_id, limit], |r| {
                Ok(formless_protocol::OptimiserAttemptInfo {
                    id: r.get(0)?,
                    base_version: r.get(1)?,
                    status: r.get(2)?,
                    detail: r.get(3)?,
                    candidate_score: r.get(4)?,
                    baseline_score: r.get(5)?,
                    replay_total: r.get(6)?,
                    regressions: r.get(7)?,
                    created_at: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for attempt in &mut rows {
            let current = serde_json::from_str::<Value>(&attempt.detail)
                .ok()
                .is_some_and(|detail| {
                    detail.get("format").and_then(Value::as_str)
                        == Some("optimiser-attempt-schema-v1")
                });
            if !current {
                attempt.detail = self.attempt_schema_detail(&attempt.id, &attempt.detail)?;
            }
        }
        Ok(rows)
    }

    pub fn attempts_today(&self, tool_id: &str) -> Result<i64> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM optimiser_attempts WHERE tool_id=?1 AND created_at LIKE ?2",
            params![tool_id, format!("{today}%")],
            |r| r.get(0),
        )?)
    }

    pub fn attempt_tokens_today(&self) -> Result<i64> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(est_tokens),0) FROM optimiser_attempts WHERE created_at LIKE ?1",
            params![format!("{today}%")],
            |r| r.get(0),
        )?)
    }

    pub fn get_opt_state(&self, tool_id: &str) -> Result<(Option<String>, i64)> {
        let row = self.conn.query_row(
            "SELECT cooldown_until, consecutive_failures FROM tools WHERE id=?1",
            params![tool_id],
            |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)),
        );
        Ok(row.unwrap_or((None, 0)))
    }

    pub fn set_cooldown(&self, tool_id: &str, until_rfc3339: Option<&str>) -> Result<()> {
        self.ex(
            "UPDATE tools SET cooldown_until=?2 WHERE id=?1",
            params![tool_id, until_rfc3339],
        )?;
        Ok(())
    }

    pub fn set_consecutive_failures(&self, tool_id: &str, n: i64) -> Result<()> {
        self.ex(
            "UPDATE tools SET consecutive_failures=?2 WHERE id=?1",
            params![tool_id, n],
        )?;
        Ok(())
    }

    pub fn set_needs_attention(&self, tool_id: &str, on: bool) -> Result<()> {
        self.ex(
            "UPDATE tools SET needs_attention=?2, updated_at=?3 WHERE id=?1",
            params![tool_id, on as i64, now()],
        )?;
        Ok(())
    }

    /// Recent audit entries, newest first: (actor, action, detail, at).
    /// `action_prefix` narrows them ("tool.", "proposal.").
    pub fn recent_audit(
        &self,
        action_prefix: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.ps("SELECT actor, action, detail, created_at FROM audit
             WHERE ?1 IS NULL OR substr(action,1,length(?1))=?1
             ORDER BY created_at DESC, rowid DESC LIMIT ?2")?;
        let rows = stmt
            .query_map(params![action_prefix, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Audit entries for the log view, newest first by timestamp then stable id.
    /// `before` accepts an opaque compound cursor or a legacy timestamp.
    /// `filter` matches the actor, action or detail (case-insensitive);
    /// `-text` leaves matches out; `*` shows everything. With no filter
    /// the decisions show and the per-call `*.run_unasked` records don't
    /// (#848): filter on `run_unasked` to see them.
    pub fn list_audit(
        &self,
        filter: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<formless_protocol::AuditEntry>> {
        let (before_time, before_id) = crate::audit_cursor::decode(before)?;
        let (include, exclude) = match filter.map(str::trim) {
            None | Some("") => (None, Some("run_unasked")),
            Some("*") => (None, None),
            Some(f) => match f.strip_prefix('-') {
                Some(out) => (None, Some(out.trim())),
                None => (Some(f), None),
            },
        };
        let mut stmt = self.ps(
            "SELECT actor, action, detail, created_at, id FROM audit
             WHERE (?1 IS NULL OR instr(lower(actor || ' ' || action || ' ' || detail), lower(?1)) > 0)
               AND (?4 IS NULL OR instr(lower(actor || ' ' || action || ' ' || detail), lower(?4)) = 0)
               AND (?2 IS NULL OR created_at < ?2 OR (?5 IS NOT NULL AND created_at=?2 AND id < ?5))
             ORDER BY created_at DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(
                params![
                    include,
                    before_time,
                    limit.clamp(1, 500) as i64,
                    exclude,
                    before_id
                ],
                |r| {
                    let created_at: String = r.get(3)?;
                    let id: String = r.get(4)?;
                    Ok(formless_protocol::AuditEntry {
                        cursor: Some(crate::audit_cursor::encode(&created_at, &id)),
                        actor: r.get(0)?,
                        action: r.get(1)?,
                        detail: r.get(2)?,
                        created_at,
                    })
                },
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Every active Tool with its current version.
    pub fn current_tools(&self) -> Result<Vec<(ToolInfo, ToolVersionInfo)>> {
        let mut out = Vec::new();
        for t in self.list_tools(false)? {
            if let Some(pair) = self.get_tool_current(&t.id)? {
                out.push(pair);
            }
        }
        Ok(out)
    }

    /// When each Tool was last invoked (tool id -> RFC 3339).
    pub fn tool_last_used(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT tool_id, MAX(created_at) FROM invocations GROUP BY tool_id")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Bounded immutable origin records, isolated by both Session and Run.
    /// IDs use a primary-key range rather than scanning the whole audit log.
    fn provenance_key(session: &str, run: &str) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "provenance:{:x}:",
            Sha256::digest(serde_json::to_vec(&(session, run)).unwrap())
        )
    }

    pub fn data_origins(&self, session: &str, run: &str) -> Result<Vec<crate::provenance::Origin>> {
        let key = Self::provenance_key(session, run);
        let mut statement =
            self.ps("SELECT detail FROM audit WHERE id>=?1 AND id<?2 ORDER BY id LIMIT 64")?;
        let rows =
            statement.query_map(params![key, format!("{key}~")], |r| r.get::<_, String>(0))?;
        let mut origins = Vec::new();
        for row in rows {
            origins.push(serde_json::from_str(&row?)?);
        }
        Ok(origins)
    }

    pub fn record_data_origin(
        &self,
        session: &str,
        run: &str,
        origin: &crate::provenance::Origin,
    ) -> Result<()> {
        use sha2::{Digest, Sha256};
        let origins = self.data_origins(session, run)?;
        if origins.contains(origin) || origins.len() >= crate::provenance::MAX_ORIGINS {
            return Ok(());
        }
        let origin = if origins.len() + 1 == crate::provenance::MAX_ORIGINS {
            crate::provenance::Origin::new(
                "unknown",
                "truncated",
                "additional data origins omitted",
            )
        } else {
            origin.clone()
        };
        let detail = serde_json::to_string(&origin)?;
        let id = format!(
            "{}{:x}",
            Self::provenance_key(session, run),
            Sha256::digest(detail.as_bytes())
        );
        self.ex("INSERT OR IGNORE INTO audit(id,actor,action,detail,created_at) VALUES(?1,'provenance','tool.data',?2,?3)",params![id,detail,now()])?;
        Ok(())
    }

    /// Durable metadata evidence. The audit primary-key range avoids scanning
    /// unrelated invocation/provenance history; no schema upgrade is needed.
    #[allow(clippy::too_many_arguments)]
    fn record_description_review(
        &self,
        kind: &str,
        name: &str,
        version: i64,
        source: &str,
        before: Option<&str>,
        after: &str,
    ) -> Result<()> {
        let before = before.map(crate::descriptions::screen);
        let after = crate::descriptions::screen(after);
        if before.as_ref().is_some_and(|old| old.text == after.text)
            || (before.is_none() && after.flags.is_empty())
        {
            return Ok(());
        }
        let at = now();
        let id = format!("description:{at}:{}", uuid::Uuid::new_v4());
        let review = formless_protocol::DescriptionReview {
            id: id.clone(),
            kind: kind.into(),
            name: name.into(),
            version,
            source: crate::provenance::Origin::new(name, kind, source).origin,
            before,
            after,
            created_at: at.clone(),
        };
        self.ex("INSERT INTO audit(id,actor,action,detail,created_at) VALUES(?1,'metadata','description.review',?2,?3)",params![id,serde_json::to_string(&review)?,at])?;
        let mut pending = self
            .description_notifications
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if pending.0.len() < 128 {
            pending.0.push(review);
        } else {
            pending.1 = true;
        }
        Ok(())
    }

    pub fn description_reviews(
        &self,
        kind: Option<&str>,
        name: Option<&str>,
        limit: usize,
    ) -> Result<Vec<formless_protocol::DescriptionReview>> {
        let mut statement=self.ps("SELECT detail FROM audit WHERE id>='description:' AND id<'description:~' AND (?1 IS NULL OR json_extract(detail,'$.kind')=?1 OR (?1='tool' AND json_extract(detail,'$.kind')='mcp')) AND (?2 IS NULL OR json_extract(detail,'$.name')=?2) ORDER BY created_at DESC,rowid DESC LIMIT ?3")?;
        let rows = statement.query_map(params![kind, name, limit.clamp(1, 200)], |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub(crate) fn take_description_notifications(
        &self,
    ) -> Result<(Vec<formless_protocol::DescriptionReview>, bool)> {
        if !self.conn.is_autocommit() {
            return Ok((Vec::new(), false));
        }
        let (pending, overflow) = std::mem::take(
            &mut *self
                .description_notifications
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        let mut committed = Vec::new();
        for review in pending {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM audit WHERE id=?1)",
                params![review.id],
                |r| r.get(0),
            )?;
            if exists {
                committed.push(review);
            }
        }
        Ok((committed, overflow))
    }

    pub fn audit(&self, actor: &str, action: &str, detail: &str) -> Result<()> {
        self.ex(
            "INSERT INTO audit(id,actor,action,detail,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                actor,
                action,
                detail,
                now()
            ],
        )?;
        // The per-call records of approvals-off mode are kept to the newest
        // UNASKED_KEEP; decisions are never trimmed (#848).
        if action.ends_with(".run_unasked") {
            self.ex(
                "DELETE FROM audit WHERE action LIKE '%.run_unasked' AND rowid NOT IN
                   (SELECT rowid FROM audit WHERE action LIKE '%.run_unasked'
                    ORDER BY created_at DESC, rowid DESC LIMIT ?1)",
                params![UNASKED_KEEP],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod subagent_message_projection_tests {
    use super::*;

    #[test]
    fn subagent_messages_parse_rows_in_sequence_and_propagate_late_conversion_errors() {
        let store = Store::open_in_memory().unwrap();
        let session = store
            .create_session(Some("synthetic transcript fixture".into()))
            .unwrap();
        let agent = store
            .create_subagent(&session.id, "fixture-run", "fixture", 0, "fixture-model")
            .unwrap();
        let other = store
            .create_subagent(&session.id, "fixture-run", "other", 0, "fixture-model")
            .unwrap();
        let before = serde_json::to_value(store.list_subagents(&session.id).unwrap()).unwrap();
        let first = serde_json::json!({"role":"user","content":"ไทย\n日本語","provider_field":{"preserve":true}});
        store.append_subagent_message(&agent.id, &first).unwrap();
        store
            .append_subagent_message(&other.id, &serde_json::json!({"other":"excluded"}))
            .unwrap();
        for (sequence, raw) in [
            (30, "\"tail\""),
            (20, "{malformed"),
            (12, "null"),
            (10, "[\"雪\",false]"),
        ] {
            store
                .conn
                .execute(
                    "INSERT INTO subagent_messages(subagent_id,seq,message) VALUES(?1,?2,?3)",
                    params![agent.id, sequence, raw],
                )
                .unwrap();
        }
        let expected = vec![
            first,
            serde_json::json!(["雪", false]),
            Value::Null,
            serde_json::json!("tail"),
        ];
        assert_eq!(store.subagent_messages(&agent.id).unwrap(), expected);
        assert!(store
            .subagent_messages("missing fixture")
            .unwrap()
            .is_empty());
        store
            .conn
            .execute(
                "INSERT INTO subagent_messages(subagent_id,seq,message) VALUES(?1,99,?2)",
                params![agent.id, vec![0u8, 255]],
            )
            .unwrap();
        let error = store.subagent_messages(&agent.id).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<rusqlite::Error>(),
                Some(rusqlite::Error::InvalidColumnType(..))
            ),
            "{error:?}"
        );
        store
            .conn
            .execute(
                "DELETE FROM subagent_messages WHERE subagent_id=?1 AND seq=99",
                params![agent.id],
            )
            .unwrap();
        assert_eq!(store.subagent_messages(&agent.id).unwrap(), expected);
        assert_eq!(
            serde_json::to_value(store.list_subagents(&session.id).unwrap()).unwrap(),
            before
        );
        store
            .append_subagent_message(&agent.id, &serde_json::json!({"appended":"last"}))
            .unwrap();
        assert_eq!(
            store.subagent_messages(&agent.id).unwrap().last(),
            Some(&serde_json::json!({"appended":"last"}))
        );
    }
}

#[cfg(test)]
mod recurring_script_projection_tests {
    use super::*;

    fn record(
        store: &Store,
        identity: (&str, &str, &str),
        source: &str,
        uses: &[&str],
        state: (&str, &str),
    ) -> String {
        let uses: Vec<String> = uses.iter().map(|name| (*name).to_owned()).collect();
        let hash = crate::scripts::recurrence_key(source, &uses);
        store
            .record_script(
                identity.0,
                identity.1,
                "fixture-run",
                &hash,
                source,
                &uses,
                state.0,
                "",
                1,
                state.1,
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE scripts SET created_at=?2 WHERE id=?1",
                params![identity.0, identity.2],
            )
            .unwrap();
        hash
    }

    #[test]
    fn recurring_script_preview_selects_both_fields_from_one_ordered_row() {
        let store = Store::open_in_memory().unwrap();
        let first = "const path = 'old'; return tools.read({path});";
        let second = "const path = 'new'; return tools.read({path});";
        let third = "const path = 'ล่าสุด'; return tools.read({path});";
        let early_hash = record(
            &store,
            ("one", "s1", "2026-01-01"),
            first,
            &["z", "a", "z"],
            ("success", ""),
        );
        let late_hash = record(
            &store,
            ("two", "s2", "2026-01-01"),
            second,
            &["a", "z"],
            ("success", ""),
        );
        assert_eq!(
            early_hash, late_hash,
            "fixture uses the current normalized recurrence key"
        );
        assert_eq!(
            store.recurring_scripts(2, 5).unwrap(),
            vec![(2, vec!["a".into(), "z".into()], second.into())]
        );
        record(
            &store,
            ("failure", "s3", "2026-01-02"),
            third,
            &["z", "a", "z"],
            ("failure", "saved-on-failure"),
        );
        assert_eq!(
            store.recurring_scripts(2, 5).unwrap(),
            vec![(2, vec!["z".into(), "a".into(), "z".into()], third.into())]
        );
        record(
            &store,
            ("older-recorded-later", "s4", "2025-01-01"),
            first,
            &["a", "z"],
            ("failure", ""),
        );
        assert_eq!(store.recurring_scripts(2, 5).unwrap()[0].2, third);
        record(
            &store,
            ("latest-tie", "s5", "2026-01-02"),
            second,
            &["a", "z"],
            ("failure", ""),
        );
        assert_eq!(
            store.recurring_scripts(2, 5).unwrap(),
            vec![(2, vec!["a".into(), "z".into()], second.into())]
        );
    }

    #[test]
    fn recurring_script_group_scopes_limits_and_malformed_uses_are_preserved() {
        let store = Store::open_in_memory().unwrap();
        let many = "return tools.alpha({x:1});";
        let few = "return tools.beta({x:1});";
        for (id, session) in [
            ("many1", "s1"),
            ("many2", "s2"),
            ("many3", "s3"),
            ("many-duplicate-session", "s3"),
        ] {
            record(
                &store,
                (id, session, "2026-01-01"),
                many,
                &["alpha"],
                ("success", ""),
            );
        }
        for (id, session) in [("few1", "s1"), ("few2", "s2")] {
            record(
                &store,
                (id, session, "2026-01-01"),
                few,
                &["beta"],
                ("success", ""),
            );
        }
        record(
            &store,
            ("below", "s1", "2026-01-01"),
            "return tools.gamma({x:1});",
            &["gamma"],
            ("success", ""),
        );
        for (id, session, saved) in [("saved1", "s1", "promoted"), ("saved2", "s2", "")] {
            record(
                &store,
                (id, session, "2026-01-01"),
                "return tools.saved({x:1});",
                &["saved"],
                ("success", saved),
            );
        }
        for (id, session) in [("bad1", "s1"), ("bad2", "s2")] {
            record(
                &store,
                (id, session, "2026-01-01"),
                "return tools.failed({x:1});",
                &["failed"],
                ("failure", ""),
            );
        }
        let latest = "return tools.alpha({x:99});";
        let key = record(
            &store,
            ("malformed", "s4", "2026-01-02"),
            latest,
            &["alpha"],
            ("failure", "ignored-failed-save"),
        );
        assert_eq!(key, crate::scripts::recurrence_key(many, &["alpha".into()]));
        store
            .conn
            .execute(
                "UPDATE scripts SET uses='{invalid' WHERE id='malformed'",
                [],
            )
            .unwrap();
        let rows = store.recurring_scripts(2, 10).unwrap();
        assert_eq!(
            rows,
            vec![
                (3, vec![], latest.into()),
                (2, vec!["beta".into()], few.into())
            ]
        );
        assert_eq!(store.recurring_scripts(2, 1).unwrap(), rows[..1]);
        assert!(store.recurring_scripts(2, 0).unwrap().is_empty());
        assert!(store.recurring_scripts(4, 10).unwrap().is_empty());
    }
}

/// Rough token estimate (chars/4). Used only for compaction triggering.
pub fn estimate_tokens(text: &str) -> usize {
    crate::tokens::text(text)
}

pub type BuiltContext = (Vec<Value>, bool, i64);

#[cfg(test)]
#[path = "module_snapshot_tests.rs"]
mod module_snapshot_tests;

#[cfg(test)]
#[path = "audit_pagination_tests.rs"]
mod audit_pagination_tests;

#[cfg(test)]
#[path = "tool_sequence_names_tests.rs"]
mod tool_sequence_names_tests;

#[cfg(test)]
#[path = "targeted_message_lookup_tests.rs"]
mod targeted_message_lookup_tests;

/// Build provider context (OpenAI message objects) from checkpoint +
/// post-checkpoint messages. Tool-call/tool messages are reconstructed from
/// their stored meta so resumed Sessions are faithful (SPEC §5, §11).
pub fn build_context(
    store: &Store,
    session_id: &str,
    system_prompt: &str,
    context_tokens: u32,
    trigger_fraction: f64,
) -> Result<BuiltContext> {
    ContextCache::default().build(
        store,
        session_id,
        system_prompt,
        context_tokens,
        trigger_fraction,
    )
}

#[path = "plan_context.rs"]
mod plan_context;

/// [`build_context`] for one Session, kept between agent steps: each build
/// reads only messages newer than the last one seen and keeps a running
/// token estimate. It starts over when the system prompt or checkpoint
/// changes, or when stored messages were rewritten (edit, delete).
#[derive(Default)]
pub struct ContextCache {
    /// (store rewrite count, checkpoint generation, up_to, system prompt).
    /// The generation invalidates local caching but never enters the wire.
    key: Option<(u64, String, i64, String)>,
    ctx: Vec<Value>,
    /// Token estimate per `ctx` element.
    tokens: Vec<usize>,
    /// Seqs of the post-checkpoint messages, in order.
    live_seqs: Vec<i64>,
    /// Indexes into `live_seqs` where a user message starts a turn.
    turn_starts: Vec<usize>,
    /// Ids of stored tool calls left out for having no name (their
    /// results are left out too).
    dropped_calls: std::collections::HashSet<String>,
}

impl ContextCache {
    /// Transcript boundary read by the most recent successful context build.
    /// Filtered legacy Tool/assistant rows can advance this boundary, but user
    /// rows are retained; it never credits user messages appended afterward.
    /// Capture it together with the returned context under the same Store lock.
    pub(crate) fn last_included_seq(&self) -> i64 {
        self.live_seqs
            .last()
            .copied()
            .or_else(|| self.key.as_ref().map(|key| key.2))
            .unwrap_or(0)
    }

    /// Use the provider's last real prompt size (when known) for the
    /// compaction decision instead of the chars/4 estimate, whichever is
    /// larger: code, JSON and CJK text are far off four chars per token.
    pub fn with_known_tokens(
        &self,
        built: BuiltContext,
        known_prompt_tokens: Option<u64>,
        context_tokens: u32,
        trigger_fraction: f64,
    ) -> BuiltContext {
        let (ctx, needs, up_to) = built;
        let over = known_prompt_tokens
            .is_some_and(|n| n as f64 > context_tokens as f64 * trigger_fraction);
        match self.turn_cut() {
            Some(cut) if over && !needs => (ctx, true, cut),
            _ => (ctx, needs, up_to),
        }
    }

    /// Re-evaluate the decision with the complete calibrated prompt, including
    /// frozen schemas/images. A ratio below one can undo a raw heuristic trigger.
    pub fn with_prompt_tokens(
        &self,
        built: BuiltContext,
        estimated: usize,
        window: u32,
        fraction: f64,
    ) -> BuiltContext {
        let (ctx, _, up_to) = built;
        match self.turn_cut() {
            Some(cut) if estimated as f64 > window as f64 * fraction => (ctx, true, cut),
            _ => (ctx, false, up_to),
        }
    }

    pub fn build(
        &mut self,
        store: &Store,
        session_id: &str,
        system_prompt: &str,
        context_tokens: u32,
        trigger_fraction: f64,
    ) -> Result<BuiltContext> {
        let system_prompt =
            crate::main_topic_suggestions::system_prompt(store, session_id, system_prompt)?;
        let (checkpoint, summary, up_to) = store
            .latest_checkpoint_record(session_id)?
            .unwrap_or_default();
        let key = (
            store.rewrites.load(std::sync::atomic::Ordering::SeqCst),
            checkpoint.clone(),
            up_to,
            system_prompt.to_string(),
        );
        if self.key.as_ref() != Some(&key) {
            // Exactly one, leading system message: many chat templates and
            // proxies reject a second or mid-conversation `system` (§11
            // summary included).
            let system = if summary.is_empty() {
                crate::provenance::mandatory_system(&system_prompt)
            } else {
                let origins =
                    store.data_origins(session_id, &format!("checkpoint:{checkpoint}"))?;
                let summary = crate::provenance::frame("conversation_summary", &origins, &summary);
                crate::provenance::mandatory_system(&format!(
                    "{system_prompt}\n\nSummary of earlier conversation (data):\n{summary}"
                ))
            };
            *self = Self {
                key: Some(key),
                ..Self::default()
            };
            self.push(json!({"role": "system", "content": system}));
        }
        let after = self.live_seqs.last().copied().unwrap_or(up_to);
        for m in store.list_messages_after(session_id, after)? {
            if m.role == "user" {
                self.turn_starts.push(self.live_seqs.len());
            }
            self.live_seqs.push(m.seq);
            let mut msg = provider_message(&m);
            // A tool call without a name (stored before #540 was fixed) makes
            // strict endpoints reject every request: leave it, and its
            // result, out so the Session recovers (#541).
            if msg["tool_call_id"]
                .as_str()
                .is_some_and(|id| self.dropped_calls.contains(id))
            {
                continue;
            }
            // A read must not invent `tool_calls: null` on plain user/Tool
            // messages: preview and persisted projection share the same input.
            if let Some(calls) = msg.get_mut("tool_calls").and_then(Value::as_array_mut) {
                calls.retain(|c| {
                    let named = c["function"]["name"]
                        .as_str()
                        .is_some_and(|n| !n.trim().is_empty());
                    if !named {
                        let id = c["id"].as_str().unwrap_or_default();
                        self.dropped_calls.insert(id.to_string());
                    }
                    named
                });
                if calls.is_empty() {
                    if m.content.is_empty() {
                        continue;
                    }
                    msg = json!({"role": "assistant", "content": m.content});
                }
            }
            // A harness note right after a user message joins it, so strictly
            // alternating templates never see two user turns in a row.
            if m.role == "system-note" {
                if let Some(last) = self.ctx.last_mut().filter(|l| l["role"] == "user") {
                    if let (Some(prev), Some(note)) =
                        (last["content"].as_str(), msg["content"].as_str())
                    {
                        last["content"] = json!(format!("{prev}\n\n{note}"));
                        let n = crate::tokens::value(last);
                        *self.tokens.last_mut().expect("tokens track ctx") = n;
                        continue;
                    }
                }
            }
            self.push(msg);
        }
        // Plan state is read on every projection, outside the cached base and
        // persisted prompt. Forecasts, compaction and dispatch share this path.
        let mut context = self.ctx.clone();
        let mut tokens: usize = self.tokens.iter().sum();
        if plan_context::append(store, session_id, &mut context)? {
            let projected = crate::provenance::project(&context[..1]);
            tokens = tokens
                .saturating_sub(self.tokens.first().copied().unwrap_or(0))
                .saturating_add(crate::tokens::value(&projected[0]));
        }
        let over = (tokens as f64) > (context_tokens as f64 * trigger_fraction);
        let (needs, compact_up_to) = match self.turn_cut() {
            Some(cut) if over => (true, cut),
            _ => (false, up_to),
        };
        Ok((context, needs, compact_up_to))
    }

    /// Where to compact: the end of a whole turn (just before a user
    /// message), near the middle of the live messages. Cutting at turn
    /// boundaries never splits a tool call from its result and keeps the
    /// cut points stable between compactions. None while too short.
    fn turn_cut(&self) -> Option<i64> {
        let live = &self.live_seqs;
        if live.len() <= 4 {
            return None;
        }
        let mid = live.len() / 2;
        let start = self
            .turn_starts
            .iter()
            .rev()
            .find(|&&i| i > 0 && i <= mid)
            .or_else(|| self.turn_starts.iter().find(|&&i| i > mid))?;
        Some(live[start - 1])
    }

    fn push(&mut self, msg: Value) {
        self.tokens.push(crate::tokens::value(
            &crate::provenance::project(&[msg.clone()]).last().unwrap(),
        ));
        self.ctx.push(msg);
    }
}

/// An imported transcript carries data, never local attachment authority.
/// Fork copies use a separate trusted path and retain their recorded lineage.
fn sanitize_imported_message_meta(meta: Option<&str>) -> Option<String> {
    let raw = meta?;
    let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
        return Some(raw.into());
    };
    if let Some(object) = value.as_object_mut() {
        object.remove("_attachments");
        object.remove("_artifact_id");
        object.remove("_source_session");
        object.remove("_source_seq");
        if let Some(attachments) = object.get_mut("attachments").and_then(Value::as_array_mut) {
            for attachment in attachments {
                let old = attachment.clone();
                *attachment = json!({"imported":true,"name":old["name"],"mime":old["mime"],"bytes":old["bytes"]});
            }
        }
    }
    Some(value.to_string())
}

pub fn provider_message(m: &MessageInfo) -> Value {
    let meta: Value = m
        .meta
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    if m.role == "system-note" {
        // Sent as a user turn: a mid-conversation `system` message is
        // rejected by many OpenAI-compatible servers.
        let content = if meta["tool_data"] == true
            || m.content.starts_with("[Pending invocations completed]")
            || m.content.starts_with("The user loaded the skill")
        {
            let origins: Vec<crate::provenance::Origin> =
                serde_json::from_value(meta["data_origins"].clone()).unwrap_or_default();
            crate::provenance::frame("deferred_tool_data", &origins, &m.content)
        } else {
            format!("[harness note] {}", m.content)
        };
        return json!({"role":"user","content":content});
    }
    if m.role == "assistant" && meta["placeholder"] == true {
        // Text the daemon wrote for an empty refusal/filter stop (#1104):
        // never handed back as words the model said.
        let why = meta["stop_reason"].as_str().unwrap_or("stopped");
        return json!({"role": "assistant", "content": format!("[harness note] No answer was given ({why}).")});
    }
    if m.role == "assistant"
        && (meta["truncated"] == true
            || meta["stop_reason"] == "model_context_window_exceeded"
            || meta["stop_reason"] == "max_tokens")
    {
        let why = meta["stop_reason"].as_str().unwrap_or("max_tokens");
        let note =
            format!("[harness note] The preceding answer was cut off ({why}); it is incomplete.");
        let content = if m.content.trim().is_empty() {
            format!(
                "[harness note] No answer text was produced before generation was cut off ({why})."
            )
        } else {
            format!("{}\n\n{note}", m.content)
        };
        return json!({"role": "assistant", "content": content});
    }
    if m.role == "tool" {
        let origins: Vec<crate::provenance::Origin> =
            serde_json::from_value(meta["data_origins"].clone()).unwrap_or_default();
        let mut msg = crate::provenance::framed_tool(
            meta["data_source_tool"]
                .as_str()
                .or_else(|| meta["name"].as_str())
                .unwrap_or("unknown"),
            &origins,
            &m.content,
        );
        msg["tool_call_id"] = meta["tool_call_id"].clone();
        // A failed call: Anthropic's `tool_result.is_error` (a marker the
        // other APIs strip before sending).
        if meta["is_error"] == true {
            msg["_is_error"] = json!(true);
        }
        return msg;
    }
    if m.role == "assistant" && meta["tool_calls"].is_array() {
        // Any preamble before the calls stays: it is the model's stated plan.
        let content = Some(m.content.trim())
            .filter(|t| !t.is_empty())
            .map_or(Value::Null, |t| json!(t));
        let mut msg =
            json!({"role": "assistant", "content": content, "tool_calls": meta["tool_calls"]});
        // Echo reasoning back only when the provider emitted it (thinking
        // mode requires it on tool-call turns; other endpoints never see it).
        if let Some(r) = meta["reasoning_content"].as_str() {
            if !r.is_empty() {
                msg["reasoning_content"] = json!(r);
            }
        }
        // Anthropic's signed thinking (only that API reads it; the others
        // drop it before sending).
        if meta["thinking_blocks"].is_array() {
            msg["thinking_blocks"] = meta["thinking_blocks"].clone();
        }
        return msg;
    }
    // Attachments (SPEC §5.6): a reference line the model can follow with
    // read_artifact; images also carry a marker the provider resolves
    // into an image part for vision models.
    if m.role == "user" {
        if let Some(atts) = meta["attachments"].as_array().filter(|a| !a.is_empty()) {
            let mut content = m.content.clone();
            let mut images = Vec::new();
            for a in atts {
                if a["imported"] == true {
                    let name = serde_json::to_string(&crate::daemon::attachment_name(
                        a["name"].as_str().unwrap_or("file"),
                    ))
                    .unwrap_or_default();
                    content.push_str(&format!("\n[imported attachment {name} ({}, {} bytes); not attached: re-attach the file to allow model access]",a["mime"].as_str().unwrap_or("application/octet-stream"),a["bytes"]));
                    continue;
                }
                if a["reference"] == true {
                    let size = if a["dir"] == true {
                        "directory".to_string()
                    } else {
                        format!("{} bytes", a["bytes"])
                    };
                    content.push_str(&format!(
                        "\n[referenced: {} ({size}); not attached, read it with your Tools]",
                        a["path"].as_str().unwrap_or_default()
                    ));
                    continue;
                }
                // The name quoted (it is the user's, not the harness's), and
                // cleaned again for messages stored before intake did (#862).
                let name = serde_json::to_string(&crate::daemon::attachment_name(
                    a["name"].as_str().unwrap_or("file"),
                ))
                .unwrap_or_default();
                // Collected by retention: say so instead of sending the model
                // to read a file that is gone (#858).
                // Projection is pure: imported paths may name private files or
                // Windows UNC shares. Only explicit retention metadata decides
                // expiry; verified owned access diagnoses missing files later.
                let gone = a["expired"] == true;
                if gone {
                    content.push_str(&format!(
                        "\n[attachment {name} is no longer available: it was removed by artifact retention]"
                    ));
                    continue;
                }
                content.push_str(&format!(
                    "\n[attached: {name} ({}, {} bytes), artifact {}; read it with read_artifact]",
                    a["mime"].as_str().unwrap_or("application/octet-stream"),
                    a["bytes"],
                    a["id"].as_str().unwrap_or_default()
                ));
                // Only raster images go to a vision model (#855).
                if a["mime"].as_str().is_some_and(|mime| {
                    crate::daemon::is_raster_image(mime)
                        || mime == "application/pdf"
                        || mime.starts_with("audio/")
                }) {
                    images.push(json!({"id": a.get("_artifact_id").unwrap_or(&a["id"]), "_source_session":m.session_id,"_source_seq":m.seq,"path": a["path"], "mime": a["mime"], "name": name, "bytes":a["bytes"], "sha256":a["sha256"]}));
                }
            }
            let mut msg = json!({"role": "user", "content": content});
            if !images.is_empty() {
                msg["_attachments"] = json!(images);
            }
            return msg;
        }
    }
    let mut msg = json!({"role": m.role, "content": m.content});
    // A paused turn's thinking goes back when it is resumed (#1090).
    if m.role == "assistant" && meta["thinking_blocks"].is_array() {
        msg["thinking_blocks"] = meta["thinking_blocks"].clone();
    }
    msg
}

/// An adjacency tier only for supported separator-joined identifiers.
/// Every segment remains a prefix, including non-final segments. This asks
/// for adjacent indexed tokens, not literal punctuation or identifier equality.
fn identifier_prefix_phrase(text: &str) -> Option<String> {
    let separator = |c| matches!(c, '_' | '.' | '-' | '/');
    if !text.chars().any(separator)
        || text.chars().any(|c| {
            !c.is_alphanumeric()
                && !unicode_normalization::char::is_combining_mark(c)
                && !separator(c)
        })
    {
        return None;
    }
    let terms = fts_query(text)?;
    terms.contains(' ').then(|| terms.replace(' ', " + "))
}

fn search_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

/// An FTS5 query from free text: each word quoted as a prefix term, all
/// required. None when the text has no word characters.
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = search_words(text).map(|w| format!("\"{w}\"*")).collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod migration_failure_tests {
    use super::*;

    #[cfg(unix)]
    mod unprivileged {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/unprivileged.rs"
        ));
    }

    fn failing_fixture() -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("fl-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("formless.db");
        let store = Store::open(&file).unwrap();
        store.create_session(Some("preserved".into())).unwrap();
        drop(store);
        let conn = Connection::open(&file).unwrap();
        conn.execute_batch("DROP TABLE run_usage; PRAGMA user_version=50;")
            .unwrap();
        drop(conn);
        (base, file)
    }

    fn backups(base: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(base)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "bak"))
            .collect()
    }

    fn fails_at_52(file: &Path) {
        let error = Store::open(file).err().expect("step 52 must fail");
        let message = format!("{error:#}");
        assert!(message.contains("migrate database to v52"), "{message}");
        assert!(!message.contains("output file already exists"), "{message}");
        let conn = Connection::open(file).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            51
        );
        assert_eq!(
            conn.query_row("SELECT name FROM sessions", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "preserved"
        );
    }

    #[test]
    fn migration_failure_retries_reuse_the_original_recovery_point() {
        let (base, file) = failing_fixture();
        fails_at_52(&file);
        let initial = backups(&base);
        assert_eq!(initial.len(), 1);
        let snapshot = std::fs::read(&initial[0]).unwrap();
        let backup = Connection::open(&initial[0]).unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            50
        );
        drop(backup);
        // Immediate retries previously collided with the second-resolution
        // filename; a later retry previously made another full snapshot.
        fails_at_52(&file);
        fails_at_52(&file);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        fails_at_52(&file);
        assert_eq!(backups(&base), initial);
        assert_eq!(std::fs::read(&initial[0]).unwrap(), snapshot);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn interrupted_attempt_reuses_its_verified_initial_snapshot() {
        let (base, file) = failing_fixture();
        let conn = Connection::open(&file).unwrap();
        conn.execute_batch(PRAGMAS).unwrap();
        let initial = backup_before_migration(&conn, &file, 50).unwrap();
        // Simulate losing the process after backup, before the first step.
        drop(conn);
        fails_at_52(&file);
        assert_eq!(backups(&base), [initial]);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn interrupted_after_a_committed_step_preserves_the_original_backup() {
        let (base, file) = failing_fixture();
        let conn = Connection::open(&file).unwrap();
        conn.execute_batch(PRAGMAS).unwrap();
        let initial = backup_before_migration(&conn, &file, 50).unwrap();
        let (_, step) = MIGRATIONS
            .iter()
            .find(|(version, _)| *version == 51)
            .unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        step(&tx).unwrap();
        tx.execute_batch("PRAGMA user_version=51;").unwrap();
        tx.commit().unwrap();
        // A crash before the receipt is updated means the changed state
        // receives a fresh recovery point; the original is never replaced.
        drop(conn);
        fails_at_52(&file);
        assert!(initial.exists());
        assert_eq!(backups(&base).len(), 2);
        fails_at_52(&file);
        assert_eq!(
            backups(&base).len(),
            2,
            "subsequent unchanged retry reuses the recovery point"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn changed_source_and_damaged_snapshot_disable_reuse() {
        let (base, file) = failing_fixture();
        fails_at_52(&file);
        let first = backups(&base).pop().unwrap();
        // A receipt is only a hint: changed backup bytes cannot authorize
        // skipping a fresh safety copy.
        std::fs::write(&first, "damaged").unwrap();
        fails_at_52(&file);
        assert_eq!(backups(&base).len(), 2);
        assert_eq!(std::fs::read(&first).unwrap(), b"damaged");
        let conn = Connection::open(&file).unwrap();
        conn.execute_batch(
            "CREATE TABLE external_change(value TEXT); INSERT INTO external_change VALUES('new');",
        )
        .unwrap();
        drop(conn);
        fails_at_52(&file);
        assert_eq!(
            backups(&base).len(),
            3,
            "changed database needs its own recovery point"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn busy_checkpoint_creates_a_fresh_backup_without_replacing_the_original() {
        let (base, file) = failing_fixture();
        let conn = Connection::open(&file).unwrap();
        conn.execute_batch(PRAGMAS).unwrap();
        conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let initial = backup_before_migration(&conn, &file, 50).unwrap();
        let original = std::fs::read(&initial).unwrap();
        let reader = Connection::open(&file).unwrap();
        reader.execute_batch("BEGIN;").unwrap();
        reader
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap();
        conn.execute_batch("CREATE TABLE concurrent_change(value TEXT);")
            .unwrap();
        assert!(migration_state_sha256(&conn, &file).unwrap().is_none());
        let fresh = backup_before_migration(&conn, &file, 50).unwrap();
        assert_ne!(initial, fresh);
        assert!(fresh.exists());
        assert_eq!(std::fs::read(&initial).unwrap(), original);
        drop(reader);
        drop(conn);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn receipt_write_failure_preserves_the_backup_and_original_migration_error() {
        let (base, file) = failing_fixture();
        std::fs::create_dir(migration_receipt_path(&file)).unwrap();
        fails_at_52(&file);
        assert_eq!(backups(&base).len(), 1);
        let _ = std::fs::remove_dir_all(base);
    }

    #[cfg(unix)]
    #[test]
    fn backup_failure_in_a_read_only_directory_prevents_all_migration_steps() {
        unprivileged::run(
            "storage::migration_failure_tests::backup_failure_in_a_read_only_directory_prevents_all_migration_steps",
            || {
                use std::os::unix::fs::PermissionsExt;
                let (base, file) = failing_fixture();
                // Keep already-created WAL/SHM handles available: only creating the
                // adjacent backup should fail when the directory becomes read-only.
                let conn = Connection::open(&file).unwrap();
                conn.execute_batch(PRAGMAS).unwrap();
                conn.execute_batch("BEGIN IMMEDIATE; COMMIT;").unwrap();
                struct RestorePermissions(PathBuf, std::fs::Permissions);
                impl Drop for RestorePermissions {
                    fn drop(&mut self) {
                        let _ = std::fs::set_permissions(&self.0, self.1.clone());
                    }
                }
                let probe = base.join("direct-write-probe");
                std::fs::write(&probe, "writable before restriction").unwrap();
                std::fs::remove_file(&probe).unwrap();
                let permissions = std::fs::metadata(&base).unwrap().permissions();
                let restore_permissions = RestorePermissions(base.clone(), permissions);
                std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o555)).unwrap();
                let denied = std::fs::write(&probe, "must fail").unwrap_err();
                assert_eq!(denied.kind(), std::io::ErrorKind::PermissionDenied);
                assert!(!probe.exists());
                let schema_before = conn
                    .query_row("SELECT group_concat(sql, ';') FROM sqlite_master", [], |r| r.get::<_, String>(0))
                    .unwrap();
                let error = Store::open(&file).err().expect("backup creation must fail");
                assert!(
                    format!("{error:#}").contains("back up database to"),
                    "{error:#}"
                );
                assert_eq!(
                    conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                        .unwrap(),
                    50
                );
                assert_eq!(
                    conn.query_row("SELECT name FROM sessions", [], |r| r.get::<_, String>(0))
                        .unwrap(),
                    "preserved"
                );
                assert!(backups(&base).is_empty());
                assert_eq!(
                    conn.query_row("SELECT group_concat(sql, ';') FROM sqlite_master", [], |r| r.get::<_, String>(0)).unwrap(),
                    schema_before,
                    "no migration step may change the schema after backup refusal"
                );
                drop(restore_permissions);
                drop(conn);
                let _ = std::fs::remove_dir_all(base);
            },
        );
    }
}

#[cfg(test)]
mod snippet_tests {
    use super::make_snippet;

    /// The snippet centres on the match even when folding changes the
    /// text's length, typed in either normal form (#789).
    #[test]
    fn snippets_centre_on_folded_matches() {
        let text = format!("{}İstanbul trip{}", "x".repeat(100), "y".repeat(100));
        for needle in ["i\u{307}stanbul", "istanbul"] {
            let s = make_snippet(&text, needle);
            assert!(s.contains("İstanbul trip"), "{needle}: {s}");
            let before = s.split("İstanbul").next().unwrap().trim_start_matches('…');
            assert_eq!(before.chars().count(), 40, "{needle}: {s}");
        }
        let s = make_snippet("a café here", "cafe\u{301}");
        assert!(s.contains("café"), "{s}");
    }
}

#[cfg(test)]
mod forecast_retention_tests {
    use super::*;
    #[test]
    fn physical_request_basis_cascades_with_run_and_ignores_non_run_owners() {
        let st = Store::open_in_memory().unwrap();
        st.record_forecast_request("not-a-Run", "llm").unwrap();
        let sid = st.create_session(None).unwrap().id;
        let run = st.create_run(&sid).unwrap();
        st.record_forecast_request(&run, "run").unwrap();
        assert_eq!(
            st.conn
                .query_row("SELECT COUNT(*) FROM forecast_request_counts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        st.delete_session(&sid).unwrap();
        assert_eq!(
            st.conn
                .query_row("SELECT COUNT(*) FROM forecast_request_counts", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn context_projection_does_not_invent_null_tool_calls_on_plain_messages() {
        let st = Store::open_in_memory().unwrap();
        let sid = st.create_session(None).unwrap().id;
        let message = st.append_message(&sid, "user", "task").unwrap();
        let context = build_context(&st, &sid, "system", 100_000, 0.8).unwrap().0;
        assert_eq!(context.last().unwrap(), &provider_message(&message));
        assert!(!context
            .last()
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("tool_calls"));
    }
}

#[cfg(test)]
#[path = "description_store_tests.rs"]
mod description_store_tests;

#[cfg(test)]
#[path = "session_lineage_tests.rs"]
mod session_lineage_tests;

#[cfg(test)]
#[path = "attachment_entry_count_tests.rs"]
mod attachment_entry_count_tests;

#[path = "session_comparison.rs"]
mod session_comparison;
pub(crate) use session_comparison::SessionComparisonFailure;

#[path = "branch_summary_storage.rs"]
mod branch_summary_storage;
pub(crate) use branch_summary_storage::{
    summary_hash, BranchSummaryConfirmation, BranchSummaryFailure, BranchSummarySnapshot,
};

#[cfg(test)]
#[path = "session_comparison_tests.rs"]
mod session_comparison_tests;

#[path = "review_storage.rs"]
mod review_storage;
pub(crate) use review_storage::ReviewFailure;

#[cfg(test)]
mod native_cost_basis_tests {
    use super::*;
    #[test]
    fn schema62_keeps_legacy_prices_and_durable_unknown_native_cost_with_reported_precedence() {
        let root = std::env::temp_dir().join(format!("fl-native-cost-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("formless.db");
        let old = schema_at(61).unwrap();
        // schema_at constructs historical DDL without promising a final
        // user_version; the on-disk upgrade fixture must identify itself.
        old.pragma_update(None, "user_version", 61).unwrap();
        assert_eq!(
            old.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            61
        );
        let native_cost_columns: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('run_usage') WHERE name='cost_unknown'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(native_cost_columns, 0, "fixture must predate schema 62");
        // Schema 61 requires the physical token counters even for a legacy
        // text-only row; it has no native-cost-basis column yet.
        old.execute("INSERT INTO run_usage(session_id,run_id,model,prompt_tokens,completion_tokens,reasoning_tokens,cached_tokens,cache_write_tokens,created_at) VALUES('s','legacy','model',10,5,0,0,0,'2026-11-01')",[]).unwrap();
        old.execute("VACUUM INTO ?1", [file.to_str().unwrap()])
            .unwrap();
        drop(old);
        let mut cfg = crate::Config::default();
        cfg.provider.prices.insert(
            "model".into(),
            crate::config::ModelPrice {
                input: 1_000_000.0,
                output: 2_000_000.0,
                ..Default::default()
            },
        );
        let store = Store::open(&file).unwrap();
        assert_eq!(
            store
                .conn
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        let metadata_columns: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('run_files') WHERE name='before_metadata'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(metadata_columns, 1);
        let legacy = store.usage_rows(None, Some("legacy")).unwrap().0;
        assert_eq!(legacy.len(), 1);
        assert!(!legacy[0].7);
        assert_eq!(store.forecast_usage("legacy", &cfg).unwrap().0, Some(20.0));
        store
            .record_usage_full(
                "s",
                "unknown",
                "default",
                "model",
                &crate::provider::Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    cost_unknown: true,
                    reported: 3,
                    ..Default::default()
                },
                "run",
            )
            .unwrap();
        store
            .record_usage_full(
                "s",
                "reported",
                "default",
                "model",
                &crate::provider::Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    cost: Some(0.25),
                    cost_unknown: true,
                    reported: 3,
                    ..Default::default()
                },
                "run",
            )
            .unwrap();
        store
            .record_usage_full(
                "s",
                "reported",
                "default",
                "model",
                &crate::provider::Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    reported: 3,
                    ..Default::default()
                },
                "llm",
            )
            .unwrap();
        assert_eq!(store.forecast_usage("unknown", &cfg).unwrap().0, None);
        assert_eq!(
            store.forecast_usage("reported", &cfg).unwrap().0,
            Some(20.25)
        );
        let rows = store.usage_rows(None, Some("reported")).unwrap().0;
        assert_eq!(
            rows.iter()
                .map(|(p, m, i, o, c, w, r, u)| cfg.cost_of_known(p, m, *i, *o, *c, *w, *r, *u))
                .sum::<Option<f64>>(),
            Some(20.25),
            "mixed estimated/reported groups preserve actual USD"
        );
        drop(store);
        cfg.provider.prices.get_mut("model").unwrap().input = 0.0;
        let store = Store::open(&file).unwrap();
        assert!(store.usage_rows(None, Some("unknown")).unwrap().0[0].7);
        assert_eq!(store.forecast_usage("unknown", &cfg).unwrap().0, None);
        assert_eq!(
            store.forecast_usage("reported", &cfg).unwrap().0,
            Some(10.25)
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[path = "routing_storage.rs"]
mod routing_storage;

#[path = "storage_tool_compatibility.rs"]
mod tool_compatibility_store;

#[cfg(test)]
mod context_watermark_tests {
    use super::*;

    #[test]
    fn context_boundary_credits_only_built_rows_and_retains_checkpoint_boundary() {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        let mut cache = ContextCache::default();
        assert_eq!(cache.last_included_seq(), 0);
        let first = store
            .append_message(&session, "user", "first request")
            .unwrap();
        let filtered = store
            .append_message_meta(
                &session,
                "assistant",
                "",
                Some(
                    json!({"tool_calls":[{"id":"invalid-call","function":{"arguments":"{}"}}]})
                        .to_string(),
                ),
            )
            .unwrap();
        let built = cache
            .build(&store, &session, "private prompt", 100_000, 0.8)
            .unwrap();
        assert_eq!(cache.last_included_seq(), filtered.seq);
        assert!(!serde_json::to_string(&built.0)
            .unwrap()
            .contains("invalid-call"));
        assert!(serde_json::to_string(&built.0)
            .unwrap()
            .contains(&first.content));
        let unseen = store
            .append_message(&session, "user", "not in the frozen request")
            .unwrap();
        assert_eq!(
            cache.last_included_seq(),
            filtered.seq,
            "new DB rows cannot retroactively advance a built context watermark"
        );
        cache
            .build(&store, &session, "private prompt", 100_000, 0.8)
            .unwrap();
        assert_eq!(cache.last_included_seq(), unseen.seq);
        store
            .save_checkpoint(&session, "private compacted data", unseen.seq)
            .unwrap();
        cache
            .build(&store, &session, "private prompt", 100_000, 0.8)
            .unwrap();
        assert!(cache.live_seqs.is_empty());
        assert_eq!(
            cache.last_included_seq(),
            unseen.seq,
            "an empty live tail still includes the recorded checkpoint boundary"
        );
        store
            .append_message(&session, "user", "after checkpoint")
            .unwrap();
        assert_eq!(cache.last_included_seq(), unseen.seq);
    }
}

#[cfg(test)]
mod narrow_transcript_tests {
    use super::*;

    fn assert_projection_matches_full(store: &Store, session: &str) {
        let full = store.list_messages(session).unwrap();
        assert_eq!(
            store.last_message_seq(session).unwrap(),
            full.last().map_or(0, |row| row.seq)
        );
        let last_user = full.iter().rev().find(|row| row.role == "user");
        assert_eq!(
            store.last_user_message(session).unwrap(),
            last_user.map(|row| (row.id.clone(), row.seq, row.content.clone()))
        );
        let users: Vec<_> = full
            .iter()
            .filter(|row| row.role == "user" && !row.content.trim().is_empty())
            .map(|row| row.content.as_str())
            .collect();
        let legacy = users[users.len().saturating_sub(4)..]
            .join("\n")
            .chars()
            .take(2000)
            .collect::<String>();
        assert_eq!(store.recent_user_naming_input(session).unwrap(), legacy);
    }

    #[test]
    fn narrow_transcript_reads_keep_unicode_cap_and_empty_latest_user() {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        let other = store.create_session(None).unwrap().id;
        store
            .append_message(&other, "user", "not this Session")
            .unwrap();
        assert_projection_matches_full(&store, &session);
        store
            .append_message(&session, "assistant", "no user yet")
            .unwrap();
        assert_projection_matches_full(&store, &session);

        let long_user = "ก中".repeat(1800);
        let long_tool = "unrelated tool output".repeat(120_000);
        for (role, content) in [
            ("user", "excluded oldest"),
            ("assistant", "assistant"),
            ("user", "oldest kept"),
            ("user", "\t\n"),
            ("user", long_user.as_str()),
            ("tool", long_tool.as_str()),
            ("user", "third kept"),
            ("user", "\u{2003}\u{00A0}"),
            ("user", "fourth kept"),
            ("user", ""),
            ("system-note", "last sequence is not the newest user"),
        ] {
            store.append_message(&session, role, content).unwrap();
        }
        assert_projection_matches_full(&store, &session);
        let recent = store.recent_user_naming_input(&session).unwrap();
        assert_eq!(recent.chars().count(), 2000);
        assert!(recent.starts_with("oldest kept\nก中"));
        assert!(store
            .last_user_message(&session)
            .unwrap()
            .unwrap()
            .2
            .is_empty());
    }

    #[test]
    fn narrow_transcript_reads_match_equal_sequences_in_both_directions() {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        for (id, role, content) in [
            ("old", "user", "discarded"),
            ("z", "user", "first kept"),
            ("y", "user", "second kept"),
            ("x", "assistant", "ignored"),
            ("w", "user", "third kept"),
            ("v", "user", "last kept"),
            ("u", "user", "\u{2003}"),
        ] {
            store.conn.execute(
                "INSERT INTO messages(id,session_id,seq,role,content,created_at) VALUES(?1,?2,1,?3,?4,'fixture')",
                params![id, session, role, content],
            ).unwrap();
        }
        assert_projection_matches_full(&store, &session);
        assert_eq!(
            store.last_user_message(&session).unwrap(),
            Some(("u".into(), 1, "\u{2003}".into()))
        );
        assert_eq!(
            store.recent_user_naming_input(&session).unwrap(),
            "first kept\nsecond kept\nthird kept\nlast kept"
        );
    }
}

#[cfg(test)]
mod user_naming_text_tests {
    use super::*;
    use crate::namer::{naming_text, try_naming_text_from_contents};
    use std::cell::Cell;
    use unicode_segmentation::UnicodeSegmentation;

    const NAMING_TEXT_MAX_CHARS: usize = 1200;

    fn marked(token: &str) -> String {
        format!("{token}{}", "x".repeat(16_384))
    }

    fn long_multibyte() -> String {
        format!("{}ก{}", "a".repeat(799), "b".repeat(401))
    }

    fn huge_grapheme() -> String {
        format!("a{}", "\u{0301}".repeat(5_000))
    }

    fn user_contents(store: &Store, session_id: &str) -> Vec<String> {
        store
            .list_messages(session_id)
            .unwrap()
            .into_iter()
            .filter(|message| message.role == "user")
            .map(|message| message.content)
            .collect()
    }

    #[track_caller]
    fn assert_same(store: &Store, session_id: &str, label: &str) -> Option<String> {
        let got = store.user_naming_text(session_id).unwrap();
        if let Some(text) = &got {
            assert!(
                text.chars().count() <= NAMING_TEXT_MAX_CHARS,
                "{label} naming text is unbounded ({})",
                text.chars().count()
            );
        }
        assert_eq!(
            got,
            naming_text(&user_contents(store, session_id)),
            "{label}"
        );
        got
    }

    #[track_caller]
    fn assert_head_tail(got: &str, original: &str) {
        let got: Vec<char> = got.chars().collect();
        let original: Vec<char> = original.chars().collect();
        assert!(original.len() > 1200);
        assert_eq!(got.len(), NAMING_TEXT_MAX_CHARS);
        // The separator consumes three characters of the 1200-character budget.
        assert_eq!(&got[..797], &original[..797]);
        assert_eq!(&got[797..800], &['\n', '\u{2026}', '\n']);
        assert_eq!(&got[800..], &original[original.len() - 400..]);
    }

    #[test]
    fn user_naming_text_preserves_exact_unicode_head_and_tail() {
        let store = Store::open_in_memory().unwrap();
        let head = "日🦀e\u{0301}".repeat(200);
        let tail = "ก🦀\u{0301}日".repeat(100);
        let long = format!("{head}discarded middle{tail}");
        let id = transcript(&store, &[("user", &long)]);
        let got = assert_same(&store, &id, "Unicode head and tail").unwrap();
        assert_head_tail(&got, &long);
        assert!(got.starts_with('日'));
        assert!(got.ends_with(&tail));
    }

    fn transcript(store: &Store, rows: &[(&str, &str)]) -> String {
        let id = store.create_session(None).unwrap().id;
        for (role, content) in rows {
            store.append_message(&id, role, content).unwrap();
        }
        id
    }

    fn insert_user_at_seq(store: &Store, session_id: &str, seq: i64, content: &str) {
        store
            .conn
            .execute(
                "INSERT INTO messages(id,session_id,seq,role,content,created_at,meta) \
                 VALUES(?1,?2,?3,'user',?4,'2020-01-01T00:00:00Z',NULL)",
                rusqlite::params![uuid::Uuid::new_v4().to_string(), session_id, seq, content],
            )
            .unwrap();
    }

    #[test]
    fn user_naming_text_matches_naming_text_over_synthetic_rows() {
        let store = Store::open_in_memory().unwrap();
        let assistant = marked("ASSISTANT_ONLY");
        let tool = marked("TOOL_ONLY");
        let late = marked("LATE_ONLY");
        let cluster = huge_grapheme();
        let eleven = "e\u{0301}".repeat(11);
        assert_eq!(cluster.graphemes(true).count(), 1);
        assert!(cluster.chars().count() > 1200);
        assert_eq!(eleven.graphemes(true).count(), 11);
        assert!(eleven.chars().count() > 12);
        let substantive = "convert this csv to json\nkeep the header row";
        let mixed = transcript(
            &store,
            &[
                ("assistant", &assistant),
                ("tool", &tool),
                ("user", " \u{00A0}\u{3000}\u{2003}\t\r\n"),
                ("user", "  hi  "),
                ("user", &eleven),
                ("user", "\nab\n"),
                ("user", "\u{200B}"),
                ("user", &cluster),
                ("user", substantive),
                ("user", &late),
                ("assistant", &assistant),
            ],
        );
        let got = assert_same(&store, &mixed, "mixed").unwrap();
        // The first uncertain short request is now substantive, even when
        // it consists of eleven combining graphemes.
        assert_eq!(got, eleven);
        assert!(!got.contains("ASSISTANT_ONLY"));
        assert!(!got.contains("TOOL_ONLY"));
        assert!(!got.contains("LATE_ONLY"));
        assert!(got.contains('\u{0301}'));

        let later = "summarise the quarterly notes in one page";
        let earliest = "please convert this csv export into json";
        let ordered = store.create_session(None).unwrap().id;
        store.append_message(&ordered, "user", later).unwrap();
        store.append_message(&ordered, "user", &late).unwrap();
        store
            .append_message(&ordered, "assistant", &assistant)
            .unwrap();
        insert_user_at_seq(&store, &ordered, -1, " \u{00A0}\u{3000}\n");
        insert_user_at_seq(&store, &ordered, 0, earliest);
        let users = user_contents(&store, &ordered);
        assert!(users[0].trim().is_empty());
        assert_eq!(users[1], earliest);
        assert_eq!(
            assert_same(&store, &ordered, "seq order").as_deref(),
            Some(earliest)
        );

        let long = long_multibyte();
        assert_eq!(long.chars().count(), 1201);
        let long_id = transcript(
            &store,
            &[
                ("assistant", &assistant),
                ("user", "  hey  "),
                ("user", &long),
                ("tool", &tool),
                ("user", &late),
            ],
        );
        let got = assert_same(&store, &long_id, "long").unwrap();
        assert_head_tail(&got, &long);
        assert!(!got.contains("LATE_ONLY"));
        assert!(!got.contains("ASSISTANT_ONLY"));

        let exact = "b".repeat(1200);
        let exact_id = transcript(&store, &[("user", &exact), ("user", &late)]);
        let got = assert_same(&store, &exact_id, "exact 1200").unwrap();
        assert_eq!(got, exact);
        assert!(!got.contains('\u{2026}'));

        let thai11 = "ก".repeat(11);
        let thai12 = "ก".repeat(12);
        assert_eq!(thai11.graphemes(true).count(), 11);
        assert_eq!(thai12.graphemes(true).count(), 12);
        assert_eq!("hello there".graphemes(true).count(), 11);
        let graphemes = transcript(
            &store,
            &[
                ("user", " \u{00A0}\n"),
                ("user", &eleven),
                ("user", "hello there"),
                ("user", &thai11),
                ("user", &thai12),
                ("user", &late),
                ("assistant", &assistant),
            ],
        );
        let got = assert_same(&store, &graphemes, "short combining request").unwrap();
        assert_eq!(got, eleven);
        assert!(got.contains('\u{0301}'));
        assert!(!got.contains("LATE_ONLY"));

        let short_nl = transcript(
            &store,
            &[
                ("tool", &tool),
                ("user", "hi"),
                ("user", "go\non"),
                ("user", &late),
            ],
        );
        assert_eq!(
            assert_same(&store, &short_nl, "short newline").as_deref(),
            Some("go\non")
        );
    }

    #[test]
    fn user_naming_text_retains_uncertain_short_requests_before_later_logs() {
        let store = Store::open_in_memory().unwrap();
        let late = marked("LATE_ONLY");
        for request in ["yo", "ab", "Fix the CI", "修复登录错误", "กกก", "e\u{0301}"] {
            let id = transcript(
                &store,
                &[("user", "hi"), ("user", request), ("user", &late)],
            );
            assert_eq!(
                assert_same(&store, &id, "short request").as_deref(),
                Some(request)
            );
        }
    }

    #[test]
    fn user_naming_text_none_and_all_trivial_users() {
        let store = Store::open_in_memory().unwrap();
        let assistant = marked("ASSISTANT_ONLY");
        let tool = marked("TOOL_ONLY");
        let cluster = huge_grapheme();
        assert_eq!(cluster.graphemes(true).count(), 1);

        let absent = transcript(&store, &[("assistant", &assistant), ("tool", &tool)]);
        assert_eq!(assert_same(&store, &absent, "no users"), None);

        let blank = " \u{00A0}\u{3000}\u{2003}\t\r\n";
        assert!(blank.trim().is_empty());
        let whitespace = transcript(
            &store,
            &[("user", ""), ("user", blank), ("assistant", &assistant)],
        );
        assert_eq!(assert_same(&store, &whitespace, "whitespace users"), None);

        let trivial = transcript(
            &store,
            &[
                ("user", blank),
                ("user", "  hi  "),
                ("user", "ok"),
                ("user", "thank you"),
                ("user", "!!!"),
                ("user", "hello"),
                ("tool", &tool),
            ],
        );
        let got = assert_same(&store, &trivial, "all trivial").unwrap();
        assert_eq!(got, "hi");
        assert!(!got.contains('\u{0301}'));

        let trimmed = transcript(&store, &[("user", "\nab\n"), ("assistant", &assistant)]);
        assert_eq!(
            assert_same(&store, &trimmed, "trimmed newline").as_deref(),
            Some("ab")
        );

        let zwsp = transcript(&store, &[("user", "\u{200B}")]);
        assert_eq!(
            assert_same(&store, &zwsp, "zwsp").as_deref(),
            Some("\u{200B}")
        );

        let only_cluster = transcript(
            &store,
            &[
                ("assistant", &assistant),
                ("user", blank),
                ("user", &cluster),
                ("tool", &tool),
            ],
        );
        let got = assert_same(&store, &only_cluster, "huge single grapheme").unwrap();
        assert_head_tail(&got, &cluster);
    }

    #[test]
    fn user_naming_text_does_not_decode_discarded_rows_or_columns() {
        let store = Store::open_in_memory().unwrap();
        let text = "please summarise this synthetic document";
        let id = transcript(
            &store,
            &[("assistant", "ignored"), ("user", text), ("user", "later")],
        );
        // Synthetic invalid values are read probes, never real session data.
        // The old full-row decoder reads metadata and every later content.
        store
            .conn
            .execute(
                "UPDATE messages SET meta='not-json' WHERE session_id=?1 AND role='assistant'",
                [&id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE messages SET content=x'ff' WHERE session_id=?1 AND content='later'",
                [&id],
            )
            .unwrap();
        assert_eq!(store.user_naming_text(&id).unwrap().as_deref(), Some(text));
        assert!(store.list_messages(&id).is_err());
    }

    /// `last_relevant` is the last index the selector may pull. `pulls` counts
    /// the terminating `None` only when the scan exhausts the iterator.
    #[track_caller]
    fn scanned(rows: &[String], last_relevant: usize, pulls: usize) -> Option<String> {
        let seen = Cell::new(0usize);
        let got = try_naming_text_from_contents(std::iter::from_fn(|| {
            let index = seen.get();
            if index > last_relevant {
                panic!("selector read user row {index}; last relevant index is {last_relevant}");
            }
            seen.set(index + 1);
            rows.get(index)
                .map(|row| Ok::<&str, &'static str>(row.as_str()))
        }))
        .expect("probe");
        assert_eq!(seen.get(), pulls, "pulled user rows");
        if let Some(text) = &got {
            assert!(text.chars().count() <= NAMING_TEXT_MAX_CHARS);
        }
        assert_eq!(got, naming_text(rows));
        got
    }

    #[test]
    fn user_naming_text_stops_before_irrelevant_late_rows() {
        assert_eq!(scanned(&[], 0, 1), None);

        let all_trivial = vec![
            " \u{00A0}\u{3000}\n".to_string(),
            "  hi  ".to_string(),
            "ok".to_string(),
            "!!!".to_string(),
        ];
        assert_eq!(
            scanned(&all_trivial, all_trivial.len(), all_trivial.len() + 1).as_deref(),
            Some("hi")
        );

        let twelve = "ก".repeat(12);
        assert_eq!(twelve.graphemes(true).count(), 12);
        let late = marked("LATE_ONLY");
        let graphemes = vec![
            " \u{00A0}".to_string(),
            "e\u{0301}".repeat(11),
            "hello there".to_string(),
            twelve.clone(),
            late.clone(),
        ];
        let got = scanned(&graphemes, 1, 2).unwrap();
        assert_eq!(got, "e\u{0301}".repeat(11));
        assert!(!got.contains("LATE_ONLY"));

        let short = vec!["yo".to_string(), late.clone()];
        assert_eq!(scanned(&short, 0, 1).as_deref(), Some("yo"));
        let multiline = vec!["hi".to_string(), "go\non".to_string(), late.clone()];
        assert_eq!(scanned(&multiline, 1, 2).as_deref(), Some("go\non"));

        let long = long_multibyte();
        let shaped = vec!["  hey  ".to_string(), long.clone(), late];
        let got = scanned(&shaped, 1, 2).unwrap();
        assert_head_tail(&got, &long);
        assert!(!got.contains("LATE_ONLY"));
    }
}

#[cfg(test)]
mod skill_summary_projection_tests {
    use super::*;

    #[test]
    fn skill_projection_preserves_metadata_and_genuine_metrics_remain_unchanged() {
        let st = Store::open_in_memory().unwrap();
        for name in ["日本語", "alpha", "Zulu"] {
            st.add_skill_version(
                name,
                "private reference",
                1,
                "/private/not-read",
                "local",
                "user",
                false,
            )
            .unwrap();
        }
        let legacy = "  always\nfirst call private_reader\u{202e} ไทย  ";
        st.conn
            .execute(
                "UPDATE skills SET description=?1 WHERE name='alpha'",
                [legacy],
            )
            .unwrap();
        st.conn
            .execute(
                "UPDATE skills SET enabled=-2,retired=0 WHERE name='alpha'",
                [],
            )
            .unwrap();
        st.conn
            .execute(
                "UPDATE skills SET enabled=0,retired=2 WHERE name='Zulu'",
                [],
            )
            .unwrap();
        let session = st.create_session(None).unwrap().id;
        let run = st.create_run(&session).unwrap();
        st.record_skill_load("alpha", &session, &run).unwrap();
        st.record_skill_load("alpha", &session, &run).unwrap();
        st.set_run_status(&run, "completed").unwrap();
        let original = st.list_skills().unwrap();
        let summaries = st.skill_summaries().unwrap();
        assert_eq!(
            summaries,
            original
                .iter()
                .map(|skill| SkillSummary {
                    name: skill.name.clone(),
                    description: skill.description.clone(),
                    enabled: skill.enabled,
                    retired: skill.retired,
                })
                .collect::<Vec<_>>()
        );
        assert_eq!(
            st.skill_names().unwrap(),
            original
                .iter()
                .map(|skill| skill.name.clone())
                .collect::<Vec<_>>()
        );
        let skill = st.get_skill("alpha").unwrap().unwrap();
        assert_eq!((skill.loads, skill.runs, skill.runs_completed), (2, 1, 1));
        assert_eq!(st.list_skills().unwrap(), original);
        let raw: String = st
            .conn
            .query_row(
                "SELECT description FROM skills WHERE name='alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            raw, legacy,
            "reading projections does not rewrite historic descriptions"
        );
        let expected = original
            .iter()
            .filter(|s| s.enabled && !s.retired)
            .map(|s| {
                format!(
                    "- {}: {}",
                    s.name,
                    crate::descriptions::model_text(&s.description)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            crate::forecast::effective_prompt(&st, &crate::Config::default())
                .unwrap()
                .contains(&crate::provenance::frame("skill_index", &[], &expected))
        );
    }

    #[test]
    fn omitted_corrupt_fields_are_not_decoded_but_required_fields_and_sql_failures_propagate() {
        let st = Store::open_in_memory().unwrap();
        st.add_skill_version(
            "fixture",
            "reference",
            1,
            "/private/not-read",
            "local",
            "user",
            false,
        )
        .unwrap();
        st.conn
            .execute("UPDATE skills SET has_scripts='malformed-unused'", [])
            .unwrap();
        assert!(st.list_skills().is_err());
        assert_eq!(st.skill_summaries().unwrap().len(), 1);
        assert_eq!(st.skill_names().unwrap(), ["fixture"]);
        st.conn
            .execute("UPDATE skills SET description=x'FF'", [])
            .unwrap();
        assert!(st.skill_summaries().is_err());
        assert_eq!(st.skill_names().unwrap(), ["fixture"]);
        st.conn
            .execute("UPDATE skills SET description='reference'", [])
            .unwrap();
        st.conn.execute("DROP TABLE skill_loads", []).unwrap();
        assert_eq!(
            st.skill_summaries().unwrap().len(),
            1,
            "projection does not depend on history aggregates"
        );
        assert_eq!(st.skill_names().unwrap(), ["fixture"]);
        st.conn
            .execute("ALTER TABLE skills RENAME TO unavailable_skills", [])
            .unwrap();
        assert!(st.skill_summaries().is_err());
        assert!(st.skill_names().is_err());
    }
}

#[cfg(test)]
mod recent_failure_order_tests {
    use super::*;

    #[test]
    fn recent_failures_break_timestamp_ties_by_recording_order() {
        let store = Store::open_in_memory().unwrap();
        let tool = store
            .create_tool_version(
                "failure_order",
                "fixture",
                "{}",
                "{}",
                "function run(i){return i}",
                &Capabilities::default(),
                64,
                "agent",
            )
            .unwrap()
            .0
            .id;
        let insert = |tag: &str, timestamp: &str, status: &str| {
            store.conn.execute(
                "INSERT INTO invocations(id,tool_id,version,status,tag,input,error,created_at) VALUES(?1,?2,1,?3,?1,?4,?5,?6)",
                params![tag,tool,status,r#"{"text":"ไทย 日本語"}"#,"ผิดพลาด\n詳細",timestamp],
            ).unwrap();
        };
        insert("first", "2026-01-02T00:00:00Z", "failure");
        insert("second", "2026-01-02T00:00:00Z", "failure");
        insert("third", "2026-01-02T00:00:00Z", "failure");
        assert_eq!(store.recent_failures(&tool, 1).unwrap()[0].1, "third");
        insert("older_recorded_later", "2026-01-01T00:00:00Z", "failure");
        insert("success", "2026-01-04T00:00:00Z", "success");
        insert("running", "2026-01-04T00:00:00Z", "running");
        insert("newest", "2026-01-03T00:00:00Z", "failure");
        let rows = store.recent_failures(&tool, 10).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.1.as_str()).collect::<Vec<_>>(),
            ["newest", "third", "second", "first", "older_recorded_later"]
        );
        assert_eq!(store.recent_failures(&tool, 3).unwrap(), rows[..3]);
        assert!(store.recent_failures(&tool, 0).unwrap().is_empty());
        assert!(store.recent_failures("other_tool", 10).unwrap().is_empty());
        for (version, _, input, error) in rows {
            assert_eq!(version, 1);
            assert_eq!(input, r#"{"text":"ไทย 日本語"}"#);
            assert_eq!(error, "ผิดพลาด\n詳細");
        }
    }
}

#[cfg(test)]
#[path = "fork_projection_tests.rs"]
mod fork_projection_tests;

#[cfg(test)]
#[path = "thinking_metadata_tests.rs"]
mod thinking_metadata_tests;

#[cfg(test)]
#[path = "applied_proposal_read_tests.rs"]
mod applied_proposal_read_tests;

#[cfg(test)]
#[path = "pending_attachment_inventory_storage_tests.rs"]
mod pending_attachment_inventory_tests;

#[path = "reviewed_reference_store.rs"]
mod reviewed_reference_store;

#[cfg(test)]
#[path = "preview_schema_compat_tests.rs"]
mod preview_schema_compat_tests;

#[path = "session_selection_store.rs"]
mod session_selection_store;
