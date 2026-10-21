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
use crate::storage::{ContextCache, Store, DEFAULT_SESSION_NAME};
use crate::tools as toolreg;
use crate::Paths;

// A std mutex (never held across .await) so broadcast() sends synchronously
// and every subscriber sees events in emission order.
type Subs = Arc<StdMutex<Vec<Subscriber>>>;

/// A connected client and the events it asked for (`Request::Subscribe`).
struct Subscriber {
    tx: mpsc::UnboundedSender<ServerMessage>,
    filter: Arc<StdMutex<EventFilter>>,
}

struct EventFilter {
    /// None = every Session.
    sessions: Option<std::collections::HashSet<String>>,
    deltas: bool,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self {
            sessions: None,
            deltas: true,
        }
    }
}

impl EventFilter {
    fn wants(&self, ev: &Event) -> bool {
        match ev {
            // Session lists, running dots and approvals stay global.
            Event::RunStarted { .. }
            | Event::RunFinished { .. }
            | Event::RunPaused { .. }
            | Event::SessionUpdated { .. }
            | Event::SessionDeleted { .. }
            | Event::ApprovalRequested { .. } => true,
            Event::MessageDelta { .. } | Event::ReasoningDelta { .. } if !self.deltas => false,
            _ => match (ev.session_id(), &self.sessions) {
                (Some(id), Some(wanted)) => wanted.contains(id),
                _ => true,
            },
        }
    }
}
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
    /// Cancels just this Invocation (a child of the Run's token).
    cancel: CancellationToken,
    /// Creation order of the handle (delivery order).
    order: u64,
}

/// Fan an event out to every live subscriber, dropping disconnected ones.
fn send_all(subs: &Subs, ev: Event) {
    subs.lock().unwrap().retain(|sub| {
        !sub.filter.lock().unwrap().wants(&ev)
            || sub.tx.send(ServerMessage::Event(ev.clone())).is_ok()
    });
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

/// Shared module (name, code) snapshot handed to Invocations.
type ModuleCode = Arc<Vec<(String, String)>>;

/// Read-only database connections for client queries (see `with_reader`).
const READER_CONNECTIONS: usize = 4;

/// What identifies a version of config.toml on disk: (mtime, length).
type ConfigStamp = Option<(std::time::SystemTime, u64)>;

fn config_stamp(paths: &Paths) -> ConfigStamp {
    let meta = std::fs::metadata(paths.config_file()).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

pub(crate) fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// What the MCP part of an approval decided.
enum McpApproval {
    Denied,
    Allowed,
    /// Continue with the per-tool approval, showing this detail.
    AskTool(String),
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
    /// Read-only connections for client queries (lists, search, exports),
    /// so a slow read never holds up writes behind the main store lock.
    readers: Vec<StdMutex<Store>>,
    pub(crate) runs: Arc<StdMutex<HashMap<String, RunCtl>>>,
    approvals: Arc<StdMutex<HashMap<String, ApprovalReply>>>,
    /// promotion_id -> (attempt_id, kind: "widening" | "replay_limited")
    pub(crate) promotions: Arc<StdMutex<HashMap<String, (String, String)>>>,
    pub(crate) pending: Arc<StdMutex<HashMap<String, Arc<PendingEntry>>>>,
    subs: Subs,
    /// One HTTP client (connection pool + TLS sessions) for every provider
    /// call the daemon makes.
    http: reqwest::Client,
    /// Module code snapshot keyed by its fingerprint (see `module_code`).
    modules: StdMutex<Option<(String, ModuleCode)>>,
    /// Running MCP servers ((SPEC §7.9)).
    pub(crate) mcp: crate::mcp::Manager,
    /// Parsed config.toml, re-read only when the file changes.
    config: StdMutex<Option<(ConfigStamp, Config)>>,
    /// Per-Session provider context, extended step by step (SPEC §11).
    contexts: StdMutex<HashMap<String, ContextCache>>,
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
        // Pending handles do not survive a restart; tell each Session's
        // agent instead of leaving a handle that later reads "unknown".
        for (handle, session_id, tool) in store.interrupt_pending_handles()? {
            let note = format!(
                "Invocation {handle} ({tool}) was interrupted by a daemon restart; its outcome \
                 is unknown and it was not re-run. Check its side effects or run it again."
            );
            store.append_message(&session_id, "system-note", &note)?;
        }
        let readers = (0..READER_CONNECTIONS)
            .map(|_| Store::open_reader(&paths.db_file()).map(StdMutex::new))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            paths,
            store: Arc::new(StdMutex::new(store)),
            readers,
            runs: Arc::new(StdMutex::new(HashMap::new())),
            approvals: Arc::new(StdMutex::new(HashMap::new())),
            promotions: Arc::new(StdMutex::new(HashMap::new())),
            pending: Arc::new(StdMutex::new(HashMap::new())),
            subs: Arc::new(StdMutex::new(Vec::new())),
            contexts: StdMutex::new(HashMap::new()),
            config: StdMutex::new(None),
            mcp: Default::default(),
            modules: StdMutex::new(None),
            http: reqwest::Client::builder().build()?,
            naming: Arc::new(tokio::sync::Semaphore::new(1)),
            model_download: Arc::new(AtomicBool::new(false)),
            shutdown: CancellationToken::new(),
        })
    }

    /// Connected clients that receive events (and can answer approvals).
    pub fn subscriber_count(&self) -> usize {
        let mut subs = self.subs.lock().unwrap();
        subs.retain(|sub| !sub.tx.is_closed());
        subs.len()
    }

    /// Current settings. config.toml is parsed again only when its
    /// modification time or size changed (one `stat` per call otherwise),
    /// and clients are told when that happens.
    pub(crate) fn config(&self) -> Result<Config> {
        let stamp = config_stamp(&self.paths);
        let mut slot = self.config.lock().unwrap();
        if let Some((cached, cfg)) = slot.as_ref() {
            if *cached == stamp {
                return Ok(cfg.clone());
            }
        }
        let cfg = Config::load(&self.paths)?;
        let changed = slot.is_some();
        *slot = Some((stamp, cfg.clone()));
        drop(slot);
        if changed {
            self.broadcast(Event::SettingsChanged);
        }
        Ok(cfg)
    }

    /// A provider for `cfg` on the daemon's shared HTTP client.
    pub(crate) async fn provider(&self, cfg: &Config) -> Result<Provider> {
        Provider::for_profile_with(cfg.provider.clone(), self.paths.clone(), self.http.clone())
            .await
    }

    pub(crate) fn broadcast(&self, ev: Event) {
        send_all(&self.subs, ev);
    }

    /// Run a read-only query on a free reader connection (waiting on the
    /// first one only when all are busy).
    pub(crate) fn with_reader<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let free = self.readers.iter().find_map(|r| r.try_lock().ok());
        let guard = match free {
            Some(g) => g,
            None => self.readers[0]
                .lock()
                .map_err(|_| anyhow!("store poisoned"))?,
        };
        f(&guard)
    }

    pub(crate) fn with_store<T>(&self, f: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let guard = self.store.lock().map_err(|_| anyhow!("store poisoned"))?;
        f(&guard)
    }

    /// Handle one request as a connected client's would be (in-process
    /// callers and tests; `Subscribe` needs a connection).
    pub async fn handle(self: &Arc<Self>, req: Request) -> Result<serde_json::Value> {
        match req {
            Request::Ping => Ok(json!({"pong": true, "protocol": PROTOCOL_VERSION})),
            Request::Status => {
                let sessions = self.with_reader(|s| Ok(s.list_sessions()?.len()))?;
                let auth = self.config().map(|cfg| {
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
            Request::GetSettings => Ok(self.config()?.settings_summary(&self.paths)),
            Request::Shutdown => {
                let token = self.shutdown.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    token.cancel();
                });
                Ok(json!({"shutting_down": true}))
            }
            Request::ListSessions => Ok(serde_json::to_value(
                self.with_reader(|s| s.list_sessions())?,
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
                // Stored outputs go with their Session (SPEC §15).
                self.with_store(|st| crate::artifacts::remove_for_session(st, &id))?;
                let existed = self.with_store(|st| st.delete_session(&id))?;
                self.contexts.lock().unwrap().remove(&id);
                if existed {
                    self.broadcast(Event::SessionDeleted { id: id.clone() });
                }
                Ok(json!({"deleted": existed}))
            }
            Request::GetSession { id } => {
                let (session, messages, queued) = self.with_reader(|st| {
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
                Ok(serde_json::to_value(self.with_reader(|st| {
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
            // Only meaningful per connection (see `handle_conn`).
            Request::Subscribe { .. } => Err(anyhow!("subscribe applies to a client connection")),
            Request::DiffToolVersions { id, from, to } => {
                let d = self
                    .with_reader(|st| st.tool_detail(&id))?
                    .context("tool not found")?;
                let find = |v: i64| d.versions.iter().find(|x| x.version == v);
                let (a, b) = (
                    find(from).with_context(|| format!("no version {from}"))?,
                    find(to).with_context(|| format!("no version {to}"))?,
                );
                Ok(json!({"diff": crate::diff::version_diff(a, b)}))
            }
            Request::ListMcpServers => Ok(serde_json::to_value(self.mcp_servers().await?)?),
            Request::AddMcpServer { name, server } => {
                let server: crate::config::McpServerCfg =
                    serde_json::from_value(server).context("invalid MCP server config")?;
                self.add_mcp_server(&name, server, "user").await
            }
            Request::RemoveMcpServer { name } => self.remove_mcp_server(&name, "user").await,
            Request::RestartMcpServer { name } => {
                let cfg = self.config()?;
                self.mcp.stop(&name).await;
                let tools = self.sync_mcp_server(&cfg, &name).await;
                self.mcp
                    .set_error(&name, tools.as_ref().err().map(|e| format!("{e:#}")));
                self.broadcast(Event::McpServersChanged);
                Ok(
                    json!({"restarted": name, "tools": tools.ok(), "error": self.mcp.last_error(&name)}),
                )
            }
            Request::ListSubagents { session_id } => Ok(serde_json::to_value(
                self.with_reader(|st| st.list_subagents(&session_id))?,
            )?),
            Request::GetSubagent { id } => {
                let (info, messages) = self.with_reader(|st| {
                    let info = st.get_subagent(&id)?.context("sub-agent not found")?;
                    Ok((info, st.subagent_messages(&id)?))
                })?;
                Ok(json!({"subagent": info, "messages": messages}))
            }
            Request::CancelInvocation { handle } => {
                let entry = self
                    .pending_get(&handle)
                    .filter(|e| e.result.lock().unwrap().is_none())
                    .context("no running invocation with that handle")?;
                entry.cancel.cancel();
                Ok(json!({"cancelled": true}))
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
                self.with_reader(|s| s.list_tools(true))?,
            )?),
            Request::ListModules => Ok(serde_json::to_value(
                self.with_reader(|s| s.list_modules())?,
            )?),
            Request::UpdateToolCode { id, code } => {
                // In-TUI editor save: a new immutable version with the same
                // schemas/capabilities/memory as the current version, run
                // through the same syntax + smoke gate as agent edits.
                let (info, ver) = self
                    .with_store(|st| st.get_tool_current(&id))?
                    .context("tool not found")?;
                let cfg = self.config()?;
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
                    .with_reader(|st| st.tool_detail(&id))?
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
                let cfg = self.config()?;
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
                let bundle = self.with_reader(|st| {
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
                self.with_reader(|s| s.list_proposals())?,
            )?),
            Request::ListModelStats => {
                let (stats, usage) =
                    self.with_reader(|s| Ok((s.list_model_stats()?, s.usage_by_model()?)))?;
                let stats: Vec<ModelStatInfo> = stats
                    .into_iter()
                    .map(|s| {
                        let (prompt, completion, cached) =
                            usage.get(&s.model).copied().unwrap_or_default();
                        ModelStatInfo {
                            model: s.model,
                            successes: s.successes,
                            failures: s.failures,
                            total_ms: s.total_ms,
                            prompt_tokens: prompt,
                            completion_tokens: completion,
                            cached_tokens: cached,
                        }
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
        let Ok(cfg) = self.config() else {
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
        let cfg = self.config()?;
        if !cfg.provider.is_configured() {
            return Err(anyhow!(
                "provider not configured; run the onboarding wizard"
            ));
        }
        let provider = self
            .provider(&cfg)
            .await?
            .with_cache_key(&format!("formless-{session_id}"));
        let session = self
            .with_store(|st| st.get_session(session_id))?
            .context("session not found")?;
        let requested_model = session
            .model
            .clone()
            .unwrap_or_else(|| cfg.provider.model.clone());
        let (context_chars, tool_count) = self.with_store(|st| {
            let chars = st.session_content_chars(session_id).unwrap_or(0);
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
            let ctx = self
                .session_context(session_id, context_tokens, cfg.compaction.fraction())?
                .0;
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
                    if let Some(usage) = &r.usage {
                        let _ = self.with_store(|st| {
                            st.record_usage(session_id, run_id, &r.model_used, usage)
                        });
                    }
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
                let Ok(input) = serde_json::from_str::<Value>(&tc.arguments) else {
                    continue;
                };
                let Ok(caps) = bound_caps(&ver, &input) else {
                    continue;
                };
                if !self
                    .ensure_approval(cfg, session_id, &info, &ver, &caps, Some(&input), cancel)
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
            toolreg::ADD_MCP_SERVER => self.agent_add_mcp_server(session_id, &args, cancel).await,
            toolreg::REMOVE_MCP_SERVER => {
                let name = args["name"].as_str().unwrap_or_default().to_string();
                let reason = args["reason"].as_str().unwrap_or("(none given)");
                let (approved, _) = self
                    .ask_user_approval(
                        session_id,
                        "remove_mcp_server",
                        vec![format!("remove MCP server '{name}'")],
                        format!(
                            "The agent wants to remove the MCP server '{name}'.\nReason: {reason}"
                        ),
                        cancel,
                    )
                    .await;
                if !approved {
                    let _ = self.with_store(|st| st.audit("agent", "mcp.remove.denied", &name));
                    "error: the user did not approve removing this MCP server".to_string()
                } else {
                    match self.remove_mcp_server(&name, "agent").await {
                        Ok(v) => v.to_string(),
                        Err(e) => format!("error: {e}"),
                    }
                }
            }
            toolreg::READ_ARTIFACT => match self.read_artifact_impl(session_id, &args) {
                Ok(text) => text,
                Err(e) => format!("error: read_artifact failed: {e}"),
            },
            toolreg::READ_MODULE => match self.read_module_impl(&args) {
                Ok(text) => text,
                Err(e) => format!("error: read_module failed: {e}"),
            },
            toolreg::UNLOAD_TOOLS => {
                let names: Vec<String> = args["names"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect();
                let ids: Vec<String> = names
                    .iter()
                    .filter_map(|n| self.with_store(|st| st.get_tool_by_name(n)).ok().flatten())
                    .map(|(info, _)| info.id)
                    .collect();
                match self.with_store(|st| st.request_unload(session_id, &ids)) {
                    Ok(n) => json!({"will_unload": n, "when": "at the next context compaction"})
                        .to_string(),
                    Err(e) => format!("error: unload_tools failed: {e}"),
                }
            }
            toolreg::READ_TOOL => match self.read_tool_impl(session_id, &args) {
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
                            self.forget_pending(handle);
                            text
                        }
                        None => "error: interrupted while awaiting invocation".to_string(),
                    },
                    None => self.stale_handle_answer(handle),
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
                                self.forget_pending(handle);
                                json!({"status": "completed", "result": text}).to_string()
                            }
                            None => {
                                json!({"status": "pending", "invocation_id": handle}).to_string()
                            }
                        }
                    }
                    None => self.stale_handle_answer(handle),
                }
            }
            toolreg::ABANDON_INVOCATION => {
                let handle = args["invocation_id"].as_str().unwrap_or_default();
                match self.pending_get(handle) {
                    Some(entry) => {
                        entry.abandoned.store(true, Ordering::SeqCst);
                        json!({"abandoned": handle}).to_string()
                    }
                    None => self.stale_handle_answer(handle),
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
        let cfg = match self.config() {
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

    /// Drop a handle from memory and from the persisted record.
    fn forget_pending(&self, handle: &str) {
        self.pending.lock().unwrap().remove(handle);
        let _ = self.with_store(|st| st.remove_pending_handle(handle));
    }

    /// Answer for a handle that is not live in memory: one a restart
    /// interrupted (answered once, then forgotten), else unknown.
    fn stale_handle_answer(&self, handle: &str) -> String {
        match self
            .with_store(|st| st.pending_handle(handle))
            .ok()
            .flatten()
        {
            Some((tool, status)) if status == "interrupted" => {
                let _ = self.with_store(|st| st.remove_pending_handle(handle));
                json!({
                    "status": "interrupted",
                    "invocation_id": handle,
                    "tool": tool,
                    "note": "interrupted by a daemon restart; outcome unknown, not re-run",
                })
                .to_string()
            }
            _ => format!("error: unknown invocation handle '{handle}'"),
        }
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
            cancel: cancel.child_token(),
            order: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                NEXT.fetch_add(1, Ordering::SeqCst)
            },
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
            // The sandbox polls a flag; raise it when this Invocation's token
            // is cancelled (by the Run's interrupt or CancelInvocation).
            let flag = Arc::new(AtomicBool::new(cancel_flag.load(Ordering::SeqCst)));
            let done = CancellationToken::new();
            {
                let (tok, flag, done) = (entry.cancel.clone(), flag.clone(), done.clone());
                tokio::spawn(async move {
                    tokio::select! {
                        _ = tok.cancelled() => flag.store(true, Ordering::SeqCst),
                        _ = done.cancelled() => {}
                    }
                });
            }
            let (tok, cfg2) = (entry.cancel.clone(), cfg.clone());
            tokio::spawn(async move {
                let _done = done.drop_guard();
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
                    this.forget_pending(&handle2);
                }
            });
        }
        let threshold = std::time::Duration::from_secs(cfg.tools.pending_threshold_secs);
        match self.wait_pending(&entry, Some(threshold), cancel).await {
            Some((_ok, text)) => {
                entry.delivered.store(true, Ordering::SeqCst);
                self.forget_pending(&handle);
                text
            }
            None => {
                let _ = self.with_store(|st| st.add_pending_handle(&handle, session_id, name));
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
        let mut ready: Vec<(String, Arc<PendingEntry>, String)> = {
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
        if ready.is_empty() {
            return;
        }
        // One appended note per step, in a fixed order and format, so the
        // transcript (and the cached prefix) is the same however the
        // completions raced.
        ready.sort_by_key(|(_, entry, _)| entry.order);
        let content = std::iter::once("[Pending invocations completed]".to_string())
            .chain(ready.iter().map(|(handle, entry, text)| {
                format!("- {handle} (tool {}): {text}", entry.tool_name)
            }))
            .collect::<Vec<_>>()
            .join("\n");
        if let Ok(msg) =
            self.with_store(|st| st.append_message(session_id, "system-note", &content))
        {
            self.broadcast(Event::MessageAppended {
                session_id: session_id.to_string(),
                message: msg,
            });
        }
        for (handle, entry, _) in ready {
            entry.delivered.store(true, Ordering::SeqCst);
            self.forget_pending(&handle);
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

    /// The system prompt a Session runs with: snapshotted on its first
    /// step, so applying or rolling back learned guidance never rewrites a
    /// running Session's cached prefix (new Sessions, or this one after
    /// compaction, pick it up).
    fn session_prompt(&self, session_id: &str) -> Result<String> {
        if let Some(p) = self.with_store(|st| st.session_system_prompt(session_id))? {
            return Ok(p);
        }
        let p = self.effective_system_prompt();
        self.with_store(|st| st.set_session_system_prompt(session_id, &p))?;
        Ok(p)
    }

    pub(crate) fn effective_system_prompt(&self) -> String {
        self.with_store(|st| st.effective_system_prompt(SYSTEM_PROMPT))
            .unwrap_or_else(|_| SYSTEM_PROMPT.to_string())
    }

    /// Current module code for Invocations: one shared snapshot, re-read
    /// only when a module changed (a cheap fingerprint query otherwise).
    pub(crate) fn module_code(&self) -> ModuleCode {
        let Ok(stamp) = self.with_store(|st| st.modules_fingerprint()) else {
            return Arc::default();
        };
        let mut cache = self.modules.lock().unwrap();
        if let Some((cached, code)) = cache.as_ref() {
            if *cached == stamp {
                return code.clone();
            }
        }
        let code = Arc::new(
            self.with_store(|st| st.list_module_code())
                .unwrap_or_default(),
        );
        *cache = Some((stamp, code.clone()));
        code
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
        let mut modules = (*self.module_code()).clone();
        modules.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        modules.push((name.to_string(), code.clone()));
        let modules = Arc::new(modules);
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
            let provider = self.provider(cfg).await?;
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
            // Visible and stored (not just a local Vec): clients see each
            // step, and the transcript outlives the call.
            let sub = self.with_store(|st| {
                st.create_subagent(session_id, run_id, task, agent_depth as u32)
            })?;
            let sub_id = sub.id.clone();
            for m in &messages {
                let _ = self.with_store(|st| st.append_subagent_message(&sub_id, m));
            }
            self.broadcast(Event::SubagentStarted {
                session_id: session_id.to_string(),
                subagent: sub,
            });
            let record = |m: &Value| {
                let _ = self.with_store(|st| st.append_subagent_message(&sub_id, m));
            };
            let mut step = 0i64;
            // No step limit (SPEC principle 3): it ends on a final answer,
            // a fault, or cancellation.
            let outcome: Result<String> = async {
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
                    step += 1;
                    let _ = self.with_store(|st| st.set_subagent_steps(&sub_id, step));
                    self.broadcast(Event::SubagentStep {
                        session_id: session_id.to_string(),
                        subagent_id: sub_id.clone(),
                        step,
                        tool_calls: result.tool_calls.iter().map(|c| c.name.clone()).collect(),
                    });
                    if result.tool_calls.is_empty() {
                        record(&json!({"role": "assistant", "content": result.content}));
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
                    let mut assistant = json!({"role": "assistant", "content": Value::Null, "tool_calls": calls_json});
                    if !result.reasoning.is_empty() {
                        assistant["reasoning_content"] = json!(result.reasoning);
                    }
                    record(&assistant);
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
                        let msg = json!({"role": "tool", "tool_call_id": tc.id, "content": text});
                        record(&msg);
                        messages.push(msg);
                    }
                }
            }
            .await;
            let (status, result) = match &outcome {
                Ok(text) => ("completed", text.clone()),
                Err(e) => ("failed", e.to_string()),
            };
            if let Ok(sub) = self.with_store(|st| st.finish_subagent(&sub_id, status, &result)) {
                self.broadcast(Event::SubagentFinished {
                    session_id: session_id.to_string(),
                    subagent: sub,
                });
            }
            outcome
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
        let method = self.config()?.tools.search;
        let hits = self.with_store(|st| toolreg::search_tools_with(st, query, limit, &method))?;
        let ids: Vec<String> = hits.iter().map(|t| t.id.clone()).collect();
        self.with_store(|st| st.load_session_tools(session_id, &ids, "found"))?;
        let upgraded = self.refresh_pins(session_id, &ids)?;
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
        // Shared modules match too, so the agent reuses (and reads, with
        // read_module) a helper instead of rewriting it.
        let words: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 3)
            .map(str::to_lowercase)
            .collect();
        let modules: Vec<Value> = self
            .with_store(|st| st.list_modules())?
            .into_iter()
            .filter(|m| !m.retired)
            .filter(|m| {
                let hay = format!("{} {}", m.name, m.description).to_lowercase();
                words.iter().any(|w| hay.contains(w.as_str()))
            })
            .take(limit)
            .map(|m| json!({"name": m.name, "description": m.description, "version": m.current_version}))
            .collect();
        Ok(json!({
            "tools": tools,
            "modules": modules,
            "upgraded": upgraded,
            "note": if tools.is_empty() {
                "No matching Tool; create one."
            } else {
                "These Tools are now callable functions (from your next step); read_tool shows their code."
            }
        })
        .to_string())
    }

    /// Re-list these Tools' current definitions from the next step, and
    /// say which changed since the Session pinned them.
    fn refresh_pins(&self, session_id: &str, tool_ids: &[String]) -> Result<Vec<String>> {
        let dropped = self.with_store(|st| st.unpin_definitions(session_id, Some(tool_ids)))?;
        let mut notes = Vec::new();
        for (id, pinned) in dropped {
            if let Some((info, v)) = self.with_store(|st| st.get_tool_current(&id))? {
                if v.version != pinned {
                    notes.push(format!(
                        "{} was upgraded from v{pinned} to v{}; its listed definition updates from your next step",
                        info.name, v.version
                    ));
                }
            }
        }
        Ok(notes)
    }

    /// The `add_mcp_server` built-in: the user approves the exact command
    /// or URL (and env keys) first; headless, it fails closed.
    async fn agent_add_mcp_server(
        &self,
        session_id: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> String {
        let name = args["name"].as_str().unwrap_or_default().to_string();
        let reason = args["reason"].as_str().unwrap_or("(none given)");
        let strings = |k: &str| -> Vec<String> {
            args[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        };
        let http = args["transport"].as_str() == Some("http");
        let server = crate::config::McpServerCfg {
            transport: if http { "http" } else { "stdio" }.into(),
            command: args["command"].as_str().unwrap_or_default().into(),
            args: strings("args"),
            url: args["url"].as_str().unwrap_or_default().into(),
            env_keys: strings("env_keys"),
            ..Default::default()
        };
        let target = if http {
            format!("url: {}", server.url)
        } else {
            format!(
                "command: {}",
                std::iter::once(server.command.clone())
                    .chain(server.args.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let mut items = vec![target.clone()];
        if !server.env_keys.is_empty() {
            items.push(format!("env: {}", server.env_keys.join(", ")));
        }
        let (approved, _) = self
            .ask_user_approval(
                session_id,
                "add_mcp_server",
                items,
                format!(
                    "The agent wants to add the MCP server '{name}'.\n{target}\nReason: {reason}"
                ),
                cancel,
            )
            .await;
        if !approved {
            let _ = self.with_store(|st| st.audit("agent", "mcp.add.denied", &name));
            return "error: the user did not approve adding this MCP server".into();
        }
        match self.add_mcp_server(&name, server, "agent").await {
            Ok(v) => v.to_string(),
            Err(e) => format!("error: {e}"),
        }
    }

    /// A slice (or grep) of one of this Session's artifacts (SPEC §9.6).
    fn read_artifact_impl(&self, session_id: &str, args: &Value) -> Result<String> {
        let id = args["id"].as_str().context("id required")?;
        let (_, path, _) = self
            .with_store(|st| st.get_artifact(id))?
            .filter(|(owner, ..)| owner.as_deref() == Some(session_id))
            .with_context(|| format!("no artifact '{id}' in this session"))?;
        let text = String::from_utf8_lossy(&std::fs::read(&path)?).into_owned();
        if let Some(needle) = args["grep"].as_str().filter(|g| !g.is_empty()) {
            let needle = needle.to_lowercase();
            let hits: Vec<Value> = text
                .lines()
                .enumerate()
                .filter(|(_, l)| l.to_lowercase().contains(&needle))
                .take(200)
                .map(|(i, l)| json!({"line": i + 1, "text": l.chars().take(500).collect::<String>()}))
                .collect();
            return Ok(json!({"id": id, "matches": hits}).to_string());
        }
        let total = text.chars().count();
        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
        let length = args["length"].as_u64().unwrap_or(20_000).min(100_000) as usize;
        let slice: String = text.chars().skip(offset).take(length).collect();
        let end = (offset + slice.chars().count()).min(total);
        Ok(
            json!({"id": id, "offset": offset, "end": end, "total_chars": total, "text": slice})
                .to_string(),
        )
    }

    fn read_module_impl(&self, args: &Value) -> Result<String> {
        let name = args["name"].as_str().context("name required")?;
        let (info, _) = self
            .with_store(|st| st.get_module_by_name(name))?
            .with_context(|| format!("no module named '{name}'"))?;
        let (versions, dependents) = self.with_store(|st| {
            Ok((
                st.module_versions(&info.id)?,
                st.module_dependents(&info.name)?,
            ))
        })?;
        let want = args["version"].as_i64().unwrap_or(info.current_version);
        let (version, code, ..) = versions
            .iter()
            .find(|v| v.0 == want)
            .with_context(|| format!("module '{name}' has no version {want}"))?;
        Ok(json!({
            "name": info.name,
            "description": info.description,
            "version": version,
            "current_version": info.current_version,
            "retired": info.retired,
            "versions": versions.iter().map(|(v, _, a, c)| json!({"version": v, "author": a, "created_at": c})).collect::<Vec<_>>(),
            "implementation_js": code,
            "required_by": dependents,
        })
        .to_string())
    }

    fn read_tool_impl(&self, session_id: &str, args: &Value) -> Result<String> {
        let name = args["name"].as_str().context("name required")?;
        let (info, _) = self
            .with_store(|st| st.get_tool_by_name(name))?
            .with_context(|| format!("no tool named '{name}'"))?;
        let upgraded = self.refresh_pins(session_id, std::slice::from_ref(&info.id))?;
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
            "upgraded": upgraded,
            "diff": args["diff_from"].as_i64().and_then(|from| {
                detail
                    .versions
                    .iter()
                    .find(|v| v.version == from)
                    .map(|old| crate::diff::version_diff(old, ver))
            }),
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
            .ensure_approval(cfg, session_id, &info, &ver, &caps, None, cancel)
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

    #[allow(clippy::too_many_arguments)]
    async fn ensure_approval(
        self: &Arc<Self>,
        cfg: &Config,
        session_id: &str,
        info: &ToolInfo,
        ver: &ToolVersionInfo,
        caps: &Capabilities,
        input: Option<&Value>,
        cancel: &CancellationToken,
    ) -> bool {
        let needs = caps.is_sensitive() || ver.memory_mb > cfg.tools.default_memory_mb;
        if !needs {
            return true;
        }
        let mut detail = None;
        if let Some(server) = caps.mcp.first() {
            match self
                .mcp_preapproval(cfg, session_id, server, info, ver, input, cancel)
                .await
            {
                McpApproval::Denied => return false,
                McpApproval::Allowed => return true,
                McpApproval::AskTool(d) => detail = Some(d),
            }
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
            detail,
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
                    .ensure_approval(&cfg, &session_id, &info, &ver, &caps, Some(&input), &cancel)
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
            if let Some(server) = caps.mcp.first().cloned() {
                // An MCP tool: the server runs it ((SPEC §7.9)). Cancelling
                // the Invocation drops the request, which tells the server.
                let tool = crate::mcp::server_tool_name(&ver.code)
                    .unwrap_or_default()
                    .to_string();
                let call = self
                    .mcp
                    .call(&self.paths, &cfg.mcp, &server, &tool, input.clone());
                let outcome = tokio::select! {
                    _ = cancel.cancelled() => Err("interrupted".to_string()),
                    r = call => r
                        .map_err(|e| e.to_string())
                        .and_then(|v| crate::mcp::call_result(&v))
                        .map(|v| crate::mcp::untrusted(&server, v)),
                };
                let elapsed = dur(&started);
                let ok = outcome.is_ok();
                let (status, tag, out, err) = match &outcome {
                    Ok(v) => ("success", "", toolreg::redact(v).to_string(), String::new()),
                    Err(e) if cancel.is_cancelled() => {
                        ("cancelled", "user-cancelled", String::new(), e.clone())
                    }
                    Err(e) => ("failure", "mcp", String::new(), e.clone()),
                };
                self.finish_inv(&inv_id, &info, status, tag, &out, &err, elapsed, &cfg);
                self.broadcast(Event::ToolCallFinished {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                    tool_name: name.clone(),
                    ok,
                    elapsed_ms: elapsed,
                });
                return outcome.map(|v| (v, inv_id));
            }
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

    /// The Session's provider context, read incrementally from its cache.
    fn session_context(
        &self,
        session_id: &str,
        context_tokens: u32,
        trigger_fraction: f64,
    ) -> Result<crate::storage::BuiltContext> {
        let prompt = self.session_prompt(session_id)?;
        let mut cache = self
            .contexts
            .lock()
            .unwrap()
            .remove(session_id)
            .unwrap_or_default();
        let built = self.with_store(|st| {
            let built = cache.build(st, session_id, &prompt, context_tokens, trigger_fraction)?;
            let known = st.last_prompt_tokens(session_id)?;
            Ok(cache.with_known_tokens(built, known, context_tokens, trigger_fraction))
        });
        if built.is_ok() {
            self.contexts
                .lock()
                .unwrap()
                .insert(session_id.to_string(), cache);
        }
        built
    }

    async fn maybe_compact(
        &self,
        provider: &Provider,
        cfg: &Config,
        session_id: &str,
        model: &str,
        context_tokens: u32,
    ) -> Result<()> {
        let (_ctx, needs, compact_up_to) =
            self.session_context(session_id, context_tokens, cfg.compaction.fraction())?;
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
        // Append, don't rewrite (§11): the earlier summary stays
        // byte-identical at the front of the system message, so the cached
        // prefix survives the compaction. Only when the accumulated summary
        // grows past a quarter of the context is it condensed as a whole.
        let condense = crate::storage::estimate_tokens(&previous) > context_tokens as usize / 4;
        let input = if previous.is_empty() || !condense {
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
        let summary = if previous.is_empty() || condense {
            summary
        } else {
            format!("{previous}\n\n{}", summary.trim())
        };
        self.with_store(|st| st.save_checkpoint(session_id, &summary, compact_up_to))?;
        // The cached prefix is rebuilt anyway: apply unloads and the cap,
        // list current Tool definitions and adopt the current guidance.
        self.with_store(|st| st.apply_unloads(session_id, cfg.tools.session_max_loaded))?;
        // Re-pick the most-used filler: Tools that became popular since.
        self.with_store(|st| st.reset_filler(session_id))?;
        self.with_store(|st| st.unpin_definitions(session_id, None))?;
        let current = self.effective_system_prompt();
        if self.session_prompt(session_id)? != current {
            self.with_store(|st| st.set_session_system_prompt(session_id, &current))?;
            let note = "The harness guidance in the system prompt was updated (applied or rolled-back proposals).";
            let msg = self.with_store(|st| st.append_message(session_id, "system-note", note))?;
            self.broadcast(Event::MessageAppended {
                session_id: session_id.to_string(),
                message: msg,
            });
        }
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
        let filter = Arc::new(StdMutex::new(EventFilter::default()));
        self.subs.lock().unwrap().push(Subscriber {
            tx: tx.clone(),
            filter: filter.clone(),
        });
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
                    } else if let Request::Subscribe { sessions, deltas } = env.request {
                        // Per connection, so handled here rather than in `handle`.
                        *filter.lock().unwrap() = EventFilter {
                            sessions: sessions.map(|s| s.into_iter().collect()),
                            deltas,
                        };
                        ServerMessage::ok(env.id, json!({"subscribed": true}))
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

    /// A detached daemon writes its stdout/stderr to `daemon.log`; keep
    /// that file within `daemon.log_max_bytes` (SPEC §9.4).
    /// List every enabled MCP server's tools into the registry, then keep
    /// it current (`tools/list_changed`) and stop idle servers.
    async fn mcp_loop(self: Arc<Self>) {
        if let Ok(cfg) = self.config() {
            for name in cfg.mcp.servers.keys() {
                if let Err(e) = self.sync_mcp_server(&cfg, name).await {
                    tracing::warn!("MCP server '{name}': {e}");
                }
            }
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let Ok(cfg) = self.config() else { continue };
            // Only running servers can announce changes; idle ones are
            // never started just to check.
            for name in self.mcp.changed_servers().await {
                if let Err(e) = self.sync_mcp_server(&cfg, &name).await {
                    tracing::warn!("MCP server '{name}': {e}");
                }
            }
            self.mcp.reap_idle(cfg.mcp.idle_secs).await;
        }
    }

    /// Add (or replace) an MCP server and list its tools in at once.
    pub(crate) async fn add_mcp_server(
        &self,
        name: &str,
        server: crate::config::McpServerCfg,
        actor: &str,
    ) -> Result<Value> {
        if name.is_empty()
            || name.contains("__")
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(anyhow!(
                "MCP server names use letters, digits, '-' and '_' (no '__')"
            ));
        }
        let mut cfg = self.config()?;
        cfg.mcp.servers.insert(name.to_string(), server);
        cfg.save(&self.paths)?;
        let _ = self.with_store(|st| st.audit(actor, "mcp.add", name));
        self.mcp.stop(name).await;
        let tools = self.sync_mcp_server(&cfg, name).await;
        self.mcp
            .set_error(name, tools.as_ref().err().map(|e| format!("{e:#}")));
        self.broadcast(Event::McpServersChanged);
        Ok(json!({"added": name, "tools": tools.ok(), "error": self.mcp.last_error(name)}))
    }

    /// Remove an MCP server; its tools are retired (metrics kept).
    pub(crate) async fn remove_mcp_server(&self, name: &str, actor: &str) -> Result<Value> {
        let mut cfg = self.config()?;
        if cfg.mcp.servers.remove(name).is_none() {
            return Err(anyhow!("no MCP server '{name}'"));
        }
        cfg.save(&self.paths)?;
        self.mcp.stop(name).await;
        let retired = self.with_store(|st| crate::mcp::sync_registry(st, name, &[]))?;
        let _ = self.with_store(|st| st.audit(actor, "mcp.remove", name));
        self.broadcast(Event::McpServersChanged);
        Ok(json!({"removed": name, "retired_tools": retired}))
    }

    /// Ask the user to approve something that is not a Tool call (SPEC
    /// §10.3): same modal and fail-closed rule, with `detail` spelling out
    /// exactly what would happen. Nothing is remembered.
    async fn ask_user_approval(
        &self,
        session_id: &str,
        what: &str,
        items: Vec<String>,
        detail: String,
        cancel: &CancellationToken,
    ) -> (bool, ApprovalScope) {
        if self.subscriber_count() == 0 {
            return (false, ApprovalScope::Once);
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
            tool_name: what.to_string(),
            version: 0,
            capabilities: items,
            memory_mb: 0,
            detail: Some(detail),
        });
        let answer = tokio::select! {
            _ = cancel.cancelled() => (false, ApprovalScope::Once),
            r = rx => r.unwrap_or((false, ApprovalScope::Once)),
        };
        self.approvals.lock().unwrap().remove(&approval_id);
        answer
    }

    /// The MCP part of an approval (SPEC §7.9): the server is approved
    /// once ("Always" remembers it); then read-only tools of a trusted
    /// server run freely, and every other tool goes through the normal
    /// per-tool approval with its annotations and input spelled out.
    #[allow(clippy::too_many_arguments)]
    async fn mcp_preapproval(
        &self,
        cfg: &Config,
        session_id: &str,
        server: &str,
        info: &ToolInfo,
        ver: &ToolVersionInfo,
        input: Option<&Value>,
        cancel: &CancellationToken,
    ) -> McpApproval {
        let scfg = cfg.mcp.servers.get(server).cloned().unwrap_or_default();
        let target = if scfg.transport == "http" {
            format!("url: {}", scfg.url)
        } else {
            format!(
                "command: {}",
                std::iter::once(scfg.command.clone())
                    .chain(scfg.args.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        if !self
            .with_store(|st| st.mcp_server_approved(server))
            .unwrap_or(false)
        {
            let (ok, scope) = self
                .ask_user_approval(
                    session_id,
                    &format!("mcp:{server}"),
                    vec![target.clone()],
                    format!("Allow Formless to use the MCP server '{server}'?\n{target}"),
                    cancel,
                )
                .await;
            if !ok {
                let _ = self.with_store(|st| st.audit("user", "mcp.server.deny", server));
                return McpApproval::Denied;
            }
            if scope == ApprovalScope::Always {
                let _ = self.with_store(|st| st.approve_mcp_server(server));
                let _ = self.with_store(|st| st.audit("user", "mcp.server.approve", server));
            }
        }
        let ann = crate::mcp::annotations(&ver.code);
        let read_only = ann["readOnlyHint"].as_bool() == Some(true);
        if read_only && scfg.trusted {
            return McpApproval::Allowed;
        }
        let mut detail = format!(
            "MCP tool {} on server '{server}' ({target})",
            crate::mcp::server_tool_name(&ver.code).unwrap_or(&info.name)
        );
        if ann["destructiveHint"].as_bool() == Some(true) {
            detail.push_str("\n⚠ The server marks this tool as destructive.");
        } else if read_only {
            detail.push_str("\nThe server marks this tool as read-only.");
        }
        if let Some(input) = input {
            let shown: String = input.to_string().chars().take(400).collect();
            detail.push_str(&format!("\ninput: {shown}"));
        }
        McpApproval::AskTool(detail)
    }

    /// Configured MCP servers with their live state.
    async fn mcp_servers(&self) -> Result<Vec<McpServerInfo>> {
        let cfg = self.config()?;
        let tools = self.with_reader(|st| st.list_tools(false))?;
        let mut out = Vec::new();
        for (name, s) in &cfg.mcp.servers {
            let prefix = crate::mcp::registry_name(name, "").to_lowercase();
            let http = s.transport == "http";
            out.push(McpServerInfo {
                name: name.clone(),
                transport: if http { "http" } else { "stdio" }.into(),
                target: if http {
                    s.url.clone()
                } else {
                    std::iter::once(s.command.clone())
                        .chain(s.args.clone())
                        .collect::<Vec<_>>()
                        .join(" ")
                },
                disabled: s.disabled,
                running: self.mcp.is_running(name).await,
                tool_count: tools
                    .iter()
                    .filter(|t| t.name.to_lowercase().starts_with(&prefix))
                    .count(),
                last_error: self.mcp.last_error(name),
                logged_in: http && crate::mcp_auth::has_login(&self.paths, name),
            });
        }
        Ok(out)
    }

    /// Fetch one server's tool list and mirror it into the registry.
    pub(crate) async fn sync_mcp_server(&self, cfg: &Config, name: &str) -> Result<usize> {
        if cfg.mcp.servers.get(name).is_none_or(|s| s.disabled) {
            return Ok(0);
        }
        let conn = self.mcp.connection(&self.paths, &cfg.mcp, name).await?;
        let tools = conn.list_tools().await?;
        self.with_store(|st| crate::mcp::sync_registry(st, name, &tools))?;
        Ok(tools.len())
    }

    /// Apply artifact retention at start and then hourly.
    async fn artifact_gc_loop(self: Arc<Self>) {
        loop {
            if let Ok(cfg) = self.config() {
                match self.with_store(|st| crate::artifacts::gc(st, &cfg.artifacts)) {
                    Ok(c) if c.files > 0 => {
                        tracing::info!("removed {} artifact(s), {} bytes", c.files, c.bytes)
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("artifact retention: {e}"),
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    }

    async fn log_rotator(self: Arc<Self>) {
        if std::env::var_os(crate::logs::DAEMON_LOG_ENV).is_none() {
            return;
        }
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let Ok(cfg) = self.config() else { continue };
            match crate::logs::rotate_if_needed(
                &self.paths,
                cfg.daemon.log_max_bytes,
                cfg.daemon.log_keep,
            ) {
                Ok(true) => {
                    if let Err(e) = crate::logs::reopen_stdio(&self.paths) {
                        tracing::warn!("reopen daemon log after rotation: {e}");
                    }
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("rotate daemon log: {e}"),
            }
        }
    }

    async fn idle_watcher(self: Arc<Self>) {
        let mut idle_since: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
            let grace = self
                .config()
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
            if let Ok(cfg) = self.config() {
                let stall_ms = cfg.daemon.stall_warning_secs.saturating_mul(1000);
                let now = epoch_ms();
                let entries: Vec<(String, Arc<PendingEntry>)> = self
                    .pending
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(h, e)| (h.clone(), e.clone()))
                    .collect();
                for (handle, e) in entries {
                    if e.result.lock().unwrap().is_some() {
                        continue;
                    }
                    let silent_ms = now.saturating_sub(e.activity.load(Ordering::SeqCst));
                    if silent_ms >= stall_ms {
                        if !e.stalled_noted.swap(true, Ordering::SeqCst) {
                            self.broadcast(Event::InvocationStalled {
                                session_id: e.session_id.clone(),
                                handle,
                                tool_name: e.tool_name.clone(),
                                silent_secs: silent_ms / 1000,
                            });
                        }
                    } else if e.stalled_noted.swap(false, Ordering::SeqCst) {
                        self.broadcast(Event::InvocationActivity {
                            session_id: e.session_id.clone(),
                            handle,
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
        let logs = {
            let this = self.clone();
            tokio::spawn(async move { this.log_rotator().await })
        };
        let artifact_gc = {
            let this = self.clone();
            tokio::spawn(async move { this.artifact_gc_loop().await })
        };
        let mcp = {
            let this = self.clone();
            tokio::spawn(async move { this.mcp_loop().await })
        };
        let result = self.accept_loop().await;
        logs.abort();
        artifact_gc.abort();
        mcp.abort();
        self.mcp.stop_all().await;
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
    spawn_daemon_child(paths).map(|_| ())
}

fn spawn_daemon_child(paths: &Paths) -> Result<std::process::Child> {
    let exe = std::env::current_exe().context("locate formless binary")?;
    let cfg = Config::load(paths).unwrap_or_default();
    let _ = crate::logs::rotate_if_needed(paths, cfg.daemon.log_max_bytes, cfg.daemon.log_keep);
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
        .env(crate::logs::DAEMON_LOG_ENV, "1")
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
    let child = cmd.spawn().context("spawn daemon")?;
    // Claim the start at once (serve rewrites the same pid): a second
    // client that arrives during a slow first start waits for this daemon
    // instead of spawning another.
    let _ = std::fs::write(paths.lock_file(), child.id().to_string());
    Ok(child)
}

#[cfg(unix)]
fn libc_setsid() -> i32 {
    extern "C" {
        fn setsid() -> i32;
    }
    unsafe { setsid() }
}

/// Whether a daemon start is in progress: the lock file is fresher than
/// `within` (a stale one, e.g. from before a reboot, may name a reused pid)
/// and names a live process.
fn daemon_starting(paths: &Paths, within: std::time::Duration) -> bool {
    let fresh = std::fs::metadata(paths.lock_file())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < within);
    if !fresh {
        return false;
    }
    let Some(pid) = std::fs::read_to_string(paths.lock_file())
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    else {
        return false;
    };
    #[cfg(unix)]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // Signal 0 only checks that the process exists.
        unsafe { kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// The last `n` lines of the daemon log, for start-up errors.
fn log_tail(paths: &Paths, n: usize) -> String {
    let text = std::fs::read_to_string(paths.log_file()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Connect, auto-starting the daemon if needed. Waits up to
/// `daemon.start_timeout_secs` for it to accept connections, joining a
/// start already in progress rather than racing a second spawn, and fails
/// early (with the log's last lines) if the new daemon exits.
pub fn ensure_daemon(paths: &Paths) -> Result<()> {
    if daemon_reachable(paths) {
        return Ok(());
    }
    let timeout = Config::load(paths)
        .map(|c| c.daemon.start_timeout_secs)
        .unwrap_or(60)
        .max(1);
    let mut child = if daemon_starting(paths, std::time::Duration::from_secs(timeout)) {
        None
    } else {
        Some(spawn_daemon_child(paths)?)
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    while std::time::Instant::now() < deadline {
        if daemon_reachable(paths) {
            return Ok(());
        }
        if let Some(c) = child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                return Err(anyhow!(
                    "daemon exited during start-up ({status}); last log lines from {}:\n{}",
                    paths.log_file().display(),
                    log_tail(paths, 10)
                ));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(anyhow!(
        "daemon did not accept connections within {timeout}s (daemon.start_timeout_secs); \
         last log lines from {}:\n{}",
        paths.log_file().display(),
        log_tail(paths, 10)
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
            .block_on(self.daemon.provider(&self.cfg))
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
