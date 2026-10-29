//! Shared daemon <-> client protocol (SPEC §9.3). NDJSON framing: one JSON
//! value per line.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// A browser connected through the relay (SPEC §12).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelayClientInfo {
    /// The relay's id for this browser connection.
    pub cid: String,
    #[serde(default)]
    pub user_agent: String,
    pub first_seen: String,
    pub last_seen: String,
}

/// A client connection showing a Session (SPEC §10.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttachedClient {
    pub conn: u64,
    pub label: String,
}

/// A saved starting point for new Sessions (SPEC §5.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TemplateInfo {
    pub name: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Put in the input of a Session made from it, to edit and send.
    #[serde(default)]
    pub first_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// The Session's working directory: the base for relative paths and
    /// `@` mentions (SPEC §5.1).
    #[serde(default)]
    pub cwd: Option<String>,
    /// For the Sessions list (SPEC §10.4): how many messages, the last
    /// user message (first line, shortened) and the last Run's status.
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub last_run_status: Option<String>,
    /// Pinned Sessions sort first (SPEC §10.4).
    #[serde(default)]
    pub pinned: bool,
    /// Archived: hidden from the default list, still searchable.
    #[serde(default)]
    pub archived: bool,
    /// Free-form tags for grouping (SPEC §10.4), lowercase, sorted.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Standing instructions for this Session's agent (SPEC §5.1).
    #[serde(default)]
    pub instructions: Option<String>,
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

/// [`Request::SecurityStatus`]'s answer as text, for the CLI and TUI.
pub fn security_report(v: &serde_json::Value) -> String {
    let mut out = Vec::new();
    let list = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect()
    };
    out.push(format!(
        "Approvals: {} grant(s){}",
        v["grants_total"],
        v["grants"]
            .as_object()
            .filter(|m| !m.is_empty())
            .map(|m| format!(
                " ({})",
                m.iter()
                    .map(|(k, n)| format!("{k} {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .unwrap_or_default()
    ));
    if v["broad_grants"].as_array().is_some_and(|a| !a.is_empty()) {
        out.push(format!(
            "  ⚠ broad: {}",
            list(&v["broad_grants"]).join(", ")
        ));
    }
    out.push(format!("Always denied: {}", v["denials"]));
    let trusted = list(&v["trusted_paths"]);
    out.push(if trusted.is_empty() {
        "Trusted folders: none".into()
    } else {
        format!(
            "Trusted folders{}: {}",
            if v["trust_writes"] == false {
                " (writes still ask)"
            } else {
                ""
            },
            trusted.join(", ")
        )
    });
    let servers = v["mcp_servers"].as_array().cloned().unwrap_or_default();
    out.push(format!("MCP servers: {}", servers.len()));
    for s in &servers {
        out.push(format!(
            "  {} ({}) {}{}{}",
            s["name"].as_str().unwrap_or_default(),
            s["transport"].as_str().unwrap_or_default(),
            s["target"].as_str().unwrap_or_default(),
            if s["trusted"] == true {
                " · trusted"
            } else {
                ""
            },
            if s["disabled"] == true {
                " · disabled"
            } else {
                ""
            },
        ));
    }
    let hooks = v["trusted_hooks"].as_array().cloned().unwrap_or_default();
    out.push(format!("Hooks run without asking: {}", hooks.len()));
    for h in &hooks {
        out.push(format!(
            "  {}: {}",
            h["event"].as_str().unwrap_or_default(),
            h["command"].as_str().unwrap_or_default()
        ));
    }
    out.push(format!(
        "Relay: {}",
        if v["relay"]["enabled"] == true {
            format!("on ({})", v["relay"]["url"].as_str().unwrap_or_default())
        } else {
            "off".into()
        }
    ));
    out.push(format!(
        "Web UI: {}",
        match v["web_ui"]["port"].as_u64() {
            Some(p) => format!("running on 127.0.0.1:{p}"),
            None => "off".into(),
        }
    ));
    out.join("\n")
}

/// A Capability the user always denies (SPEC §10.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DenialInfo {
    pub capability: String,
    /// The Tool whose prompt it was denied on.
    pub tool_name: String,
    #[serde(default)]
    pub denied_by: String,
    pub created_at: String,
}

/// The scopes a path Capability on an approval can be granted with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PathOption {
    /// The requested spec (`fs:write:/home/me/proj/src/foo.rs`).
    pub capability: String,
    /// Narrowest first; the first is the path itself.
    pub choices: Vec<PathChoice>,
}

/// One scope a path can be granted with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PathChoice {
    pub path: String,
    /// `file`, `folder`, `repo` or `project`.
    pub kind: String,
    /// The repository kind (`git`, `Mercurial`, …) or project file.
    #[serde(default)]
    pub label: String,
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
    /// A short human title ("Last git author"); empty when the Tool has
    /// none. UIs show it first, with the name as secondary text.
    #[serde(default)]
    pub title: String,
    /// Lowercase topic tags (`git`, `web`, `finance`), set by the agent or
    /// the user (SPEC §7.2).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Pinned by the user: always listed to the model (SPEC §7.1).
    #[serde(default)]
    pub pinned: bool,
    /// Switched off by the user: hidden from the agent and uncallable
    /// until switched back on; versions and metrics untouched (SPEC §7.2).
    #[serde(default)]
    pub disabled: bool,
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
    /// Outcomes of the last 40 finished Invocations, oldest first: `s`
    /// success, `f` failure (SPEC §10.4 sparkline and trend).
    #[serde(default)]
    pub recent: String,
    /// Who wrote the current version: agent, user, optimiser, import.
    #[serde(default)]
    pub author: String,
}

/// A registry change the agent made during a Run (SPEC §10.6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolChange {
    pub tool_id: String,
    pub name: String,
    /// The version the Run left it at.
    pub version: i64,
    /// created, updated or retired.
    pub change: String,
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
    /// Tokens the Tool's own model use (`llm`, `agent`) spent per call, on
    /// average; `None` when it has never used a model (SPEC §10.4).
    #[serde(default)]
    pub model_tokens_per_call: Option<f64>,
    /// Their estimated USD per call, when every model it used is priced.
    #[serde(default)]
    pub model_cost_per_call: Option<f64>,
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

/// Why a Capability is unusually broad, if it is (SPEC §10.3): bare `net`
/// or `process`, `fs:write` on the whole disk or a home folder, or `env`
/// for a key that looks like a secret. Approval prompts flag these loudly
/// and ask twice before granting them beyond one call.
pub fn broad_capability(spec: &str) -> Option<&'static str> {
    match spec {
        "net" => return Some("any host on the internet"),
        "process" => return Some("any program on this computer"),
        _ => {}
    }
    if let Some(path) = spec.strip_prefix("fs:write:") {
        let p = path.trim_end_matches('/');
        let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        let home = matches!(
            parts.as_slice(),
            [] | ["Users" | "home"] | ["Users" | "home", _] | ["root"]
        );
        if p.is_empty() || p == "~" || home {
            return Some("writing anywhere in your files");
        }
    }
    if let Some(key) = spec.strip_prefix("env:") {
        let k = key.to_uppercase();
        let secret = [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "CREDENTIAL",
            "PRIVATE",
            "AUTH",
        ]
        .iter()
        .any(|w| k.contains(w))
            || k.ends_with("KEY")
            || k.ends_with("_PAT");
        if secret {
            return Some("a secret from your environment");
        }
    }
    None
}

/// Where an `fs:` Capability's path lies (SPEC §10.3), for approval
/// prompts: `(warn, note)`. Folders that hold secrets warn whatever the
/// working folder; otherwise a path outside the Session's working folder
/// warns and one inside it doesn't. `None` for other Capabilities, or
/// when there is no working folder and nothing stands out.
pub fn path_note(spec: &str, cwd: Option<&str>) -> Option<(bool, String)> {
    let path = spec
        .strip_prefix("fs:read:")
        .or_else(|| spec.strip_prefix("fs:write:"))?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    const SECRET_DIRS: [(&str, &str); 7] = [
        (".ssh", "SSH keys"),
        (".aws", "cloud credentials"),
        (".gnupg", "encryption keys"),
        (".kube", "cluster credentials"),
        (".docker", "registry credentials"),
        ("Keychains", "the keychain"),
        (".password-store", "saved passwords"),
    ];
    for (dir, what) in SECRET_DIRS {
        if segments.contains(&dir) {
            return Some((true, format!("holds {what}")));
        }
    }
    if segments
        .windows(2)
        .any(|w| w == [".config", "gh"] || w == [".config", "gcloud"])
    {
        return Some((true, "holds account credentials".into()));
    }
    if segments.first() == Some(&"etc") {
        return Some((true, "system configuration".into()));
    }
    let cwd = cwd.filter(|c| !c.is_empty())?.trim_end_matches('/');
    let inside = path == cwd || path.starts_with(&format!("{cwd}/"));
    Some(if inside {
        (false, "inside the working folder".into())
    } else {
        (true, format!("outside the working folder {cwd}"))
    })
}

/// One granted Capability, for the central approvals list (SPEC §10.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GrantInfo {
    pub tool_id: String,
    pub tool_name: String,
    pub capability: String,
    /// always, session or hour.
    pub scope: ApprovalScope,
    /// When the grant was last widened (RFC 3339).
    #[serde(default)]
    pub granted_at: Option<String>,
    /// The client that answered ("TUI", "web", …); empty when unknown.
    #[serde(default)]
    pub granted_by: String,
    /// When an `hour` grant ends.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// The Session a `session` grant belongs to.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The Tool's latest Invocation.
    #[serde(default)]
    pub last_used: Option<String>,
    /// Trusted folders or hosts in config.toml approve this Capability
    /// anyway, so revoking the grant won't make it ask (SPEC §10.3).
    #[serde(default)]
    pub trusted: bool,
}

/// One call of a parallel batch that waits for approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeldCall {
    pub tool_name: String,
    /// Its Capabilities, input-bound scopes resolved to this call's values.
    pub capabilities: Vec<String>,
    /// Those not already granted.
    pub new_capabilities: Vec<String>,
}

/// A held call's place in its batch (SPEC §5.2): `index` is 1-based.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalBatch {
    pub batch_id: String,
    pub index: usize,
    pub calls: Vec<HeldCall>,
}

/// The latest change to the Tool registry, the one `/undo tool` reverts
/// (SPEC §7.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryChange {
    pub tool_id: String,
    pub name: String,
    /// created, updated, retired, restored or rolled_back.
    pub action: String,
    /// The current version before the change (0 for a new Tool).
    pub from_version: i64,
    /// The current version after it.
    pub to_version: i64,
    /// agent, user, import or optimiser.
    pub author: String,
    pub at: String,
}

impl RegistryChange {
    /// "agent created fetch_page v1", "user retired fetch_page".
    pub fn summary(&self) -> String {
        match self.action.as_str() {
            "retired" | "restored" => format!("{} {} {}", self.author, self.action, self.name),
            "rolled_back" => format!(
                "{} rolled {} back to v{}",
                self.author, self.name, self.to_version
            ),
            action => format!(
                "{} {action} {} v{}",
                self.author, self.name, self.to_version
            ),
        }
    }
}

/// One version of a module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModuleVersionInfo {
    pub version: i64,
    pub author: String,
    pub created_at: String,
    pub code: String,
}

/// A module with its history and the Tools that load it (SPEC §7.7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModuleDetail {
    pub module: ModuleInfo,
    /// Oldest first.
    pub versions: Vec<ModuleVersionInfo>,
    /// Active Tools whose current code loads it, sorted.
    pub used_by: Vec<String>,
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
    /// When a firing comes while the previous one is still queued or
    /// running: `skip` (default) or `queue` (SPEC §9.7).
    #[serde(default = "default_overlap")]
    pub overlap: String,
    /// Where the answer goes when a firing's Run ends: `{"notify": true}`,
    /// `{"file": "/abs/path"}` and/or `{"webhook": "https://…"}`.
    #[serde(default)]
    pub deliver: Option<serde_json::Value>,
}

fn default_overlap() -> String {
    "skip".into()
}

/// One firing of a job (SPEC §9.7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct JobRunInfo {
    pub id: String,
    pub job_id: String,
    pub session_id: Option<String>,
    /// `schedule` or `manual`.
    pub trigger: String,
    /// `running`, then the Run's outcome (`completed`, `failed`, …), or
    /// `skipped`.
    pub status: String,
    /// Late firings coalesced, why it was skipped, delivery results.
    #[serde(default)]
    pub detail: String,
    /// USD, when known.
    #[serde(default)]
    pub cost: Option<f64>,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
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
    /// Saved memories the Run's prompt carried (SPEC §7.11).
    #[serde(default)]
    pub memories: i64,
}

/// One Run's metrics (SPEC §8.7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunMetricsInfo {
    pub run_id: String,
    pub session_id: String,
    /// completed, interrupted, paused, resumed, running.
    pub status: String,
    /// The status, or `edited` / `corrected` when the user's next move
    /// says the Run missed its goal, or `rated_bad` after a 👎.
    pub outcome: String,
    /// The user's own verdict: "good", "bad" or "".
    #[serde(default)]
    pub rating: String,
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
    /// Shares of finished Runs the user rated 👍 / 👎.
    #[serde(default)]
    pub rated_good: f64,
    #[serde(default)]
    pub rated_bad: f64,
    pub avg_steps: f64,
    pub avg_tool_calls: f64,
    pub avg_tokens: f64,
    pub avg_duration_ms: f64,
}

/// One model's answer in a comparison (SPEC §5.10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComparisonResult {
    pub model: String,
    /// The fork the Run happened in (open it to continue there).
    pub session_id: String,
    /// completed, paused or interrupted.
    pub status: String,
    pub answer: String,
    pub duration_ms: i64,
    pub tokens: i64,
    /// USD, when every model used has a price.
    pub cost: Option<f64>,
    pub steps: i64,
}

/// One step of the agent's plan (`update_plan`, SPEC §5.8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanItem {
    pub content: String,
    /// "pending", "in_progress" or "completed".
    pub status: String,
}

/// "What I did": a long Run's recap from its trace, no model call
/// (SPEC §5.7).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RunRecap {
    pub steps: i64,
    pub duration_ms: i64,
    /// "read_file ×3", "fetch_page ×2 (1 failed)".
    pub used: Vec<String>,
    /// "created csv_stats v1", "updated git_log v3".
    pub built: Vec<String>,
    /// "modified src/main.rs".
    pub files: Vec<String>,
    /// Messages the user steered with during the Run.
    pub steered: i64,
}

/// A file a Run's Tools changed (SPEC §5.7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunFileInfo {
    pub path: String,
    /// "created", "modified", "removed" or "unchanged" (changed back),
    /// comparing the snapshot with the file now.
    pub status: String,
    /// The pre-Run content was kept, so `RewindRun` can restore it.
    pub revertible: bool,
    /// For text files: the change as `- `/`+ ` lines with context.
    #[serde(default)]
    pub diff: Option<String>,
}

/// A message waiting to start its own Run after the current one (§5.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueuedMessage {
    pub id: i64,
    pub content: String,
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
    /// Why it was retired automatically (unused, or unhelpful with its
    /// completion rate).
    #[serde(default)]
    pub retire_reason: Option<String>,
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
    /// "disabled", "ready" (running), "idle" (starts on first use),
    /// "failed" or "auth needed".
    #[serde(default)]
    pub state: String,
    /// The server's own tool names left out (`disabled_tools`).
    #[serde(default)]
    pub disabled_tools: Vec<String>,
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
    /// The last 20 Invocations, newest first (SPEC §10.4).
    #[serde(default)]
    pub recent_calls: Vec<ToolCallInfo>,
    /// Why the Tool needs (or may need) attention, in plain words (SPEC
    /// §10.4); empty when nothing stands out.
    #[serde(default)]
    pub attention: Vec<String>,
    /// A real call: the latest successful Invocation (SPEC §10.4).
    #[serde(default)]
    pub example: Option<ToolExample>,
    /// About how many tokens listing this Tool adds to each request, the
    /// price of pinning it (SPEC §7.1).
    #[serde(default)]
    pub listing_tokens: usize,
    /// Tools its current code calls (`tools.call`), sorted (SPEC §10.4).
    #[serde(default)]
    pub uses: Vec<String>,
    /// Modules its current code loads (`requireModule`), sorted.
    #[serde(default)]
    pub modules: Vec<String>,
    /// Active Tools whose current code calls this one, sorted.
    #[serde(default)]
    pub used_by: Vec<String>,
}

/// One concrete call of a Tool: its (redacted) input and output, as
/// recorded, the output cut to a few KB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolExample {
    pub version: i64,
    pub input: String,
    pub output: String,
    pub created_at: String,
}

/// One past Invocation of a Tool, for its detail view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallInfo {
    pub id: String,
    pub version: i64,
    #[serde(default)]
    pub session_id: Option<String>,
    pub status: String,
    pub input: String,
    pub output: String,
    pub error: String,
    pub duration_ms: i64,
    pub created_at: String,
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
    /// Suggest a fresh name from a Session's recent messages (SPEC §10.4):
    /// `{name}`, null with nothing to go on. Doesn't rename; the client
    /// confirms with `RenameSession`.
    SuggestSessionName {
        id: String,
    },
    /// Set (or with null/empty, clear) a Session's standing instructions
    /// (SPEC §5.1); they take effect at the agent's next step.
    SetSessionInstructions {
        id: String,
        instructions: Option<String>,
    },
    /// Save Session `id`'s model, effort, instructions and working
    /// directory as template `name` (replacing one of that name), with an
    /// optional first message. Returns the TemplateInfo.
    SaveSessionTemplate {
        id: String,
        name: String,
        #[serde(default)]
        first_message: Option<String>,
    },
    /// Templates, by name.
    ListSessionTemplates,
    DeleteSessionTemplate {
        name: String,
    },
    /// A new Session set up from a template: `{session, first_message}`.
    /// The first message isn't sent; clients put it in the input.
    CreateSessionFromTemplate {
        template: String,
        #[serde(default)]
        name: Option<String>,
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
    /// Set (or, with `None`, clear) a Session's working directory; it must
    /// exist on the daemon's machine. Returns the Session.
    /// Replace a Session's tags (normalised: trimmed, lowercase, no `#`,
    /// deduplicated, sorted). Returns it.
    SetSessionTags {
        id: String,
        tags: Vec<String>,
    },
    /// Archive or unarchive a Session: archived ones are hidden from the
    /// default list but kept, searchable and restorable. Returns it.
    SetSessionArchived {
        id: String,
        archived: bool,
    },
    /// Pin or unpin a Session (pinned ones list first). Returns it.
    SetSessionPinned {
        id: String,
        pinned: bool,
    },
    SetSessionCwd {
        id: String,
        cwd: Option<String>,
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
    /// The files a Run's Tools changed, with diffs: `run_id` defaults to
    /// the Session's latest Run that changed any.
    RunFiles {
        session_id: String,
        #[serde(default)]
        run_id: Option<String>,
    },
    /// Put files back as they were before a Run changed them (`/rewind`):
    /// one `path`, or all of the Run's revertible files.
    RewindRun {
        session_id: String,
        #[serde(default)]
        run_id: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    /// Change a queued message's text, or remove it (`content: None`).
    EditQueued {
        session_id: String,
        id: i64,
        #[serde(default)]
        content: Option<String>,
    },
    /// Move a queued message one place earlier (`up`) or later.
    MoveQueued {
        session_id: String,
        id: i64,
        up: bool,
    },
    /// Re-run the Session's last user message (`/retry [--model X]`),
    /// on `model` for that Run only when given. With `keep_original`
    /// (default) the current answer is kept in a fork of the Session
    /// named "… · original answer" before it is replaced.
    RetryRun {
        session_id: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default = "default_true")]
        keep_original: bool,
    },
    /// The user's verdict on a Run's answer (👍/👎, SPEC §8.7): `rating`
    /// "good", "bad" or "" to clear; `run_id` defaults to the Session's
    /// latest finished Run.
    RateRun {
        session_id: String,
        #[serde(default)]
        run_id: Option<String>,
        rating: String,
        #[serde(default)]
        comment: Option<String>,
    },
    /// Cap the Session's active Run (or, when idle, its next Run) at
    /// `max_steps` steps; `None` removes the cap. Reaching it pauses the
    /// Run like `limits.run_steps` (resume continues past it). Cleared
    /// when that Run ends.
    SetStepLimit {
        session_id: String,
        max_steps: Option<u32>,
    },
    /// Choose which events this connection receives. `sessions: None` =
    /// every Session; otherwise only those Sessions' transcript and live
    /// events. `deltas: false` drops streamed answer/reasoning tokens.
    /// Global events (Tools, approvals, Session list and Run start/end)
    /// always arrive. A connection starts subscribed to everything.
    /// `client` names this connection to the others on the same Session
    /// ("TUI", "web", "web (relay)"; SPEC §10.4). The reply carries this
    /// connection's id, `{subscribed, conn}`.
    Subscribe {
        #[serde(default)]
        sessions: Option<Vec<String>>,
        #[serde(default = "default_true")]
        deltas: bool,
        #[serde(default)]
        client: Option<String>,
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
    /// Run the same prompt on two or more models, each in a fork of the
    /// Session (`/compare A B [prompt]`, SPEC §5.10): `prompt`, or the
    /// Session's last message re-asked. `ComparisonReady` reports back.
    CompareModels {
        session_id: String,
        models: Vec<String>,
        #[serde(default)]
        prompt: Option<String>,
    },
    /// Step mode (`/step`): pause before each Tool call of this Session's
    /// Runs until the user decides (SPEC §5.9).
    SetStepMode {
        session_id: String,
        on: bool,
    },
    /// Answer a `StepPending`: `action` is "continue", "skip", "edit"
    /// (run it with `arguments`, a JSON object as text) or "all" (run this
    /// and the rest of the Run without pausing).
    StepDecision {
        step_id: String,
        action: String,
        #[serde(default)]
        arguments: Option<String>,
    },
    /// Everything for a new machine as one `formless.all/1` bundle (SPEC
    /// §15): Tools, modules, Skills, memories, jobs, and the config with
    /// credentials blanked; Sessions too when `sessions`.
    ExportAll {
        #[serde(default)]
        sessions: bool,
    },
    /// Load a `formless.all/1` bundle. Tools and Skills import as usual
    /// (renamed on collision; Skills disabled for review), memories and
    /// modules are added, jobs come in paused, and providers, Roles and MCP
    /// servers this machine lacks are added (blanked secrets to fill in).
    ImportAll {
        bundle: serde_json::Value,
    },
    /// Disk use by kind (SPEC §9.6).
    StorageUsage,
    /// What clean-up frees (`dry_run`) or freed.
    Gc {
        #[serde(default)]
        dry_run: bool,
    },
    /// Take a database backup now (rotated like the daily one).
    Backup,
    /// Prompts offered by the configured MCP servers.
    ListMcpPrompts,
    /// The last `lines` (default 50) of a stdio server's stderr log.
    McpServerLog {
        name: String,
        #[serde(default)]
        lines: Option<usize>,
    },
    /// A server's tools with whether each is enabled and its approximate
    /// token cost in the tool list (starts the server).
    ListMcpTools {
        name: String,
    },
    /// Turn one of a server's tools on or off (`disabled_tools`).
    SetMcpToolEnabled {
        name: String,
        tool: String,
        enabled: bool,
    },
    /// The curated catalog of popular MCP servers.
    McpCatalog,
    /// Add a catalog server: `params` fill its placeholders (e.g. `path`);
    /// `name` defaults to the catalog id.
    AddMcpFromCatalog {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        params: serde_json::Map<String, serde_json::Value>,
    },
    /// Servers configured in Claude Desktop, Cursor or VS Code (`cwd` adds
    /// that project's `.cursor` / `.vscode`), to review and add.
    McpImportCandidates {
        #[serde(default)]
        cwd: Option<String>,
    },
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
    /// The user loads an enabled skill into a Session (`/skill:NAME
    /// [args]`): its instructions join the conversation as a note, with
    /// the arguments, and the load is counted (SPEC §7.10).
    LoadSkill {
        session_id: String,
        name: String,
        #[serde(default)]
        args: Option<String>,
    },
    /// Recent Runs' metrics and their summary (SPEC §8.7):
    /// `{"runs": [RunMetricsInfo], "summary": RunSummary}`.
    RunMetrics {
        #[serde(default)]
        limit: Option<usize>,
        /// Only this Session's Runs.
        #[serde(default)]
        session_id: Option<String>,
        /// Only what happened at or after this RFC 3339 time (SPEC §10.4
        /// Metrics range); None = all history.
        #[serde(default)]
        since: Option<String>,
    },
    /// Built-in call stats (SPEC §8.8).
    ListBuiltinStats {
        /// Only what happened at or after this RFC 3339 time (SPEC §10.4
        /// Metrics range); None = all history.
        #[serde(default)]
        since: Option<String>,
    },
    /// Provider token use (and cost, when priced) since `since`, per day
    /// or, with `by` = "model", "role" or "session", per that key over the
    /// range (SPEC §10.4): `[{key, prompt_tokens, completion_tokens,
    /// cached_tokens, cost}]`, days oldest first or keys biggest first.
    UsageByDay {
        #[serde(default)]
        since: Option<String>,
        #[serde(default)]
        by: Option<String>,
    },
    /// Per-Tool calls and success rate in a range:
    /// `[{tool_id, invocations, success_rate}]`.
    ListToolMetrics {
        /// Only what happened at or after this RFC 3339 time (SPEC §10.4
        /// Metrics range); None = all history.
        #[serde(default)]
        since: Option<String>,
    },
    /// Browsers connected through the relay right now (SPEC §12).
    ListRelayClients,
    /// Close one browser's relay connection. It can reconnect with the
    /// pairing link; rotating the secret is what revokes access.
    DisconnectRelayClient {
        cid: String,
    },
    /// Take back a pending attachment before the message is sent: `id` is
    /// the attachment's id (or, for a referenced path, the path). Returns
    /// `{removed}`.
    DetachFile {
        session_id: String,
        id: String,
    },
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
        /// Without `root`: this Session's working directory.
        #[serde(default)]
        session_id: Option<String>,
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
    /// Every grant, one row per Capability: stored, Session and timed
    /// ones (`Vec<GrantInfo>`, SPEC §10.3).
    ListApprovals,
    /// Revoke one grant from the list: a Capability (or, with none, the
    /// whole grant) of a stored, Session or timed grant.
    RevokeGrant {
        tool_id: String,
        #[serde(default)]
        capability: Option<String>,
        scope: ApprovalScope,
        #[serde(default)]
        session_id: Option<String>,
    },
    /// How full a Session's context is (SPEC §10.6): `{model,
    /// context_tokens, used_tokens, measured, breakdown: {system_prompt,
    /// tool_schemas, history, attachments}}`. `used_tokens` is the last
    /// prompt's count when known (`measured`), else an estimate; the
    /// breakdown is estimated (about 4 characters per token).
    ContextUsage {
        session_id: String,
    },
    /// Compact a Session now (SPEC §11): summarise everything so far into
    /// its checkpoint, keeping what `focus` asks for. Runs in the
    /// background and ends with `CompactionNotice` (or a `Note` on
    /// failure); refused while a Run is active.
    CompactSession {
        session_id: String,
        #[serde(default)]
        focus: Option<String>,
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
    /// Add a job as the user (approved): `cron` for recurring (a cron
    /// expression or plain English, "every weekday at 9am"), else `at`.
    AddJob {
        prompt: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        cron: Option<String>,
        #[serde(default)]
        at: Option<String>,
        #[serde(default)]
        overlap: Option<String>,
        #[serde(default)]
        deliver: Option<serde_json::Value>,
    },
    /// A schedule (cron or plain English) as cron, with its next runs:
    /// `{cron, next: [RFC 3339; 3]}`.
    PreviewSchedule {
        schedule: String,
    },
    /// Fire a job now, outside its schedule (its next run is unchanged).
    RunJobNow {
        id: String,
    },
    /// A job's latest firings (`Vec<JobRunInfo>`), newest first.
    JobRuns {
        id: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Change a job's overlap policy or delivery (`deliver: null` clears).
    SetJobOptions {
        id: String,
        #[serde(default)]
        overlap: Option<String>,
        #[serde(default)]
        deliver: Option<serde_json::Value>,
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
    /// The memories a Run's prompt carried (`Vec<MemoryInfo>`): that Run,
    /// or the Session's latest one.
    RunMemories {
        #[serde(default)]
        run_id: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    },
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
        /// Values for the prompt's arguments in their declared order
        /// (`/mcp:server:prompt 123 main`); named `arguments` win.
        #[serde(default)]
        positional: Vec<String>,
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
    /// A module by id or name, with versions and the Tools using it.
    GetModule {
        id: String,
    },
    /// Save new module code as a new version (author `user`), through the
    /// same syntax and load check as `create_module`.
    UpdateModuleCode {
        id: String,
        code: String,
    },
    /// Make an earlier version's code current again, as a new version.
    RollbackModule {
        id: String,
        version: i64,
    },
    /// Delete a module and all its versions (Tools that load it will fail).
    DeleteModule {
        id: String,
    },
    GetTool {
        id: String,
    },
    /// Run a Tool by hand with `input` (SPEC §10.4), through the usual
    /// approval path, as part of `session_id` (approvals and the
    /// Invocation are attributed to it). Returns `{result}`: the text the
    /// agent would have got.
    RunTool {
        id: String,
        input: serde_json::Value,
        session_id: String,
    },
    /// Run a Tool by hand without real side effects (SPEC §10.4): reads
    /// are real, writes go to a throwaway overlay, processes and non-GET
    /// requests aren't made. Same approval as a real run; not recorded as
    /// an Invocation. `{ok, result?, error?, effects, logs}`, where
    /// `effects` are the would-be effects ("write 12 bytes to /tmp/x").
    DryRunTool {
        id: String,
        input: serde_json::Value,
        session_id: String,
    },
    /// Run Tool JS through the same syntax gate as saving, without saving
    /// (the web editor's inline errors): `{ok, error?, line?}`.
    CheckToolCode {
        code: String,
    },
    /// Review an edit before saving it (the TUI editor's Ctrl+D): the diff
    /// against the current version and what the save gate would say,
    /// `{diff, ok, stage?, error?, line?, smoke_note?}`. Nothing is saved.
    PreviewToolCode {
        id: String,
        code: String,
    },
    /// Save a new version with a new description and/or schemas, the
    /// rest as the current version, through the usual gates with author
    /// `user` (SPEC §10.4).
    UpdateToolSpec {
        id: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        input_schema: Option<serde_json::Value>,
        #[serde(default)]
        output_schema: Option<serde_json::Value>,
    },
    /// Save edited Tool JS as a new version (in-TUI editor, v2).
    UpdateToolCode {
        id: String,
        code: String,
    },
    RetireTool {
        id: String,
    },
    /// Revert the newest registry change (`RegistryChange`): a new Tool is
    /// retired, an update or rollback rolled back, a retire or restore
    /// reversed. `{undone, tool}`; an error when there is nothing to undo.
    UndoRegistryChange,
    /// Switch a Tool off (or back on) without retiring it.
    SetToolDisabled {
        id: String,
        disabled: bool,
    },
    /// Pin a Tool so every Session lists it to the model, or unpin it.
    SetToolPinned {
        id: String,
        pinned: bool,
    },
    /// Replace a Tool's tags (normalised; an empty list clears them).
    SetToolTags {
        id: String,
        tags: Vec<String>,
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
        /// A wider path to grant instead of a path Capability asked for:
        /// requested `fs:read:`/`fs:write:` spec → a folder above its path
        /// (one of the request's `path_options`). Older clients omit it
        /// and grant exactly what was asked.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        paths: std::collections::BTreeMap<String, String>,
        /// With `approved = false`: always deny the call's new sensitive
        /// Capabilities, for every Tool, until forgotten (SPEC §10.3).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        remember_denial: bool,
    },
    /// Capabilities the user always denies (`Vec<DenialInfo>`).
    ListDenials,
    /// What the daemon can reach, in one report (SPEC §10.3): grants by
    /// Capability family, denials, trusted folders, MCP servers, trusted
    /// hooks, relay and web UI. Formatted by [`security_report`].
    SecurityStatus,
    /// Forget a remembered denial (`capability`), or all of them.
    ForgetDenial {
        #[serde(default)]
        capability: Option<String>,
    },
    PromotionDecision {
        promotion_id: String,
        approved: bool,
    },
    RunOptimiser {
        tool_id: Option<String>,
    },
    ListProposals,
    ListModelStats {
        /// Only what happened at or after this RFC 3339 time (SPEC §10.4
        /// Metrics range); None = all history.
        #[serde(default)]
        since: Option<String>,
    },
    GenerateProposal,
    ProposalDecision {
        id: String,
        approved: bool,
    },
    /// What a Proposal changes (SPEC §10.4): `{target, diff: [[op,
    /// line]], applied}` with op ' ', '-' or '+'. `target` is "learned
    /// guidance" (the system prompt's learned part) or "memory".
    ProposalDiff {
        id: String,
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
    /// Fetch a Tool bundle from an https URL or gist to preview it before
    /// importing: `{source, file?, bundle}`. Nothing is imported.
    FetchToolBundle {
        url: String,
    },
    /// `format`: "json" (default; the re-importable bundle), or
    /// "markdown" / "html", which return `{name, format, content}`
    /// (SPEC §5.1).
    ExportSession {
        id: String,
        #[serde(default)]
        format: Option<String>,
    },
    ImportSession {
        bundle: serde_json::Value,
    },
}

fn default_true() -> bool {
    true
}

/// How long an approval lasts (SPEC §10.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// This call only; nothing is stored.
    Once,
    /// Every call of the Tool in this Session, until the Session is
    /// deleted (or the daemon restarts); never stored globally.
    Session,
    /// Every call of the Tool, in any Session, for the next hour; then it
    /// asks again. Kept in the daemon's memory only.
    Hour,
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
// Events dominate the traffic, so boxing them would only add allocations.
#[allow(clippy::large_enum_variant)]
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
    /// The agent replaced the Session's plan (`update_plan`); empty
    /// `items` clears it.
    PlanUpdated {
        session_id: String,
        items: Vec<PlanItem>,
    },
    /// At the end of a long Run (4+ steps): what it did, for a
    /// collapsible "What I did" block under the answer.
    RunRecap {
        session_id: String,
        run_id: String,
        recap: RunRecap,
    },
    /// At the end of a Run whose Tools changed files: what changed (no
    /// diffs; `RunFiles` has them).
    RunFilesChanged {
        session_id: String,
        run_id: String,
        files: Vec<RunFileInfo>,
    },
    /// Every Run of a `CompareModels` finished: the answers side by side.
    ComparisonReady {
        session_id: String,
        comparison_id: String,
        results: Vec<ComparisonResult>,
    },
    /// Step mode: the Run is about to call `tool_name` with `arguments`
    /// (call `index` of `count` in this step) and waits for StepDecision.
    StepPending {
        session_id: String,
        run_id: String,
        step_id: String,
        tool_name: String,
        arguments: String,
        index: usize,
        count: usize,
    },
    /// The step was decided (by any client) or the Run stopped waiting.
    StepResolved {
        session_id: String,
        step_id: String,
    },
    /// A Session's queue of messages waiting for the current Run changed
    /// (queued, sent, edited, removed or reordered).
    QueueChanged {
        session_id: String,
        queued: Vec<QueuedMessage>,
    },
    /// A provider request failed with a retryable error (429, 5xx, a
    /// dropped connection) and is re-sent after `wait_secs`; `attempt` of
    /// `max_attempts` retries. Esc (Interrupt) gives up.
    ProviderRetrying {
        session_id: String,
        run_id: String,
        reason: String,
        wait_secs: u64,
        attempt: u32,
        max_attempts: u32,
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
    /// A running Tool reported `progress(fraction, label)`: `fraction`
    /// 0..1 or None when unknown; `depth` as in ToolCallStarted.
    ToolProgress {
        session_id: String,
        run_id: String,
        tool_name: String,
        #[serde(default)]
        depth: usize,
        fraction: Option<f64>,
        label: String,
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
    /// The clients showing a Session (subscribed to it), sent to them
    /// whenever that changes (SPEC §10.4).
    SessionClients {
        session_id: String,
        clients: Vec<AttachedClient>,
    },
    /// An approval was answered, by the client named `by`; the others
    /// close their prompt ("answered in web").
    ApprovalAnswered {
        approval_id: String,
        approved: bool,
        #[serde(default)]
        by: Option<String>,
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
        /// The Tool's id, to fetch its full code (`GetTool`).
        #[serde(default)]
        tool_id: Option<String>,
        /// When the model's parallel batch holds several calls: where this
        /// one stands and all of them, so clients list each (SPEC §5.2).
        #[serde(default)]
        batch: Option<ApprovalBatch>,
        /// Wider scopes the user may grant for each new path Capability,
        /// narrowest first (SPEC §10.3).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        path_options: Vec<PathOption>,
    },
    /// A plain-language, AI-written summary of what the Tool behind an
    /// open approval does (SPEC §10.3), sent after `ApprovalRequested`.
    ApprovalSummary {
        approval_id: String,
        tool_id: String,
        version: i64,
        summary: String,
    },
    ToolUpdated {
        tool: ToolInfo,
    },
    ModuleUpdated {
        module: ModuleInfo,
    },
    ModuleDeleted {
        id: String,
    },
    ProposalUpdated {
        proposal: PromptProposalInfo,
    },
    ToolDeleted {
        id: String,
    },
    /// A create, update, retire, restore or rollback in the registry; the
    /// newest is what `UndoRegistryChange` reverts.
    RegistryChanged {
        change: RegistryChange,
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
    /// The Tools a Run created, updated or retired, sent just before its
    /// `RunFinished` when there were any (SPEC §10.6).
    RunToolChanges {
        session_id: String,
        run_id: String,
        changes: Vec<ToolChange>,
    },
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
            | Event::ProviderRetrying { session_id, .. }
            | Event::QueueChanged { session_id, .. }
            | Event::RunFilesChanged { session_id, .. }
            | Event::RunRecap { session_id, .. }
            | Event::ToolProgress { session_id, .. }
            | Event::PlanUpdated { session_id, .. }
            | Event::StepPending { session_id, .. }
            | Event::ComparisonReady { session_id, .. }
            | Event::StepResolved { session_id, .. }
            | Event::RunFinished { session_id, .. }
            | Event::RunPaused { session_id, .. }
            | Event::CompactionNotice { session_id, .. }
            | Event::MessagesEdited { session_id, .. }
            | Event::ToolCallStarted { session_id, .. }
            | Event::ToolCallFinished { session_id, .. }
            | Event::ApprovalRequested { session_id, .. }
            | Event::UserQuestion { session_id, .. }
            | Event::RunUsage { session_id, .. }
            | Event::RunToolChanges { session_id, .. }
            | Event::InvocationPending { session_id, .. }
            | Event::InvocationCompleted { session_id, .. }
            | Event::InvocationStalled { session_id, .. }
            | Event::InvocationActivity { session_id, .. }
            | Event::SubagentStarted { session_id, .. }
            | Event::SubagentStep { session_id, .. }
            | Event::SubagentFinished { session_id, .. }
            | Event::SessionClients { session_id, .. } => Some(session_id),
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
            | Event::ModuleDeleted { .. }
            | Event::ProposalUpdated { .. }
            | Event::ToolDeleted { .. }
            | Event::RegistryChanged { .. }
            | Event::ApprovalSummary { .. }
            | Event::OptimiserActivity { .. }
            | Event::PromotionApprovalRequested { .. }
            | Event::ApprovalAnswered { .. } => None,
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
            path_options: Default::default(),
            approval_id: "a".into(),
            session_id: "s1".into(),
            tool_name: "t".into(),
            version: 1,
            capabilities: vec![],
            memory_mb: 0,
            detail: None,
            already_allowed: vec![],
            code_preview: None,
            tool_id: None,
            batch: None,
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

    #[test]
    fn broad_capabilities_are_recognised() {
        for broad in [
            "net",
            "process",
            "fs:write:/",
            "fs:write:/Users/ada",
            "fs:write:/home/ada/",
            "fs:write:~",
            "env:GITHUB_TOKEN",
            "env:OPENAI_API_KEY",
            "env:DB_PASSWORD",
        ] {
            assert!(broad_capability(broad).is_some(), "{broad}");
        }
        for scoped in [
            "net:example.com",
            "process:git",
            "fs:write:/Users/ada/project",
            "fs:write:/tmp/out",
            "fs:read:/",
            "env:HOME",
            "env:LANG",
        ] {
            assert!(broad_capability(scoped).is_none(), "{scoped}");
        }
    }

    #[test]
    fn paths_are_placed_against_the_working_folder() {
        let cwd = Some("/Users/ada/project");
        assert_eq!(
            path_note("fs:write:/Users/ada/project/out.txt", cwd),
            Some((false, "inside the working folder".into()))
        );
        assert_eq!(
            path_note("fs:read:/Users/ada/projects-old/x", cwd),
            Some((true, "outside the working folder /Users/ada/project".into())),
            "a sibling with the same prefix is outside"
        );
        assert_eq!(
            path_note("fs:read:/Users/ada/.ssh/id_ed25519", cwd),
            Some((true, "holds SSH keys".into()))
        );
        assert_eq!(
            path_note("fs:write:/etc/hosts", None),
            Some((true, "system configuration".into()))
        );
        assert_eq!(
            path_note("fs:read:/tmp/x", None),
            None,
            "no working folder, nothing special"
        );
        assert_eq!(path_note("net:example.com", cwd), None);
    }
}
