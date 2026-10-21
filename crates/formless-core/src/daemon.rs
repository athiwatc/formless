use anyhow::{anyhow, Context, Result};
use formless_protocol::*;
use formless_runtime::{Capabilities, ExecErrorKind, ExecRequest, HostMode, HostOps};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::config::{Config, ProviderCfg};
use crate::namer::{self, ModelSpec};
use crate::provider::{DeltaKind, Provider, ToolCall, SYSTEM_PROMPT};
use crate::storage::{build_context, Store, DEFAULT_SESSION_NAME};
use crate::tools as toolreg;
use crate::Paths;

// A std mutex (never held across .await) so broadcast() sends synchronously
// and every subscriber sees events in emission order.
type Subs = Arc<StdMutex<Vec<mpsc::UnboundedSender<ServerMessage>>>>;
type ExecFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = std::result::Result<(Value, String), String>> + Send>,
>;

pub(crate) struct PendingEntry {
    tool_name: String,
    session_id: String,
    result: StdMutex<Option<(bool, String)>>,
    delivered: AtomicBool,
    abandoned: AtomicBool,
    stalled_noted: AtomicBool,
    activity: Arc<std::sync::atomic::AtomicU64>,
    notify: tokio::sync::Notify,
}

/// Fan an event out to every live subscriber, dropping disconnected ones.
fn send_all(subs: &Subs, ev: Event) {
    subs.lock()
        .unwrap()
        .retain(|tx| tx.send(ServerMessage::Event(ev.clone())).is_ok());
}

/// Tool result for a call whose arguments hit the output token limit.
fn truncated_call_message(tool: &str) -> String {
    format!(
        "error: your {tool} call was cut off at the output token limit, so its arguments are \
         incomplete and it was not run. Do not resend it unchanged: make it smaller (for \
         create_tool, move helpers into a module or shorten implementation_js; otherwise \
         send less data per call)."
    )
}

pub(crate) fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Answers a waiting approval: (approved, scope).
type ApprovalReply = oneshot::Sender<(bool, ApprovalScope)>;

pub(crate) struct RunCtl {
    cancel: CancellationToken,
    cancel_flag: Arc<AtomicBool>,
    steer_tx: mpsc::UnboundedSender<()>,
    /// User-requested re-send of the in-flight provider request.
    retry: Arc<tokio::sync::Notify>,
}

pub struct Daemon {
    pub(crate) paths: Paths,
    pub(crate) store: Arc<StdMutex<Store>>,
    pub(crate) runs: Arc<StdMutex<HashMap<String, RunCtl>>>,
    approvals: Arc<StdMutex<HashMap<String, ApprovalReply>>>,
    /// promotion_id -> (attempt_id, kind: "widening" | "replay_limited")
    pub(crate) promotions: Arc<StdMutex<HashMap<String, (String, String)>>>,
    pub(crate) pending: Arc<StdMutex<HashMap<String, Arc<PendingEntry>>>>,
    subs: Subs,
    /// One naming inference at a time: bounds CPU and keeps at most one
    /// copy of the model in memory.
    naming: Arc<tokio::sync::Semaphore>,
    /// Set while (or after a failed) naming-model download, so it is
    /// attempted once per daemon lifetime.
    model_download: Arc<AtomicBool>,
    pub(crate) shutdown: CancellationToken,
}

impl Daemon {
    pub fn new(paths: Paths) -> Result<Self> {
        let store = Store::open(&paths.db_file())?;
        store.mark_orphan_runs_interrupted()?;
        Ok(Self {
            paths,
            store: Arc::new(StdMutex::new(store)),
            runs: Arc::new(StdMutex::new(HashMap::new())),
            approvals: Arc::new(StdMutex::new(HashMap::new())),
            promotions: Arc::new(StdMutex::new(HashMap::new())),
            pending: Arc::new(StdMutex::new(HashMap::new())),
            subs: Arc::new(StdMutex::new(Vec::new())),
            naming: Arc::new(tokio::sync::Semaphore::new(1)),
            model_download: Arc::new(AtomicBool::new(false)),
            shutdown: CancellationToken::new(),
        })
    }

    /// Connected clients that receive events (and can answer approvals).
    pub fn subscriber_count(&self) -> usize {
        let mut subs = self.subs.lock().unwrap();
        subs.retain(|tx| !tx.is_closed());
        subs.len()
    }

    pub(crate) fn broadcast(&self, ev: Event) {
        send_all(&self.subs, ev);
    }

    pub(crate) fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let guard = self.store.lock().map_err(|_| anyhow!("store poisoned"))?;
        f(&guard)
    }

    async fn handle(self: &Arc<Self>, req: Request) -> Result<serde_json::Value> {
        match req {
            Request::Ping => Ok(json!({"pong": true, "protocol": PROTOCOL_VERSION})),
            Request::Status => {
                let sessions = self.with_store(|s| Ok(s.list_sessions()?.len()))?;
                let auth = Config::load(&self.paths).map(|cfg| {
                    let status = crate::auth::auth_status(&self.paths, &cfg.provider);
                    json!({
                        "mode": status.mode,
                        "credential_present": status.credential_present,
                        "has_refresh_token": status.has_refresh_token,
                        "expires_at": status.expires_at,
                    })
                });
                Ok(json!({
                    "protocol": PROTOCOL_VERSION,
                    "sessions": sessions,
                    "pid": std::process::id(),
                    "auth": auth.ok(),
                }))
            }
            Request::GetSettings => Ok(Config::load(&self.paths)?.settings_summary(&self.paths)),
            Request::Shutdown => {
                let token = self.shutdown.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    token.cancel();
                });
                Ok(json!({"shutting_down": true}))
            }
            Request::ListSessions => Ok(serde_json::to_value(
                self.with_store(|s| s.list_sessions())?,
            )?),
            Request::CreateSession { name } => {
                let s = self.with_store(|st| st.create_session(name))?;
                Ok(serde_json::to_value(s)?)
            }
            Request::RenameSession { id, name } => {
                let s = self
                    .with_store(|st| st.rename_session(&id, &name))?
                    .context("session not found")?;
                self.broadcast(Event::SessionUpdated { session: s.clone() });
                Ok(serde_json::to_value(s)?)
            }
            Request::DeleteSession { id } => {
                self.stop_session_work(&id).await;
                let existed = self.with_store(|st| st.delete_session(&id))?;
                if existed {
                    self.broadcast(Event::SessionDeleted { id: id.clone() });
                }
                Ok(json!({"deleted": existed}))
            }
            Request::GetSession { id } => {
                let (session, messages, queued) = self.with_store(|st| {
                    let s = st.get_session(&id)?.context("session not found")?;
                    let m = st.list_messages(&id)?;
                    let q = st.list_queued_messages(&id)?;
                    Ok((s, m, q))
                })?;
                Ok(json!({"session": session, "messages": messages, "queued": queued}))
            }
            Request::ForkSession {
                id,
                name,
                up_to_seq,
            } => {
                let s = self
                    .with_store(|st| st.fork_session(&id, name, up_to_seq))?
                    .context("session not found")?;
                self.broadcast(Event::SessionUpdated { session: s.clone() });
                Ok(serde_json::to_value(s)?)
            }
            Request::SearchSessions { query, limit } => {
                Ok(serde_json::to_value(self.with_store(|st| {
                    st.search_sessions(&query, limit.unwrap_or(50))
                })?)?)
            }
            Request::EditMessage {
                session_id,
                message_id,
                content,
            } => {
                if self.runs.lock().unwrap().contains_key(&session_id) {
                    return Err(anyhow!(
                        "session has an active run; interrupt it before editing"
                    ));
                }
                let msg = self
                    .with_store(|st| st.edit_message(&session_id, &message_id, &content))?
                    .context("user message not found")?;
                self.broadcast(Event::MessagesEdited {
                    session_id: session_id.clone(),
                });
                self.start_run(session_id)?;
                Ok(serde_json::to_value(msg)?)
            }
            Request::SetSessionModel {
                id,
                model,
                reasoning_effort,
            } => {
                let s = self
                    .with_store(|st| st.set_session_model(&id, model, reasoning_effort))?
                    .context("session not found")?;
                Ok(serde_json::to_value(s)?)
            }
            Request::SendMessage {
                session_id,
                content,
                mode,
            } => self.send_message(session_id, content, mode).await,
            Request::ResumeRun { session_id } => {
                if self.runs.lock().unwrap().contains_key(&session_id) {
                    return Err(anyhow!("a run is already active in this session"));
                }
                let (run_id, status) = self
                    .with_store(|st| st.last_run(&session_id))?
                    .filter(|(_, s)| s == "interrupted" || s == "paused")
                    .context("nothing to resume: the last run was not interrupted or paused")?;
                self.with_store(|st| st.set_run_status(&run_id, "resumed"))?;
                let note = format!(
                    "The previous run was {status} before it finished. Continue the user's \
                     task from where it stopped; do not repeat work already done above."
                );
                let msg =
                    self.with_store(|st| st.append_message(&session_id, "system-note", &note))?;
                self.broadcast(Event::MessageAppended {
                    session_id: session_id.clone(),
                    message: msg,
                });
                self.start_run(session_id)?;
                Ok(json!({"started": true}))
            }
            Request::AbandonRun { session_id } => {
                let last = self
                    .with_store(|st| st.last_run(&session_id))?
                    .filter(|(_, s)| s == "interrupted" || s == "paused");
                let abandoned = match last {
                    Some((run_id, _)) => {
                        self.with_store(|st| st.set_run_status(&run_id, "abandoned"))?;
                        true
                    }
                    None => false,
                };
                Ok(json!({"abandoned": abandoned}))
            }
            Request::RetryProviderRequest { session_id } => {
                let runs = self.runs.lock().unwrap();
                let ctl = runs
                    .get(&session_id)
                    .context("no active run in this session")?;
                ctl.retry.notify_one();
                Ok(json!({"retrying": true}))
            }
            Request::InterruptRun { session_id } => {
                let runs = self.runs.lock().unwrap();
                if let Some(ctl) = runs.get(&session_id) {
                    ctl.cancel_flag.store(true, Ordering::SeqCst);
                    ctl.cancel.cancel();
                    Ok(json!({"interrupted": true}))
                } else {
                    Ok(json!({"interrupted": false}))
                }
            }
            Request::ListTools => Ok(serde_json::to_value(
                self.with_store(|s| s.list_tools(true))?,
            )?),
            Request::ListModules => Ok(serde_json::to_value(
                self.with_store(|s| s.list_modules())?,
            )?),
            Request::UpdateToolCode { id, code } => {
                // In-TUI editor save: a new immutable version with the same
                // schemas/capabilities/memory as the current version, run
                // through the same syntax + smoke gate as agent edits.
                let (info, ver) = self
                    .with_store(|st| st.get_tool_current(&id))?
                    .context("tool not found")?;
                let cfg = Config::load(&self.paths)?;
                let args = json!({
                    "name": info.name,
                    "description": info.description,
                    "input_schema": serde_json::from_str::<Value>(&ver.input_schema).unwrap_or_else(|_| json!({"type":"object"})),
                    "output_schema": serde_json::from_str::<Value>(&ver.output_schema).unwrap_or_else(|_| json!({"type":"object"})),
                    "implementation_js": code,
                    "capabilities": ver.capabilities,
                    "memory_mb": ver.memory_mb,
                });
                let (info, version, _) = self.create_tool_as(&args, &cfg, "user").await?;
                Ok(json!({"tool": info, "version": version}))
            }
            Request::GetTool { id } => {
                let d = self
                    .with_store(|st| st.tool_detail(&id))?
                    .context("tool not found")?;
                Ok(serde_json::to_value(d)?)
            }
            Request::RetireTool { id } => {
                let t = self
                    .with_store(|st| st.set_tool_retired(&id, true))?
                    .context("tool not found")?;
                let _ = self.with_store(|st| st.audit("user", "tool.retire", &t.name));
                self.broadcast(Event::ToolUpdated { tool: t.clone() });
                Ok(serde_json::to_value(t)?)
            }
            Request::RestoreTool { id } => {
                let t = self
                    .with_store(|st| st.set_tool_retired(&id, false))?
                    .context("tool not found")?;
                let _ = self.with_store(|st| st.audit("user", "tool.restore", &t.name));
                self.broadcast(Event::ToolUpdated { tool: t.clone() });
                Ok(serde_json::to_value(t)?)
            }
            Request::PurgeTool { id } => {
                let existed = self.with_store(|st| st.purge_tool(&id))?;
                if existed {
                    self.broadcast(Event::ToolDeleted { id: id.clone() });
                }
                Ok(json!({"purged": existed}))
            }
            Request::RollbackTool { id, version } => {
                let t = self
                    .with_store(|st| st.rollback_tool(&id, version))?
                    .context("tool/version not found")?;
                let _ = self.with_store(|st| {
                    st.audit("user", "tool.rollback", &format!("{} v{version}", t.name))
                });
                self.broadcast(Event::ToolUpdated { tool: t.clone() });
                Ok(serde_json::to_value(t)?)
            }
            Request::PromotionDecision {
                promotion_id,
                approved,
            } => {
                let pending = self.promotions.lock().unwrap().remove(&promotion_id);
                match pending {
                    Some((attempt_id, kind)) => {
                        let this = self.clone();
                        tokio::spawn(async move {
                            this.resume_promotion(attempt_id, kind, approved).await;
                        });
                        Ok(json!({"resolved": true}))
                    }
                    None => Ok(json!({"resolved": false})),
                }
            }
            Request::ExportTool { id } => {
                let bundle = self.export_tool_bundle(&id)?.context("tool not found")?;
                Ok(bundle)
            }
            Request::ExportTools => {
                let mut tools = Vec::new();
                for info in self.with_store(|st| st.list_tools(true))? {
                    if let Some(b) = self.export_tool_bundle(&info.id)? {
                        tools.push(b);
                    }
                }
                Ok(json!({"format": "formless.tools/1", "tools": tools}))
            }
            Request::ImportTool { bundle } => {
                let cfg = Config::load(&self.paths)?;
                if bundle["format"].as_str() == Some("formless.tools/1") {
                    let mut imported = Vec::new();
                    for b in bundle["tools"].as_array().cloned().unwrap_or_default() {
                        imported.push(self.import_one_tool(&b, &cfg).await?);
                    }
                    Ok(serde_json::to_value(imported)?)
                } else {
                    Ok(serde_json::to_value(
                        self.import_one_tool(&bundle, &cfg).await?,
                    )?)
                }
            }
            Request::ExportSession { id } => {
                let bundle = self.with_store(|st| {
                    let s = st.get_session(&id)?.context("session not found")?;
                    let msgs = st.list_messages(&id)?;
                    let cps = st.list_checkpoints(&id)?;
                    Ok(json!({
                        "format": "formless.session/1",
                        "name": s.name, "model": s.model, "reasoning_effort": s.reasoning_effort,
                        "messages": msgs.iter().map(|m| json!({"role": m.role, "content": m.content, "meta": m.meta})).collect::<Vec<_>>(),
                        "checkpoints": cps.iter().map(|(s2, u)| json!({"summary": s2, "up_to_seq": u})).collect::<Vec<_>>(),
                    }))
                })?;
                Ok(bundle)
            }
            Request::ImportSession { bundle } => {
                if bundle["format"].as_str() != Some("formless.session/1") {
                    return Err(anyhow!("unsupported session bundle format"));
                }
                let messages: Vec<(String, String, Option<String>)> = bundle["messages"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|m| {
                        (
                            m["role"].as_str().unwrap_or("user").to_string(),
                            m["content"].as_str().unwrap_or_default().to_string(),
                            m["meta"].as_str().map(str::to_string),
                        )
                    })
                    .collect();
                let checkpoints: Vec<(String, i64)> = bundle["checkpoints"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|c| {
                        (
                            c["summary"].as_str().unwrap_or_default().to_string(),
                            c["up_to_seq"].as_i64().unwrap_or(0),
                        )
                    })
                    .collect();
                let s = self.with_store(|st| {
                    st.import_session(
                        bundle["name"].as_str().unwrap_or("Imported"),
                        bundle["model"].as_str().map(str::to_string),
                        bundle["reasoning_effort"].as_str().map(str::to_string),
                        &messages,
                        &checkpoints,
                    )
                })?;
                Ok(serde_json::to_value(s)?)
            }
            Request::RunOptimiser { tool_id } => {
                let this = self.clone();
                tokio::spawn(async move {
                    this.run_optimiser_now(tool_id).await;
                });
                Ok(json!({"started": true}))
            }
            Request::ListProposals => Ok(serde_json::to_value(
                self.with_store(|s| s.list_proposals())?,
            )?),
            Request::ListModelStats => {
                let stats: Vec<ModelStatInfo> = self
                    .with_store(|s| s.list_model_stats())?
                    .into_iter()
                    .map(|s| ModelStatInfo {
                        model: s.model,
                        successes: s.successes,
                        failures: s.failures,
                        total_ms: s.total_ms,
                    })
                    .collect();
                Ok(serde_json::to_value(stats)?)
            }
            Request::GenerateProposal => {
                let this = self.clone();
                tokio::spawn(async move {
                    this.generate_and_measure_proposal().await;
                });
                Ok(json!({"started": true}))
            }
            Request::ProposalDecision { id, approved } => {
                let status = if approved { "applied" } else { "rejected" };
                // Applying requires a measured improvement (gate in optimiser
                // module checks scores before this status transition).
                let p = if approved {
                    self.apply_proposal_checked(&id)?
                } else {
                    self.with_store(|st| st.set_proposal_status(&id, status))?
                }
                .context("proposal not found or not in an actionable state")?;
                let _ = self
                    .with_store(|st| st.audit("user", &format!("proposal.{status}"), &p.content));
                self.broadcast(Event::ProposalUpdated {
                    proposal: p.clone(),
                });
                Ok(serde_json::to_value(p)?)
            }
            Request::RollbackProposal { id } => {
                let p = self
                    .with_store(|st| st.set_proposal_status(&id, "rolled_back"))?
                    .context("proposal not found or not applied")?;
                let _ = self.with_store(|st| st.audit("user", "proposal.rolled_back", &p.content));
                self.broadcast(Event::ProposalUpdated {
                    proposal: p.clone(),
                });
                Ok(serde_json::to_value(p)?)
            }
            Request::ApprovalDecision {
                approval_id,
                approved,
                scope,
            } => {
                // First answer wins (SPEC §10.4): removing the sender resolves it.
                let tx = self.approvals.lock().unwrap().remove(&approval_id);
                let resolved = match tx {
                    Some(tx) => tx.send((approved, scope)).is_ok(),
                    None => false,
                };
                Ok(json!({"resolved": resolved}))
            }
        }
    }

    /// Stop everything still working on a Session before it is deleted:
    /// drop its queued messages, interrupt its Run (and wait for it to
    /// finish writing), and abandon its background invocations.
    async fn stop_session_work(&self, session_id: &str) {
        let _ = self.with_store(|st| st.clear_queued_messages(session_id));
        if let Some(ctl) = self.runs.lock().unwrap().get(session_id) {
            ctl.cancel_flag.store(true, Ordering::SeqCst);
            ctl.cancel.cancel();
        }
        for entry in self.pending.lock().unwrap().values() {
            if entry.session_id == session_id {
                entry.abandoned.store(true, Ordering::SeqCst);
                entry.notify.notify_waiters();
            }
        }
        for _ in 0..100 {
            if !self.runs.lock().unwrap().contains_key(session_id) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        tracing::warn!("run for deleted session {session_id} did not stop within 5s");
    }

    /// A Run that stopped mid-batch leaves tool calls without results,
    /// which providers reject. Answer them (never re-run: the outcome is
    /// unknown, SPEC §9.5) before the next Run builds its context.
    fn close_dangling_tool_calls(&self, session_id: &str) -> Result<()> {
        for (call_id, name) in self.with_store(|st| st.dangling_tool_calls(session_id))? {
            let msg = self.with_store(|st| {
                st.append_message_meta(
                    session_id,
                    "tool",
                    "error: the run stopped before this call finished; it was not retried. \
                     Run it again if it is still needed.",
                    Some(json!({"tool_call_id": call_id, "name": name}).to_string()),
                )
            })?;
            self.broadcast(Event::MessageAppended {
                session_id: session_id.to_string(),
                message: msg,
            });
        }
        Ok(())
    }

    async fn send_message(
        self: &Arc<Self>,
        session_id: String,
        content: String,
        mode: SendMode,
    ) -> Result<serde_json::Value> {
        self.with_store(|st| st.get_session(&session_id))?
            .context("session not found")?;
        let busy = self.runs.lock().unwrap().contains_key(&session_id);
        match (busy, mode) {
            (true, SendMode::Queue) => {
                // Persisted, so a daemon restart does not lose it.
                self.with_store(|st| st.enqueue_message(&session_id, &content))?;
                Ok(json!({"queued": true}))
            }
            (true, _) => {
                // Steer (Enter) or Send-while-busy: persist + notify run (SPEC §5.3).
                let msg = self.with_store(|st| st.append_message(&session_id, "user", &content))?;
                self.broadcast(Event::MessageAppended {
                    session_id: session_id.clone(),
                    message: msg,
                });
                if let Some(ctl) = self.runs.lock().unwrap().get(&session_id) {
                    let _ = ctl.steer_tx.send(());
                }
                Ok(json!({"steered": true}))
            }
            (false, _) => {
                let msg = self.with_store(|st| st.append_message(&session_id, "user", &content))?;
                self.broadcast(Event::MessageAppended {
                    session_id: session_id.clone(),
                    message: msg,
                });
                self.start_run(session_id)?;
                Ok(json!({"started": true}))
            }
        }
    }

    fn start_run(self: &Arc<Self>, session_id: String) -> Result<()> {
        self.close_dangling_tool_calls(&session_id)?;
        let run_id = self.with_store(|st| st.create_run(&session_id))?;
        let cancel = CancellationToken::new();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (steer_tx, steer_rx) = mpsc::unbounded_channel::<()>();
        self.runs.lock().unwrap().insert(
            session_id.clone(),
            RunCtl {
                cancel: cancel.clone(),
                cancel_flag: cancel_flag.clone(),
                steer_tx,
                retry: Arc::new(tokio::sync::Notify::new()),
            },
        );
        self.broadcast(Event::RunStarted {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
        });
        let this = self.clone();
        tokio::spawn(async move {
            this.run_loop(session_id, run_id, cancel, cancel_flag, steer_rx)
                .await;
        });
        Ok(())
    }

    async fn run_loop(
        self: Arc<Self>,
        session_id: String,
        run_id: String,
        cancel: CancellationToken,
        cancel_flag: Arc<AtomicBool>,
        mut steer_rx: mpsc::UnboundedReceiver<()>,
    ) {
        let mut final_status = "completed";
        loop {
            match self
                .run_step(&session_id, &run_id, &cancel, &cancel_flag)
                .await
            {
                Ok(()) => {
                    // Steered messages continue the same Run (next step boundary).
                    let mut steered = false;
                    while steer_rx.try_recv().is_ok() {
                        steered = true;
                    }
                    if steered && !cancel.is_cancelled() {
                        continue;
                    }
                    break;
                }
                Err(e) if e.to_string().contains("interrupted") || cancel.is_cancelled() => {
                    final_status = "interrupted";
                    break;
                }
                Err(e) => {
                    final_status = "paused";
                    let _ = self.with_store(|st| st.set_run_status(&run_id, "paused"));
                    self.broadcast(Event::RunPaused {
                        session_id: session_id.clone(),
                        run_id: run_id.clone(),
                        error: e.to_string(),
                    });
                    break;
                }
            }
        }
        if final_status != "paused" {
            let _ = self.with_store(|st| st.set_run_status(&run_id, final_status));
        }
        self.runs.lock().unwrap().remove(&session_id);
        self.broadcast(Event::RunFinished {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            status: final_status.to_string(),
        });
        self.maybe_name_session(&session_id);
        // Queued messages start fresh Runs, in order (SPEC §5.3).
        let next = self
            .with_store(|st| st.pop_queued_message(&session_id))
            .ok()
            .flatten();
        if let Some(content) = next {
            if let Ok(msg) = self.with_store(|st| st.append_message(&session_id, "user", &content))
            {
                self.broadcast(Event::MessageAppended {
                    session_id: session_id.clone(),
                    message: msg,
                });
                let _ = self.start_run(session_id);
            }
        }
    }

    /// Title an "Untitled" Session from its first user message once a Run
    /// ends. Fire-and-forget: inference is CPU-bound, so it runs on the
    /// blocking pool behind the one-permit `naming` semaphore and a
    /// timeout, and nothing awaits it, so Runs and requests never wait on
    /// the model. The result lands as a SessionUpdated event.
    fn maybe_name_session(self: &Arc<Self>, session_id: &str) {
        let Ok(cfg) = Config::load(&self.paths) else {
            return;
        };
        if !cfg.naming.enabled {
            return;
        }
        let first_user = self.with_store(|st| {
            if !st
                .get_session(session_id)?
                .is_some_and(|s| s.name == DEFAULT_SESSION_NAME)
            {
                return Ok(None);
            }
            Ok(st
                .list_messages(session_id)?
                .into_iter()
                .find(|m| m.role == "user" && !m.content.trim().is_empty())
                .map(|m| m.content))
        });
        let Ok(Some(user)) = first_user else {
            return;
        };
        let spec = cfg.naming.spec();
        if let Some(spec) = spec {
            if cfg.naming.auto_download
                && namer::embedded_available()
                && !spec.installed(&self.paths)
            {
                self.spawn_model_download(spec);
            }
        }
        let timeout = std::time::Duration::from_secs(cfg.naming.timeout_secs.max(1));
        let this = self.clone();
        let session_id = session_id.to_string();
        tokio::spawn(async move {
            let Ok(permit) = this.naming.clone().acquire_owned().await else {
                return;
            };
            let fallback = namer::heuristic_title(&user);
            let paths = this.paths.clone();
            // The permit moves into the blocking job: a timed-out job keeps
            // it until it really finishes, so jobs never pile up.
            let job = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                namer::title(&paths, spec.as_ref(), &user)
            });
            let name = match tokio::time::timeout(timeout, job).await {
                Ok(Ok(name)) => name,
                Ok(Err(e)) => {
                    tracing::warn!("session naming panicked: {e}");
                    fallback
                }
                Err(_) => {
                    tracing::warn!("session naming timed out after {timeout:?}");
                    fallback
                }
            };
            let Some(name) = name else {
                return;
            };
            if let Ok(Some(s)) = this.with_store(|st| st.auto_name_session(&session_id, &name)) {
                this.broadcast(Event::SessionUpdated { session: s });
            }
        });
    }

    fn spawn_model_download(self: &Arc<Self>, spec: ModelSpec) {
        if self.model_download.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            tracing::info!(
                "downloading session-naming model {} ({} MB)",
                spec.id,
                spec.gguf_bytes / 1_000_000
            );
            match namer::download(&this.paths, &spec).await {
                Ok(()) => {
                    tracing::info!("session-naming model {} installed", spec.id);
                    this.model_download.store(false, Ordering::SeqCst);
                }
                // Stays set: no retry storm on every new Session.
                Err(e) => tracing::warn!("session-naming model download failed: {e:#}"),
            }
        });
    }

    async fn run_step(
        self: &Arc<Self>,
        session_id: &str,
        run_id: &str,
        cancel: &CancellationToken,
        cancel_flag: &Arc<AtomicBool>,
    ) -> Result<()> {
        let cfg = Config::load(&self.paths)?;
        if !cfg.provider.is_configured() {
            return Err(anyhow!(
                "provider not configured; run the onboarding wizard"
            ));
        }
        let provider = Provider::for_profile(cfg.provider.clone(), self.paths.clone()).await?;
        let session = self
            .with_store(|st| st.get_session(session_id))?
            .context("session not found")?;
        let requested_model = session
            .model
            .clone()
            .unwrap_or_else(|| cfg.provider.model.clone());
        let (context_chars, tool_count) = self.with_store(|st| {
            let chars = st
                .list_messages(session_id)
                .map(|msgs| {
                    msgs.iter()
                        .map(|m| m.content.chars().count())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            let tools = toolreg::tool_definitions(st, Some(session_id), cfg.tools.inline_limit)
                .map(|d| d.len())
                .unwrap_or(0);
            anyhow::Ok((chars, tools))
        })?;
        let complex = crate::routing::is_complex_task(
            context_chars,
            tool_count,
            cfg.provider.routing_complex_chars,
        );
        let model = self.resolve_model(&requested_model, &cfg, complex)?;
        if ProviderCfg::is_auto_model(&requested_model) {
            self.broadcast(Event::Note {
                session_id: Some(session_id.to_string()),
                text: format!("Routed to model {model} (measured auto-selection)."),
            });
        }
        let effort = session
            .reasoning_effort
            .clone()
            .or_else(|| cfg.provider.reasoning_effort.clone());
        let context_tokens = cfg.provider.context_for(&model);

        self.maybe_compact(&provider, &cfg, session_id, &model, context_tokens)
            .await?;

        // Agent loop: provider turn -> tool calls -> tool results -> repeat
        // until a final text answer (SPEC §5.2). Context is rebuilt from the
        // DB each iteration, so steered messages join at step boundaries.
        let mut reminded_pending = false;
        loop {
            if cancel.is_cancelled() {
                return Err(anyhow!("interrupted"));
            }
            // Push delivery of completed Pending Invocations (§6.3).
            self.deliver_completed_pending(session_id);
            let prompt = self.effective_system_prompt();
            let ctx = self.with_store(|st| {
                build_context(
                    st,
                    session_id,
                    &prompt,
                    context_tokens,
                    cfg.compaction.fraction(),
                )
                .map(|(c, _, _)| c)
            })?;
            let defs = self.with_store(|st| {
                toolreg::tool_definitions(st, Some(session_id), cfg.tools.inline_limit)
            })?;
            let retry = self
                .runs
                .lock()
                .unwrap()
                .get(session_id)
                .map(|c| c.retry.clone())
                .unwrap_or_default();
            let last_byte = Arc::new(std::sync::atomic::AtomicU64::new(epoch_ms()));
            let got_bytes = Arc::new(AtomicBool::new(false));
            let started = std::time::Instant::now();
            let mut auto_resends = 0;
            let outcome = 'request: loop {
                let (sid, rid) = (session_id.to_string(), run_id.to_string());
                let subs = self.subs.clone();
                let (last_byte2, got_bytes2) = (last_byte.clone(), got_bytes.clone());
                last_byte.store(epoch_ms(), Ordering::SeqCst);
                got_bytes.store(false, Ordering::SeqCst);
                let chat_fut = provider.chat_stream(
                    &ctx,
                    &defs,
                    &model,
                    effort.as_deref(),
                    cancel,
                    move |kind, delta| {
                        last_byte2.store(epoch_ms(), Ordering::SeqCst);
                        got_bytes2.store(true, Ordering::SeqCst);
                        let ev = match kind {
                            DeltaKind::Bytes => return,
                            DeltaKind::Content => Event::MessageDelta {
                                session_id: sid.clone(),
                                run_id: rid.clone(),
                                delta: delta.to_string(),
                            },
                            DeltaKind::Reasoning => Event::ReasoningDelta {
                                session_id: sid.clone(),
                                run_id: rid.clone(),
                                delta: delta.to_string(),
                            },
                            DeltaKind::Restart => Event::StreamRestarted {
                                session_id: sid.clone(),
                                run_id: rid.clone(),
                            },
                        };
                        send_all(&subs, ev);
                    },
                );
                let mut chat_fut = std::pin::pin!(chat_fut);
                let sent_at = tokio::time::Instant::now();
                let mut pending_noted = false;
                let mut stall_noted = false;
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    tokio::select! {
                        r = &mut chat_fut => break 'request r,
                        _ = retry.notified() => {
                            self.broadcast(Event::StreamRestarted {
                                session_id: session_id.to_string(),
                                run_id: run_id.to_string(),
                            });
                            continue 'request;
                        }
                        _ = tick.tick() => {}
                    }
                    let waited = sent_at.elapsed().as_secs();
                    if !pending_noted && waited >= cfg.tools.pending_threshold_secs {
                        pending_noted = true;
                        self.broadcast(Event::Note {
                            session_id: Some(session_id.to_string()),
                            text: "Provider request still pending; waiting (not terminated)."
                                .into(),
                        });
                    }
                    let first_byte = cfg.provider.first_byte_timeout_secs;
                    if first_byte > 0
                        && !got_bytes.load(Ordering::SeqCst)
                        && waited >= first_byte
                        && auto_resends < cfg.provider.max_retries
                    {
                        auto_resends += 1;
                        self.broadcast(Event::Note {
                            session_id: Some(session_id.to_string()),
                            text: format!("No response from the provider in {first_byte}s; re-sending the request."),
                        });
                        continue 'request;
                    }
                    // Stall notice (SPEC §6.1 spirit: advisory, never auto-kill).
                    let idle = epoch_ms().saturating_sub(last_byte.load(Ordering::SeqCst)) / 1000;
                    let notice = cfg.provider.stall_notice_secs;
                    if notice > 0 && idle >= notice && !stall_noted {
                        stall_noted = true;
                        self.broadcast(Event::ProviderStalled {
                            session_id: session_id.to_string(),
                            run_id: run_id.to_string(),
                            idle_secs: idle,
                        });
                    } else if idle < notice {
                        stall_noted = false;
                    }
                }
            };
            let result = match outcome {
                Ok(r) => {
                    self.record_model_outcome(&r.model_used, true, started);
                    r
                }
                Err(e) => {
                    if !cancel.is_cancelled() {
                        self.record_model_outcome(&model, false, started);
                    }
                    return Err(e);
                }
            };
            if result.dropped_reasoning_effort {
                self.broadcast(Event::Note {
                    session_id: Some(session_id.to_string()),
                    text: "Provider rejected reasoning_effort; retried without it.".into(),
                });
            }
            if result.tool_calls.is_empty() {
                // Pending handles still live: remind once, then accept the
                // next final answer and mark the rest abandoned (§6.3).
                self.deliver_completed_pending(session_id);
                let live = self.live_pending(session_id);
                if !live.is_empty() {
                    if !reminded_pending {
                        reminded_pending = true;
                        let handles: Vec<String> =
                            live.iter().map(|(h, n)| format!("{n} ({h})")).collect();
                        let note = format!(
                            "You still have pending invocations running: {}. Use await_invocation/check_invocation to get their results, or abandon_invocation to drop them, before giving a final answer.",
                            handles.join(", ")
                        );
                        let msg = self
                            .with_store(|st| st.append_message(session_id, "system-note", &note))?;
                        self.broadcast(Event::MessageAppended {
                            session_id: session_id.to_string(),
                            message: msg,
                        });
                        continue;
                    }
                    for (handle, _n) in &live {
                        if let Some(entry) = self.pending_get(handle) {
                            entry.abandoned.store(true, Ordering::SeqCst);
                        }
                    }
                }
                let msg = self
                    .with_store(|st| st.append_message(session_id, "assistant", &result.content))?;
                self.broadcast(Event::MessageComplete {
                    session_id: session_id.to_string(),
                    run_id: run_id.to_string(),
                    message: msg,
                });
                if result.truncated {
                    self.broadcast(Event::Note {
                        session_id: Some(session_id.to_string()),
                        text: "The answer was cut off at the model's output token limit.".into(),
                    });
                }
                return Ok(());
            }
            // Persist the assistant tool-call turn, execute calls, persist results.
            let calls_json: Vec<Value> = result
                .tool_calls
                .iter()
                .map(|tc| {
                    json!({"id": tc.id, "type": "function", "function": {"name": tc.name, "arguments": tc.arguments}})
                })
                .collect();
            // Reasoning is kept only as provider metadata on tool-call turns
            // (thinking-mode endpoints require it echoed back; §10.6) —
            // it is never stored as the assistant's answer content.
            let mut meta = json!({"tool_calls": calls_json});
            if !result.reasoning.is_empty() {
                meta["reasoning_content"] = json!(result.reasoning);
            }
            let call_msg = self.with_store(|st| {
                st.append_message_meta(
                    session_id,
                    "assistant",
                    &result.content,
                    Some(meta.to_string()),
                )
            })?;
            // Clients render the tool call(s) inline from this message.
            self.broadcast(Event::MessageAppended {
                session_id: session_id.to_string(),
                message: call_msg,
            });
            if result.truncated {
                // The arguments are cut off: running them (or treating them as
                // a malformed call) would only repeat the failure. Say why.
                for tc in &result.tool_calls {
                    let msg = self.with_store(|st| {
                        st.append_message_meta(
                            session_id,
                            "tool",
                            &truncated_call_message(&tc.name),
                            Some(json!({"tool_call_id": tc.id, "name": tc.name}).to_string()),
                        )
                    })?;
                    self.broadcast(Event::MessageAppended {
                        session_id: session_id.to_string(),
                        message: msg,
                    });
                }
                continue;
            }
            let denied = self
                .approve_calls(&cfg, session_id, run_id, &result.tool_calls, cancel)
                .await?;
            // Execute the batch in parallel (isolated sandboxes).
            let mut futs = Vec::new();
            for tc in &result.tool_calls {
                let this = self.clone();
                let (sid, rid) = (session_id.to_string(), run_id.to_string());
                let tc = tc.clone();
                let cfg2 = cfg.clone();
                let flag = cancel_flag.clone();
                let tok = cancel.clone();
                let denied = denied.clone();
                futs.push(async move {
                    let text = this
                        .dispatch_call(&sid, &rid, &tc, &cfg2, &flag, &tok, &denied)
                        .await;
                    (tc, text)
                });
            }
            let results = futures_util::future::join_all(futs).await;
            for (tc, text) in results {
                let msg = self.with_store(|st| {
                    st.append_message_meta(
                        session_id,
                        "tool",
                        &text,
                        Some(json!({"tool_call_id": tc.id, "name": tc.name}).to_string()),
                    )
                })?;
                self.broadcast(Event::MessageAppended {
                    session_id: session_id.to_string(),
                    message: msg,
                });
            }
        }
    }

    /// Approval pre-pass (SPEC §5.2/§10.3): one prompt per unapproved
    /// sensitive registry Tool call in a batch, before anything runs.
    /// Keyed by call id: input-bound scopes differ per call. Every path that
    /// dispatches model tool calls (main loop, sub-agents, direct calls)
    /// goes through here, because dispatch itself treats calls as approved.
    async fn approve_calls(
        self: &Arc<Self>,
        cfg: &Config,
        session_id: &str,
        run_id: &str,
        calls: &[ToolCall],
        cancel: &CancellationToken,
    ) -> Result<HashMap<String, bool>> {
        let mut denied: HashMap<String, bool> = HashMap::new();
        for tc in calls {
            if toolreg::is_builtin(&tc.name) {
                continue;
            }
            if let Some((info, ver)) = self.with_store(|st| st.get_tool_by_name(&tc.name))? {
                // Unbindable input fails at execution with a clear error.
                let Ok(caps) = serde_json::from_str::<Value>(&tc.arguments)
                    .map_err(|e| e.to_string())
                    .and_then(|input| bound_caps(&ver, &input))
                else {
                    continue;
                };
                if !self
                    .ensure_approval(cfg, session_id, &info, &ver, &caps, cancel)
                    .await
                {
                    denied.insert(tc.id.clone(), true);
                    self.record_inv(
                        &info,
                        ver.version,
                        session_id,
                        run_id,
                        None,
                        "denied",
                        "approval",
                        &tc.arguments,
                        "",
                        "approval denied",
                        0,
                        cfg,
                    );
                }
            }
        }
        Ok(denied)
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_call(
        self: &Arc<Self>,
        session_id: &str,
        run_id: &str,
        tc: &ToolCall,
        cfg: &Config,
        cancel_flag: &Arc<AtomicBool>,
        cancel: &CancellationToken,
        denied: &HashMap<String, bool>,
    ) -> String {
        self.dispatch_call_inner(session_id, run_id, tc, cfg, cancel_flag, cancel, denied, 0)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_call_inner(
        self: &Arc<Self>,
        session_id: &str,
        run_id: &str,
        tc: &ToolCall,
        cfg: &Config,
        cancel_flag: &Arc<AtomicBool>,
        cancel: &CancellationToken,
        denied: &HashMap<String, bool>,
        agent_depth: usize,
    ) -> String {
        let args: Value = match serde_json::from_str(&tc.arguments) {
            Ok(v) => v,
            Err(e) => {
                // Malformed model tool-call: feed back for correction, never
                // counted against any Tool (SPEC §4.4).
                return format!(
                    "error: invalid JSON arguments for {}: {e}. Retry with valid JSON.",
                    tc.name
                );
            }
        };
        // Bootstrap/meta tools don't pass through execute_tool_value, so
        // broadcast their lifecycle here (registry Tools broadcast there).
        let bootstrap = toolreg::is_builtin(&tc.name);
        let started = std::time::Instant::now();
        if bootstrap {
            self.broadcast(Event::ToolCallStarted {
                session_id: session_id.to_string(),
                run_id: run_id.to_string(),
                tool_name: tc.name.clone(),
                version: 0,
                arguments: tc.arguments.clone(),
            });
        }
        let text = match tc.name.as_str() {
            toolreg::CREATE_TOOL => match self.create_tool_impl(&args, cfg).await {
                Ok((text, tool_id)) => {
                    let _ = self
                        .with_store(|st| st.load_session_tools(session_id, &[tool_id], "created"));
                    text
                }
                Err(e) => format!("error: create_tool failed: {e}"),
            },
            toolreg::FIND_TOOLS => match self.find_tools_impl(session_id, &args) {
                Ok(text) => text,
                Err(e) => format!("error: find_tools failed: {e}"),
            },
            toolreg::READ_TOOL => match self.read_tool_impl(&args) {
                Ok(text) => text,
                Err(e) => format!("error: read_tool failed: {e}"),
            },
            toolreg::TEST_TOOL => match self
                .test_tool_impl(
                    session_id,
                    run_id,
                    &args,
                    cfg,
                    cancel_flag,
                    cancel,
                    agent_depth,
                )
                .await
            {
                Ok(text) => text,
                Err(e) => format!("error: test_tool failed: {e}"),
            },
            toolreg::CREATE_MODULE => match self.create_module_impl(&args, cfg).await {
                Ok(text) => text,
                Err(e) => format!("error: create_module failed: {e}"),
            },
            toolreg::RETIRE_TOOL => {
                let name = args["name"].as_str().unwrap_or_default();
                let reason = args["reason"].as_str().unwrap_or_default();
                match self.with_store(|st| st.get_tool_by_name(name)) {
                    Ok(Some((info, _))) => {
                        let _ = self.with_store(|st| st.set_tool_retired(&info.id, true));
                        let _ = self.with_store(|st| {
                            st.audit("agent", "tool.retire", &format!("{name}: {reason}"))
                        });
                        if let Ok(Some(t)) = self
                            .with_store(|st| st.get_tool_current(&info.id))
                            .map(|o| o.map(|(i, _)| i))
                        {
                            self.broadcast(Event::ToolUpdated { tool: t });
                        }
                        json!({"retired": name, "reason": reason}).to_string()
                    }
                    _ => format!("error: no tool named '{name}'"),
                }
            }
            toolreg::AWAIT_INVOCATION => {
                let handle = args["invocation_id"].as_str().unwrap_or_default();
                match self.pending_get(handle) {
                    Some(entry) => match self.wait_pending(&entry, None, cancel).await {
                        Some((_ok, text)) => {
                            entry.delivered.store(true, Ordering::SeqCst);
                            self.pending.lock().unwrap().remove(handle);
                            text
                        }
                        None => "error: interrupted while awaiting invocation".to_string(),
                    },
                    None => format!("error: unknown invocation handle '{handle}'"),
                }
            }
            toolreg::CHECK_INVOCATION => {
                let handle = args["invocation_id"].as_str().unwrap_or_default();
                match self.pending_get(handle) {
                    Some(entry) => {
                        let done = entry.result.lock().unwrap().clone();
                        match done {
                            Some((_ok, text)) => {
                                entry.delivered.store(true, Ordering::SeqCst);
                                self.pending.lock().unwrap().remove(handle);
                                json!({"status": "completed", "result": text}).to_string()
                            }
                            None => {
                                json!({"status": "pending", "invocation_id": handle}).to_string()
                            }
                        }
                    }
                    None => format!("error: unknown invocation handle '{handle}'"),
                }
            }
            toolreg::ABANDON_INVOCATION => {
                let handle = args["invocation_id"].as_str().unwrap_or_default();
                match self.pending_get(handle) {
                    Some(entry) => {
                        entry.abandoned.store(true, Ordering::SeqCst);
                        json!({"abandoned": handle}).to_string()
                    }
                    None => format!("error: unknown invocation handle '{handle}'"),
                }
            }
            name => {
                // Denied in the approval pre-pass?
                if denied.contains_key(&tc.id) {
                    return format!("error: user denied approval for tool '{name}'");
                }
                self.dispatch_pending(
                    session_id,
                    run_id,
                    name,
                    args,
                    cfg,
                    cancel_flag,
                    cancel,
                    agent_depth,
                )
                .await
            }
        };
        if bootstrap {
            self.broadcast(Event::ToolCallFinished {
                session_id: session_id.to_string(),
                run_id: run_id.to_string(),
                tool_name: tc.name.clone(),
                ok: !text.starts_with("error:"),
                elapsed_ms: started.elapsed().as_millis() as i64,
            });
        }
        text
    }

    /// Run one built-in or registry Tool call outside a model turn, as the
    /// agent would (approvals fail closed without a client).
    pub async fn call_tool_direct(
        self: &Arc<Self>,
        session_id: &str,
        name: &str,
        args: Value,
    ) -> String {
        let cfg = match Config::load(&self.paths) {
            Ok(c) => c,
            Err(e) => return format!("error: {e}"),
        };
        let tc = ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        };
        let cancel = CancellationToken::new();
        let denied = match self
            .approve_calls(
                &cfg,
                session_id,
                "direct",
                std::slice::from_ref(&tc),
                &cancel,
            )
            .await
        {
            Ok(d) => d,
            Err(e) => return format!("error: {e}"),
        };
        self.dispatch_call_inner(
            session_id,
            "direct",
            &tc,
            &cfg,
            &Arc::new(AtomicBool::new(false)),
            &cancel,
            &denied,
            0,
        )
        .await
    }

    fn pending_get(&self, handle: &str) -> Option<Arc<PendingEntry>> {
        self.pending.lock().unwrap().get(handle).cloned()
    }

    async fn wait_pending(
        &self,
        entry: &Arc<PendingEntry>,
        timeout: Option<std::time::Duration>,
        cancel: &CancellationToken,
    ) -> Option<(bool, String)> {
        let wait = async {
            loop {
                if let Some(r) = entry.result.lock().unwrap().clone() {
                    return r;
                }
                entry.notify.notified().await;
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => None,
            r = async {
                match timeout {
                    Some(d) => tokio::time::timeout(d, wait).await.ok(),
                    None => Some(wait.await),
                }
            } => r,
        }
    }

    /// Run a registry Tool with the soft pending threshold (SPEC §6.2):
    /// completes inline under the threshold, else hands off a handle.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_pending(
        self: &Arc<Self>,
        session_id: &str,
        run_id: &str,
        name: &str,
        args: Value,
        cfg: &Config,
        cancel_flag: &Arc<AtomicBool>,
        cancel: &CancellationToken,
        agent_depth: usize,
    ) -> String {
        let handle = uuid::Uuid::new_v4().to_string();
        let entry = Arc::new(PendingEntry {
            tool_name: name.to_string(),
            session_id: session_id.to_string(),
            result: StdMutex::new(None),
            delivered: AtomicBool::new(false),
            abandoned: AtomicBool::new(false),
            stalled_noted: AtomicBool::new(false),
            activity: Arc::new(std::sync::atomic::AtomicU64::new(epoch_ms())),
            notify: tokio::sync::Notify::new(),
        });
        self.pending
            .lock()
            .unwrap()
            .insert(handle.clone(), entry.clone());
        {
            let this = self.clone();
            let entry2 = entry.clone();
            let handle2 = handle.clone();
            let (sid, rid, nm) = (session_id.to_string(), run_id.to_string(), name.to_string());
            let activity = entry.activity.clone();
            let (flag, tok, cfg2) = (cancel_flag.clone(), cancel.clone(), cfg.clone());
            tokio::spawn(async move {
                let res = this
                    .clone()
                    .execute_tool_at(
                        sid.clone(),
                        rid.clone(),
                        nm.clone(),
                        args,
                        0,
                        None,
                        flag,
                        Some(activity),
                        tok,
                        cfg2.clone(),
                        true,
                        agent_depth,
                    )
                    .await;
                let (ok, text) = match res {
                    Ok((value, inv_id)) => {
                        if entry2.abandoned.load(Ordering::SeqCst) {
                            let _ =
                                this.with_store(|st| st.set_invocation_tag(&inv_id, "abandoned"));
                        }
                        (true, this.model_text(&sid, &value, &cfg2))
                    }
                    Err(e) => (false, format!("error: {e}")),
                };
                *entry2.result.lock().unwrap() = Some((ok, text));
                entry2.notify.notify_waiters();
                this.broadcast(Event::InvocationCompleted {
                    session_id: sid,
                    handle: handle2.clone(),
                    tool_name: nm,
                    ok,
                });
                if entry2.abandoned.load(Ordering::SeqCst) {
                    this.pending.lock().unwrap().remove(&handle2);
                }
            });
        }
        let threshold = std::time::Duration::from_secs(cfg.tools.pending_threshold_secs);
        match self.wait_pending(&entry, Some(threshold), cancel).await {
            Some((_ok, text)) => {
                entry.delivered.store(true, Ordering::SeqCst);
                self.pending.lock().unwrap().remove(&handle);
                text
            }
            None => {
                self.broadcast(Event::InvocationPending {
                    session_id: session_id.to_string(),
                    run_id: run_id.to_string(),
                    handle: handle.clone(),
                    tool_name: name.to_string(),
                });
                json!({
                    "status": "pending",
                    "invocation_id": handle,
                    "tool": name,
                    "note": format!("still running after {}s; not terminated. Use await_invocation to wait, check_invocation to poll, abandon_invocation to drop it, or continue other work - the result will also be delivered when it completes.", cfg.tools.pending_threshold_secs)
                })
                .to_string()
            }
        }
    }

    /// Push delivery (SPEC §6.3): completed, undelivered pending results for
    /// this Session become system-note messages at the step boundary.
    fn deliver_completed_pending(&self, session_id: &str) {
        let ready: Vec<(String, Arc<PendingEntry>, String)> = {
            let map = self.pending.lock().unwrap();
            map.iter()
                .filter(|(_, e)| {
                    e.session_id == session_id
                        && !e.delivered.load(Ordering::SeqCst)
                        && !e.abandoned.load(Ordering::SeqCst)
                })
                .filter_map(|(h, e)| {
                    e.result
                        .lock()
                        .unwrap()
                        .clone()
                        .map(|(_, text)| (h.clone(), e.clone(), text))
                })
                .collect()
        };
        for (handle, entry, text) in ready {
            let content = format!(
                "[Pending invocation {handle} (tool {}) completed] {text}",
                entry.tool_name
            );
            if let Ok(msg) =
                self.with_store(|st| st.append_message(session_id, "system-note", &content))
            {
                self.broadcast(Event::MessageAppended {
                    session_id: session_id.to_string(),
                    message: msg,
                });
            }
            entry.delivered.store(true, Ordering::SeqCst);
            self.pending.lock().unwrap().remove(&handle);
        }
    }

    fn live_pending(&self, session_id: &str) -> Vec<(String, String)> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| {
                e.session_id == session_id
                    && e.result.lock().unwrap().is_none()
                    && !e.delivered.load(Ordering::SeqCst)
                    && !e.abandoned.load(Ordering::SeqCst)
            })
            .map(|(h, e)| (h.clone(), e.tool_name.clone()))
            .collect()
    }

    fn export_tool_bundle(&self, id: &str) -> Result<Option<Value>> {
        let Some((info, ver)) = self.with_store(|st| st.get_tool_current(id))? else {
            return Ok(None);
        };
        Ok(Some(json!({
            "format": "formless.tool/1",
            "name": info.name,
            "description": info.description,
            "input_schema": serde_json::from_str::<Value>(&ver.input_schema).unwrap_or(json!({})),
            "output_schema": serde_json::from_str::<Value>(&ver.output_schema).unwrap_or(json!({})),
            "implementation_js": ver.code,
            "capabilities": ver.capabilities,
            "memory_mb": ver.memory_mb,
        })))
    }

    async fn import_one_tool(self: &Arc<Self>, bundle: &Value, cfg: &Config) -> Result<ToolInfo> {
        if bundle["format"].as_str() != Some("formless.tool/1") {
            return Err(anyhow!("unsupported tool bundle format"));
        }
        let base = bundle["name"]
            .as_str()
            .context("bundle name required")?
            .to_string();
        toolreg::validate_name(&base)?;
        // Collision imports as a NEW tool, never overwrites versions (§7.8).
        let mut name = base.clone();
        if self.with_store(|st| st.get_tool_by_name(&name))?.is_some() {
            name = format!("{base}_imported");
            let mut n = 2;
            while self.with_store(|st| st.get_tool_by_name(&name))?.is_some() {
                name = format!("{base}_imported{n}");
                n += 1;
            }
            toolreg::validate_name(&name)?;
        }
        let mut args = bundle.clone();
        args["name"] = json!(name);
        let (info, _v, _) = self.create_tool_as(&args, cfg, "import").await?;
        Ok(info)
    }

    /// Resolve a requested model name, expanding "auto" via measured
    /// routing (recorded successes/failures/latency). Errors only when
    /// auto is requested without any configured candidates.
    pub(crate) fn resolve_model(
        &self,
        requested: &str,
        cfg: &Config,
        complex: bool,
    ) -> Result<String> {
        if !ProviderCfg::is_auto_model(requested) {
            return Ok(requested.to_string());
        }
        let stats = self
            .with_store(|st| st.list_model_stats())
            .unwrap_or_default();
        crate::routing::select_model(&cfg.provider, &stats, complex).context(
            "provider.model is \"auto\" but no routing candidates are configured; set provider.routing_models (or fallback_models)",
        )
    }

    pub(crate) fn record_model_outcome(&self, model: &str, ok: bool, started: std::time::Instant) {
        let ms = started.elapsed().as_millis() as i64;
        let _ = self.with_store(|st| st.record_model_outcome(model, ok, ms));
    }

    pub(crate) fn effective_system_prompt(&self) -> String {
        self.with_store(|st| st.effective_system_prompt(SYSTEM_PROMPT))
            .unwrap_or_else(|_| SYSTEM_PROMPT.to_string())
    }

    fn module_code(&self) -> Vec<(String, String)> {
        self.with_store(|st| st.list_module_code())
            .unwrap_or_default()
    }

    async fn create_module_impl(self: &Arc<Self>, args: &Value, cfg: &Config) -> Result<String> {
        let name = args["name"]
            .as_str()
            .context("create_module: name required")?;
        toolreg::validate_name(name)?;
        let description = args["description"].as_str().unwrap_or_default().to_string();
        let code = args["implementation_js"]
            .as_str()
            .context("implementation_js required")?
            .to_string();
        formless_runtime::check_syntax(&format!("const module = {{exports:{{}}}};\n{code}"))
            .map_err(|e| anyhow!("syntax check failed: {e}"))?;
        // Smoke: actually load the candidate through requireModule.
        let mut modules = self.module_code();
        modules.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        modules.push((name.to_string(), code.clone()));
        let probe = format!(
            "function run() {{ const m = requireModule({}); return {{ ok: typeof m }}; }}",
            serde_json::to_string(name)?
        );
        let req = ExecRequest {
            code: probe,
            input: json!({}),
            caps: Capabilities::default(),
            memory_mb: cfg.tools.default_memory_mb,
            mode: HostMode::Smoke,
            overlay: None,
            activity: None,
            modules,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let smoke = tokio::task::spawn_blocking(move || {
            formless_runtime::execute(&req, Arc::new(formless_runtime::NoHostOps))
        })
        .await
        .map_err(|e| anyhow!("module smoke test panicked: {e}"))?;
        if let Err(e) = smoke {
            if !e.after_stub {
                return Err(anyhow!("module smoke test failed: {}", e.message));
            }
        }
        let (info, version) =
            self.with_store(|st| st.create_module_version(name, &description, &code, "agent"))?;
        let _ =
            self.with_store(|st| st.audit("agent", "module.create", &format!("{name} v{version}")));
        self.broadcast(Event::ModuleUpdated { module: info });
        Ok(json!({"created_module": name, "version": version}).to_string())
    }

    /// Run a sub-agent for JS `agents.run` (the `agent` Capability): its own
    /// provider loop over the same Tools, one level deeper (`agent_depth`),
    /// returning its final text.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_subagent_text<'a>(
        self: &'a Arc<Self>,
        session_id: &'a str,
        run_id: &'a str,
        task: &'a str,
        system: Option<&'a str>,
        model: Option<&'a str>,
        cfg: &'a Config,
        cancel_flag: &'a Arc<AtomicBool>,
        cancel: &'a CancellationToken,
        agent_depth: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            if task.trim().is_empty() {
                return Err(anyhow!("sub-agent task is empty"));
            }
            let provider = Provider::for_profile(cfg.provider.clone(), self.paths.clone()).await?;
            let requested = model
                .map(str::to_string)
                .unwrap_or_else(|| cfg.provider.model.clone());
            let sub_defs = || -> Result<Vec<Value>> {
                self.with_store(|st| {
                    toolreg::tool_definitions(st, Some(session_id), cfg.tools.inline_limit)
                })
            };
            let tool_count = sub_defs()?.len();
            let model = self
                .resolve_model(
                    &requested,
                    cfg,
                    crate::routing::is_complex_task(
                        task.chars().count(),
                        tool_count,
                        cfg.provider.routing_complex_chars,
                    ),
                )
                .map_err(|e| anyhow!("{e}"))?;
            let mut messages = vec![json!({
                "role": "system",
                "content": "You are a Formless sub-agent. Complete the single task you are given, building and using Tools as needed, then return a concise final result."
            })];
            if let Some(sys) = system {
                messages.push(json!({"role": "system", "content": sys}));
            }
            messages.push(json!({"role": "user", "content": task}));
            // No step limit (SPEC principle 3): it ends on a final answer,
            // a fault, or cancellation.
            loop {
                if cancel.is_cancelled() {
                    return Err(anyhow!("interrupted"));
                }
                // Rebuilt each step: find_tools/create_tool load new Tools.
                let defs = sub_defs()?;
                let result = provider
                    .chat_stream(
                        &messages,
                        &defs,
                        &model,
                        cfg.provider.reasoning_effort.as_deref(),
                        cancel,
                        |_, _| {},
                    )
                    .await?;
                if result.tool_calls.is_empty() {
                    return Ok(result.content);
                }
                let calls_json: Vec<Value> = result
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({"id": c.id, "type": "function",
                           "function": {"name": c.name, "arguments": c.arguments}})
                    })
                    .collect();
                let mut assistant =
                    json!({"role": "assistant", "content": Value::Null, "tool_calls": calls_json});
                if !result.reasoning.is_empty() {
                    assistant["reasoning_content"] = json!(result.reasoning);
                }
                messages.push(assistant);
                let denied = self
                    .approve_calls(cfg, session_id, run_id, &result.tool_calls, cancel)
                    .await?;
                for tc in &result.tool_calls {
                    let text = self
                        .dispatch_call_inner(
                            session_id,
                            run_id,
                            tc,
                            cfg,
                            cancel_flag,
                            cancel,
                            &denied,
                            agent_depth,
                        )
                        .await;
                    messages.push(json!({"role": "tool", "tool_call_id": tc.id, "content": text}));
                }
            }
        })
    }

    async fn create_tool_impl(
        self: &Arc<Self>,
        args: &Value,
        cfg: &Config,
    ) -> Result<(String, String)> {
        let (info, version, smoke_note) = self.create_tool_as(args, cfg, "agent").await?;
        let mut out = json!({"created": info.name, "version": version});
        if let Some(note) = smoke_note {
            out["smoke_test"] = json!(note);
        }
        Ok((out.to_string(), info.id))
    }

    /// Create a Tool version through the validation gate (SPEC §7.1).
    /// Returns a note when the smoke test was inconclusive: the Tool failed
    /// only after a stubbed effect (fake fetch body, unbound input scope,
    /// ...), which says more about the stub than about the Tool.
    async fn create_tool_as(
        self: &Arc<Self>,
        args: &Value,
        cfg: &Config,
        author: &str,
    ) -> Result<(ToolInfo, i64, Option<String>)> {
        let d = ToolDraft::parse(args, cfg, true)?;
        // Smoke test in the stubbed sandbox with a schema-minimal input.
        // Input-bound scopes stay as templates: the runtime stubs them.
        let req = ExecRequest {
            code: d.code.clone(),
            input: toolreg::minimal_input(&d.input_schema),
            caps: d.caps.clone(),
            memory_mb: d.memory_mb,
            mode: HostMode::Smoke,
            overlay: None,
            activity: None,
            modules: self.module_code(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let smoke = tokio::task::spawn_blocking(move || {
            formless_runtime::execute(&req, Arc::new(formless_runtime::NoHostOps))
        })
        .await
        .map_err(|e| anyhow!("smoke test panicked: {e}"))?;
        let smoke_note = match smoke {
            Ok(_) => None,
            Err(e) if e.after_stub && e.kind != ExecErrorKind::Resource => Some(format!(
                "inconclusive (failed after a stubbed effect: {}); call the Tool or test_tool it with real input",
                e.message.lines().next().unwrap_or_default()
            )),
            Err(e) => return Err(anyhow!("smoke test failed: {}", e.message)),
        };
        let (info, version) = self.with_store(|st| {
            st.create_tool_version(
                &d.name,
                &d.description,
                &d.input_schema.to_string(),
                &d.output_schema.to_string(),
                &d.code,
                &d.caps,
                d.memory_mb,
                author,
            )
        })?;
        let _ = self
            .with_store(|st| st.audit(author, "tool.create", &format!("{} v{version}", d.name)));
        self.broadcast(Event::ToolUpdated { tool: info.clone() });
        Ok((info, version, smoke_note))
    }

    fn find_tools_impl(&self, session_id: &str, args: &Value) -> Result<String> {
        let query = args["query"].as_str().unwrap_or_default();
        let limit = args["limit"].as_u64().unwrap_or(8).clamp(1, 25) as usize;
        let hits = self.with_store(|st| toolreg::search_tools(st, query, limit))?;
        let ids: Vec<String> = hits.iter().map(|t| t.id.clone()).collect();
        self.with_store(|st| st.load_session_tools(session_id, &ids, "found"))?;
        let tools: Vec<Value> = hits
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "capabilities": t.capabilities,
                    "invocations": t.invocations,
                    "success_rate": t.success_rate,
                })
            })
            .collect();
        Ok(json!({
            "tools": tools,
            "note": if tools.is_empty() {
                "No matching Tool; create one."
            } else {
                "These Tools are now callable functions (from your next step); read_tool shows their code."
            }
        })
        .to_string())
    }

    fn read_tool_impl(&self, args: &Value) -> Result<String> {
        let name = args["name"].as_str().context("name required")?;
        let (info, _) = self
            .with_store(|st| st.get_tool_by_name(name))?
            .with_context(|| format!("no tool named '{name}'"))?;
        let detail = self
            .with_store(|st| st.tool_detail(&info.id))?
            .context("tool vanished")?;
        let want = args["version"].as_i64().unwrap_or(info.current_version);
        let ver = detail
            .versions
            .iter()
            .find(|v| v.version == want)
            .with_context(|| format!("'{name}' has no version {want}"))?;
        let failures = self.with_store(|st| st.recent_failures(&info.id, 5))?;
        let parse = |s: &str| serde_json::from_str::<Value>(s).unwrap_or(json!(s));
        Ok(json!({
            "name": info.name,
            "description": info.description,
            "version": ver.version,
            "current_version": info.current_version,
            "versions": detail.versions.iter().map(|v| json!({"version": v.version, "author": v.author, "created_at": v.created_at})).collect::<Vec<_>>(),
            "input_schema": parse(&ver.input_schema),
            "output_schema": parse(&ver.output_schema),
            "capabilities": ver.capabilities,
            "memory_mb": ver.memory_mb,
            "implementation_js": ver.code,
            "metrics": {"invocations": info.invocations, "success_rate": info.success_rate, "needs_attention": info.needs_attention},
            "recent_failures": failures.iter().map(|(v, tag, input, error)| json!({
                "version": v, "tag": tag, "input": parse(input),
                "error": error.lines().next().unwrap_or_default(),
            })).collect::<Vec<_>>(),
        })
        .to_string())
    }

    /// Dry-run a saved Tool or an unsaved draft: live, approval-gated, but
    /// neither saved nor recorded as an Invocation (SPEC §7.1).
    #[allow(clippy::too_many_arguments)]
    async fn test_tool_impl(
        self: &Arc<Self>,
        session_id: &str,
        run_id: &str,
        args: &Value,
        cfg: &Config,
        cancel_flag: &Arc<AtomicBool>,
        cancel: &CancellationToken,
        agent_depth: usize,
    ) -> Result<String> {
        let input = args.get("input").cloned().unwrap_or(json!({}));
        let existing = match args["name"].as_str() {
            Some(n) => self.with_store(|st| st.get_tool_by_name(n))?,
            None => None,
        };
        let d = if args["implementation_js"].is_string() {
            ToolDraft::parse(args, cfg, false)?
        } else {
            let (info, ver) = existing.clone().with_context(|| {
                "give the name of an existing Tool, or a draft with implementation_js".to_string()
            })?;
            ToolDraft {
                name: info.name,
                description: info.description,
                input_schema: serde_json::from_str(&ver.input_schema)?,
                output_schema: serde_json::from_str(&ver.output_schema)?,
                code: ver.code,
                caps: Capabilities::parse(&ver.capabilities).map_err(|e| anyhow!(e))?,
                memory_mb: ver.memory_mb,
            }
        };
        if let Err(e) =
            toolreg::validate_against(&toolreg::compile_schema(&d.input_schema)?, &input)
        {
            return Ok(json!({"ok": false, "stage": "input_validation", "error": e}).to_string());
        }
        let caps = match d.caps.bind(&input) {
            Ok(c) => c,
            Err(e) => {
                return Ok(
                    json!({"ok": false, "stage": "input_validation", "error": e}).to_string(),
                )
            }
        };
        // Approval as for a real call: an existing Tool's grant applies; a
        // new draft is asked about each time (nothing is persisted for it).
        let info = existing.map(|(i, _)| i).unwrap_or_else(|| ToolInfo {
            id: format!("test:{}", d.name),
            name: format!("{} (test)", d.name),
            description: d.description.clone(),
            retired: false,
            current_version: 0,
            capabilities: d.caps.to_specs(),
            memory_mb: d.memory_mb,
            invocations: 0,
            success_rate: None,
            needs_attention: false,
            created_at: String::new(),
            updated_at: String::new(),
        });
        let ver = ToolVersionInfo {
            version: 0,
            author: "test".into(),
            created_at: String::new(),
            capabilities: d.caps.to_specs(),
            memory_mb: d.memory_mb,
            input_schema: d.input_schema.to_string(),
            output_schema: d.output_schema.to_string(),
            code: d.code.clone(),
        };
        if !self
            .ensure_approval(cfg, session_id, &info, &ver, &caps, cancel)
            .await
        {
            return Ok(
                json!({"ok": false, "stage": "approval", "error": "user denied approval"})
                    .to_string(),
            );
        }
        let ops = Arc::new(NestedOps {
            daemon: self.clone(),
            session_id: session_id.to_string(),
            run_id: run_id.to_string(),
            parent_inv: None,
            activity: None,
            depth: 1,
            agent_depth,
            cancel_flag: cancel_flag.clone(),
            cancel: cancel.clone(),
            cfg: cfg.clone(),
            handle: tokio::runtime::Handle::current(),
            emitted: StdMutex::new(None),
            artifact_dir: self.paths.data_dir.join("artifacts"),
        });
        let req = ExecRequest {
            code: d.code.clone(),
            input,
            caps,
            memory_mb: d.memory_mb,
            mode: HostMode::Live,
            overlay: None,
            activity: None,
            modules: self.module_code(),
            cancel: cancel_flag.clone(),
        };
        let ops_dyn: Arc<dyn HostOps> = ops.clone();
        let started = std::time::Instant::now();
        let outcome = tokio::task::spawn_blocking(move || formless_runtime::execute(&req, ops_dyn))
            .await
            .map_err(|e| anyhow!("test run panicked: {e}"))?;
        let elapsed_ms = started.elapsed().as_millis() as i64;
        let emitted = ops.emitted.lock().unwrap().take();
        Ok(match outcome {
            Ok(out) => {
                let schema_error = toolreg::compile_schema(&d.output_schema)
                    .map_err(|e| e.to_string())
                    .and_then(|v| toolreg::validate_against(&v, &out.output))
                    .err();
                let mut res = json!({
                    "ok": schema_error.is_none(),
                    "output": out.output,
                    "output_matches_schema": schema_error.is_none(),
                    "logs": out.logs,
                    "elapsed_ms": elapsed_ms,
                });
                if let Some(e) = schema_error {
                    res["stage"] = json!("output_validation");
                    res["error"] = json!(e);
                }
                if let Some((path, bytes)) = emitted {
                    res["emitted"] = json!({"path": path, "bytes": bytes});
                }
                self.model_text(session_id, &res, cfg)
            }
            Err(e) => json!({
                "ok": false,
                "stage": "run",
                "error_kind": format!("{:?}", e.kind).to_lowercase(),
                "error": e.message,
                "elapsed_ms": elapsed_ms,
            })
            .to_string(),
        })
    }

    async fn ensure_approval(
        self: &Arc<Self>,
        cfg: &Config,
        session_id: &str,
        info: &ToolInfo,
        ver: &ToolVersionInfo,
        caps: &Capabilities,
        cancel: &CancellationToken,
    ) -> bool {
        let needs = caps.is_sensitive() || ver.memory_mb > cfg.tools.default_memory_mb;
        if !needs {
            return true;
        }
        let prior = self
            .with_store(|st| st.get_approval(&info.id))
            .ok()
            .flatten();
        if let Some((approved, mem)) = &prior {
            if approved.covers(caps) && ver.memory_mb <= *mem {
                return true;
            }
        }
        // Fail closed when nobody can answer (SPEC §12 headless rule).
        if self.subscriber_count() == 0 {
            return false;
        }
        let approval_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel::<(bool, ApprovalScope)>();
        self.approvals
            .lock()
            .unwrap()
            .insert(approval_id.clone(), tx);
        self.broadcast(Event::ApprovalRequested {
            approval_id: approval_id.clone(),
            session_id: session_id.to_string(),
            tool_name: info.name.clone(),
            version: ver.version,
            capabilities: caps.to_specs(),
            memory_mb: ver.memory_mb,
        });
        let (approved, scope) = tokio::select! {
            _ = cancel.cancelled() => (false, ApprovalScope::Once),
            r = rx => r.unwrap_or((false, ApprovalScope::Once)),
        };
        self.approvals.lock().unwrap().remove(&approval_id);
        if approved && scope == ApprovalScope::Once {
            let _ = self.with_store(|st| st.audit("user", "tool.approve_once", &info.name));
        } else if approved {
            // Accumulate: approving a new bound path keeps earlier ones.
            let (grant, mem) = match &prior {
                Some((p, m)) => (p.union(caps), ver.memory_mb.max(*m)),
                None => (caps.clone(), ver.memory_mb),
            };
            let _ = self.with_store(|st| st.set_approval(&info.id, &grant, mem));
            let _ = self.with_store(|st| st.audit("user", "tool.approve", &info.name));
        } else {
            let _ = self.with_store(|st| st.audit("user", "tool.deny", &info.name));
        }
        approved
    }

    #[allow(clippy::too_many_arguments)]
    fn record_inv(
        &self,
        info: &ToolInfo,
        version: i64,
        session_id: &str,
        run_id: &str,
        parent: Option<&str>,
        status: &str,
        tag: &str,
        input: &str,
        output: &str,
        error: &str,
        duration_ms: i64,
        cfg: &Config,
    ) {
        let _ = self.with_store(|st| {
            st.record_invocation(
                &info.id,
                version,
                Some(session_id),
                Some(run_id),
                parent,
                status,
                tag,
                input,
                output,
                error,
                duration_ms,
                cfg.metrics.retention_per_tool,
            )
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_inv(
        &self,
        inv_id: &str,
        info: &ToolInfo,
        status: &str,
        tag: &str,
        output: &str,
        error: &str,
        duration_ms: i64,
        cfg: &Config,
    ) {
        let _ = self.with_store(|st| {
            st.finish_invocation(
                inv_id,
                &info.id,
                status,
                tag,
                output,
                error,
                duration_ms,
                cfg.metrics.retention_per_tool,
            )
        });
    }

    fn model_text(&self, session_id: &str, value: &Value, cfg: &Config) -> String {
        let text = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let cap = cfg.tools.artifact_preview_chars;
        if text.chars().count() <= cap {
            return text;
        }
        // Store the full output as an artifact; preview enters context (§9.6).
        let dir = self.paths.data_dir.join("artifacts");
        let _ = std::fs::create_dir_all(&dir);
        let fname = format!("{}.txt", uuid::Uuid::new_v4());
        let path = dir.join(&fname);
        let preview: String = text.chars().take(cap).collect();
        if std::fs::write(&path, &text).is_ok() {
            let id = self
                .with_store(|st| {
                    st.save_artifact(
                        Some(session_id),
                        &path.to_string_lossy(),
                        &preview,
                        text.len() as i64,
                    )
                })
                .unwrap_or_default();
            return format!(
                "{preview}\n[full output ({} bytes) stored as artifact {id} at {}]",
                text.len(),
                path.display()
            );
        }
        preview
    }

    /// Execute one registry Tool and record the Invocation. Boxed to break
    /// the async recursion cycle via nested `tools.call` (SPEC §7.6).
    #[allow(clippy::too_many_arguments)]
    pub fn execute_tool_value(
        self: Arc<Self>,
        session_id: String,
        run_id: String,
        name: String,
        input: Value,
        depth: usize,
        parent: Option<String>,
        cancel_flag: Arc<AtomicBool>,
        activity: Option<Arc<std::sync::atomic::AtomicU64>>,
        cancel: CancellationToken,
        cfg: Config,
        preapproved: bool,
    ) -> ExecFuture {
        self.execute_tool_at(
            session_id,
            run_id,
            name,
            input,
            depth,
            parent,
            cancel_flag,
            activity,
            cancel,
            cfg,
            preapproved,
            0,
        )
    }

    /// [`Self::execute_tool_value`] on behalf of a sub-agent `agent_depth`
    /// levels down, so `agents.run` inside it is depth-checked.
    #[allow(clippy::too_many_arguments)]
    fn execute_tool_at(
        self: Arc<Self>,
        session_id: String,
        run_id: String,
        name: String,
        input: Value,
        depth: usize,
        parent: Option<String>,
        cancel_flag: Arc<AtomicBool>,
        activity: Option<Arc<std::sync::atomic::AtomicU64>>,
        cancel: CancellationToken,
        cfg: Config,
        preapproved: bool,
        agent_depth: usize,
    ) -> ExecFuture {
        Box::pin(async move {
            if depth > 8 {
                return Err("tool nesting too deep (possible cycle)".to_string());
            }
            let (info, ver) = match self.with_store(|st| st.get_tool_by_name(&name)) {
                Ok(Some(pair)) => pair,
                Ok(None) => return Err(format!("no tool named '{name}'")),
                Err(e) => return Err(e.to_string()),
            };
            if info.retired {
                return Err(format!("tool '{name}' is retired"));
            }
            let input_text = toolreg::redact(&input).to_string();
            let started = std::time::Instant::now();
            let dur = |s: &std::time::Instant| s.elapsed().as_millis() as i64;
            // Input validation (SPEC §7.3).
            let in_schema: Value =
                serde_json::from_str(&ver.input_schema).unwrap_or(json!({"type": "object"}));
            match toolreg::compile_schema(&in_schema) {
                Ok(v) => {
                    if let Err(e) = toolreg::validate_against(&v, &input) {
                        self.record_inv(
                            &info,
                            ver.version,
                            &session_id,
                            &run_id,
                            parent.as_deref(),
                            "failure",
                            "validation",
                            &input_text,
                            "",
                            &e,
                            dur(&started),
                            &cfg,
                        );
                        return Err(format!("input failed schema validation: {e}"));
                    }
                }
                Err(e) => return Err(format!("tool has invalid input schema: {e}")),
            }
            let caps = match bound_caps(&ver, &input) {
                Ok(c) => c,
                Err(e) => {
                    self.record_inv(
                        &info,
                        ver.version,
                        &session_id,
                        &run_id,
                        parent.as_deref(),
                        "failure",
                        "validation",
                        &input_text,
                        "",
                        &e,
                        dur(&started),
                        &cfg,
                    );
                    return Err(e);
                }
            };
            if !preapproved
                && !self
                    .ensure_approval(&cfg, &session_id, &info, &ver, &caps, &cancel)
                    .await
            {
                self.record_inv(
                    &info,
                    ver.version,
                    &session_id,
                    &run_id,
                    parent.as_deref(),
                    "denied",
                    "approval",
                    &input_text,
                    "",
                    "approval denied",
                    dur(&started),
                    &cfg,
                );
                return Err(format!("user denied approval for tool '{name}'"));
            }
            self.broadcast(Event::ToolCallStarted {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                tool_name: name.clone(),
                version: ver.version,
                arguments: input_text.clone(),
            });
            let inv_id = match self.with_store(|st| {
                st.start_invocation(
                    &info.id,
                    ver.version,
                    Some(&session_id),
                    Some(&run_id),
                    parent.as_deref(),
                    &input_text,
                )
            }) {
                Ok(id) => id,
                Err(e) => return Err(e.to_string()),
            };
            let ops = Arc::new(NestedOps {
                daemon: self.clone(),
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                parent_inv: Some(inv_id.clone()),
                activity: activity.clone(),
                depth: depth + 1,
                agent_depth,
                cancel_flag: cancel_flag.clone(),
                cancel: cancel.clone(),
                cfg: cfg.clone(),
                handle: tokio::runtime::Handle::current(),
                emitted: StdMutex::new(None),
                artifact_dir: self.paths.data_dir.join("artifacts"),
            });
            let req = ExecRequest {
                code: ver.code.clone(),
                input: input.clone(),
                caps,
                memory_mb: ver.memory_mb,
                mode: HostMode::Live,
                overlay: None,
                activity: activity.clone(),
                modules: self.module_code(),
                cancel: cancel_flag.clone(),
            };
            let ops_dyn: Arc<dyn HostOps> = ops.clone();
            let outcome =
                tokio::task::spawn_blocking(move || formless_runtime::execute(&req, ops_dyn)).await;
            let elapsed = dur(&started);
            let finish = |ok: bool, this: &Daemon| {
                this.broadcast(Event::ToolCallFinished {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                    tool_name: name.clone(),
                    ok,
                    elapsed_ms: elapsed,
                });
            };
            match outcome {
                Ok(Ok(mut out)) => {
                    // Emitted artifact reference (streamed output, §7.5).
                    if let Some((path, bytes)) = ops.emitted.lock().unwrap().take() {
                        let preview: String = out.output.to_string().chars().take(200).collect();
                        if let Ok(id) = self.with_store(|st| {
                            st.save_artifact(
                                Some(&session_id),
                                &path.to_string_lossy(),
                                &preview,
                                bytes,
                            )
                        }) {
                            out.output = json!({"result": out.output, "emitted_artifact": id, "emitted_bytes": bytes});
                        }
                    }
                    // Output validation.
                    let out_schema: Value =
                        serde_json::from_str(&ver.output_schema).unwrap_or(json!({}));
                    if let Ok(v) = toolreg::compile_schema(&out_schema) {
                        if let Err(e) = toolreg::validate_against(&v, &out.output) {
                            self.finish_inv(
                                &inv_id,
                                &info,
                                "failure",
                                "validation",
                                &toolreg::redact(&out.output).to_string(),
                                &e,
                                elapsed,
                                &cfg,
                            );
                            finish(false, &self);
                            return Err(format!("output failed schema validation: {e}"));
                        }
                    }
                    self.finish_inv(
                        &inv_id,
                        &info,
                        "success",
                        "",
                        &toolreg::redact(&out.output).to_string(),
                        "",
                        elapsed,
                        &cfg,
                    );
                    finish(true, &self);
                    Ok((out.output, inv_id.clone()))
                }
                Ok(Err(e)) => {
                    let (status, tag) = match e.kind {
                        ExecErrorKind::Cancelled => ("cancelled", "user-cancelled"),
                        ExecErrorKind::Resource => ("failure", "resource"),
                        ExecErrorKind::Denied => ("failure", "denied-capability"),
                        ExecErrorKind::Js => ("failure", ""),
                    };
                    self.finish_inv(&inv_id, &info, status, tag, "", &e.message, elapsed, &cfg);
                    finish(false, &self);
                    Err(e.message)
                }
                Err(join_err) => {
                    self.finish_inv(
                        &inv_id,
                        &info,
                        "failure",
                        "",
                        "",
                        &join_err.to_string(),
                        elapsed,
                        &cfg,
                    );
                    finish(false, &self);
                    Err(format!("tool execution panicked: {join_err}"))
                }
            }
        })
    }

    async fn maybe_compact(
        &self,
        provider: &Provider,
        cfg: &Config,
        session_id: &str,
        model: &str,
        context_tokens: u32,
    ) -> Result<()> {
        let prompt = self.effective_system_prompt();
        let (_ctx, needs, compact_up_to) = self.with_store(|st| {
            build_context(
                st,
                session_id,
                &prompt,
                context_tokens,
                cfg.compaction.fraction(),
            )
        })?;
        if !needs {
            return Ok(());
        }
        let text = self.with_store(|st| {
            let (_s, prev_up_to) = st.latest_checkpoint(session_id)?.unwrap_or_default();
            let msgs = st.list_messages(session_id)?;
            Ok(msgs
                .iter()
                .filter(|m| m.seq > prev_up_to && m.seq <= compact_up_to)
                .map(|m| format!("{}: {}", m.role, m.content))
                .collect::<Vec<_>>()
                .join("\n"))
        })?;
        let cmodel = cfg
            .compaction
            .model
            .clone()
            .unwrap_or_else(|| model.to_string());
        let previous = self.with_store(|st| {
            Ok(st
                .latest_checkpoint(session_id)?
                .map(|(s, _)| s)
                .unwrap_or_default())
        })?;
        let input = if previous.is_empty() {
            text
        } else {
            format!("Previous summary:\n{previous}\n\n{text}")
        };
        let summary = if cfg.compaction.server_side {
            match provider.compact_server(&input, &cmodel).await {
                Ok(s) => s,
                Err(e) => {
                    self.broadcast(Event::Note {
                        session_id: Some(session_id.to_string()),
                        text: format!("Server-side compaction unavailable ({e}); using daemon-side compaction."),
                    });
                    provider.summarize(&input, &cmodel).await?
                }
            }
        } else {
            provider.summarize(&input, &cmodel).await?
        };
        self.with_store(|st| st.save_checkpoint(session_id, &summary, compact_up_to))?;
        let preview: String = summary.chars().take(200).collect();
        self.broadcast(Event::CompactionNotice {
            session_id: session_id.to_string(),
            summary_preview: preview,
        });
        Ok(())
    }

    pub(crate) async fn handle_conn<S>(self: Arc<Self>, stream: S)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();
        self.subs.lock().unwrap().push(tx.clone());
        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Ok(line) = serde_json::to_string(&msg) {
                    if write_half.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    if write_half.write_all(b"\n").await.is_err() {
                        break;
                    }
                }
            }
        });
        let mut lines = BufReader::new(read_half).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let resp = match serde_json::from_str::<RequestEnvelope>(&line) {
                Ok(env) => {
                    if env.protocol != PROTOCOL_VERSION {
                        ServerMessage::err(
                            env.id,
                            format!(
                                "protocol mismatch: client {} daemon {PROTOCOL_VERSION}",
                                env.protocol
                            ),
                        )
                    } else {
                        match self.handle(env.request).await {
                            Ok(v) => ServerMessage::ok(env.id, v),
                            Err(e) => ServerMessage::err(env.id, e.to_string()),
                        }
                    }
                }
                Err(e) => ServerMessage::err(0, format!("bad request: {e}")),
            };
            if tx.send(resp).is_err() {
                break;
            }
        }
        writer.abort();
    }

    async fn idle_watcher(self: Arc<Self>) {
        let mut idle_since: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
            let grace = Config::load(&self.paths)
                .map(|c| c.daemon.idle_grace_secs)
                .unwrap_or(60);
            if grace == u64::MAX {
                continue; // idle-close disabled (used by explicit foreground serve in tests)
            }
            let pending_running = self
                .pending
                .lock()
                .unwrap()
                .values()
                .any(|e| e.result.lock().unwrap().is_none());
            // Stall warnings (SPEC §6.1): advisory only, never auto-kill.
            if let Ok(cfg) = Config::load(&self.paths) {
                let stall_ms = cfg.daemon.stall_warning_secs.saturating_mul(1000);
                let now = epoch_ms();
                let entries: Vec<(String, Arc<PendingEntry>)> = self
                    .pending
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(h, e)| (h.clone(), e.clone()))
                    .collect();
                for (_h, e) in entries {
                    if e.result.lock().unwrap().is_none()
                        && now.saturating_sub(e.activity.load(Ordering::SeqCst)) >= stall_ms
                        && !e.stalled_noted.swap(true, Ordering::SeqCst)
                    {
                        self.broadcast(Event::Note {
                            session_id: Some(e.session_id.clone()),
                            text: format!("tool {} possibly stalled (no activity)", e.tool_name),
                        });
                    }
                }
            }
            let busy = pending_running
                || !self.runs.lock().unwrap().is_empty()
                || self.subscriber_count() > 0;
            if busy {
                idle_since = None;
            } else {
                let since = idle_since.get_or_insert_with(tokio::time::Instant::now);
                if since.elapsed() >= std::time::Duration::from_secs(grace) {
                    tracing::info!("idle grace elapsed; daemon shutting down");
                    self.shutdown.cancel();
                    return;
                }
            }
        }
    }

    pub async fn serve(self: Arc<Self>) -> Result<()> {
        std::fs::write(self.paths.lock_file(), std::process::id().to_string())?;
        let watcher = {
            let this = self.clone();
            tokio::spawn(async move { this.idle_watcher().await })
        };
        let optimiser = {
            let this = self.clone();
            tokio::spawn(async move { this.optimiser_loop().await })
        };
        let relay = {
            let this = self.clone();
            tokio::spawn(async move { crate::relay::relay_loop(this).await })
        };
        let result = self.accept_loop().await;
        relay.abort();
        optimiser.abort();
        watcher.abort();
        let _ = std::fs::remove_file(self.paths.lock_file());
        result
    }

    #[cfg(unix)]
    async fn accept_loop(self: &Arc<Self>) -> Result<()> {
        let sock = self.paths.socket_path();
        if daemon_reachable(&self.paths) {
            return Err(anyhow!("a daemon is already running for this data dir"));
        }
        let _ = std::fs::remove_file(&sock);
        let listener = tokio::net::UnixListener::bind(&sock)
            .with_context(|| format!("bind {}", sock.display()))?;
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let this = self.clone();
                    tokio::spawn(async move { this.handle_conn(stream).await });
                }
            }
        }
        let _ = std::fs::remove_file(&sock);
        Ok(())
    }

    #[cfg(windows)]
    async fn accept_loop(self: &Arc<Self>) -> Result<()> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = self.paths.pipe_name();
        loop {
            let server = ServerOptions::new().create(&name)?;
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                r = server.connect() => {
                    r?;
                    let this = self.clone();
                    tokio::spawn(async move { this.handle_conn(server).await });
                }
            }
        }
        Ok(())
    }
}

/// A Tool version's Capabilities with `$input.<field>` scopes resolved
/// against this Invocation's input (SPEC §7.4).
/// A Tool definition as passed to create_tool/test_tool, validated.
struct ToolDraft {
    name: String,
    description: String,
    input_schema: Value,
    output_schema: Value,
    code: String,
    caps: Capabilities,
    memory_mb: u32,
}

impl ToolDraft {
    /// Validate the create_tool fields (SPEC §7.1). A test draft may omit
    /// name, description and schemas.
    fn parse(args: &Value, cfg: &Config, strict: bool) -> Result<Self> {
        let name = match args["name"].as_str() {
            Some(n) => n,
            None if !strict => "draft",
            None => return Err(anyhow!("create_tool: name required")),
        };
        toolreg::validate_name(name)?;
        let schema = |key: &str, default: Value| -> Result<Value> {
            match &args[key] {
                Value::Null if !strict => Ok(default),
                v if v.is_object() => {
                    toolreg::compile_schema(v)?;
                    Ok(v.clone())
                }
                _ => Err(anyhow!(
                    "input_schema and output_schema must be JSON Schema objects"
                )),
            }
        };
        let input_schema = schema("input_schema", json!({"type": "object"}))?;
        let output_schema = schema("output_schema", json!({}))?;
        let code = args["implementation_js"]
            .as_str()
            .context("implementation_js required")?
            .to_string();
        let cap_specs: Vec<String> = args["capabilities"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let caps = Capabilities::parse(&cap_specs).map_err(|e| anyhow!(e))?;
        let memory_mb = args["memory_mb"]
            .as_u64()
            .unwrap_or(cfg.tools.default_memory_mb as u64) as u32;
        if memory_mb == 0 || memory_mb > cfg.tools.max_memory_mb {
            return Err(anyhow!("memory_mb must be 1..={}", cfg.tools.max_memory_mb));
        }
        formless_runtime::check_syntax(&code).map_err(|e| anyhow!("syntax check failed: {e}"))?;
        Ok(Self {
            name: name.to_string(),
            description: args["description"].as_str().unwrap_or_default().to_string(),
            input_schema,
            output_schema,
            code,
            caps,
            memory_mb,
        })
    }
}

pub(crate) fn bound_caps(
    ver: &ToolVersionInfo,
    input: &Value,
) -> std::result::Result<Capabilities, String> {
    Capabilities::parse(&ver.capabilities)
        .unwrap_or_default()
        .bind(input)
}

/// Is a daemon already answering on this transport?
#[cfg(unix)]
pub fn daemon_reachable(paths: &Paths) -> bool {
    std::os::unix::net::UnixStream::connect(paths.socket_path()).is_ok()
}
#[cfg(windows)]
pub fn daemon_reachable(paths: &Paths) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.pipe_name())
        .is_ok()
}

/// Spawn `formless daemon serve` detached (client auto-start, SPEC §9.1).
pub fn spawn_daemon(paths: &Paths) -> Result<()> {
    let exe = std::env::current_exe().context("locate formless binary")?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())?;
    let log_err = log.try_clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .arg("serve")
        .env("FORMLESS_DATA_DIR", &paths.data_dir)
        .env("FORMLESS_CONFIG_DIR", &paths.config_dir)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            // Detach into its own session so client exit (or our process
            // group teardown) never takes the daemon with it.
            if libc_setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().context("spawn daemon")?;
    Ok(())
}

#[cfg(unix)]
fn libc_setsid() -> i32 {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe { setsid() }
}

/// Connect, auto-starting the daemon if needed. Waits up to ~5s.
pub fn ensure_daemon(paths: &Paths) -> Result<()> {
    if daemon_reachable(paths) {
        return Ok(());
    }
    spawn_daemon(paths)?;
    for _ in 0..50 {
        if daemon_reachable(paths) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(anyhow!(
        "daemon did not come up; see {}",
        paths.log_file().display()
    ))
}

/// HostOps for nested `tools.call` + `emit` (SPEC §7.6). Runs on the
/// blocking QuickJS thread; nested calls block_on the daemon executor.
struct NestedOps {
    daemon: Arc<Daemon>,
    session_id: String,
    run_id: String,
    parent_inv: Option<String>,
    activity: Option<Arc<std::sync::atomic::AtomicU64>>,
    depth: usize,
    /// Sub-agent nesting of the Run this Tool serves (0 = the main agent).
    agent_depth: usize,
    cancel_flag: Arc<AtomicBool>,
    cancel: CancellationToken,
    cfg: Config,
    handle: tokio::runtime::Handle,
    emitted: StdMutex<Option<(std::path::PathBuf, i64)>>,
    artifact_dir: std::path::PathBuf,
}

impl NestedOps {
    /// Block the sandbox thread on provider work, returning "interrupted"
    /// as soon as the Run is cancelled instead of waiting the call out.
    fn block_cancellable<T>(
        &self,
        work: impl std::future::Future<Output = Result<T>>,
    ) -> std::result::Result<T, String> {
        let cancel = self.cancel.clone();
        self.handle.block_on(async move {
            tokio::select! {
                _ = cancel.cancelled() => Err("interrupted".to_string()),
                r = work => r.map_err(|e| e.to_string()),
            }
        })
    }
}

impl HostOps for NestedOps {
    fn run_subagent(
        &self,
        task: &str,
        system: Option<&str>,
        model: Option<&str>,
    ) -> std::result::Result<String, String> {
        if self.depth > 1 {
            return Err("sub-agents cannot be spawned from nested tools".into());
        }
        // A sub-agent's own Tool calls restart at tool depth 0, so tool
        // depth alone cannot stop agents.run recursing through them.
        if self.agent_depth >= self.cfg.tools.max_agent_depth {
            return Err(format!(
                "denied: sub-agent depth limit ({}) reached",
                self.cfg.tools.max_agent_depth
            ));
        }
        if self.cancel.is_cancelled() {
            return Err("interrupted".into());
        }
        let agent_depth = self.agent_depth + 1;
        let daemon = self.daemon.clone();
        let (session_id, run_id) = (self.session_id.clone(), self.run_id.clone());
        let cfg = self.cfg.clone();
        let cancel_flag = self.cancel_flag.clone();
        let cancel = self.cancel.clone();
        let (task, system, model) = (
            task.to_string(),
            system.map(str::to_string),
            model.map(str::to_string),
        );
        self.block_cancellable(async move {
            daemon
                .run_subagent_text(
                    &session_id,
                    &run_id,
                    &task,
                    system.as_deref(),
                    model.as_deref(),
                    &cfg,
                    &cancel_flag,
                    &cancel,
                    agent_depth,
                )
                .await
        })
    }

    fn llm_complete(
        &self,
        prompt: &str,
        system: Option<&str>,
        model: Option<&str>,
    ) -> std::result::Result<String, String> {
        if self.cancel.is_cancelled() {
            return Err("interrupted".into());
        }
        let provider = self
            .handle
            .block_on(Provider::for_profile(
                self.cfg.provider.clone(),
                self.daemon.paths.clone(),
            ))
            .map_err(|e| e.to_string())?;
        let requested = model
            .map(str::to_string)
            .unwrap_or_else(|| self.cfg.provider.model.clone());
        let model = self
            .daemon
            .resolve_model(
                &requested,
                &self.cfg,
                crate::routing::is_complex_task(
                    prompt.chars().count(),
                    0,
                    self.cfg.provider.routing_complex_chars,
                ),
            )
            .map_err(|e| e.to_string())?;
        let mut messages = Vec::new();
        if let Some(sys) = system {
            messages.push(json!({"role": "system", "content": sys}));
        }
        messages.push(json!({"role": "user", "content": prompt}));
        let effort = self.cfg.provider.reasoning_effort.clone();
        self.block_cancellable(provider.complete(&messages, &model, effort.as_deref()))
    }

    fn call_tool(&self, name: &str, input: Value) -> std::result::Result<Value, String> {
        let daemon = self.daemon.clone();
        let (sid, rid, name) = (
            self.session_id.clone(),
            self.run_id.clone(),
            name.to_string(),
        );
        let (flag, tok, cfg, depth, parent, activity, agent_depth) = (
            self.cancel_flag.clone(),
            self.cancel.clone(),
            self.cfg.clone(),
            self.depth,
            self.parent_inv.clone(),
            self.activity.clone(),
            self.agent_depth,
        );
        self.handle.block_on(async move {
            daemon
                .execute_tool_at(
                    sid,
                    rid,
                    name,
                    input,
                    depth,
                    parent,
                    flag,
                    activity,
                    tok,
                    cfg,
                    false,
                    agent_depth,
                )
                .await
                .map(|(v, _inv)| v)
        })
    }
    fn emit(&self, chunk: &str) -> std::result::Result<(), String> {
        use std::io::Write;
        std::fs::create_dir_all(&self.artifact_dir).map_err(|e| e.to_string())?;
        let mut guard = self.emitted.lock().unwrap();
        if guard.is_none() {
            let path = self
                .artifact_dir
                .join(format!("emit-{}.txt", uuid::Uuid::new_v4()));
            *guard = Some((path, 0));
        }
        let (path, bytes) = guard.as_mut().unwrap();
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&*path)
            .map_err(|e| e.to_string())?;
        f.write_all(chunk.as_bytes()).map_err(|e| e.to_string())?;
        *bytes += chunk.len() as i64;
        Ok(())
    }
}
