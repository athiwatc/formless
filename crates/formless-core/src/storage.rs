use anyhow::{anyhow, Context, Result};
use formless_protocol::{
    MessageInfo, ModuleInfo, PromptProposalInfo, SearchHit, SessionInfo, SubagentInfo,
    TemplateInfo, ToolInfo, ToolMetricsInfo, ToolVersionInfo,
};
use formless_runtime::Capabilities;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Token use for one key and model: (key, model, prompt, completion,
/// cached).
pub type GroupedUsage = (String, String, i64, i64, i64);

pub struct Store {
    conn: Connection,
    /// Bumped whenever stored messages change other than by appending
    /// (edit/truncate, delete), so [`ContextCache`]s know to rebuild.
    rewrites: std::sync::atomic::AtomicU64,
}

#[derive(Debug, Clone)]
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

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn make_snippet(content: &str, needle_lower: &str) -> String {
    let flat: String = content
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let chars: Vec<char> = flat.chars().collect();
    let lower: String = flat.to_lowercase();
    let byte_at = lower.find(needle_lower).unwrap_or(0);
    let char_at = lower[..byte_at].chars().count();
    let start = char_at.saturating_sub(40);
    let end = (char_at + needle_lower.chars().count() + 60).min(chars.len());
    let core: String = chars[start..end].iter().collect();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        core,
        if end < chars.len() { "…" } else { "" }
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

/// Tool definitions as a Session last listed them (pinned for the prompt
/// cache until that Session refreshes them).
/// Columns for [`Store::row_skill`]: the row plus usage metrics.
const SKILL_COLS: &str = "s.*,
  (SELECT COUNT(*) FROM skill_loads l WHERE l.skill_id=s.id) AS loads,
  (SELECT COUNT(DISTINCT l.run_id) FROM skill_loads l WHERE l.skill_id=s.id AND l.run_id IS NOT NULL) AS runs,
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

/// Token use of one model: (model, prompt, completion, cached).
pub type ModelUsage = (String, i64, i64, i64);

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

/// Applied memory-kind proposals move into the memory store (author
/// `optimiser`, linked for rollback).
const SCHEMA_V18: &str = "
INSERT OR IGNORE INTO memories(id,text,dedup_key,scope,author,enabled,created_at,updated_at,proposal_id)
  SELECT 'm' || lower(hex(randomblob(4))), trim(content), lower(trim(content)), 'global', 'optimiser', 1,
         COALESCE(applied_at, created_at), COALESCE(applied_at, created_at), id
  FROM prompt_proposals WHERE kind='memory' AND status='applied';
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

/// Schema version this build writes (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 39;

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
const MIGRATIONS: [(i64, MigrationStep); 39] = [
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
        Ok(c.execute_batch(SCHEMA_V18)?)
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
];

/// Bring the schema to [`SCHEMA_VERSION`] (SPEC §9.4). An existing
/// database is backed up first; each step runs in its own transaction
/// together with its version bump, so a failure leaves the database at
/// the last completed version.
fn migrate(conn: &Connection, file: Option<&Path>) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(anyhow!(
            "database schema v{version} is newer than this build (v{SCHEMA_VERSION}); upgrade formless"
        ));
    }
    if version > 0 && version < SCHEMA_VERSION {
        if let Some(f) = file {
            backup_before_migration(conn, f, version)?;
        }
    }
    for (target, step) in MIGRATIONS {
        if version < target {
            let tx = conn.unchecked_transaction()?;
            step(&tx).with_context(|| format!("migrate database to v{target}"))?;
            tx.execute_batch(&format!("PRAGMA user_version={target};"))?;
            tx.commit()?;
        }
    }
    Ok(())
}

/// Consistent copy (WAL included) next to the database, named after the
/// version it holds and the time it was taken.
fn backup_before_migration(conn: &Connection, file: &Path, version: i64) -> Result<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let name = format!(
        "{}.v{version}-{stamp}.bak",
        file.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("formless.db")
    );
    let dest = file.with_file_name(name);
    conn.execute("VACUUM INTO ?1", params![dest.to_string_lossy()])
        .with_context(|| format!("back up database to {}", dest.display()))?;
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
        let conn = Connection::open(file).with_context(|| format!("open db {}", file.display()))?;
        conn.execute_batch(PRAGMAS)?;
        conn.set_prepared_statement_cache_capacity(STMT_CACHE);
        migrate(&conn, Some(file))?;
        Ok(Self {
            conn,
            rewrites: Default::default(),
        })
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
        })
    }

    /// `execute` through the connection's statement cache.
    fn ex(&self, sql: &str, params: impl rusqlite::Params) -> Result<usize> {
        Ok(self.conn.prepare_cached(sql)?.execute(params)?)
    }

    /// `prepare` through the connection's statement cache.
    fn ps(&self, sql: &str) -> Result<rusqlite::CachedStatement<'_>> {
        Ok(self.conn.prepare_cached(sql)?)
    }

    fn row_session(r: &rusqlite::Row) -> rusqlite::Result<SessionInfo> {
        Ok(SessionInfo {
            id: r.get("id")?,
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
            "UPDATE sessions SET name=?2, updated_at=?3 WHERE id=?1",
            params![id, name, now()],
        )?;
        self.get_session(id)
    }

    /// Auto-naming: rename only while the Session still has the default
    /// name, so a user rename that raced the namer always wins. Leaves
    /// `updated_at` alone (naming is not activity). Returns the Session
    /// when the rename applied.
    pub fn auto_name_session(&self, id: &str, name: &str) -> Result<Option<SessionInfo>> {
        let n = self.ex(
            "UPDATE sessions SET name=?2 WHERE id=?1 AND name=?3",
            params![id, name, DEFAULT_SESSION_NAME],
        )?;
        if n == 0 {
            return Ok(None);
        }
        self.get_session(id)
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

    pub fn delete_session(&self, id: &str) -> Result<bool> {
        self.note_rewrite();
        Ok(self
            .conn
            .execute("DELETE FROM sessions WHERE id=?1", params![id])?
            > 0)
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
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        // One write transaction, taken up front (IMMEDIATE): the next seq is
        // computed inside the INSERT itself, so two writers can never pick
        // the same one, and the session's timestamp moves with the message.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let seq: i64 = tx
            .prepare_cached(
                "INSERT INTO messages(id,session_id,seq,role,content,created_at,meta) \
                 SELECT ?1, ?2, COALESCE(MAX(seq),0)+1, ?3, ?4, ?5, ?6 FROM messages WHERE session_id=?2 \
                 RETURNING seq",
            )?
            .query_row(params![id, session_id, role, content, t, meta], |r| r.get(0))?;
        tx.prepare_cached("UPDATE sessions SET updated_at=?2 WHERE id=?1")?
            .execute(params![session_id, t])?;
        tx.commit()?;
        Ok(MessageInfo {
            id,
            session_id: session_id.into(),
            seq,
            role: role.into(),
            content: content.into(),
            created_at: t,
            meta,
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
            self.append_message_meta(&s.id, role, content, meta.clone())?;
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
        let Some(orig) = self.get_session(id)? else {
            return Ok(None);
        };
        let limit = up_to_seq.unwrap_or(i64::MAX);
        let messages: Vec<(String, String, Option<String>)> = self
            .list_messages(id)?
            .into_iter()
            .filter(|m| m.seq <= limit)
            .map(|m| (m.role, m.content, m.meta))
            .collect();
        let checkpoints: Vec<(String, i64)> = self
            .list_checkpoints(id)?
            .into_iter()
            .filter(|(_, up_to)| *up_to <= limit)
            .collect();
        let name = name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("{} (fork)", orig.name));
        let fork = self.import_session(
            &name,
            orig.model,
            orig.reasoning_effort,
            &messages,
            &checkpoints,
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
        let sql = if fts.is_some() {
            "SELECT m.session_id, s.name, m.id, m.seq, m.role, m.content, m.created_at \
             FROM messages_fts f JOIN messages m ON m.rowid = f.rowid \
             JOIN sessions s ON s.id = m.session_id AND s.deleted_at IS NULL \
             WHERE messages_fts MATCH ?1 \
             ORDER BY bm25(messages_fts), m.created_at DESC LIMIT ?2"
        } else {
            "SELECT m.session_id, s.name, m.id, m.seq, m.role, m.content, m.created_at \
             FROM messages m JOIN sessions s ON s.id = m.session_id AND s.deleted_at IS NULL \
             WHERE instr(lower(m.content), lower(?1)) > 0 \
             ORDER BY m.created_at DESC LIMIT ?2"
        };
        let mut stmt = self.ps(sql)?;
        let arg = fts.unwrap_or_else(|| query.to_string());
        let rows = stmt.query_map(params![arg, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        let needle = query
            .split(|c: char| !c.is_alphanumeric())
            .find(|w| !w.is_empty())
            .unwrap_or(query)
            .to_lowercase();
        let mut hits = Vec::new();
        for row in rows {
            let (session_id, session_name, message_id, seq, role, content, created_at) = row?;
            hits.push(SearchHit {
                session_id,
                session_name,
                message_id,
                seq,
                role,
                snippet: make_snippet(&content, &needle),
                created_at,
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
        let found = self
            .list_messages(session_id)?
            .into_iter()
            .find(|m| m.id == message_id);
        let Some(mut msg) = found else {
            return Ok(None);
        };
        if msg.role != "user" {
            return Ok(None);
        }
        self.note_rewrite();
        // One edit is one commit: four separate ones cost four fsyncs (or
        // four WAL commits) for a single user action.
        let tx = self.conn.unchecked_transaction()?;
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
        tx.commit()?;
        msg.content = content.to_string();
        Ok(Some(msg))
    }

    pub fn create_run(&self, session_id: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        self.ex(
            "INSERT INTO runs(id,session_id,status,created_at,updated_at) VALUES(?1,?2,'running',?3,?3)",
            params![id, session_id, t],
        )?;
        Ok(id)
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
            if out.last().is_none_or(|s| s.name != name) {
                out.push(formless_protocol::BuiltinStatInfo {
                    name: name.clone(),
                    ..Default::default()
                });
            }
            let s = out.last_mut().unwrap();
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
        self.ex(
            "INSERT INTO scripts(id,session_id,run_id,hash,source,uses,status,error,duration_ms,saved_tool,created_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
        let mut stmt = self.ps(
            "SELECT count(DISTINCT session_id) AS n, \
                    (SELECT uses FROM scripts l WHERE l.hash=s.hash ORDER BY created_at DESC LIMIT 1), \
                    (SELECT source FROM scripts l WHERE l.hash=s.hash ORDER BY created_at DESC LIMIT 1) \
             FROM scripts s WHERE status='success' GROUP BY hash \
             HAVING n>=?1 AND max(saved_tool)='' ORDER BY n DESC LIMIT ?2",
        )?;
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
        })
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
        let id = format!("j{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        self.ex(
            "INSERT INTO jobs(id,prompt,session_id,cron,next_run,enabled,approved,author,created_at) VALUES(?1,?2,?3,?4,?5,?6,?6,?7,?8)",
            params![id, prompt, session_id, cron, next_run, approved, author, now()],
        )?;
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
        Ok(self.ex(
            "UPDATE jobs SET enabled=?2, approved=CASE WHEN ?2 THEN 1 ELSE approved END WHERE id=?1",
            params![id, enabled],
        )? > 0)
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
            "SELECT model, SUM(prompt_tokens), SUM(completion_tokens), SUM(cached_tokens) FROM run_usage
             WHERE (?1 IS NULL OR session_id=?1) AND (?2 IS NULL OR run_id=?2) GROUP BY model",
        )?;
        let rows = stmt
            .query_map(params![session_id, run_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let steps: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(steps),0) FROM runs WHERE (?1 IS NULL OR session_id=?1) AND (?2 IS NULL OR id=?2)",
            params![session_id, run_id],
            |r| r.get(0),
        )?;
        Ok((rows, steps))
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
        self.ex(
            "INSERT INTO task_stats(id,role,model,session_id,run_id,task_hash,ms,tokens,turns,outcome,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![uuid::Uuid::new_v4().to_string(), role, model, session_id, run_id, task_hash, ms, tokens, turns, outcome, now()],
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
        let id = format!("h{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        self.ex(
            "INSERT INTO harness_changes(id,op,target,before,after,tool,reason,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![id, op, target, before.to_string(), after.to_string(), tool, reason, now()],
        )?;
        Ok(id)
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

    /// Per-Run metrics, newest first.
    pub fn run_metrics(&self, limit: usize) -> Result<Vec<formless_protocol::RunMetricsInfo>> {
        self.run_metrics_for(None, limit, None)
    }

    /// The newest `limit` Runs, of one Session or all.
    pub fn run_metrics_for(
        &self,
        session_id: Option<&str>,
        limit: usize,
        since: Option<&str>,
    ) -> Result<Vec<formless_protocol::RunMetricsInfo>> {
        let mut stmt = self.ps(
            "SELECT r.id, r.session_id, r.status, r.followup, r.steps,
               (SELECT COUNT(*) FROM invocations i WHERE i.run_id=r.id),
               (SELECT COALESCE(SUM(prompt_tokens),0) FROM run_usage u WHERE u.run_id=r.id),
               (SELECT COALESCE(SUM(completion_tokens),0) FROM run_usage u WHERE u.run_id=r.id),
               r.created_at, r.updated_at
             FROM runs r WHERE (?2 IS NULL OR r.session_id=?2) AND (?3 IS NULL OR r.created_at >= ?3)
             ORDER BY r.created_at DESC, r.rowid DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64, session_id, since], |r| {
                let (created, updated): (String, String) = (r.get(8)?, r.get(9)?);
                let duration_ms = match (
                    chrono::DateTime::parse_from_rfc3339(&created),
                    chrono::DateTime::parse_from_rfc3339(&updated),
                ) {
                    (Ok(a), Ok(b)) => (b - a).num_milliseconds().max(0),
                    _ => 0,
                };
                let status: String = r.get(2)?;
                let followup: String = r.get(3)?;
                Ok(formless_protocol::RunMetricsInfo {
                    run_id: r.get(0)?,
                    session_id: r.get(1)?,
                    outcome: if followup.is_empty() {
                        status.clone()
                    } else {
                        followup
                    },
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
            "UPDATE runs SET status=?2, updated_at=?3 WHERE id=?1",
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
        let rows = stmt
            .query_map(params![subagent_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .iter()
            .filter_map(|m| serde_json::from_str(m).ok())
            .collect())
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
        let next: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id, content FROM queued_messages WHERE session_id=?1 ORDER BY id LIMIT 1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        if let Some((id, _)) = &next {
            self.conn
                .execute("DELETE FROM queued_messages WHERE id=?1", params![id])?;
        }
        Ok(next.map(|(_, content)| content))
    }

    pub fn list_queued_messages(&self, session_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT content FROM queued_messages WHERE session_id=?1 ORDER BY id")?;
        let rows = stmt
            .query_map(params![session_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
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
        self.ex(
            "INSERT INTO checkpoints(id,session_id,summary,up_to_seq,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![id, session_id, summary, up_to_seq, now()],
        )?;
        Ok(id)
    }
    pub fn latest_checkpoint(&self, session_id: &str) -> Result<Option<(String, i64)>> {
        let mut stmt = self.ps(
            "SELECT summary, up_to_seq FROM checkpoints WHERE session_id=?1 ORDER BY up_to_seq DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![session_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    // --- Tools (SPEC §7, §8) ---

    fn tool_info(&self, id: &str) -> Result<Option<ToolInfo>> {
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
        let metrics = self.tool_metrics(&id, None)?;
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
            description,
            retired: retired != 0,
            current_version,
            capabilities,
            memory_mb,
            invocations: metrics.total,
            success_rate: metrics.success_rate,
            needs_attention: needs_attention != 0,
            created_at,
            updated_at,
            recent: recent.into_iter().collect(),
            author,
        }))
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
        Ok((self.tool_info(&tool_id)?.context("tool vanished")?, version))
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

    fn module_info(&self, id: &str) -> Result<Option<ModuleInfo>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id,name,description,current_version,retired,created_at,updated_at FROM modules WHERE id=?1",
                params![id],
                |r| {
                    Ok(ModuleInfo {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        description: r.get(2)?,
                        current_version: r.get(3)?,
                        retired: r.get::<_, i64>(4)? != 0,
                        created_at: r.get(5)?,
                        updated_at: r.get(6)?,
                    })
                },
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

    /// Active Tools whose current code requires the module (by name).
    pub fn module_dependents(&self, name: &str) -> Result<Vec<String>> {
        let lower = name.to_lowercase();
        let mut out = Vec::new();
        for t in self.list_tools(false)? {
            if let Some((_, v)) = self.get_tool_current(&t.id)? {
                let code = v.code.to_lowercase();
                if ["\"", "'", "`"].iter().any(|q| {
                    code.contains(&format!("requiremodule({q}{lower}{q}"))
                        || code.contains(&format!("modules.load({q}{lower}{q}"))
                }) {
                    out.push(t.name);
                }
            }
        }
        Ok(out)
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
        let mut out = Vec::new();
        for info in self.list_modules()? {
            if info.retired {
                continue;
            }
            if let Some((_, code)) = self.get_module_by_name(&info.name)? {
                out.push((info.name, code));
            }
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
        Ok(self
            .list_proposals()?
            .into_iter()
            .filter(|p| p.status == "applied")
            .rev()
            .collect())
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
    /// proposals as versioned sections (oldest first). Memory proposals
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
        let mut stmt = self.ps(
            "SELECT model, SUM(ok), SUM(ok=0), SUM(ms) FROM model_events
             WHERE created_at >= ?1 GROUP BY model ORDER BY model",
        )?;
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
        Ok(Some(formless_protocol::ToolDetail {
            tool,
            versions,
            metrics,
            optimiser_attempts,
            version_metrics,
            approval,
            recent_calls,
            attention,
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

    /// Pinned definitions of a Session's Tools that are no longer active
    /// (retired, e.g. an MCP server dropped them), as (tool id, definition).
    pub fn pinned_inactive(&self, session_id: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.ps(
            "SELECT st.tool_id, st.pinned_def FROM session_tools st JOIN tools t ON t.id = st.tool_id
             WHERE st.session_id=?1 AND st.pinned_def IS NOT NULL AND st.source!='unloaded' AND t.retired=1",
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
        let mut stmt = self.ps(
            "SELECT version, tag, input, error FROM invocations
             WHERE tool_id=?1 AND status='failure' ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![tool_id, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
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

    /// Keys under `prefix`, sorted (at most 1000).
    pub fn kv_list(&self, namespace: &str, prefix: &str) -> Result<Vec<String>> {
        let mut stmt = self.ps(
            "SELECT key FROM tool_kv WHERE namespace=?1 AND substr(key,1,length(?2))=?2 ORDER BY key LIMIT 1000",
        )?;
        let rows = stmt
            .query_map(params![namespace, prefix], |r| r.get(0))?
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
        let tx = self.conn.unchecked_transaction()?;
        if let Some(run) = run_id {
            let _ = tx.execute(
                "UPDATE invocations SET retried=1 WHERE rowid=(SELECT MAX(rowid) FROM invocations WHERE tool_id=?1 AND run_id=?2 AND status='success')",
                params![tool_id, run],
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO invocations(id,tool_id,version,session_id,run_id,parent_id,status,tag,input,output,error,duration_ms,created_at) VALUES(?1,?2,?3,?4,?5,?6,'running','',?7,'','',0,?8)",
            params![id, tool_id, version, session_id, run_id, parent_id, input, now()],
        )?;
        tx.commit()?;
        Ok(id)
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
        tx.execute(
            "INSERT INTO invocations(id,tool_id,version,session_id,run_id,parent_id,status,tag,input,output,error,duration_ms,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![id, tool_id, version, session_id, run_id, parent_id, status, tag, input, output, error, duration_ms, now()],
        )?;
        prune_invocations(&tx, tool_id, retention)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn tool_metrics(&self, tool_id: &str, version: Option<i64>) -> Result<ToolMetricsInfo> {
        let (total, success, failure, denied, cancelled, validation, retried): (i64, i64, i64, i64, i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(status='success'),0), COALESCE(SUM(status='failure'),0), COALESCE(SUM(status='denied'),0), COALESCE(SUM(status='cancelled'),0), COALESCE(SUM(tag='validation'),0), COALESCE(SUM(retried),0) FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2)",
            params![tool_id, version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )?;
        // Cost per Success (§8.1 metric 3): everything spent, failures
        // included, over the successes it bought.
        let (all_ms, all_tokens): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(duration_ms),0), COALESCE(SUM(tokens),0) FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2)",
            params![tool_id, version],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let per_success = |v: i64| (success > 0).then(|| v as f64 / success as f64);
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
        Ok(ToolMetricsInfo {
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
        })
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

    pub fn set_approval(&self, tool_id: &str, caps: &Capabilities, memory_mb: u32) -> Result<()> {
        self.ex(
            "INSERT INTO approvals(tool_id,capabilities,memory_mb,approved_at) VALUES(?1,?2,?3,?4) ON CONFLICT(tool_id) DO UPDATE SET capabilities=excluded.capabilities, memory_mb=excluded.memory_mb, approved_at=excluded.approved_at",
            params![tool_id, serde_json::to_string(&caps.to_specs())?, memory_mb, now()],
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
        let t = now();
        self.ex(
            "INSERT INTO skills(id,name,description,current_version,enabled,source,author,has_scripts,created_at,updated_at)
             VALUES(?1,?2,?3,?4,0,?5,?6,?7,?8,?8)
             ON CONFLICT(name) DO UPDATE SET description=excluded.description, current_version=excluded.current_version,
               enabled=0, source=excluded.source, author=excluded.author, has_scripts=excluded.has_scripts,
               retired=0, updated_at=excluded.updated_at",
            params![uuid::Uuid::new_v4().to_string(), name, description, version, source, author, has_scripts, t],
        )?;
        self.ex(
            "INSERT OR REPLACE INTO skill_versions(skill_id,version,path,created_at) SELECT id,?2,?3,?4 FROM skills WHERE name=?1",
            params![name, version, path, t],
        )?;
        self.get_skill(name)?.context("skill vanished")
    }

    fn row_skill(r: &rusqlite::Row) -> rusqlite::Result<formless_protocol::SkillInfo> {
        Ok(formless_protocol::SkillInfo {
            name: r.get("name")?,
            description: r.get("description")?,
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
        })
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
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {SKILL_COLS} FROM skills s WHERE s.name=?1"),
                params![name],
                Self::row_skill,
            )
            .ok())
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
            "UPDATE skills SET enabled=?2, retired=CASE WHEN ?2 THEN 0 ELSE retired END, updated_at=?3 WHERE name=?1",
            params![name, enabled, now()],
        )? > 0)
    }

    /// Record that a Session loaded a skill (usage metrics).
    pub fn record_skill_load(&self, name: &str, session_id: &str, run_id: &str) -> Result<()> {
        self.ex(
            "INSERT INTO skill_loads(skill_id, session_id, run_id, created_at) SELECT id, ?2, ?3, ?4 FROM skills WHERE name=?1",
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
                self.ex(
                    "UPDATE skills SET retired=1, enabled=0, updated_at=?2 WHERE name=?1",
                    params![s.name, now()],
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
            let other: Vec<String> = self
                .run_invocations(&run)?
                .into_iter()
                .map(|(name, _, _)| name)
                .collect();
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
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM memories WHERE scope=?1 AND dedup_key=?2",
                params![scope, key],
                |r| r.get(0),
            )
            .ok();
        let (id, created) = match existing {
            Some(id) => {
                self.ex(
                    "UPDATE memories SET enabled=1, updated_at=?2 WHERE id=?1",
                    params![id, t],
                )?;
                (id, false)
            }
            None => {
                let id = format!("m{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
                self.ex(
                    "INSERT INTO memories(id,text,dedup_key,scope,author,session_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",
                    params![id, crate::memory::clean(text), key, scope, author, session_id, t],
                )?;
                (id, true)
            }
        };
        Ok((self.get_memory(&id)?.context("memory vanished")?, created))
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

    /// Record one provider request's reported token usage (one Run step).
    pub fn record_usage(
        &self,
        session_id: &str,
        run_id: &str,
        model: &str,
        usage: &crate::provider::Usage,
    ) -> Result<()> {
        self.ex(
            "INSERT INTO run_usage(session_id,run_id,model,prompt_tokens,completion_tokens,reasoning_tokens,cached_tokens,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                session_id,
                run_id,
                model,
                usage.prompt_tokens as i64,
                usage.completion_tokens as i64,
                usage.reasoning_tokens as i64,
                usage.cached_tokens as i64,
                now()
            ],
        )?;
        Ok(())
    }

    /// Summed (prompt, completion, cached) tokens per model.
    pub fn usage_by_model(
        &self,
        since: Option<&str>,
    ) -> Result<std::collections::HashMap<String, (i64, i64, i64)>> {
        let mut stmt = self.ps(
            "SELECT model, SUM(prompt_tokens), SUM(completion_tokens), SUM(cached_tokens) FROM run_usage
             WHERE ?1 IS NULL OR created_at >= ?1 GROUP BY model",
        )?;
        let rows = stmt
            .query_map(params![since], |r| {
                Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Token use since `since` grouped by `by` ("model", "role",
    /// "session", else the UTC day) and model (for pricing): (key, model,
    /// prompt, completion, cached).
    pub fn usage_grouped(&self, since: Option<&str>, by: &str) -> Result<Vec<GroupedUsage>> {
        let key = match by {
            "model" => "u.model",
            "session" => "COALESCE(s.name, u.session_id)",
            "role" => "COALESCE((SELECT t.role FROM task_stats t WHERE t.run_id=u.run_id LIMIT 1), 'main')",
            _ => "substr(u.created_at, 1, 10)",
        };
        let mut stmt = self.ps(&format!(
            "SELECT {key}, u.model, SUM(u.prompt_tokens), SUM(u.completion_tokens), SUM(u.cached_tokens)
             FROM run_usage u LEFT JOIN sessions s ON s.id=u.session_id
             WHERE ?1 IS NULL OR u.created_at >= ?1
             GROUP BY 1, 2 ORDER BY 1"
        ))?;
        let rows = stmt
            .query_map(params![since], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Summed usage of a Run's provider requests.
    pub fn run_usage(&self, run_id: &str) -> Result<crate::provider::Usage> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(reasoning_tokens),0), COALESCE(SUM(cached_tokens),0) FROM run_usage WHERE run_id=?1",
            params![run_id],
            |r| {
                Ok(crate::provider::Usage {
                    prompt_tokens: r.get::<_, i64>(0)? as u64,
                    completion_tokens: r.get::<_, i64>(1)? as u64,
                    reasoning_tokens: r.get::<_, i64>(2)? as u64,
                    cached_tokens: r.get::<_, i64>(3)? as u64,
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
                "SELECT prompt_tokens FROM run_usage WHERE session_id=?1 \
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
        let mut stmt = self.ps(
            "SELECT id, path, bytes, created_at FROM artifacts ORDER BY created_at, rowid",
        )?;
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
            .ok())
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

    pub fn replay_set(&self, tool_id: &str) -> Result<Vec<ReplayItem>> {
        let query = |status: &str, limit: i64| -> Result<Vec<ReplayItem>> {
            let mut stmt = self.ps(
                "SELECT input, error, tag FROM invocations WHERE tool_id=?1 AND status=?2 ORDER BY rowid DESC LIMIT ?3",
            )?;
            let rows = stmt
                .query_map(params![tool_id, status, limit], |r| {
                    Ok(ReplayItem {
                        input: r.get::<_, String>(0)?,
                        error: r.get::<_, String>(1)?,
                        tag: r.get::<_, String>(2)?,
                        recorded_success: status == "success",
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        };
        let failures = query("failure", i64::MAX)?;
        let successes = query("success", failures.len() as i64)?;
        let mut out = failures;
        out.extend(successes);
        Ok(out)
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
        let rows = stmt
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
        let mut stmt = self.ps(
            "SELECT actor, action, detail, created_at FROM audit
             WHERE ?1 IS NULL OR substr(action,1,length(?1))=?1
             ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![action_prefix, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Audit entries for the log view, newest first: `filter` matches the
    /// actor, action or detail (case-insensitive); `before` pages back.
    pub fn list_audit(
        &self,
        filter: Option<&str>,
        before: Option<&str>,
        limit: usize,
    ) -> Result<Vec<formless_protocol::AuditEntry>> {
        let mut stmt = self.ps(
            "SELECT actor, action, detail, created_at FROM audit
             WHERE (?1 IS NULL OR instr(lower(actor || ' ' || action || ' ' || detail), lower(?1)) > 0)
               AND (?2 IS NULL OR created_at < ?2)
             ORDER BY created_at DESC, rowid DESC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![filter, before, limit.clamp(1, 500) as i64], |r| {
                Ok(formless_protocol::AuditEntry {
                    actor: r.get(0)?,
                    action: r.get(1)?,
                    detail: r.get(2)?,
                    created_at: r.get(3)?,
                })
            })?
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
        Ok(())
    }
}

/// Rough token estimate (chars/4). Used only for compaction triggering.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

pub type BuiltContext = (Vec<Value>, bool, i64);

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

/// [`build_context`] for one Session, kept between agent steps: each build
/// reads only messages newer than the last one seen and keeps a running
/// token estimate. It starts over when the system prompt or checkpoint
/// changes, or when stored messages were rewritten (edit, delete).
#[derive(Default)]
pub struct ContextCache {
    /// (store rewrite count, checkpoint up_to, system prompt) it was built for.
    key: Option<(u64, i64, String)>,
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

    pub fn build(
        &mut self,
        store: &Store,
        session_id: &str,
        system_prompt: &str,
        context_tokens: u32,
        trigger_fraction: f64,
    ) -> Result<BuiltContext> {
        let (summary, up_to) = store.latest_checkpoint(session_id)?.unwrap_or_default();
        let key = (
            store.rewrites.load(std::sync::atomic::Ordering::SeqCst),
            up_to,
            system_prompt.to_string(),
        );
        if self.key.as_ref() != Some(&key) {
            // Exactly one, leading system message: many chat templates and
            // proxies reject a second or mid-conversation `system` (§11
            // summary included).
            let system = if summary.is_empty() {
                system_prompt.to_string()
            } else {
                format!("{system_prompt}\n\nSummary of earlier conversation:\n{summary}")
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
            if let Some(calls) = msg["tool_calls"].as_array_mut() {
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
                        let n = estimate_tokens(&last.to_string());
                        *self.tokens.last_mut().expect("tokens track ctx") = n;
                        continue;
                    }
                }
            }
            self.push(msg);
        }
        let tokens: usize = self.tokens.iter().sum();
        let over = (tokens as f64) > (context_tokens as f64 * trigger_fraction);
        let (needs, compact_up_to) = match self.turn_cut() {
            Some(cut) if over => (true, cut),
            _ => (false, up_to),
        };
        Ok((self.ctx.clone(), needs, compact_up_to))
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
        self.tokens.push(estimate_tokens(&msg.to_string()));
        self.ctx.push(msg);
    }
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
        return json!({"role": "user", "content": format!("[harness note] {}", m.content)});
    }
    if m.role == "tool" {
        return json!({"role": "tool", "tool_call_id": meta["tool_call_id"].as_str().unwrap_or_default(), "content": m.content});
    }
    if m.role == "assistant" && meta["tool_calls"].is_array() {
        let mut msg =
            json!({"role": "assistant", "content": Value::Null, "tool_calls": meta["tool_calls"]});
        // Echo reasoning back only when the provider emitted it (thinking
        // mode requires it on tool-call turns; other endpoints never see it).
        if let Some(r) = meta["reasoning_content"].as_str() {
            if !r.is_empty() {
                msg["reasoning_content"] = json!(r);
            }
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
                content.push_str(&format!(
                    "\n[attached: {} ({}, {} bytes), artifact {}; read it with read_artifact]",
                    a["name"].as_str().unwrap_or("file"),
                    a["mime"].as_str().unwrap_or("application/octet-stream"),
                    a["bytes"],
                    a["id"].as_str().unwrap_or_default()
                ));
                if a["mime"].as_str().is_some_and(|t| t.starts_with("image/")) {
                    images.push(json!({"path": a["path"], "mime": a["mime"]}));
                }
            }
            let mut msg = json!({"role": "user", "content": content});
            if !images.is_empty() {
                msg["_attachments"] = json!(images);
            }
            return msg;
        }
    }
    json!({"role": m.role, "content": m.content})
}

/// An FTS5 query from free text: each word quoted as a prefix term, all
/// required. None when the text has no word characters.
pub fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}
