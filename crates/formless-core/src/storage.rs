use anyhow::{Context, Result};
use formless_protocol::{
    MessageInfo, SearchHit, SessionInfo, ToolInfo, ToolMetricsInfo, ToolVersionInfo,
};
use formless_runtime::Capabilities;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::Path;

pub struct Store {
    conn: Connection,
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

fn migrate(conn: &Connection, file: Option<&Path>) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(SCHEMA_V1)?;
        conn.execute_batch("PRAGMA user_version=1;")?;
    }
    if version < 2 {
        if version == 1 {
            if let Some(f) = file {
                let _ = std::fs::copy(f, f.with_extension("db.bak"));
            }
        }
        // messages.meta (ignore if a previous partial migration added it)
        let _ = conn.execute("ALTER TABLE messages ADD COLUMN meta TEXT", []);
        conn.execute_batch(SCHEMA_V2)?;
        conn.execute_batch("PRAGMA user_version=2;")?;
    }
    if version < 3 {
        let _ = conn.execute("ALTER TABLE tools ADD COLUMN cooldown_until TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE tools ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE invocations ADD COLUMN retried INTEGER NOT NULL DEFAULT 0",
            [],
        );
        conn.execute_batch(SCHEMA_V3)?;
        conn.execute_batch("PRAGMA user_version=3;")?;
    }
    Ok(())
}

impl Store {
    pub fn open(file: &Path) -> Result<Self> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(file).with_context(|| format!("open db {}", file.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        migrate(&conn, Some(file))?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        migrate(&conn, None)?;
        Ok(Self { conn })
    }

    fn row_session(r: &rusqlite::Row) -> rusqlite::Result<SessionInfo> {
        Ok(SessionInfo {
            id: r.get("id")?,
            name: r.get("name")?,
            model: r.get("model")?,
            reasoning_effort: r.get("reasoning_effort")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
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
            .unwrap_or_else(|| "Untitled".into());
        self.conn.execute(
            "INSERT INTO sessions(id,name,created_at,updated_at) VALUES(?1,?2,?3,?3)",
            params![id, name, t],
        )?;
        self.get_session(&id)?.context("session vanished")
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions ORDER BY updated_at DESC")?;
        let rows = stmt
            .query_map([], Self::row_session)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<SessionInfo>> {
        let mut stmt = self.conn.prepare("SELECT * FROM sessions WHERE id=?1")?;
        let mut rows = stmt.query_map(params![id], Self::row_session)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn rename_session(&self, id: &str, name: &str) -> Result<Option<SessionInfo>> {
        self.conn.execute(
            "UPDATE sessions SET name=?2, updated_at=?3 WHERE id=?1",
            params![id, name, now()],
        )?;
        self.get_session(id)
    }

    pub fn set_session_model(
        &self,
        id: &str,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<Option<SessionInfo>> {
        self.conn.execute(
            "UPDATE sessions SET model=?2, reasoning_effort=?3, updated_at=?4 WHERE id=?1",
            params![id, model, effort, now()],
        )?;
        self.get_session(id)
    }

    pub fn delete_session(&self, id: &str) -> Result<bool> {
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
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM messages WHERE session_id=?1",
            params![session_id],
            |r| r.get(0),
        )?;
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        self.conn.execute(
            "INSERT INTO messages(id,session_id,seq,role,content,created_at,meta) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![id, session_id, seq, role, content, t, meta],
        )?;
        self.conn.execute(
            "UPDATE sessions SET updated_at=?2 WHERE id=?1",
            params![session_id, t],
        )?;
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

    pub fn list_checkpoints(&self, session_id: &str) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
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
        Ok(Some(self.import_session(
            &name,
            orig.model,
            orig.reasoning_effort,
            &messages,
            &checkpoints,
        )?))
    }

    /// Case-insensitive substring search across all sessions' messages.
    pub fn search_sessions(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 200) as i64;
        let mut stmt = self.conn.prepare(
            "SELECT m.session_id, s.name, m.id, m.seq, m.role, m.content, m.created_at \
             FROM messages m JOIN sessions s ON s.id = m.session_id \
             WHERE instr(lower(m.content), lower(?1)) > 0 \
             ORDER BY m.created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit], |r| {
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
        let needle = query.to_lowercase();
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
        self.conn.execute(
            "UPDATE messages SET content=?3 WHERE session_id=?1 AND id=?2",
            params![session_id, message_id, content],
        )?;
        self.conn.execute(
            "DELETE FROM messages WHERE session_id=?1 AND seq>?2",
            params![session_id, msg.seq],
        )?;
        self.conn.execute(
            "DELETE FROM checkpoints WHERE session_id=?1 AND up_to_seq>=?2",
            params![session_id, msg.seq],
        )?;
        self.conn.execute(
            "UPDATE sessions SET updated_at=?2 WHERE id=?1",
            params![session_id, now()],
        )?;
        msg.content = content.to_string();
        Ok(Some(msg))
    }

    pub fn create_run(&self, session_id: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let t = now();
        self.conn.execute(
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
    pub fn set_run_status(&self, run_id: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status=?2, updated_at=?3 WHERE id=?1",
            params![run_id, status, now()],
        )?;
        Ok(())
    }
    /// On daemon startup, runs left 'running' were interrupted by a crash (SPEC §9.5).
    pub fn mark_orphan_runs_interrupted(&self) -> Result<usize> {
        // In-flight invocations: outcome unknown, never re-executed (§9.5).
        let _ = self.conn.execute(
            "UPDATE invocations SET status='interrupted', tag='interrupted' WHERE status='running'",
            [],
        );
        Ok(self.conn.execute(
            "UPDATE runs SET status='interrupted', updated_at=?1 WHERE status='running'",
            params![now()],
        )?)
    }

    pub fn save_checkpoint(
        &self,
        session_id: &str,
        summary: &str,
        up_to_seq: i64,
    ) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO checkpoints(id,session_id,summary,up_to_seq,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![id, session_id, summary, up_to_seq, now()],
        )?;
        Ok(id)
    }
    pub fn latest_checkpoint(&self, session_id: &str) -> Result<Option<(String, i64)>> {
        let mut stmt = self.conn.prepare(
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
        let mut stmt = self.conn.prepare("SELECT * FROM tools WHERE id=?1")?;
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
        let (capabilities, memory_mb) = self
            .conn
            .query_row(
                "SELECT capabilities, memory_mb FROM tool_versions WHERE tool_id=?1 AND version=?2",
                params![id, current_version],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .map(|(c, m)| {
                (
                    serde_json::from_str::<Vec<String>>(&c).unwrap_or_default(),
                    m as u32,
                )
            })
            .unwrap_or_default();
        let metrics = self.tool_metrics(&id, None)?;
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
                self.conn.execute(
                    "INSERT INTO tools(id,name,name_lower,description,current_version,created_at,updated_at) VALUES(?1,?2,?3,?4,0,?5,?5)",
                    params![tid, name, lower, description, t],
                )?;
                (tid, 1)
            }
        };
        self.conn.execute(
            "INSERT INTO tool_versions(id,tool_id,version,input_schema,output_schema,code,capabilities,memory_mb,author,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![uuid::Uuid::new_v4().to_string(), tool_id, version, input_schema, output_schema, code, serde_json::to_string(&caps.to_specs())?, memory_mb, author, t],
        )?;
        self.conn.execute(
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
            let mut stmt = self.conn.prepare(sql)?;
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

    pub fn tool_detail(&self, id: &str) -> Result<Option<formless_protocol::ToolDetail>> {
        let Some(tool) = self.tool_info(id)? else {
            return Ok(None);
        };
        let versions = {
            let mut stmt = self.conn.prepare(
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
        Ok(Some(formless_protocol::ToolDetail {
            tool,
            versions,
            metrics,
            optimiser_attempts,
        }))
    }

    pub fn set_tool_retired(&self, id: &str, retired: bool) -> Result<Option<ToolInfo>> {
        self.conn.execute(
            "UPDATE tools SET retired=?2, updated_at=?3 WHERE id=?1",
            params![id, retired as i64, now()],
        )?;
        self.tool_info(id)
    }

    pub fn purge_tool(&self, id: &str) -> Result<bool> {
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
        self.conn.execute(
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
        if let Some(run) = run_id {
            let _ = self.conn.execute(
                "UPDATE invocations SET retried=1 WHERE rowid=(SELECT MAX(rowid) FROM invocations WHERE tool_id=?1 AND run_id=?2 AND status='success')",
                params![tool_id, run],
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO invocations(id,tool_id,version,session_id,run_id,parent_id,status,tag,input,output,error,duration_ms,created_at) VALUES(?1,?2,?3,?4,?5,?6,'running','',?7,'','',0,?8)",
            params![id, tool_id, version, session_id, run_id, parent_id, input, now()],
        )?;
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
        self.conn.execute(
            "UPDATE invocations SET status=?2, tag=?3, output=?4, error=?5, duration_ms=?6 WHERE id=?1",
            params![id, status, tag, output, error, duration_ms],
        )?;
        self.conn.execute(
            "DELETE FROM invocations WHERE tool_id=?1 AND status!='running' AND rowid NOT IN (SELECT rowid FROM invocations WHERE tool_id=?1 ORDER BY rowid DESC LIMIT ?2)",
            params![tool_id, retention as i64],
        )?;
        Ok(())
    }

    pub fn set_invocation_tag(&self, id: &str, tag: &str) -> Result<()> {
        self.conn.execute(
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
        if let Some(run) = run_id {
            let _ = self.conn.execute(
                "UPDATE invocations SET retried=1 WHERE rowid=(SELECT MAX(rowid) FROM invocations WHERE tool_id=?1 AND run_id=?2 AND status='success')",
                params![tool_id, run],
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO invocations(id,tool_id,version,session_id,run_id,parent_id,status,tag,input,output,error,duration_ms,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![id, tool_id, version, session_id, run_id, parent_id, status, tag, input, output, error, duration_ms, now()],
        )?;
        self.conn.execute(
            "DELETE FROM invocations WHERE tool_id=?1 AND rowid NOT IN (SELECT rowid FROM invocations WHERE tool_id=?1 ORDER BY rowid DESC LIMIT ?2)",
            params![tool_id, retention as i64],
        )?;
        Ok(id)
    }

    pub fn tool_metrics(&self, tool_id: &str, version: Option<i64>) -> Result<ToolMetricsInfo> {
        let (total, success, failure, denied, cancelled, validation, retried): (i64, i64, i64, i64, i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(status='success'),0), COALESCE(SUM(status='failure'),0), COALESCE(SUM(status='denied'),0), COALESCE(SUM(status='cancelled'),0), COALESCE(SUM(tag='validation'),0), COALESCE(SUM(retried),0) FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2)",
            params![tool_id, version],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )?;
        let mut durations: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT duration_ms FROM invocations WHERE tool_id=?1 AND (?2 IS NULL OR version=?2) AND status='success' ORDER BY duration_ms")?;
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
        })
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
        self.conn.execute(
            "INSERT INTO approvals(tool_id,capabilities,memory_mb,approved_at) VALUES(?1,?2,?3,?4) ON CONFLICT(tool_id) DO UPDATE SET capabilities=excluded.capabilities, memory_mb=excluded.memory_mb, approved_at=excluded.approved_at",
            params![tool_id, serde_json::to_string(&caps.to_specs())?, memory_mb, now()],
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
        self.conn.execute(
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
            let mut stmt = self.conn.prepare(
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
        self.conn.execute(
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
        self.conn.execute(
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
        let mut stmt = self.conn.prepare(
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
        self.conn.execute(
            "UPDATE tools SET cooldown_until=?2 WHERE id=?1",
            params![tool_id, until_rfc3339],
        )?;
        Ok(())
    }

    pub fn set_consecutive_failures(&self, tool_id: &str, n: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE tools SET consecutive_failures=?2 WHERE id=?1",
            params![tool_id, n],
        )?;
        Ok(())
    }

    pub fn set_needs_attention(&self, tool_id: &str, on: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE tools SET needs_attention=?2, updated_at=?3 WHERE id=?1",
            params![tool_id, on as i64, now()],
        )?;
        Ok(())
    }

    pub fn audit(&self, actor: &str, action: &str, detail: &str) -> Result<()> {
        self.conn.execute(
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
    let (summary, up_to) = store.latest_checkpoint(session_id)?.unwrap_or_default();
    let all = store.list_messages(session_id)?;
    let live: Vec<&MessageInfo> = all.iter().filter(|m| m.seq > up_to).collect();
    let mut ctx: Vec<Value> = vec![json!({"role": "system", "content": system_prompt})];
    if !summary.is_empty() {
        ctx.push(json!({"role": "system", "content": format!("Summary of earlier conversation:\n{summary}")}));
    }
    for m in &live {
        ctx.push(provider_message(m));
    }
    let tokens: usize = ctx.iter().map(|v| estimate_tokens(&v.to_string())).sum();
    let needs = (tokens as f64) > (context_tokens as f64 * trigger_fraction) && live.len() > 4;
    let compact_up_to = if needs {
        live[live.len() / 2].seq
    } else {
        up_to
    };
    Ok((ctx, needs, compact_up_to))
}

pub fn provider_message(m: &MessageInfo) -> Value {
    let meta: Value = m
        .meta
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    if m.role == "system-note" {
        return json!({"role": "system", "content": m.content});
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
    json!({"role": m.role, "content": m.content})
}
