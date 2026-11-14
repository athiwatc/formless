//! QuickJS Tool runtime (SPEC §7.5): single-file plain-JS Tools with zero
//! ambient authority. Every privileged effect goes through one native
//! `__host(op, argsJson)` bridge, checked against declared Capabilities.

pub mod caps;
mod clipboard_diagnostics;
mod dns;
mod process_executable;
mod processes;
pub mod sensitive_paths;
pub mod structured_output;
pub mod sync;
pub use caps::Capabilities;
pub use sync::LockExt;

use anyhow::{anyhow, Result};
use base64::Engine as _;
use rquickjs::{context::EvalOptions, Context, Function, Runtime};
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
    /// A user's dry run (SPEC §10.4): reads are real; fs and store writes
    /// go to a throwaway overlay; processes, non-GET requests, clipboard
    /// writes, notifications, harness changes, sub-agents and nested Tool
    /// calls are not made. Each would-be effect is logged as
    /// "[dry run] would …".
    DryRun,
}

impl HostMode {
    pub fn is_overlay(self) -> bool {
        matches!(self, HostMode::Smoke | HostMode::Replay | HostMode::DryRun)
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
    /// Hard boundaries and additive credential rules for this host profile.
    fn path_policy(&self) -> Result<sensitive_paths::PathPolicy, String> {
        Ok(sensitive_paths::PathPolicy::default())
    }
    /// Exact-path credential consent, independent of ordinary fs grants.
    fn approve_sensitive_read(
        &self,
        _path: &Path,
        _operation: &str,
        _reason: &str,
    ) -> Result<(), String> {
        Err("credential read needs an interactive approval host".into())
    }
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
    /// The harness API (SPEC §7.13): `op` such as `read` or `roles.upsert`,
    /// with its JSON `args` and the agent's `reason` (shown to the user).
    fn harness(&self, _op: &str, _args: &Value, _reason: &str) -> Result<Value, String> {
        Err("the harness is not available in this context".into())
    }
    /// Key-value store (SPEC §7.4): `op` is get, set, delete or list.
    /// `namespace` is "" for the Tool's own, else a shared namespace.
    fn kv(
        &self,
        _op: &str,
        _namespace: &str,
        _key: &str,
        _value: Option<&Value>,
    ) -> Result<Value, String> {
        Err("store is not available in this context".into())
    }
    /// A live `fs.write` / `fs.append` / `fs.remove` / `fs.rename` is about
    /// to change `path` (each side of a rename): the daemon snapshots the
    /// file first so the Run's changes can be listed and reverted.
    fn file_changing(&self, _path: &std::path::Path) {}
    /// Requested spelling and resolved target of a tracked live mutation.
    fn file_changing_resolved(&self, _requested: &Path, resolved: &Path, _operation: &str) {
        self.file_changing(resolved);
    }
    /// A tracked mutation succeeded; captures an attributable post-image.
    fn file_changed(&self, _requested: &Path, _resolved: &Path, _operation: &str) {}
    /// create_dir succeeded for this exact new directory (not AlreadyExists).
    fn directory_created(&self, _path: &Path) {}
    /// A live program or other opaque writer may mutate paths outside capture.
    fn untracked_writes_possible(&self) {}

    /// Actual external data source, recorded by the daemon without payloads.
    fn data_observed(&self, _kind: &str, _origin: &str) -> Result<(), String> {
        Ok(())
    }

    /// `progress(fraction, label)` from a running Tool (SPEC §7.5).
    fn progress(&self, _fraction: Option<f64>, _label: &str) {}
    /// One non-streaming LLM completion via the daemon's provider.
    fn llm_complete(
        &self,
        _prompt: &str,
        _system: Option<&str>,
        _model: Option<&str>,
    ) -> Result<String, String> {
        Err("llm.complete is not available in this context".into())
    }
    /// Compatibility default for embedders; daemon hosts override this to send
    /// the schema mode as well. Existing text implementations keep their ABI.
    fn llm_complete_with_options(
        &self,
        prompt: &str,
        system: Option<&str>,
        model: Option<&str>,
        options: &structured_output::CompletionOptions,
    ) -> Result<Value, String> {
        let text = self.llm_complete(prompt, system, model)?;
        match options.output_schema() {
            Some(schema) => schema
                .parse_and_validate(&text)
                .map_err(|error| error.to_string()),
            None => Ok(Value::String(text)),
        }
    }
}

/// Process-wide sandbox limits the daemon sets from its config.
static MAX_SLEEP_MS: AtomicU64 = AtomicU64::new(60_000);
static MAX_RANDOM_BYTES: AtomicU64 = AtomicU64::new(1 << 20);
static MAX_PARALLEL_CALLS: AtomicU64 = AtomicU64::new(8);

/// Set the longest `sleep` and the largest `randomBytes` a Tool may use.
pub fn set_limits(max_sleep_ms: u64, max_random_bytes: u64) {
    MAX_SLEEP_MS.store(max_sleep_ms, Ordering::Relaxed);
    MAX_RANDOM_BYTES.store(max_random_bytes, Ordering::Relaxed);
}

/// Native buffers outside the JS heap (#604): one `fs.read` / `fetch`
/// body, one `process.run`'s stdout and stderr each, all `log` lines.
static MAX_READ_BYTES: AtomicU64 = AtomicU64::new(64 << 20);
static MAX_PROCESS_OUTPUT: AtomicU64 = AtomicU64::new(16 << 20);
static MAX_LOG_BYTES: AtomicU64 = AtomicU64::new(1 << 20);
static FETCH_CONNECT_TIMEOUT_MS: AtomicU64 = AtomicU64::new(10_000);
static FETCH_TIMEOUT_MS: AtomicU64 = AtomicU64::new(60_000);

/// Finite Tool fetch deadlines, shared by literal-IP and DNS-pinned clients.
/// Zero cannot disable them. The upper bound avoids overflowing clock deadlines.
pub fn set_fetch_timeouts(connect_ms: u64, total_ms: u64) {
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    FETCH_CONNECT_TIMEOUT_MS.store(connect_ms.clamp(1, DAY_MS), Ordering::Relaxed);
    FETCH_TIMEOUT_MS.store(total_ms.clamp(1, DAY_MS), Ordering::Relaxed);
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FetchTimeouts {
    connect_ms: u64,
    total_ms: u64,
}
impl FetchTimeouts {
    fn configured() -> Self {
        Self {
            connect_ms: FETCH_CONNECT_TIMEOUT_MS.load(Ordering::Relaxed),
            total_ms: FETCH_TIMEOUT_MS.load(Ordering::Relaxed),
        }
    }
}

/// Set the native caps: `tools.max_read_bytes`,
/// `tools.max_process_output_bytes`, `tools.max_log_bytes`.
pub fn set_io_limits(read: u64, process_output: u64, log: u64) {
    MAX_READ_BYTES.store(read.max(1), Ordering::Relaxed);
    MAX_PROCESS_OUTPUT.store(process_output.max(1), Ordering::Relaxed);
    MAX_LOG_BYTES.store(log.max(1), Ordering::Relaxed);
}

/// "fs.read: 80 MB is over the 64 MB resource limit (tools.max_read_bytes); use fs.readChunks".
fn over_limit(what: &str, size: u64, setting: &str, instead: &str) -> String {
    let mb = |b: u64| format!("{:.1} MB", b as f64 / 1_048_576.0);
    format!(
        "{what}: {} is over the {} resource limit ({setting}); {instead}",
        mb(size),
        mb(MAX_READ_BYTES.load(Ordering::Relaxed))
    )
}

fn check_read_size(len: u64) -> Result<(), String> {
    if len > MAX_READ_BYTES.load(Ordering::Relaxed) {
        return Err(over_limit(
            "fs.read",
            len,
            "tools.max_read_bytes",
            "read it with fs.readChunks or fs.readLines",
        ));
    }
    Ok(())
}

/// A whole file, refused past `tools.max_read_bytes` by construction: at
/// most cap + 1 bytes are read, whatever its size claims or becomes
/// between a stat and the read (#1103).
fn read_capped(p: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let cap = MAX_READ_BYTES.load(Ordering::Relaxed);
    // Fast early error for files that already say they're too big.
    if let Ok(m) = std::fs::metadata(p) {
        check_read_size(m.len())?;
    }
    let f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut out = Vec::new();
    f.take(cap + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("{}: {e}", p.display()))?;
    check_read_size(out.len() as u64)?;
    Ok(out)
}

/// Set how many `tools.call` Invocations one sandbox may have in
/// flight at once; the rest queue (SPEC §7.12, `scripts.max_parallel`).
pub fn set_max_parallel_calls(n: u64) {
    MAX_PARALLEL_CALLS.store(n.max(1), Ordering::Relaxed);
}

pub struct NoHostOps;
impl HostOps for NoHostOps {}

/// Smoke/module hosts know the profile boundary, but cannot ask for consent.
pub struct GuardedNoHostOps(pub sensitive_paths::PathPolicy);
impl HostOps for GuardedNoHostOps {
    fn path_policy(&self) -> Result<sensitive_paths::PathPolicy, String> {
        Ok(self.0.clone())
    }
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub code: String,
    pub input: Value,
    /// Declared Tool output schema; scripts and unschematized calls use None.
    pub output_schema: Option<Value>,
    pub caps: Capabilities,
    pub memory_mb: u32,
    pub mode: HostMode,
    pub cancel: Arc<AtomicBool>,
    /// Shared write overlay for Smoke/Replay (nested tools share it, §8.4).
    pub overlay: Option<Overlay>,
    /// Last-activity heartbeat (epoch millis), updated per host call (§6.1).
    pub activity: Option<Arc<AtomicU64>>,
    /// Shared JS modules available via `requireModule(name)` (v2), as
    /// (name, code). Shared, not copied, between Invocations.
    pub modules: Arc<Vec<(String, String)>>,
}

#[derive(Debug)]
pub struct ExecOutcome {
    pub output: Value,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct NestedToolError {
    pub tool: String,
    pub detail: String,
}

/// The only eval filename whose frames refer directly to ExecRequest.code.
pub const TOOL_SOURCE_NAME: &str = "formless-tool.js";

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
    /// `log` lines (and a dry run's would-be effects) before it failed.
    pub logs: Vec<String>,
    /// A propagated tools.call rejection, separate from this Tool's stack.
    pub nested: Option<NestedToolError>,
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
    read_consents: Mutex<std::collections::HashSet<PathBuf>>,
    caps: Capabilities,
    mode: HostMode,
    ops: Arc<dyn HostOps>,
    logs: Mutex<Vec<String>>,
    /// Smoke/Replay write overlay: discarded writes stay readable within
    /// the same run so write-then-read tools work (§8.4).
    overlay: Overlay,
    modules: Arc<Vec<(String, String)>>,
    streams: Mutex<HashMap<u64, Stream>>,
    next_stream: AtomicU64,
    processes: Mutex<HashMap<u64, Arc<processes::Process>>>,
    next_process: AtomicU64,
    stubbed: AtomicBool,
    /// The Invocation's cancel flag: blocking host calls (process, network)
    /// poll it so Esc stops them, not only the JS interpreter.
    cancel: Arc<AtomicBool>,
    /// Smoke/Replay key-value store: in memory, gone after the run.
    kv_overlay: Mutex<std::collections::BTreeMap<(String, String), Value>>,
    /// `tools.call` bookkeeping: each call runs on its own thread.
    calls: Mutex<AsyncCalls>,
    settled_tx: std::sync::mpsc::Sender<Settled>,
    settled_rx: Mutex<std::sync::mpsc::Receiver<Settled>>,
}

/// A finished `tools.call`: its id and result.
type Settled = (u64, Result<Value, String>);

/// Nested calls: those running, and those waiting for a slot under the
/// parallelism cap. A sandbox stopped with calls still running leaves
/// them to finish on their own; their results are discarded.
#[derive(Default)]
struct AsyncCalls {
    next_id: u64,
    running: usize,
    queued: std::collections::VecDeque<(u64, String, Value)>,
}

const PRELUDE: &str = r#"
function __call(op, args) {
  const r = JSON.parse(__host(op, JSON.stringify(args || {})));
  if (!r.ok) throw new Error(r.error);
  return r.value;
}
function __formatValue(x) {
  try {
    if (typeof x === "string") return x;
    if (typeof x === "number" && !Number.isFinite(x)) return String(x);
    const ancestors = [];
    const text = JSON.stringify(x, function (_key, value) {
      if (typeof value === "number" && !Number.isFinite(value)) return String(value);
      if (typeof value === "bigint") return String(value) + "n";
      if (value === undefined) return "undefined";
      if (typeof value === "function" || typeof value === "symbol") return String(value);
      if (value instanceof Error) return value.name + ": " + value.message;
      if (value && typeof value === "object") {
        while (ancestors.length && ancestors[ancestors.length - 1] !== this) ancestors.pop();
        if (ancestors.includes(value)) return "[Circular]";
        ancestors.push(value);
      }
      return value;
    });
    // Match the ordinary console spelling for scalar special values.
    if (x === undefined || typeof x === "bigint" || typeof x === "function" || typeof x === "symbol") return String(x) + (typeof x === "bigint" ? "n" : "");
    if (x instanceof Error) return x.name + ": " + x.message;
    return text;
  } catch (_) { return "[Unformattable]"; }
}
function __fmt(a) { return a.map(__formatValue).join(" "); }
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
function __processReq(cmd, args, o) {
  o = o || {};
  const timeout = o.timeout_ms ?? o.timeoutMs ?? null;
  // JSON would otherwise turn NaN/Infinity into null and remove the deadline.
  if (timeout !== null && (!Number.isInteger(timeout) || timeout < 0 || timeout > 86400000)) {
    throw new Error("process timeout_ms must be an integer from 0 to 86400000");
  }
  return { cmd: String(cmd), args: (args || []).map(String), cwd: o.cwd || null,
    env: o.env || null, stdin: o.stdin == null ? null : String(o.stdin),
    stdin_encoding: o.stdinEncoding || null, encoding: o.encoding || null, timeout_ms: timeout };
}
globalThis.process = {
  run: (cmd, args, o) => __call("process_run", __processReq(cmd, args, o)),
  spawn: (cmd, args, o) => {
    const h = __call("process_spawn", __processReq(cmd, args, o));
    let finished = null;
    return {
      read: (size) => finished ? {stream: null, data: "", eof: true} : __call("process_read", {id: h.id, size: size || 65536}),
      wait: () => finished || (finished = __call("process_wait", {id: h.id})),
      kill: () => finished || (finished = __call("process_kill", {id: h.id})),
      close: () => { if (!finished) finished = __call("process_kill", {id: h.id}); },
    };
  },
};
// tools.call returns a Promise settled from the event loop, so
// Promise.all runs nested Tools in parallel (SPEC §7.6, §7.12).
const __pendingCalls = new Map();
const __nestedErrors = new WeakMap();
const __nestedError = (e) => e && (typeof e === "object" || typeof e === "function") ? (__nestedErrors.get(e) || null) : null;
globalThis.tools = {
  call: (name, input) => new Promise((resolve, reject) => {
    const callsite = new Error("nested tools.call");
    const target = String(name);
    const id = __call("tools_call", { name: target, input: input || {} });
    __pendingCalls.set(id, { resolve, reject, name: target, stack: callsite.stack });
  }),
};
globalThis.__settleCall = (id, r) => {
  const p = __pendingCalls.get(id);
  if (!p) return;
  __pendingCalls.delete(id);
  if (r.ok) p.resolve(r.value);
  else {
    const error = new Error(r.error);
    error.stack = p.stack;
    __nestedErrors.set(error, { tool: p.name, detail: String(r.error) });
    p.reject(error);
  }
};
globalThis.clipboard = {
  read: () => __call("clipboard_read", {}),
  write: (t) => __call("clipboard_write", { text: String(t) }),
};
globalThis.notify = (title, body) => __call("notify", { title: String(title || ""), body: String(body || "") });
globalThis.notifications = { send: globalThis.notify };
globalThis.harness = {
  read: () => __call("harness", { op: "read", args: {}, reason: "" }),
  call: (op, args, reason) => __call("harness", { op: String(op), args: args || {}, reason: String(reason || "") }),
};
globalThis.store = {
  get: (k, o) => __call("kv", { op: "get", key: String(k), ns: (o && o.ns) || "" }),
  set: (k, v, o) => __call("kv", { op: "set", key: String(k), value: v === undefined ? null : v, ns: (o && o.ns) || "" }),
  delete: (k, o) => __call("kv", { op: "delete", key: String(k), ns: (o && o.ns) || "" }),
  list: (prefix, o) => __call("kv", { op: "list", key: String(prefix || ""), ns: (o && o.ns) || "" }),
};
globalThis.llm = {
  complete: (prompt, opts) => {
    const args = { prompt: String(prompt), system: (opts && opts.system) || null, model: (opts && opts.role) ? "role:" + opts.role : (opts && opts.model) || null };
    if (opts && Object.prototype.hasOwnProperty.call(opts, "schema")) {
      if (typeof opts.schema !== "boolean" && (!opts.schema || typeof opts.schema !== "object" || Array.isArray(opts.schema))) throw new Error("llm.complete schema must be a JSON Schema object or boolean");
      args.schema = opts.schema;
    }
    return __call("llm_complete", args);
  },
};
globalThis.agents = {
  run: (task, opts) => __call("agent_run", { task: String(task), system: (opts && opts.system) || null, model: (opts && opts.role) ? "role:" + opts.role : (opts && opts.model) || null }),
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
// Progress for long work: a fraction 0..1 (or null when unknown) and a
// short label; the daemon decides whether it represents advancing work.
globalThis.progress = (fraction, label) => __call("progress", {
  fraction: typeof fraction === "number" && isFinite(fraction) ? Math.min(1, Math.max(0, fraction)) : null,
  label: label == null ? "" : String(label).slice(0, 120),
});
function __nonnegativeSafeInteger(api, n) {
  if (typeof n !== "number" || !Number.isSafeInteger(n) || n < 0)
    throw new Error(api + " requires a finite non-negative safe-integer number");
  return n;
}
globalThis.sleep = (ms) => __call("sleep", { ms: __nonnegativeSafeInteger("sleep", ms) });
// Real timers: callbacks run from the host's event loop after the current
// job, in due order (ties by creation), never synchronously.
const __timers = new Map();
let __timerSeq = 0;
function __addTimer(fn, ms, args, repeat) {
  const id = ++__timerSeq;
  const delay = Math.max(0, Number(ms) || 0);
  __timers.set(id, { fn, args, at: Date.now() + delay, every: repeat ? Math.max(1, delay) : 0 });
  return id;
}
globalThis.setTimeout = (fn, ms, ...args) => __addTimer(fn, ms, args, false);
globalThis.setInterval = (fn, ms, ...args) => __addTimer(fn, ms, args, true);
globalThis.clearTimeout = globalThis.clearInterval = (id) => { __timers.delete(id); };
// Runs every timer that is due; returns ms until the next one, or -1.
globalThis.__runTimers = () => {
  const due = [...__timers].filter(([, t]) => t.at <= Date.now()).sort((a, b) => a[1].at - b[1].at || a[0] - b[0]);
  for (const [id, t] of due) {
    if (!__timers.has(id)) continue; // cleared by an earlier callback
    if (t.every) t.at = Date.now() + t.every; else __timers.delete(id);
    if (typeof t.fn === "function") {
      try { t.fn(...t.args); } catch (e) { console.error("timer callback threw: " + ((e && e.message) || e)); }
    }
  }
  let next = -1;
  for (const t of __timers.values()) {
    const wait = Math.max(0, t.at - Date.now());
    if (next < 0 || wait < next) next = wait;
  }
  return next;
};
globalThis.cryptoRandom = () => __call("crypto_random", {});
globalThis.randomBytes = (n) => __call("random_bytes", { n: __nonnegativeSafeInteger("randomBytes", n) });
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
/// `n` bytes from the OS CSPRNG (uniform; usable for keys and nonces).
fn random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.try_reserve_exact(n)
        .map_err(|_| "randomBytes allocation failed (tools.max_random_bytes)".to_string())?;
    out.resize(n, 0);
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut out);
    Ok(out)
}

fn random_base64(n: u64) -> Result<Value, String> {
    let n = usize::try_from(n).map_err(|_| {
        "randomBytes exceeds supported allocation bounds (tools.max_random_bytes)".to_string()
    })?;
    let encoded_len = base64::encoded_len(n, true)
        .filter(|&n| n <= isize::MAX as usize)
        .ok_or_else(|| {
            "randomBytes exceeds supported allocation bounds (tools.max_random_bytes)".to_string()
        })?;
    let bytes = random_bytes(n)?;
    let mut text = String::new();
    text.try_reserve_exact(encoded_len).map_err(|_| {
        "randomBytes encoding allocation failed (tools.max_random_bytes)".to_string()
    })?;
    base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut text);
    Ok(Value::String(text))
}

fn bounded_count(
    args: &Value,
    field: &str,
    api: &str,
    cap: u64,
    setting: &str,
) -> Result<u64, String> {
    const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
    let n = args
        .get(field)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= MAX_SAFE && n.fract() == 0.0)
        .ok_or_else(|| format!("{api} requires a finite non-negative safe-integer number"))?
        as u64;
    if n > cap {
        return Err(format!("{api} request {n} exceeds limit {cap} ({setting})"));
    }
    Ok(n)
}

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

/// A `net:` grant as (host, port): `host`, `host:port`, an IPv6 literal
/// (`::1`), or a bracketed one with a port (`[::1]:8080`, the form input
/// binding produces).
fn split_grant(d: &str) -> (&str, Option<u16>) {
    if let Some(rest) = d.strip_prefix('[') {
        if let Some((host, after)) = rest.split_once(']') {
            return (host, after.strip_prefix(':').and_then(|p| p.parse().ok()));
        }
    }
    match d.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => match p.parse() {
            Ok(port) => (h, Some(port)),
            Err(_) => (d, None),
        },
        _ => (d, None),
    }
}

fn host_in_scope(domains: &[String], host: &str, port: Option<u16>) -> bool {
    let host = host.trim_matches(['[', ']']);
    domains.iter().any(|d| {
        // `host:port` grants (input-bound URLs with a port) match that port only.
        let (dh, dp) = split_grant(d);
        formless_protocol::net_scope::host_covers(dh, host) && dp.is_none_or(|p| Some(p) == port)
    })
}

/// A grant names `host` itself (not a parent domain), on this port or on
/// any: only then may it reach a private address (`net:localhost` for a
/// local model).
fn names_host_exactly(domains: &[String], host: &str, port: u16) -> bool {
    let host = host.trim_matches(['[', ']']);
    domains.iter().any(|d| {
        let (dh, dp) = split_grant(d);
        dh == host && dp.is_none_or(|p| p == port)
    })
}

/// Loopback, private, link-local, CGNAT, multicast, unspecified, reserved
/// or otherwise not public (SPEC §7.4). IPv6 forms that carry an IPv4
/// address (mapped, compatible, NAT64, 6to4) are judged by that address.
pub fn non_public(ip: std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr};
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT
                || (o[0] == 198 && (18..20).contains(&o[1])) // benchmarking
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol
                || o[0] >= 240 // reserved
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped and IPv4-compatible (`::a.b.c.d`).
            if let Some(v4) = v6.to_ipv4() {
                return non_public(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            let embedded = |hi: u16, lo: u16| {
                IpAddr::V4(Ipv4Addr::new(
                    (hi >> 8) as u8,
                    hi as u8,
                    (lo >> 8) as u8,
                    lo as u8,
                ))
            };
            // NAT64 well-known prefix 64:ff9b::/96.
            if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return non_public(embedded(seg[6], seg[7]));
            }
            // 6to4 2002::/16.
            if seg[0] == 0x2002 {
                return non_public(embedded(seg[1], seg[2]));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80 // link-local
                || (seg[0] & 0xffc0) == 0xfec0 // deprecated site-local
                || (seg[0] == 0x64 && seg[1] == 0xff9b) // local-use NAT64
                || (seg[0] == 0x2001 && seg[1] == 0) // Teredo
                || (seg[0] == 0x2001 && seg[1] == 0xdb8) // documentation
                || (seg[0] == 0x2001 && seg[1] == 2 && seg[2] == 0) // benchmarking
                || (seg[0] == 0x3fff && (seg[1] & 0xf000) == 0) // documentation
                || (seg[0] == 0x100 && seg[1..4] == [0, 0, 0]) // discard-only
        }
    }
}

type DnsResolver<'a> =
    dyn Fn(&str, u16, std::time::Instant) -> Result<Vec<std::net::SocketAddr>, String> + 'a;

/// Resolve the URL's host once and check every address (SPEC §7.4): a
/// private one is refused unless a grant names this host exactly. The
/// address returned is the one the request is then pinned to, so DNS
/// can't change between the check and the connection.
fn checked_address(
    url: &reqwest::Url,
    scope: &[String],
    deadline: std::time::Instant,
    resolve: &DnsResolver<'_>,
) -> Result<Vec<std::net::SocketAddr>, String> {
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_lowercase();
    let port = url.port_or_known_default().unwrap_or(443);
    let literal = host.parse::<std::net::IpAddr>().ok();
    let addrs: Vec<std::net::SocketAddr> = match literal {
        Some(ip) => vec![std::net::SocketAddr::new(ip, port)],
        None => resolve(&host, port, deadline)?,
    };
    if addrs.is_empty() {
        // Never hand an empty pinned set to reqwest and let it resolve again.
        return Err(format!("cannot resolve {host}: no addresses returned"));
    }
    let exact = names_host_exactly(scope, &host, port);
    if let Some(bad) = addrs.iter().find(|a| non_public(a.ip())) {
        if !exact {
            return Err(denied(format!(
                "net:{host} resolves to {}, a private or local address; grant net:{host} itself to reach it",
                bad.ip()
            )));
        }
    }
    // An IP literal needs no pinning; a name is pinned to what was checked,
    // every address in order.
    Ok(if literal.is_some() { Vec::new() } else { addrs })
}

/// Is `cmd` covered by a `process:<name>` grant? A bare grant matches the
/// bare command (resolved through PATH by the OS) or a path that is that
/// same PATH binary; a path grant matches only that file. A look-alike
/// binary elsewhere (`/tmp/x/ls` under `process:ls`) never matches.
/// Send a built request; a scope-denied redirect surfaces as "denied: …".
/// A fetch kept as data, so each redirect hop can be sent again.
struct FetchRequest {
    method: reqwest::Method,
    url: reqwest::Url,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    /// Granted hosts (empty = unscoped `net`).
    scope: Vec<String>,
    timeouts: FetchTimeouts,
}

/// A client for fetches. Redirects are off: [`send_request`] follows
/// them itself so it can scope-check every hop. Proxies are off too: a
/// proxy would resolve the name itself, so the checked address would not
/// be the one connected to (#1093).
fn client_builder(timeouts: FetchTimeouts) -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_millis(timeouts.connect_ms))
        .timeout(std::time::Duration::from_millis(timeouts.total_ms))
        .no_proxy()
}

/// One blocking client for IP-literal fetches (connection pool and TLS
/// sessions survive between calls).
fn http_client(timeouts: FetchTimeouts) -> Result<reqwest::blocking::Client, String> {
    static CLIENT: Mutex<Option<(FetchTimeouts, reqwest::blocking::Client)>> = Mutex::new(None);
    let mut slot = CLIENT.locked();
    if let Some((_, client)) = slot.as_ref().filter(|(limits, _)| *limits == timeouts) {
        return Ok(client.clone());
    }
    let client = client_builder(timeouts)
        .build()
        .map_err(|e| fetch_error(&e))?;
    *slot = Some((timeouts, client.clone()));
    Ok(client)
}

/// A client that connects only to `addrs` for `host` (tried in order; TLS
/// still checks the name), kept per (host, addresses) so repeated fetches
/// reuse its pool instead of a new handshake per call and per hop.
fn pinned_client(
    host: &str,
    addrs: &[std::net::SocketAddr],
    timeouts: FetchTimeouts,
) -> Result<reqwest::blocking::Client, String> {
    type Key = (String, Vec<std::net::SocketAddr>, FetchTimeouts);
    static PINNED: std::sync::Mutex<Vec<(Key, reqwest::blocking::Client)>> =
        std::sync::Mutex::new(Vec::new());
    const KEEP: usize = 32;
    let key = (host.to_string(), addrs.to_vec(), timeouts);
    let mut pinned = PINNED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = pinned.iter().position(|(k, _)| *k == key) {
        let hit = pinned.remove(i);
        let client = hit.1.clone();
        pinned.push(hit);
        return Ok(client);
    }
    let client = client_builder(timeouts)
        .resolve_to_addrs(host, addrs)
        .build()
        .map_err(|e| e.to_string())?;
    if pinned.len() >= KEEP {
        pinned.remove(0);
    }
    pinned.push((key, client.clone()));
    Ok(client)
}

/// Return before blocking work on cancellation. The work itself must carry
/// finite native I/O deadlines; cancelling cannot forcibly terminate a thread.
fn cancellable_work<T: Send + 'static>(
    cancel: &AtomicBool,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if cancel.load(Ordering::SeqCst) => {
                return Err("interrupted".into())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("host call thread panicked".into())
            }
        }
    }
}

/// Preserve the error chain and distinguish deadline expiry from connection
/// failure, including reqwest timeout errors wrapped in a body's io::Error.
fn fetch_error(error: &(dyn std::error::Error + 'static)) -> String {
    let mut timeout = false;
    let mut connect = false;
    let mut parts = Vec::new();
    let mut next = Some(error);
    while let Some(e) = next {
        if let Some(e) = e.downcast_ref::<reqwest::Error>() {
            timeout |= e.is_timeout();
            connect |= e.is_connect();
        }
        if let Some(e) = e.downcast_ref::<std::io::Error>() {
            timeout |= e.kind() == std::io::ErrorKind::TimedOut;
        }
        parts.push(e.to_string());
        next = e.source();
    }
    let kind = if timeout {
        "fetch timed out"
    } else if connect {
        "fetch connection failed"
    } else {
        "fetch failed"
    };
    format!("{kind}: {}", parts.join(": "))
}

/// Send a fetch, following up to 10 redirects. Every hop is scope-checked,
/// so a scoped Tool can never be bounced to a host it was not granted.
/// Like reqwest's own policy: 303 (and 301/302 after a POST) become a
/// bodiless GET, and credentials are not sent on to another host.
fn send_request(req: FetchRequest) -> Result<reqwest::blocking::Response, String> {
    send_request_with_resolver(req, &dns::resolve)
}

fn send_request_with_resolver(
    req: FetchRequest,
    resolve: &DnsResolver<'_>,
) -> Result<reqwest::blocking::Response, String> {
    send_request_with_dependencies(
        req,
        resolve,
        &|url, pinned, timeouts| match url.host_str() {
            Some(host) if !pinned.is_empty() => pinned_client(host, pinned, timeouts),
            _ => http_client(timeouts),
        },
    )
}

// Keep client initialization injectable for deterministic HTTP deadline tests:
// cold native TLS/runtime initialization must not consume the fixture's entire
// short header/body budget before its server has received any HTTP request.
// Production always supplies the checked, pooled client selector above.
fn send_request_with_dependencies(
    mut req: FetchRequest,
    resolve: &DnsResolver<'_>,
    client_for: &dyn Fn(
        &reqwest::Url,
        &[std::net::SocketAddr],
        FetchTimeouts,
    ) -> Result<reqwest::blocking::Client, String>,
) -> Result<reqwest::blocking::Response, String> {
    use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, LOCATION};
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(req.timeouts.total_ms);
    for _ in 0..=10 {
        let dns_deadline = deadline.min(
            std::time::Instant::now() + std::time::Duration::from_millis(req.timeouts.connect_ms),
        );
        let pinned = checked_address(&req.url, &req.scope, dns_deadline, resolve)?;
        let client = client_for(&req.url, &pinned, req.timeouts)?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("fetch timed out: tools.fetch_timeout_secs total deadline expired".into());
        }
        // Request-level timeout is forwarded to async reqwest and stays active
        // through the whole body, even when a peer trickles bytes forever.
        let mut builder = client
            .request(req.method.clone(), req.url.clone())
            .timeout(remaining);
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = &req.body {
            builder = builder.body(body.clone());
        }
        let resp = builder.send().map_err(|e| fetch_error(&e))?;
        let status = resp.status().as_u16();
        let location = resp.headers().get(LOCATION).and_then(|v| v.to_str().ok());
        let (true, Some(location)) = (matches!(status, 301 | 302 | 303 | 307 | 308), location)
        else {
            return Ok(resp);
        };
        let next = req
            .url
            .join(location)
            .map_err(|e| format!("bad redirect location {location}: {e}"))?;
        if !matches!(next.scheme(), "http" | "https") {
            return Err(denied(format!(
                "redirect to scheme {} not allowed",
                next.scheme()
            )));
        }
        let host = next.host_str().unwrap_or_default().to_lowercase();
        if !req.scope.is_empty() && !host_in_scope(&req.scope, &host, next.port_or_known_default())
        {
            return Err(denied(format!("redirect to {host} not in net scope")));
        }
        if status == 303 || (matches!(status, 301 | 302) && req.method == reqwest::Method::POST) {
            req.method = reqwest::Method::GET;
            req.body = None;
            req.headers.retain(|(k, _)| {
                !k.eq_ignore_ascii_case(CONTENT_TYPE.as_str())
                    && !k.eq_ignore_ascii_case(CONTENT_LENGTH.as_str())
            });
        }
        if next.host_str() != req.url.host_str()
            || next.port_or_known_default() != req.url.port_or_known_default()
        {
            req.headers.retain(|(k, _)| {
                !k.eq_ignore_ascii_case(AUTHORIZATION.as_str())
                    && !k.eq_ignore_ascii_case(COOKIE.as_str())
                    && !k.eq_ignore_ascii_case("proxy-authorization")
            });
        }
        req.url = next;
    }
    Err("too many redirects".into())
}

/// Variables a `process.run` child inherits without an `env:` grant: what
/// ordinary programs need to locate binaries, temp dirs and the locale.
/// An environment variable that changes which program runs or what it
/// loads: a Tool sets one for a child only with that `env:` grant (#1098).
fn steers_execution(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    const EXACT: &[&str] = &[
        "PATH",
        "IFS",
        "ENV",
        "BASH_ENV",
        "SHELLOPTS",
        "BASHOPTS",
        "PS4",
        "PROMPT_COMMAND",
        "ZDOTDIR",
        "PAGER",
        "MANPAGER",
        "EDITOR",
        "VISUAL",
        "LESSOPEN",
        "LESSCLOSE",
        "SSH_ASKPASS",
        "PYTHONPATH",
        "PYTHONHOME",
        "PYTHONSTARTUP",
        "PERL5LIB",
        "PERL5OPT",
        "PERLLIB",
        "RUBYLIB",
        "RUBYOPT",
        "NODE_PATH",
        "TMPDIR",
        "HOME",
    ];
    EXACT.contains(&k.as_str())
        || ["LD_", "DYLD_", "GIT_", "NPM_CONFIG_"]
            .iter()
            .any(|p| k.starts_with(p))
        || k.ends_with("_OPTIONS")
        || k.ends_with("_OPTS")
}

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

/// A `process:` grant's command and, for `process:git:status|log`, the
/// first arguments it allows (None = any).
pub fn process_grant(grant: &str) -> (&str, Option<Vec<&str>>) {
    match grant.rsplit_once(':') {
        // `C:\…` is a Windows path, not a command with sub-commands.
        Some((cmd, subs)) if cmd.len() > 1 && !subs.is_empty() && !subs.contains(['/', '\\']) => {
            (cmd, Some(subs.split('|').collect()))
        }
        _ => (grant, None),
    }
}

fn process_allowed(allowed: &[String], cmd: &str, first_arg: Option<&str>) -> bool {
    let is_path = |s: &str| s.contains('/') || s.contains('\\');
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    };
    allowed.iter().any(|grant| {
        let (a, subs) = process_grant(grant);
        // Argument scoping: the first argument must be one it names.
        if let Some(subs) = subs {
            if !first_arg.is_some_and(|f| subs.contains(&f)) {
                return false;
            }
        }
        let a = &a.to_string();
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
    /// A dry run's would-be effect, kept in the logs (SPEC §10.4).
    fn would(&self, what: String) {
        if let Ok(mut g) = self.logs.lock() {
            g.push(format!("[dry run] would {what}"));
        }
    }

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
                        // Past the cap, lines are dropped, once said (#604).
                        let used: usize = g.iter().map(String::len).sum();
                        let cap = MAX_LOG_BYTES.load(Ordering::Relaxed) as usize;
                        if used + m.len() <= cap {
                            g.push(m.to_string());
                        } else if g.last().is_none_or(|l| !l.starts_with("[log limit")) {
                            g.push(format!("[log limit of {cap} bytes reached (tools.max_log_bytes); later lines dropped]"));
                        }
                    }
                }
                Ok(Value::Null)
            }
            "sleep" => {
                let ms = bounded_count(
                    a,
                    "ms",
                    "sleep",
                    MAX_SLEEP_MS.load(Ordering::Relaxed),
                    "tools.max_sleep_secs",
                )?;
                let duration = std::time::Duration::from_millis(ms);
                if std::time::Instant::now().checked_add(duration).is_none() {
                    return Err(
                        "sleep exceeds supported duration bounds (tools.max_sleep_secs)".into(),
                    );
                }
                if self.sleep_cancellable(duration) {
                    Ok(Value::Null)
                } else {
                    Err("interrupted".into())
                }
            }
            "crypto_random" => Ok(json!(uuid::Uuid::new_v4().to_string())),
            "random_bytes" => {
                let n = bounded_count(
                    a,
                    "n",
                    "randomBytes",
                    MAX_RANDOM_BYTES.load(Ordering::Relaxed),
                    "tools.max_random_bytes",
                )?;
                random_base64(n)
            }
            "emit" => {
                self.ops.emit(a["chunk"].as_str().unwrap_or_default())?;
                Ok(Value::Null)
            }
            "progress" => {
                if self.mode == HostMode::Live {
                    self.ops.progress(
                        a["fraction"].as_f64(),
                        a["label"].as_str().unwrap_or_default(),
                    );
                }
                Ok(Value::Null)
            }
            "tools_call" => {
                if self.mode == HostMode::Smoke {
                    return Err(self.stub(denied("tools.call in smoke test")));
                }
                if self.mode == HostMode::DryRun {
                    let name = a["name"].as_str().unwrap_or_default();
                    self.would(format!("call the Tool {name} with {}", a["input"]));
                    return Err(denied(format!(
                        "dry run: the nested call to {name} was not made"
                    )));
                }
                let mut calls = self.calls.locked();
                calls.next_id += 1;
                let id = calls.next_id;
                let name = a["name"].as_str().unwrap_or_default().to_string();
                calls.queued.push_back((id, name, a["input"].clone()));
                self.start_queued(&mut calls);
                Ok(json!(id))
            }
            "clipboard_read" => {
                if !self.caps.clipboard_read {
                    return Err(denied("clipboard:read not declared"));
                }
                match self.mode {
                    HostMode::Smoke => Ok(self.stub(json!(""))),
                    HostMode::Replay => Err(denied("clipboard denied in replay")),
                    HostMode::Live | HostMode::DryRun => {
                        let content = clipboard_read()?;
                        self.ops.data_observed("clipboard", "clipboard")?;
                        Ok(json!(content))
                    }
                }
            }
            "clipboard_write" => {
                if !self.caps.clipboard_write {
                    return Err(denied("clipboard:write not declared"));
                }
                match self.mode {
                    HostMode::Smoke => Ok(Value::Null),
                    HostMode::Replay => Err(denied("clipboard denied in replay")),
                    HostMode::DryRun => {
                        let n = a["text"].as_str().unwrap_or_default().chars().count();
                        self.would(format!("copy {n} characters to the clipboard"));
                        Ok(Value::Null)
                    }
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
                    HostMode::DryRun => {
                        self.would(format!(
                            "show the notification \"{}\"",
                            a["title"].as_str().unwrap_or_default()
                        ));
                        Ok(Value::Null)
                    }
                    HostMode::Live => {
                        notify(
                            a["title"].as_str().unwrap_or_default(),
                            a["body"].as_str().unwrap_or_default(),
                        )?;
                        Ok(Value::Null)
                    }
                }
            }
            "harness" => {
                let op = a["op"].as_str().unwrap_or_default();
                let scope = op.split('.').next().unwrap_or_default();
                if !self.caps.harness.iter().any(|h| h == scope) {
                    return Err(denied(format!("harness:{scope} not declared")));
                }
                match self.mode {
                    HostMode::Smoke => Ok(self.stub(json!({"stubbed": true}))),
                    HostMode::Replay => Err(denied("harness denied in replay")),
                    // Reading the setup is harmless; changing it is not done.
                    HostMode::DryRun if scope == "read" => {
                        self.ops
                            .harness(op, &a["args"], a["reason"].as_str().unwrap_or_default())
                    }
                    HostMode::DryRun => {
                        self.would(format!("change the harness: {op} {}", a["args"]));
                        Ok(json!({"dry_run": true}))
                    }
                    HostMode::Live => {
                        self.ops
                            .harness(op, &a["args"], a["reason"].as_str().unwrap_or_default())
                    }
                }
            }
            "kv" => {
                let ns = a["ns"].as_str().unwrap_or_default();
                if ns.is_empty() && !self.caps.store {
                    return Err(denied("store not declared"));
                }
                if !ns.is_empty() && !self.caps.store_shared.iter().any(|n| n == ns) {
                    return Err(denied(format!("store:{ns} not declared")));
                }
                let op = a["op"].as_str().unwrap_or_default();
                let key = a["key"].as_str().unwrap_or_default();
                let value = a.get("value").filter(|v| !v.is_null());
                match self.mode {
                    HostMode::Live => self.ops.kv(op, ns, key, value),
                    // Reads see real values (and this run's writes); writes
                    // stay in memory.
                    HostMode::DryRun => {
                        let mut kv = self.kv_overlay.locked();
                        let k = (ns.to_string(), key.to_string());
                        match op {
                            "get" => match kv.get(&k) {
                                Some(v) => Ok(v.clone()),
                                None => self.ops.kv(op, ns, key, None),
                            },
                            "set" => {
                                self.would(format!(
                                    "store {key} = {}",
                                    value.cloned().unwrap_or(Value::Null)
                                ));
                                kv.insert(k, value.cloned().unwrap_or(Value::Null));
                                Ok(Value::Null)
                            }
                            "delete" => {
                                self.would(format!("delete the stored {key}"));
                                kv.insert(k, Value::Null);
                                Ok(json!(true))
                            }
                            _ => self.ops.kv(op, ns, key, value),
                        }
                    }
                    HostMode::Smoke | HostMode::Replay => {
                        let mut kv = self.kv_overlay.locked();
                        let k = (ns.to_string(), key.to_string());
                        Ok(match op {
                            "get" => kv.get(&k).cloned().unwrap_or(Value::Null),
                            "set" => {
                                kv.insert(k, value.cloned().unwrap_or(Value::Null));
                                Value::Null
                            }
                            "delete" => json!(kv.remove(&k).is_some()),
                            "list" => json!(kv
                                .keys()
                                .filter(|(n, key2)| n == ns && key2.starts_with(key))
                                .map(|(_, key2)| key2.clone())
                                .collect::<Vec<_>>()),
                            other => return Err(format!("unknown store op {other}")),
                        })
                    }
                }
            }
            "llm_complete" => {
                let model = scoped_model(&self.caps.llm, a["model"].as_str(), "llm")?;
                let options = match a.get("schema") {
                    Some(schema) => {
                        structured_output::CompletionOptions::json_schema(schema.clone())
                            .map_err(|error| error.to_string())?
                    }
                    None => structured_output::CompletionOptions::default(),
                };
                match self.mode {
                    HostMode::Smoke => Ok(self.stub(if options.output_schema().is_some() {
                        Value::Null
                    } else {
                        json!("")
                    })),
                    HostMode::Replay => Err(denied("llm denied in replay")),
                    // A model call changes nothing: made for real.
                    HostMode::Live | HostMode::DryRun => self.ops.llm_complete_with_options(
                        a["prompt"].as_str().unwrap_or_default(),
                        a["system"].as_str(),
                        model,
                        &options,
                    ),
                }
            }
            "agent_run" => {
                let model = scoped_model(&self.caps.agent, a["model"].as_str(), "agent")?;
                match self.mode {
                    HostMode::Smoke => Ok(self.stub(json!(""))),
                    HostMode::Replay => Err(denied("agent denied in replay")),
                    HostMode::DryRun => {
                        self.would(format!(
                            "start a sub-agent on: {}",
                            a["task"].as_str().unwrap_or_default()
                        ));
                        Ok(json!(""))
                    }
                    HostMode::Live => Ok(json!(self.ops.run_subagent(
                        a["task"].as_str().unwrap_or_default(),
                        a["system"].as_str(),
                        model,
                    )?)),
                }
            }
            "module_load" => {
                let name = a["name"].as_str().unwrap_or_default();
                self.ops.data_observed("module", name)?;
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
                    HostMode::Live | HostMode::DryRun => {
                        Ok(json!(std::env::var(key).unwrap_or_default()))
                    }
                }
            }
            "fetch" => self.fetch(a),
            "fetch_open" => self.fetch_open(a),
            "stream_read" => self.stream_read(a),
            "stream_close" => {
                if let Some(id) = a["id"].as_u64() {
                    self.streams.locked().remove(&id);
                }
                Ok(Value::Null)
            }
            "process_run" => self.process_run(a),
            "process_spawn" => self.process_spawn(a),
            "process_read" => self.process_read(a),
            "process_wait" => self.process_finish(a, false),
            "process_kill" => self.process_finish(a, true),
            "fs_read" => {
                let enc = enc_of(&a["encoding"])?;
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), false)?;
                match self.overlay_get(&p) {
                    Some(Some(bytes)) => {
                        check_read_size(bytes.len() as u64)?;
                        return Ok(encode(&bytes, enc));
                    }
                    Some(None) => return Err(format!("not found: {}", p.display())),
                    None => {}
                }
                self.guard_read(a["path"].as_str().unwrap_or_default(), &p, "file contents")?;
                read_capped(&p).map(|b| encode(&b, enc))
            }
            "fs_write" => {
                let enc = enc_of(&a["encoding"])?;
                let data = decode(a["content"].as_str().unwrap_or_default(), enc)?;
                let append = a["append"].as_bool().unwrap_or(false);
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), true)?;
                if append {
                    self.guard_read(
                        a["path"].as_str().unwrap_or_default(),
                        &p,
                        "append prior contents",
                    )?;
                }
                if self.mode.is_overlay() {
                    // Capability must still be declared; the write lands in
                    // the overlay (readable this run, never on disk) (§8.4).
                    let mut full = if append {
                        match self.overlay_get(&p) {
                            Some(prev) => prev.unwrap_or_default(),
                            None if p.exists() => read_capped(&p)?,
                            None => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    full.extend_from_slice(&data);
                    check_read_size(full.len() as u64)?;
                    if self.mode == HostMode::DryRun {
                        self.would(format!(
                            "{} {} bytes to {}",
                            if append { "append" } else { "write" },
                            data.len(),
                            p.display()
                        ));
                    }
                    self.overlay_put(&p, Some(full));
                    return Ok(json!(data.len()));
                }
                self.changing(a["path"].as_str().unwrap_or_default(), &p, "write");
                if let Some(parent) = p.parent() {
                    self.create_tracked_directories(parent)?;
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
                self.changed(a["path"].as_str().unwrap_or_default(), &p, "write");
                Ok(json!(data.len()))
            }
            "fs_exists" => {
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), false)?;
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
                    self.create_tracked_directories(&p)?;
                } else if self.mode == HostMode::DryRun {
                    self.would(format!("create the folder {}", p.display()));
                }
                Ok(Value::Null)
            }
            "fs_remove" => {
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), true)?;
                if a["recursive"].as_bool().unwrap_or(false) {
                    self.guard_container(&p)?;
                }
                if self.mode.is_overlay() {
                    if self.mode == HostMode::DryRun {
                        self.would(format!("delete {}", p.display()));
                    }
                    self.overlay_put(&p, None);
                    return Ok(Value::Null);
                }
                let m =
                    std::fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                self.changing(a["path"].as_str().unwrap_or_default(), &p, "remove");
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
                self.changed(a["path"].as_str().unwrap_or_default(), &p, "remove");
                Ok(Value::Null)
            }
            "fs_rename" => {
                let from = self.resolve(a["from"].as_str().unwrap_or_default(), true)?;
                let to = self.resolve(a["to"].as_str().unwrap_or_default(), true)?;
                let raw_from = caps::lexical_absolute(
                    &std::env::current_dir().map_err(|e| denied(e.to_string()))?,
                    Path::new(a["from"].as_str().unwrap_or_default()),
                )
                .map_err(denied)?;
                if !self
                    .ops
                    .path_policy()
                    .map_err(denied)?
                    .relocation_allowed(&raw_from, &from, &to)
                    .map_err(denied)?
                {
                    return Err(denied("rename would remove a configured sensitive namespace or credential-directory classification; exact read consent does not authorize this relocation"));
                }
                self.guard_read(
                    a["from"].as_str().unwrap_or_default(),
                    &from,
                    "rename source contents",
                )?;
                if from.is_dir() {
                    self.guard_container(&from)?;
                }
                if to.is_dir() {
                    self.guard_container(&to)?;
                }

                if self.mode.is_overlay() {
                    let bytes = match self.overlay_get(&from) {
                        Some(Some(b)) => b,
                        Some(None) => return Err(format!("not found: {}", from.display())),
                        None if from.is_dir() => {
                            return Err("renaming a directory is not supported in replay".into())
                        }
                        None => read_capped(&from)?,
                    };
                    if self.mode == HostMode::DryRun {
                        self.would(format!("rename {} to {}", from.display(), to.display()));
                    }
                    self.overlay_put(&to, Some(bytes));
                    self.overlay_put(&from, None);
                    return Ok(Value::Null);
                }
                self.changing(a["from"].as_str().unwrap_or_default(), &from, "rename");
                self.changing(a["to"].as_str().unwrap_or_default(), &to, "rename");
                if let Some(parent) = to.parent() {
                    self.create_tracked_directories(parent)?;
                }
                std::fs::rename(&from, &to).map_err(|e| e.to_string())?;
                self.changed(a["from"].as_str().unwrap_or_default(), &from, "rename");
                self.changed(a["to"].as_str().unwrap_or_default(), &to, "rename");
                Ok(Value::Null)
            }
            "fs_chunk" => {
                let enc = enc_of(&a["encoding"])?;
                let p = self.resolve(a["path"].as_str().unwrap_or_default(), false)?;
                let offset = a["offset"].as_u64().unwrap_or(0);
                let size = a["size"].as_u64().unwrap_or(65536).clamp(4, 1 << 20) as usize;
                self.guard_read(a["path"].as_str().unwrap_or_default(), &p, "file chunk")?;
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
            if let Some(rd) =
                listing_directory(&p, self.mode.is_overlay(), |path| std::fs::read_dir(path))?
            {
                for e in rd {
                    let e = e.map_err(|e| e.to_string())?;
                    let name = e.file_name().to_string_lossy().to_string();
                    let entry_path = e.path();
                    let raw_entry = entry_path
                        .to_str()
                        .ok_or_else(|| denied("directory entry path is not UTF-8"))?;
                    let canonical_entry = caps::canonical_path(raw_entry).map_err(denied)?;
                    if self.path_decision(raw_entry, &canonical_entry)?
                        == sensitive_paths::PathDecision::Forbidden
                    {
                        continue;
                    }
                    if !details {
                        entries.insert(name, Value::Null);
                        continue;
                    }
                    self.guard_read(raw_entry, &canonical_entry, "directory entry metadata")?;
                    let info = match e.path().symlink_metadata() {
                        Ok(m) => json!({"name": name, "type": file_type(&m), "size": m.len()}),
                        Err(_) => json!({"name": name, "type": "other", "size": 0}),
                    };
                    entries.insert(name, info);
                }
            }
        }
        if self.mode.is_overlay() {
            // Only metadata is copied under the overlay lock. Consent may wait
            // on another Tool/client, so never hold this shared lock across it.
            let overlay_entries: Vec<_> = self
                .overlay
                .lock()
                .map_err(|e| e.to_string())?
                .iter()
                .filter(|(key, _)| Path::new(key).parent() == Some(p.as_path()))
                .map(|(key, value)| (key.clone(), value.as_ref().map(Vec::len)))
                .collect();
            for (key, bytes) in overlay_entries {
                let kp = Path::new(&key);
                let name = kp
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let canonical = caps::canonical_path(&key).map_err(denied)?;
                if self.path_decision(&key, &canonical)? == sensitive_paths::PathDecision::Forbidden
                {
                    continue;
                }
                if details && bytes.is_some() {
                    self.guard_read(&key, &canonical, "overlay entry metadata")?;
                }
                match bytes {
                    Some(bytes) => {
                        let info = if details {
                            json!({"name":name,"type":"file","size":bytes})
                        } else {
                            Value::Null
                        };
                        entries.insert(name, info);
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
        if !domains.is_empty()
            && !host_in_scope(&fixed, &host, parsed.port_or_known_default())
            && !smoke_stub
        {
            let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
            return Err(denied(format!("net:{host}{port} not in scope")));
        }
        if self.mode == HostMode::Replay {
            return Err(denied("net denied in replay"));
        }
        Ok((parsed, fixed))
    }

    /// Sleep in short slices; false if the Invocation was cancelled.
    fn sleep_cancellable(&self, total: std::time::Duration) -> bool {
        let until = std::time::Instant::now() + total;
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return false;
            }
            let left = until.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return true;
            }
            std::thread::sleep(left.min(std::time::Duration::from_millis(25)));
        }
    }

    /// Start queued nested calls while there are free slots.
    fn start_queued(&self, calls: &mut AsyncCalls) {
        let cap = MAX_PARALLEL_CALLS.load(Ordering::Relaxed) as usize;
        while calls.running < cap {
            let Some((id, name, input)) = calls.queued.pop_front() else {
                break;
            };
            calls.running += 1;
            let (ops, tx) = (self.ops.clone(), self.settled_tx.clone());
            std::thread::spawn(move || {
                let _ = tx.send((id, ops.call_tool(&name, input)));
            });
        }
    }

    /// Nested calls not yet settled (running or queued).
    fn outstanding_calls(&self) -> usize {
        let calls = self.calls.locked();
        calls.running + calls.queued.len()
    }

    /// Wait up to `wait` (forever when None) for nested calls to finish,
    /// returning those that did; "interrupted" once cancelled.
    fn wait_settled(&self, wait: Option<std::time::Duration>) -> Result<Vec<Settled>, String> {
        let until = wait.map(|w| std::time::Instant::now() + w);
        let rx = self.settled_rx.locked();
        let mut done = Vec::new();
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err("interrupted".into());
            }
            let slice = match until {
                Some(u) => u
                    .saturating_duration_since(std::time::Instant::now())
                    .min(std::time::Duration::from_millis(25)),
                None => std::time::Duration::from_millis(25),
            };
            match rx.recv_timeout(slice) {
                Ok(s) => {
                    done.push(s);
                    done.extend(rx.try_iter());
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if until.is_some_and(|u| std::time::Instant::now() >= u) {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let mut calls = self.calls.locked();
        calls.running -= done.len().min(calls.running);
        self.start_queued(&mut calls);
        Ok(done)
    }

    /// Return early on cancellation; fetch workers still finish under their
    /// finite request deadline, releasing the response and socket themselves.
    fn cancellable<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        cancellable_work(&self.cancel, work)
    }

    fn build_request(
        &self,
        a: &Value,
        url: reqwest::Url,
        scope: Vec<String>,
    ) -> Result<FetchRequest, String> {
        let method = reqwest::Method::from_bytes(a["method"].as_str().unwrap_or("GET").as_bytes())
            .map_err(|e| format!("bad method: {e}"))?;
        let headers = a["headers"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect();
        let body = match a["body"].as_str() {
            Some(b) => Some(decode(b, enc_of(&a["body_encoding"])?)?),
            None => None,
        };
        Ok(FetchRequest {
            method,
            url,
            headers,
            body,
            scope,
            timeouts: FetchTimeouts::configured(),
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

    /// In a dry run, a request that may change something (anything but
    /// GET/HEAD) is logged, not sent: `Some(method)` when skipped.
    fn dry_run_skips(&self, a: &Value, url: &str) -> Option<String> {
        if self.mode != HostMode::DryRun {
            return None;
        }
        let method = a["method"].as_str().unwrap_or("GET").to_uppercase();
        if matches!(method.as_str(), "GET" | "HEAD") {
            return None;
        }
        self.would(format!("send {method} {url}"));
        Some(method)
    }

    fn fetch(&self, a: &Value) -> Result<Value, String> {
        let url = a["url"].as_str().unwrap_or_default();
        let (parsed, scope) = self.check_net(url)?;
        let enc = enc_of(&a["encoding"])?;
        if self.mode == HostMode::Smoke {
            return Ok(self.stub(json!({"status": 200, "url": url, "headers": {}, "body": ""})));
        }
        if let Some(method) = self.dry_run_skips(a, url) {
            return Ok(
                json!({"status": 200, "url": url, "headers": {}, "body": "", "dry_run": method}),
            );
        }
        let req = self.build_request(a, parsed, scope)?;
        let ops = self.ops.clone();
        let (mut meta, bytes) = self.cancellable(move || {
            let mut resp = send_request(req)?;
            let meta = Self::response_meta(&resp);
            if let Some(url) = meta["url"].as_str() {
                ops.data_observed("url", url)?;
            }
            // The body is read up to the cap and no further (#604).
            let cap = MAX_READ_BYTES.load(Ordering::Relaxed);
            if let Some(len) = resp.content_length().filter(|l| *l > cap) {
                return Err(over_limit(
                    "fetch",
                    len,
                    "tools.max_read_bytes",
                    "stream it with fetchChunks",
                ));
            }
            let mut bytes = Vec::new();
            (&mut resp)
                .take(cap + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| fetch_error(&e))?;
            if bytes.len() as u64 > cap {
                return Err(over_limit(
                    "fetch",
                    bytes.len() as u64,
                    "tools.max_read_bytes",
                    "stream it with fetchChunks",
                ));
            }
            Ok((meta, bytes))
        })?;
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
        if let Some(method) = self.dry_run_skips(a, url) {
            return Ok(
                json!({"id": 0, "status": 200, "url": url, "headers": {}, "dry_run": method}),
            );
        }
        let req = self.build_request(a, parsed, scope)?;
        let resp = self.cancellable(move || send_request(req))?;
        let mut meta = Self::response_meta(&resp);
        if let Some(url) = meta["url"].as_str() {
            self.ops.data_observed("url", url)?;
        }
        let id = self.next_stream.fetch_add(1, Ordering::SeqCst) + 1;
        self.streams.locked().insert(
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
        // Taken out of the map while reading so a blocked read can run on a
        // cancellable thread; a cancelled read drops the stream.
        let Some(mut s) = self.streams.locked().remove(&id) else {
            // Smoke stub (id 0) or an already-closed stream: empty body.
            return Ok(json!({"data": "", "eof": true}));
        };
        let (mut s, read) = self.cancellable(move || {
            let mut buf = std::mem::take(&mut s.carry);
            let mut eof = false;
            while buf.len() < size {
                let mut chunk = vec![0u8; size - buf.len()];
                let n = match s.resp.read(&mut chunk) {
                    Ok(n) => n,
                    Err(e) => return Ok((s, Err(fetch_error(&e)))),
                };
                if n == 0 {
                    eof = true;
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Ok((s, Ok((buf, eof))))
        })?;
        let (mut buf, eof) = match read {
            Ok(r) => r,
            Err(e) => {
                self.streams.locked().insert(id, s);
                return Err(e);
            }
        };
        if s.enc == Enc::Text && !eof {
            let cut = utf8_cut(&buf).max(1);
            s.carry = buf.split_off(cut);
        }
        let out = json!({"data": encode(&buf, s.enc), "eof": eof && s.carry.is_empty()});
        self.streams.locked().insert(id, s);
        Ok(out)
    }

    fn process_start(&self, a: &Value) -> Result<Option<processes::Process>, String> {
        let cmd = a["cmd"].as_str().unwrap_or_default();
        let allowed = self
            .caps
            .process
            .as_ref()
            .ok_or_else(|| denied("process not declared"))?;
        let first_arg = a["args"]
            .as_array()
            .and_then(|v| v.first())
            .and_then(|x| x.as_str());
        let possible = if self.mode == HostMode::Live {
            process_executable::possibly_allowed(allowed, cmd, first_arg)
        } else {
            allowed.is_empty() || process_allowed(allowed, cmd, first_arg)
        };
        if !possible {
            return Err(denied(format!(
                "process:{cmd}{} not in scope",
                first_arg.map(|f| format!(" {f}")).unwrap_or_default()
            )));
        }
        // Variables that change what the program loads or runs need their
        // own env grant (#1098): `PATH`, loader and interpreter hooks, git's.
        if let Some(env) = a["env"].as_object() {
            if let Some(k) = env
                .keys()
                .find(|k| steers_execution(k) && !self.caps.env.iter().any(|g| g == *k))
            {
                return Err(denied(format!(
                    "process.run env {k} changes what the program loads or runs; declare env:{k} to set it"
                )));
            }
        }
        // A working folder is a place the Tool may read (#608): inside
        // its fs grants, never anywhere on disk.
        let cwd = a["cwd"]
            .as_str()
            .map(|raw| self.process_cwd(raw))
            .transpose()?;
        if self.mode == HostMode::Replay {
            return Err(denied("process denied in replay"));
        }
        if self.mode == HostMode::Smoke {
            self.stub(Value::Null);
            return Ok(None);
        }
        if self.mode == HostMode::DryRun {
            let args: Vec<&str> = a["args"]
                .as_array()
                .map(|v| v.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            self.would(
                format!("run {cmd} {}", args.join(" "))
                    .trim_end()
                    .to_string(),
            );
            return Ok(None);
        }
        let enc = enc_of(&a["encoding"])?;
        let mut args: Vec<String> = a["args"]
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
        let effective_cwd = cwd
            .clone()
            .unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
        let child_env = process_executable::ChildEnv::capture(&self.caps.env, &a["env"]);
        let executable =
            process_executable::resolve(cmd, &effective_cwd, &child_env).map_err(|error| {
                if !allowed.is_empty() && !process_executable::names_request(allowed, cmd) {
                    // An unresolved path cannot establish identity with a scoped
                    // executable grant. Keep denial opaque, as before resolution.
                    denied(format!("process:{cmd} not in scope"))
                } else {
                    error
                }
            })?;
        if !process_executable::allowed(
            allowed,
            cmd,
            first_arg,
            &executable,
            &effective_cwd,
            &child_env,
        ) {
            return Err(denied(format!(
                "process:{cmd}{} not in scope",
                first_arg.map(|arg| format!(" {arg}")).unwrap_or_default()
            )));
        }
        let executable_raw = executable
            .raw
            .to_str()
            .ok_or_else(|| denied("process executable is not UTF-8"))?;
        self.guard_read(executable_raw, &executable.canonical, "process executable")?;
        self.process_operand_paths(executable_raw, &args, &effective_cwd)?;
        // Bind execution to the guarded canonical program. No second PATH search
        // may choose another executable after a credential-consent callback.
        let mut command = std::process::Command::new(&executable.canonical);
        // Git under a sub-command grant (`process:git:status|log`) runs
        // without the repository settings that start programs: fsmonitor,
        // pager, credential helpers, external diff and textconv (#1099, #1130).
        let base = std::path::Path::new(cmd)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(cmd);
        if base == "git"
            && allowed
                .iter()
                .any(|g| g.starts_with("git:") || g.contains("/git:"))
        {
            command.args([
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.pager=cat",
                "-c",
                "credential.helper=",
                "-c",
                "core.sshCommand=false",
            ]);
            if matches!(first_arg, Some("diff" | "log" | "show")) {
                // These are sub-command options, not global git options.
                // Put them after caller options so --ext-diff/--textconv
                // cannot turn drivers back on, but before a path separator.
                let end = args.iter().position(|a| a == "--").unwrap_or(args.len());
                args.splice(end..end, ["--no-ext-diff".into(), "--no-textconv".into()]);
            }
        }
        command
            .args(&args)
            .stdin(if stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(cwd) = &cwd {
            command.current_dir(cwd);
        }
        child_env.apply(&mut command);
        command.current_dir(&effective_cwd);
        self.process_operand_paths(executable_raw, &args, &effective_cwd)?;
        self.recheck_path(executable_raw, &executable.canonical)?;
        if let Some(cwd) = &cwd {
            self.recheck_path(a["cwd"].as_str().unwrap(), cwd)?;
        }
        let timeout = match a.get("timeout_ms").filter(|v| !v.is_null()) {
            None => None,
            Some(value) => {
                let ms = value
                    .as_u64()
                    .filter(|ms| *ms <= 86_400_000)
                    .ok_or("process timeout_ms must be an integer from 0 to 86400000")?;
                Some(std::time::Duration::from_millis(ms))
            }
        };
        self.ops.untracked_writes_possible();
        let process = processes::Process::start(command, stdin, timeout, self.cancel.clone(), enc)
            .map_err(|error| format!("{cmd}: {error}"))?;
        self.ops.data_observed("process", cmd)?;
        Ok(Some(process))
    }

    fn process_stub(&self) -> Value {
        json!({"code": 0, "signal": null, "stdout": "", "stderr": "",
            "timed_out": false, "killed": false, "truncated": false,
            "dry_run": self.mode == HostMode::DryRun})
    }

    fn process_run(&self, a: &Value) -> Result<Value, String> {
        let Some(process) = self.process_start(a)? else {
            return Ok(self.process_stub());
        };
        let cap = MAX_PROCESS_OUTPUT.load(Ordering::Relaxed) as usize;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut truncated = false;
        while let Some((is_stdout, chunk)) = process.read_bytes(65536)? {
            let out = if is_stdout { &mut stdout } else { &mut stderr };
            let room = cap.saturating_sub(out.len());
            out.extend_from_slice(&chunk[..chunk.len().min(room)]);
            truncated |= chunk.len() > room;
        }
        let mut result = process.wait()?;
        result["stdout"] = encode(&stdout, enc_of(&a["encoding"])?);
        result["stderr"] = encode(&stderr, Enc::Text);
        result["truncated"] = json!(truncated || result["truncated"] == true);
        Ok(result)
    }

    fn process_spawn(&self, a: &Value) -> Result<Value, String> {
        if self.processes.locked().len() >= processes::MAX_HANDLES {
            return Err(
                "process.spawn resource limit: at most 16 open handles; wait or close one first"
                    .into(),
            );
        }
        let Some(process) = self.process_start(a)? else {
            return Ok(json!({"id": 0}));
        };
        let id = self.next_process.fetch_add(1, Ordering::Relaxed) + 1;
        self.processes.locked().insert(id, Arc::new(process));
        Ok(json!({"id": id}))
    }

    fn process_read(&self, a: &Value) -> Result<Value, String> {
        let id = a["id"].as_u64().unwrap_or(0);
        if id == 0 && matches!(self.mode, HostMode::Smoke | HostMode::DryRun) {
            return Ok(json!({"stream": null, "data": "", "eof": true}));
        }
        let process = self
            .processes
            .locked()
            .get(&id)
            .cloned()
            .ok_or("process handle is closed or unknown to this Invocation")?;
        let size = a["size"].as_u64().unwrap_or(65536).clamp(4, 1 << 20) as usize;
        process.read(size)
    }

    fn process_finish(&self, a: &Value, kill: bool) -> Result<Value, String> {
        let id = a["id"].as_u64().unwrap_or(0);
        if id == 0 && matches!(self.mode, HostMode::Smoke | HostMode::DryRun) {
            return Ok(self.process_stub());
        }
        let process = self
            .processes
            .locked()
            .remove(&id)
            .ok_or("process handle is closed or unknown to this Invocation")?;
        if kill {
            process.request_stop();
        }
        process.wait()
    }

    fn close_processes(&self) {
        let processes: Vec<_> = std::mem::take(&mut *self.processes.locked())
            .into_values()
            .collect();
        for process in &processes {
            process.request_stop();
        }
        for process in processes {
            let _ = process.wait();
        }
    }

    /// Canonical containment check (SPEC §14): target must be inside a
    /// granted root, symlinks resolved. In Smoke mode a path that only an
    /// input-bound template could grant is a stub denial (inconclusive).
    fn guard_container(&self, path: &Path) -> Result<(), String> {
        if self
            .ops
            .path_policy()
            .map_err(denied)?
            .contains_profile(path)
            .map_err(denied)?
        {
            return Err(denied(
                "directory operation would move/remove a protected profile",
            ));
        }
        Ok(())
    }

    fn process_cwd(&self, raw: &str) -> Result<PathBuf, String> {
        let canonical = caps::canonical_path(raw).map_err(denied)?;
        self.protected_path(raw, &canonical)?;
        let contained = self
            .caps
            .fs_read
            .iter()
            .chain(&self.caps.fs_write)
            .filter(|root| !root.starts_with(INPUT_PREFIX))
            .any(|root| caps::canonical_path(root).is_ok_and(|root| canonical.starts_with(root)));
        if self.mode == HostMode::Live && !contained {
            return Err(denied(format!(
                "cwd {raw} is outside this Tool's fs grants; declare fs:read:{raw} to run there"
            )));
        }
        self.guard_read(raw, &canonical, "process working directory")?;
        Ok(canonical)
    }

    fn process_operand_paths(&self, cmd: &str, args: &[String], cwd: &Path) -> Result<(), String> {
        // Explicit operands are checked; arbitrary approved program code is
        // not confined by this scan or by an OS filesystem sandbox.
        let cwd_text = cwd
            .to_str()
            .ok_or_else(|| denied("process cwd is not UTF-8"))?;
        self.guard_read(cwd_text, cwd, "process working directory")?;
        let mut literal_arguments = false;
        let mut operands = Vec::new();
        // A bare command is resolved through PATH, not as a filesystem
        // operand under cwd. Inspect its resolved executable instead of
        // inventing an unrelated <cwd>/<command> credential identity.
        // Explicit executable paths still pass the ordinary operand checks.
        let executable = if !cmd.contains(['/', '\\']) {
            which(cmd)
        } else {
            operands.push(cmd);
            None
        };
        if let Some(path) = &executable {
            operands.push(
                path.to_str()
                    .ok_or_else(|| denied("process executable is not UTF-8"))?,
            );
        }
        for value in args.iter().map(String::as_str) {
            if value == "--" && !literal_arguments {
                literal_arguments = true;
                continue;
            }
            if !literal_arguments && value.starts_with('-') {
                if let Some((_, value)) = value.split_once('=') {
                    operands.push(value);
                }
                continue;
            }
            operands.push(value);
            // Some programs use @response files, while cat treats @ literally.
            // Inspect both spellings; never rewrite the child's actual argv.
            if let Some(response_file) = value.strip_prefix('@') {
                operands.push(response_file);
            }
        }
        for operand in operands {
            if operand.is_empty() {
                continue;
            }
            let file_uri = operand
                .get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"));
            if operand.contains("://") && !file_uri {
                continue;
            }
            let explicit_path = file_uri
                || Path::new(operand).has_root()
                || operand.contains('/')
                || (cfg!(windows) && operand.contains('\\'))
                || operand.starts_with('.');
            let path = if file_uri {
                reqwest::Url::parse(operand)
                    .map_err(|e| denied(e.to_string()))?
                    .to_file_path()
                    .map_err(|_| denied("invalid file operand URI"))?
            } else {
                PathBuf::from(operand)
            };
            let path = caps::lexical_absolute(cwd, &path).map_err(denied)?;
            let raw = path
                .to_str()
                .ok_or_else(|| denied("process operand is not UTF-8"))?;
            let canonical = match caps::canonical_path(raw) {
                Ok(path) => path,
                Err(_)
                    if !explicit_path
                        && self.path_decision(raw, &path)?
                            == sensitive_paths::PathDecision::Ordinary =>
                {
                    continue
                }
                Err(error) => return Err(denied(error)),
            };
            self.guard_read(raw, &canonical, "process path operand")?;
        }
        Ok(())
    }

    fn path_decision(
        &self,
        raw: &str,
        canonical: &Path,
    ) -> Result<sensitive_paths::PathDecision, String> {
        let configured = caps::lexical_absolute(
            &std::env::current_dir().map_err(|e| denied(e.to_string()))?,
            Path::new(raw),
        )
        .map_err(denied)?;
        self.ops
            .path_policy()
            .map_err(denied)?
            .classify(&configured, canonical)
            .map_err(denied)
    }

    fn protected_path(&self, raw: &str, canonical: &Path) -> Result<(), String> {
        if self.path_decision(raw, canonical)? == sensitive_paths::PathDecision::Forbidden {
            return Err(denied(format!(
                "direct access to the Formless profile is forbidden: {}",
                canonical.display()
            )));
        }
        Ok(())
    }

    fn recheck_path(&self, raw: &str, expected: &Path) -> Result<(), String> {
        let current = caps::canonical_path(raw).map_err(denied)?;
        if current != expected {
            return Err(denied("path changed while awaiting credential consent"));
        }
        self.protected_path(raw, &current)
    }

    fn guard_read(&self, raw: &str, canonical: &Path, operation: &str) -> Result<(), String> {
        match self.path_decision(raw, canonical)? {
            sensitive_paths::PathDecision::Forbidden => self.protected_path(raw, canonical)?,
            sensitive_paths::PathDecision::Ordinary => {}
            sensitive_paths::PathDecision::Sensitive(reason) => {
                if matches!(self.mode, HostMode::Smoke | HostMode::Replay) {
                    let error = denied(format!(
                        "credential read unavailable in noninteractive mode: {}",
                        canonical.display()
                    ));
                    return Err(if self.mode == HostMode::Smoke {
                        self.stub(error)
                    } else {
                        error
                    });
                }
                let allowed = self.read_consents.locked().contains(canonical);
                if !allowed {
                    self.ops
                        .approve_sensitive_read(canonical, operation, &reason)
                        .map_err(denied)?;
                    self.read_consents.locked().insert(canonical.to_path_buf());
                }
            }
        }
        self.recheck_path(raw, canonical)
    }

    fn requested_path(raw: &str) -> Option<PathBuf> {
        caps::lexical_absolute(&std::env::current_dir().ok()?, Path::new(raw)).ok()
    }
    fn changing(&self, raw: &str, resolved: &Path, operation: &str) {
        if let Some(requested) = Self::requested_path(raw) {
            self.ops
                .file_changing_resolved(&requested, resolved, operation);
        }
    }
    fn changed(&self, raw: &str, resolved: &Path, operation: &str) {
        if let Some(requested) = Self::requested_path(raw) {
            self.ops.file_changed(&requested, resolved, operation);
        }
    }
    fn create_tracked_directories(&self, path: &Path) -> Result<(), String> {
        let mut missing = Vec::new();
        let mut next = Some(path);
        while let Some(dir) = next {
            match std::fs::symlink_metadata(dir) {
                Ok(metadata) if metadata.is_dir() => break,
                Ok(_) => {
                    return Err(format!(
                        "{}: an existing path is not a directory",
                        dir.display()
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(dir.to_owned())
                }
                Err(error) => return Err(error.to_string()),
            }
            next = dir.parent();
        }
        for dir in missing.into_iter().rev() {
            match std::fs::create_dir(&dir) {
                Ok(()) => self.ops.directory_created(&dir),
                // A racing creator owns this directory; do not claim creation.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {
                }
                Err(error) => return Err(format!("{}: {error}", dir.display())),
            }
        }
        Ok(())
    }
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
        let canonical = match caps::canonical_path(raw) {
            Ok(canonical) => canonical,
            Err(_) if smoke_stub => return Err(stub_denial()),
            Err(error) => return Err(denied(error)),
        };
        self.protected_path(raw, &canonical)?;
        if fixed.is_empty() {
            return Err(if smoke_stub {
                stub_denial()
            } else {
                denied(format!("fs:{kind} not declared"))
            });
        }

        for r in fixed {
            let root = caps::canonical_path(r).unwrap_or_else(|_| PathBuf::from(r));
            if canonical.starts_with(&root) {
                // A write may implicitly read old contents for append/diff
                // snapshots. Protect existing files before that callback too.
                if !write || canonical.is_file() {
                    self.guard_read(
                        raw,
                        &canonical,
                        if write {
                            "existing file change (may retain prior contents)"
                        } else {
                            "filesystem read"
                        },
                    )?;
                }
                if !write {
                    self.ops
                        .data_observed("file", &canonical.to_string_lossy())?;
                }
                self.recheck_path(raw, &canonical)?;
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
    run_capture_impl(cmd, args, stdin_text, false)
}

fn run_capture_impl(
    cmd: &str,
    args: &[&str],
    stdin_text: Option<&str>,
    detailed: bool,
) -> Result<String, String> {
    use std::io::Write;
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            if detailed {
                clipboard_diagnostics::failure(cmd, "could not spawn", &e.to_string(), stdin_text)
            } else {
                format!("{cmd}: {e}")
            }
        })?;
    if let Some(text) = stdin_text {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
    }
    let out = child.wait_with_output().map_err(|e| {
        if detailed {
            clipboard_diagnostics::failure(cmd, "could not wait", &e.to_string(), stdin_text)
        } else {
            e.to_string()
        }
    })?;
    if !out.status.success() {
        if detailed {
            return Err(clipboard_diagnostics::failure(
                cmd,
                &format!("exited unsuccessfully ({})", out.status),
                &String::from_utf8_lossy(&out.stderr),
                stdin_text,
            ));
        }
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
        clipboard_diagnostics::read()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err("clipboard unsupported on this platform".into())
    }
}

/// Put text on the OS clipboard (pbcopy, wl-copy/xclip/xsel, PowerShell).
pub fn clipboard_write(text: &str) -> Result<(), String> {
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
        clipboard_diagnostics::write(text)
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

fn listing_directory<T>(
    path: &Path,
    overlay: bool,
    open: impl FnOnce(&Path) -> std::io::Result<T>,
) -> Result<Option<T>, String> {
    match open(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(_) if overlay => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn classify(msg: &str, cancelled: bool) -> ExecError {
    if cancelled {
        return ExecError {
            kind: ExecErrorKind::Cancelled,
            message: "interrupted".into(),
            after_stub: false,
            logs: Vec::new(),
            nested: None,
        };
    }
    let low = msg.to_lowercase();
    let kind = if msg.starts_with("denied:") {
        ExecErrorKind::Denied
    } else if low.contains("resource limit")
        || low.contains("out of memory")
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
        logs: Vec::new(),
        nested: None,
    }
}

/// Recognize declarations for the returned object, not declarations belonging to
/// nested properties or unused definitions. Alternative/conditional branches only
/// contribute when they validate this output. Local references retain root context.
fn declares_output_error(schema: &Value, output: &Value) -> bool {
    const MAX_DEPTH: usize = 64;
    const MAX_NODES: usize = 4096;
    fn bounded(value: &Value, depth: usize, left: &mut usize) -> bool {
        if depth > MAX_DEPTH || *left == 0 {
            return false;
        }
        *left -= 1;
        match value {
            Value::Array(values) => values.iter().all(|v| bounded(v, depth + 1, left)),
            Value::Object(values) => values.values().all(|v| bounded(v, depth + 1, left)),
            _ => true,
        }
    }
    let mut remaining = MAX_NODES;
    if !bounded(schema, 0, &mut remaining) {
        return false;
    }
    // Offline compilation cannot turn inspecting Tool output into a network read.
    let validators = jsonschema::options().offline().build_map(schema).ok();
    struct Declarations<'a> {
        root: &'a Value,
        output: &'a Value,
        validators: Option<jsonschema::ValidatorMap>,
        left: usize,
    }
    impl Declarations<'_> {
        fn reference(&self, node: &Value) -> Option<String> {
            let reference = node.get("$ref")?.as_str()?;
            // JSON Pointer references only: external resources and dynamic
            // anchors cannot be resolved by guessing against the root document.
            let pointer = reference.strip_prefix('#')?;
            (pointer.is_empty() || pointer.starts_with('/'))
                .then(|| self.root.pointer(pointer).map(|_| reference.to_string()))?
        }
        fn visit(&mut self, path: &str, stack: &mut Vec<String>) -> bool {
            if self.left == 0 || stack.len() >= MAX_DEPTH || stack.iter().any(|p| p == path) {
                return false;
            }
            self.left -= 1;
            let Some(node) = self.root.pointer(path.strip_prefix('#').unwrap_or(path)) else {
                return false;
            };
            stack.push(path.to_string());
            let declared = node["properties"]
                .as_object()
                .is_some_and(|p| p.contains_key("error"));
            let mut result = declared;
            if let Some(reference) = self.reference(node) {
                result |= self.visit(&reference, stack);
            }
            for keyword in ["allOf", "anyOf", "oneOf"] {
                if let Some(branches) = node[keyword].as_array() {
                    for index in 0..branches.len() {
                        let branch = format!("{path}/{keyword}/{index}");
                        if keyword == "allOf" || self.matches(&branch) {
                            result |= self.visit(&branch, stack);
                        }
                    }
                }
            }
            if node.get("if").is_some() {
                if let Some(matches) = self.match_result(&format!("{path}/if")) {
                    let selected = if matches { "then" } else { "else" };
                    result |= self.visit(&format!("{path}/{selected}"), stack);
                }
            }
            if let Some(dependencies) = node["dependentSchemas"].as_object() {
                for key in dependencies.keys() {
                    if self.output.get(key).is_some() {
                        let escaped = key.replace('~', "~0").replace('/', "~1");
                        result |= self.visit(&format!("{path}/dependentSchemas/{escaped}"), stack);
                    }
                }
            }
            stack.pop();
            result
        }
        // Validators handle recursive object schemas, but a reference cycle that
        // reapplies to the same instance can recurse forever. Refuse that graph
        // before asking whether an alternative/condition matches.
        fn acyclic(&self, path: &str, stack: &mut Vec<String>, left: &mut usize) -> bool {
            if *left == 0 || stack.len() >= MAX_DEPTH || stack.iter().any(|p| p == path) {
                return false;
            }
            *left -= 1;
            let Some(node) = self.root.pointer(path.strip_prefix('#').unwrap_or(path)) else {
                return false;
            };
            stack.push(path.to_string());
            let mut safe = self
                .reference(node)
                .is_none_or(|p| self.acyclic(&p, stack, left));
            for keyword in ["allOf", "anyOf", "oneOf"] {
                if let Some(branches) = node[keyword].as_array() {
                    safe &= (0..branches.len())
                        .all(|i| self.acyclic(&format!("{path}/{keyword}/{i}"), stack, left));
                }
            }
            for keyword in ["if", "then", "else", "not"] {
                if node.get(keyword).is_some() {
                    safe &= self.acyclic(&format!("{path}/{keyword}"), stack, left);
                }
            }
            if let Some(dependencies) = node["dependentSchemas"].as_object() {
                for key in dependencies.keys() {
                    let escaped = key.replace('~', "~0").replace('/', "~1");
                    safe &=
                        self.acyclic(&format!("{path}/dependentSchemas/{escaped}"), stack, left);
                }
            }
            stack.pop();
            safe
        }
        fn match_result(&self, path: &str) -> Option<bool> {
            let mut remaining = MAX_NODES;
            if !self.acyclic(path, &mut Vec::new(), &mut remaining) {
                return None;
            }
            Some(self.validators.as_ref()?.get(path)?.is_valid(self.output))
        }
        fn matches(&self, path: &str) -> bool {
            self.match_result(path) == Some(true)
        }
    }
    Declarations {
        root: schema,
        output,
        validators,
        left: MAX_NODES,
    }
    .visit("#", &mut Vec::new())
}

/// Undeclared `{error: <truthy>}` results report failure (§8.1). An error
/// property explicitly declared in the output schema is ordinary domain data.
fn reported_error(output: &Value, output_schema: Option<&Value>) -> Option<String> {
    let e = output.as_object()?.get("error")?;
    let truthy = match e {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    };
    if !truthy || output_schema.is_some_and(|schema| declares_output_error(schema, output)) {
        return None;
    }
    Some(match e {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// Syntax + `run` presence check, without executing `run` (SPEC §7.1).
pub fn check_syntax(code: &str) -> Result<(), String> {
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    // The check evaluates the top level: bound it, so a loop or a huge
    // allocation outside run() fails the check instead of hanging it.
    const LIMIT: std::time::Duration = std::time::Duration::from_secs(3);
    let deadline = std::time::Instant::now() + LIMIT;
    runtime.set_interrupt_handler(Some(Box::new(move || {
        std::time::Instant::now() >= deadline
    })));
    runtime.set_memory_limit(256 * 1024 * 1024);
    let ctx = Context::full(&runtime).map_err(|e| e.to_string())?;
    let timed_out = move || std::time::Instant::now() >= deadline;
    ctx.with(|ctx| {
        let host = Function::new(ctx.clone(), |_op: String, _a: String| {
            r#"{"ok":true,"value":null}"#.to_string()
        })
        .map_err(|e: rquickjs::Error| e.to_string())?;
        ctx.globals()
            .set("__host", host)
            .map_err(|e: rquickjs::Error| e.to_string())?;
        ctx.eval::<(), _>(PRELUDE).map_err(|e| e.to_string())?;
        ctx.eval::<(), _>(code).map_err(|e| {
            if timed_out() {
                format!(
                    "top-level code did not finish within {}s (an endless loop outside run()?)",
                    LIMIT.as_secs()
                )
            } else if matches!(e, rquickjs::Error::Exception) {
                // The thrown error itself: "SyntaxError: … at <eval>:2:13",
                // not rquickjs's generic "Exception generated by QuickJS".
                let thrown = ctx.catch();
                match thrown.as_exception() {
                    Some(ex) => {
                        let what = ex.message().unwrap_or_default();
                        let kind: String = ex.get("name").unwrap_or_else(|_| "Error".into());
                        let at = ex
                            .stack()
                            .and_then(|s| {
                                s.lines()
                                    .map(str::trim)
                                    .find(|l| !l.is_empty())
                                    .map(str::to_string)
                            })
                            .unwrap_or_default();
                        format!("{kind}: {what} {at}").trim().to_string()
                    }
                    None => e.to_string(),
                }
            } else {
                e.to_string()
            }
        })?;
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
  (function (heapLimitBytes) {
    // QuickJS throws null when it cannot even allocate the out-of-memory
    // error, so a null exception is reported as out of memory.
    const fail = (e) => { globalThis.__result_json = JSON.stringify({ ok: false, error: e === null ? "out of memory (heap limit reached) (configured limit: " + heapLimitBytes + " bytes)" : String((e && e.message) || e) + "\n" + String((e && e.stack) || ""), nested: __nestedError(e) }); };
    const done = (v) => {
      try {
        const encoded = JSON.stringify(v === undefined ? null : v);
        if (typeof encoded !== "string") throw new Error("JSON.stringify produced no JSON value");
        // Serialize the Tool's value once: wrapping it and stringifying again
        // would either omit non-values or run a stateful toJSON twice.
        globalThis.__result_json = '{"ok":true,"value":' + encoded + '}';
      }
      catch (e) { fail(new Error("return value is not JSON-serializable: " + String((e && e.message) || e))); }
    };
    try {
      const out = run(JSON.parse(globalThis.__input_json));
      if (out && typeof out.then === "function") out.then(done, fail); else done(out);
    } catch (e) { fail(e); }
  })
"#;

fn effective_heap_limit_bytes(memory_mb: u32) -> usize {
    (memory_mb as usize).saturating_mul(1024 * 1024)
}

fn nested_error<'js>(
    ctx: &rquickjs::Ctx<'js>,
    reason: rquickjs::Value<'js>,
) -> Option<NestedToolError> {
    let getter: Function<'js> = ctx.eval("__nestedError").ok()?;
    let value: rquickjs::Value<'js> = match getter.call((reason,)) {
        Ok(value) => value,
        Err(_) => {
            let _ = ctx.catch();
            return None;
        }
    };
    let object = value.as_object()?;
    Some(NestedToolError {
        tool: object.get("tool").ok()?,
        detail: object.get("detail").ok()?,
    })
}

/// Execute one Tool Invocation. Blocking; run on a dedicated thread.
pub fn execute(req: &ExecRequest, ops: Arc<dyn HostOps>) -> Result<ExecOutcome, ExecError> {
    let (settled_tx, settled_rx) = std::sync::mpsc::channel();
    let state = Arc::new(State {
        read_consents: Mutex::new(Default::default()),
        caps: req.caps.clone(),
        mode: req.mode,
        ops,
        logs: Mutex::new(Vec::new()),
        overlay: req.overlay.clone().unwrap_or_else(new_overlay),
        modules: req.modules.clone(),
        streams: Mutex::new(HashMap::new()),
        next_stream: AtomicU64::new(0),
        processes: Mutex::new(HashMap::new()),
        next_process: AtomicU64::new(0),
        stubbed: AtomicBool::new(false),
        cancel: req.cancel.clone(),
        kv_overlay: Mutex::new(Default::default()),
        calls: Mutex::new(AsyncCalls::default()),
        settled_tx,
        settled_rx: Mutex::new(settled_rx),
    });
    // State can outlive this call through nested async work. Native children
    // cannot: every success, JS error, rejection and interruption closes them.
    struct CloseProcesses(Arc<State>);
    impl Drop for CloseProcesses {
        fn drop(&mut self) {
            self.0.close_processes();
        }
    }
    let _processes = CloseProcesses(state.clone());
    let cancelled = req.cancel.clone();
    let err = |e: &dyn std::fmt::Display| {
        let mut x = classify(&e.to_string(), cancelled.load(Ordering::SeqCst));
        x.after_stub = state.stubbed.load(Ordering::SeqCst);
        x.logs = state.logs.lock().map(|l| l.clone()).unwrap_or_default();
        x
    };
    let runtime = Runtime::new().map_err(|e| err(&e))?;
    // Keep promise identities alive until completion (a raw pointer/hash could
    // be reused after GC). Clear persistent handles before Runtime is dropped,
    // including on early error paths, as required by rquickjs::Persistent.
    #[derive(Clone)]
    struct Rejection {
        message: String,
        reason: rquickjs::Persistent<rquickjs::Value<'static>>,
    }
    type Rejections = std::rc::Rc<
        std::cell::RefCell<HashMap<rquickjs::Persistent<rquickjs::Value<'static>>, Rejection>>,
    >;
    struct ClearRejections(Rejections);
    impl Drop for ClearRejections {
        fn drop(&mut self) {
            self.0.borrow_mut().clear();
        }
    }
    let unhandled: Rejections = Default::default();
    let _clear_rejections = ClearRejections(unhandled.clone());
    let rejected = unhandled.clone();
    runtime.set_host_promise_rejection_tracker(Some(Box::new(
        move |ctx, promise, reason, handled| {
            let promise = rquickjs::Persistent::save(&ctx, promise);
            if handled {
                rejected.borrow_mut().remove(&promise);
            } else {
                let message = reason
                    .as_exception()
                    .and_then(|e| e.message())
                    .or_else(|| reason.as_string().and_then(|s| s.to_string().ok()))
                    .unwrap_or_else(|| format!("{reason:?}"));
                rejected.borrow_mut().insert(
                    promise,
                    Rejection {
                        message,
                        reason: rquickjs::Persistent::save(&ctx, reason),
                    },
                );
            }
        },
    )));

    let heap_limit_bytes = effective_heap_limit_bytes(req.memory_mb);
    runtime.set_memory_limit(heap_limit_bytes);
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
                if let Some(a) = activity.as_ref().filter(|_| op != "progress") {
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
        let mut host_options = EvalOptions::default();
        host_options.filename = Some("formless-host.js".into());
        ctx.eval_with_options::<(), _>(PRELUDE, host_options)
            .map_err(|e| err(&e))?;
        let mut tool_options = EvalOptions::default();
        tool_options.filename = Some(TOOL_SOURCE_NAME.into());
        ctx.eval_with_options::<(), _>(req.code.as_str(), tool_options)
            .map_err(|e| err(&e))?;
        let mut runner_options = EvalOptions::default();
        runner_options.filename = Some("formless-runner.js".into());
        ctx.eval_with_options::<(), _>(
            format!("{}({heap_limit_bytes});", RUNNER.trim()),
            runner_options,
        )
        .map_err(|e| err(&e))
    })?;
    // Event loop: drain the job queue (an async run() settles through it),
    // then fire due timers, waiting until the next one or a nested call's
    // result. It ends when the result is set (leftover timers are dropped)
    // or nothing is left to run.
    let result_json: Option<String> = loop {
        while runtime.is_job_pending() {
            if runtime.execute_pending_job().is_err() {
                break;
            }
        }
        let (result, next_timer) = ctx.with(|ctx| -> Result<(Option<String>, i64), ExecError> {
            let result = ctx
                .globals()
                .get::<_, Option<String>>("__result_json")
                .map_err(|e| err(&e))?;
            if result.is_some() {
                return Ok((result, -1));
            }
            let next = ctx.eval::<i64, _>("__runTimers()").map_err(|e| err(&e))?;
            Ok((None, next))
        })?;
        if result.is_some() {
            break result;
        }
        let outstanding = state.outstanding_calls();
        if next_timer < 0 && outstanding == 0 && !runtime.is_job_pending() {
            break None;
        }
        let next = (next_timer >= 0).then(|| std::time::Duration::from_millis(next_timer as u64));
        if outstanding > 0 {
            let settled = state.wait_settled(next).map_err(|e| err(&e))?;
            ctx.with(|ctx| -> Result<(), ExecError> {
                for (id, result) in settled {
                    let r = match result {
                        Ok(v) => json!({"ok": true, "value": v}),
                        Err(e) => json!({"ok": false, "error": e}),
                    };
                    let settle: Function =
                        ctx.globals().get("__settleCall").map_err(|e| err(&e))?;
                    let value = ctx.json_parse(r.to_string()).map_err(|e| err(&e))?;
                    settle.call::<_, ()>((id, value)).map_err(|e| err(&e))?;
                }
                Ok(())
            })?;
        } else if let Some(next) = next.filter(|n| !n.is_zero()) {
            if !state.sleep_cancellable(next) {
                return Err(err(&"interrupted"));
            }
        }
    };
    let result_json =
        result_json.ok_or_else(|| err(&"run() returned a Promise that never settled"))?;
    // A call still running when run() is done was never awaited: its
    // result cannot have reached the output, so the output is suspect.
    let unsettled = state.outstanding_calls();
    if unsettled > 0 {
        return Err(err(&format!(
            "run() finished while {unsettled} tools.call result(s) were still pending: \
             tools.call returns a Promise, await every call"
        )));
    }
    let rejection = unhandled.borrow().values().next().cloned();
    if let Some(rejection) = rejection {
        let mut message = format!("unhandled Promise rejection: {}", rejection.message);
        let nested = ctx.with(|ctx| {
            let reason = rejection.reason.clone().restore(&ctx).ok()?;
            if let Some(stack) = reason
                .as_exception()
                .and_then(|exception| exception.stack())
            {
                message.push('\n');
                message.push_str(&stack);
            }
            nested_error(&ctx, reason)
        });
        let mut error = err(&message);
        error.nested = nested;
        return Err(error);
    }
    let parsed: Value = serde_json::from_str(&result_json).map_err(|e| err(&e))?;
    let logs = state.logs.lock().map(|g| g.clone()).unwrap_or_default();
    if !parsed["ok"].as_bool().unwrap_or(false) {
        let mut error = err(&parsed["error"].as_str().unwrap_or("tool error"));
        error.nested = serde_json::from_value(parsed["nested"].clone()).ok();
        return Err(error);
    }
    let output = parsed["value"].clone();
    if let Some(msg) = reported_error(&output, req.output_schema.as_ref()) {
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
    fn structured_incomplete_reasons_keep_runtime_error_classification() {
        use structured_output::{IncompleteReason, StructuredOutputError};
        for (reason, expected) in [
            (
                IncompleteReason::Unknown,
                "structured output response was incomplete",
            ),
            (
                IncompleteReason::OutputLimit,
                "structured output response was incomplete (output limit reached)",
            ),
            (
                IncompleteReason::ContextWindow,
                "structured output response was incomplete (model context window exceeded)",
            ),
            (
                IncompleteReason::Truncated,
                "structured output response was incomplete (response was truncated)",
            ),
        ] {
            let text = StructuredOutputError::IncompleteResponse(reason).to_string();
            assert_eq!(text, expected);
            assert_eq!(classify(&text, false).kind, ExecErrorKind::Js);
            assert_eq!(classify(&text, true).kind, ExecErrorKind::Cancelled);
        }
    }

    fn checked_address(
        url: &reqwest::Url,
        scope: &[String],
    ) -> Result<Vec<std::net::SocketAddr>, String> {
        super::checked_address(
            url,
            scope,
            std::time::Instant::now() + std::time::Duration::from_secs(2),
            &dns::resolve,
        )
    }

    #[test]
    fn bounded_dns_keeps_all_address_checks_and_literal_ip_bypass() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let literal = reqwest::Url::parse("http://127.0.0.1:1234/").unwrap();
        let never_resolve =
            |_: &str, _: u16, _: std::time::Instant| -> Result<Vec<std::net::SocketAddr>, String> {
                panic!("literal addresses must not enter the resolver pool")
            };
        assert!(
            super::checked_address(&literal, &["127.0.0.1".into()], deadline, &never_resolve,)
                .unwrap()
                .is_empty()
        );
        let named = reqwest::Url::parse("http://sub.example.com:1234/").unwrap();
        let mixed = |_: &str, _: u16, _: std::time::Instant| {
            Ok(vec![
                "1.1.1.1:1234".parse().unwrap(),
                "127.0.0.1:1234".parse().unwrap(),
            ])
        };
        assert!(
            super::checked_address(&named, &["example.com".into()], deadline, &mixed)
                .unwrap_err()
                .starts_with("denied:")
        );
        assert_eq!(
            super::checked_address(&named, &["sub.example.com".into()], deadline, &mixed)
                .unwrap()
                .len(),
            2
        );
        assert!(
            super::checked_address(&named, &[], deadline, &|_, _, _| Ok(Vec::new()))
                .unwrap_err()
                .contains("no addresses returned")
        );
    }

    /// Records `progress` calls.
    struct ProgressOps(Mutex<Vec<(Option<f64>, String)>>);
    impl HostOps for ProgressOps {
        fn progress(&self, fraction: Option<f64>, label: &str) {
            self.0.locked().push((fraction, label.to_string()));
        }
    }

    #[test]
    fn progress_reaches_the_host_clamped() {
        let ops = Arc::new(ProgressOps(Mutex::new(Vec::new())));
        let req = live(
            "function run() { progress(0.5, 'half'); progress(7); progress('x', null); return 1; }",
        );
        execute(&req, ops.clone()).unwrap();
        assert_eq!(
            *ops.0.locked(),
            [
                (Some(0.5), "half".into()),
                (Some(1.0), String::new()),
                (None, String::new())
            ]
        );
    }

    #[test]
    fn progress_does_not_refresh_the_generic_activity_heartbeat() {
        let ops = Arc::new(ProgressOps(Mutex::new(Vec::new())));
        let heartbeat = Arc::new(AtomicU64::new(1));
        let mut req = live("function run(){ for(let i=0;i<50;i++){ progress(0.5,'working'); __call('progress',{fraction:0.5,label:'raw'}); } return 1; }");
        req.activity = Some(heartbeat.clone());
        execute(&req, ops.clone()).unwrap();
        assert_eq!(ops.0.locked().len(), 100);
        assert_eq!(
            heartbeat.load(Ordering::SeqCst),
            1,
            "only the daemon may credit actual progress advancement"
        );
        req.code = "function run(){ emit('ordinary work'); return 1; }".into();
        execute(&req, ops).unwrap();
        assert!(
            heartbeat.load(Ordering::SeqCst) > 1,
            "other host activity keeps its established heartbeat behavior"
        );
    }

    #[test]
    fn private_addresses_need_an_exact_grant() {
        use std::net::IpAddr;
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.9",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fd00::5",
            "::ffff:10.0.0.1",
            "0.0.0.0",
        ] {
            assert!(non_public(ip.parse::<IpAddr>().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700::1111"] {
            assert!(!non_public(ip.parse::<IpAddr>().unwrap()), "{ip}");
        }
        let url = |u: &str| reqwest::Url::parse(u).unwrap();
        let err = checked_address(&url("http://169.254.169.254/latest"), &[]).unwrap_err();
        assert!(
            err.starts_with("denied:") && err.contains("169.254.169.254"),
            "{err}"
        );
        assert!(checked_address(&url("http://127.0.0.1:11434/v1"), &["127.0.0.1".into()]).is_ok());
        // A parent-domain grant doesn't reach a private address.
        assert!(checked_address(&url("http://localhost/"), &["host".into()]).is_err());
        assert!(checked_address(&url("http://localhost/"), &["localhost".into()]).is_ok());
    }

    /// IANA special-purpose families plus deprecated site-local addresses.
    /// https://www.iana.org/assignments/iana-ipv6-special-registry/
    /// https://www.iana.org/assignments/iana-ipv4-special-registry/
    #[test]
    fn special_purpose_registry_addresses_are_not_public() {
        use std::net::IpAddr;
        for (name, prefix, ip) in [
            ("This network", "0.0.0.0/8", "0.1.2.3"),
            ("Private-use", "10.0.0.0/8", "10.1.2.3"),
            ("Shared address space", "100.64.0.0/10", "100.64.0.1"),
            ("Loopback", "127.0.0.0/8", "127.0.0.1"),
            ("Link-local", "169.254.0.0/16", "169.254.169.254"),
            ("Private-use", "172.16.0.0/12", "172.16.0.1"),
            ("Private-use", "192.168.0.0/16", "192.168.0.1"),
            ("Documentation TEST-NET-1", "192.0.2.0/24", "192.0.2.1"),
            (
                "Documentation TEST-NET-2",
                "198.51.100.0/24",
                "198.51.100.1",
            ),
            ("Documentation TEST-NET-3", "203.0.113.0/24", "203.0.113.1"),
            ("Multicast", "224.0.0.0/4", "224.0.0.1"),
            ("Limited broadcast", "255.255.255.255/32", "255.255.255.255"),
            ("Loopback", "::1/128", "::1"),
            ("Unspecified", "::/128", "::"),
            ("Unique-local", "fc00::/7", "fd00::1"),
            ("Link-local unicast", "fe80::/10", "fe80::1"),
            ("Multicast", "ff00::/8", "ff00::1"),
            ("IPv4-compatible private", "::/96", "::a9fe:a9fe"),
            ("IPv4-mapped private", "::ffff:0:0/96", "::ffff:10.0.0.1"),
            ("NAT64 private", "64:ff9b::/96", "64:ff9b::a9fe:a9fe"),
            ("NAT64 loopback", "64:ff9b::/96", "64:ff9b::7f00:1"),
            ("Local-use NAT64", "64:ff9b:1::/48", "64:ff9b:1::1"),
            ("6to4 private", "2002::/16", "2002:a9fe:a9fe::1"),
            ("Teredo", "2001::/32", "2001:0:4136:e378::1"),
            ("Documentation", "2001:db8::/32", "2001:db8::1"),
            ("Discard-only", "100::/64", "100::1"),
            ("Reserved", "240.0.0.0/4", "240.0.0.1"),
            ("Reserved upper end", "240.0.0.0/4", "255.255.255.254"),
            ("Benchmarking", "198.18.0.0/15", "198.18.0.1"),
            ("IETF protocol assignments", "192.0.0.0/24", "192.0.0.8"),
            ("Deprecated site-local", "fec0::/10", "fec0::1"),
            (
                "Deprecated site-local upper end",
                "fec0::/10",
                "feff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            ),
            ("Benchmarking", "2001:2::/48", "2001:2::1"),
            (
                "Benchmarking upper end",
                "2001:2::/48",
                "2001:2:0:ffff:ffff:ffff:ffff:ffff",
            ),
            ("Documentation", "3fff::/20", "3fff::1"),
            (
                "Documentation upper end",
                "3fff::/20",
                "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff",
            ),
        ] {
            assert!(
                non_public(ip.parse::<IpAddr>().unwrap()),
                "{name} ({prefix}): {ip}"
            );
        }
        for ip in [
            "64:ff9b::808:808",
            "2002:808:808::1",
            "198.20.0.1",
            "2001:2:1::1",
            "3fff:1000::1",
            "3ffe:ffff::1",
        ] {
            assert!(!non_public(ip.parse::<IpAddr>().unwrap()), "{ip}");
        }
        let url = |u: &str| reqwest::Url::parse(u).unwrap();
        for u in ["http://[64:ff9b::a9fe:a9fe]/", "http://[::a9fe:a9fe]/"] {
            let err = checked_address(&url(u), &["example.com".into()]).unwrap_err();
            assert!(err.starts_with("denied:"), "{err}");
        }
        for ip in ["fec0::1", "2001:2::1", "3fff::1"] {
            let u = url(&format!("http://[{ip}]/"));
            assert!(checked_address(&u, &[]).unwrap_err().starts_with("denied:"));
            assert!(checked_address(&u, &[ip.to_string()]).is_ok());
        }
        assert!(checked_address(&url("http://[::a9fe:a9fe]/"), &["::a9fe:a9fe".into()]).is_ok());
    }

    /// The private-address exception honours the grant's port, and IPv6
    /// host:port grants match in their bracketed form (#1094).
    #[test]
    fn exact_grants_match_port_and_ipv6() {
        let g = vec![
            "example.com".to_string(),
            "api.example.com:8443".to_string(),
        ];
        assert!(names_host_exactly(&g, "api.example.com", 8443));
        assert!(
            !names_host_exactly(&g, "api.example.com", 22),
            "a parent grant plus host:port is not any port"
        );
        let v6 = vec!["[::1]:8080".to_string()];
        assert!(host_in_scope(&v6, "[::1]", Some(8080)));
        assert!(!host_in_scope(&v6, "[::1]", Some(9090)));
        assert!(names_host_exactly(&v6, "::1", 8080));
        let url = |u: &str| reqwest::Url::parse(u).unwrap();
        assert!(checked_address(&url("http://[::1]:8080/"), &v6).is_ok());
        assert!(checked_address(&url("http://[::1]:9090/"), &v6).is_err());
        assert!(host_in_scope(&["::1".to_string()], "[::1]", Some(1)));
    }

    #[test]
    fn ports_in_grants_are_kept_and_enforced() {
        let g = vec![
            "api.example.com:8443".to_string(),
            "example.org".to_string(),
        ];
        assert!(host_in_scope(&g, "api.example.com", Some(8443)));
        assert!(
            !host_in_scope(&g, "api.example.com", Some(443)),
            "not every port"
        );
        assert!(
            host_in_scope(&g, "example.org", Some(8080)),
            "portless grants keep any port on the exact host"
        );
    }

    #[test]
    fn fetch_and_redirect_scope_require_explicit_child_authority() {
        let exact = vec!["example.com".into()];
        assert!(host_in_scope(&exact, "example.com", Some(443)));
        assert!(!host_in_scope(&exact, "x.example.com", Some(443)));
        let explicit = vec!["*.example.com:443".into()];
        assert!(host_in_scope(&explicit, "x.example.com", Some(443)));
        assert!(!host_in_scope(&explicit, "example.com", Some(443)));
        assert!(!host_in_scope(&explicit, "x.example.com", Some(80)));
        assert!(
            !names_host_exactly(&explicit, "x.example.com", 443),
            "a wildcard never enables the private-address exception"
        );
        let private = reqwest::Url::parse("http://127.0.0.1:8080/").unwrap();
        assert!(checked_address(&private, &["*.0.0.1".into()]).is_err());
        assert!(checked_address(&private, &["127.0.0.1:8080".into()]).is_ok());
    }

    #[test]
    fn process_grants_scope_sub_commands_and_cwd() {
        assert_eq!(
            process_grant("git:status|log"),
            ("git", Some(vec!["status", "log"]))
        );
        assert_eq!(
            process_grant("C:\\tools\\x.exe"),
            ("C:\\tools\\x.exe", None)
        );
        let allowed = vec!["git:status|log".to_string()];
        assert!(process_allowed(&allowed, "git", Some("log")));
        assert!(!process_allowed(&allowed, "git", Some("push")));
        assert!(!process_allowed(&allowed, "git", None));
        // A working folder outside the fs grants is refused.
        let mut req = live("function run() { return process.run('ls', [], {cwd: '/'}); }");
        req.caps = Capabilities::parse(&["process:ls".to_string()]).unwrap();
        let e = execute(&req, Arc::new(NoHostOps)).unwrap_err();
        assert!(
            e.message.contains("outside this Tool's fs grants"),
            "{}",
            e.message
        );
    }

    /// Patch commands stay useful without executing repository diff drivers.
    #[test]
    #[cfg(unix)]
    fn scoped_git_patch_commands_disable_diff_drivers() {
        struct Repo(PathBuf);
        impl Drop for Repo {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let repo =
            Repo(std::env::temp_dir().join(format!("formless-git-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&repo.0).unwrap();
        let root = repo.0.canonicalize().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                ])
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("file.txt"), "before\n").unwrap();
        std::fs::write(root.join(".gitattributes"), "file.txt diff=probe\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "before"]);
        std::fs::write(root.join("file.txt"), "after\n").unwrap();
        git(&["commit", "-qam", "after"]);
        std::fs::write(root.join("file.txt"), "working\n").unwrap();
        std::fs::write(
            root.join("driver.sh"),
            "#!/bin/sh\nprintf ran > driver-ran\ncat \"$1\"\n",
        )
        .unwrap();
        git(&["config", "diff.probe.textconv", "sh ./driver.sh"]);
        // Prove the fixture would execute a configured textconv driver.
        git(&["diff", "--textconv"]);
        assert!(root.join("driver-ran").exists());
        std::fs::remove_file(root.join("driver-ran")).unwrap();
        // An external driver must also stay disabled; executing this would fail.
        git(&["config", "diff.external", "false"]);
        for args in [
            json!(["diff", "--ext-diff", "--textconv"]),
            json!(["diff", "--ext-diff", "--textconv", "--", "file.txt"]),
            json!(["log", "-1", "-p", "--ext-diff", "--textconv"]),
            json!(["show", "--ext-diff", "--textconv"]),
        ] {
            let mut req = live(&format!(
                "function run() {{ return process.run('git', {}, {{cwd: {}}}); }}",
                args,
                json!(root.to_str().unwrap())
            ));
            req.caps = Capabilities::parse(&[
                "process:git:diff|log|show".into(),
                format!("fs:read:{}", root.display()),
            ])
            .unwrap();
            let out = execute(&req, Arc::new(NoHostOps)).unwrap().output;
            assert_eq!(out["code"], json!(0), "{args}: {out}");
            assert!(
                out["stdout"].as_str().unwrap().contains("diff --git"),
                "{args}: {out}"
            );
            assert!(!root.join("driver-ran").exists(), "{args} ran textconv");
        }
    }

    /// A call's own env can't steer what runs without its env grant (#1098).
    #[test]
    fn execution_steering_env_needs_its_grant() {
        for (grant, code) in [
            (
                "process:git:log",
                "process.run('git', ['log'], {env: {GIT_SSH_COMMAND: 'touch /tmp/x'}})",
            ),
            ("process:ls", "process.run('ls', [], {env: {PATH: '/tmp'}})"),
            (
                "process:ls",
                "process.run('ls', [], {env: {LD_PRELOAD: '/tmp/x.so'}})",
            ),
            (
                "process:ls",
                "process.run('ls', [], {env: {NODE_OPTIONS: '--require x'}})",
            ),
        ] {
            let mut req = live(&format!("function run() {{ return {code}; }}"));
            req.caps = Capabilities::parse(&[grant.to_string()]).unwrap();
            let e = execute(&req, Arc::new(NoHostOps)).unwrap_err();
            assert!(
                e.message.contains("changes what the program loads or runs"),
                "{}",
                e.message
            );
        }
        assert!(steers_execution("dyld_insert_libraries") && !steers_execution("LANG"));
        #[cfg(unix)]
        {
            let mut req = live(
                "function run() { return process.run('ls', [], {env: {PATH: '/bin:/usr/bin', LANG: 'C'}}).code; }",
            );
            req.caps =
                Capabilities::parse(&["process:ls".to_string(), "env:PATH".to_string()]).unwrap();
            assert_eq!(execute(&req, Arc::new(NoHostOps)).unwrap().output, json!(0));
        }
    }

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
    fn explicit_sleep_and_random_arguments_reject_coercion_and_named_cap_excess() {
        for api in ["sleep", "randomBytes"] {
            let req = live(&format!(
                r#"function run() {{
                const invalid=[-1,0.5,NaN,Infinity,-Infinity,Number.MAX_SAFE_INTEGER+1,undefined,null,true,'1',[],{{}},1n];
                return invalid.map(n=>{{try {{ {api}(n); return 'accepted'; }} catch(e) {{return e.message;}}}});
            }}"#
            ));
            let out = execute(&req, Arc::new(NoHostOps)).unwrap();
            for message in out.output.as_array().unwrap() {
                assert!(
                    message
                        .as_str()
                        .unwrap()
                        .contains("finite non-negative safe-integer number"),
                    "{api}: {message}"
                );
            }
            let setting = if api == "sleep" {
                "tools.max_sleep_secs"
            } else {
                "tools.max_random_bytes"
            };
            let cap = if api == "sleep" {
                MAX_SLEEP_MS.load(Ordering::Relaxed)
            } else {
                MAX_RANDOM_BYTES.load(Ordering::Relaxed)
            };
            for n in [
                cap + 1,
                3_000_000_000_u64,
                1_u64 << 32,
                9_007_199_254_740_991,
            ] {
                let error = execute(
                    &live(&format!("function run() {{ return {api}({n}); }}")),
                    Arc::new(NoHostOps),
                )
                .unwrap_err();
                assert!(error.message.contains(setting), "{}", error.message);
                assert!(error.message.contains(&n.to_string()), "{}", error.message);
            }
        }
    }

    #[test]
    fn direct_host_payloads_cannot_bypass_count_validation_or_caps() {
        for (op, key, setting) in [
            ("sleep", "ms", "tools.max_sleep_secs"),
            ("random_bytes", "n", "tools.max_random_bytes"),
        ] {
            for raw in [
                "{}".to_string(),
                "null".to_string(),
                format!(r#"{{"{key}":null}}"#),
                format!(r#"{{"{key}":"1"}}"#),
                format!(r#"{{"{key}":-1}}"#),
                format!(r#"{{"{key}":0.25}}"#),
                format!(r#"{{"{key}":9007199254740992}}"#),
                format!(r#"{{"{key}":18446744073709551615}}"#),
            ] {
                let req = live(&format!(
                    "function run() {{ return {{reply:JSON.parse(__host('{}',{}))}}; }}",
                    op,
                    serde_json::to_string(&raw).unwrap()
                ));
                let out = execute(&req, Arc::new(NoHostOps)).unwrap();
                assert_eq!(out.output["reply"]["ok"], false, "{op} {raw}");
                assert!(out.output["reply"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("finite non-negative safe-integer number"));
            }
            let raw = format!(r#"{{"{key}":4294967296}}"#);
            let out = execute(
                &live(&format!(
                    "function run() {{ return {{reply:JSON.parse(__host('{}',{}))}}; }}",
                    op,
                    serde_json::to_string(&raw).unwrap()
                )),
                Arc::new(NoHostOps),
            )
            .unwrap();
            assert_eq!(out.output["reply"]["ok"], false);
            assert!(out.output["reply"]["error"]
                .as_str()
                .unwrap()
                .contains(setting));
        }
        assert_eq!(
            bounded_count(
                &json!({"n":0}),
                "n",
                "randomBytes",
                0,
                "tools.max_random_bytes"
            )
            .unwrap(),
            0
        );
        assert_eq!(
            bounded_count(
                &json!({"n":1.0}),
                "n",
                "randomBytes",
                1,
                "tools.max_random_bytes"
            )
            .unwrap(),
            1
        );
        assert!(bounded_count(
            &json!({"n":1}),
            "n",
            "randomBytes",
            0,
            "tools.max_random_bytes"
        )
        .unwrap_err()
        .contains("tools.max_random_bytes"));
        assert!(random_base64(u64::MAX)
            .unwrap_err()
            .contains("supported allocation bounds"));
    }

    #[test]
    fn valid_random_counts_have_exact_decoded_lengths_and_short_sleeps_still_return_null() {
        for n in [0, 1, 7, 256, 1 << 20] {
            let req = live(&format!("function run() {{ return randomBytes({n}); }}"));
            let out = execute(&req, Arc::new(NoHostOps)).unwrap();
            assert_eq!(
                base64::engine::general_purpose::STANDARD
                    .decode(out.output.as_str().unwrap())
                    .unwrap()
                    .len(),
                n
            );
        }
        let out = execute(
            &live("function run() {return [sleep(0),sleep(1),sleep(2)];}"),
            Arc::new(NoHostOps),
        )
        .unwrap();
        assert_eq!(out.output, json!([null, null, null]));
    }

    #[test]
    fn valid_sleep_keeps_the_existing_interrupted_error_after_host_entry() {
        let mut req = live("function run() {sleep(250); return 'finished';}");
        let activity = Arc::new(AtomicU64::new(0));
        req.activity = Some(activity.clone());
        let cancel = req.cancel.clone();
        let worker = std::thread::spawn(move || execute(&req, Arc::new(NoHostOps)));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while activity.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        cancel.store(true, Ordering::SeqCst);
        let error = worker.join().unwrap().unwrap_err();
        assert!(activity.load(Ordering::SeqCst) > 0);
        assert!(error.message.contains("interrupted"), "{}", error.message);
    }

    #[test]
    fn random_bytes_vary_in_every_bit_position() {
        // UUIDv4 bytes fix 6 bits per 16-byte block; a CSPRNG fixes none.
        let (mut seen_one, mut seen_zero) = ([0u8; 16], [0u8; 16]);
        for block in random_bytes(16 * 256).unwrap().chunks(16) {
            for (i, b) in block.iter().enumerate() {
                seen_one[i] |= b;
                seen_zero[i] |= !b;
            }
        }
        assert_eq!(seen_one, [0xff; 16]);
        assert_eq!(seen_zero, [0xff; 16]);
        assert_eq!(random_bytes(5).unwrap().len(), 5);
        assert!(random_bytes(0).unwrap().is_empty());
    }

    /// Nested Tools that each take 300 ms; "bad" fails.
    struct SlowOps;
    impl HostOps for SlowOps {
        fn call_tool(&self, name: &str, input: Value) -> Result<Value, String> {
            // A failing call must settle before the successful slow call in
            // the unawaited-rejection fixture, rather than racing two 300ms waits.
            if name == "bad" {
                return Err("bad tool".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
            Ok(json!({"name": name, "n": input["n"]}))
        }
    }

    fn live(code: &str) -> ExecRequest {
        ExecRequest {
            output_schema: None,
            code: code.into(),
            input: json!({}),
            caps: Capabilities::default(),
            memory_mb: 64,
            mode: HostMode::Live,
            cancel: Arc::new(AtomicBool::new(false)),
            overlay: None,
            activity: None,
            modules: Arc::new(Vec::new()),
        }
    }

    #[test]
    fn generated_heap_diagnostic_reports_only_the_configured_effective_cap() {
        for (memory_mb, expected_bytes) in [(32, 33_554_432), (96, 100_663_296)] {
            // Exercise the existing null-exception branch without exhausting a
            // heap. This does not assert that a null throw proves genuine OOM.
            let mut request = live("function run() { console.log('fixture log'); throw null; }");
            request.memory_mb = memory_mb;
            let error = execute(&request, Arc::new(NoHostOps)).unwrap_err();
            assert_eq!(
                error.message,
                format!(
                    "out of memory (heap limit reached) (configured limit: {} bytes)",
                    expected_bytes
                )
            );
            assert!(matches!(error.kind, ExecErrorKind::Resource));
            assert_eq!(error.logs, vec!["fixture log"]);
            let mut ordinary =
                live("function run() { throw new Error('out of memory from the Tool'); }");
            ordinary.memory_mb = memory_mb;
            let error = execute(&ordinary, Arc::new(NoHostOps)).unwrap_err();
            assert!(error.message.starts_with("out of memory from the Tool"));
            assert!(!error.message.contains("configured limit"));
            assert_eq!(
                execute(&live("function run() { return 7; }"), Arc::new(NoHostOps))
                    .unwrap()
                    .output,
                json!(7)
            );
        }
        for memory_mb in [0, 1, 32, u32::MAX] {
            let mathematical_bytes = u128::from(memory_mb) * 1_048_576;
            assert_eq!(
                effective_heap_limit_bytes(memory_mb),
                mathematical_bytes.min(usize::MAX as u128) as usize
            );
        }
        let unrelated = classify("resource limit: read cap reached", false);
        assert!(matches!(unrelated.kind, ExecErrorKind::Resource));
        assert_eq!(unrelated.message, "resource limit: read cap reached");
        let cancelled = classify("out of memory (heap limit reached)", true);
        assert!(matches!(cancelled.kind, ExecErrorKind::Cancelled));
        assert_eq!(cancelled.message, "interrupted");
    }

    /// A real callee serializes a value constructed with JSON.parse, so
    /// __proto__ is already an own data key before nested settlement begins.
    struct JsonCalleeOps;
    impl HostOps for JsonCalleeOps {
        fn call_tool(&self, _: &str, input: Value) -> Result<Value, String> {
            let mut req = live("function run(input) { return JSON.parse(input.json); }");
            req.input = input;
            execute(&req, Arc::new(NoHostOps))
                .map(|out| out.output)
                .map_err(|e| e.message)
        }
    }

    #[test]
    fn nested_json_settlement_retains_own_proto_data_after_callee_and_caller_serialization() {
        let mut req = live(
            r#"async function run(input) {
            const v=await tools.call('json_callee',input);
            const again=JSON.parse(JSON.stringify(v));
            return {value:v,own:Object.prototype.hasOwnProperty.call(v,'__proto__'),
                plain:Object.getPrototypeOf(v)===Object.prototype,
                inheritedX:'x' in v,againOwn:Object.prototype.hasOwnProperty.call(again,'__proto__'),again};
        }"#,
        );
        req.input = json!({"json":r#"{"__proto__":{"x":1},"y":2}"#});
        let out = execute(&req, Arc::new(JsonCalleeOps)).unwrap();
        let expected: Value = serde_json::from_str(r#"{"__proto__":{"x":1},"y":2}"#).unwrap();
        assert_eq!(
            out.output,
            json!({"value":expected,"own":true,"plain":true,"inheritedX":false,"againOwn":true,"again":expected})
        );
    }

    #[test]
    fn nested_json_settlement_preserves_arrays_primitives_and_all_proto_value_types() {
        for raw in [
            r#"{"__proto__":null,"y":2}"#,
            r#"{"__proto__":7,"y":2}"#,
            r#"{"__proto__":"日本語","y":2}"#,
            r#"{"__proto__":false,"y":2}"#,
            r#"[{"__proto__":{"x":1}},{"deep":{"__proto__":null}},[true,42.25,"ไทย"]]"#,
            r#"{"nested":{"__proto__":{"__proto__":{"x":1}}},"y":2}"#,
            "null",
            "true",
            "false",
            "42.25",
            r#""ไทย\n__settleCall(1, {})""#,
        ] {
            let mut req = live(
                r#"async function run(input) {
                const value=await tools.call('json_callee',input);
                function normal(v) {
                    if(v===null || typeof v!=='object') return true;
                    if(Object.getPrototypeOf(v)!==(Array.isArray(v)?Array.prototype:Object.prototype)) return false;
                    return Object.keys(v).every(k=>normal(v[k]));
                }
                return {value,serialized:JSON.stringify(value),normal:normal(value)};
            }"#,
            );
            req.input = json!({"json":raw});
            let out = execute(&req, Arc::new(JsonCalleeOps)).unwrap();
            let expected: Value = serde_json::from_str(raw).unwrap();
            assert_eq!(out.output["value"], expected, "{raw}");
            assert_eq!(
                serde_json::from_str::<Value>(out.output["serialized"].as_str().unwrap()).unwrap(),
                expected,
                "{raw}"
            );
            assert_eq!(out.output["normal"], true, "{raw}");
        }
    }

    #[test]
    fn nested_settlement_lookup_and_call_failures_surface_as_exec_errors() {
        for setup in [
            "globalThis.__settleCall=null;",
            "globalThis.__settleCall=()=>{throw new Error('settlement failure');};",
            "Object.defineProperty(globalThis,'__settleCall',{get(){throw new Error('settlement lookup failure');}});",
        ] {
            let req=live(&format!("async function run() {{ {setup} return await tools.call('json_callee',{{json:'7'}}); }}"));
            let error=execute(&req,Arc::new(JsonCalleeOps)).unwrap_err();
            assert!(!error.message.contains("never settled"),"{}",error.message);
        }
    }

    #[test]
    fn nested_calls_run_in_parallel() {
        let req = live(
            "async function run() {
               const xs = await Promise.all([1, 2, 3, 4].map(n => tools.call('t', {n})));
               return xs.map(x => x.n);
             }",
        );
        let started = std::time::Instant::now();
        let out = execute(&req, Arc::new(SlowOps)).unwrap();
        assert_eq!(out.output, json!([1, 2, 3, 4]));
        // Four 300 ms calls one after another would take 1.2 s.
        assert!(started.elapsed() < std::time::Duration::from_millis(900));
    }

    #[test]
    fn a_failed_nested_call_rejects_with_its_error() {
        let req = live(
            "async function run() {
               try { await tools.call('bad', {}); return 'no'; }
               catch (e) { return e.message; }
             }",
        );
        let out = execute(&req, Arc::new(SlowOps)).unwrap();
        assert_eq!(out.output, json!("bad tool"));
    }

    #[test]
    fn propagated_nested_errors_keep_callee_detail_and_original_caller_stack() {
        for body in [
            "await tools.call('bad', {});",
            "try { await tools.call('bad', {}); } catch (e) { throw e; }",
        ] {
            let req = live(&format!("async function run() {{\n  {body}\n}}"));
            let error = execute(&req, Arc::new(SlowOps)).unwrap_err();
            let nested = error
                .nested
                .as_ref()
                .expect("nested rejection context was lost");
            assert_eq!(nested.tool, "bad");
            assert_eq!(nested.detail, "bad tool");
            assert!(
                error.message.contains("run (formless-tool.js:2:"),
                "{}",
                error.message
            );
        }
        let req = live("async function run() { try { await tools.call('bad', {}); } catch (_) {} throw new Error('different failure'); }");
        let error = execute(&req, Arc::new(SlowOps)).unwrap_err();
        assert!(
            error.nested.is_none(),
            "caught failure leaked into unrelated error"
        );
        assert!(error.message.starts_with("different failure"));
        let req = live("async function run() {\n tools.call('bad', {}); await tools.call('t', {}); return {};\n}");
        let error = execute(&req, Arc::new(SlowOps)).unwrap_err();
        assert_eq!(error.nested.as_ref().unwrap().tool, "bad");
        assert!(
            error.message.contains("run (formless-tool.js:2:"),
            "{}",
            error.message
        );
    }

    #[test]
    fn nested_calls_are_stubbed_in_smoke_tests() {
        let mut req = live("async function run() { return await tools.call('t', {}); }");
        req.mode = HostMode::Smoke;
        let e = execute(&req, Arc::new(SlowOps)).unwrap_err();
        assert!(e.after_stub, "{}", e.message);
    }

    #[test]
    fn a_tool_that_does_not_await_its_calls_fails() {
        let req = live("function run() { const r = tools.call('t', {n: 1}); return {n: r.n}; }");
        let e = execute(&req, Arc::new(SlowOps)).unwrap_err();
        assert!(e.message.contains("await every call"), "{}", e.message);
        // Returning the Promise itself is awaited by the runner.
        let req = live("function run() { return tools.call('t', {n: 1}); }");
        let out = execute(&req, Arc::new(SlowOps)).unwrap();
        assert_eq!(out.output["n"], 1);
    }

    #[test]
    fn console_preserves_primitive_nonfinite_diagnostics_without_changing_return_json() {
        let req = live(
            r#"function run() {
          const returns=[];
          for(const method of ['log','info','warn','error']) {
            returns.push(console[method](NaN,Infinity,-Infinity,null,1.5,-0,'ไทย 🦀'));
            returns.push(console[method]({score:NaN,positive:Infinity,negative:-Infinity,actual:null,finite:1.5,zero:-0},[NaN,Infinity,-Infinity,null,1.5,-0]));
          }
          let calls=0;
          console.log({toJSON(){calls++;return {custom:1.5};}});
          console.log(new Number(Infinity));
          return {calls,values:[NaN,Infinity,-Infinity,null,1.5,-0],returns};
        }"#,
        );
        let out = execute(&req, Arc::new(NoHostOps)).unwrap();
        assert_eq!(out.logs.len(), 10);
        for pair in out.logs[..8].chunks_exact(2) {
            assert_eq!(pair[0], "NaN Infinity -Infinity null 1.5 0 ไทย 🦀");
            assert_eq!(
                pair[1],
                r#"{"score":"NaN","positive":"Infinity","negative":"-Infinity","actual":null,"finite":1.5,"zero":0} ["NaN","Infinity","-Infinity",null,1.5,0]"#
            );
        }
        assert_eq!(out.logs[8], r#"{"custom":1.5}"#);
        assert_eq!(out.logs[9], "null");
        assert_eq!(out.output["calls"], 1);
        assert_eq!(
            out.output["values"],
            json!([null, null, null, null, 1.5, 0])
        );
        assert_eq!(
            out.output["returns"],
            json!([null, null, null, null, null, null, null, null])
        );
    }

    #[test]
    fn fs_list_open_keeps_the_first_error_without_consuming_a_later_success() {
        use std::{cell::Cell, collections::VecDeque, io};
        let path = Path::new("synthetic-listing-path");
        let mut results = VecDeque::from([
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "original-open-failure",
            )),
            Ok(vec!["later-success"]),
        ]);
        let calls = Cell::new(0);
        let error = listing_directory(path, false, |_| {
            calls.set(calls.get() + 1);
            results.pop_front().unwrap()
        })
        .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert_eq!(error, "synthetic-listing-path: original-open-failure");
        assert_eq!(
            results.len(),
            1,
            "Err-then-Ok cannot become an empty success through a second open"
        );
        for mode in [HostMode::Smoke, HostMode::Replay, HostMode::DryRun] {
            let result = listing_directory::<()>(path, mode.is_overlay(), |_| {
                Err(io::Error::new(io::ErrorKind::NotFound, "fixture"))
            })
            .unwrap();
            assert!(
                result.is_none(),
                "overlay disk-open failure still falls through"
            );
        }
        assert_eq!(
            listing_directory(path, false, |_| Ok(vec!["a", "b"])).unwrap(),
            Some(vec!["a", "b"])
        );
    }

    #[test]
    fn fs_list_names_details_and_overlays_keep_sorted_merge_and_tombstone_results() {
        struct PrivateDir(PathBuf);
        impl Drop for PrivateDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = PrivateDir(
            std::env::temp_dir().join(format!("fl-list-fixture-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&fixture.0).unwrap();
        let root = fixture.0.canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "aa").unwrap();
        std::fs::write(root.join("b.txt"), "bbb").unwrap();
        std::fs::create_dir(root.join("目录")).unwrap();
        let request = |code: &str, mode| {
            let mut req = live(code);
            req.mode = mode;
            req.input = json!({"root":root});
            req.caps = Capabilities::parse(&[
                format!("fs:read:{}", root.display()),
                format!("fs:write:{}", root.display()),
            ])
            .unwrap();
            req
        };
        let out = execute(&request("function run(i) { return {names:fs.list(i.root), details:fs.list(i.root,{details:true})}; }", HostMode::Live), Arc::new(NoHostOps)).unwrap().output;
        assert_eq!(out["names"], json!(["a.txt", "b.txt", "目录"]));
        let details = out["details"].as_array().unwrap();
        assert_eq!(details[0], json!({"name":"a.txt","type":"file","size":2}));
        assert_eq!(details[1], json!({"name":"b.txt","type":"file","size":3}));
        assert_eq!(details[2]["name"], "目录");
        assert_eq!(details[2]["type"], "dir");
        for mode in [HostMode::Smoke, HostMode::Replay, HostMode::DryRun] {
            let out = execute(&request(r#"function run(i) {
              fs.write(i.root+'/b.txt','replacement'); fs.remove(i.root+'/a.txt'); fs.write(i.root+'/added.txt','new');
              return {names:fs.list(i.root),details:fs.list(i.root,{details:true})};
            }"#, mode), Arc::new(NoHostOps)).unwrap().output;
            assert_eq!(out["names"], json!(["added.txt", "b.txt", "目录"]));
            assert_eq!(
                out["details"][0],
                json!({"name":"added.txt","type":"file","size":3})
            );
            assert_eq!(
                out["details"][1],
                json!({"name":"b.txt","type":"file","size":11})
            );
            let out = execute(&request(r#"function run(i) {
              const path=i.root+'/missing'; const before=fs.list(path); fs.write(path+'/made','new');
              return {before,after:fs.list(path),details:fs.list(path,{details:true})};
            }"#, mode), Arc::new(NoHostOps)).unwrap().output;
            assert_eq!(
                out,
                json!({"before":[],"after":["made"],"details":[{"name":"made","type":"file","size":3}]})
            );
        }
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "aa");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "bbb");
        assert!(!root.join("added.txt").exists() && !root.join("missing").exists());
        let error = execute(
            &request(
                "function run(i) { return fs.list(i.root+'/missing'); }",
                HostMode::Live,
            ),
            Arc::new(NoHostOps),
        )
        .unwrap_err();
        assert!(
            error
                .message
                .contains(&root.join("missing").display().to_string()),
            "{}",
            error.message
        );
        let mut tombstone = request(
            "function run(i) { return fs.list(i.root); }",
            HostMode::Replay,
        );
        let overlay = new_overlay();
        overlay
            .lock()
            .unwrap()
            .insert(root.to_string_lossy().into_owned(), None);
        tombstone.overlay = Some(overlay);
        assert_eq!(
            execute(&tombstone, Arc::new(NoHostOps)).unwrap().output,
            json!([])
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            std::fs::write(
                root.join(std::ffi::OsString::from_vec(vec![b'b', 255])),
                "fixture",
            )
            .unwrap();
            for details in [false, true] {
                let mut req = request(
                    "function run(i) { return fs.list(i.root,{details:i.details}); }",
                    HostMode::Live,
                );
                req.input["details"] = json!(details);
                assert!(execute(&req, Arc::new(NoHostOps))
                    .unwrap_err()
                    .message
                    .contains("directory entry path is not UTF-8"));
            }
        }
    }

    #[test]
    fn console_formats_circular_bigint_error_and_undefined_values() {
        let req = live("function run() { const node = {}; node.parent = node; console.log(node); console.log('got', undefined, new Error('x'), 12n); return 7; }");
        let out = execute(&req, Arc::new(NoHostOps)).unwrap();
        assert_eq!(out.output, json!(7));
        assert_eq!(
            out.logs,
            vec![r#"{"parent":"[Circular]"}"#, "got undefined Error: x 12n"]
        );
    }

    #[test]
    fn return_serialization_failures_name_the_actual_problem() {
        for prefix in ["", "async "] {
            for body in [
                "return {id: 9007199254740993n};",
                "const x = {}; x.self = x; return x;",
                "return function () {};",
                "return Symbol('not JSON');",
                "return {toJSON() { return undefined; }};",
            ] {
                let req = live(&format!("{prefix}function run() {{ {body} }}"));
                let e = execute(&req, Arc::new(NoHostOps)).unwrap_err();
                assert!(
                    e.message.contains("return value is not JSON-serializable:"),
                    "{}",
                    e.message
                );
                assert!(!e.message.contains("never settled"), "{}", e.message);
            }
        }
    }

    #[test]
    fn return_serialization_calls_to_json_once_and_preserves_absent_returns() {
        for prefix in ["", "async "] {
            let req = live(&format!(
                "{prefix}function run() {{ let calls = 0; return {{toJSON() {{ if (++calls > 1) throw new Error('serialized twice'); return {{calls}}; }} }}; }}"
            ));
            assert_eq!(
                execute(&req, Arc::new(NoHostOps)).unwrap().output,
                json!({"calls": 1})
            );
            let req = live(&format!("{prefix}function run() {{ return undefined; }}"));
            assert_eq!(
                execute(&req, Arc::new(NoHostOps)).unwrap().output,
                json!(null)
            );
        }
    }

    #[test]
    fn unhandled_rejections_fail_the_invocation() {
        for (code, message) in [
            ("function run() { Promise.reject(new Error('floating')); return 1; }", "floating"),
            ("async function run() { tools.call('bad', {}); await tools.call('t', {}); return 1; }", "bad tool"),
            ("async function run() { setTimeout(async () => { throw new Error('flush failed'); }, 0); await tools.call('t', {}); return 1; }", "flush failed"),
        ] {
            let e = execute(&live(code), Arc::new(SlowOps)).unwrap_err();
            assert!(e.message.contains("unhandled Promise rejection:"), "{}", e.message);
            assert!(e.message.contains(message), "{}", e.message);
        }
    }

    #[test]
    fn rejections_handled_before_completion_do_not_fail() {
        let req = live("async function run() { const p = Promise.reject(new Error('later handled')); await tools.call('t', {}); p.catch(() => {}); return 7; }");
        let out = execute(&req, Arc::new(SlowOps)).unwrap();
        assert_eq!(out.output, json!(7));
    }

    /// The peer remains connected until the request's own deadline closes it.
    /// Both literal-IP (shared client) and hostname (pinned client) paths run.
    #[test]
    fn fetch_deadlines_bound_headers_bodies_and_trickling_peers() {
        for host in ["127.0.0.1", "localhost"] {
            for body in [false, true] {
                let (addr, accepted, server) = stalled_http_peer(body, false);
                let req = timeout_request(host, addr, 150);
                let client = prepared_timeout_client(&req, addr);
                let started = std::time::Instant::now();
                let result =
                    send_prepared_timeout_request(req, addr, &client).and_then(|mut response| {
                        let mut bytes = Vec::new();
                        response
                            .read_to_end(&mut bytes)
                            .map_err(|e| fetch_error(&e))?;
                        Ok(())
                    });
                accepted
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                let error = result.unwrap_err();
                assert!(
                    error.starts_with("fetch timed out:"),
                    "{host}, body={body}: {error}"
                );
                assert!(started.elapsed() < std::time::Duration::from_secs(2));
                server.join().unwrap();
            }
        }
        let (addr, accepted, server) = stalled_http_peer(true, true);
        let req = timeout_request("127.0.0.1", addr, 150);
        let client = prepared_timeout_client(&req, addr);
        let started = std::time::Instant::now();
        let mut response = send_prepared_timeout_request(req, addr, &client).unwrap();
        accepted
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let error = response.read_to_end(&mut Vec::new()).unwrap_err();
        assert!(
            fetch_error(&error).starts_with("fetch timed out:"),
            "{error}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        drop(response);
        server.join().unwrap();
        // Connection refusal remains recognizable instead of being labelled
        // as the same error as an endpoint that connects and then stalls.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let error = send_request(timeout_request("127.0.0.1", addr, 150))
            .err()
            .unwrap();
        assert!(error.starts_with("fetch connection failed:"), "{error}");
    }

    #[test]
    fn cancelled_fetch_worker_exits_under_its_native_deadline() {
        for body in [false, true] {
            let (addr, accepted, server) = stalled_http_peer(body, false);
            let req = timeout_request("127.0.0.1", addr, 200);
            let client = prepared_timeout_client(&req, addr);
            let cancel = Arc::new(AtomicBool::new(false));
            let flag = cancel.clone();
            let (completed_tx, completed_rx) = std::sync::mpsc::channel();
            let caller = std::thread::spawn(move || {
                cancellable_work(&flag, move || {
                    let result = send_prepared_timeout_request(req, addr, &client).and_then(
                        |mut response| {
                            response
                                .read_to_end(&mut Vec::new())
                                .map_err(|e| fetch_error(&e))?;
                            Ok(())
                        },
                    );
                    // A completion signal from inside the abandoned worker, not
                    // merely the caller returning or a sleep that hopes it stopped.
                    completed_tx.send(result.clone()).unwrap();
                    result
                })
            });
            accepted
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            cancel.store(true, Ordering::SeqCst);
            assert_eq!(caller.join().unwrap().unwrap_err(), "interrupted");
            let error = completed_rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .unwrap_err();
            assert!(error.starts_with("fetch timed out:"), "{error}");
            server.join().unwrap();
        }
    }

    fn prepared_timeout_client(
        req: &FetchRequest,
        addr: std::net::SocketAddr,
    ) -> reqwest::blocking::Client {
        client_builder(req.timeouts)
            .resolve_to_addrs("localhost", &[addr])
            .build()
            .unwrap()
    }

    fn send_prepared_timeout_request(
        req: FetchRequest,
        addr: std::net::SocketAddr,
        client: &reqwest::blocking::Client,
    ) -> Result<reqwest::blocking::Response, String> {
        send_request_with_dependencies(
            req,
            &|host, port, _deadline| {
                assert_eq!(host, "localhost");
                assert_eq!(port, addr.port());
                Ok(vec![addr])
            },
            &|url, pinned, _timeouts| {
                if url.host_str() == Some("localhost") {
                    assert_eq!(
                        pinned,
                        &[addr],
                        "hostname still follows checked-address pinning"
                    );
                } else {
                    assert!(pinned.is_empty(), "literal IP bypasses DNS");
                }
                Ok(client.clone())
            },
        )
    }

    fn timeout_request(host: &str, addr: std::net::SocketAddr, total_ms: u64) -> FetchRequest {
        FetchRequest {
            method: reqwest::Method::GET,
            url: reqwest::Url::parse(&format!("http://{host}:{}/", addr.port())).unwrap(),
            headers: Vec::new(),
            body: None,
            scope: vec![host.into()],
            timeouts: FetchTimeouts {
                connect_ms: 100,
                total_ms,
            },
        }
    }

    fn stalled_http_peer(
        body: bool,
        trickle: bool,
    ) -> (
        std::net::SocketAddr,
        std::sync::mpsc::Receiver<()>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "fetch never connected"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                assert_eq!(socket.read(&mut byte).unwrap(), 1);
                request.push(byte[0]);
                assert!(request.len() < 16384);
            }
            if body {
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n")
                    .unwrap();
            }
            tx.send(()).unwrap();
            if trickle {
                while std::time::Instant::now() < deadline {
                    if socket.write_all(b"x").is_err() {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
                panic!("trickling request did not close under its total deadline");
            } else {
                // Deadline expiry drops the client's connection, not just its
                // Invocation's reference to a still-blocked worker.
                let mut byte = [0];
                assert_eq!(
                    socket.read(&mut byte).unwrap(),
                    0,
                    "fetch socket never closed"
                );
            }
        });
        (addr, rx, server)
    }

    #[test]
    fn declared_error_fields_follow_only_applicable_output_schemas() {
        let output = json!({"kind":"diagnostic", "error":"negative finding"});
        let declaration = json!({"type":"object", "properties":{"error":{"type":"string"}}});
        for schema in [
            json!({"allOf":[declaration]}),
            json!({"$defs":{"diagnostic":declaration}, "$ref":"#/$defs/diagnostic"}),
            json!({"$defs":{"a/b~c":declaration}, "allOf":[{"$ref":"#/$defs/a~1b~0c"}]}),
            json!({"anyOf":[{"type":"integer"}, declaration]}),
            json!({"oneOf":[{"type":"integer"}, declaration]}),
            json!({"if":{"properties":{"kind":{"const":"diagnostic"}}}, "then":declaration}),
            json!({"if":{"properties":{"kind":{"const":"other"}}}, "else":declaration}),
            json!({"dependentSchemas":{"kind":declaration}}),
        ] {
            assert!(reported_error(&output, Some(&schema)).is_none(), "{schema}");
        }
        for schema in [
            json!({"properties":{"nested":declaration}}),
            json!({"$defs":{"unused":declaration}}),
            json!({"items":declaration}),
            json!({"not":declaration}),
            json!({"dependentSchemas":{"absent":declaration}}),
            json!({"anyOf":[{"allOf":[{"properties":{"kind":{"const":"other"}}},declaration]}, {"type":"object"}]}),
            json!({"oneOf":[{"allOf":[{"properties":{"kind":{"const":"other"}}},declaration]}, {"type":"object"}]}),
            json!({"if":{"properties":{"kind":{"const":"other"}}}, "then":declaration}),
            json!({"if":{"properties":{"kind":{"const":"diagnostic"}}}, "else":declaration}),
            json!({"$ref":"#"}),
            json!({"anyOf":[{"$ref":"#"}, {"type":"object"}]}),
            json!({"$defs":{"a":{"$ref":"#/$defs/b"},"b":{"$ref":"#/$defs/a"}},"$ref":"#/$defs/a"}),
            json!({"$ref":"https://schemas.example.invalid/output"}),
            json!({"$ref":"file:///tmp/output-schema.json"}),
        ] {
            assert!(reported_error(&output, Some(&schema)).is_some(), "{schema}");
        }
        // An unusable condition must not select an else branch by guessing.
        let cyclic = json!({"if":{"$ref":"#/if"}, "else":declaration});
        assert!(reported_error(&output, Some(&cyclic)).is_some());
        let mut deep = declaration;
        for _ in 0..65 {
            deep = json!({"allOf":[deep]});
        }
        assert!(reported_error(&output, Some(&deep)).is_some());
        let wide = json!({"allOf": vec![json!({}); 4096]});
        assert!(reported_error(&output, Some(&wide)).is_some());
    }

    #[test]
    fn reported_error_detects_truthy_error_fields() {
        assert!(reported_error(&json!({"error": "boom"}), None).is_some());
        assert!(reported_error(&json!({"error": {"code": 1}}), None).is_some());
        assert!(reported_error(&json!({"error": null, "v": 1}), None).is_none());
        assert!(reported_error(&json!({"error": ""}), None).is_none());
        assert!(reported_error(&json!({"error": false}), None).is_none());
        assert!(reported_error(&json!(["error"]), None).is_none());
        let schema = json!({"type":"object", "properties":{"error":{"type":"string"}}});
        assert!(reported_error(&json!({"error":"invalid input"}), Some(&schema)).is_none());
        let mut req = live("function run() { return {valid: false, error: 'invalid input'}; }");
        req.output_schema = Some(schema);
        assert_eq!(
            execute(&req, Arc::new(NoHostOps)).unwrap().output,
            json!({"valid":false,"error":"invalid input"})
        );
    }
}

#[cfg(test)]
mod sensitive_read_tests;

#[cfg(test)]
mod run_file_capture_tests;
