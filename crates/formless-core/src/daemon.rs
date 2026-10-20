use anyhow::{anyhow, Context, Result};
use formless_protocol::*;
use formless_runtime::{Capabilities, ExecErrorKind, ExecRequest, HostMode, HostOps};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;

use crate::config::{load_api_key, Config};
use crate::provider::{DeltaKind, Provider, ToolCall, SYSTEM_PROMPT};
use crate::storage::{build_context, Store};
use crate::tools as toolreg;
use crate::Paths;

type Subs = Arc<Mutex<Vec<mpsc::UnboundedSender<ServerMessage>>>>;
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

pub(crate) fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) struct RunCtl {
    cancel: CancellationToken,
    cancel_flag: Arc<AtomicBool>,
    steer_tx: mpsc::UnboundedSender<()>,
}

pub struct Daemon {
    pub(crate) paths: Paths,
    pub(crate) store: Arc<StdMutex<Store>>,
    pub(crate) runs: Arc<StdMutex<HashMap<String, RunCtl>>>,
    queued: Arc<Mutex<HashMap<String, Vec<String>>>>,
    approvals: Arc<StdMutex<HashMap<String, oneshot::Sender<bool>>>>,
    /// promotion_id -> (attempt_id, kind: "widening" | "replay_limited")
    pub(crate) promotions: Arc<StdMutex<HashMap<String, (String, String)>>>,
    pub(crate) pending: Arc<StdMutex<HashMap<String, Arc<PendingEntry>>>>,
    subs: Subs,
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
            queued: Arc::new(Mutex::new(HashMap::new())),
            approvals: Arc::new(StdMutex::new(HashMap::new())),
            promotions: Arc::new(StdMutex::new(HashMap::new())),
            pending: Arc::new(StdMutex::new(HashMap::new())),
            subs: Arc::new(Mutex::new(Vec::new())),
            shutdown: CancellationToken::new(),
        })
    }

    pub(crate) fn broadcast(&self, ev: Event) {
        let subs = self.subs.clone();
        tokio::spawn(async move {
            let mut guard = subs.lock().await;
            guard.retain(|tx| tx.send(ServerMessage::Event(ev.clone())).is_ok());
        });
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
                Ok(
                    json!({"protocol": PROTOCOL_VERSION, "sessions": sessions, "pid": std::process::id()}),
                )
            }
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
                let existed = self.with_store(|st| st.delete_session(&id))?;
                if existed {
                    self.broadcast(Event::SessionDeleted { id: id.clone() });
                }
                Ok(json!({"deleted": existed}))
            }
            Request::GetSession { id } => {
                let (session, messages) = self.with_store(|st| {
                    let s = st.get_session(&id)?.context("session not found")?;
                    let m = st.list_messages(&id)?;
                    Ok((s, m))
                })?;
                Ok(json!({"session": session, "messages": messages}))
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
            Request::ApprovalDecision {
                approval_id,
                approved,
            } => {
                // First answer wins (SPEC §10.4): removing the sender resolves it.
                let tx = self.approvals.lock().unwrap().remove(&approval_id);
                let resolved = match tx {
                    Some(tx) => tx.send(approved).is_ok(),
                    None => false,
                };
                Ok(json!({"resolved": resolved}))
            }
        }
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
                self.queued
                    .lock()
                    .await
                    .entry(session_id)
                    .or_default()
                    .push(content);
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
        // Queued messages start fresh Runs, in order (SPEC §5.3).
        let next = {
            let mut q = self.queued.lock().await;
            q.get_mut(&session_id).and_then(|v| {
                if v.is_empty() {
                    None
                } else {
                    Some(v.remove(0))
                }
            })
        };
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
        let api_key = load_api_key(&self.paths).unwrap_or_default();
        let provider = Provider::new(cfg.provider.clone(), api_key)?;
        let session = self
            .with_store(|st| st.get_session(session_id))?
            .context("session not found")?;
        let model = session
            .model
            .clone()
            .unwrap_or_else(|| cfg.provider.model.clone());
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
            let ctx = self.with_store(|st| {
                build_context(
                    st,
                    session_id,
                    SYSTEM_PROMPT,
                    context_tokens,
                    cfg.compaction.fraction(),
                )
                .map(|(c, _, _)| c)
            })?;
            let defs = self.with_store(toolreg::tool_definitions)?;
            let (sid, rid) = (session_id.to_string(), run_id.to_string());
            let subs = self.subs.clone();
            let chat_fut = provider.chat_stream(
                &ctx,
                &defs,
                &model,
                effort.as_deref(),
                cancel,
                |kind, delta| {
                    let ev = match kind {
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
                    };
                    let subs2 = subs.clone();
                    tokio::spawn(async move {
                        let mut g = subs2.lock().await;
                        g.retain(|tx| tx.send(ServerMessage::Event(ev.clone())).is_ok());
                    });
                },
            );
            let mut chat_fut = std::pin::pin!(chat_fut);
            let result = tokio::select! {
                r = &mut chat_fut => r?,
                _ = tokio::time::sleep(std::time::Duration::from_secs(cfg.tools.pending_threshold_secs)) => {
                    self.broadcast(Event::Note {
                        session_id: Some(session_id.to_string()),
                        text: "Provider request still pending; waiting (not terminated).".into(),
                    });
                    (&mut chat_fut).await?
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
            // Approval pre-pass: one prompt per unapproved sensitive Tool in
            // the batch, before anything runs (SPEC §5.2/§10.3).
            let mut denied: HashMap<String, bool> = HashMap::new();
            for tc in &result.tool_calls {
                if tc.name == toolreg::CREATE_TOOL || tc.name == toolreg::RETIRE_TOOL {
                    continue;
                }
                if let Some((info, ver)) = self.with_store(|st| st.get_tool_by_name(&tc.name))? {
                    if !self
                        .ensure_approval(&cfg, session_id, &info, &ver, cancel)
                        .await
                    {
                        denied.insert(info.id.clone(), true);
                        let caps = Capabilities::parse(&ver.capabilities).unwrap_or_default();
                        let _ = caps;
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
                            &cfg,
                        );
                    }
                }
            }
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
        let bootstrap = matches!(
            tc.name.as_str(),
            toolreg::CREATE_TOOL
                | toolreg::RETIRE_TOOL
                | toolreg::AWAIT_INVOCATION
                | toolreg::CHECK_INVOCATION
                | toolreg::ABANDON_INVOCATION
        );
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
                Ok(text) => text,
                Err(e) => format!("error: create_tool failed: {e}"),
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
                if let Ok(Some((info, _))) = self.with_store(|st| st.get_tool_by_name(name)) {
                    if denied.contains_key(&info.id) {
                        return format!("error: user denied approval for tool '{name}'");
                    }
                }
                self.dispatch_pending(session_id, run_id, name, args, cfg, cancel_flag, cancel)
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
                    .execute_tool_value(
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
        let (info, _v) = self.create_tool_as(&args, cfg, "import").await?;
        Ok(info)
    }

    async fn create_tool_impl(self: &Arc<Self>, args: &Value, cfg: &Config) -> Result<String> {
        let (info, version) = self.create_tool_as(args, cfg, "agent").await?;
        Ok(json!({"created": info.name, "version": version}).to_string())
    }

    async fn create_tool_as(
        self: &Arc<Self>,
        args: &Value,
        cfg: &Config,
        author: &str,
    ) -> Result<(ToolInfo, i64)> {
        let name = args["name"]
            .as_str()
            .context("create_tool: name required")?;
        toolreg::validate_name(name)?;
        let description = args["description"].as_str().unwrap_or_default();
        let input_schema = &args["input_schema"];
        let output_schema = &args["output_schema"];
        if !input_schema.is_object() || !output_schema.is_object() {
            return Err(anyhow!(
                "input_schema and output_schema must be JSON Schema objects"
            ));
        }
        toolreg::compile_schema(input_schema)?;
        toolreg::compile_schema(output_schema)?;
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
        // Smoke test in the stubbed sandbox with a schema-minimal input.
        let smoke_input = toolreg::minimal_input(input_schema);
        let req = ExecRequest {
            code: code.clone(),
            input: smoke_input,
            caps: caps.clone(),
            memory_mb,
            mode: HostMode::Smoke,
            overlay: None,
            activity: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let smoke = tokio::task::spawn_blocking(move || {
            formless_runtime::execute(&req, Arc::new(formless_runtime::NoHostOps))
        })
        .await
        .map_err(|e| anyhow!("smoke test panicked: {e}"))?;
        if let Err(e) = smoke {
            return Err(anyhow!("smoke test failed: {}", e.message));
        }
        let (info, version) = self.with_store(|st| {
            st.create_tool_version(
                name,
                description,
                &input_schema.to_string(),
                &output_schema.to_string(),
                &code,
                &caps,
                memory_mb,
                author,
            )
        })?;
        let _ =
            self.with_store(|st| st.audit(author, "tool.create", &format!("{name} v{version}")));
        self.broadcast(Event::ToolUpdated { tool: info.clone() });
        Ok((info, version))
    }

    async fn ensure_approval(
        self: &Arc<Self>,
        cfg: &Config,
        session_id: &str,
        info: &ToolInfo,
        ver: &ToolVersionInfo,
        cancel: &CancellationToken,
    ) -> bool {
        let caps = Capabilities::parse(&ver.capabilities).unwrap_or_default();
        let needs = caps.is_sensitive() || ver.memory_mb > cfg.tools.default_memory_mb;
        if !needs {
            return true;
        }
        if let Ok(Some((approved, mem))) = self.with_store(|st| st.get_approval(&info.id)) {
            if approved.covers(&caps) && ver.memory_mb <= mem {
                return true;
            }
        }
        // Fail closed when nobody can answer (SPEC §12 headless rule).
        self.subs.lock().await.retain(|tx| !tx.is_closed());
        if self.subs.lock().await.is_empty() {
            return false;
        }
        let approval_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel::<bool>();
        self.approvals
            .lock()
            .unwrap()
            .insert(approval_id.clone(), tx);
        self.broadcast(Event::ApprovalRequested {
            approval_id: approval_id.clone(),
            session_id: session_id.to_string(),
            tool_name: info.name.clone(),
            version: ver.version,
            capabilities: ver.capabilities.clone(),
            memory_mb: ver.memory_mb,
        });
        let approved = tokio::select! {
            _ = cancel.cancelled() => false,
            r = rx => r.unwrap_or(false),
        };
        self.approvals.lock().unwrap().remove(&approval_id);
        if approved {
            let _ = self.with_store(|st| st.set_approval(&info.id, &caps, ver.memory_mb));
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
            if !preapproved
                && !self
                    .ensure_approval(&cfg, &session_id, &info, &ver, &cancel)
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
            let caps = Capabilities::parse(&ver.capabilities).unwrap_or_default();
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
        let (_ctx, needs, compact_up_to) = self.with_store(|st| {
            build_context(
                st,
                session_id,
                SYSTEM_PROMPT,
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
        self.subs.lock().await.push(tx.clone());
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
            // Prune subscribers whose client disconnected (receiver dropped).
            self.subs.lock().await.retain(|tx| !tx.is_closed());
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
                || !self.subs.lock().await.is_empty();
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
    cancel_flag: Arc<AtomicBool>,
    cancel: CancellationToken,
    cfg: Config,
    handle: tokio::runtime::Handle,
    emitted: StdMutex<Option<(std::path::PathBuf, i64)>>,
    artifact_dir: std::path::PathBuf,
}

impl HostOps for NestedOps {
    fn llm_complete(
        &self,
        prompt: &str,
        system: Option<&str>,
        model: Option<&str>,
    ) -> std::result::Result<String, String> {
        if self.cancel.is_cancelled() {
            return Err("interrupted".into());
        }
        let api_key = load_api_key(&self.daemon.paths).unwrap_or_default();
        let provider =
            Provider::new(self.cfg.provider.clone(), api_key).map_err(|e| e.to_string())?;
        let model = model
            .map(str::to_string)
            .unwrap_or_else(|| self.cfg.provider.model.clone());
        let mut messages = Vec::new();
        if let Some(sys) = system {
            messages.push(json!({"role": "system", "content": sys}));
        }
        messages.push(json!({"role": "user", "content": prompt}));
        let effort = self.cfg.provider.reasoning_effort.clone();
        self.handle
            .block_on(provider.complete(&messages, &model, effort.as_deref()))
            .map_err(|e| e.to_string())
    }

    fn call_tool(&self, name: &str, input: Value) -> std::result::Result<Value, String> {
        let daemon = self.daemon.clone();
        let (sid, rid, name) = (
            self.session_id.clone(),
            self.run_id.clone(),
            name.to_string(),
        );
        let (flag, tok, cfg, depth, parent, activity) = (
            self.cancel_flag.clone(),
            self.cancel.clone(),
            self.cfg.clone(),
            self.depth,
            self.parent_inv.clone(),
            self.activity.clone(),
        );
        self.handle.block_on(async move {
            daemon
                .execute_tool_value(
                    sid, rid, name, input, depth, parent, flag, activity, tok, cfg, false,
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
