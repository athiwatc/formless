-- Historical migration SQL, copied from storage.rs at 60554b7fc77ba0537d945e26a49bbd9f4498ea2f.
-- See PROVENANCE.md. Apply only steps 1..k; this is not a current-schema dump.

-- migration 1
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

-- migration 2
ALTER TABLE messages ADD COLUMN meta TEXT;
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

-- migration 3
ALTER TABLE tools ADD COLUMN cooldown_until TEXT;
ALTER TABLE tools ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE invocations ADD COLUMN retried INTEGER NOT NULL DEFAULT 0;
CREATE TABLE IF NOT EXISTS optimiser_attempts (
  id TEXT PRIMARY KEY, tool_id TEXT NOT NULL REFERENCES tools(id) ON DELETE CASCADE,
  base_version INTEGER NOT NULL, status TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '',
  candidate_code TEXT NOT NULL DEFAULT '', candidate_description TEXT NOT NULL DEFAULT '',
  candidate_input_schema TEXT NOT NULL DEFAULT '{}', candidate_output_schema TEXT NOT NULL DEFAULT '{}',
  candidate_capabilities TEXT NOT NULL DEFAULT '[]', candidate_memory_mb INTEGER NOT NULL DEFAULT 256,
  candidate_score REAL, baseline_score REAL, replay_total INTEGER NOT NULL DEFAULT 0,
  regressions INTEGER NOT NULL DEFAULT 0, est_tokens INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_attempts_tool ON optimiser_attempts(tool_id, created_at);

-- migration 4
CREATE TABLE IF NOT EXISTS modules (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, name_lower TEXT NOT NULL UNIQUE,
  description TEXT NOT NULL DEFAULT '', retired INTEGER NOT NULL DEFAULT 0,
  current_version INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS module_versions (
  id TEXT PRIMARY KEY, module_id TEXT NOT NULL REFERENCES modules(id) ON DELETE CASCADE,
  version INTEGER NOT NULL, code TEXT NOT NULL,
  author TEXT NOT NULL DEFAULT 'agent', created_at TEXT NOT NULL, UNIQUE(module_id, version));

-- migration 5
CREATE TABLE IF NOT EXISTS prompt_proposals (
  id TEXT PRIMARY KEY, kind TEXT NOT NULL, content TEXT NOT NULL,
  rationale TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'proposed',
  baseline_score REAL, candidate_score REAL, eval_total INTEGER NOT NULL DEFAULT 0,
  est_tokens INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, applied_at TEXT);
CREATE INDEX IF NOT EXISTS idx_proposals_status ON prompt_proposals(status, created_at);

-- migration 6
CREATE TABLE IF NOT EXISTS model_stats (
  model TEXT PRIMARY KEY, successes INTEGER NOT NULL DEFAULT 0,
  failures INTEGER NOT NULL DEFAULT 0, total_ms INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL);

-- migration 7
CREATE TABLE IF NOT EXISTS session_tools (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  tool_id TEXT NOT NULL REFERENCES tools(id) ON DELETE CASCADE,
  source TEXT NOT NULL DEFAULT 'found', loaded_at TEXT NOT NULL,
  PRIMARY KEY (session_id, tool_id));

-- migration 8
CREATE TABLE IF NOT EXISTS queued_messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  content TEXT NOT NULL, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_queued_session ON queued_messages(session_id, id);

-- migration 9
CREATE TABLE IF NOT EXISTS pending_handles (
  handle TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  tool_name TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
  created_at TEXT NOT NULL);

-- migration 10
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

-- migration 11
CREATE TABLE IF NOT EXISTS run_usage (
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  run_id TEXT NOT NULL, model TEXT NOT NULL,
  prompt_tokens INTEGER NOT NULL, completion_tokens INTEGER NOT NULL,
  reasoning_tokens INTEGER NOT NULL, cached_tokens INTEGER NOT NULL,
  created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_run_usage_session ON run_usage(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_run_usage_run ON run_usage(run_id);

-- migration 12
ALTER TABLE session_tools ADD COLUMN pinned_version INTEGER;
ALTER TABLE session_tools ADD COLUMN pinned_def TEXT;

-- migration 13
ALTER TABLE sessions ADD COLUMN system_prompt TEXT;

-- migration 14
ALTER TABLE session_tools ADD COLUMN unload INTEGER NOT NULL DEFAULT 0;

-- migration 15
CREATE TABLE IF NOT EXISTS mcp_server_grants (server TEXT PRIMARY KEY, approved_at TEXT NOT NULL);

-- migration 16
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

-- migration 17
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY, text TEXT NOT NULL, dedup_key TEXT NOT NULL,
  scope TEXT NOT NULL DEFAULT 'global', author TEXT NOT NULL DEFAULT 'agent',
  session_id TEXT, enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_used TEXT);
CREATE UNIQUE INDEX IF NOT EXISTS idx_memories_dedup ON memories(scope, dedup_key);

-- migration 18
ALTER TABLE memories ADD COLUMN proposal_id TEXT;
INSERT OR IGNORE INTO memories(id,text,dedup_key,scope,author,enabled,created_at,updated_at,proposal_id)
  SELECT 'm' || lower(hex(randomblob(4))), trim(content), lower(trim(content)), 'global', 'optimiser', 1,
         COALESCE(applied_at, created_at), COALESCE(applied_at, created_at), id
  FROM prompt_proposals WHERE kind='memory' AND status='applied';

-- migration 19
ALTER TABLE runs ADD COLUMN steps INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN followup TEXT NOT NULL DEFAULT '';

-- migration 20
CREATE TABLE IF NOT EXISTS builtin_calls (
  name TEXT NOT NULL, session_id TEXT, run_id TEXT, ok INTEGER NOT NULL,
  kind TEXT NOT NULL DEFAULT '', duration_ms INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_builtin_calls ON builtin_calls(name, created_at);
CREATE INDEX IF NOT EXISTS idx_builtin_calls_run ON builtin_calls(run_id);

-- migration 21
ALTER TABLE invocations ADD COLUMN tokens INTEGER NOT NULL DEFAULT 0;

-- migration 22
CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY, prompt TEXT NOT NULL, session_id TEXT,
  cron TEXT, next_run TEXT, last_run TEXT, last_status TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 1, approved INTEGER NOT NULL DEFAULT 0,
  author TEXT NOT NULL DEFAULT 'user', created_at TEXT NOT NULL);

-- migration 23
CREATE TABLE IF NOT EXISTS tool_kv (
  namespace TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
  updated_at TEXT NOT NULL, PRIMARY KEY (namespace, key));

-- migration 24
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

-- migration 25
CREATE TABLE IF NOT EXISTS optimiser_queue (tool_id TEXT PRIMARY KEY, queued_at TEXT NOT NULL);

-- migration 26
ALTER TABLE runs ADD COLUMN error TEXT NOT NULL DEFAULT '';

-- migration 27
ALTER TABLE sessions ADD COLUMN deleted_at TEXT;

-- migration 28
CREATE TABLE IF NOT EXISTS scripts (
  id TEXT PRIMARY KEY, session_id TEXT, run_id TEXT, hash TEXT NOT NULL,
  source TEXT NOT NULL, uses TEXT NOT NULL DEFAULT '[]', status TEXT NOT NULL,
  error TEXT NOT NULL DEFAULT '', duration_ms INTEGER NOT NULL DEFAULT 0,
  saved_tool TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_scripts_hash ON scripts(hash, session_id);

-- migration 29
ALTER TABLE subagents ADD COLUMN model TEXT NOT NULL DEFAULT '';

-- migration 30
CREATE TABLE IF NOT EXISTS task_stats (
               id TEXT PRIMARY KEY, role TEXT NOT NULL, model TEXT NOT NULL,
               session_id TEXT, run_id TEXT, task_hash TEXT NOT NULL,
               ms INTEGER NOT NULL, tokens INTEGER NOT NULL, turns INTEGER NOT NULL,
               outcome TEXT NOT NULL, created_at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_task_stats ON task_stats(role, model);

-- migration 31
CREATE TABLE IF NOT EXISTS harness_changes (
               id TEXT PRIMARY KEY, op TEXT NOT NULL, target TEXT NOT NULL,
               before TEXT NOT NULL, after TEXT NOT NULL, tool TEXT NOT NULL,
               reason TEXT NOT NULL, undone INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);

-- migration 32
ALTER TABLE sessions ADD COLUMN cwd TEXT;

-- migration 33
ALTER TABLE sessions ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;

-- migration 34
ALTER TABLE sessions ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;

-- migration 35
ALTER TABLE sessions ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';

-- migration 36
ALTER TABLE sessions ADD COLUMN instructions TEXT;

-- migration 37
CREATE TABLE IF NOT EXISTS session_templates (
               name TEXT PRIMARY KEY, model TEXT, reasoning_effort TEXT,
               instructions TEXT, cwd TEXT, first_message TEXT, created_at TEXT NOT NULL);

-- migration 38
CREATE TABLE IF NOT EXISTS model_events (
               model TEXT NOT NULL, ok INTEGER NOT NULL, ms INTEGER NOT NULL, created_at TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_model_events ON model_events(created_at);

-- migration 39
CREATE INDEX IF NOT EXISTS idx_runs_session ON runs(session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_inv_run ON invocations(run_id);
CREATE INDEX IF NOT EXISTS idx_inv_session ON invocations(session_id, tool_id);
CREATE INDEX IF NOT EXISTS idx_inv_running ON invocations(tool_id) WHERE status='running';
CREATE INDEX IF NOT EXISTS idx_artifacts_session ON artifacts(session_id);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit(created_at);
ALTER TABLE sessions ADD COLUMN message_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN preview TEXT;
ALTER TABLE sessions ADD COLUMN last_run_status TEXT;
UPDATE sessions SET
           message_count = (SELECT COUNT(*) FROM messages m WHERE m.session_id=sessions.id),
           preview = (SELECT substr(content,1,2000) FROM messages m
                        WHERE m.session_id=sessions.id AND m.role='user'
                        ORDER BY m.seq DESC LIMIT 1),
           last_run_status = (SELECT status FROM runs r WHERE r.session_id=sessions.id
                                ORDER BY r.created_at DESC, r.rowid DESC LIMIT 1);
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

-- migration 40
ALTER TABLE tools ADD COLUMN title TEXT NOT NULL DEFAULT '';

-- migration 41
ALTER TABLE tools ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';

-- migration 42
ALTER TABLE tools ADD COLUMN user_pinned INTEGER NOT NULL DEFAULT 0;

-- migration 43
ALTER TABLE tools ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0;

-- migration 44
ALTER TABLE invocations ADD COLUMN model_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE invocations ADD COLUMN model_cost REAL NOT NULL DEFAULT 0;
ALTER TABLE invocations ADD COLUMN model_unpriced INTEGER NOT NULL DEFAULT 0;

-- migration 45
ALTER TABLE approvals ADD COLUMN granted_by TEXT NOT NULL DEFAULT '';

-- migration 46
ALTER TABLE run_usage ADD COLUMN cost REAL;
ALTER TABLE run_usage ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0;
