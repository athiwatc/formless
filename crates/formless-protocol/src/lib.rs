//! Shared daemon <-> client protocol (SPEC §9.3). NDJSON framing: one JSON
//! value per line.
//!
//! Compatibility rules:
//! - Additive changes are minor and keep [`PROTOCOL_VERSION`]: a new
//!   optional field (with `#[serde(default)]`), a new `Request` or `Event`
//!   variant. Peers ignore fields they don't know; a daemon answers a
//!   request type it doesn't know with an `unsupported` error (see
//!   [`unsupported`]); clients skip events they can't parse. Name the new
//!   capability in [`FEATURES`] so peers can check for it.
//! - Removing or renaming a field or variant, or changing a field's type
//!   or meaning, is major: bump [`PROTOCOL_VERSION`], and raise
//!   [`MIN_PROTOCOL_VERSION`] only when the old shape is no longer served.

pub mod net_scope;
use serde::{Deserialize, Serialize};

/// The newest protocol this build speaks.
pub const PROTOCOL_VERSION: u32 = 1;
/// The oldest protocol this build still accepts.
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// A reviewed Send requires idle state and a reviewed Steer requires an active Run.
pub const REVIEWED_SEND_MODE_FEATURE: &str = "reviewed_send_mode_v1";
/// A Steer review identifies one active Run and dispatch checks that identity.
pub const REVIEWED_STEER_RUN_FEATURE: &str = "reviewed_steer_run_v1";
/// Disable Step mode only if the Session is still idle at the daemon.
pub const IDLE_STEP_OFF_FEATURE: &str = "idle_step_off_v1";

/// Run IDs displayed and sent by the exact Steer review use a bounded ASCII form.
pub fn valid_reviewed_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Named optional capabilities, advertised by `Ping` and `Status`.
pub const FEATURES: &[&str] = &[
    "setting_env_override",
    "setting_map_merge",
    "subscribe_filter",
    "ask_user",
    "approval_batches",
    "approval_context",
    "explicit_net_subdomains",
    "unsupported_error",
    "dropped_event_kinds",
    "future_transcript_attempts_v1",
    "pending_attachment_inventory_v1",
    "reviewed_attachment_send_v1",
    REVIEWED_SEND_MODE_FEATURE,
    REVIEWED_STEER_RUN_FEATURE,
    IDLE_STEP_OFF_FEATURE,
    "reviewed_reference_detach_v1",
    "session_model_compare_set_v1",
];

/// Explicit nullable pair; both keys must be present even when inheriting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionModelSelection {
    #[serde(deserialize_with = "required_selection_value")]
    pub model: Option<String>,
    #[serde(deserialize_with = "required_selection_value")]
    pub reasoning_effort: Option<String>,
}
fn required_selection_value<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
pub const SESSION_MODEL_MAX_BYTES: usize = 512;
pub const SESSION_EFFORT_MAX_BYTES: usize = 64;

/// Complete pending-attachment metadata inventory limits. No file bodies are read.
pub const PENDING_ATTACHMENT_MAX_ENTRIES: usize = 64;
pub const PENDING_ATTACHMENT_METADATA_BYTES: usize = 64 * 1024;

/// A daemon-lifetime producer boundary, sampled AFTER a canonical snapshot.
/// This admits future attempt starts only; it cannot replay an active prefix
/// or classify canonical commits that race with delayed event publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptBarrier {
    pub epoch: String,
    pub cursor: u64,
}

/// Explicit correlation for ephemeral answer text. Canonical messages remain
/// authoritative and must be obtained through a new owned snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptAttemptEvent {
    pub epoch: String,
    pub cursor: u64,
    pub session_id: String,
    pub run_id: String,
    pub turn_id: String,
    pub attempt_id: String,
    #[serde(flatten)]
    pub change: TranscriptAttemptChange,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum TranscriptAttemptChange {
    Started {
        base_seq: i64,
    },
    Delta {
        offset: u64,
        text: String,
    },
    /// Always invalidates the provisional view. Returned means only that the
    /// provider future ended, not that its candidate was accepted/persisted.
    Closed {
        bytes: u64,
        reason: TranscriptAttemptClose,
    },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptAttemptClose {
    Returned,
    Restarted,
    Dropped,
}

/// The highest version both peers speak, if any.
pub fn negotiate(their_min: u32, their_max: u32) -> Option<u32> {
    let v = their_max.min(PROTOCOL_VERSION);
    (v >= their_min.max(MIN_PROTOCOL_VERSION)).then_some(v)
}

/// The envelope id of a request line that failed to parse, when it is
/// readable at all, so the error still reaches the right caller.
pub fn lenient_id(line: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v["id"].as_u64())
        .unwrap_or(0)
}

/// The `type` of a request line this build doesn't know, if that is why
/// it failed to parse (the envelope itself is well formed).
pub fn unknown_request_type(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = v["request"]["type"].as_str()?;
    let known = serde_json::from_value::<Request>(serde_json::json!({ "type": kind }));
    match known {
        Err(e) if e.to_string().contains("unknown variant") => Some(kind.to_string()),
        _ => None,
    }
}

/// The reply to a request type this daemon doesn't serve: an error with
/// a machine-readable `{"code": "unsupported", "type": …}` result.
pub fn unsupported(id: u64, kind: &str) -> ServerMessage {
    ServerMessage::Response {
        id,
        ok: false,
        result: Some(serde_json::json!({"code": codes::UNSUPPORTED, "type": kind})),
        error: Some(format!(
            "unsupported: this daemon does not know the request type '{kind}'"
        )),
    }
}

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
    /// Direct parent of a recorded fork; absent for roots/unknown old lineage.
    #[serde(default)]
    pub forked_from: Option<String>,
    /// Last source message copied at fork creation, or 0 for an empty prefix.
    #[serde(default)]
    pub fork_point_seq: Option<i64>,
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

/// Stored transcript comparison. This never creates a Run or calls a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionComparison {
    pub left: SessionComparisonSide,
    pub right: SessionComparisonSide,
    pub common_ancestor: Option<String>,
    pub fork_point_seq: Option<i64>,
    pub diff_from_seq: i64,
    pub prefix_changed: bool,
    pub messages: Vec<SessionMessageDiff>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionComparisonSide {
    pub session: SessionInfo,
    pub usage: SessionBranchUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessageDiff {
    /// `unchanged`, `left_only` or `right_only`, at whole-message granularity.
    pub kind: String,
    pub left: Option<MessageInfo>,
    pub right: Option<MessageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionBranchUsage {
    pub scope: String,
    pub recorded_prompt_tokens: i64,
    pub recorded_completion_tokens: i64,
    pub recorded_cached_tokens: i64,
    pub recorded_cache_write_tokens: i64,
    pub requests: u64,
    /// Entire recorded requests from legacy/straddling/unanchored Runs. Their
    /// contents cannot be divided at a transcript boundary without guessing.
    pub unallocated_requests: u64,
    pub unallocated_prompt_tokens: i64,
    pub unallocated_completion_tokens: i64,
    pub unknown_runs: u64,
    pub usage_complete: bool,
    /// Total only when allocation/coverage and every request price are known.
    pub cost: Option<f64>,
    /// Priced allocated rows only; never represented as the complete bill.
    pub priced_cost_subtotal: f64,
    pub unallocated_priced_cost_subtotal: f64,
    /// Possible allocation range over priced OBSERVED rows only. Missing
    /// provider usage and unpriced rows are never bounded by this range.
    pub known_records_cost_range: Option<[f64; 2]>,
    pub estimated_requests: u64,
    pub unpriced: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchSummaryProjection {
    pub total_messages: u64,
    pub included_messages: u64,
    pub omitted_messages: u64,
    /// Source content bytes considered, before secret redaction.
    pub total_bytes: u64,
    pub included_bytes: u64,
    pub omitted_bytes: u64,
    pub truncated_messages: u64,
    /// Policy: transcript metadata/attachment authorities are never projected.
    pub metadata_omitted: bool,
    pub attachment_bodies_omitted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BranchSummaryBasis {
    pub source_id: String,
    pub target_id: String,
    /// Bounded display labels; IDs and revision hashes remain authoritative.
    pub source_name: String,
    pub target_name: String,
    pub source_revision: String,
    pub target_revision: String,
    pub provider: String,
    /// Effective Session selector; role/auto is resolved only on generation.
    pub requested_model: String,
    pub projection: BranchSummaryProjection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BranchSummaryUsage {
    pub requests: u64,
    pub reported_requests: u64,
    pub reported_prompt_tokens: u64,
    pub reported_completion_tokens: u64,
    pub reported_cached_tokens: u64,
    pub reported_cache_write_tokens: u64,
    pub usage_complete: bool,
    pub cost: Option<f64>,
    pub priced_cost_subtotal: f64,
    pub estimated_requests: u64,
    pub unpriced: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BranchSummaryPreview {
    pub preview_id: String,
    pub source_id: String,
    pub target_id: String,
    pub source_revision: String,
    pub target_revision: String,
    /// Exact server-owned note content reviewed before a separate confirmation.
    pub candidate: String,
    pub candidate_hash: String,
    pub provider: String,
    /// Concrete selected model, after ordinary role/auto routing.
    pub requested_model: String,
    pub served_model: String,
    pub usage: BranchSummaryUsage,
    pub projection: BranchSummaryProjection,
    pub expires_at: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BranchSummaryApplied {
    pub preview_id: String,
    pub source_id: String,
    pub target_id: String,
    pub message: MessageInfo,
    pub already_applied: bool,
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
    for warning in list(&v["network_scope_warnings"]) {
        out.push(format!("  ⚠ {warning}"));
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
    /// Start time of the latest retained Invocation, regardless of outcome.
    /// Missing when the daemon has no retained history for this Tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
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

/// Split a process grant into its executable and optional first-argument scope.
pub fn split_process_grant(grant: &str) -> (&str, Option<&str>) {
    match grant.rsplit_once(':') {
        // `C:\…` is a Windows path, not a command with sub-commands.
        Some((cmd, scope))
            if cmd.len() > 1 && !scope.is_empty() && !scope.contains(['/', '\\']) =>
        {
            (cmd, Some(scope))
        }
        _ => (grant, None),
    }
}

fn windows_drive_path(path: &str) -> bool {
    matches!(path.as_bytes(), [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
}

fn approval_path(path: &str) -> (std::borrow::Cow<'_, str>, bool) {
    let plain = path
        .strip_prefix(r"\\?\")
        .filter(|plain| windows_drive_path(plain))
        .unwrap_or(path);
    if windows_drive_path(plain) || path.starts_with(r"\\") {
        (plain.replace('\\', "/").to_ascii_lowercase().into(), true)
    } else {
        (path.into(), false)
    }
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
        let (path, _) = approval_path(path);
        let p = path.trim_end_matches('/');
        let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        let home = matches!(
            parts.as_slice(),
            [] | ["Users" | "home"] | ["Users" | "home", _] | ["root"]
        );
        let windows_home = windows_drive_path(&path)
            && matches!(parts.as_slice(), [_] | [_, "users"] | [_, "users", _]);
        if p.is_empty() || p == "~" || home || windows_home {
            return Some("writing anywhere in your files");
        }
    }
    // Programs that are broader than they look (#608): interpreters and
    // shells run anything; downloaders and remote shells reach the network
    // and write files. Argument scoping (`process:git:status`) narrows the
    // others, not these.
    if let Some(cmd) = spec.strip_prefix("process:") {
        let (name, _) = split_process_grant(cmd);
        let base = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(name)
            .to_ascii_lowercase();
        let base = base.trim_end_matches(".exe");
        // Versioned names (`python3.11`, `ruby3.2`, `node20`) count too.
        let stem = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        if matches!(
            base,
            "sh" | "bash"
                | "zsh"
                | "fish"
                | "dash"
                | "ksh"
                | "csh"
                | "tcsh"
                | "mksh"
                | "ash"
                | "cmd"
                | "powershell"
                | "pwsh"
                | "osascript"
                | "env"
                | "xargs"
                | "sudo"
                | "doas"
                | "pkexec"
                | "su"
                | "runuser"
        ) || base == "nodejs"
            || matches!(
                stem,
                "python"
                    | "pythonw"
                    | "py"
                    | "pyw"
                    | "node"
                    | "deno"
                    | "dart"
                    | "bun"
                    | "perl"
                    | "ruby"
                    | "php"
                    | "lua"
                    | "tclsh"
            )
        {
            return Some("an interpreter or shell: it can run any program, like bare process");
        }
        if matches!(
            base,
            "curl" | "wget" | "ssh" | "scp" | "rsync" | "nc" | "ncat"
        ) {
            return Some("it can reach any host and write files, like net plus fs:write");
        }
        // Run other programs through options or files they read (#1099).
        match base {
            _ if stem == "dotnet" => {
                return Some("dotnet can build and execute repository code")
            }
            "git" => {
                return Some(
                    "git runs hooks and settings from the repository it works in, even for read-only sub-commands",
                )
            }
            _ if stem == "go" => {
                return Some("the Go tool can build and execute repository code")
            }
            "find" => return Some("find can run any program (-exec)"),
            _ if stem == "pip" => {
                return Some("pip can execute code from package build backends")
            }
            "tar" => return Some("tar can run any program (--to-command) and write anywhere"),
            "bundle" | "bundler" => return Some("Bundler can execute code in the Gemfile"),
            _ if stem == "gem" => return Some("RubyGems can execute package build code"),
            "gradle" | "gradlew" | "mvn" | "mvnw" => {
                return Some("Gradle and Maven can execute repository build code")
            }
            "make" | "cmake" | "ninja" => {
                return Some("a build tool runs whatever the build files say")
            }
            _ => {}
        }
    }
    if let Some(key) = spec.strip_prefix("env:") {
        let k = key.to_uppercase();
        let schema_role = matches!(
            k.as_str(),
            "PRIMARY_KEY"
                | "FOREIGN_KEY"
                | "UNIQUE_KEY"
                | "CANDIDATE_KEY"
                | "SORT_KEY"
                | "PARTITION_KEY"
        );
        // Preserve warnings for compact credential spellings as well.
        let credential_key = !schema_role && k.split('_').any(|part| part == "KEY")
            || [
                "APIKEY",
                "ACCESSKEY",
                "ENCRYPTIONKEY",
                "SIGNINGKEY",
                "HMACKEY",
                "CLIENTKEY",
                "ACCOUNTKEY",
                "MASTERKEY",
                "SESSIONKEY",
                "SHAREDKEY",
            ]
            .iter()
            .any(|ending| k.ends_with(ending));
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
            || credential_key
            || k.ends_with("_PAT");
        if secret {
            return Some("a secret from your environment");
        }
    }
    None
}

/// Display-only warning for an exact credential-read approval. It does not
/// interpret, resolve or widen the server's approved path identity.
pub fn sensitive_read_warning(spec: &str) -> Option<&'static str> {
    spec.strip_prefix("fs:read-sensitive:").map(|_| {
        "Credential read: exact approved path identity only, not descendants. Contents may flow to the model provider."
    })
}

/// The credential file in a conventional Composer configuration directory.
pub fn is_composer_auth_path(path: &std::path::Path) -> bool {
    let mut parts = path.iter().rev();
    let matches = |part: Option<&std::ffi::OsStr>, expected: &str| {
        part.is_some_and(|part| {
            let name = part.to_string_lossy();
            #[cfg(windows)]
            let name = name.split(':').next().unwrap_or_default();
            name.eq_ignore_ascii_case(expected)
        })
    };
    if !matches(parts.next(), "auth.json") {
        return false;
    }
    let parent = parts.next();
    matches(parent, ".composer") || matches(parent, "composer") && matches(parts.next(), ".config")
}

/// Where an `fs:` Capability's path lies (SPEC §10.3), for approval
/// prompts: `(warn, note)`. Credential reads always warn, including inside
/// the working folder. Other secret folders and paths outside it warn too.
pub fn path_note(spec: &str, cwd: Option<&str>) -> Option<(bool, String)> {
    if let Some(warning) = sensitive_read_warning(spec) {
        return Some((true, warning.into()));
    }
    let path = spec
        .strip_prefix("fs:read:")
        .or_else(|| spec.strip_prefix("fs:write:"))?;
    if is_composer_auth_path(std::path::Path::new(path)) {
        return Some((true, "holds Composer credentials".into()));
    }
    let (path, windows) = approval_path(path);
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
        if segments.contains(&dir)
            || (windows && segments.iter().any(|part| part.eq_ignore_ascii_case(dir)))
        {
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
    let cwd = cwd.filter(|c| !c.is_empty())?;
    let (normalized_cwd, _) = approval_path(cwd);
    let compared_cwd = normalized_cwd.trim_end_matches('/');
    let inside = path == compared_cwd || path.starts_with(&format!("{compared_cwd}/"));
    Some(if inside {
        (false, "inside the working folder".into())
    } else {
        (
            true,
            format!("outside the working folder {}", cwd.trim_end_matches('/')),
        )
    })
}

/// One granted Capability, for the central approvals list (SPEC §10.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GrantInfo {
    /// Offline PSL/wildcard warning supplied by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_warning: Option<String>,
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
    /// Identity of this operation. Older daemons and stored records omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
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
    #[serde(default)]
    pub description_review: DescriptionMetadata,
    #[serde(default)]
    pub description_reviews: Vec<DescriptionReview>,
    pub module: ModuleInfo,
    /// Oldest first.
    pub versions: Vec<ModuleVersionInfo>,
    /// Active Tools whose current code loads it, sorted.
    pub used_by: Vec<String>,
}

/// One audit-log entry (SPEC §14).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditEntry {
    /// Opaque ListAudit boundary; absent when reading older daemon responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
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
    #[serde(default)]
    pub invocation_id: Option<String>,
    #[serde(default)]
    pub last_fraction: Option<f64>,
    #[serde(default)]
    pub last_label: Option<String>,
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
    /// Part of `prompt_tokens` written to the prompt cache (#804).
    #[serde(default)]
    pub cache_write_tokens: i64,
    /// Estimated USD, when every model used has a configured price.
    pub cost: Option<f64>,
    /// Models used that have no price, so `cost` (and dollar limits) can't
    /// count them (#809).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unpriced: Vec<String>,
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

/// A local-only preview attachment. No path is accepted from remote clients.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ForecastAttachment {
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    #[serde(default)]
    pub sha256: String,
    /// Display-only reference metadata; never a daemon-local read capability.
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub directory: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
/// Immutable per-Run pipeline policy; omitted fields preserve ordinary Runs.
/// These are never Session settings or persistent standing instructions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RunOptions {
    /// Raw per-Run selector, resolved once by forecast/admission, never by a
    /// Session model-selection request (which could start a paid probe).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    /// Explicit zero forbids model dispatch; None retains the profile Run cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
}
impl RunOptions {
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.reasoning_effort.is_none()
            && self.system.is_none()
            && self.schema.is_none()
            && self.max_usd.is_none()
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ForecastInput {
    #[serde(default, skip_serializing_if = "RunOptions::is_empty")]
    pub options: RunOptions,
    pub session_id: Option<String>,
    pub draft: String,
    #[serde(default)]
    pub edit_message_id: Option<String>,
    #[serde(default)]
    pub resume: bool,
    #[serde(default)]
    pub retry: bool,
    pub model: Option<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ForecastAttachment>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastRange {
    pub p50: f64,
    pub p90: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastBasis {
    pub provider_key: String,
    pub model: String,
    pub role: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub session_id: Option<String>,
    pub cwd_hash: String,
    pub tools_hash: String,
    pub prompt_tokens: usize,
}
/// The measured, non-text observations used for one immutable quote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastComparable {
    pub run_id: String,
    pub prompt_tokens: usize,
    pub steps: i64,
    pub tokens: i64,
    pub duration_ms: i64,
    pub cost_usd: Option<f64>,
    pub cached_fraction: Option<f64>,
    pub peak_prompt_tokens: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunForecast {
    pub fingerprint: String,
    pub basis: ForecastBasis,
    pub comparable_runs: usize,
    #[serde(default)]
    pub comparables: Vec<ForecastComparable>,
    pub steps: Option<ForecastRange>,
    pub tokens: Option<ForecastRange>,
    pub cost_usd: Option<ForecastRange>,
    pub duration_ms: Option<ForecastRange>,
    pub context_tokens: u32,
    pub context_source: String,
    pub will_compact: bool,
    pub compaction_probability: Option<f64>,
    pub cached_fraction: Option<ForecastRange>,
    pub unpriced: bool,
    pub warnings: Vec<String>,
    pub confirmation_required: bool,
    #[serde(default)]
    pub confirmation_id: Option<String>,
    pub hint: String,
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

/// Ownership and lifecycle of the legacy plan item list. Missing fields are
/// unknown; foreign/copied Run IDs never grant local write authority.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanState {
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub run_status: Option<String>,
    #[serde(default)]
    pub previous_task: bool,
    /// "live", "resumed", "copied", "imported", or "legacy"; empty is unknown.
    #[serde(default)]
    pub origin: String,
}

/// The immutable terminal recap recorded for this exact owned Run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordedRunRecap {
    pub run_id: String,
    pub recap: RunRecap,
}

/// "What I did": a long Run's recap from its trace, no model call
/// (SPEC §5.7).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RunRecap {
    /// Empty means the terminal status was not recorded by a legacy producer.
    #[serde(default)]
    pub status: String,
    /// All actual calls, including the unfinished subset. None is legacy unknown.
    #[serde(default)]
    pub total_calls: Option<i64>,
    /// The subset still running at this snapshot; excluded from terminal outcomes.
    #[serde(default)]
    pub unfinished_calls: i64,
    /// Programs and MCP tools may have changed paths outside file tracking.
    #[serde(default)]
    pub untracked_writes_possible: bool,
    pub steps: i64,
    pub duration_ms: i64,
    /// Terminal calls only, including nested calls: "read_file ×3",
    /// "fetch_page ×2 (1 failed)". Unfinished calls are excluded.
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
    /// File-system kind; empty means unknown for legacy records.
    #[serde(default)]
    pub kind: String,
    /// Why the recorded path cannot be restored.
    #[serde(default)]
    pub revert_reason: Option<String>,
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

/// Exact text fragments, including punctuation and whitespace. Display clients
/// must escape terminal controls; these are data, never rendering instructions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewTextSpan {
    pub text: String,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewLine {
    /// "context", "removed", or "added".
    pub kind: String,
    pub old_line: Option<u64>,
    pub new_line: Option<u64>,
    pub text: Vec<ReviewTextSpan>,
    /// Original line terminator: "\n", "\r\n", or empty at EOF.
    pub ending: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewHunk {
    pub id: String,
    pub old_start: u64,
    pub old_lines: u64,
    pub new_start: u64,
    pub new_lines: u64,
    /// "pending", "accepted", or "rejected". Accepting does not write a file.
    pub decision: String,
    pub lines: Vec<ReviewLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewFile {
    pub id: String,
    pub path: String,
    pub status: String,
    /// Domain-separated hashes distinguish a missing file from an empty file.
    pub before_hash: String,
    pub after_hash: String,
    pub hunks: Vec<ReviewHunk>,
    /// Listed but never hunk-editable when present.
    pub exclusion_reason: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewChangeInfo {
    pub change_id: String,
    pub file_id: String,
    pub hunk_id: String,
    pub description: String,
    pub undone: bool,
}

/// A server-owned review. Exactly one of run_id/tool_id is present. Tool
/// candidates remain drafts; no review action publishes a registry version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HunkReview {
    pub review_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub tool_id: Option<String>,
    pub base_version: Option<i64>,
    pub original_candidate_hash: Option<String>,
    pub candidate_hash: Option<String>,
    pub draft_code: Option<String>,
    pub files: Vec<ReviewFile>,
    pub undo_changes: Vec<ReviewChangeInfo>,
    pub expires_at: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewDecisionResult {
    pub review: HunkReview,
    pub change_id: Option<String>,
    pub already_applied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewCommentResult {
    pub message: MessageInfo,
    /// Delivered to an already-running agent; false means saved for a later
    /// explicit Run. A comment never starts a Run or queues a model request.
    pub steered: bool,
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

/// Bounded, display-only call evidence. It does not alter execution arguments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalCallPreview {
    pub arguments: serde_json::Value,
    pub urls: Vec<ApprovalUrlPreview>,
    pub paths: Vec<String>,
    pub truncated: bool,
    pub note: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalUrlPreview {
    pub field: String,
    pub url: String,
    pub method: Option<String>,
    pub body_bytes: Option<u64>,
    pub body_preview: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalSource {
    pub tool: String,
    pub kind: String,
    pub origin: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ApprovalContext {
    pub run_id: Option<String>,
    pub call: Option<ApprovalCallPreview>,
    #[serde(default)]
    pub sources: Vec<ApprovalSource>,
    #[serde(default)]
    pub scope_warnings: Vec<String>,
}

/// Plain text evidence shared by terminal and line-mode clients. The daemon
/// has already bounded/redacted these display values; no input is resolved here.
pub fn approval_context_lines(context: &ApprovalContext) -> Vec<String> {
    let mut lines = Vec::new();
    for warning in &context.scope_warnings {
        lines.push(format!("⚠ {warning}"));
    }
    if !context.sources.is_empty() {
        lines.push("This Run read external content:".into());
        for source in &context.sources {
            lines.push(format!(
                "  {} ({}): {}",
                source.tool, source.kind, source.origin
            ));
        }
    }
    if let Some(call) = &context.call {
        lines.push("Resolved call arguments (redacted):".into());
        for url in &call.urls {
            lines.push(format!(
                "  URL {}: {} ({})",
                url.field,
                url.url,
                url.method.as_deref().unwrap_or("method unresolved")
            ));
            if let Some(bytes) = url.body_bytes {
                lines.push(format!("  Supplied body: {bytes} bytes"));
            }
            if let Some(preview) = &url.body_preview {
                lines.push(format!("  Body preview (redacted): {preview}"));
            }
        }
        for path in &call.paths {
            lines.push(format!("  Path: {path}"));
        }
        lines.extend(
            serde_json::to_string_pretty(&call.arguments)
                .unwrap_or_default()
                .lines()
                .map(str::to_string),
        );
        if call.truncated {
            lines.push("Preview truncated; omitted values remain unresolved here.".into());
        }
        lines.push(call.note.clone());
    }
    lines
}

/// A bounded single-line third-party description and its review warnings.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DescriptionMetadata {
    pub text: String,
    #[serde(default)]
    pub flags: Vec<String>,
}

/// Durable evidence of screened metadata creation or a description change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DescriptionReview {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub version: i64,
    pub source: String,
    pub before: Option<DescriptionMetadata>,
    pub after: DescriptionMetadata,
    pub created_at: String,
}

/// An installed skill (SPEC §7.10).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillInfo {
    #[serde(default)]
    pub description_review: DescriptionMetadata,
    #[serde(default)]
    pub description_reviews: Vec<DescriptionReview>,
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
    #[serde(default)]
    pub description_review: DescriptionMetadata,
    #[serde(default)]
    pub description_reviews: Vec<DescriptionReview>,
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
/// recorded. Each field is a UTF-8 prefix of at most 4,000 bytes plus a cut
/// marker when oversized: an illustrative preview, not necessarily valid JSON
/// or a complete replayable call. Original records are unchanged.
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

/// Read-only Step inventory sampled by GetSession. A preview is not decision authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingStep {
    pub session_id: String,
    pub run_id: String,
    pub step_id: String,
    pub tool_name: String,
    pub arguments_preview: String,
    pub arguments_bytes: usize,
    pub index: usize,
    pub count: usize,
}

/// Bounded inventory; omitted by older daemons. Decisions may change after sampling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSteps {
    pub prompts: Vec<PendingStep>,
    pub more: bool,
}

pub const STEP_PREVIEW_BYTES: usize = 4096;
pub const MAX_PENDING_STEPS: usize = 64;

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
    /// Compare stored branches, their message diff and recorded local usage.
    CompareSessions {
        left: String,
        right: String,
    },
    /// Read-only durable revision basis before explicit paid summary generation.
    InspectBranchSummary {
        source_id: String,
        target_id: String,
    },
    PrepareBranchSummary {
        source_id: String,
        target_id: String,
        source_revision: String,
        target_revision: String,
    },
    /// Confirm an exact server-owned candidate; never accepts client note text.
    CommitBranchSummary {
        preview_id: String,
        source_id: String,
        target_id: String,
        source_revision: String,
        target_revision: String,
        candidate_hash: String,
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
    /// Atomic metadata-only update of the complete reviewed pair in schema67.
    /// Both stored values must still match expected, including NULL. Returns
    /// SessionInfo. Does not rewrite auto, start a Run or invoke model tests.
    /// Mismatch is an error; clients must not fall back to SetSessionModel.
    CompareSetSessionModel {
        id: String,
        expected: SessionModelSelection,
        desired: SessionModelSelection,
    },
    /// Local-only preview; does not freeze Tools, upload files or start a Run.
    ForecastRun {
        input: ForecastInput,
        #[serde(default)]
        for_send: bool,
    },
    /// Explicit consent for an unexpired preview on this connection. Changed
    /// input/history/Tools/model/prices/threshold invalidates it before send.
    ConfirmForecast {
        id: String,
    },
    SendMessage {
        session_id: String,
        content: String,
        mode: SendMode,
        /// One per user action: a retry with the same key (after a lost
        /// reply) returns the first outcome instead of sending twice (#891).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_msg_id: Option<String>,
    },
    /// Append only if the complete, ordered pending metadata still matches
    /// the reviewed inventory. Comparison and consumption share a transaction.
    /// Queue/deferred sends are unsupported. When `reviewed_send_mode_v1` is
    /// advertised, a change between idle and active rejects the reviewed mode.
    /// Never fall back to SendMessage.
    /// This binds metadata, not mutable referenced file contents or permission.
    SendReviewedMessage {
        session_id: String,
        content: String,
        mode: SendMode,
        expected_attachments: Vec<serde_json::Value>,
        /// Required per-action key; same connection-local retention as SendMessage.
        client_msg_id: String,
    },
    /// Steer one exact Run using the complete ordered inventory from its review.
    /// Replies with `{steered:true,run_id}`. Replacements, closed Runs and
    /// changed inventory reject. Callers must not substitute another send RPC.
    SendReviewedSteer {
        session_id: String,
        run_id: String,
        content: String,
        expected_attachments: Vec<serde_json::Value>,
        client_msg_id: String,
    },
    /// Start an idle Session with immutable Run-only pipeline options. Older
    /// daemons must reject this request; clients never silently drop policy.
    StartPipelineRun {
        session_id: String,
        content: String,
        #[serde(default)]
        options: RunOptions,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_msg_id: Option<String>,
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
    /// Inspect the actual latest owned Run; no provider or file writes.
    InspectRunReview {
        session_id: String,
        #[serde(default)]
        run_id: Option<String>,
    },
    /// Upload a draft for review only; base_version must still be current.
    PrepareToolReview {
        session_id: String,
        tool_id: String,
        base_version: i64,
        candidate_code: String,
    },
    /// Server-owned content-bound hunk decision; no client paths or patch text.
    DecideReviewHunk {
        review_id: String,
        file_id: String,
        hunk_id: String,
        expected_hash: String,
        decision: String,
    },
    UndoReviewChange {
        review_id: String,
        change_id: String,
        expected_hash: String,
    },
    /// Append explicit user commentary with server-derived quoted path:line.
    /// Steers an existing Run; never starts one when idle.
    CommentReviewHunk {
        review_id: String,
        file_id: String,
        hunk_id: String,
        expected_hash: String,
        comment: String,
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
        /// Reject when the latest Run has changed since the user selected it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_run_id: Option<String>,
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
    /// Opt in on this connection to correlated answer attempts for one Session.
    /// Suppresses legacy answer/reasoning chunks. Subscribe resets this mode.
    /// Requires future_transcript_attempts_v1; use GetTranscriptBarrier next.
    SubscribeTranscriptAttempts {
        session_id: String,
    },
    /// Canonical GetSession data plus a later producer barrier. Only attempts
    /// started strictly after that barrier may be admitted. This is NOT a
    /// canonical event cutoff: never discard canonical invalidations by cursor.
    GetTranscriptBarrier {
        id: String,
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
    /// Probe a model: latency, streaming, reasoning and tool calling,
    /// remembered per (endpoint, model) (`provider` names a non-default
    /// provider). The picker's Test and `formless providers test --model`.
    TestModel {
        model: String,
        #[serde(default)]
        provider: Option<String>,
    },
    /// The latest routing decisions with each candidate's figures and the
    /// reason for the pick (`/routing`, SPEC §4.6).
    RoutingDecisions,
    /// The rate limits each endpoint last reported (`x-ratelimit-*`).
    RateLimits,
    /// Ollama: download a model (`POST /api/pull`), streaming
    /// `ModelPullProgress`.
    OllamaPull {
        model: String,
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
    /// Turn Step mode off only for a live, idle Session without recovery or
    /// waiting Steps. Returns `{session_id,on:false}`. Starts no work.
    DisableStepModeIfIdle {
        session_id: String,
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
    PreviewMcpCatalog {
        id: String,
        #[serde(default)]
        params: serde_json::Map<String, serde_json::Value>,
    },
    /// Resolve an opaque, reviewed daemon-side candidate reference again.
    ImportMcpCandidate {
        candidate_id: String,
        skip_secrets: bool,
    },
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
    /// Screen a description without saving or running anything.
    PreviewDescription {
        kind: String,
        name: String,
        description: String,
    },
    /// Inspect actual Tool/module/Skill bundle metadata without installing it.
    PreviewDescriptionBundle {
        bundle: serde_json::Value,
    },
    /// Durable description warnings and before/after changes, newest first.
    ListDescriptionReviews {
        #[serde(default)]
        kind: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        limit: Option<u32>,
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
    /// Inspect a stored Run (full id or unambiguous prefix).
    GetRun {
        run_id: String,
    },
    /// Interrupt only the matching active Run, never a replacement.
    CancelRunId {
        run_id: String,
    },
    /// Outstanding Tool capability approval requests.
    PendingApprovals,
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
    /// Read the complete pending metadata for this Session's next user message.
    /// Returns `{session_id, attachments}`; bounded by the inventory constants.
    /// Overflow/corrupt metadata is an error, never a partial or empty success.
    /// Reads no attachment bytes and mutates/consumes nothing.
    ListPendingAttachments {
        session_id: String,
    },
    /// Read the current steerable Run and complete pending attachment metadata.
    /// Returns `{session_id,run_id,attachments}` without consuming anything.
    ReadSteerReview {
        session_id: String,
    },
    /// Remove exactly one reviewed pending reference by its position in the
    /// complete inventory. Never reads/deletes reference targets or artifacts.
    /// Returns `{removed:true,session_id,index,operation_id}`. No legacy fallback.
    DetachReviewedReference {
        session_id: String,
        expected_attachments: Vec<serde_json::Value>,
        index: u32,
        /// One identity per user action; receipts are connection-local/10 min.
        operation_id: String,
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
    /// Start a chunked attachment upload of `bytes` bytes (SPEC §5.6,
    /// #909): `{upload_id, chunk_bytes, max_bytes}`. Large files go in
    /// pieces so no single message (or relay frame) holds the whole file.
    AttachBegin {
        session_id: String,
        name: String,
        bytes: u64,
        #[serde(default)]
        mime: Option<String>,
    },
    /// One piece of an upload, in order from 0: `{received}`.
    AttachChunk {
        upload_id: String,
        index: u64,
        data_b64: String,
    },
    /// Finish an upload whose pieces add up to its size: the attachment,
    /// as `AttachFile` returns it. Until then nothing is attached.
    AttachFinish {
        upload_id: String,
    },
    /// Drop an unfinished upload: `{aborted}`.
    AttachAbort {
        upload_id: String,
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
    /// action or detail; `before` accepts an entry's opaque `cursor` or a
    /// legacy `created_at` timestamp (strictly older timestamps only).
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
    /// Replace a complete value, or set/remove one literal map entry.
    SetSetting {
        key: String,
        value: serde_json::Value,
    },
    /// Merge a map atomically. A separate request makes older daemons reject
    /// the operation instead of ignoring a new mode field and replacing it.
    MergeSetting {
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
    /// tool_schemas, history, attachments}}`. `used_tokens` is the
    /// current rebuilt prompt's locally calibrated estimate (`measured: false`);
    /// breakdown uses the shared Unicode/JSON/image estimator.
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
    /// provider's `GET /models`; `recent` deduplicates exact nonempty Session
    /// model IDs after ordering stored updated_at descending, then Session ID
    /// ascending. Archived Sessions participate; this is not model-use history.
    ListModels,
    /// Add or replace a named provider; `api_key` goes to the keychain.
    UpsertProvider {
        name: String,
        base_url: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        api_key: Option<String>,
        /// Extra request headers and query parameters (gateways, Azure's
        /// `api-version`); None leaves them as they are.
        #[serde(default)]
        headers: Option<std::collections::BTreeMap<String, String>>,
        #[serde(default)]
        query_params: Option<std::collections::BTreeMap<String, String>>,
        /// Header carrying the key instead of `Authorization: Bearer`
        /// (`api-key` for Azure); Some("") restores Bearer.
        #[serde(default)]
        api_key_header: Option<String>,
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
        /// Bind a reviewed metadata edit to its current version; older clients
        /// omit this and retain their existing explicit-update behavior.
        #[serde(default)]
        expected_current_version: Option<i64>,
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
    /// Revert only the registry change identified by a specific event.
    /// A separate request prevents older daemons from treating it as global Undo.
    UndoRegistryChangeIfCurrent {
        change_id: String,
    },
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

/// Live update categories that a slow subscriber may miss. Repeated drops
/// of one kind in one scope coalesce until the warning can be delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DroppedEventKind {
    MessageDelta,
    ReasoningDelta,
    ToolProgress,
    RunUsage,
    InvocationActivity,
    ModelPullProgress,
    /// Preserve a warning when a newer peer adds another live category.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Opt-in, correlated ephemeral answer attempt. Not canonical history.
    TranscriptAttempt {
        update: TranscriptAttemptEvent,
    },
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
    /// Live text or progress updates were skipped for this subscriber.
    /// Empty Session/Run strings denote a global or non-Run scope. Optional
    /// metadata is additive: older peers still understand the original fields.
    DeltasDropped {
        session_id: String,
        run_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kinds: Vec<DroppedEventKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handle: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
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
        #[serde(default)]
        plan_state: Option<PlanState>,
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
        #[serde(default)]
        untracked_writes_possible: bool,
    },
    /// Ollama download progress for `model` (status text, bytes so far
    /// and in total when known); `done` ends it.
    ModelPullProgress {
        model: String,
        status: String,
        #[serde(default)]
        completed: Option<u64>,
        #[serde(default)]
        total: Option<u64>,
        #[serde(default)]
        done: bool,
        #[serde(default)]
        error: Option<String>,
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
        #[serde(default)]
        invocation_id: Option<String>,
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
        #[serde(default)]
        invocation_id: Option<String>,
        session_id: String,
        run_id: String,
        tool_name: String,
        #[serde(default)]
        depth: usize,
        fraction: Option<f64>,
        label: String,
    },
    ToolCallFinished {
        #[serde(default)]
        invocation_id: Option<String>,
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
    /// An unanswered approval was closed, for example when its Run was
    /// interrupted. This is neither an approval nor a user denial.
    ApprovalClosed {
        approval_id: String,
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
        /// Additive resolved-call/source evidence, also retained for reconnect.
        #[serde(default)]
        context: Option<ApprovalContext>,
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
        #[serde(default)]
        invocation_id: Option<String>,
        session_id: String,
        run_id: String,
        handle: String,
        tool_name: String,
    },
    InvocationCompleted {
        #[serde(default)]
        invocation_id: Option<String>,
        session_id: String,
        handle: String,
        tool_name: String,
        ok: bool,
    },
    /// A running Invocation has shown no activity for `silent_secs`
    /// (SPEC §6.1: advisory, never auto-killed).
    InvocationStalled {
        #[serde(default)]
        last_fraction: Option<f64>,
        #[serde(default)]
        last_label: Option<String>,
        #[serde(default)]
        invocation_id: Option<String>,
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
        #[serde(default)]
        invocation_id: Option<String>,
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
    /// A promotion prompt was resolved by any client.
    PromotionApprovalResolved {
        promotion_id: String,
    },
}

impl Event {
    /// The Session this event belongs to, or `None` for global events.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::TranscriptAttempt { update } => Some(&update.session_id),
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
            Event::DeltasDropped { session_id, .. } => {
                (!session_id.is_empty()).then_some(session_id.as_str())
            }
            Event::Note { session_id, .. } => session_id.as_deref(),
            Event::SessionUpdated { session } => Some(&session.id),
            Event::SessionDeleted { id } => Some(id),
            Event::SettingsChanged
            | Event::McpServersChanged
            | Event::ModelPullProgress { .. }
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
            | Event::PromotionApprovalResolved { .. }
            | Event::ApprovalAnswered { .. }
            | Event::ApprovalClosed { .. } => None,
        }
    }
}

/// Largest NDJSON line either side reads (a 20 MB attachment's base64 plus
/// room); a longer one is refused while reading, never buffered (#888).
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// Read one `\n`-terminated line of at most `max` bytes, without the
/// newline (None at EOF). Longer lines are an `InvalidData` error after
/// reading at most `max + 1` bytes of them.
pub fn read_line_capped<R: std::io::BufRead>(
    r: &mut R,
    max: usize,
) -> std::io::Result<Option<String>> {
    use std::io::{BufRead as _, Read};
    let mut buf = Vec::new();
    let n = r
        .by_ref()
        .take(max as u64 + 1)
        .read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
    } else if buf.len() > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a message over {max} bytes"),
        ));
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Machine-readable error codes (#885). A failed response carries one as
/// `result: {"code": …}`; the `error` text is for people only, so clients
/// branch on [`ServerMessage::error_code`], never on the wording.
///
/// One rule for partial failure: a request that did what was asked but
/// with a degraded part (`ListModels` when the listing call failed, an
/// MCP server added but not yet started) succeeds and names the problem
/// in its result's `error` field; a request that did not do what was
/// asked fails.
pub mod codes {
    pub const PROTOCOL_MISMATCH: &str = "protocol_mismatch";
    pub const UNSUPPORTED: &str = "unsupported";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const NOT_FOUND: &str = "not_found";
    pub const BUSY: &str = "busy";
    pub const BUDGET: &str = "budget";
    pub const INVALID: &str = "invalid";
    pub const INTERNAL: &str = "internal";
}

impl ServerMessage {
    /// A failed response with a machine-readable `code` (see [`codes`]).
    pub fn err_code(id: u64, code: &str, error: String) -> Self {
        ServerMessage::Response {
            id,
            ok: false,
            result: Some(serde_json::json!({ "code": code })),
            error: Some(error),
        }
    }

    /// The `code` of a failed response, if it has one.
    pub fn error_code(&self) -> Option<&str> {
        match self {
            ServerMessage::Response {
                ok: false, result, ..
            } => result.as_ref()?.get("code")?.as_str(),
            _ => None,
        }
    }

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
    #[test]
    fn tool_usage_timestamp_is_an_optional_additive_wire_field() {
        let legacy = serde_json::json!({
            "id": "tool", "name": "tool", "description": "", "retired": false,
            "current_version": 1, "capabilities": [], "memory_mb": 64,
            "invocations": 2, "success_rate": 0.5, "needs_attention": false,
            "created_at": "2026-01-01T00:00:00+00:00",
            "updated_at": "2026-02-01T00:00:00+00:00"
        });
        let mut decoded: super::ToolInfo = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.last_used_at, None);
        let before = serde_json::to_value(&decoded).unwrap();
        assert!(before.get("last_used_at").is_none());
        decoded.last_used_at = Some("2026-01-10T00:00:00+00:00".into());
        let mut after = serde_json::to_value(&decoded).unwrap();
        assert_eq!(
            serde_json::from_value::<super::ToolInfo>(after.clone()).unwrap(),
            decoded
        );
        assert_eq!(
            after
                .as_object_mut()
                .unwrap()
                .remove("last_used_at")
                .unwrap(),
            "2026-01-10T00:00:00+00:00"
        );
        assert_eq!(after, before);
    }

    #[test]
    fn unanswered_approval_close_is_global_and_has_no_decision_value() {
        let event: super::Event = serde_json::from_value(
            serde_json::json!({"type":"approval_closed","approval_id":"held"}),
        )
        .unwrap();
        assert_eq!(event.session_id(), None);
        let encoded = serde_json::to_value(event).unwrap();
        assert_eq!(encoded["approval_id"], "held");
        assert!(encoded.get("approved").is_none());
    }

    #[test]
    fn legacy_plan_and_recap_lifecycle_fields_are_unknown_and_new_records_round_trip() {
        let legacy: super::Event = serde_json::from_value(
            serde_json::json!({"type":"plan_updated","session_id":"s","items":[]}),
        )
        .unwrap();
        let super::Event::PlanUpdated { plan_state, .. } = legacy else {
            panic!("wrong event")
        };
        assert_eq!(plan_state, None);
        let metadata: super::PlanState = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(metadata, super::PlanState::default());
        let recap: super::RunRecap = serde_json::from_value(serde_json::json!({"steps":4,"duration_ms":1,"used":["tool ×7"],"built":[],"files":[],"steered":0})).unwrap();
        assert_eq!(recap.status, "");
        assert_eq!(recap.total_calls, None);
        assert_eq!(recap.unfinished_calls, 0);
        let record = super::RecordedRunRecap {
            run_id: "owned".into(),
            recap: super::RunRecap {
                status: "paused".into(),
                total_calls: Some(7),
                unfinished_calls: 2,
                ..recap
            },
        };
        assert_eq!(
            serde_json::from_value::<super::RecordedRunRecap>(
                serde_json::to_value(&record).unwrap()
            )
            .unwrap(),
            record
        );
        let metadata = super::PlanState {
            run_id: None,
            updated_at: Some("2026-11-02T00:00:00Z".into()),
            run_status: Some("interrupted".into()),
            previous_task: true,
            origin: "imported".into(),
        };
        assert_eq!(
            serde_json::from_value::<super::PlanState>(serde_json::to_value(&metadata).unwrap())
                .unwrap(),
            metadata
        );
    }

    #[test]
    fn legacy_invocation_identity_and_progress_diagnostics_default_and_round_trip() {
        for kind in [
            "tool_call_started",
            "tool_progress",
            "tool_call_finished",
            "invocation_pending",
            "invocation_completed",
            "invocation_stalled",
            "invocation_activity",
        ] {
            let raw = serde_json::json!({"type":kind,"session_id":"s","run_id":"r","tool_name":"same","version":1,"fraction":0.5,"label":"work","ok":true,"handle":"h","silent_secs":5});
            let event: super::Event = serde_json::from_value(raw).unwrap();
            let mut encoded = serde_json::to_value(event).unwrap();
            assert!(encoded["invocation_id"].is_null());
            encoded["invocation_id"] = serde_json::json!("inv-1");
            if kind == "invocation_stalled" {
                encoded["last_fraction"] = serde_json::json!(0.5);
                encoded["last_label"] = serde_json::json!("work");
            }
            let decoded: super::Event = serde_json::from_value(encoded.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
        }
        let mut info: super::InvocationInfo = serde_json::from_value(serde_json::json!({"handle":"h","tool_name":"same","session_id":"s","elapsed_ms":1,"silent_ms":2,"stalled":false,"abandoned":false})).unwrap();
        assert_eq!(info.invocation_id, None);
        assert_eq!(info.last_fraction, None);
        assert_eq!(info.last_label, None);
        info.invocation_id = Some("inv-1".into());
        info.last_fraction = Some(0.5);
        info.last_label = Some("work".into());
        assert_eq!(
            serde_json::from_value::<super::InvocationInfo>(serde_json::to_value(&info).unwrap())
                .unwrap(),
            info
        );
    }

    #[test]
    fn legacy_run_file_coverage_defaults_and_new_fields_round_trip() {
        let raw = serde_json::json!({"type":"run_files_changed","session_id":"s","run_id":"r","files":[{"path":"x","status":"created","revertible":true}]});
        let event: super::Event = serde_json::from_value(raw).unwrap();
        let super::Event::RunFilesChanged {
            mut files,
            untracked_writes_possible,
            ..
        } = event
        else {
            panic!("wrong event")
        };
        assert!(!untracked_writes_possible);
        assert_eq!(files[0].kind, "");
        assert_eq!(files[0].revert_reason, None);
        files[0].kind = "symlink".into();
        files[0].revert_reason = Some("symlink target is not tracked".into());
        let new = super::Event::RunFilesChanged {
            session_id: "s".into(),
            run_id: "r".into(),
            files,
            untracked_writes_possible: true,
        };
        let encoded = serde_json::to_value(&new).unwrap();
        let decoded: super::Event = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
        let recap: super::RunRecap = serde_json::from_value(serde_json::json!({"steps":4,"duration_ms":1,"used":[],"built":[],"files":[],"steered":0})).unwrap();
        assert!(!recap.untracked_writes_possible);
        let new = super::RunRecap {
            untracked_writes_possible: true,
            ..recap
        };
        assert_eq!(
            serde_json::from_value::<super::RunRecap>(serde_json::to_value(&new).unwrap()).unwrap(),
            new
        );
    }

    #[test]
    fn legacy_session_payloads_have_unknown_lineage_and_new_records_round_trip() {
        let raw = serde_json::json!({"id":"legacy","name":"Imported or old", "model":null,"reasoning_effort":null,"created_at":"2020-01-01T00:00:00Z","updated_at":"2020-01-01T00:00:00Z"});
        let mut session: super::SessionInfo = serde_json::from_value(raw).unwrap();
        assert_eq!(session.forked_from, None);
        assert_eq!(session.fork_point_seq, None);
        session.forked_from = Some("recorded-parent".into());
        session.fork_point_seq = Some(0);
        let encoded = serde_json::to_value(&session).unwrap();
        assert_eq!(encoded["forked_from"], "recorded-parent");
        assert_eq!(encoded["fork_point_seq"], 0);
        assert_eq!(
            serde_json::from_value::<super::SessionInfo>(encoded).unwrap(),
            session
        );
    }
    use super::*;

    #[test]
    fn dropped_event_metadata_is_additive_and_global_scope_remains_global() {
        let old: Event = serde_json::from_value(
            serde_json::json!({"type":"deltas_dropped", "session_id":"s", "run_id":"r"}),
        )
        .unwrap();
        assert!(
            matches!(old, Event::DeltasDropped { kinds, handle: None, model: None, .. } if kinds.is_empty())
        );
        let future: Event = serde_json::from_value(serde_json::json!({"type":"deltas_dropped", "session_id":"s", "run_id":"r", "kinds":["future_live_kind"]})).unwrap();
        assert!(
            matches!(future, Event::DeltasDropped { kinds, .. } if kinds == [DroppedEventKind::Unknown])
        );
        let event = Event::DeltasDropped {
            session_id: String::new(),
            run_id: String::new(),
            kinds: vec![DroppedEventKind::ModelPullProgress],
            handle: None,
            model: Some("model".into()),
        };
        assert_eq!(event.session_id(), None);
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kinds"], serde_json::json!(["model_pull_progress"]));
        assert!(value.get("handle").is_none());
        let decoded: Event = serde_json::from_value(value).unwrap();
        assert!(
            matches!(decoded, Event::DeltasDropped { model: Some(model), .. } if model == "model")
        );
    }

    #[test]
    fn programs_that_run_others_are_flagged_broad() {
        for spec in [
            "process:python3.11",
            "process:ruby3.2",
            "process:node20",
            "process:git",
            "process:git:status|log",
            "process:find",
            "process:tar",
            "process:make",
        ] {
            assert!(broad_capability(spec).is_some(), "{spec}");
        }
        assert!(broad_capability("process:ls").is_none());
    }

    #[test]
    fn dart_require_broad_process_confirmation() {
        for spec in [
            "process:dart",
            "process:dart3.2",
            "process:/usr/bin/dart",
            "process:dart.exe",
            "process:dart:run",
        ] {
            assert!(broad_capability(spec).is_some(), "{spec}");
        }
        for spec in ["process:dartdoc", "process:dart2js", "process:ls"] {
            assert_eq!(broad_capability(spec), None, "{spec}");
        }
    }

    #[test]
    fn ruby_package_tools_require_broad_process_confirmation() {
        for spec in [
            "process:gem",
            "process:gem3.2",
            "process:/usr/bin/gem",
            "process:gem.exe",
            "process:gem:install",
            "process:bundle",
            "process:bundler",
            "process:bundle.exe",
        ] {
            assert!(broad_capability(spec).is_some(), "{spec}");
        }
        for spec in [
            "process:gemini",
            "process:bundles",
            "process:bundle2",
            "process:rake",
            "process:ls",
        ] {
            assert_eq!(broad_capability(spec), None, "{spec}");
        }
    }

    #[test]
    fn gradle_maven_requires_broad_process_confirmation() {
        for spec in [
            "process:gradle",
            "process:gradlew",
            "process:/usr/bin/gradle",
            "process:gradle.exe",
            "process:mvn",
            "process:mvnw",
            "process:gradle:tasks",
            "process:mvnw:validate",
        ] {
            assert_eq!(
                broad_capability(spec),
                Some("Gradle and Maven can execute repository build code"),
                "{spec}"
            );
        }
        for spec in ["process:mvnDebug", "process:gradle-profiler"] {
            assert_eq!(broad_capability(spec), None, "{spec}");
        }
    }

    #[test]
    fn dotnet_requires_broad_process_confirmation() {
        for spec in [
            "process:dotnet",
            "process:/usr/bin/dotnet",
            "process:dotnet.exe",
            "process:DOTNET",
            "process:dotnet8",
            "process:dotnet:run",
        ] {
            assert_eq!(
                broad_capability(spec),
                Some("dotnet can build and execute repository code"),
                "{spec}"
            );
        }
        for spec in [
            "process:dotnet-dump",
            "process:dotnet-trace",
            "process:dotnet-counters",
        ] {
            assert_eq!(broad_capability(spec), None, "{spec}");
        }
    }

    #[test]
    fn shell_launcher_and_interpreter_aliases_require_extra_confirmation() {
        let reason = broad_capability("process:bash").unwrap();
        for spec in [
            "process:pkexec",
            "process:/usr/bin/pkexec",
            "process:su",
            "process:su.exe",
            "process:runuser",
            "process:ksh",
            "process:/usr/bin/ksh",
            "process:csh",
            "process:tcsh",
            "process:mksh",
            "process:ash",
            "process:nodejs",
            "process:pythonw",
            "process:pythonw.exe",
            "process:py",
            "process:py.exe",
            "process:pyw",
            "process:pyw.exe",
            "process:py3",
            r"process:C:\Python311\pythonw.exe",
        ] {
            assert_eq!(broad_capability(spec), Some(reason), "{spec}");
        }
        for spec in ["process:ls", "process:pytest", "process:nodejs-helper"] {
            assert_eq!(broad_capability(spec), None, "{spec}");
        }
    }

    #[test]
    fn windows_process_path_keeps_drive_prefix_and_optional_scope() {
        for (spec, equivalent) in [
            (r"process:C:\Windows\System32\CMD.EXE", "process:cmd"),
            (
                r"process:C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                "process:powershell",
            ),
            (r"process:C:\Python311\python.exe", "process:python"),
            ("process:C:/Python311/python.exe", "process:python"),
            (r"process:C:\Windows\System32\curl.exe", "process:curl"),
            (r"process:C:\Program Files\Git\cmd\git.exe", "process:git"),
            (
                r"process:C:\Program Files\Git\cmd\git.exe:status|log",
                "process:git:status|log",
            ),
        ] {
            let reason = broad_capability(equivalent).unwrap();
            assert_eq!(broad_capability(spec), Some(reason), "{spec}");
        }
        assert_eq!(broad_capability(r"process:C:\tools\pytest.exe"), None);
        assert_eq!(broad_capability(r"process:C:\tools\ls.exe"), None);
    }

    #[test]
    fn go_requires_broad_process_confirmation() {
        for spec in [
            "process:go",
            "process:/usr/local/go/bin/go",
            "process:go.exe",
            "process:GO",
            "process:go1.22.5",
            "process:go:test",
        ] {
            assert_eq!(
                broad_capability(spec),
                Some("the Go tool can build and execute repository code"),
                "{spec}"
            );
        }
        for spec in [
            "process:gofmt",
            "process:godoc",
            "process:goreleaser",
            "process:golangci-lint",
        ] {
            assert_eq!(broad_capability(spec), None, "{spec}");
        }
    }

    #[test]
    fn pip_requires_broad_process_confirmation() {
        for spec in [
            "process:pip",
            "process:pip3",
            "process:pip3.11",
            "process:/usr/bin/pip3",
            "process:pip.exe",
            "process:pip:install",
        ] {
            assert_eq!(
                broad_capability(spec),
                Some("pip can execute code from package build backends"),
                "{spec}"
            );
        }
        for spec in ["process:pipx", "process:pipenv", "process:piper"] {
            assert_eq!(broad_capability(spec), None, "{spec}");
        }
    }

    #[test]
    fn windows_drive_and_home_write_scopes_are_broad() {
        for path in [
            r"C:\",
            "c:/",
            r"D:\Users",
            r"C:\Users\Ada\",
            r"\\?\C:\USERS\Ada",
        ] {
            assert_eq!(
                broad_capability(&format!("fs:write:{path}")),
                Some("writing anywhere in your files"),
                "{path}"
            );
        }
        for path in [
            r"C:",
            r"C:Users\Ada",
            r"C:\Users\Ada\project",
            r"C:\repo\Users\Ada",
            r"/project\Users\Ada",
        ] {
            assert_eq!(
                broad_capability(&format!("fs:write:{path}")),
                None,
                "{path}"
            );
        }
    }

    #[test]
    fn windows_path_notes_handle_credential_folders_and_working_directory_boundaries() {
        assert_eq!(
            path_note(r"fs:read:C:\Users\Ada\.SSH\id_rsa", Some(r"C:\Users\Ada")),
            Some((true, "holds SSH keys".into()))
        );
        assert_eq!(
            path_note(
                "fs:read:c:/Users/Ada/project/file",
                Some(r"C:\USERS\Ada\project\")
            ),
            Some((false, "inside the working folder".into()))
        );
        assert_eq!(
            path_note(
                "fs:read:c:/Users/Ada/project-other/file",
                Some(r"C:\Users\Ada\project")
            ),
            Some((
                true,
                r"outside the working folder C:\Users\Ada\project".into()
            ))
        );
    }

    #[test]
    fn lines_are_capped_while_reading() {
        let mut r = std::io::BufReader::new(&b"short\nxxxxxxxxxxxxxxxxxxxx"[..]);
        assert_eq!(
            read_line_capped(&mut r, 10).unwrap().as_deref(),
            Some("short")
        );
        assert!(
            read_line_capped(&mut r, 10).is_err(),
            "20 bytes, no newline"
        );
        let mut r = std::io::BufReader::new(&b"last"[..]);
        assert_eq!(
            read_line_capped(&mut r, 10).unwrap().as_deref(),
            Some("last")
        );
        assert_eq!(read_line_capped(&mut r, 10).unwrap(), None);
        let coded = ServerMessage::err_code(3, codes::NOT_FOUND, "no such Tool".into());
        assert_eq!(coded.error_code(), Some("not_found"));
        assert_eq!(ServerMessage::err(3, "x".into()).error_code(), None);
    }

    #[test]
    fn versions_negotiate_to_the_highest_common_one() {
        assert_eq!(negotiate(1, 1), Some(1));
        assert_eq!(negotiate(1, PROTOCOL_VERSION + 3), Some(PROTOCOL_VERSION));
        assert_eq!(negotiate(PROTOCOL_VERSION + 1, PROTOCOL_VERSION + 2), None);
    }

    #[test]
    fn older_and_newer_envelopes_still_parse() {
        // One version behind: a Subscribe from before `client` existed.
        let old: RequestEnvelope = serde_json::from_str(
            r#"{"id":4,"protocol":1,"request":{"type":"subscribe","sessions":["s"]}}"#,
        )
        .unwrap();
        assert!(matches!(
            old.request,
            Request::Subscribe {
                client: None,
                deltas: true,
                ..
            }
        ));
        // A newer peer's extra fields are ignored (additive = minor).
        let newer: RequestEnvelope = serde_json::from_str(
            r#"{"id":5,"protocol":1,"trace":"x","request":{"type":"ping","hint":1}}"#,
        )
        .unwrap();
        assert!(matches!(newer.request, Request::Ping));
        let round = serde_json::to_string(&newer).unwrap();
        assert!(serde_json::from_str::<RequestEnvelope>(&round).is_ok());
    }

    #[test]
    fn unknown_request_types_are_named_and_keep_their_id() {
        let line = r#"{"id":9,"protocol":1,"request":{"type":"teleport","to":"mars"}}"#;
        assert_eq!(lenient_id(line), 9);
        assert_eq!(unknown_request_type(line).as_deref(), Some("teleport"));
        // A known type with bad fields is a bad request, not unsupported.
        let bad = r#"{"id":3,"protocol":1,"request":{"type":"send_message"}}"#;
        assert_eq!(unknown_request_type(bad), None);
        assert_eq!(lenient_id("not json"), 0);
        let ServerMessage::Response { id, ok, result, .. } = unsupported(9, "teleport") else {
            panic!()
        };
        assert_eq!((id, ok), (9, false));
        assert_eq!(result.unwrap()["code"], "unsupported");
    }

    #[test]
    fn event_session_id_scopes_session_events() {
        let ev = Event::ApprovalRequested {
            context: None,
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
            "process:ls",
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
    fn composer_auth_warnings_are_limited_to_config_locations() {
        for path in [
            "/work/.composer/auth.json",
            "/work/.config/Composer/AUTH.JSON",
        ] {
            assert_eq!(
                path_note(&format!("fs:read:{path}"), Some("/work")),
                Some((true, "holds Composer credentials".into()))
            );
        }
        for path in [
            "/work/auth.json",
            "/work/.composer/cache/auth.json",
            "/work/.config/composer/config.json",
        ] {
            assert_eq!(
                path_note(&format!("fs:read:{path}"), Some("/work")),
                Some((false, "inside the working folder".into()))
            );
        }
    }

    #[test]
    fn environment_key_grants_use_credential_name_boundaries() {
        for name in [
            "AWS_ACCESS_KEY_ID",
            "aws_access_key_id",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "KEY",
            "OPENAI_API_KEY",
            "OPENAIAPIKEY",
            "ACCESSKEY",
            "ENCRYPTIONKEY",
            "SIGNINGKEY",
            "HMACKEY",
            "PRIMARY_KEY_TOKEN",
            "SSH_AUTH_SOCK",
        ] {
            assert_eq!(
                broad_capability(&format!("env:{name}")),
                Some("a secret from your environment"),
                "{name}"
            );
        }
        for name in [
            "MONKEY",
            "TURKEY",
            "DONKEY",
            "HOCKEY",
            "JOCKEY",
            "HOTKEY",
            "PRIMARY_KEY",
            "foreign_key",
            "UNIQUE_KEY",
            "CANDIDATE_KEY",
            "SORT_KEY",
            "PARTITION_KEY",
            "HOME",
        ] {
            assert_eq!(broad_capability(&format!("env:{name}")), None, "{name}");
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

    #[test]
    fn credential_read_warning_is_exact_and_independent_of_working_directory() {
        for capability in [
            "fs:read-sensitive:/private/project/token.json",
            r"fs:read-sensitive:C:\private\token.json",
        ] {
            let warning = sensitive_read_warning(capability).unwrap();
            assert!(warning.contains("exact approved path identity only, not descendants"));
            assert!(warning.contains("may flow to the model provider"));
            for cwd in [None, Some("/private/project"), Some("/different/project")] {
                assert_eq!(path_note(capability, cwd), Some((true, warning.into())));
            }
        }
        assert_eq!(sensitive_read_warning("fs:read:/private/project"), None);
        assert_eq!(sensitive_read_warning("fs:write:/private/project"), None);
    }
}

#[cfg(test)]
mod future_attempt_wire_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn future_attempt_requests_are_additive_and_legacy_subscribe_is_unchanged() {
        for value in [
            json!({"type":"subscribe_transcript_attempts","session_id":"s"}),
            json!({"type":"get_transcript_barrier","id":"s"}),
        ] {
            let request: Request = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), value);
        }
        let old = json!({"type":"subscribe","sessions":["s"],"deltas":false,"client":"old client"});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<Request>(old.clone()).unwrap()).unwrap(),
            old
        );
        assert!(FEATURES.contains(&"future_transcript_attempts_v1"));
    }
    #[test]
    fn attempts_have_explicit_scope_identity_offset_and_terminal_semantics() {
        for change in [
            TranscriptAttemptChange::Started { base_seq: 0 },
            TranscriptAttemptChange::Delta {
                offset: 13,
                text: "ไทย😀".into(),
            },
            TranscriptAttemptChange::Closed {
                bytes: 26,
                reason: TranscriptAttemptClose::Returned,
            },
        ] {
            let event = Event::TranscriptAttempt {
                update: TranscriptAttemptEvent {
                    epoch: "e".into(),
                    cursor: u64::MAX,
                    session_id: "s".into(),
                    run_id: "r".into(),
                    turn_id: "t".into(),
                    attempt_id: "a".into(),
                    change,
                },
            };
            assert_eq!(event.session_id(), Some("s"));
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(
                serde_json::to_value(serde_json::from_value::<Event>(value.clone()).unwrap())
                    .unwrap(),
                value
            );
            assert_eq!(value["type"], "transcript_attempt");
        }
    }
}

#[cfg(test)]
mod idle_step_off_wire_tests {
    use super::*;

    #[test]
    fn idle_step_off_requires_a_session_and_uses_its_own_capability() {
        let value = serde_json::json!({"type":"disable_step_mode_if_idle","session_id":"s"});
        let request = serde_json::from_value::<Request>(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
        assert!(serde_json::from_value::<Request>(
            serde_json::json!({"type":"disable_step_mode_if_idle"})
        )
        .is_err());
        assert!(FEATURES.contains(&IDLE_STEP_OFF_FEATURE));
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}

#[cfg(test)]
mod pending_attachment_inventory_wire_tests {
    use super::*;
    #[test]
    fn pending_inventory_is_an_additive_advertised_request() {
        let request = Request::ListPendingAttachments {
            session_id: "s".into(),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"type":"list_pending_attachments","session_id":"s"})
        );
        assert!(
            matches!(serde_json::from_value::<Request>(value).unwrap(),Request::ListPendingAttachments{session_id}if session_id=="s")
        );
        assert!(FEATURES.contains(&"pending_attachment_inventory_v1"));
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}

#[cfg(test)]
mod reviewed_attachment_send_tests {
    use super::*;
    #[test]
    fn reviewed_send_is_an_explicit_additive_operation_with_required_expected_metadata_and_identity(
    ) {
        let value = serde_json::json!({"type":"send_reviewed_message","session_id":"s","content":"draft ไทย😀","mode":"send","expected_attachments":[],"client_msg_id":"one-action"});
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert!(
            matches!(&request, Request::SendReviewedMessage {expected_attachments, ..} if expected_attachments.is_empty())
        );
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        for field in ["expected_attachments", "client_msg_id"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        assert!(FEATURES.contains(&"reviewed_attachment_send_v1"));
        assert_eq!(PROTOCOL_VERSION, 1);
    }
    #[test]
    fn exact_steer_requires_run_id_and_separate_optional_operations() {
        let value = serde_json::json!({"type":"send_reviewed_steer","session_id":"s","run_id":"run-a","content":"exact","expected_attachments":[],"client_msg_id":"one"});
        let request = serde_json::from_value::<Request>(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
        for field in ["run_id", "expected_attachments", "client_msg_id"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        let read = Request::ReadSteerReview {
            session_id: "s".into(),
        };
        assert_eq!(
            serde_json::to_value(read).unwrap(),
            serde_json::json!({"type":"read_steer_review","session_id":"s"})
        );
        assert!(valid_reviewed_run_id("run-a"));
        for invalid in ["", "a b", "a\n", "a\u{202e}", &"a".repeat(257)] {
            assert!(!valid_reviewed_run_id(invalid));
        }
        assert!(FEATURES.contains(&REVIEWED_STEER_RUN_FEATURE));
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}

#[cfg(test)]
mod reviewed_reference_detach_tests {
    use super::*;
    #[test]
    fn reviewed_reference_detach_is_additive_and_requires_position_inventory_and_identity() {
        let value = serde_json::json!({"type":"detach_reviewed_reference","session_id":"s","expected_attachments":[],"index":0,"operation_id":"one"});
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
        for field in ["expected_attachments", "index", "operation_id"] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Request>(missing).is_err());
        }
        assert!(FEATURES.contains(&"reviewed_reference_detach_v1"));
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}

#[cfg(test)]
mod session_selection_cas_tests {
    use super::*;
    #[test]
    fn session_selection_cas_requires_explicit_complete_nullable_pairs() {
        let value = serde_json::json!({"type":"compare_set_session_model","id":"s","expected":{"model":null,"reasoning_effort":null},"desired":{"model":"role:reviewer","reasoning_effort":"high"}});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<Request>(value.clone()).unwrap())
                .unwrap(),
            value
        );
        for side in ["expected", "desired"] {
            for key in ["model", "reasoning_effort"] {
                let mut bad = value.clone();
                bad[side].as_object_mut().unwrap().remove(key);
                assert!(serde_json::from_value::<Request>(bad).is_err());
            }
            let mut bad = value.clone();
            bad[side]["extra"] = serde_json::json!(1);
            assert!(serde_json::from_value::<Request>(bad).is_err());
        }
        for key in ["id", "expected", "desired"] {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<Request>(bad).is_err());
        }
        assert!(FEATURES.contains(&"session_model_compare_set_v1"));
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
