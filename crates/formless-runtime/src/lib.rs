//! QuickJS Tool runtime (SPEC §7.5): single-file plain-JS Tools with zero
//! ambient authority. Every privileged effect goes through one native
//! `__host(op, argsJson)` bridge, checked against declared Capabilities.

pub mod caps;
pub use caps::Capabilities;

use anyhow::{anyhow, Result};
use base64::Engine as _;
use rquickjs::{Context, Function, Runtime};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Copy-on-write file overlay for Smoke/Replay: `Some(bytes)` is a
/// discarded write, `None` a discarded removal (§8.4).
pub type Overlay = Arc<Mutex<HashMap<String, Option<Vec<u8>>>>>;

pub fn new_overlay() -> Overlay {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Operations only the daemon can provide (nested Tools, artifact emit).
pub trait HostOps: Send + Sync {
    fn call_tool(&self, _name: &str, _input: Value) -> Result<Value, String> {
        Err("tools.call is not available in this context".into())
    }
    fn emit(&self, _chunk: &str) -> Result<(), String> {
        Ok(())
    }
    /// Run a sub-agent (its own model+tools loop) and return its final text.
    fn run_subagent(
        &self,
        _task: &str,
        _system: Option<&str>,
        _model: Option<&str>,
    ) -> Result<String, String> {
        Err("agents.run is not available in this context".into())
    }
    /// One non-streaming LLM completion via the daemon's provider.
    fn llm_complete(
        &self,
        _prompt: &str,
        _system: Option<&str>,
        _model: Option<&str>,
    ) -> Result<String, String> {
        Err("llm.complete is not available in this context".into())
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
    pub activity: Option<Arc<AtomicU64>>,
    /// Shared JS modules available via `requireModule(name)` (v2).
    pub modules: Vec<(String, String)>,
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
    /// The Tool had already been handed stubbed data (or a stub denial) by
    /// a Smoke-mode host call, so the failure may be the stub's, not the
    /// Tool's: the smoke test is inconclusive rather than failed (§7.1).
    pub after_stub: bool,
}
impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ExecError {}

/// An open streamed HTTP body (`fetchChunks`).
struct Stream {
    resp: reqwest::blocking::Response,
    enc: Enc,
    /// Bytes read but not yet handed out (an incomplete UTF-8 sequence).
    carry: Vec<u8>,
}

struct State {
    caps: Capabilities,
    mode: HostMode,
    ops: Arc<dyn HostOps>,
    logs: Mutex<Vec<String>>,
    /// Smoke/Replay write overlay: discarded writes stay readable within
    /// the same run so write-then-read tools work (§8.4).
    overlay: Overlay,
    modules: Vec<(String, String)>,
    streams: Mutex<HashMap<u64, Stream>>,
    next_stream: AtomicU64,
    stubbed: AtomicBool,
}

const PRELUDE: &str = r#"
function __call(op, args) {
  const r = JSON.parse(__host(op, JSON.stringify(args || {})));
  if (!r.ok) throw new Error(r.error);
  return r.value;
}
function __fmt(a) { return a.map(x => typeof x === "string" ? x : JSON.stringify(x)).join(" "); }
globalThis.console = {
  log: (...a) => __call("log", { msg: __fmt(a) }),
  info: (...a) => __call("log", { msg: __fmt(a) }),
  warn: (...a) => __call("log", { msg: __fmt(a) }),
  error: (...a) => __call("log", { msg: __fmt(a) }),
};
function __req(url, o) {
  o = o || {};
  return { url: String(url), method: o.method || "GET", headers: o.headers || {},
           body: o.body == null ? null : String(o.body), body_encoding: o.bodyEncoding || null,
           encoding: o.encoding || null };
}
function __res(r) {
  return { status: r.status, ok: r.status >= 200 && r.status < 300, url: r.url, headers: r.headers,
           body: r.body, text: () => r.body, json: () => JSON.parse(r.body) };
}
globalThis.fetch = (url, opts) => __res(__call("fetch", __req(url, opts)));
globalThis.fetchChunks = (url, opts, size, cb) => {
  const h = __call("fetch_open", __req(url, opts));
  try {
    while (true) {
      const c = __call("stream_read", { id: h.id, size: size || 65536 });
      if (c.data) cb(c.data);
      if (c.eof) break;
    }
  } finally { __call("stream_close", { id: h.id }); }
  return h.status;
};
globalThis.fs = {
  read: (p, o) => __call("fs_read", { path: String(p), encoding: (o && o.encoding) || null }),
  write: (p, c, o) => __call("fs_write", { path: String(p), content: String(c), encoding: (o && o.encoding) || null }),
  append: (p, c, o) => __call("fs_write", { path: String(p), content: String(c), encoding: (o && o.encoding) || null, append: true }),
  exists: (p) => __call("fs_exists", { path: String(p) }),
  stat: (p) => __call("fs_stat", { path: String(p) }),
  list: (p, o) => __call("fs_list", { path: String(p), details: !!(o && o.details) }),
  mkdir: (p) => __call("fs_mkdir", { path: String(p) }),
  remove: (p, o) => __call("fs_remove", { path: String(p), recursive: !!(o && o.recursive) }),
  rename: (from, to) => __call("fs_rename", { from: String(from), to: String(to) }),
  readChunks: (p, size, cb, o) => {
    let off = 0;
    while (true) {
      const r = __call("fs_chunk", { path: String(p), offset: off, size: size || 65536, encoding: (o && o.encoding) || null });
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
globalThis.process = {
  run: (cmd, args, o) => {
    o = o || {};
    return __call("process_run", { cmd: String(cmd), args: (args || []).map(String), cwd: o.cwd || null,
      env: o.env || null, stdin: o.stdin == null ? null : String(o.stdin),
      stdin_encoding: o.stdinEncoding || null, encoding: o.encoding || null });
  },
};
globalThis.tools = { call: (name, input) => __call("tools_call", { name: String(name), input: input || {} }) };
globalThis.clipboard = {
  read: () => __call("clipboard_read", {}),
  write: (t) => __call("clipboard_write", { text: String(t) }),
};
globalThis.notify = (title, body) => __call("notify", { title: String(title || ""), body: String(body || "") });
globalThis.notifications = { send: globalThis.notify };
globalThis.llm = {
  complete: (prompt, opts) => __call("llm_complete", { prompt: String(prompt), system: (opts && opts.system) || null, model: (opts && opts.model) || null }),
};
globalThis.agents = {
  run: (task, opts) => __call("agent_run", { task: String(task), system: (opts && opts.system) || null, model: (opts && opts.model) || null }),
};
globalThis.__moduleCache = {};
globalThis.requireModule = (name) => {
  const key = String(name);
  if (Object.prototype.hasOwnProperty.call(globalThis.__moduleCache, key)) return globalThis.__moduleCache[key];
  const code = __call("module_load", { name: key });
  const module = { exports: {} };
  globalThis.__moduleCache[key] = module.exports;
  (new Function("module", "exports", "requireModule", code))(module, module.exports, globalThis.requireModule);
  globalThis.__moduleCache[key] = module.exports;
  return module.exports;
};
globalThis.modules = { load: globalThis.requireModule, list: () => __call("module_list", {}) };
globalThis.emit = (chunk) => __call("emit", { chunk: String(chunk) });
globalThis.sleep = (ms) => __call("sleep", { ms: ms | 0 });
globalThis.setTimeout = (fn, ms) => { sleep(ms || 0); if (fn) fn(); return 0; };
globalThis.cryptoRandom = () => __call("crypto_random", {});
globalThis.randomBytes = (n) => __call("random_bytes", { n: n | 0 });
"#;

fn denied(msg: impl Into<String>) -> String {
    format!("denied: {}", msg.into())
}

const INPUT_PREFIX: &str = "$input.";

/// Data encoding at the JS boundary: UTF-8 text (lossy) or base64 bytes,
/// so Tools can move any binary data through strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enc {
    Text,
    Base64,
}

fn enc_of(v: &Value) -> Result<Enc, String> {
    match v.as_str() {
        None | Some("utf8") | Some("utf-8") | Some("text") => Ok(Enc::Text),
        Some("base64") => Ok(Enc::Base64),
        Some(other) => Err(format!("unknown encoding '{other}' (use utf8 or base64)")),
    }
}

fn encode(bytes: &[u8], enc: Enc) -> Value {
    match enc {
        Enc::Text => json!(String::from_utf8_lossy(bytes)),
        Enc::Base64 => json!(base64::engine::general_purpose::STANDARD.encode(bytes)),
    }
}

fn decode(s: &str, enc: Enc) -> Result<Vec<u8>, String> {
    match enc {
        Enc::Text => Ok(s.as_bytes().to_vec()),
        Enc::Base64 => base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .map_err(|e| format!("invalid base64: {e}")),
    }
}

/// Length of the longest prefix of `b` that does not end inside a UTF-8
/// sequence, so text chunks never split a character.
fn utf8_cut(b: &[u8]) -> usize {
    let n = b.len();
    for back in 1..=n.min(4) {
        let c = b[n - back];
        if c & 0xC0 == 0x80 {
            continue;
        }
        let need = match c {
            0xF0.. => 4,
            0xE0.. => 3,
            0xC0.. => 2,
            _ => 1,
        };
        return if need > back { n - back } else { n };
    }
    n
}

fn host_in_scope(domains: &[String], host: &str) -> bool {
    domains
        .iter()
        .any(|d| host == d || host.ends_with(&format!(".{d}")))
}

/// Is `cmd` covered by a `process:<name>` grant? A bare grant matches the
/// bare command (resolved through PATH by the OS) or a path that is that
/// same PATH binary; a path grant matches only that file. A look-alike
/// binary elsewhere (`/tmp/x/ls` under `process:ls`) never matches.
/// Variables a `process.run` child inherits without an `env:` grant: what
/// ordinary programs need to locate binaries, temp dirs and the locale.
const PROCESS_BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    // Windows needs these for most programs to start at all.
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
];

fn process_allowed(allowed: &[String], cmd: &str) -> bool {
    let is_path = |s: &str| s.contains('/') || s.contains('\\');
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    };
    allowed.iter().any(|a| {
        if a == cmd {
            return true;
        }
        match (is_path(a), is_path(cmd)) {
            (false, true) => which(a).is_some_and(|p| same(&p, Path::new(cmd))),
            (true, false) => which(cmd).is_some_and(|p| same(&p, Path::new(a))),
            _ => false,
        }
    })
}

/// The binary a bare command name resolves to through PATH.
fn which(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(str::to_lowercase)
            .chain([String::new()])
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        exts.iter()
            .map(|e| dir.join(format!("{name}{e}")))
            .find(|p| p.is_file())
    })
}

fn file_type(m: &std::fs::Metadata) -> &'static str {
    if m.file_type().is_symlink() {
        "symlink"
    } else if m.is_dir() {
        "dir"
    } else if m.is_file() {
        "file"
    } else {
        "other"
    }
}

fn modified_ms(m: &std::fs::Metadata) -> Option<u64> {
    m.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

impl State {
    /// Smoke-mode stub: remember that the Tool has seen fake data.
    fn stub<T>(&self, v: T) -> T {
        self.stubbed.store(true, Ordering::SeqCst);
        v
    }

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
            "random_bytes" => {
                let n = a["n"].as_u64().unwrap_or(0).min(1 << 20) as usize;
                let mut out = Vec::with_capacity(n + 16);
                while out.len() < n {
                    out.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
                }
                out.truncate(n);
                Ok(encode(&out, Enc::Base64))
            }
            "emit" => {
                self.ops.emit(a["chunk"].as_str().unwrap_or_default())?;
                Ok(Value::Null)
            }
            "tools_call" => {
                if self.mode == HostMode::Smoke {
                    return Err(self.stub(denied("tools.call in smoke test")));
                }
                self.ops
                    .call_tool(a["name"].as_str().unwrap_or_default(), a["input"].clone())
            }
            "clipboard_read" => {
                if !self.caps.clipboard_read {
                    return Err(denied("clipboard:read not declared"));
                }
                match self.mode {
                    HostMode::Smoke => Ok(self.stub(json!(""))),
                    HostMode::Replay => Err(denied("clipboard denied in replay")),
                    HostMode::Live => Ok(json!(clipboard_read()?)),
                }
            }
            "clipboard_write" => {
                if !self.caps.clipboard_write {
                    return Err(denied("clipboard:write not declared"));
                }
                match self.mode {
                    HostMode::Smoke => Ok(Value::Null),
                    HostMode::Replay => Err(denied("clipboard denied in replay")),
                    HostMode::Live => {
                        clipboard_write(a["text"].as_str().unwrap_or_default())?;
                        Ok(Value::Null)
                    }
                }
            }
            "notify" => {
                if !self.caps.notify {
                    return Err(denied("notify not declared"));
                }
                match self.mode {
                    HostMode::Smoke => Ok(Value::Null),
                    HostMode::Replay => Err(denied("notify denied in replay")),
                    HostMode::Live => {
                        notify(
                            a["title"].as_str().unwrap_or_default(),
                            a["body"].as_str().unwrap_or_default(),
                        )?;
                        Ok(Value::Null)
                    }
                }
            }
            "llm_complete" => {
                let model = scoped_model(&self.caps.llm, a["model"].as_str(), "llm")?;
                match self.mode {
                    HostMode::Smoke => Ok(self.stub(json!(""))),
                    HostMode::Replay => Err(denied("llm denied in replay")),
                    HostMode::Live => Ok(json!(self.ops.llm_complete(
                        a["prompt"].as_str().unwrap_or_default(),
                        a["system"].as_str(),
                        model,
                    )?)),
                }
            }
            "agent_run" => {
                let model = scoped_model(&self.caps.agent, a["model"].as_str(), "agent")?;
                match self.mode {
                    HostMode::Smoke => Ok(self.stub(json!(""))),
                    HostMode::Replay => Err(denied("agent denied in replay")),
                    HostMode::Live => Ok(json!(self.ops.run_subagent(
                        a["task"].as_str().unwrap_or_default(),
                        a["system"].as_str(),
                        model,
                    )?)),
                }
            }
            "module_load" => {
                let name = a["name"].as_str().unwrap_or_default();
                self.modules
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(name))
                    .map(|(_, code)| json!(code))
                    .ok_or_else(|| {
                        format!("unknown module '{name}' (create it with create_module)")
                    })
            }
            "module_list" => Ok(json!(self
                .modules
                .iter()
                .map(|(n, _)| n.clone())
                .collect::<Vec<_>>())),
            "env_get" => {
                let key = a["key"].as_str().unwrap_or_default();
                if !self.caps.env.iter().any(|k| k == key) {
                    return Err(denied(format!("env:{key} not declared")));
                }
                match self.mode {
                    HostMode::Smoke => Ok(self.stub(json!(""))),
                    HostMode::Replay => Ok(json!("")),
                    HostMode::Live => Ok(json!(std::env::var(key).unwrap_or_default())),
                }
            }
            "fetch" => self.fetch(a),
            "fetch_open" => self.fetch_open(a),
            "stream_read" => self.stream_read(a),
            "stream_close" => {
                if let Some(id) = a["id"].as_u64() {
                    self.streams.lock().unwrap().remove(&id);
                }
                Ok(Value::Null)
            }
            "process_run" => self.process_run(a),
            "fs_read" => {
                let enc = enc_of(&a["encoding"])?;
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), false)?;
                match self.overlay_get(&p) {
                    Some(Some(bytes)) => return Ok(encode(&bytes, enc)),
                    Some(None) => return Err(format!("not found: {}", p.display())),
                    None => {}
                }
                std::fs::read(&p)
                    .map(|b| encode(&b, enc))
                    .map_err(|e| format!("{}: {e}", p.display()))
            }
            "fs_write" => {
                let enc = enc_of(&a["encoding"])?;
                let data = decode(a["content"].as_str().unwrap_or_default(), enc)?;
                let append = a["append"].as_bool().unwrap_or(false);
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), true)?;
                if self.mode.is_overlay() {
                    // Capability must still be declared; the write lands in
                    // the overlay (readable this run, never on disk) (§8.4).
                    let mut full = if append {
                        match self.overlay_get(&p) {
                            Some(prev) => prev.unwrap_or_default(),
                            None => std::fs::read(&p).unwrap_or_default(),
                        }
                    } else {
                        Vec::new()
                    };
                    full.extend_from_slice(&data);
                    self.overlay_put(&p, Some(full));
                    return Ok(json!(data.len()));
                }
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                if append {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&p)
                        .and_then(|mut f| f.write_all(&data))
                } else {
                    std::fs::write(&p, &data)
                }
                .map_err(|e| format!("{}: {e}", p.display()))?;
                Ok(json!(data.len()))
            }
            "fs_exists" => {
                let Ok(p) = self.resolve(a["path"].as_str().unwrap_or_default(), false) else {
                    return Ok(json!(false));
                };
                Ok(json!(match self.overlay_get(&p) {
                    Some(v) => v.is_some(),
                    None => p.exists(),
                }))
            }
            "fs_stat" => {
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), false)?;
                match self.overlay_get(&p) {
                    Some(Some(b)) => {
                        return Ok(json!({"type": "file", "size": b.len(), "modified_ms": null}))
                    }
                    Some(None) => return Ok(Value::Null),
                    None => {}
                }
                match std::fs::symlink_metadata(&p) {
                    Ok(m) => Ok(json!({
                        "type": file_type(&m),
                        "size": m.len(),
                        "modified_ms": modified_ms(&m),
                        "readonly": m.permissions().readonly(),
                    })),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
                    Err(e) => Err(e.to_string()),
                }
            }
            "fs_list" => self.fs_list(a),
            "fs_mkdir" => {
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), true)?;
                if !self.mode.is_overlay() {
                    std::fs::create_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                }
                Ok(Value::Null)
            }
            "fs_remove" => {
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), true)?;
                if self.mode.is_overlay() {
                    self.overlay_put(&p, None);
                    return Ok(Value::Null);
                }
                let m =
                    std::fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                if m.is_dir() {
                    if a["recursive"].as_bool().unwrap_or(false) {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_dir(&p)
                    }
                } else {
                    std::fs::remove_file(&p)
                }
                .map_err(|e| format!("{}: {e}", p.display()))?;
                Ok(Value::Null)
            }
            "fs_rename" => {
                let from = self.resolve(a["from"].as_str().unwrap_or_default(), true)?;
                let to = self.resolve(a["to"].as_str().unwrap_or_default(), true)?;
                if self.mode.is_overlay() {
                    let bytes = match self.overlay_get(&from) {
                        Some(Some(b)) => b,
                        Some(None) => return Err(format!("not found: {}", from.display())),
                        None if from.is_dir() => {
                            return Err("renaming a directory is not supported in replay".into())
                        }
                        None => std::fs::read(&from).map_err(|e| e.to_string())?,
                    };
                    self.overlay_put(&to, Some(bytes));
                    self.overlay_put(&from, None);
                    return Ok(Value::Null);
                }
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            "fs_chunk" => {
                let enc = enc_of(&a["encoding"])?;
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), false)?;
                let offset = a["offset"].as_u64().unwrap_or(0);
                let size = a["size"].as_u64().unwrap_or(65536).clamp(4, 1 << 20) as usize;
                let (buf, total) = match self.overlay_get(&p) {
                    Some(Some(bytes)) => {
                        let start = (offset as usize).min(bytes.len());
                        let end = (start + size).min(bytes.len());
                        (bytes[start..end].to_vec(), bytes.len() as u64)
                    }
                    Some(None) => return Err(format!("not found: {}", p.display())),
                    None => {
                        let mut f =
                            std::fs::File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                        let total = f.metadata().map_err(|e| e.to_string())?.len();
                        f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
                        let mut buf = Vec::with_capacity(size);
                        f.take(size as u64)
                            .read_to_end(&mut buf)
                            .map_err(|e| e.to_string())?;
                        (buf, total)
                    }
                };
                let at_end = offset + buf.len() as u64 >= total;
                let n = match enc {
                    Enc::Text if !at_end => utf8_cut(&buf).max(1),
                    _ => buf.len(),
                };
                let next = offset + n as u64;
                Ok(
                    json!({"data": encode(&buf[..n], enc), "next_offset": next, "eof": next >= total}),
                )
            }
            other => Err(format!("unknown host op: {other}")),
        }
    }

    /// Overlay entry for `p`: `Some(None)` if it or an ancestor was removed.
    fn overlay_get(&self, p: &Path) -> Option<Option<Vec<u8>>> {
        if !self.mode.is_overlay() {
            return None;
        }
        let g = self.overlay.lock().ok()?;
        if let Some(v) = g.get(&p.to_string_lossy().to_string()) {
            return Some(v.clone());
        }
        p.ancestors()
            .skip(1)
            .any(|a| matches!(g.get(&a.to_string_lossy().to_string()), Some(None)))
            .then_some(None)
    }

    fn overlay_put(&self, p: &Path, v: Option<Vec<u8>>) {
        if let Ok(mut g) = self.overlay.lock() {
            g.insert(p.to_string_lossy().to_string(), v);
        }
    }

    fn fs_list(&self, a: &Value) -> Result<Value, String> {
        let p = self.resolve(a["path"].as_str().unwrap_or_default(), false)?;
        let details = a["details"].as_bool().unwrap_or(false);
        let mut entries: std::collections::BTreeMap<String, Value> = Default::default();
        if self.overlay_get(&p) != Some(None) {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd {
                    let e = e.map_err(|e| e.to_string())?;
                    let name = e.file_name().to_string_lossy().to_string();
                    let info = match e.path().symlink_metadata() {
                        Ok(m) => json!({"name": name, "type": file_type(&m), "size": m.len()}),
                        Err(_) => json!({"name": name, "type": "other", "size": 0}),
                    };
                    entries.insert(name, info);
                }
            } else if !self.mode.is_overlay() {
                std::fs::read_dir(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            }
        }
        if self.mode.is_overlay() {
            let g = self.overlay.lock().map_err(|e| e.to_string())?;
            for (k, v) in g.iter() {
                let kp = Path::new(k);
                if kp.parent() != Some(p.as_path()) {
                    continue;
                }
                let name = kp
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                match v {
                    Some(b) => {
                        entries.insert(
                            name.clone(),
                            json!({"name": name, "type": "file", "size": b.len()}),
                        );
                    }
                    None => {
                        entries.remove(&name);
                    }
                }
            }
        }
        Ok(if details {
            json!(entries.into_values().collect::<Vec<_>>())
        } else {
            json!(entries.into_keys().collect::<Vec<_>>())
        })
    }

    /// Scope-check a URL against the `net` grant. In Smoke mode a host
    /// that only an input-bound template could grant is stubbed, not denied.
    fn check_net(&self, url: &str) -> Result<(reqwest::Url, Vec<String>), String> {
        let domains = self
            .caps
            .net
            .as_ref()
            .ok_or_else(|| denied("net not declared"))?;
        let fixed: Vec<String> = domains
            .iter()
            .filter(|d| !d.starts_with(INPUT_PREFIX))
            .cloned()
            .collect();
        let templated = fixed.len() != domains.len();
        let parsed = match reqwest::Url::parse(url) {
            Ok(u) => u,
            // The schema-minimal smoke input has no real URL to bind.
            Err(_) if templated && self.mode == HostMode::Smoke => {
                return Err(self.stub(denied(
                    "net is input-bound and not granted in the smoke test",
                )))
            }
            Err(e) => return Err(format!("bad URL {url}: {e}")),
        };
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(denied(format!("scheme {} not allowed", parsed.scheme())));
        }
        let host = parsed.host_str().unwrap_or_default().to_lowercase();
        let smoke_stub = templated && self.mode == HostMode::Smoke;
        if !domains.is_empty() && !host_in_scope(&fixed, &host) && !smoke_stub {
            return Err(denied(format!("net:{host} not in scope")));
        }
        if self.mode == HostMode::Replay {
            return Err(denied("net denied in replay"));
        }
        Ok((parsed, fixed))
    }

    fn send(
        &self,
        a: &Value,
        parsed: reqwest::Url,
        scope: Vec<String>,
    ) -> Result<reqwest::blocking::Response, String> {
        // Redirects are followed, but every hop is scope-checked: a
        // scoped Tool can never be bounced to a host it was not granted.
        let unscoped = scope.is_empty();
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            let host = attempt.url().host_str().unwrap_or_default().to_lowercase();
            if attempt.previous().len() >= 10 {
                attempt.error("too many redirects")
            } else if unscoped || host_in_scope(&scope, &host) {
                attempt.follow()
            } else {
                attempt.error(denied(format!("redirect to {host} not in net scope")))
            }
        });
        let client = reqwest::blocking::Client::builder()
            .redirect(policy)
            .timeout(None)
            .build()
            .map_err(|e| e.to_string())?;
        let method = reqwest::Method::from_bytes(a["method"].as_str().unwrap_or("GET").as_bytes())
            .map_err(|e| format!("bad method: {e}"))?;
        let mut req = client.request(method, parsed);
        if let Some(h) = a["headers"].as_object() {
            for (k, v) in h {
                if let Some(v) = v.as_str() {
                    req = req.header(k, v);
                }
            }
        }
        if let Some(b) = a["body"].as_str() {
            req = req.body(decode(b, enc_of(&a["body_encoding"])?)?);
        }
        req.send().map_err(|e| {
            use std::error::Error as _;
            let mut msg = e.to_string();
            let mut src = e.source();
            while let Some(s) = src {
                if s.to_string().starts_with("denied:") {
                    return s.to_string();
                }
                msg = format!("{msg}: {s}");
                src = s.source();
            }
            msg
        })
    }

    fn response_meta(resp: &reqwest::blocking::Response) -> Value {
        let mut headers = serde_json::Map::new();
        for (k, v) in resp.headers() {
            let v = String::from_utf8_lossy(v.as_bytes()).to_string();
            match headers.get_mut(k.as_str()) {
                Some(Value::String(prev)) => *prev = format!("{prev}, {v}"),
                _ => {
                    headers.insert(k.as_str().to_string(), json!(v));
                }
            }
        }
        json!({"status": resp.status().as_u16(), "url": resp.url().as_str(), "headers": headers})
    }

    fn fetch(&self, a: &Value) -> Result<Value, String> {
        let url = a["url"].as_str().unwrap_or_default();
        let (parsed, scope) = self.check_net(url)?;
        let enc = enc_of(&a["encoding"])?;
        if self.mode == HostMode::Smoke {
            return Ok(self.stub(json!({"status": 200, "url": url, "headers": {}, "body": ""})));
        }
        let resp = self.send(a, parsed, scope)?;
        let mut meta = Self::response_meta(&resp);
        let bytes = resp.bytes().map_err(|e| e.to_string())?;
        meta["body"] = encode(&bytes, enc);
        Ok(meta)
    }

    fn fetch_open(&self, a: &Value) -> Result<Value, String> {
        let url = a["url"].as_str().unwrap_or_default();
        let (parsed, scope) = self.check_net(url)?;
        let enc = enc_of(&a["encoding"])?;
        if self.mode == HostMode::Smoke {
            return Ok(self.stub(json!({"id": 0, "status": 200, "url": url, "headers": {}})));
        }
        let resp = self.send(a, parsed, scope)?;
        let mut meta = Self::response_meta(&resp);
        let id = self.next_stream.fetch_add(1, Ordering::SeqCst) + 1;
        self.streams.lock().unwrap().insert(
            id,
            Stream {
                resp,
                enc,
                carry: Vec::new(),
            },
        );
        meta["id"] = json!(id);
        Ok(meta)
    }

    fn stream_read(&self, a: &Value) -> Result<Value, String> {
        let id = a["id"].as_u64().unwrap_or(0);
        let size = a["size"].as_u64().unwrap_or(65536).clamp(4, 1 << 20) as usize;
        let mut streams = self.streams.lock().unwrap();
        let Some(s) = streams.get_mut(&id) else {
            // Smoke stub (id 0) or an already-closed stream: empty body.
            return Ok(json!({"data": "", "eof": true}));
        };
        let mut buf = std::mem::take(&mut s.carry);
        let mut eof = false;
        while buf.len() < size {
            let mut chunk = vec![0u8; size - buf.len()];
            let n = s.resp.read(&mut chunk).map_err(|e| e.to_string())?;
            if n == 0 {
                eof = true;
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        if s.enc == Enc::Text && !eof {
            let cut = utf8_cut(&buf).max(1);
            s.carry = buf.split_off(cut);
        }
        Ok(json!({"data": encode(&buf, s.enc), "eof": eof && s.carry.is_empty()}))
    }

    fn process_run(&self, a: &Value) -> Result<Value, String> {
        let cmd = a["cmd"].as_str().unwrap_or_default();
        let allowed = self
            .caps
            .process
            .as_ref()
            .ok_or_else(|| denied("process not declared"))?;
        if !allowed.is_empty() && !process_allowed(allowed, cmd) {
            return Err(denied(format!("process:{cmd} not in scope")));
        }
        if self.mode == HostMode::Replay {
            return Err(denied("process denied in replay"));
        }
        if self.mode == HostMode::Smoke {
            return Ok(self.stub(json!({"code": 0, "signal": null, "stdout": "", "stderr": ""})));
        }
        let enc = enc_of(&a["encoding"])?;
        let args: Vec<String> = a["args"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let stdin = match a["stdin"].as_str() {
            Some(s) => Some(decode(s, enc_of(&a["stdin_encoding"])?)?),
            None => None,
        };
        let mut command = std::process::Command::new(cmd);
        command
            .args(&args)
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(cwd) = a["cwd"].as_str() {
            command.current_dir(cwd);
        }
        // Children see only a minimal base environment plus the keys the
        // Tool's env Capability grants, never the daemon's secrets.
        command.env_clear();
        let inherited = PROCESS_BASE_ENV
            .iter()
            .copied()
            .chain(self.caps.env.iter().map(String::as_str))
            .filter(|k| !k.to_ascii_uppercase().starts_with("FORMLESS_"));
        for key in inherited {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        if let Some(env) = a["env"].as_object() {
            for (k, v) in env {
                match v {
                    Value::Null => {
                        command.env_remove(k);
                    }
                    Value::String(s) => {
                        command.env(k, s);
                    }
                    other => {
                        command.env(k, other.to_string());
                    }
                }
            }
        }
        let mut child = command.spawn().map_err(|e| format!("{cmd}: {e}"))?;
        // Feed stdin on its own thread so a chatty child cannot deadlock
        // against a full stdout pipe.
        let feeder = match (stdin, child.stdin.take()) {
            (Some(data), Some(mut pipe)) => Some(std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            })),
            _ => None,
        };
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if let Some(f) = feeder {
            let _ = f.join();
        }
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&out.status);
        #[cfg(not(unix))]
        let signal: Option<i32> = None;
        Ok(json!({
            "code": out.status.code().unwrap_or(-1),
            "signal": signal,
            "stdout": encode(&out.stdout, enc),
            "stderr": encode(&out.stderr, Enc::Text),
        }))
    }

    /// Canonical containment check (SPEC §14): target must be inside a
    /// granted root, symlinks resolved. In Smoke mode a path that only an
    /// input-bound template could grant is a stub denial (inconclusive).
    fn resolve(&self, raw: &str, write: bool) -> Result<PathBuf, String> {
        let roots = if write {
            &self.caps.fs_write
        } else {
            &self.caps.fs_read
        };
        let kind = if write { "write" } else { "read" };
        let fixed: Vec<&String> = roots
            .iter()
            .filter(|r| !r.starts_with(INPUT_PREFIX))
            .collect();
        let smoke_stub = fixed.len() != roots.len() && self.mode == HostMode::Smoke;
        let stub_denial = || {
            self.stub(denied(format!(
                "fs:{kind} is input-bound and not granted in the smoke test"
            )))
        };
        if fixed.is_empty() {
            return Err(if smoke_stub {
                stub_denial()
            } else {
                denied(format!("fs:{kind} not declared"))
            });
        }
        let canonical = match caps::canonical_path(raw) {
            Ok(c) => c,
            Err(_) if smoke_stub => return Err(stub_denial()),
            Err(e) => return Err(denied(e)),
        };
        for r in fixed {
            let root = caps::canonical_path(r).unwrap_or_else(|_| PathBuf::from(r));
            if canonical.starts_with(&root) {
                return Ok(canonical);
            }
        }
        Err(if smoke_stub {
            stub_denial()
        } else {
            denied(format!("path outside granted roots: {raw}"))
        })
    }
}

/// Resolve the model for a scoped `llm`/`agent` grant: an explicit model
/// must be in scope; no explicit model means the first granted model.
fn scoped_model<'a>(
    grant: &'a Option<Vec<String>>,
    requested: Option<&'a str>,
    cap: &str,
) -> Result<Option<&'a str>, String> {
    let allowed = grant
        .as_ref()
        .ok_or_else(|| denied(format!("{cap} not declared")))?;
    if allowed.is_empty() {
        return Ok(requested);
    }
    match requested {
        Some(m) if allowed.iter().any(|x| x == m) => Ok(Some(m)),
        Some(m) => Err(denied(format!("{cap}:{m} not in scope"))),
        None => Ok(allowed.first().map(String::as_str)),
    }
}

fn run_capture(cmd: &str, args: &[&str], stdin_text: Option<&str>) -> Result<String, String> {
    use std::io::Write;
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if let Some(text) = stdin_text {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "{cmd} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn clipboard_read() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        run_capture("pbpaste", &[], None)
    }
    #[cfg(target_os = "windows")]
    {
        run_capture(
            "powershell",
            &["-NoProfile", "-Command", "Get-Clipboard"],
            None,
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        run_capture("wl-paste", &[], None)
            .or_else(|_| run_capture("xclip", &["-selection", "clipboard", "-o"], None))
            .or_else(|_| run_capture("xsel", &["--clipboard", "--output"], None))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err("clipboard unsupported on this platform".into())
    }
}

fn clipboard_write(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        run_capture("pbcopy", &[], Some(text)).map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        run_capture(
            "powershell",
            &["-NoProfile", "-Command", "Set-Clipboard -Value $input"],
            Some(text),
        )
        .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        run_capture("wl-copy", &[], Some(text))
            .or_else(|_| run_capture("xclip", &["-selection", "clipboard"], Some(text)))
            .or_else(|_| run_capture("xsel", &["--clipboard", "--input"], Some(text)))
            .map(|_| ())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = text;
        Err("clipboard unsupported on this platform".into())
    }
}

fn notify(title: &str, body: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            esc(body),
            esc(title)
        );
        run_capture("osascript", &["-e", &script], None).map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        run_capture("notify-send", &[title, body], None).map(|_| ())
    }
    #[cfg(not(unix))]
    {
        let _ = (title, body);
        Err("notifications unsupported on this platform".into())
    }
}

fn classify(msg: &str, cancelled: bool) -> ExecError {
    if cancelled {
        return ExecError {
            kind: ExecErrorKind::Cancelled,
            message: "interrupted".into(),
            after_stub: false,
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
        after_stub: false,
    }
}

/// A Tool that returns `{error: <truthy>}` instead of throwing has failed:
/// counting it as a success would hide it from Self-Optimisation (§8.1).
fn reported_error(output: &Value) -> Option<String> {
    let e = output.as_object()?.get("error")?;
    let truthy = match e {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    };
    truthy.then(|| match e {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
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

const RUNNER: &str = r#"
  (function () {
    const done = (v) => { globalThis.__result_json = JSON.stringify({ ok: true, value: v === undefined ? null : v }); };
    const fail = (e) => { globalThis.__result_json = JSON.stringify({ ok: false, error: String((e && e.message) || e) + "\n" + String((e && e.stack) || "") }); };
    try {
      const out = run(JSON.parse(globalThis.__input_json));
      if (out && typeof out.then === "function") out.then(done, fail); else done(out);
    } catch (e) { fail(e); }
  })();
"#;

/// Execute one Tool Invocation. Blocking; run on a dedicated thread.
pub fn execute(req: &ExecRequest, ops: Arc<dyn HostOps>) -> Result<ExecOutcome, ExecError> {
    let state = Arc::new(State {
        caps: req.caps.clone(),
        mode: req.mode,
        ops,
        logs: Mutex::new(Vec::new()),
        overlay: req.overlay.clone().unwrap_or_else(new_overlay),
        modules: req.modules.clone(),
        streams: Mutex::new(HashMap::new()),
        next_stream: AtomicU64::new(0),
        stubbed: AtomicBool::new(false),
    });
    let cancelled = req.cancel.clone();
    let err = |e: &dyn std::fmt::Display| {
        let mut x = classify(&e.to_string(), cancelled.load(Ordering::SeqCst));
        x.after_stub = state.stubbed.load(Ordering::SeqCst);
        x
    };
    let runtime = Runtime::new().map_err(|e| err(&e))?;
    runtime.set_memory_limit((req.memory_mb as usize).saturating_mul(1024 * 1024));
    runtime.set_max_stack_size(1024 * 1024);
    let cancel = req.cancel.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || cancel.load(Ordering::SeqCst))));
    let ctx = Context::full(&runtime).map_err(|e| err(&e))?;
    ctx.with(|ctx| -> Result<(), ExecError> {
        let st = state.clone();
        let activity = req.activity.clone();
        let host = Function::new(
            ctx.clone(),
            move |op: String, args_json: String| -> String {
                if let Some(a) = &activity {
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    a.store(ms, Ordering::SeqCst);
                }
                let args: Value = serde_json::from_str(&args_json).unwrap_or(Value::Null);
                match st.dispatch(&op, &args) {
                    Ok(v) => json!({"ok": true, "value": v}).to_string(),
                    Err(e) => json!({"ok": false, "error": e}).to_string(),
                }
            },
        )
        .map_err(|e| err(&e))?;
        let g = ctx.globals();
        g.set("__host", host).map_err(|e| err(&e))?;
        g.set("__input_json", req.input.to_string())
            .map_err(|e| err(&e))?;
        ctx.eval::<(), _>(PRELUDE).map_err(|e| err(&e))?;
        ctx.eval::<(), _>(req.code.as_str()).map_err(|e| err(&e))?;
        ctx.eval::<(), _>(RUNNER).map_err(|e| err(&e))
    })?;
    // An async run() settles through the job queue; host calls are
    // synchronous, so draining it always makes progress.
    while runtime.is_job_pending() {
        if runtime.execute_pending_job().is_err() {
            break;
        }
    }
    let result_json: Option<String> = ctx.with(|ctx| {
        ctx.globals()
            .get::<_, Option<String>>("__result_json")
            .map_err(|e| err(&e))
    })?;
    let result_json =
        result_json.ok_or_else(|| err(&"run() returned a Promise that never settled"))?;
    let parsed: Value = serde_json::from_str(&result_json).map_err(|e| err(&e))?;
    let logs = state.logs.lock().map(|g| g.clone()).unwrap_or_default();
    if !parsed["ok"].as_bool().unwrap_or(false) {
        return Err(err(&parsed["error"].as_str().unwrap_or("tool error")));
    }
    let output = parsed["value"].clone();
    if let Some(msg) = reported_error(&output) {
        return Err(err(&format!(
            "tool returned an error result (throw instead of returning {{error}}): {msg}"
        )));
    }
    Ok(ExecOutcome { output, logs })
}

/// Convenience for callers that only need a Result.
pub fn execute_anyhow(req: &ExecRequest, ops: Arc<dyn HostOps>) -> Result<ExecOutcome> {
    execute(req, ops).map_err(|e| anyhow!("{}", e.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_cut_never_splits_a_char() {
        let s = "aé€😀".as_bytes();
        for n in 0..=s.len() {
            let cut = utf8_cut(&s[..n]);
            assert!(std::str::from_utf8(&s[..cut]).is_ok(), "n={n}");
            assert!(n - cut < 4);
        }
    }

    #[test]
    fn reported_error_detects_truthy_error_fields() {
        assert!(reported_error(&json!({"error": "boom"})).is_some());
        assert!(reported_error(&json!({"error": {"code": 1}})).is_some());
        assert!(reported_error(&json!({"error": null, "v": 1})).is_none());
        assert!(reported_error(&json!({"error": ""})).is_none());
        assert!(reported_error(&json!({"error": false})).is_none());
        assert!(reported_error(&json!(["error"])).is_none());
    }
}
