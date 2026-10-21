//! External MCP servers (SPEC §7.9): JSON-RPC clients for configured
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

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// How messages reach the server.
enum Io {
    /// A child process: newline-delimited JSON-RPC on stdin/stdout.
    Stdio {
        stdin: Mutex<tokio::process::ChildStdin>,
        child: StdMutex<Option<tokio::process::Child>>,
    },
    /// Streamable HTTP: every message is a POST; an answer comes back as
    /// the JSON body or on an SSE body. The session id rides on a header.
    Http {
        client: reqwest::Client,
        url: String,
        headers: Vec<(String, String)>,
        session: StdMutex<Option<String>>,
        /// For OAuth tokens (`formless mcp login`): (paths, server name).
        auth: (Paths, String),
    },
    /// Legacy HTTP+SSE: answers arrive on a long-lived GET event stream;
    /// messages are POSTed to the endpoint it announced.
    LegacySse {
        client: reqwest::Client,
        post_url: String,
        headers: Vec<(String, String)>,
        reader: tokio::task::JoinHandle<()>,
    },
}

/// One live server connection (any transport).
pub struct Connection {
    io: Io,
    pending: Pending,
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

/// Route one message from the server: an answer to a waiting request, a
/// tools-changed notice, or a server request (answered via `reply`).
fn route(msg: &Value, pending: &Pending, changed: &AtomicBool) -> Option<Value> {
    match (msg.get("id").and_then(Value::as_u64), msg.get("method")) {
        (Some(id), None) => {
            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                let out = match msg.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("MCP error").to_string()),
                    None => Ok(msg["result"].clone()),
                };
                let _ = tx.send(out);
            }
            None
        }
        (_, Some(m)) if m == "notifications/tools/list_changed" => {
            changed.store(true, Ordering::SeqCst);
            None
        }
        // A request from the server: only `ping` is supported.
        (_, Some(m)) if msg.get("id").is_some() => Some(if m == "ping" {
            json!({"jsonrpc": "2.0", "id": msg["id"], "result": {}})
        } else {
            json!({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "method not supported"}})
        }),
        _ => None,
    }
}

/// JSON messages in an SSE body (`data:` lines, events split by a blank line).
fn sse_messages(body: &str) -> Vec<Value> {
    body.split("\n\n")
        .filter_map(|event| {
            let data: Vec<&str> = event
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                .collect();
            serde_json::from_str(&data.join("\n")).ok()
        })
        .collect()
}

fn with_headers(
    mut req: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    for (k, v) in headers {
        req = req.header(k, v);
    }
    req
}

impl Connection {
    /// Connect to a configured server and complete the MCP handshake.
    pub async fn start(paths: &Paths, name: &str, cfg: &McpServerCfg) -> Result<Arc<Self>> {
        let conn = match cfg.transport.as_str() {
            "" | "stdio" => Self::spawn_stdio(paths, name, cfg)?,
            "http" => {
                let conn = Self::http(paths, name, cfg)?;
                match conn.initialize().await {
                    Ok(()) => return Ok(conn),
                    // An older server: fall back to HTTP+SSE.
                    Err(e) if e.to_string().contains("legacy") => Self::legacy_sse(cfg).await?,
                    Err(e) => {
                        return Err(e).with_context(|| format!("initialize MCP server '{name}'"))
                    }
                }
            }
            other => return Err(anyhow!("unknown MCP transport '{other}'")),
        };
        conn.initialize()
            .await
            .with_context(|| format!("initialize MCP server '{name}'"))?;
        Ok(conn)
    }

    async fn initialize(self: &Arc<Self>) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "formless", "version": env!("CARGO_PKG_VERSION")}
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    fn new(
        io: Io,
        pending: Pending,
        alive: Arc<AtomicBool>,
        changed: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            io,
            pending,
            next_id: AtomicU64::new(1),
            alive,
            last_used: AtomicU64::new(now_secs()),
            tools_changed: changed,
        })
    }

    fn spawn_stdio(paths: &Paths, name: &str, cfg: &McpServerCfg) -> Result<Arc<Self>> {
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
        let (pending, alive, changed): (Pending, _, _) = (
            Default::default(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
        );
        let conn = Self::new(
            Io::Stdio {
                stdin: Mutex::new(stdin),
                child: StdMutex::new(Some(child)),
            },
            pending.clone(),
            alive.clone(),
            changed.clone(),
        );
        let weak = Arc::downgrade(&conn);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Servers may print non-protocol lines; skip them.
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(reply) = route(&msg, &pending, &changed) {
                    if let Some(c) = weak.upgrade() {
                        let _ = c.write(&reply).await;
                    }
                }
            }
            alive.store(false, Ordering::SeqCst);
            // Fail whatever was waiting: the server is gone.
            pending.lock().unwrap().clear();
        });
        Ok(conn)
    }

    fn http(paths: &Paths, name: &str, cfg: &McpServerCfg) -> Result<Arc<Self>> {
        if cfg.url.is_empty() {
            return Err(anyhow!("an http MCP server needs a url"));
        }
        Ok(Self::new(
            Io::Http {
                client: reqwest::Client::new(),
                url: cfg.url.clone(),
                headers: cfg.headers.clone().into_iter().collect(),
                session: StdMutex::new(None),
                auth: (paths.clone(), name.to_string()),
            },
            Default::default(),
            Arc::new(AtomicBool::new(true)),
            Default::default(),
        ))
    }

    /// Legacy HTTP+SSE: open the event stream, wait for the `endpoint`
    /// event, then read answers from the stream in the background.
    async fn legacy_sse(cfg: &McpServerCfg) -> Result<Arc<Self>> {
        let client = reqwest::Client::new();
        let headers: Vec<(String, String)> = cfg.headers.clone().into_iter().collect();
        let resp = with_headers(client.get(&cfg.url), &headers)
            .header("accept", "text/event-stream")
            .send()
            .await?
            .error_for_status()?;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let endpoint = loop {
            let chunk = futures_util::StreamExt::next(&mut stream)
                .await
                .context("SSE stream ended before the endpoint event")??;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if let Some(end) = buf.find("\n\n") {
                let event: String = buf.drain(..end + 2).collect();
                if event.lines().any(|l| l.trim() == "event: endpoint") {
                    let data = event
                        .lines()
                        .find_map(|l| l.strip_prefix("data:"))
                        .context("endpoint event without data")?;
                    break data.trim().to_string();
                }
            }
        };
        let post_url = reqwest::Url::parse(&cfg.url)?.join(&endpoint)?.to_string();
        let (pending, alive, changed): (Pending, _, _) = (
            Default::default(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicBool::new(false)),
        );
        let reader = {
            let (pending, alive, changed) = (pending.clone(), alive.clone(), changed.clone());
            let (client, post, headers) = (client.clone(), post_url.clone(), headers.clone());
            tokio::spawn(async move {
                while let Some(Ok(chunk)) = futures_util::StreamExt::next(&mut stream).await {
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(end) = buf.find("\n\n") {
                        let event: String = buf.drain(..end + 2).collect();
                        for msg in sse_messages(&event) {
                            if let Some(reply) = route(&msg, &pending, &changed) {
                                let _ = with_headers(client.post(&post), &headers)
                                    .json(&reply)
                                    .send()
                                    .await;
                            }
                        }
                    }
                }
                alive.store(false, Ordering::SeqCst);
                pending.lock().unwrap().clear();
            })
        };
        Ok(Self::new(
            Io::LegacySse {
                client,
                post_url,
                headers,
                reader,
            },
            pending,
            alive,
            changed,
        ))
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Send a message whose answer (if any) arrives through `route`.
    async fn write(&self, msg: &Value) -> Result<()> {
        match &self.io {
            Io::Stdio { stdin, .. } => {
                let mut stdin = stdin.lock().await;
                stdin.write_all(format!("{msg}\n").as_bytes()).await?;
                stdin.flush().await?;
            }
            Io::LegacySse {
                client,
                post_url,
                headers,
                ..
            } => {
                with_headers(client.post(post_url), headers)
                    .json(msg)
                    .send()
                    .await?
                    .error_for_status()?;
            }
            Io::Http { .. } => {
                self.http_post(msg, None).await?;
            }
        }
        Ok(())
    }

    /// Streamable HTTP: POST one message; with `want`, read its answer
    /// from the JSON or SSE body. 429/5xx are retried with jittered backoff.
    async fn http_post(&self, msg: &Value, want: Option<u64>) -> Result<Option<Value>> {
        let Io::Http {
            client,
            url,
            headers,
            session,
            auth: (paths, server),
        } = &self.io
        else {
            return Err(anyhow!("not an HTTP connection"));
        };
        // A stored OAuth login supplies the bearer token, unless the config
        // sets Authorization itself.
        let fixed_auth = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
        let mut bearer = if fixed_auth {
            None
        } else {
            crate::mcp_auth::bearer(paths, server).await?
        };
        let mut refreshed = false;
        let mut attempt = 0u32;
        let resp = loop {
            let mut req = with_headers(client.post(url), headers)
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", PROTOCOL_VERSION)
                .json(msg);
            if let Some(id) = session.lock().unwrap().clone() {
                req = req.header("mcp-session-id", id);
            }
            if let Some(token) = &bearer {
                req = req.bearer_auth(token);
            }
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            if status == 401 && !fixed_auth {
                if bearer.is_some() && !refreshed {
                    refreshed = true;
                    bearer = Some(crate::mcp_auth::refresh(paths, server).await?);
                    continue;
                }
                return Err(anyhow!(
                    "MCP server '{server}' needs authorization: run `formless mcp login {server}`"
                ));
            }
            if (status == 429 || resp.status().is_server_error()) && attempt < 3 {
                let wait = 250u64 << attempt;
                tokio::time::sleep(std::time::Duration::from_millis(
                    wait / 2 + rand::random::<u64>() % (wait / 2 + 1),
                ))
                .await;
                attempt += 1;
                continue;
            }
            // Servers without Streamable HTTP reject the initialize POST.
            if msg["method"] == "initialize" && matches!(status, 404 | 405) {
                return Err(anyhow!("server speaks the legacy HTTP+SSE transport"));
            }
            break resp.error_for_status()?;
        };
        if let Some(id) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *session.lock().unwrap() = Some(id.to_string());
        }
        let Some(want) = want else {
            return Ok(None);
        };
        let is_sse = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.contains("text/event-stream"));
        let body = resp.text().await?;
        let messages = if is_sse {
            sse_messages(&body)
        } else {
            match serde_json::from_str::<Value>(&body)? {
                Value::Array(batch) => batch,
                one => vec![one],
            }
        };
        let mut answer = None;
        for m in messages {
            if m["id"].as_u64() == Some(want) && m.get("method").is_none() {
                answer = Some(m);
            } else if let Some(reply) = route(&m, &self.pending, &self.tools_changed) {
                let _ = Box::pin(self.http_post(&reply, None)).await;
            }
        }
        Ok(answer)
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
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let mut guard = CancelOnDrop(Some((self.clone(), id)));
        let answer = if let Io::Http { .. } = self.io {
            let reply = self
                .http_post(&msg, Some(id))
                .await?
                .context("the server sent no answer")?;
            match reply.get("error") {
                Some(e) => Err(e["message"].as_str().unwrap_or("MCP error").to_string()),
                None => Ok(reply["result"].clone()),
            }
        } else {
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().insert(id, tx);
            self.write(&msg).await?;
            rx.await
                .map_err(|_| anyhow!("MCP server stopped before answering"))?
        };
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

    /// Stop the server: kill a stdio process group, close an event stream.
    pub fn stop(&self) {
        self.alive.store(false, Ordering::SeqCst);
        match &self.io {
            Io::Stdio { child, .. } => {
                if let Some(mut child) = child.lock().unwrap().take() {
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
            Io::LegacySse { reader, .. } => reader.abort(),
            Io::Http { .. } => {}
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
