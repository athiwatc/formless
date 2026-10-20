//! QuickJS Tool runtime (SPEC §7.5): single-file plain-JS Tools with zero
//! ambient authority. Every privileged effect goes through one native
//! `__host(op, argsJson)` bridge, checked against declared Capabilities.

pub mod caps;
pub use caps::Capabilities;

use anyhow::{anyhow, Result};
use rquickjs::{Context, Function, Runtime};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMode {
    Live,
    /// Smoke test: net/process stubbed, fs-writes discarded, env empty (§7.1).
    Smoke,
    /// Optimiser replay (SPEC §8.4): fs-writes to a throwaway overlay,
    /// network and process denied outright, env empty (no real secrets).
    Replay,
}

impl HostMode {
    pub fn is_overlay(self) -> bool {
        matches!(self, HostMode::Smoke | HostMode::Replay)
    }
}

pub type Overlay = Arc<Mutex<std::collections::HashMap<String, String>>>;

pub fn new_overlay() -> Overlay {
    Arc::new(Mutex::new(std::collections::HashMap::new()))
}

/// Operations only the daemon can provide (nested Tools, artifact emit).
pub trait HostOps: Send + Sync {
    fn call_tool(&self, _name: &str, _input: Value) -> Result<Value, String> {
        Err("tools.call is not available in this context".into())
    }
    fn emit(&self, _chunk: &str) -> Result<(), String> {
        Ok(())
    }
}

pub struct NoHostOps;
impl HostOps for NoHostOps {}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub code: String,
    pub input: Value,
    pub caps: Capabilities,
    pub memory_mb: u32,
    pub mode: HostMode,
    pub cancel: Arc<AtomicBool>,
    /// Shared write overlay for Smoke/Replay (nested tools share it, §8.4).
    pub overlay: Option<Overlay>,
    /// Last-activity heartbeat (epoch millis), updated per host call (§6.1).
    pub activity: Option<Arc<std::sync::atomic::AtomicU64>>,
}

#[derive(Debug)]
pub struct ExecOutcome {
    pub output: Value,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecErrorKind {
    Js,
    Denied,
    Resource,
    Cancelled,
}

#[derive(Debug)]
pub struct ExecError {
    pub kind: ExecErrorKind,
    pub message: String,
}
impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ExecError {}

struct State {
    caps: Capabilities,
    mode: HostMode,
    ops: Arc<dyn HostOps>,
    logs: Mutex<Vec<String>>,
    /// Smoke/Replay write overlay: discarded writes stay readable within
    /// the same run so write-then-read tools work (§8.4).
    smoke_fs: Overlay,
}

const PRELUDE: &str = r#"
function __call(op, args) {
  const r = JSON.parse(__host(op, JSON.stringify(args || {})));
  if (!r.ok) throw new Error(r.error);
  return r.value;
}
globalThis.console = {
  log: (...a) => __call("log", { msg: a.map(x => typeof x === "string" ? x : JSON.stringify(x)).join(" ") }),
  info: (...a) => __call("log", { msg: a.map(x => typeof x === "string" ? x : JSON.stringify(x)).join(" ") }),
  error: (...a) => __call("log", { msg: a.map(x => typeof x === "string" ? x : JSON.stringify(x)).join(" ") }),
};
globalThis.fetch = (url, opts) => {
  const r = __call("fetch", { url: String(url), method: (opts && opts.method) || "GET", body: (opts && opts.body) || null, headers: (opts && opts.headers) || {} });
  return { status: r.status, ok: r.status >= 200 && r.status < 300, body: r.body, text: () => r.body, json: () => JSON.parse(r.body) };
};
globalThis.fetchChunks = (url, opts, size, cb) => {
  const r = fetch(url, opts); const b = r.body; const n = size || 65536;
  for (let i = 0; i < b.length; i += n) cb(b.slice(i, i + n));
  return r.status;
};
globalThis.fs = {
  read: (p) => __call("fs_read", { path: String(p) }),
  write: (p, c) => __call("fs_write", { path: String(p), content: String(c) }),
  exists: (p) => __call("fs_exists", { path: String(p) }),
  list: (p) => __call("fs_list", { path: String(p) }),
  readChunks: (p, size, cb) => {
    let off = 0;
    while (true) {
      const r = __call("fs_chunk", { path: String(p), offset: off, size: size || 65536 });
      if (r.data) cb(r.data);
      if (r.eof) break;
      off = r.next_offset;
    }
  },
  readLines: (p, cb) => {
    let buf = "";
    fs.readChunks(p, 65536, (chunk) => {
      buf += chunk;
      let i;
      while ((i = buf.indexOf("\n")) >= 0) { cb(buf.slice(0, i)); buf = buf.slice(i + 1); }
    });
    if (buf.length) cb(buf);
  },
};
globalThis.env = { get: (k) => __call("env_get", { key: String(k) }) };
globalThis.process = { run: (cmd, args) => __call("process_run", { cmd: String(cmd), args: args || [] }) };
globalThis.tools = { call: (name, input) => __call("tools_call", { name: String(name), input: input || {} }) };
globalThis.emit = (chunk) => __call("emit", { chunk: String(chunk) });
globalThis.sleep = (ms) => __call("sleep", { ms: ms | 0 });
globalThis.setTimeout = (fn, ms) => { sleep(ms || 0); if (fn) fn(); return 0; };
globalThis.cryptoRandom = () => __call("crypto_random", {});
"#;

fn denied(msg: impl Into<String>) -> String {
    format!("denied: {}", msg.into())
}

impl State {
    fn dispatch(&self, op: &str, a: &Value) -> Result<Value, String> {
        match op {
            "log" => {
                if let Some(m) = a["msg"].as_str() {
                    if let Ok(mut g) = self.logs.lock() {
                        g.push(m.to_string());
                    }
                }
                Ok(Value::Null)
            }
            "sleep" => {
                let ms = a["ms"].as_u64().unwrap_or(0).min(60_000);
                std::thread::sleep(std::time::Duration::from_millis(ms));
                Ok(Value::Null)
            }
            "crypto_random" => Ok(json!(uuid::Uuid::new_v4().to_string())),
            "emit" => {
                self.ops.emit(a["chunk"].as_str().unwrap_or_default())?;
                Ok(Value::Null)
            }
            "tools_call" => {
                if self.mode == HostMode::Smoke {
                    return Err(denied("tools.call in smoke test"));
                }
                self.ops
                    .call_tool(a["name"].as_str().unwrap_or_default(), a["input"].clone())
            }
            "env_get" => {
                let key = a["key"].as_str().unwrap_or_default();
                if !self.caps.env.iter().any(|k| k == key) {
                    return Err(denied(format!("env:{key} not declared")));
                }
                if self.mode.is_overlay() {
                    return Ok(json!(""));
                }
                Ok(json!(std::env::var(key).unwrap_or_default()))
            }
            "fetch" => self.fetch(a),
            "process_run" => self.process_run(a),
            "fs_read" => {
                let p = self.resolve(
                    a["path"].as_str().unwrap_or_default(),
                    &self.caps.fs_read,
                    false,
                )?;
                if self.mode.is_overlay() {
                    if let Some(s) = self
                        .smoke_fs
                        .lock()
                        .ok()
                        .and_then(|g| g.get(&p.to_string_lossy().to_string()).cloned())
                    {
                        return Ok(json!(s));
                    }
                }
                std::fs::read_to_string(&p)
                    .map(|s| json!(s))
                    .map_err(|e| e.to_string())
            }
            "fs_write" => {
                let content = a["content"].as_str().unwrap_or_default();
                if self.mode.is_overlay() {
                    // Capability must still be declared; write lands in the
                    // overlay (readable this run, never touches disk) (§8.4).
                    let p = self.resolve(
                        a["path"].as_str().unwrap_or_default(),
                        &self.caps.fs_write,
                        true,
                    )?;
                    if let Ok(mut g) = self.smoke_fs.lock() {
                        g.insert(p.to_string_lossy().to_string(), content.to_string());
                    }
                    return Ok(json!(content.len()));
                }
                let p = self.resolve(
                    a["path"].as_str().unwrap_or_default(),
                    &self.caps.fs_write,
                    true,
                )?;
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&p, content).map_err(|e| e.to_string())?;
                Ok(json!(content.len()))
            }
            "fs_exists" => {
                let p = self.resolve(
                    a["path"].as_str().unwrap_or_default(),
                    &self.caps.fs_read,
                    false,
                );
                let in_overlay = self.mode.is_overlay()
                    && p.as_ref()
                        .map(|p| {
                            self.smoke_fs
                                .lock()
                                .map(|g| g.contains_key(&p.to_string_lossy().to_string()))
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                Ok(json!(in_overlay || p.map(|p| p.exists()).unwrap_or(false)))
            }
            "fs_list" => {
                let p = self.resolve(
                    a["path"].as_str().unwrap_or_default(),
                    &self.caps.fs_read,
                    false,
                )?;
                let mut names = Vec::new();
                for e in std::fs::read_dir(&p).map_err(|e| e.to_string())? {
                    names.push(
                        e.map_err(|e| e.to_string())?
                            .file_name()
                            .to_string_lossy()
                            .to_string(),
                    );
                }
                Ok(json!(names))
            }
            "fs_chunk" => {
                let p = self.resolve(
                    a["path"].as_str().unwrap_or_default(),
                    &self.caps.fs_read,
                    false,
                )?;
                let offset = a["offset"].as_u64().unwrap_or(0);
                let size = a["size"].as_u64().unwrap_or(65536).clamp(1, 1 << 20) as usize;
                if self.mode.is_overlay() {
                    if let Some(s) = self
                        .smoke_fs
                        .lock()
                        .ok()
                        .and_then(|g| g.get(&p.to_string_lossy().to_string()).cloned())
                    {
                        let bytes = s.as_bytes();
                        let start = (offset as usize).min(bytes.len());
                        let end = (start + size).min(bytes.len());
                        return Ok(
                            json!({"data": String::from_utf8_lossy(&bytes[start..end]), "next_offset": end as u64, "eof": end >= bytes.len()}),
                        );
                    }
                }
                let mut f = std::fs::File::open(&p).map_err(|e| e.to_string())?;
                f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
                let mut buf = vec![0u8; size];
                let n = f.read(&mut buf).map_err(|e| e.to_string())?;
                buf.truncate(n);
                Ok(
                    json!({"data": String::from_utf8_lossy(&buf), "next_offset": offset + n as u64, "eof": n < size}),
                )
            }
            other => Err(format!("unknown host op: {other}")),
        }
    }

    fn fetch(&self, a: &Value) -> Result<Value, String> {
        let url = a["url"].as_str().unwrap_or_default();
        let domains = self
            .caps
            .net
            .as_ref()
            .ok_or_else(|| denied("net not declared"))?;
        let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
        let host = parsed.host_str().unwrap_or_default();
        if !domains.is_empty()
            && !domains
                .iter()
                .any(|d| host == d || host.ends_with(&format!(".{d}")))
        {
            return Err(denied(format!("net:{host} not in scope")));
        }
        if self.mode == HostMode::Replay {
            return Err(denied("net denied in replay"));
        }
        if self.mode == HostMode::Smoke {
            return Ok(json!({"status": 200, "body": ""}));
        }
        let scoped = !domains.is_empty();
        let client = reqwest::blocking::Client::builder()
            .redirect(if scoped {
                reqwest::redirect::Policy::none()
            } else {
                reqwest::redirect::Policy::limited(5)
            })
            .build()
            .map_err(|e| e.to_string())?;
        let method = reqwest::Method::from_bytes(a["method"].as_str().unwrap_or("GET").as_bytes())
            .unwrap_or(reqwest::Method::GET);
        let mut req = client.request(method, url);
        if let Some(h) = a["headers"].as_object() {
            for (k, v) in h {
                if let Some(v) = v.as_str() {
                    req = req.header(k, v);
                }
            }
        }
        if let Some(b) = a["body"].as_str() {
            req = req.body(b.to_string());
        }
        let resp = req.send().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        Ok(json!({"status": status, "body": body}))
    }

    fn process_run(&self, a: &Value) -> Result<Value, String> {
        let cmd = a["cmd"].as_str().unwrap_or_default();
        let allowed = self
            .caps
            .process
            .as_ref()
            .ok_or_else(|| denied("process not declared"))?;
        let base = Path::new(cmd)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(cmd);
        if !allowed.is_empty() && !allowed.iter().any(|c| c == cmd || c == base) {
            return Err(denied(format!("process:{cmd} not in scope")));
        }
        if self.mode == HostMode::Replay {
            return Err(denied("process denied in replay"));
        }
        if self.mode == HostMode::Smoke {
            return Ok(json!({"code": 0, "stdout": "", "stderr": ""}));
        }
        let args: Vec<String> = a["args"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let out = std::process::Command::new(cmd)
            .args(&args)
            .output()
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "code": out.status.code().unwrap_or(-1),
            "stdout": String::from_utf8_lossy(&out.stdout),
            "stderr": String::from_utf8_lossy(&out.stderr),
        }))
    }

    /// Canonical containment check (SPEC §14): target must be inside a
    /// granted root, symlinks resolved.
    fn resolve(&self, raw: &str, roots: &[String], write: bool) -> Result<PathBuf, String> {
        if roots.is_empty() {
            return Err(denied(format!(
                "fs:{} not declared",
                if write { "write" } else { "read" }
            )));
        }
        let target = {
            let p = PathBuf::from(raw);
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().map_err(|e| e.to_string())?.join(p)
            }
        };
        // Canonical target if it exists; else canonical parent + file name.
        let canonical = if target.exists() {
            target.canonicalize().map_err(|e| e.to_string())?
        } else {
            let parent = target.parent().ok_or("path has no parent")?;
            let name = target.file_name().ok_or("path has no file name")?;
            let cp = parent
                .canonicalize()
                .map_err(|_| denied(format!("path parent does not exist: {}", parent.display())))?;
            cp.join(name)
        };
        for r in roots {
            let rp = PathBuf::from(r);
            let root_c = rp.canonicalize().unwrap_or(rp);
            if canonical == root_c || canonical.starts_with(&root_c) {
                return Ok(canonical);
            }
        }
        Err(denied(format!("path outside granted roots: {raw}")))
    }
}

fn classify(msg: &str, cancelled: bool) -> ExecError {
    if cancelled {
        return ExecError {
            kind: ExecErrorKind::Cancelled,
            message: "interrupted".into(),
        };
    }
    let low = msg.to_lowercase();
    let kind = if msg.starts_with("denied:") {
        ExecErrorKind::Denied
    } else if low.contains("out of memory")
        || low.contains("allocation")
        || low.contains("memory") && low.contains("limit")
    {
        ExecErrorKind::Resource
    } else {
        ExecErrorKind::Js
    };
    ExecError {
        kind,
        message: msg.to_string(),
    }
}

/// Syntax + `run` presence check, without executing `run` (SPEC §7.1).
pub fn check_syntax(code: &str) -> Result<(), String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    let ctx = Context::full(&runtime).map_err(|e| e.to_string())?;
    ctx.with(|ctx| {
        let host = Function::new(ctx.clone(), |_op: String, _a: String| {
            r#"{"ok":true,"value":null}"#.to_string()
        })
        .map_err(|e: rquickjs::Error| e.to_string())?;
        ctx.globals()
            .set("__host", host)
            .map_err(|e: rquickjs::Error| e.to_string())?;
        ctx.eval::<(), _>(PRELUDE).map_err(|e| e.to_string())?;
        ctx.eval::<(), _>(code).map_err(|e| e.to_string())?;
        let has: bool = ctx
            .eval("typeof run === 'function'")
            .map_err(|e| e.to_string())?;
        if !has {
            return Err("tool code must define a global function run(input)".to_string());
        }
        Ok(())
    })
}

/// Execute one Tool Invocation. Blocking; run on a dedicated thread.
pub fn execute(req: &ExecRequest, ops: Arc<dyn HostOps>) -> Result<ExecOutcome, ExecError> {
    let state = Arc::new(State {
        caps: req.caps.clone(),
        mode: req.mode,
        ops,
        logs: Mutex::new(Vec::new()),
        smoke_fs: req.overlay.clone().unwrap_or_else(new_overlay),
    });
    let runtime = Runtime::new().map_err(|e| classify(&e.to_string(), false))?;
    runtime.set_memory_limit((req.memory_mb as usize).saturating_mul(1024 * 1024));
    runtime.set_max_stack_size(1024 * 1024);
    let cancel = req.cancel.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || cancel.load(Ordering::SeqCst))));
    let ctx = Context::full(&runtime)
        .map_err(|e| classify(&e.to_string(), req.cancel.load(Ordering::SeqCst)))?;
    let cancelled = req.cancel.clone();
    let result_json: String = ctx.with(|ctx| -> Result<String, ExecError> {
        let st = state.clone();
        let activity = req.activity.clone();
        let host = Function::new(ctx.clone(), move |op: String, args_json: String| -> String {
            if let Some(a) = &activity {
                let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
                a.store(ms, Ordering::SeqCst);
            }
            let args: Value = serde_json::from_str(&args_json).unwrap_or(Value::Null);
            match st.dispatch(&op, &args) {
                Ok(v) => json!({"ok": true, "value": v}).to_string(),
                Err(e) => json!({"ok": false, "error": e}).to_string(),
            }
        })
        .map_err(|e| classify(&e.to_string(), cancelled.load(Ordering::SeqCst)))?;
        ctx.globals().set("__host", host).map_err(|e| classify(&e.to_string(), cancelled.load(Ordering::SeqCst)))?;
        ctx.globals()
            .set("__input_json", req.input.to_string())
            .map_err(|e| classify(&e.to_string(), cancelled.load(Ordering::SeqCst)))?;
        ctx.eval::<(), _>(PRELUDE).map_err(|e| classify(&e.to_string(), cancelled.load(Ordering::SeqCst)))?;
        ctx.eval::<(), _>(req.code.as_str()).map_err(|e| classify(&e.to_string(), cancelled.load(Ordering::SeqCst)))?;
        let runner = r#"
          globalThis.__result_json = (function () {
            try {
              const input = JSON.parse(globalThis.__input_json);
              const out = run(input);
              return JSON.stringify({ ok: true, value: out === undefined ? null : out });
            } catch (e) {
              return JSON.stringify({ ok: false, error: String((e && e.message) || e) + "\n" + String((e && e.stack) || "") });
            }
          })();
        "#;
        ctx.eval::<(), _>(runner).map_err(|e| classify(&e.to_string(), cancelled.load(Ordering::SeqCst)))?;
        ctx.globals()
            .get::<_, String>("__result_json")
            .map_err(|e| classify(&e.to_string(), cancelled.load(Ordering::SeqCst)))
    })?;
    let parsed: Value = serde_json::from_str(&result_json)
        .map_err(|e| classify(&e.to_string(), req.cancel.load(Ordering::SeqCst)))?;
    let logs = state.logs.lock().map(|g| g.clone()).unwrap_or_default();
    if parsed["ok"].as_bool().unwrap_or(false) {
        Ok(ExecOutcome {
            output: parsed["value"].clone(),
            logs,
        })
    } else {
        Err(classify(
            parsed["error"].as_str().unwrap_or("tool error"),
            req.cancel.load(Ordering::SeqCst),
        ))
    }
}

/// Convenience for callers that only need a Result.
pub fn execute_anyhow(req: &ExecRequest, ops: Arc<dyn HostOps>) -> Result<ExecOutcome> {
    execute(req, ops).map_err(|e| anyhow!("{}", e.message))
}
