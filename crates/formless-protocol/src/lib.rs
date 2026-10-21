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
    /// A sub-agent and its transcript (provider-shaped messages).
    GetSubagent {
        id: String,
    },
    /// Stop one running Tool Invocation without interrupting its Run (it is
    /// recorded as user-cancelled, and the agent gets the error result).
    CancelInvocation {
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
    },
    ToolCallFinished {
        session_id: String,
        run_id: String,
        tool_name: String,
        ok: bool,
        #[serde(default)]
        elapsed_ms: i64,
    },
    ApprovalRequested {
        approval_id: String,
        session_id: String,
        tool_name: String,
        version: i64,
        capabilities: Vec<String>,
        memory_mb: u32,
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
}
