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
    ListTools,
    GetTool {
        id: String,
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
    },
    PromotionDecision {
        promotion_id: String,
        approved: bool,
    },
    RunOptimiser {
        tool_id: Option<String>,
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
    ToolCallStarted {
        session_id: String,
        run_id: String,
        tool_name: String,
        version: i64,
    },
    ToolCallFinished {
        session_id: String,
        run_id: String,
        tool_name: String,
        ok: bool,
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
