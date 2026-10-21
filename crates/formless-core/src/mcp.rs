//! External MCP servers ((SPEC §7.9)): JSON-RPC clients for configured
//! servers, supervised by the daemon, whose tools appear in the registry as
//! `mcp__<server>__<tool>` entries (listed, approved and measured like
//! Tools; executed by the server, never by the sandbox).

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{oneshot, Mutex};

use crate::config::{McpCfg, McpServerCfg};
use crate::{Paths, Store};

pub const PREFIX: &str = "mcp__";
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Registry name of a server's tool: `mcp__<server>__<tool>`, restricted
/// to the characters provider function names allow (max 64).
pub fn registry_name(server: &str, tool: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let mut name = format!("{PREFIX}{}__{}", clean(server), clean(tool));
    name.truncate(64);
    name
}

/// (server, tool) of a registry name, if it stands for an MCP tool. The
/// tool part is the server's own name for it, looked up on the server.
pub fn split_name(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix(PREFIX)?.split_once("__")
}

/// A tool as a server lists it.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Environment a stdio server starts with: a minimal base, its literal
/// `env`, and the daemon variables it names in `env_keys` (never
/// `FORMLESS_*`; see the `process.run` rule in SPEC §7.4).
fn server_env(cfg: &McpServerCfg) -> Vec<(String, String)> {
    const BASE: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "TERM",
        "TZ",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SYSTEMROOT",
        "SYSTEMDRIVE",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
    ];
    let mut out: Vec<(String, String)> = BASE
        .iter()
        .copied()
        .chain(cfg.env_keys.iter().map(String::as_str))
        .filter(|k| !k.to_ascii_uppercase().starts_with("FORMLESS_"))
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    out.extend(cfg.env.iter().map(|(k, v)| (k.clone(), v.clone())));
    out
}

/// One running stdio server: newline-delimited JSON-RPC over its stdin and
/// stdout; stderr goes to `<data>/mcp/<server>.log`.
pub struct Connection {
    child: StdMutex<Option<tokio::process::Child>>,
    stdin: Mutex<tokio::process::ChildStdin>,
    pending: Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    last_used: AtomicU64,
    /// Set when the server announces `notifications/tools/list_changed`.
    pub tools_changed: Arc<AtomicBool>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Connection {
    /// Spawn the server and complete the MCP initialize handshake.
    pub async fn start(paths: &Paths, name: &str, cfg: &McpServerCfg) -> Result<Arc<Self>> {
        if cfg.command.is_empty() {
            return Err(anyhow!("MCP server '{name}' has no command"));
        }
        let log_dir = paths.data_dir.join("mcp");
        std::fs::create_dir_all(&log_dir)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir.join(format!("{name}.log")))?;
        let mut cmd = tokio::process::Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .env_clear()
            .envs(server_env(cfg))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(log)
            .kill_on_drop(true);
        if let Some(dir) = &cfg.cwd {
            cmd.current_dir(dir);
        }
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("start MCP server '{name}' ({})", cfg.command))?;
        let stdin = child.stdin.take().context("server stdin")?;
        let stdout = child.stdout.take().context("server stdout")?;
        let conn = Arc::new(Self {
            child: StdMutex::new(Some(child)),
            stdin: Mutex::new(stdin),
            pending: Default::default(),
            next_id: AtomicU64::new(1),
            alive: Arc::new(AtomicBool::new(true)),
            last_used: AtomicU64::new(now_secs()),
            tools_changed: Default::default(),
        });
        // Reader: answers, server notifications, and the server's own pings.
        {
            let (pending, alive, changed) = (
                conn.pending.clone(),
                conn.alive.clone(),
                conn.tools_changed.clone(),
            );
            let weak = Arc::downgrade(&conn);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // Servers may print non-protocol lines; skip them.
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    match (msg.get("id").and_then(Value::as_u64), msg.get("method")) {
                        (Some(id), None) => {
                            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                                let out = match msg.get("error") {
                                    Some(e) => Err(e["message"]
                                        .as_str()
                                        .unwrap_or("MCP error")
                                        .to_string()),
                                    None => Ok(msg["result"].clone()),
                                };
                                let _ = tx.send(out);
                            }
                        }
                        (_, Some(m)) if m == "notifications/tools/list_changed" => {
                            changed.store(true, Ordering::SeqCst);
                        }
                        (_, Some(m)) if msg.get("id").is_some() => {
                            // A request from the server: only `ping` is supported.
                            let reply = if m == "ping" {
                                json!({"jsonrpc": "2.0", "id": msg["id"], "result": {}})
                            } else {
                                json!({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "method not supported"}})
                            };
                            if let Some(c) = weak.upgrade() {
                                let _ = c.write(&reply).await;
                            }
                        }
                        _ => {}
                    }
                }
                alive.store(false, Ordering::SeqCst);
                // Fail whatever was waiting: the server is gone.
                pending.lock().unwrap().clear();
            });
        }
        conn.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "formless", "version": env!("CARGO_PKG_VERSION")}
            }),
        )
        .await
        .with_context(|| format!("initialize MCP server '{name}'"))?;
        conn.notify("notifications/initialized", json!({})).await?;
        Ok(conn)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    async fn write(&self, msg: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(format!("{msg}\n").as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    /// Send a request and wait for its answer. Dropping the future (the
    /// call was cancelled) tells the server with `notifications/cancelled`.
    pub async fn request(self: &Arc<Self>, method: &str, params: Value) -> Result<Value> {
        if !self.is_alive() {
            return Err(anyhow!("MCP server is not running"));
        }
        self.last_used.store(now_secs(), Ordering::SeqCst);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        struct CancelOnDrop(Option<(Arc<Connection>, u64)>);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                if let Some((conn, id)) = self.0.take() {
                    conn.pending.lock().unwrap().remove(&id);
                    tokio::spawn(async move {
                        let _ = conn
                            .notify(
                                "notifications/cancelled",
                                json!({"requestId": id, "reason": "cancelled by the user"}),
                            )
                            .await;
                    });
                }
            }
        }
        let mut guard = CancelOnDrop(Some((self.clone(), id)));
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        let answer = rx
            .await
            .map_err(|_| anyhow!("MCP server stopped before answering"))?;
        guard.0 = None;
        answer.map_err(|e| anyhow!("{e}"))
    }

    pub async fn list_tools(self: &Arc<Self>) -> Result<Vec<McpTool>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let page = self.request("tools/list", params).await?;
            for t in page["tools"].as_array().into_iter().flatten() {
                out.push(McpTool {
                    name: t["name"].as_str().unwrap_or_default().to_string(),
                    description: t["description"].as_str().unwrap_or_default().to_string(),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object"})),
                });
            }
            cursor = page["nextCursor"].as_str().map(str::to_string);
            if cursor.is_none() {
                return Ok(out);
            }
        }
    }

    /// Stop the server (its whole process group).
    pub fn stop(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                extern "C" {
                    fn kill(pid: i32, sig: i32) -> i32;
                }
                // SAFETY: plain syscall; the server leads its own group.
                unsafe {
                    kill(-(pid as i32), 9);
                }
            }
            let _ = child.start_kill();
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The result of a `tools/call`, as the model sees it: structured content
/// when the server gives it, else the text parts. `isError` results fail.
pub fn call_result(result: &Value) -> std::result::Result<Value, String> {
    let text: Vec<&str> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect();
    if result["isError"].as_bool() == Some(true) {
        return Err(if text.is_empty() {
            "the MCP tool reported an error".into()
        } else {
            text.join("\n")
        });
    }
    if let Some(s) = result.get("structuredContent").filter(|s| !s.is_null()) {
        return Ok(s.clone());
    }
    match text.as_slice() {
        [one] => Ok(json!(one)),
        [] => Ok(result["content"].clone()),
        many => Ok(json!(many.join("\n"))),
    }
}

/// Running servers, started on first use and stopped when idle.
#[derive(Default)]
pub struct Manager {
    conns: Mutex<HashMap<String, Arc<Connection>>>,
}

impl Manager {
    /// The server's live connection, (re)starting it if needed: a crashed
    /// server is simply started again on the next call.
    pub async fn connection(
        &self,
        paths: &Paths,
        cfg: &McpCfg,
        server: &str,
    ) -> Result<Arc<Connection>> {
        let scfg = cfg
            .servers
            .get(server)
            .filter(|s| !s.disabled)
            .with_context(|| format!("no enabled MCP server '{server}'"))?;
        if !scfg.transport.is_empty() && scfg.transport != "stdio" {
            return Err(anyhow!(
                "MCP transport '{}' is not supported yet",
                scfg.transport
            ));
        }
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(server).filter(|c| c.is_alive()) {
            return Ok(c.clone());
        }
        let conn = Connection::start(paths, server, scfg).await?;
        conns.insert(server.to_string(), conn.clone());
        Ok(conn)
    }

    pub async fn call(
        &self,
        paths: &Paths,
        cfg: &McpCfg,
        server: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value> {
        let conn = self.connection(paths, cfg, server).await?;
        conn.request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
    }

    /// Stop servers idle for `idle_secs` (they restart on the next call).
    pub async fn reap_idle(&self, idle_secs: u64) {
        let now = now_secs();
        self.conns.lock().await.retain(|_, c| {
            let keep =
                c.is_alive() && now.saturating_sub(c.last_used.load(Ordering::SeqCst)) < idle_secs;
            if !keep {
                c.stop();
            }
            keep
        });
    }

    /// Running servers that announced a changed tool list since asked.
    pub async fn changed_servers(&self) -> Vec<String> {
        self.conns
            .lock()
            .await
            .iter()
            .filter(|(_, c)| c.is_alive() && c.tools_changed.swap(false, Ordering::SeqCst))
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub async fn stop_all(&self) {
        for (_, c) in self.conns.lock().await.drain() {
            c.stop();
        }
    }
}

/// Bring the registry in line with a server's tool list: new tools are
/// added, changed ones get a new version, vanished ones are retired.
pub fn sync_registry(store: &Store, server: &str, tools: &[McpTool]) -> Result<usize> {
    let caps = formless_runtime::Capabilities::parse(&[format!("mcp:{server}")])
        .map_err(|e| anyhow!(e))?;
    let mut seen = std::collections::HashSet::new();
    for t in tools {
        let name = registry_name(server, &t.name);
        seen.insert(name.to_lowercase());
        let schema = t.input_schema.to_string();
        // The server's own tool name rides in the version's code field (never
        // executed): registry names are sanitised, calls need the original.
        let code = format!("// MCP tool {}", t.name);
        match store.get_tool_by_name(&name)? {
            Some((info, v))
                if info.description == t.description
                    && v.input_schema == schema
                    && v.code == code =>
            {
                if info.retired {
                    store.set_tool_retired(&info.id, false)?;
                }
            }
            Some((info, _)) => {
                if info.retired {
                    store.set_tool_retired(&info.id, false)?;
                }
                store.create_tool_version(
                    &name,
                    &t.description,
                    &schema,
                    "{}",
                    &code,
                    &caps,
                    256,
                    "mcp",
                )?;
            }
            None => {
                store.create_tool_version(
                    &name,
                    &t.description,
                    &schema,
                    "{}",
                    &code,
                    &caps,
                    256,
                    "mcp",
                )?;
            }
        }
    }
    let prefix = registry_name(server, "").to_lowercase();
    let mut retired = 0;
    for t in store.list_tools(false)? {
        let lower = t.name.to_lowercase();
        if lower.starts_with(&prefix) && !seen.contains(&lower) {
            store.set_tool_retired(&t.id, true)?;
            retired += 1;
        }
    }
    Ok(retired)
}

/// The server's own name for a registry MCP tool (from its version code).
pub fn server_tool_name(code: &str) -> Option<&str> {
    code.strip_prefix("// MCP tool ")
}
