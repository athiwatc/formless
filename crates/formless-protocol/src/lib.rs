//! Shared daemon <-> client protocol (SPEC §9.3). NDJSON framing: one JSON
//! value per line.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub session_id: String,
    pub session_name: String,
    pub message_id: String,
    pub seq: i64,
    pub role: String,
    pub snippet: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageInfo {
    pub id: String,
    pub session_id: String,
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub created_at: String,
    /// Provider-format extras (tool_calls / tool_call_id) as JSON, if any.
    #[serde(default)]
    pub meta: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub retired: bool,
    pub current_version: i64,
    pub capabilities: Vec<String>,
    pub memory_mb: u32,
    pub invocations: i64,
    pub success_rate: Option<f64>,
    pub needs_attention: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// A provider failure explained for people (SPEC §4.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorExplained {
    /// auth, model, rate_limit, context, unreachable, tool_calling,
    /// server or other.
    pub category: String,
    pub message: String,
    /// The next step to take, if any.
    #[serde(default)]
    pub action: Option<String>,
    /// The raw error.
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolVersionInfo {
    pub version: i64,
    pub author: String,
    pub created_at: String,
    pub capabilities: Vec<String>,
    pub memory_mb: u32,
    pub input_schema: String,
    pub output_schema: String,
    pub code: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolMetricsInfo {
    pub total: i64,
    pub success: i64,
    pub failure: i64,
    pub denied: i64,
    pub cancelled: i64,
    pub validation_failures: i64,
    pub success_rate: Option<f64>,
    pub retry_rate: Option<f64>,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    /// Distinct Sessions that invoked the Tool: a reuse/generality signal.
    #[serde(default)]
    pub sessions: i64,
    /// Cost per Success (SPEC §8.1 metric 3): total wall time and provider
    /// tokens of all Invocations, divided by the successes.
    #[serde(default)]
    pub ms_per_success: Option<f64>,
    #[serde(default)]
    pub tokens_per_success: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimiserAttemptInfo {
    pub id: String,
    pub base_version: i64,
    pub status: String,
    pub detail: String,
    pub candidate_score: Option<f64>,
    pub baseline_score: Option<f64>,
    pub replay_total: i64,
    pub regressions: i64,
    pub created_at: String,
}

/// One shared JS module in the registry (v2). Tools load the current
/// version's code via `requireModule(name)` at runtime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModuleInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub current_version: i64,
    pub retired: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// One audit-log entry (SPEC §14).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEntry {
    pub actor: String,
    pub action: String,
    pub detail: String,
    pub created_at: String,
}

/// A scheduled job (SPEC §9.7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobInfo {
    pub id: String,
    pub prompt: String,
    /// Run in this Session; None starts a new Session each time.
    pub session_id: Option<String>,
    /// Recurring schedule (5-field cron, local time); None is one-shot.
    pub cron: Option<String>,
    pub next_run: Option<String>,
    pub last_run: Option<String>,
    pub last_status: String,
    pub enabled: bool,
    /// Recurring jobs the agent created wait for the user's approval.
    pub approved: bool,
    /// "user" or "agent".
    pub author: String,
    pub created_at: String,
}

/// A built-in's measured use (SPEC §8.8).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BuiltinStatInfo {
    pub name: String,
    pub calls: i64,
    pub failures: i64,
    /// Counts per outcome kind: syntax, smoke, empty, near-duplicate, …
    pub kinds: std::collections::BTreeMap<String, i64>,
    pub avg_ms: f64,
    #[serde(skip)]
    pub total_ms: f64,
}

/// A running Invocation, for the Invocations panel (SPEC §6.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvocationInfo {
    pub handle: String,
    pub tool_name: String,
    pub session_id: String,
    pub elapsed_ms: u64,
    /// Since its last sign of activity.
    pub silent_ms: u64,
    pub stalled: bool,
    /// The agent no longer waits for it (it may still finish).
    pub abandoned: bool,
}

/// Token use (and, with prices configured, cost) of a Run or Session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageInfo {
    pub steps: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    /// Estimated USD, when every model used has a configured price.
    pub cost: Option<f64>,
    /// The latest step's prompt tokens and the model's context size
    /// (SPEC §10.6); 0 when unknown.
    #[serde(default)]
    pub context_used: i64,
    #[serde(default)]
    pub context_tokens: i64,
}

/// One Run's metrics (SPEC §8.7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunMetricsInfo {
    pub run_id: String,
    pub session_id: String,
    /// completed, interrupted, paused, resumed, running.
    pub status: String,
    /// The status, or `edited` / `corrected` when the user's next move
    /// says the Run missed its goal.
    pub outcome: String,
    /// Model calls.
    pub steps: i64,
    /// Registry Tool Invocations.
    pub tool_calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub duration_ms: i64,
    pub created_at: String,
}

/// Run metrics rolled up (SPEC §8.7).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RunSummary {
    pub runs: i64,
    /// Share of finished Runs per outcome (0..1).
    pub completed: f64,
    pub interrupted: f64,
    pub paused: f64,
    pub edited: f64,
    pub corrected: f64,
    pub avg_steps: f64,
    pub avg_tool_calls: f64,
    pub avg_tokens: f64,
    pub avg_duration_ms: f64,
}

/// A user-memory note (SPEC §7.11).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryInfo {
    pub id: String,
    pub text: String,
    /// "global" or "project:<path>".
    pub scope: String,
    /// "agent", "user" or "optimiser".
    pub author: String,
    /// The Session that wrote it (agent notes).
    pub session_id: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    /// When a Session's prompt last included it.
    pub last_used: Option<String>,
}

/// An installed skill (SPEC §7.10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub version: i64,
    pub enabled: bool,
    /// Where it was installed from (folder, git URL, zip).
    pub source: String,
    /// "user", "import" or "auto".
    pub author: String,
    /// Bundles scripts (which never run on the host).
    pub has_scripts: bool,
    pub retired: bool,
    /// Times a Session loaded it.
    pub loads: i64,
    /// Runs that loaded it, and how many of those completed.
    #[serde(default)]
    pub runs: i64,
    #[serde(default)]
    pub runs_completed: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// A configured MCP server and its state (SPEC §7.9).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerInfo {
    pub name: String,
    /// "stdio" or "http".
    pub transport: String,
    /// stdio: command line; http: URL.
    pub target: String,
    pub disabled: bool,
    pub running: bool,
    /// Active registry tools from this server.
    pub tool_count: usize,
    pub last_error: Option<String>,
    /// http: whether OAuth tokens are stored.
    pub logged_in: bool,
}

/// A sub-agent started by `agents.run` (its transcript is stored apart
/// from the Session's). Status: running | completed | failed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubagentInfo {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub task: String,
    /// Sub-agent nesting (1 = started by the main agent).
    pub depth: u32,
    /// The role and model it ran on ("explorer · qwen3", or just a model).
    #[serde(default)]
    pub model: String,
    pub status: String,
    pub steps: i64,
    pub result: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A versioned prompt/memory self-optimisation proposal (v2). Status:
/// proposed -> measured -> applied | rejected | rolled_back.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromptProposalInfo {
    pub id: String,
    /// "prompt" (bootstrap addendum) or "memory" (learned note).
    pub kind: String,
    pub content: String,
    pub rationale: String,
    pub status: String,
    pub baseline_score: Option<f64>,
    pub candidate_score: Option<f64>,
    pub eval_total: i64,
    pub est_tokens: i64,
    pub created_at: String,
    pub applied_at: Option<String>,
}

/// Measured provider outcome for one model (routing, v2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelStatInfo {
    pub model: String,
    pub successes: i64,
    pub failures: i64,
    pub total_ms: i64,
    /// Provider-reported tokens over all recorded requests (0 when the
    /// endpoint reports none).
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    /// Part of `prompt_tokens` served from the prompt cache.
    #[serde(default)]
    pub cached_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDetail {
    pub tool: ToolInfo,
    pub versions: Vec<ToolVersionInfo>,
    pub metrics: ToolMetricsInfo,
    #[serde(default)]
    pub optimiser_attempts: Vec<OptimiserAttemptInfo>,
    /// Metrics of each version, as (version, metrics).
    #[serde(default)]
    pub version_metrics: Vec<(i64, ToolMetricsInfo)>,
    /// The accumulated approval grant, if any (SPEC §10.3).
    #[serde(default)]
    pub approval: Option<ApprovalGrant>,
}

/// What the user has allowed a Tool, accumulated across approvals.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalGrant {
    pub capabilities: Vec<String>,
    pub memory_mb: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    /// Idle: start a new Run. Busy: same as Steer is NOT implied; use Steer.
    Send,
    /// Inject at next step boundary (Enter during a Run).
    Steer,
    /// Start after the current Run ends (Ctrl+Enter).
    Queue,
}

/// One question of an `ask_user` call (SPEC §5.5).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Question {
    pub question: String,
    /// A short label (a few words) clients may show as a chip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    /// The user may pick several options.
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The answer to one `Question`: the option labels picked, and/or the
/// user's own text (the answer is never limited to the options).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct QuestionAnswer {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl Question {
    /// The one question an older daemon sends as `question`/`options`.
    pub fn single(question: String, options: Vec<String>) -> Self {
        Question {
            question,
            header: None,
            options: options
                .into_iter()
                .map(|label| QuestionOption {
                    label,
                    description: None,
                })
                .collect(),
            multi_select: false,
        }
    }

    /// Read a typed reply: option numbers ("2", or "1,3" when
    /// `multi_select`) pick those options; anything else is free text.
    pub fn parse_reply(&self, line: &str) -> QuestionAnswer {
        let line = line.trim();
        let picks: Option<Vec<usize>> = line
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(|t| {
                t.parse::<usize>()
                    .ok()
                    .filter(|n| (1..=self.options.len()).contains(n))
            })
            .collect();
        match picks {
            Some(p) if !p.is_empty() && (self.multi_select || p.len() == 1) => {
                let mut selected = Vec::new();
                for n in p {
                    let label = &self.options[n - 1].label;
                    if !selected.contains(label) {
                        selected.push(label.clone());
                    }
                }
                QuestionAnswer {
                    selected,
                    text: None,
                }
            }
            _ => QuestionAnswer {
                selected: Vec::new(),
                text: (!line.is_empty()).then(|| line.to_string()),
            },
        }
    }
}

impl QuestionAnswer {
    /// The answer as one line of text: picked labels, then own text.
    pub fn summary(&self) -> String {
        let mut parts = self.selected.clone();
        parts.extend(self.text.clone());
        parts.join(", ")
    }

    /// Several answers as one line, for `AnswerQuestion.answer`.
    pub fn summary_of(answers: &[QuestionAnswer]) -> String {
        answers
            .iter()
            .map(QuestionAnswer::summary)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Status,
    /// Read-only configuration summary for remote clients (no secrets).
    GetSettings,
    Shutdown,
    ListSessions,
    CreateSession {
        name: Option<String>,
    },
    RenameSession {
        id: String,
        name: String,
    },
    DeleteSession {
        id: String,
    },
    GetSession {
        id: String,
    },
    ForkSession {
        id: String,
        name: Option<String>,
        #[serde(default)]
        up_to_seq: Option<i64>,
    },
    SearchSessions {
        query: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Edit a past user message, drop everything after it, and re-run.
    EditMessage {
        session_id: String,
        message_id: String,
        content: String,
    },
    SetSessionModel {
        id: String,
        model: Option<String>,
        reasoning_effort: Option<String>,
    },
    SendMessage {
        session_id: String,
        content: String,
        mode: SendMode,
    },
    InterruptRun {
        session_id: String,
    },
    /// Continue a Session whose last Run was interrupted or paused, from
    /// its persisted transcript (SPEC §9.5).
    ResumeRun {
        session_id: String,
    },
    /// Give up on a Session's interrupted or paused Run.
    AbandonRun {
        session_id: String,
    },
    /// Choose which events this connection receives. `sessions: None` =
    /// every Session; otherwise only those Sessions' transcript and live
    /// events. `deltas: false` drops streamed answer/reasoning tokens.
    /// Global events (Tools, approvals, Session list and Run start/end)
    /// always arrive. A connection starts subscribed to everything.
    Subscribe {
        #[serde(default)]
        sessions: Option<Vec<String>>,
        #[serde(default = "default_true")]
        deltas: bool,
    },
    ListSubagents {
        session_id: String,
    },
    ListMcpServers,
    /// Add (or replace) a server: `server` is its `[mcp.servers.<name>]`
    /// table as JSON. Its tools are listed into the registry at once.
    AddMcpServer {
        name: String,
        server: serde_json::Value,
    },
    /// Remove a server; its tools are retired (kept for metrics).
    RemoveMcpServer {
        name: String,
    },
    /// Stop the server's process (if any) and list its tools again.
    RestartMcpServer {
        name: String,
    },
    /// Prompts offered by the configured MCP servers.
    ListMcpPrompts,
    ListSkills,
    /// Install a skill from a folder, a git URL (`git_ref`, `subdir`) or a
    /// `.zip`. New installs start disabled: review, then enable.
    AddSkill {
        source: String,
        #[serde(default)]
        subdir: Option<String>,
        #[serde(default)]
        git_ref: Option<String>,
    },
    RemoveSkill {
        name: String,
    },
    SetSkillEnabled {
        name: String,
        enabled: bool,
    },
    /// A skill with its SKILL.md body and file list, for review.
    GetSkill {
        name: String,
    },
    /// Recent Runs' metrics and their summary (SPEC §8.7):
    /// `{"runs": [RunMetricsInfo], "summary": RunSummary}`.
    RunMetrics {
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Built-in call stats (SPEC §8.8).
    ListBuiltinStats,
    /// Attach a file to the Session's next user message (SPEC §5.6): stored
    /// as an artifact; returns `{id, name, mime, bytes}`.
    AttachFile {
        session_id: String,
        name: String,
        data_b64: String,
        #[serde(default)]
        mime: Option<String>,
    },
    /// Files under `root` (default: the daemon's working directory) for `@`
    /// mentions (SPEC §5.6): `{root, query, files: [{path, dir, bytes}],
    /// truncated}`,
    /// best match first.
    ListFiles {
        #[serde(default)]
        root: Option<String>,
        #[serde(default)]
        query: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Attach a file the daemon can read (an `@` mention) to the Session's
    /// next user message. `path` is absolute or relative to `root`. Small
    /// files are stored like `AttachFile`; large files and directories go
    /// as a reference the agent reads with its own Tools. Returns
    /// `{name, bytes, mime?, id?, reference}`.
    AttachPath {
        session_id: String,
        path: String,
        #[serde(default)]
        root: Option<String>,
    },
    /// Serve the web UI on 127.0.0.1 (SPEC §12); returns a one-time
    /// login `url`. `port` 0 or unset picks a free one.
    StartWebUi {
        #[serde(default)]
        port: Option<u16>,
    },
    StopWebUi,
    /// The audit log, newest first (SPEC §14): `filter` matches actor,
    /// action or detail; `before` (an entry's `created_at`) pages back.
    ListAudit {
        #[serde(default)]
        filter: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        before: Option<String>,
    },
    /// Change one setting (SPEC §13), validated by the daemon against an
    /// allow-list; credentials and endpoints are never settable.
    SetSetting {
        key: String,
        value: serde_json::Value,
    },
    /// Revoke one granted Capability spec of a Tool, or (without
    /// `capability`) its whole grant; the next call asks again.
    RevokeApproval {
        tool_id: String,
        #[serde(default)]
        capability: Option<String>,
    },
    /// How full a Session's context is (SPEC §10.6): `{model,
    /// context_tokens, used_tokens, measured, breakdown: {system_prompt,
    /// tool_schemas, history, attachments}}`. `used_tokens` is the last
    /// prompt's count when known (`measured`), else an estimate; the
    /// breakdown is estimated (about 4 characters per token).
    ContextUsage {
        session_id: String,
    },
    /// A Session's usage: `{"last_run": UsageInfo?, "total": UsageInfo, "runs": n}`.
    SessionUsage {
        session_id: String,
    },
    /// Undo a Session delete within its window (`daemon.delete_undo_secs`).
    RestoreSession {
        id: String,
    },
    /// Read a stored output (SPEC §9.6): a text slice from `offset`
    /// characters (default 200 000 long). Never returns the file path.
    GetArtifact {
        id: String,
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        length: Option<usize>,
    },
    /// Providers and Roles (SPEC §4.5): `{"providers": [...], "roles": {...}}`
    /// (never API keys; `has_key` says whether one is stored).
    ListProviders,
    /// Models to pick from (SPEC §4.5): `{"models": [id], "roles":
    /// [{"name", "provider", "model"}], "recent": [id], "current": {"model",
    /// "reasoning_effort"}, "error": str?}`. `models` comes from the default
    /// provider's `GET /models`; `recent` is the Sessions' models, newest first.
    ListModels,
    /// Add or replace a named provider; `api_key` goes to the keychain.
    UpsertProvider {
        name: String,
        base_url: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        api_key: Option<String>,
    },
    /// Remove a named provider (refused while a role uses it).
    RemoveProvider {
        name: String,
    },
    /// List a provider's models (a connectivity test).
    TestProvider {
        name: String,
    },
    /// Create or replace a Role.
    UpsertRole {
        name: String,
        role: serde_json::Value,
    },
    DeleteRole {
        name: String,
    },
    /// Changes the agent made to the harness, newest first (SPEC §7.13).
    ListHarnessChanges,
    /// Revert one (or, without `id`, the latest not yet undone).
    UndoHarnessChange {
        #[serde(default)]
        id: Option<String>,
    },
    /// Scheduled jobs (SPEC §9.7).
    ListJobs,
    /// Add a job as the user (approved): `cron` for recurring, else `at`.
    AddJob {
        prompt: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        cron: Option<String>,
        #[serde(default)]
        at: Option<String>,
    },
    /// Enabling also approves an agent-created recurring job.
    SetJobEnabled {
        id: String,
        enabled: bool,
    },
    DeleteJob {
        id: String,
    },
    /// Answer an `ask_user` question (first answer wins). `answers` holds
    /// one entry per `UserQuestion.questions`, in order; clients that only
    /// know `answer` send free text and leave it empty.
    AnswerQuestion {
        question_id: String,
        #[serde(default)]
        answer: String,
        #[serde(default)]
        answers: Vec<QuestionAnswer>,
    },
    /// User-memory notes (SPEC §7.11).
    ListMemories,
    /// Add a note (no `id`, author `user`) or edit one: text, scope,
    /// enabled. Unset fields keep their value.
    UpsertMemory {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        enabled: Option<bool>,
    },
    DeleteMemory {
        id: String,
    },
    /// A skill's files as a `formless.skill/1` bundle (no metrics).
    ExportSkill {
        name: String,
    },
    /// Every skill, as `formless.skills/1`.
    ExportSkills,
    /// Install a `formless.skill/1` or `formless.skills/1` bundle: author
    /// `import`, installed disabled for review.
    ImportSkill {
        bundle: serde_json::Value,
    },
    /// One MCP prompt rendered to text (for the composer).
    GetMcpPrompt {
        server: String,
        name: String,
        #[serde(default)]
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// What changed between two versions of a Tool, as diff text.
    DiffToolVersions {
        id: String,
        from: i64,
        to: i64,
    },
    /// A sub-agent and its transcript (provider-shaped messages).
    GetSubagent {
        id: String,
    },
    /// Stop one running Tool Invocation without interrupting its Run (it is
    /// recorded as user-cancelled, and the agent gets the error result).
    CancelInvocation {
        handle: String,
    },
    /// Running Invocations across Sessions, oldest first.
    ListInvocations,
    /// Stop waiting for an Invocation (the agent is told it was abandoned).
    AbandonInvocation {
        handle: String,
    },
    /// Abort the active Run's in-flight provider request and send it again
    /// (for a stalled stream; nothing in the transcript is lost).
    RetryProviderRequest {
        session_id: String,
    },
    ListTools,
    ListModules,
    GetTool {
        id: String,
    },
    /// Save edited Tool JS as a new version (in-TUI editor, v2).
    UpdateToolCode {
        id: String,
        code: String,
    },
    RetireTool {
        id: String,
    },
    RestoreTool {
        id: String,
    },
    PurgeTool {
        id: String,
    },
    RollbackTool {
        id: String,
        version: i64,
    },
    ApprovalDecision {
        approval_id: String,
        approved: bool,
        /// How long an approval lasts; older clients omit it (`Always`).
        #[serde(default)]
        scope: ApprovalScope,
    },
    PromotionDecision {
        promotion_id: String,
        approved: bool,
    },
    RunOptimiser {
        tool_id: Option<String>,
    },
    ListProposals,
    ListModelStats,
    GenerateProposal,
    ProposalDecision {
        id: String,
        approved: bool,
    },
    RollbackProposal {
        id: String,
    },
    ExportTool {
        id: String,
    },
    ExportTools,
    ImportTool {
        bundle: serde_json::Value,
    },
    ExportSession {
        id: String,
    },
    ImportSession {
        bundle: serde_json::Value,
    },
}

fn default_true() -> bool {
    true
}

/// How long an approval lasts (SPEC §10.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// This call only; nothing is stored.
    Once,
    /// Stored for the Tool until its Capabilities widen.
    #[default]
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    pub protocol: u32,
    pub request: Request,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    Response {
        id: u64,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Event(Event),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RunStarted {
        session_id: String,
        run_id: String,
    },
    MessageAppended {
        session_id: String,
        message: MessageInfo,
    },
    MessageDelta {
        session_id: String,
        run_id: String,
        delta: String,
    },
    /// Live model reasoning chunk (ephemeral; never the stored answer).
    ReasoningDelta {
        session_id: String,
        run_id: String,
        delta: String,
    },
    MessageComplete {
        session_id: String,
        run_id: String,
        message: MessageInfo,
    },
    /// The provider stream dropped and the request is being re-sent:
    /// clients discard the partial answer and reasoning shown so far.
    StreamRestarted {
        session_id: String,
        run_id: String,
    },
    /// An open provider stream has sent nothing for `idle_secs` (sent once
    /// per stall; any later delta means it recovered).
    ProviderStalled {
        session_id: String,
        run_id: String,
        idle_secs: u64,
    },
    RunFinished {
        session_id: String,
        run_id: String,
        status: String,
    },
    RunPaused {
        session_id: String,
        run_id: String,
        error: String,
        /// The error sorted into a category with a next step (SPEC §4.4).
        #[serde(default)]
        explained: Option<ErrorExplained>,
    },
    CompactionNotice {
        session_id: String,
        summary_preview: String,
    },
    Note {
        session_id: Option<String>,
        text: String,
    },
    SessionUpdated {
        session: SessionInfo,
    },
    SessionDeleted {
        id: String,
    },
    /// Transcript was edited/truncated; clients should reload the session.
    MessagesEdited {
        session_id: String,
    },
    ToolCallStarted {
        session_id: String,
        run_id: String,
        tool_name: String,
        version: i64,
        /// Raw JSON arguments (display only; empty for older daemons).
        #[serde(default)]
        arguments: String,
        /// `tools.call` nesting: 0 for the agent's own calls, 1 for a call
        /// made inside that Tool, … (clients draw a tree).
        #[serde(default)]
        depth: usize,
    },
    ToolCallFinished {
        session_id: String,
        run_id: String,
        tool_name: String,
        ok: bool,
        #[serde(default)]
        elapsed_ms: i64,
    },
    /// The agent asks the user something mid-Run (`ask_user`); answer with
    /// `AnswerQuestion`. Global like approvals. `questions` is the full
    /// structured form (1–4); `question`/`options` summarise it for
    /// clients that predate it.
    UserQuestion {
        question_id: String,
        session_id: String,
        question: String,
        #[serde(default)]
        options: Vec<String>,
        #[serde(default)]
        questions: Vec<Question>,
    },
    /// A question was answered (or abandoned): clients close their prompt.
    UserQuestionClosed {
        question_id: String,
    },
    ApprovalRequested {
        approval_id: String,
        session_id: String,
        tool_name: String,
        version: i64,
        capabilities: Vec<String>,
        memory_mb: u32,
        /// What exactly is being approved, when not a Tool's Capabilities
        /// alone (e.g. the command an agent wants to add as an MCP server).
        #[serde(default)]
        detail: Option<String>,
        /// Capabilities this Tool already holds (so clients show what is new).
        #[serde(default)]
        already_allowed: Vec<String>,
        /// The first lines of the Tool's code, to see what is being trusted.
        #[serde(default)]
        code_preview: Option<String>,
    },
    ToolUpdated {
        tool: ToolInfo,
    },
    ModuleUpdated {
        module: ModuleInfo,
    },
    ProposalUpdated {
        proposal: PromptProposalInfo,
    },
    ToolDeleted {
        id: String,
    },
    InvocationPending {
        session_id: String,
        run_id: String,
        handle: String,
        tool_name: String,
    },
    InvocationCompleted {
        session_id: String,
        handle: String,
        tool_name: String,
        ok: bool,
    },
    /// A running Invocation has shown no activity for `silent_secs`
    /// (SPEC §6.1: advisory, never auto-killed).
    InvocationStalled {
        session_id: String,
        handle: String,
        tool_name: String,
        silent_secs: u64,
    },
    SubagentStarted {
        session_id: String,
        subagent: SubagentInfo,
    },
    /// A sub-agent finished a model step; `tool_calls` names the Tools it
    /// is calling next (empty on its final answer).
    SubagentStep {
        session_id: String,
        subagent_id: String,
        step: i64,
        tool_calls: Vec<String>,
    },
    SubagentFinished {
        session_id: String,
        subagent: SubagentInfo,
    },
    /// config.toml changed on disk; the daemon now uses the new settings.
    SettingsChanged,
    /// MCP servers were added, removed or changed state.
    McpServersChanged,
    /// Skills were installed, removed, enabled or disabled.
    SkillsChanged,
    /// A Run's usage so far, after each model step (SPEC §10.6).
    RunUsage {
        session_id: String,
        run_id: String,
        usage: UsageInfo,
    },
    /// User memory changed (SPEC §7.11).
    MemoryUpdated,
    /// Scheduled jobs changed (SPEC §9.7).
    JobsChanged,
    /// A stalled Invocation is active again (clears the stall flag).
    InvocationActivity {
        session_id: String,
        handle: String,
    },
    OptimiserActivity {
        tool_name: String,
        text: String,
    },
    PromotionApprovalRequested {
        promotion_id: String,
        tool_name: String,
        reason: String,
        capabilities: Vec<String>,
        memory_mb: u32,
    },
}

impl Event {
    /// The Session this event belongs to, or `None` for global events.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::RunStarted { session_id, .. }
            | Event::MessageAppended { session_id, .. }
            | Event::MessageDelta { session_id, .. }
            | Event::ReasoningDelta { session_id, .. }
            | Event::MessageComplete { session_id, .. }
            | Event::StreamRestarted { session_id, .. }
            | Event::ProviderStalled { session_id, .. }
            | Event::RunFinished { session_id, .. }
            | Event::RunPaused { session_id, .. }
            | Event::CompactionNotice { session_id, .. }
            | Event::MessagesEdited { session_id, .. }
            | Event::ToolCallStarted { session_id, .. }
            | Event::ToolCallFinished { session_id, .. }
            | Event::ApprovalRequested { session_id, .. }
            | Event::UserQuestion { session_id, .. }
            | Event::RunUsage { session_id, .. }
            | Event::InvocationPending { session_id, .. }
            | Event::InvocationCompleted { session_id, .. }
            | Event::InvocationStalled { session_id, .. }
            | Event::InvocationActivity { session_id, .. }
            | Event::SubagentStarted { session_id, .. }
            | Event::SubagentStep { session_id, .. }
            | Event::SubagentFinished { session_id, .. } => Some(session_id),
            Event::Note { session_id, .. } => session_id.as_deref(),
            Event::SessionUpdated { session } => Some(&session.id),
            Event::SessionDeleted { id } => Some(id),
            Event::SettingsChanged
            | Event::McpServersChanged
            | Event::SkillsChanged
            | Event::MemoryUpdated
            | Event::JobsChanged
            | Event::UserQuestionClosed { .. }
            | Event::ToolUpdated { .. }
            | Event::ModuleUpdated { .. }
            | Event::ProposalUpdated { .. }
            | Event::ToolDeleted { .. }
            | Event::OptimiserActivity { .. }
            | Event::PromotionApprovalRequested { .. } => None,
        }
    }
}

impl ServerMessage {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        ServerMessage::Response {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: u64, error: String) -> Self {
        ServerMessage::Response {
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

/// Artifact ids referenced in a tool result: the "stored as artifact <id>"
/// note and `"emitted_artifact": "<id>"` (clients make them openable).
pub fn artifact_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for marker in ["stored as artifact ", "\"emitted_artifact\":\""] {
        let mut rest = text;
        while let Some(i) = rest.find(marker) {
            rest = &rest[i + marker.len()..];
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
                .collect();
            if id.len() == 36 && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_session_id_scopes_session_events() {
        let ev = Event::ApprovalRequested {
            approval_id: "a".into(),
            session_id: "s1".into(),
            tool_name: "t".into(),
            version: 1,
            capabilities: vec![],
            memory_mb: 0,
            detail: None,
            already_allowed: vec![],
            code_preview: None,
        };
        assert_eq!(ev.session_id(), Some("s1"));
        let note = Event::Note {
            session_id: None,
            text: "x".into(),
        };
        assert_eq!(note.session_id(), None);
        let global = Event::ToolDeleted { id: "t".into() };
        assert_eq!(global.session_id(), None);
    }

    #[test]
    fn question_reply_picks_numbers_or_keeps_text() {
        let opt = |l: &str| QuestionOption {
            label: l.into(),
            description: None,
        };
        let mut q = Question {
            question: "Which?".into(),
            header: None,
            options: vec![opt("csv"), opt("json"), opt("xml")],
            multi_select: false,
        };
        assert_eq!(q.parse_reply(" 2 ").selected, ["json"]);
        // Several numbers on a single-select question are free text.
        assert_eq!(q.parse_reply("1,3").text.as_deref(), Some("1,3"));
        assert_eq!(q.parse_reply("4").text.as_deref(), Some("4"));
        q.multi_select = true;
        let a = q.parse_reply("3, 1 3");
        assert_eq!(a.selected, ["xml", "csv"]);
        assert_eq!(a.summary(), "xml, csv");
        assert_eq!(q.parse_reply("parquet").summary(), "parquet");
    }

    #[test]
    fn legacy_question_messages_still_parse() {
        let ev: Event = serde_json::from_str(
            r#"{"type":"user_question","question_id":"q","session_id":"s","question":"Which?","options":["a"]}"#,
        )
        .unwrap();
        assert!(matches!(ev, Event::UserQuestion { questions, .. } if questions.is_empty()));
        let req: Request =
            serde_json::from_str(r#"{"type":"answer_question","question_id":"q","answer":"a"}"#)
                .unwrap();
        assert!(matches!(req, Request::AnswerQuestion { answers, .. } if answers.is_empty()));
    }
}
