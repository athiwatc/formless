use anyhow::{anyhow, bail, Context, Result};
use formless_runtime::LockExt as _;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::config::ProviderCfg;
use crate::Paths;

/// The Tool authoring contract and host API (SPEC §7.5), shared by the
/// bootstrap prompt and the optimiser so they never drift apart.
macro_rules! host_api_doc {
    () => {
        "Tool code is plain JavaScript defining `function run(input) { ... }` (or `async function run`) returning the output value. On failure, throw an Error: a returned {error: ...} is counted as a failure unless output_schema declares an error property, and swallowing errors hides broken Tools from measurement. Host API (all Capability-gated; data options take encoding \"utf8\" (default) or \"base64\" for binary): fetch(url, {method, headers, body, bodyEncoding, encoding}) -> {status, ok, url, headers, body, text(), json()} (redirects followed only within the granted hosts); fetchChunks(url, opts, size, cb) streams the body in chunks and returns status; fs.read(path, {encoding}), fs.write(path, data, {encoding}), fs.append(path, data, {encoding}), fs.exists(path), fs.stat(path) -> {type, size, modified_ms} or null, fs.list(dir, {details}) -> names or [{name, type, size}], fs.mkdir(path), fs.remove(path, {recursive}), fs.rename(from, to), fs.readChunks(path, size, cb, {encoding}), fs.readLines(path, cb); env.get(key); process.run(cmd, argsArray, {cwd, env, stdin, stdinEncoding, encoding}) -> {code, signal, stdout, stderr} (no shell); tools.call(name, input) -> Promise of the Tool's output (always await it; several in Promise.all run in parallel, and a Tool that returns with a call still pending fails); clipboard.read(), clipboard.write(text); notify(title, body); harness.read() (providers, roles, MCP servers, skills, settings; never keys) and harness.call(op, args, reason) for mcp.add/remove, skills.add/remove/enable, models.set, providers.upsert/remove (no keys; upsert takes base_url or a preset: openai, anthropic, openrouter, gemini, groq, ollama, lmstudio), roles.upsert/delete, config.set: each change is shown to the user with your reason and needs their approval; store.get(key, {ns}), store.set(key, jsonValue, {ns}), store.delete(key, {ns}), store.list(prefix, {ns}) persist small state between Invocations (\"store\" = this Tool's own, \"store:<ns>\" = a shared namespace; 64 KB per value); llm.complete(prompt, {system, model, role}) -> text; agents.run(task, {system, model, role}) -> text (role: a configured Role by name, see the Roles list; it picks the provider and model); requireModule(name) / modules.load(name), modules.list(); emit(chunk) streams large output to an artifact; progress(fraction, label) shows how far long work has got (fraction 0..1 or null; no Capability needed); console.log, sleep(ms), setTimeout/setInterval/clearTimeout (real timers; run() may await them), cryptoRandom(), randomBytes(n) -> base64. Nothing else exists: anything more (URL parsing, base64 decoding to bytes, hashing, compression, formats) you write in JS."
    };
}
pub(crate) use host_api_doc;

/// Bootstrap system prompt (SPEC §7.7). Versioned; v2.1 adds generic-Tool
/// guidance and input-bound Capability scopes; v2.2 adds the find/read/test
/// built-ins, throw-on-failure and the extended host API; v2.3 adds
/// `run_script` orchestration (SPEC §7.12); v2.4 adds when to wrap MCP
/// tools in a Tool.
pub const SYSTEM_PROMPT: &str = concat!("You are Formless, a general-purpose agent.\n\
You start with only built-ins: `create_tool`, `create_module`, `retire_tool`, `find_tools`, `read_tool`, `test_tool` and `run_script`. Everything else you build yourself: small, minimally-capabled Tools created with create_tool, persisted and reused across tasks and projects. Never expect a Tool to exist - find it or build it.\n\
The registry can hold far more Tools than are listed to you as functions. Before creating a Tool, find_tools for it; matches become callable. To change a Tool, read_tool it first and create a new version (same name) from its real code. Test a draft with test_tool on real input before saving it, then call the saved Tool. Shared JS helpers (parsers, encoders, hashing, URL handling) belong in modules via create_module, loaded inside Tools with requireModule(name). To delegate a focused task to a sub-agent with the same Tools, build a Tool with the \"agent\" Capability, e.g. `function run(i){ return agents.run(i.task, {model: i.model}); }`; several such calls in one turn run in parallel.\n\
To call existing Tools many times, or chain them with loops, filters and branches, use run_script instead of one call per step: it runs a short script that calls only the Tools in its uses list, in parallel with Promise.all, and returns just the final value. Scripts only connect Tools: anything that reads or changes the world is a Tool, never script code. Save a script you would run twice as a Tool with save_as.\n\
You have no abilities beyond your Tools, but you can always build one: when a task needs files, the web, processes, the clipboard or anything else outside this conversation, find_tools and, if nothing fits, create_tool and do it. Never say you can't access something, and never ask the user to do a step a Tool could do.\n\
Make Tools generic: a Tool does one kind of thing (read_text_file, http_get_json, csv_column_sum), and everything task- or project-specific - paths, URLs, hosts, queries, names, column/field names - comes in through its input. Never hardcode such values in implementation_js, the Tool name, or its description. If an existing Tool nearly fits, create a more general new version of it (existing input fields keep working, new ones optional) rather than a new narrow Tool. Specialise only when the logic itself is inherently specific to one project. Give every Tool a short human title for the user (\"Last git author\"); the name stays the machine identifier.\n\
", host_api_doc!(), "\n\
Stream anything large (fs.readChunks / fetchChunks / fs.append / emit); never load it whole. Declare only the Capabilities a Tool needs, as strings like \"net\", \"net:example.com\", \"fs:read:/abs/path\", \"fs:write:/abs/path\", input-bound scopes \"fs:read:$input.path\", \"fs:write:$input.out\", \"net:$input.url\" (granted per call for exactly the path(s)/host named by that input field - string or array of strings - so a generic Tool stays least-privilege; prefer these over fixed project paths), \"env:KEY\", \"process:cmd\", \"clipboard:read\", \"clipboard:write\", \"notify\", \"store\", \"store:<ns>\", \"harness:read\", \"harness:<mcp|skills|models|providers|roles|config>\", \"llm\", \"agent\" (or \"llm:model\"/\"agent:model\"). Prefer retiring a broken Tool and creating a new version over silent workarounds.\n\
MCP tools (named mcp__<server>__<tool>) can be called directly: do that for one-off use. When the same MCP call or sequence of calls comes up again, or needs fixed arguments, trimmed output or several calls combined, create a Tool that makes them with tools.call and declares the \"mcp:<server>\" Capability: a Tool gets an editable description and schema, measured success and optimisation, which raw MCP tools never do. Never create a Tool that only forwards one MCP call unchanged.\n\
When the user states a lasting preference or fact about themselves or their setup (tools they use, language, where projects live), save it with remember; never remember secrets or one-off details, and forget a note when asked or when it stops being true.");

/// Extra guidance for model families that under-use Tools (#237): GPT
/// models tend to answer from memory or refuse instead of building one.
const OPENAI_ADDENDUM: &str = "Tool use rule: you have no built-in access to files, the web, processes or the clipboard, but you can build it. If a task needs anything outside this conversation, do not answer from memory, do not say you can't access it, and do not ask the user to run or paste anything: call find_tools, and if nothing fits, create_tool (test it with test_tool), then call it. Act first; ask the user only for decisions or facts no Tool could find.";

/// The system-prompt addendum for `model`'s family, if any. Chosen when a
/// Session's prompt is snapshotted, so it is stable for the Session.
pub fn model_family_addendum(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    let id = m.rsplit('/').next().unwrap_or(&m);
    let openai = ["gpt-", "gpt5", "chatgpt", "o1", "o3", "o4"]
        .iter()
        .any(|p| id.starts_with(p));
    openai.then_some(OPENAI_ADDENDUM)
}

/// Sent back once per step when a final answer refuses for lack of
/// access before any Tool was used.
pub const REFUSAL_NUDGE: &str = "You answered that you can't access or do something, or asked the user to do a step themselves, without trying a Tool. You can build that access: call find_tools for it and, if nothing fits, create_tool, test it, and call it. Only if that truly cannot work, explain why.";

/// Whether a final answer refuses for lack of access or hands a step back
/// to the user ("I can't access files", "please paste the output").
pub fn looks_like_access_refusal(text: &str) -> bool {
    const PATTERNS: &[&str] = &[
        "can't access",
        "cannot access",
        "don't have access",
        "do not have access",
        "unable to access",
        "not able to access",
        "can't browse",
        "cannot browse",
        "unable to browse",
        "don't have the ability",
        "do not have the ability",
        "don't have direct access",
        "can't read files",
        "can't run commands",
        "cannot run commands",
        "can't execute",
        "cannot execute",
        "no access to your",
        "please paste",
        "you can paste",
        "could you paste",
        "can you paste",
        "you'll need to run",
        "you will need to run",
        "you can run the following",
        "run the following command",
    ];
    // Curly apostrophes count too.
    let t = text.to_lowercase().replace('\u{2019}', "'");
    PATTERNS.iter().any(|p| t.contains(p))
}

/// What a compaction summary must keep, so later steps can carry on
/// without the turns it replaces (#619).
const SUMMARIZE_PROMPT: &str = "Summarise the following conversation excerpt \
faithfully and compactly; later steps will see only your summary, not the \
excerpt. Use these sections, leaving out any that would be empty:\n\
## Goal — what the user wants.\n\
## Decisions — what was decided or agreed, and why.\n\
## Changes — files, Tools and settings created or changed (exact names and paths).\n\
## Facts — identifiers, values, commands, errors and results worth keeping verbatim.\n\
## Open — what is unfinished, pending or was asked and not yet answered.\n\
Keep it under about 600 words. Output only the summary.";

fn url(base: &str, path: &str) -> String {
    format!("{}/{path}", base.trim_end_matches('/'))
}

fn apply_headers(
    mut req: reqwest::RequestBuilder,
    cfg: &ProviderCfg,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if cfg.anthropic() {
        req = req.header("anthropic-version", crate::anthropic::VERSION);
    }
    if !api_key.is_empty() {
        let header = cfg
            .api_key_header
            .as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .or(cfg.anthropic().then_some("x-api-key"));
        req = match header {
            Some(h) => req.header(h, api_key),
            None => req.bearer_auth(api_key),
        };
    }
    for (k, v) in &cfg.extra_headers {
        req = req.header(k, v);
    }
    if !cfg.query_params.is_empty() {
        req = req.query(&cfg.query_params);
    }
    req
}

/// The latest rate-limit headers per endpoint (SPEC §4.4), from any
/// response that carried them: `x-ratelimit-*` / `anthropic-ratelimit-*`.
static RATE_LIMITS: std::sync::Mutex<Option<HashMap<String, Value>>> = std::sync::Mutex::new(None);

/// What the endpoint's last response said about its rate limits.
pub fn rate_limits() -> HashMap<String, Value> {
    RATE_LIMITS.locked().clone().unwrap_or_default()
}

fn note_rate_limits(base_url: &str, headers: &reqwest::header::HeaderMap) {
    let mut found = serde_json::Map::new();
    for (k, v) in headers {
        let k = k.as_str().to_ascii_lowercase();
        let short = k
            .strip_prefix("x-ratelimit-")
            .or_else(|| k.strip_prefix("anthropic-ratelimit-"));
        if let (Some(short), Ok(v)) = (short, v.to_str()) {
            found.insert(short.to_string(), json!(v));
        }
    }
    if !found.is_empty() {
        found.insert("at".into(), json!(chrono::Utc::now().to_rfc3339()));
        RATE_LIMITS
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::new)
            .insert(base_url.to_string(), Value::Object(found));
    }
}

/// What `TestModel` found (SPEC §4.3).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ModelTest {
    pub model: String,
    pub ok: bool,
    /// Time to the first streamed token, and to the end of the answer.
    pub first_token_ms: Option<u64>,
    pub total_ms: Option<u64>,
    /// The answer arrived in more than one delta.
    pub streaming: bool,
    /// Reasoning came as its own stream.
    pub reasoning: bool,
    pub tool_calling: bool,
    /// Tool calls work only through the prompt-based fallback.
    pub prompt_fallback: bool,
    pub error: Option<String>,
    pub tested_at: String,
}

pub struct Provider {
    cfg: ProviderCfg,
    api_key: String,
    /// When set, credentials are re-resolved before each HTTP request so
    /// an OAuth token expiring during a long Run is refreshed in place.
    credential_paths: Option<Paths>,
    http: reqwest::Client,
    /// Sent as `prompt_cache_key` so the endpoint routes one Session's
    /// requests to the same cache (see `provider.cache_key`).
    cache_key: Option<String>,
    /// Told before each retry wait, so clients can show "rate limited ·
    /// retrying in 8s (2/5)" instead of a plain wait (SPEC §4.4).
    on_retry: Option<RetryHook>,
    /// Told when a model's circuit breaker opens or closes (#621).
    on_note: Option<NoteHook>,
    /// Told of every request's reported usage (model, usage), so the
    /// daemon's own calls are counted too (#806).
    on_usage: Option<UsageHook>,
    /// The configured provider's name (`default` or a `[providers.<name>]`
    /// key), for pricing usage from its own table (#810).
    name: String,
}

pub type NoteHook = Arc<dyn Fn(String) + Send + Sync>;
pub type UsageHook = Arc<dyn Fn(&str, &str, &Usage) + Send + Sync>;

/// Per-model circuit breakers (#621), shared by every Session in the
/// daemon: after `BREAKER_TRIP` consecutive retryable failures a model is
/// skipped for a cooldown (doubling, with jitter, up to 10 minutes), then
/// tried by one request (half-open).
#[derive(Default)]
struct Breaker {
    fails: u32,
    open_until: Option<std::time::Instant>,
    cooldown: Duration,
}

const BREAKER_TRIP: u32 = 3;

static BREAKERS: std::sync::Mutex<Option<HashMap<String, Breaker>>> = std::sync::Mutex::new(None);

fn with_breaker<T>(key: &str, f: impl FnOnce(&mut Breaker) -> T) -> T {
    let mut all = BREAKERS.lock().unwrap_or_else(|e| e.into_inner());
    f(all
        .get_or_insert_with(HashMap::new)
        .entry(key.to_string())
        .or_default())
}

/// Skipped for now: open and still cooling down.
fn breaker_open(key: &str) -> bool {
    with_breaker(key, |b| {
        b.open_until.is_some_and(|t| std::time::Instant::now() < t)
    })
}

/// A success closes it; true when it had been open (worth saying).
fn breaker_success(key: &str) -> bool {
    with_breaker(key, |b| {
        let was = b.open_until.is_some();
        *b = Breaker::default();
        was
    })
}

/// A retryable failure; Some(cooldown) when this one opened the breaker.
fn breaker_failure(key: &str) -> Option<Duration> {
    with_breaker(key, |b| {
        b.fails += 1;
        if b.fails < BREAKER_TRIP {
            return None;
        }
        let base = if b.cooldown.is_zero() {
            Duration::from_secs(60)
        } else {
            (b.cooldown * 2).min(Duration::from_secs(600))
        };
        b.cooldown = base;
        b.fails = 0;
        b.open_until =
            Some(std::time::Instant::now() + base.mul_f64(1.0 + rand::random::<f64>() * 0.25));
        Some(base)
    })
}

/// A provider request is about to be retried after `wait`.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryNotice {
    /// Short reason: "rate limited", "server error 503", "connection
    /// failed", "stream dropped".
    pub reason: String,
    pub wait: Duration,
    /// 1-based number of the retry about to be made.
    pub attempt: usize,
    pub max_attempts: usize,
}

pub type RetryHook = Arc<dyn Fn(RetryNotice) + Send + Sync>;

/// Optional request parameters an endpoint may not know: dropped (once)
/// when a 400 names them, so they never cost a working setup anything.
const OPTIONAL_PARAMS: [&str; 2] = ["stream_options", "prompt_cache_key"];

#[derive(Debug, Clone, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ChatResult {
    pub content: String,
    /// Model reasoning emitted alongside the answer (`reasoning_content`
    /// / `reasoning` deltas), empty when the endpoint emits none.
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    /// Anthropic `thinking` blocks with signatures, sent back on the next
    /// request of a tool-use turn (empty for other APIs).
    pub thinking_blocks: Vec<Value>,
    pub dropped_reasoning_effort: bool,
    /// Model that actually served the request (failover routing).
    pub model_used: String,
    /// Output stopped at the token limit (`finish_reason: "length"`): the
    /// answer or the tool-call arguments are cut off.
    pub truncated: bool,
    /// Token usage as reported by the provider (None when it sends none).
    pub usage: Option<Usage>,
    /// Why generation stopped, when the API says more than "done"
    /// (Anthropic `refusal`, `pause_turn`, `model_context_window_exceeded`).
    pub stop_reason: Option<String>,
    /// A refusal's category, when the API gives one.
    pub stop_detail: Option<String>,
    /// What was changed to get the request accepted, for the user.
    pub notes: Vec<String>,
    /// Replayed thinking was rejected and left out: the stored blocks
    /// should not be sent again.
    pub dropped_replayed_thinking: bool,
}

/// What [`Provider::post_provider`] changed to get a request accepted.
#[derive(Debug, Clone, Default)]
struct Recovered {
    dropped_effort: bool,
    notes: Vec<String>,
    dropped_replayed_thinking: bool,
}

/// Provider-reported token counts for one request.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Part of `completion_tokens` spent on reasoning.
    pub reasoning_tokens: u64,
    /// Part of `prompt_tokens` served from the provider's prompt cache.
    pub cached_tokens: u64,
    /// Part of `prompt_tokens` written to the prompt cache (billed above
    /// plain input on Anthropic models).
    pub cache_write_tokens: u64,
    /// USD the provider billed for the request (OpenRouter's
    /// `usage.cost`); preferred over `provider.prices` when present.
    pub cost: Option<f64>,
}

impl Usage {
    /// Chat Completions (`prompt_tokens`, …) or Responses (`input_tokens`,
    /// …) usage object.
    pub fn parse(u: &Value) -> Option<Self> {
        if !u.is_object() {
            return None;
        }
        let n = |v: &Value| v.as_u64().unwrap_or(0);
        // Anthropic reports cache reads and writes beside `input_tokens`;
        // everywhere else they are parts of the prompt. Normalised to
        // parts, so each is priced once (#804).
        let beside = u.get("prompt_tokens").is_none()
            && (u.get("cache_read_input_tokens").is_some()
                || u.get("cache_creation_input_tokens").is_some());
        let extra = if beside {
            n(&u["cache_read_input_tokens"]) + n(&u["cache_creation_input_tokens"])
        } else {
            0
        };
        Some(Self {
            prompt_tokens: n(&u["prompt_tokens"]).max(n(&u["input_tokens"])) + extra,
            completion_tokens: n(&u["completion_tokens"]).max(n(&u["output_tokens"])),
            reasoning_tokens: n(&u["completion_tokens_details"]["reasoning_tokens"])
                .max(n(&u["output_tokens_details"]["reasoning_tokens"])),
            cached_tokens: n(&u["prompt_tokens_details"]["cached_tokens"])
                .max(n(&u["input_tokens_details"]["cached_tokens"]))
                .max(n(&u["cache_read_input_tokens"])),
            cache_write_tokens: n(&u["prompt_tokens_details"]["cache_write_tokens"])
                .max(n(&u["input_tokens_details"]["cache_write_tokens"]))
                .max(n(&u["cache_creation_input_tokens"])),
            cost: u["cost"].as_f64().filter(|c| c.is_finite() && *c >= 0.0),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    Content,
    Reasoning,
    /// The stream dropped and the request is being re-sent: discard the
    /// partial content and reasoning delivered so far (text is empty).
    Restart,
    /// Bytes arrived from the provider (text is empty): liveness only, for
    /// stall detection; also covers tool-call streaming, which has no
    /// visible deltas.
    Bytes,
}

/// Convert Chat-Completions messages to Responses-API input items.
/// A mid-stream failure event (Responses `error` / `response.failed` /
/// `response.incomplete`, or a Chat Completions `{"error": ...}` chunk)
/// as (retryable, message). Server and rate-limit failures are
/// retryable; an incomplete response (token limit, content filter) is not.
pub fn responses_failure(v: &Value) -> (bool, String) {
    let kind = v["type"].as_str().unwrap_or("error");
    if kind == "response.incomplete" {
        let reason = v["response"]["incomplete_details"]["reason"]
            .as_str()
            .unwrap_or("unknown reason");
        return (false, format!("response incomplete: {reason}"));
    }
    let err = if v["response"]["error"].is_object() {
        &v["response"]["error"]
    } else if v["error"].is_object() {
        &v["error"]
    } else {
        v
    };
    let code = err["code"]
        .as_str()
        .or_else(|| err["type"].as_str())
        .unwrap_or("unknown");
    let message = err["message"].as_str().unwrap_or("no message");
    let retryable = [
        "server",
        "rate_limit",
        "overloaded",
        "unavailable",
        "timeout",
    ]
    .iter()
    .any(|k| code.contains(k));
    (retryable, format!("{kind} ({code}): {message}"))
}

pub fn responses_input(messages: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for m in messages {
        let role = m["role"].as_str().unwrap_or_default();
        if role == "tool" {
            out.push(json!({
                "type": "function_call_output",
                "call_id": m["tool_call_id"].as_str().unwrap_or_default(),
                "output": m["content"].as_str().unwrap_or_default(),
            }));
            continue;
        }
        if let Some(calls) = m["tool_calls"].as_array() {
            // A preamble written before the calls comes first, as in the turn.
            let text = m["content"].as_str().unwrap_or_default();
            if !text.is_empty() {
                out.push(json!({"role": "assistant", "content": [{"type": "output_text", "text": text}]}));
            }
            for c in calls {
                out.push(json!({
                    "type": "function_call",
                    "call_id": c["id"].as_str().unwrap_or_default(),
                    "name": c["function"]["name"].as_str().unwrap_or_default(),
                    "arguments": c["function"]["arguments"].as_str().unwrap_or_default(),
                }));
            }
            continue;
        }
        // Content parts (text and images from attachments) (#616).
        if let Some(parts) = m["content"].as_array() {
            let converted: Vec<Value> = parts
                .iter()
                .filter_map(|p| match p["type"].as_str() {
                    Some("text") => Some(json!({"type": if role == "assistant" { "output_text" } else { "input_text" }, "text": p["text"]})),
                    Some("image_url") => {
                        let mut img = json!({"type": "input_image", "image_url": p["image_url"]["url"]});
                        if let Some(d) = p["image_url"]["detail"].as_str() {
                            img["detail"] = json!(d);
                        }
                        Some(img)
                    }
                    _ => None,
                })
                .collect();
            if !converted.is_empty() {
                out.push(json!({"role": role, "content": converted}));
            }
            continue;
        }
        let text = m["content"].as_str().unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let ctype = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        out.push(json!({"role": role, "content": [{"type": ctype, "text": text}]}));
    }
    out
}

/// Mark prompt-cache breakpoints (Anthropic `cache_control`) on Chat
/// Completions messages: the system prompt, and the last user/assistant
/// text before the newest message, so each step re-reads the history the
/// previous one wrote. Messages become content parts only where marked.
pub fn with_cache_breakpoints(messages: &[Value]) -> Vec<Value> {
    let mark = |m: &mut Value| {
        if let Some(text) = m["content"].as_str().filter(|t| !t.is_empty()) {
            m["content"] = json!([{"type": "text", "text": text,
                "cache_control": {"type": "ephemeral"}}]);
            true
        } else {
            false
        }
    };
    let mut out = messages.to_vec();
    if let Some(first) = out.first_mut().filter(|m| m["role"] == "system") {
        mark(first);
    }
    let last = out.len().saturating_sub(1);
    for m in out[1.min(last)..last].iter_mut().rev() {
        let role = m["role"].as_str().unwrap_or_default();
        if (role == "user" || role == "assistant") && mark(m) {
            break;
        }
    }
    out
}

/// Mark or unmark a Chat Completions or Responses body's breakpoints for
/// the model it goes to. Unmarked text parts go back to plain strings.
fn apply_breakpoints(body: &mut Value, on: bool) {
    if let Some(messages) = body["messages"].as_array().cloned() {
        let plain: Vec<Value> = messages.into_iter().map(unmark).collect();
        body["messages"] = json!(if on {
            with_cache_breakpoints(&plain)
        } else {
            plain
        });
    } else if body["input"].is_array() {
        let mut input = body["input"].take();
        crate::anthropic::strip_cache_control(&mut input);
        if let (true, Some(parts)) = (on, input.as_array_mut()) {
            mark_responses_system(parts);
        }
        body["input"] = input;
    }
}

/// Body keys `provider.extra_body` may not replace.
const CORE_BODY_KEYS: [&str; 6] = ["model", "messages", "input", "tools", "stream", "system"];

/// Merge `provider.extra_body` into a request (objects one level deep, so
/// `options` adds to what is there), and give Ollama a context window
/// as large as the one Formless budgets for: its own default is small and
/// it silently drops the start of the prompt past it (#622).
fn apply_extra_body(body: &mut Value, cfg: &ProviderCfg, model: &str) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    for (k, v) in &cfg.extra_body {
        if CORE_BODY_KEYS.contains(&k.as_str()) {
            continue;
        }
        match (obj.get_mut(k), v) {
            (Some(Value::Object(have)), Value::Object(add)) => {
                for (ik, iv) in add {
                    have.insert(ik.clone(), iv.clone());
                }
            }
            _ => {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let ollama = crate::presets::for_url(&cfg.base_url).is_some_and(|p| p.id == "ollama");
    if ollama && !cfg.anthropic() && obj["options"].get("num_ctx").is_none() {
        let options = obj.entry("options").or_insert_with(|| json!({}));
        if let Some(o) = options.as_object_mut() {
            o.insert("num_ctx".into(), json!(cfg.context_for(model)));
        }
    }
}

/// A message whose content is one marked text part, as a plain string.
fn unmark(mut m: Value) -> Value {
    crate::anthropic::strip_cache_control(&mut m);
    let single = m["content"]
        .as_array()
        .filter(|parts| parts.len() == 1 && parts[0]["type"] == "text")
        .and_then(|parts| parts[0]["text"].as_str())
        .map(str::to_string);
    if let Some(text) = single {
        m["content"] = json!(text);
    }
    m
}

/// The Responses-API form of [`with_cache_breakpoints`]: the system
/// prompt's text part carries the breakpoint.
fn mark_responses_system(input: &mut [Value]) {
    if let Some(part) = input
        .first_mut()
        .filter(|m| m["role"] == "system")
        .and_then(|m| m["content"].get_mut(0))
    {
        part["cache_control"] = json!({"type": "ephemeral"});
    }
}

/// Convert Chat-Completions tool definitions to Responses-API tools.
pub fn responses_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let f = &t["function"];
            json!({
                "type": "function",
                "name": f["name"].as_str().unwrap_or_default(),
                "description": f["description"].as_str().unwrap_or_default(),
                "parameters": f["parameters"].clone(),
            })
        })
        .collect()
}

pub fn responses_text(v: &Value) -> String {
    if let Some(t) = v["output_text"].as_str() {
        return t.to_string();
    }
    let mut out = String::new();
    if let Some(items) = v["output"].as_array() {
        for item in items {
            if let Some(content) = item["content"].as_array() {
                for c in content {
                    // A refusal part is the answer too (#615).
                    if let Some(t) = c["text"].as_str().or_else(|| c["refusal"].as_str()) {
                        out.push_str(t);
                    }
                }
            }
        }
    }
    out
}

/// The `tool_calls` slot for a Responses stream event. `output_index`
/// counts every output item (reasoning, messages, calls), so it is mapped
/// to a slot rather than used as one: a reasoning item at index 0 must not
/// leave an empty call behind (#540). Events without an index get a new slot.
fn responses_slot(
    tool_calls: &mut Vec<ToolCall>,
    slots: &mut HashMap<u64, usize>,
    event: &Value,
) -> Result<usize> {
    let index = event["output_index"].as_u64();
    if let Some(&slot) = index.and_then(|i| slots.get(&i)) {
        return Ok(slot);
    }
    if tool_calls.len() >= MAX_TOOL_CALLS {
        bail!("provider sent more than {MAX_TOOL_CALLS} tool calls in one response");
    }
    tool_calls.push(ToolCall::default());
    let slot = tool_calls.len() - 1;
    if let Some(i) = index {
        slots.insert(i, slot);
    }
    Ok(slot)
}

fn responses_tool_calls(v: &Value) -> Vec<ToolCall> {
    let mut out = Vec::new();
    if let Some(items) = v["output"].as_array() {
        for item in items {
            if item["type"].as_str() == Some("function_call") {
                out.push(ToolCall {
                    id: item["call_id"]
                        .as_str()
                        .or_else(|| item["id"].as_str())
                        .unwrap_or_default()
                        .to_string(),
                    name: item["name"].as_str().unwrap_or_default().to_string(),
                    arguments: item["arguments"].as_str().unwrap_or_default().to_string(),
                });
            }
        }
    }
    out
}

/// Instruction appended for prompt-based tool calling (no native tools).
pub fn prompt_tool_instruction(tools: &[Value]) -> String {
    format!(
        "\n\nTool call format: this endpoint has no native tool-calling. When you need a tool, reply with ONLY a JSON object (no prose): {{\"tool_calls\": [{{\"name\": \"<tool>\", \"arguments\": {{}}}}]}} using one of these tools: {}. When you are done, reply with plain text only.",
        serde_json::to_string(tools).unwrap_or_default()
    )
}

/// Parse prompt-mode tool calls out of model text. Returns the remaining
/// plain text and any parsed calls (ids synthesised when absent).
pub fn parse_prompt_tool_calls(content: &str) -> (String, Vec<ToolCall>) {
    let Some(start) = content.find('{') else {
        return (content.to_string(), Vec::new());
    };
    let Some(end) = content.rfind('}') else {
        return (content.to_string(), Vec::new());
    };
    if end <= start {
        return (content.to_string(), Vec::new());
    }
    let Ok(v) = serde_json::from_str::<Value>(&content[start..=end]) else {
        return (content.to_string(), Vec::new());
    };
    let raw_calls: Vec<&Value> = if let Some(arr) = v["tool_calls"].as_array() {
        arr.iter().collect()
    } else if v.get("tool_call").is_some() {
        vec![&v["tool_call"]]
    } else if v["name"].is_string() {
        vec![&v]
    } else {
        return (content.to_string(), Vec::new());
    };
    let mut calls = Vec::new();
    for (i, c) in raw_calls.iter().enumerate() {
        let name = c["name"].as_str().unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let arguments = match &c["arguments"] {
            Value::String(s) => s.clone(),
            Value::Null => "{}".to_string(),
            other => other.to_string(),
        };
        calls.push(ToolCall {
            id: c["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{}", i + 1)),
            name: name.to_string(),
            arguments,
        });
    }
    if calls.is_empty() {
        return (content.to_string(), Vec::new());
    }
    let mut clean = content[..start].trim().to_string();
    let suffix = content[end + 1..].trim();
    if !suffix.is_empty() {
        if !clean.is_empty() {
            clean.push(' ');
        }
        clean.push_str(suffix);
    }
    (
        clean
            .replace("```json", "")
            .replace("```", "")
            .trim()
            .to_string(),
        calls,
    )
}

/// An HTTP client through `proxy` when set; otherwise reqwest's own
/// handling of `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` (SPEC §4.1).
pub fn http_client(proxy: &str) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder();
    if !proxy.trim().is_empty() {
        b = b.proxy(reqwest::Proxy::all(proxy.trim()).context("provider.proxy")?);
    }
    Ok(b.build()?)
}

/// The proxy requests go through: `provider.proxy`, else the environment.
pub fn proxy_in_use(cfg: &ProviderCfg) -> Option<String> {
    if !cfg.proxy.trim().is_empty() {
        return Some(cfg.proxy.trim().to_string());
    }
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
}

impl Provider {
    pub fn new(cfg: ProviderCfg, api_key: String) -> Result<Self> {
        let http = http_client(&cfg.proxy)?;
        Ok(Self {
            cfg,
            api_key,
            credential_paths: None,
            cache_key: None,
            on_retry: None,
            on_note: None,
            on_usage: None,
            name: crate::config::DEFAULT_PROVIDER.to_string(),
            http,
        })
    }

    /// This provider's settings.
    pub fn settings(&self) -> &ProviderCfg {
        &self.cfg
    }

    /// A provider with a fixed key on a shared HTTP client (named
    /// providers, SPEC §4.5).
    pub fn with_key(cfg: ProviderCfg, api_key: String, http: reqwest::Client) -> Self {
        Self {
            cfg,
            api_key,
            credential_paths: None,
            cache_key: None,
            on_retry: None,
            on_note: None,
            on_usage: None,
            name: crate::config::DEFAULT_PROVIDER.to_string(),
            http,
        }
    }

    /// Construct a provider whose credential is resolved from a profile
    /// before every request. The initial resolution also fails fast for a
    /// missing OAuth login rather than deferring that to the first POST.
    pub async fn for_profile(cfg: ProviderCfg, paths: Paths) -> Result<Self> {
        let http = http_client(&cfg.proxy)?;
        Self::for_profile_with(cfg, paths, http).await
    }

    /// [`Provider::for_profile`] on an existing HTTP client, so long-lived
    /// callers (the daemon) keep one connection pool and TLS sessions
    /// across Runs instead of a new handshake per call.
    pub async fn for_profile_with(
        cfg: ProviderCfg,
        paths: Paths,
        http: reqwest::Client,
    ) -> Result<Self> {
        let api_key = crate::auth::resolve_provider_token(&paths, &cfg).await?;
        Ok(Self {
            cfg,
            api_key,
            credential_paths: Some(paths),
            cache_key: None,
            on_retry: None,
            on_note: None,
            on_usage: None,
            name: crate::config::DEFAULT_PROVIDER.to_string(),
            http,
        })
    }

    async fn credential(&self) -> Result<String> {
        match &self.credential_paths {
            Some(paths) => crate::auth::resolve_provider_token(paths, &self.cfg).await,
            None => Ok(self.api_key.clone()),
        }
    }

    /// Route this provider's requests to a stable prompt cache (e.g. one
    /// per Session); off when `provider.cache_key = "off"`.
    pub fn with_cache_key(mut self, key: &str) -> Self {
        if !self.cfg.cache_key.eq_ignore_ascii_case("off") {
            self.cache_key = Some(key.to_string());
        }
        self
    }

    /// Report each retry wait to `hook` (see [`RetryNotice`]).
    pub fn with_retry_hook(mut self, hook: RetryHook) -> Self {
        self.on_retry = Some(hook);
        self
    }

    /// Report breaker changes (a model skipped, back again) to `hook`.
    pub fn with_note_hook(mut self, hook: NoteHook) -> Self {
        self.on_note = Some(hook);
        self
    }

    /// Report each request's usage to `hook` as (provider name, model,
    /// usage).
    pub fn with_usage_hook(mut self, hook: UsageHook) -> Self {
        self.on_usage = Some(hook);
        self
    }

    /// Name this provider as it is configured.
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// The configured provider's name (`default` or a named provider).
    pub fn name(&self) -> &str {
        &self.name
    }

    fn report_usage(&self, model: &str, usage: Option<&Usage>) {
        if let (Some(hook), Some(u)) = (&self.on_usage, usage) {
            hook(&self.name, model, u);
        }
    }

    fn note(&self, text: String) {
        tracing::info!("{text}");
        if let Some(h) = &self.on_note {
            h(text);
        }
    }

    /// Tell the hook, then wait before retry `attempt` (0-based).
    async fn retry_wait(&self, attempt: usize, server_wait: Option<Duration>, reason: String) {
        let wait = self.backoff(attempt, server_wait);
        if let Some(hook) = &self.on_retry {
            hook(RetryNotice {
                reason,
                wait,
                attempt: attempt + 1,
                max_attempts: self.cfg.max_retries,
            });
        }
        tokio::time::sleep(wait).await;
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        Ok(self.list_models_with_windows().await?.0)
    }

    /// The endpoint's models, and the context window of those whose
    /// listing reports one (OpenRouter `context_length`, vLLM
    /// `max_model_len`, …).
    /// A short prompt and a tool call on `model`: latency, streaming,
    /// reasoning and tool support (`TestModel`, the picker's Test).
    pub async fn test_model(&self, model: &str) -> ModelTest {
        let mut t = ModelTest {
            model: model.to_string(),
            tested_at: chrono::Utc::now().to_rfc3339(),
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let (mut first, mut deltas, mut reasoning) = (None, 0usize, false);
        let answer = self
            .chat_stream(
                &[json!({"role": "user", "content": "Reply with the single word: ok"})],
                &[],
                model,
                None,
                &CancellationToken::new(),
                |kind, _| match kind {
                    DeltaKind::Content => {
                        deltas += 1;
                        first.get_or_insert(started.elapsed().as_millis() as u64);
                    }
                    DeltaKind::Reasoning => {
                        reasoning = true;
                        first.get_or_insert(started.elapsed().as_millis() as u64);
                    }
                    _ => {}
                },
            )
            .await;
        if let Err(e) = answer {
            t.error = Some(format!("{e:#}"));
            return t;
        }
        t.first_token_ms = first;
        t.total_ms = Some(started.elapsed().as_millis() as u64);
        t.streaming = deltas > 1;
        t.reasoning = reasoning;
        let tool = json!({
            "type": "function",
            "function": {
                "name": "probe_ping",
                "description": "Connectivity probe. Call this.",
                "parameters": {"type": "object", "properties": {}}
            }
        });
        let native = match probe_tool_call(self, model, &tool, true).await {
            Err(e) if is_tool_choice_rejection(&e.to_string()) => {
                probe_tool_call(self, model, &tool, false).await
            }
            r => r,
        };
        match native {
            Ok(true) => t.tool_calling = true,
            other => {
                if probe_prompt_tool_call(self, model, &tool)
                    .await
                    .unwrap_or(false)
                {
                    t.tool_calling = true;
                    t.prompt_fallback = true;
                } else if let Err(e) = other {
                    t.error = Some(format!("tool call: {e:#}"));
                }
            }
        }
        t.ok = t.tool_calling && t.error.is_none();
        t
    }

    /// `/models` with each model's metadata (SPEC §4.2).
    pub async fn list_models_meta(
        &self,
    ) -> Result<(Vec<String>, HashMap<String, u32>, HashMap<String, Value>)> {
        let body = self.models_body().await?;
        Ok((
            parse_models(&body),
            parse_model_windows(&body),
            parse_model_meta(&body),
        ))
    }

    /// `GET /models`; on the Messages API every page of it
    /// (`has_more` / `after_id`), joined into one `data` list.
    async fn models_body(&self) -> Result<Value> {
        const MAX_PAGES: usize = 20;
        let credential = self.credential().await?;
        let mut all: Option<Value> = None;
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut req = self.http.get(url(&self.cfg.base_url, "models"));
            if self.cfg.anthropic() {
                req = req.query(&[("limit", "1000")]);
                if let Some(id) = &after {
                    req = req.query(&[("after_id", id.as_str())]);
                }
            }
            let resp = apply_headers(req, &self.cfg, &credential)
                .send()
                .await
                .context("GET /models")?;
            let status = resp.status();
            if !status.is_success() {
                return Err(anyhow!(
                    "GET /models failed: {status}: {}",
                    resp.text().await.unwrap_or_default()
                ));
            }
            let page: Value = resp.json().await.context("parse /models")?;
            let more = self.cfg.anthropic() && page["has_more"] == true;
            after = page["last_id"].as_str().map(str::to_string);
            match &mut all {
                None => all = Some(page),
                Some(a) => {
                    let rows = page["data"].as_array().cloned().unwrap_or_default();
                    if let Some(d) = a["data"].as_array_mut() {
                        d.extend(rows);
                    }
                }
            }
            if !more || after.is_none() {
                break;
            }
        }
        let body = all.unwrap_or_default();
        crate::anthropic::note_output_caps(
            body["data"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| Some((m["id"].as_str()?.to_string(), m["max_tokens"].as_u64()?))),
        );
        Ok(body)
    }

    /// Prices the endpoint's `/models` listing gives (OpenRouter's
    /// per-model `pricing`), for `formless prices import` (#809).
    pub async fn list_model_prices(&self) -> Result<HashMap<String, crate::config::ModelPrice>> {
        let body = self.models_body().await?;
        Ok(parse_model_prices(&body, &chrono::Utc::now().to_rfc3339()))
    }

    pub async fn list_models_with_windows(&self) -> Result<(Vec<String>, HashMap<String, u32>)> {
        let body = self.models_body().await?;
        Ok((parse_models(&body), parse_model_windows(&body)))
    }

    /// Non-streaming completion (compaction, probe helpers).
    pub async fn complete(
        &self,
        messages: &[Value],
        model: &str,
        effort: Option<&str>,
    ) -> Result<String> {
        Ok(self.complete_with_usage(messages, model, effort).await?.0)
    }

    /// Resolve attachment markers (`_attachments` on user messages, from
    /// `storage::provider_message`): images become `image_url` parts for
    /// a vision model on Chat Completions; otherwise only the reference
    /// line in the text remains. The marker never reaches the endpoint.
    pub fn prepare_attachments(&self, messages: &[Value], model: &str) -> Vec<Value> {
        // Prompt-mode tools ride on Chat Completions text; the native
        // Messages path keeps image blocks either way. The Responses API
        // takes images as `input_image` parts (#616).
        let vision = (!self.cfg.prompt_tools() || self.cfg.anthropic())
            && self.cfg.vision.unwrap_or_else(|| is_vision_model(model));
        messages
            .iter()
            .map(|m| {
                let Some(atts) = m.get("_attachments").and_then(Value::as_array) else {
                    return m.clone();
                };
                let mut m = m.clone();
                m.as_object_mut().map(|o| o.remove("_attachments"));
                if !vision {
                    return m;
                }
                let mut parts = vec![json!({"type": "text", "text": m["content"]})];
                for a in atts {
                    let (Some(path), Some(mime)) = (a["path"].as_str(), a["mime"].as_str()) else {
                        continue;
                    };
                    let name = a["name"].as_str().unwrap_or(path);
                    match encoded_image(path, mime, self.cfg.image_max_px) {
                        Ok(url) => {
                            let mut image = json!({"url": url});
                            if self.cfg.image_detail != "auto" && !self.cfg.image_detail.is_empty() {
                                image["detail"] = json!(self.cfg.image_detail);
                            }
                            parts.push(json!({"type": "image_url", "image_url": image}));
                        }
                        // Never silently: the model is told and so is the
                        // user (#616).
                        Err(why) => {
                            parts.push(json!({"type": "text", "text": format!("[image {name} could not be attached: {why}]")}));
                            self.note(format!("The image {name} wasn't sent to the model: {why}."));
                        }
                    }
                }
                m["content"] = json!(parts);
                m
            })
            .collect()
    }

    /// [`Provider::complete`] plus the reported token usage.
    pub async fn complete_with_usage(
        &self,
        messages: &[Value],
        model: &str,
        effort: Option<&str>,
    ) -> Result<(String, Option<Usage>)> {
        let (text, usage, _) = self.complete_full(messages, model, effort).await?;
        Ok((text, usage))
    }

    /// A one-shot completion, its usage, and whether it stopped at the
    /// output limit (`finish_reason: length`, an incomplete Responses
    /// reply, Anthropic `max_tokens`) (#619).
    pub async fn complete_full(
        &self,
        messages: &[Value],
        model: &str,
        effort: Option<&str>,
    ) -> Result<(String, Option<Usage>, bool)> {
        let out = self
            .complete_with_usage_inner(messages, model, effort)
            .await?;
        self.report_usage(model, out.1.as_ref());
        Ok(out)
    }

    async fn complete_with_usage_inner(
        &self,
        messages: &[Value],
        model: &str,
        effort: Option<&str>,
    ) -> Result<(String, Option<Usage>, bool)> {
        if self.cfg.anthropic() {
            let mut body = crate::anthropic::body(model, messages, &[], false, effort, false);
            let (resp, _, _) = self.post_provider(&mut body, None, true).await?;
            let v: Value = resp.json().await.context("parse Messages response")?;
            if let Some(why) = crate::anthropic::unusable_stop(&v) {
                return Err(anyhow!("provider error: {why}"));
            }
            let usage = Usage::parse(&v["usage"]);
            let cut = v["stop_reason"] == "max_tokens";
            return Ok((crate::anthropic::response_parts(&v).0, usage, cut));
        }
        let mut body = if self.cfg.responses() {
            json!({"model": model, "input": responses_input(messages), "stream": false})
        } else {
            json!({"model": model, "messages": messages, "stream": false})
        };
        let resp = self.post_chat(&mut body, effort).await?;
        let v: Value = resp.json().await.context("parse chat completion")?;
        let usage = Usage::parse(&v["usage"]);
        if self.cfg.responses() {
            let cut = v["status"] == "incomplete";
            return Ok((responses_text(&v), usage, cut));
        }
        let cut = v["choices"][0]["finish_reason"] == "length";
        Ok((
            v["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            usage,
            cut,
        ))
    }

    /// POST /chat/completions with bounded retries (SPEC §4.4). Returns the
    /// raw response on success. If a 400 looks like an unknown-parameter
    /// rejection and reasoning_effort was set, retries once without it and
    /// flags `dropped` via the returned marker in ChatResult paths.
    async fn post_chat(&self, body: &mut Value, effort: Option<&str>) -> Result<reqwest::Response> {
        self.post_provider(body, effort, true)
            .await
            .map(|(r, _, _)| r)
    }

    async fn post_provider(
        &self,
        body: &mut Value,
        effort: Option<&str>,
        allow_failover: bool,
    ) -> Result<(reqwest::Response, Recovered, String)> {
        let endpoint = if self.cfg.anthropic() {
            "messages"
        } else if self.cfg.responses() {
            "responses"
        } else {
            "chat/completions"
        };
        let requested = self
            .cfg
            .resolve_alias(body["model"].as_str().unwrap_or_default());
        let mut models = vec![requested.clone()];
        if allow_failover {
            for m in &self.cfg.fallback_models {
                let m = self.cfg.resolve_alias(m);
                if !m.is_empty() && !models.contains(&m) {
                    models.push(m);
                }
            }
        }
        let mut recovered = Recovered::default();
        let mut applied_fixes = Vec::new();
        let mut last_err = String::new();
        let last = models.len().saturating_sub(1);
        for (mi, model) in models.iter().enumerate() {
            // A model whose breaker is open is skipped while another is
            // left to try; the last one is always tried (#621).
            let key = format!("{} {model}", self.cfg.base_url);
            if mi < last && breaker_open(&key) {
                tracing::info!("skipping {model}: its circuit breaker is open");
                continue;
            }
            let mut with_effort = effort.is_some();
            let mut reauthed = false;
            // Retries count transient failures only; a resend after fixing a
            // rejected request (each fix happens at most once) is not one
            // (#1088), so `sends` just bounds the total.
            let (mut attempt, mut sends) = (0, 0);
            'attempts: while attempt <= self.cfg.max_retries && sends <= attempt + MAX_RESENDS {
                sends += 1;
                let mut b = body.clone();
                b["model"] = json!(model);
                // Breakpoints follow the model actually tried: a fallback
                // that isn't Claude gets plain content, one that is gets
                // them, whoever built the body (#769).
                if !self.cfg.anthropic() && body.get("__no_cache_control").is_none() {
                    apply_breakpoints(&mut b, self.cfg.cache_breakpoints(model));
                }
                if let Some(o) = b.as_object_mut() {
                    o.remove("__no_cache_control");
                }
                apply_extra_body(&mut b, &self.cfg, model);
                if let (true, Some(effort)) = (with_effort, effort) {
                    if self.cfg.responses() {
                        b["reasoning"] = json!({"effort": effort});
                    } else {
                        b["reasoning_effort"] = json!(effort);
                    }
                }
                let credential = self.credential().await?;
                let req = apply_headers(
                    self.http.post(url(&self.cfg.base_url, endpoint)),
                    &self.cfg,
                    &credential,
                )
                .json(&b);
                let mut server_wait = None;
                let reason;
                match req.send().await {
                    Ok(resp) => {
                        let status = resp.status();
                        note_rate_limits(&self.cfg.base_url, resp.headers());
                        if status.is_success() {
                            if breaker_success(&key) {
                                self.note(format!("{model} is answering again."));
                            }
                            return Ok((resp, recovered, model.clone()));
                        }
                        server_wait = retry_after(resp.headers());
                        let text = resp.text().await.unwrap_or_default();
                        // Save every rejected response before any recovery can
                        // continue, including the final send at the resend cap.
                        last_err = match server_wait {
                            Some(w) => format!("{status} (retry after {}s): {text}", w.as_secs()),
                            None => format!("{status}: {text}"),
                        };
                        let lower = text.to_lowercase();
                        let rejected = OPTIONAL_PARAMS
                            .iter()
                            .find(|p| body.get(**p).is_some() && lower.contains(**p));
                        if let (400, Some(param)) = (status.as_u16(), rejected) {
                            tracing::info!(
                                "provider rejected optional {param}; sending without it"
                            );
                            if let Some(o) = body.as_object_mut() {
                                o.remove(*param);
                            }
                            continue;
                        }
                        if status.as_u16() == 400 && with_effort && is_unknown_param(&text) {
                            with_effort = false;
                            recovered.dropped_effort = true;
                            continue;
                        }
                        // Nested parts a 400 can name (thinking, output
                        // cap, cache breakpoints, images): each fixed once,
                        // so a rejected history never bricks the Session.
                        if status.as_u16() == 400 {
                            let mut repaired = body.clone();
                            let fix = if self.cfg.anthropic() {
                                crate::anthropic::recover(&mut repaired, &text)
                            } else if lower.contains("cache_control")
                                && body.get("__no_cache_control").is_none()
                            {
                                // Not marked again on later attempts.
                                crate::anthropic::strip_cache_control(&mut repaired);
                                repaired["__no_cache_control"] = json!(true);
                                Some(crate::anthropic::Recovery::CacheControl)
                            } else {
                                None
                            };
                            if let Some(fix) = fix.filter(|f| !applied_fixes.contains(f)) {
                                *body = repaired;
                                applied_fixes.push(fix);
                                tracing::info!(
                                    "provider rejected the request ({fix:?}); resending"
                                );
                                recovered.notes.push(fix.note().to_string());
                                recovered.dropped_replayed_thinking |=
                                    fix == crate::anthropic::Recovery::ReplayedThinking;
                                continue;
                            }
                        }

                        // A rejected OAuth token (revoked, or clock skew past
                        // the expiry margin): refresh once and resend.
                        if status.as_u16() == 401 && self.cfg.oauth() && !reauthed {
                            if let Some(paths) = &self.credential_paths {
                                reauthed = true;
                                crate::auth::force_refresh_provider_token(
                                    paths,
                                    &self.cfg,
                                    &credential,
                                )
                                .await
                                .with_context(|| format!("provider error: {last_err}"))?;
                                continue;
                            }
                        }
                        if is_model_error(status.as_u16(), &text) && models.len() > 1 {
                            break; // fail over to the next model
                        }
                        if !(status.as_u16() == 429 || status.is_server_error()) {
                            return Err(anyhow!("provider error: {last_err}"));
                        }
                        reason = if status.as_u16() == 429 {
                            "rate limited".to_string()
                        } else {
                            format!("server error {}", status.as_u16())
                        };
                    }
                    Err(e) => {
                        last_err = e.to_string();
                        reason = "connection failed".to_string();
                    }
                }
                // Several failures in a row: stop paying retries on this
                // model and fail over, for every Session, for a while.
                if let Some(cool) = breaker_failure(&key) {
                    if mi < last {
                        self.note(format!(
                            "{model} failed {BREAKER_TRIP} times in a row ({reason}); skipping it for about {}s.",
                            cool.as_secs()
                        ));
                        break 'attempts;
                    }
                }
                if attempt < self.cfg.max_retries {
                    self.retry_wait(attempt, server_wait, reason).await;
                }
                attempt += 1;
            }
            // Retryable failures exhausted for this model: fail over.
            if !last_err.is_empty() && models.len() > 1 {
                tracing::warn!("provider model {model} failed ({last_err}); failing over");
            }
        }
        Err(anyhow!("provider request failed after retries: {last_err}"))
    }

    /// Wait before retry `attempt` (0-based): the server's own hint when it
    /// sent one (`Retry-After`, rate-limit reset headers), otherwise full
    /// jitter up to an exponential cap, so concurrent Sessions do not
    /// retry in lockstep.
    fn backoff(&self, attempt: usize, server_wait: Option<Duration>) -> Duration {
        let cap = Duration::from_secs(self.cfg.max_backoff_secs.max(1));
        if let Some(wait) = server_wait {
            // Honour the server even past our own cap, within reason.
            return wait.min(cap.max(Duration::from_secs(300)));
        }
        let ceiling = Duration::from_millis(500)
            .saturating_mul(2u32.saturating_pow(attempt as u32))
            .min(cap);
        ceiling.mul_f64(rand::random::<f64>())
    }

    /// Streaming chat with optional tools. Calls `on_delta(kind, text)`
    /// per content or reasoning chunk; honours cancel; assembles streamed
    /// tool_calls (SPEC §5/§7).
    pub async fn chat_stream<F: FnMut(DeltaKind, &str) + Send>(
        &self,
        messages: &[Value],
        tools: &[Value],
        model: &str,
        effort: Option<&str>,
        cancel: &CancellationToken,
        on_delta: F,
    ) -> Result<ChatResult> {
        let out = self
            .chat_stream_inner(messages, tools, model, effort, cancel, on_delta)
            .await?;
        let served = if out.model_used.is_empty() {
            model
        } else {
            &out.model_used
        };
        self.report_usage(served, out.usage.as_ref());
        Ok(out)
    }

    async fn chat_stream_inner<F: FnMut(DeltaKind, &str) + Send>(
        &self,
        messages: &[Value],
        tools: &[Value],
        model: &str,
        effort: Option<&str>,
        cancel: &CancellationToken,
        mut on_delta: F,
    ) -> Result<ChatResult> {
        let prepared = self.prepare_attachments(messages, model);
        if self.cfg.anthropic() {
            if !self.cfg.prompt_tools() || tools.is_empty() {
                return self
                    .chat_stream_anthropic(&prepared, tools, model, effort, cancel, on_delta)
                    .await;
            }
            // Prompt-based tools on the Messages API: the instruction joins
            // the system prompt, no native tools are sent, and the content
            // is held back so tool-call JSON never streams to the user.
            let msgs = with_prompt_tools(&prepared, tools);
            let mut result = self
                .chat_stream_anthropic(&msgs, &[], model, effort, cancel, |kind, d| {
                    if kind != DeltaKind::Content {
                        on_delta(kind, d)
                    }
                })
                .await?;
            let (clean, calls) = parse_prompt_tool_calls(&result.content);
            if !clean.is_empty() {
                on_delta(DeltaKind::Content, &clean);
            }
            result.content = clean;
            result.tool_calls = calls;
            return Ok(result);
        }
        // Anthropic thinking blocks and result markers mean nothing to
        // other APIs.
        let prepared: Vec<Value> = prepared
            .into_iter()
            .map(|mut m| {
                if let Some(o) = m.as_object_mut() {
                    o.remove("thinking_blocks");
                    o.remove("_is_error");
                }
                m
            })
            .collect();
        let messages: &[Value] = &prepared;
        // Prompt-based fallback: describe tools in-band, send none natively,
        // and buffer content so tool-call JSON never streams to the user.
        let prompt_mode = self.cfg.prompt_tools() && !tools.is_empty();
        let augmented: Vec<Value>;
        let messages: &[Value] = if prompt_mode {
            augmented = with_prompt_tools(messages, tools);
            &augmented
        } else {
            messages
        };
        let effective_tools: &[Value] = if prompt_mode { &[] } else { tools };
        let cache_breakpoints = self.cfg.cache_breakpoints(model);
        let mut body = if self.cfg.responses() {
            let mut input = responses_input(messages);
            if cache_breakpoints {
                mark_responses_system(&mut input);
            }
            let mut b = json!({"model": model, "input": input, "stream": true});
            if let Some(key) = &self.cache_key {
                b["prompt_cache_key"] = json!(key);
            }
            if !effective_tools.is_empty() {
                b["tools"] = json!(responses_tools(effective_tools));
            }
            b
        } else {
            // Ask for real token counts in the final chunk (dropped again if
            // the endpoint rejects the option).
            let marked;
            let messages = if cache_breakpoints {
                marked = with_cache_breakpoints(messages);
                &marked[..]
            } else {
                messages
            };
            let mut b = json!({"model": model, "messages": messages, "stream": true,
                "stream_options": {"include_usage": true}});
            if let Some(key) = &self.cache_key {
                b["prompt_cache_key"] = json!(key);
            }
            if !effective_tools.is_empty() {
                b["tools"] = json!(effective_tools);
            }
            b
        };
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        // Responses `output_index` → position in `tool_calls`.
        let mut call_slots: HashMap<u64, usize> = HashMap::new();
        // Chat tool-call slots seen streaming incrementally (#1106).
        let mut incremental: Vec<bool> = Vec::new();
        let mut truncated = false;
        // Why it stopped (#615), and refusal text the model sent instead
        // of an answer.
        let mut finish: Option<String> = None;
        // A `data:` field whose JSON spans several lines (#614).
        let mut pending = String::new();
        let mut usage = None;
        let mut attempt = 0;
        let (recovered, model_used) = 'request: loop {
            // Waiting for the response headers is cancellable too (a slow or
            // stuck endpoint must not outlive an interrupt).
            let (resp, recovered, model_used) = tokio::select! {
                _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                r = self.post_provider(&mut body, effort, true) => r?,
            };
            let mut stream = resp.bytes_stream();
            // Raw bytes: a chunk boundary can split a multibyte character, so
            // only complete lines are decoded.
            let mut buf: Vec<u8> = Vec::new();
            pending.clear();
            let mut eof = false;
            loop {
                if eof {
                    break;
                }
                tokio::select! {
                    _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                    next = stream.next() => {
                        // At the end, a last event without its newline still
                        // counts (often the usage / finish chunk, #614).
                        let next = match next {
                            Some(c) => Some(c),
                            None if buf.is_empty() && pending.is_empty() => break,
                            None => {
                                eof = true;
                                Some(Ok(tokio_util::bytes::Bytes::from_static(b"\n\n")))
                            }
                        };
                        let Some(chunk) = next else { break };
                        let chunk = match chunk {
                            Ok(chunk) => chunk,
                            // A dropped stream is retried like a failed request
                            // (SPEC §4.4); partial output is discarded, and
                            // clients are told to clear what they showed.
                            Err(e) if attempt < self.cfg.max_retries => {
                                tracing::warn!("provider stream dropped ({e}); re-sending the request");
                                if !content.is_empty() || !reasoning.is_empty() {
                                    on_delta(DeltaKind::Restart, "");
                                }
                                content.clear();
                                reasoning.clear();
                                tool_calls.clear();
                                call_slots.clear();
                                truncated = false;
                                usage = None;
                                tokio::select! {
                                    _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                                    _ = self.retry_wait(attempt, None, "stream dropped".into()) => {}
                                }
                                attempt += 1;
                                continue 'request;
                            }
                            Err(e) => return Err(e).context("stream read"),
                        };
                        on_delta(DeltaKind::Bytes, "");
                        buf.extend_from_slice(&chunk);
                        if buf.len() > MAX_SSE_LINE && !buf.contains(&b'\n') {
                            return Err(anyhow!(
                                "provider stream line over {} MB without a newline",
                                MAX_SSE_LINE / (1024 * 1024)
                            ));
                        }
                        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                            let raw: Vec<u8> = buf.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&raw);
                            let line = line.trim();
                            if line.is_empty() {
                                // End of an event: a split field that never
                                // became JSON is dropped, and said so.
                                if !pending.is_empty() {
                                    tracing::warn!("dropped an SSE event that is not JSON ({} bytes)", pending.len());
                                    pending.clear();
                                }
                                continue;
                            }
                            if let Some(data) = line.strip_prefix("data:") {
                                let data = data.trim();
                                if data == "[DONE]" { continue; }
                                // Several `data:` lines make one field (SSE): a
                                // line that isn't JSON alone waits for the next.
                                let joined = if pending.is_empty() {
                                    data.to_string()
                                } else {
                                    format!("{pending}\n{data}")
                                };
                                let parsed = serde_json::from_str::<Value>(&joined);
                                if parsed.is_err() {
                                    if joined.len() > MAX_SSE_LINE {
                                        return Err(anyhow!("provider stream event over {} MB", MAX_SSE_LINE / (1024 * 1024)));
                                    }
                                    pending = joined;
                                    continue;
                                }
                                pending.clear();
                                if let Ok(v) = parsed {
                                    // Mid-stream failures: Responses `error` / `response.failed` /
                                    // `response.incomplete` events, or a Chat Completions `{"error": ...}`
                                    // chunk. Retry only before anything streamed, so the user never sees
                                    // duplicated output; otherwise the Run fails loudly (SPEC §4.4).
                                    // Responses stopped early for length or a filter:
                                    // the partial answer stands, marked (#615).
                                    if self.cfg.responses() && v["type"] == "response.incomplete" {
                                        match v["response"]["incomplete_details"]["reason"].as_str() {
                                            Some("max_output_tokens") => {
                                                truncated = true;
                                                finish = Some("length".into());
                                                continue;
                                            }
                                            Some("content_filter") => {
                                                finish = Some("content_filter".into());
                                                continue;
                                            }
                                            _ => {}
                                        }
                                    }
                                    let failed = if self.cfg.responses() {
                                        matches!(
                                            v["type"].as_str(),
                                            Some("error" | "response.failed" | "response.incomplete")
                                        )
                                    } else {
                                        v["error"].is_object()
                                    };
                                    if failed {
                                        let (retryable, message) = responses_failure(&v);
                                        let nothing_streamed =
                                            content.is_empty() && reasoning.is_empty() && tool_calls.is_empty();
                                        if retryable && nothing_streamed && attempt < self.cfg.max_retries {
                                            tokio::select! {
                                                _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                                                _ = self.retry_wait(attempt, None, "provider error".into()) => {}
                                            }
                                            attempt += 1;
                                            continue 'request;
                                        }
                                        return Err(anyhow!("provider error: {message}"));
                                    }
                                    if self.cfg.responses() {
                                        match v["type"].as_str().unwrap_or_default() {
                                            "response.output_text.delta" => {
                                                if let Some(d) = v["delta"].as_str() {
                                                    content.push_str(d);
                                                    if !prompt_mode {
                                                        on_delta(DeltaKind::Content, d);
                                                    }
                                                }
                                            }
                                            // A refusal is the answer, never an empty one (#615).
                                            "response.refusal.delta" => {
                                                if let Some(d) = v["delta"].as_str() {
                                                    content.push_str(d);
                                                    finish = Some("refusal".into());
                                                    if !prompt_mode {
                                                        on_delta(DeltaKind::Content, d);
                                                    }
                                                }
                                            }
                                            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                                                if let Some(d) = v["delta"].as_str() {
                                                    reasoning.push_str(d);
                                                    on_delta(DeltaKind::Reasoning, d);
                                                }
                                            }
                                            "response.output_item.added" | "response.output_item.done" => {
                                                let item = &v["item"];
                                                if item["type"].as_str() == Some("function_call") {
                                                    let idx = responses_slot(&mut tool_calls, &mut call_slots, &v)?;
                                                    let slot = &mut tool_calls[idx];
                                                    if let Some(id) = item["call_id"].as_str().or_else(|| item["id"].as_str()) {
                                                        slot.id = id.to_string();
                                                    }
                                                    if let Some(n) = item["name"].as_str() {
                                                        slot.name = n.to_string();
                                                    }
                                                    if let Some(a) = item["arguments"].as_str() {
                                                        if !a.is_empty() {
                                                            slot.arguments = a.to_string();
                                                        }
                                                    }
                                                }
                                            }
                                            "response.function_call_arguments.delta" => {
                                                let idx = responses_slot(&mut tool_calls, &mut call_slots, &v)?;
                                                if let Some(d) = v["delta"].as_str() {
                                                    tool_calls[idx].arguments.push_str(d);
                                                }
                                            }
                                            "response.completed" => {
                                                usage = Usage::parse(&v["response"]["usage"]).or(usage);
                                                if content.is_empty() {
                                                    let t = responses_text(&v["response"]);
                                                    if !t.is_empty() {
                                                        content.push_str(&t);
                                                        if !prompt_mode {
                                                            on_delta(DeltaKind::Content, &t);
                                                        }
                                                    }
                                                }
                                                if tool_calls.iter().all(|c| c.name.is_empty()) {
                                                    tool_calls = responses_tool_calls(&v["response"]);
                                                }
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    }
                                    // With include_usage the last chunk carries usage
                                    // (and usually no choices).
                                    usage = Usage::parse(&v["usage"]).or(usage);
                                    if let Some(fr) = v["choices"][0]["finish_reason"].as_str() {
                                        if fr == "length" {
                                            truncated = true;
                                        }
                                        if finish.as_deref() != Some("refusal") {
                                            finish = Some(fr.to_string());
                                        }
                                    }
                                    let delta = &v["choices"][0]["delta"];
                                    // A refusal is the answer, never an empty one (#615).
                                    if let Some(d) = delta["refusal"].as_str() {
                                        if !d.is_empty() {
                                            content.push_str(d);
                                            finish = Some("refusal".into());
                                            if !prompt_mode {
                                                on_delta(DeltaKind::Content, d);
                                            }
                                        }
                                    }
                                    if let Some(d) = delta["content"].as_str() {
                                        if !d.is_empty() {
                                            content.push_str(d);
                                            if !prompt_mode {
                                                on_delta(DeltaKind::Content, d);
                                            }
                                        }
                                    }
                                    // Thinking-mode endpoints stream reasoning separately
                                    // (`reasoning_content`, e.g. DeepSeek; some use `reasoning`).
                                    for key in ["reasoning_content", "reasoning"] {
                                        if let Some(d) = delta[key].as_str() {
                                            if !d.is_empty() {
                                                reasoning.push_str(d);
                                                on_delta(DeltaKind::Reasoning, d);
                                            }
                                        }
                                    }
                                    if let Some(tcs) = delta["tool_calls"].as_array() {
                                        for tc in tcs {
                                            merge_tool_fragment(&mut tool_calls, &mut incremental, tc)?;
                                        }
                                    }
                                    // Non-streaming-shaped servers may put message.tool_calls in
                                    // one chunk; a call already streamed as deltas isn't added twice.
                                    // Id-less calls match by name and arguments, each earlier
                                    // call at most once, so parallel calls all survive (#1108).
                                    if let Some(tcs) = v["choices"][0]["message"]["tool_calls"].as_array() {
                                        let mut matched = vec![false; tool_calls.len()];
                                        for tc in tcs {
                                            let id = tc["id"].as_str().unwrap_or_default();
                                            let name = tc["function"]["name"].as_str().unwrap_or_default();
                                            let args = tc["function"]["arguments"].as_str().unwrap_or_default();
                                            let seen = (0..matched.len()).find(|&i| {
                                                !matched[i]
                                                    && if id.is_empty() {
                                                        tool_calls[i].name == name && tool_calls[i].arguments == args
                                                    } else {
                                                        tool_calls[i].id == id
                                                    }
                                            });
                                            match seen {
                                                Some(i) => matched[i] = true,
                                                None if tool_calls.len() >= MAX_TOOL_CALLS => {
                                                    bail!("provider sent more than {MAX_TOOL_CALLS} tool calls in one response")
                                                }
                                                None => tool_calls.push(ToolCall {
                                                    id: id.to_string(),
                                                    name: name.to_string(),
                                                    arguments: args.to_string(),
                                                }),
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            break (recovered, model_used);
        };
        if prompt_mode {
            let (clean, calls) = parse_prompt_tool_calls(&content);
            if !clean.is_empty() {
                on_delta(DeltaKind::Content, &clean);
            }
            content = clean;
            if !calls.is_empty() {
                tool_calls = calls;
            }
        }
        // A call without a name can never be dispatched, and once stored it
        // makes every later request of the Session fail (#541).
        let before = tool_calls.len();
        tool_calls.retain(|c| !c.name.trim().is_empty());
        if tool_calls.len() < before {
            tracing::warn!(
                "dropped {} tool call(s) without a name from the provider response",
                before - tool_calls.len()
            );
        }
        for (i, tc) in tool_calls.iter_mut().enumerate() {
            if tc.id.is_empty() {
                tc.id = format!("call_{i}");
            }
        }
        Ok(ChatResult {
            content,
            reasoning,
            tool_calls,
            thinking_blocks: Vec::new(),
            dropped_reasoning_effort: recovered.dropped_effort,
            model_used,
            truncated,
            usage,
            // Chat / Responses: a filter or refusal stop is said (#615);
            // a normal or length stop is not (length is `truncated`).
            stop_reason: finish.filter(|f| matches!(f.as_str(), "content_filter" | "refusal")),
            stop_detail: None,
            notes: recovered.notes,
            dropped_replayed_thinking: false,
        })
    }

    /// The native Anthropic Messages stream (SPEC §4.1): retries like
    /// the other APIs before anything streamed, folds the events into the
    /// same `ChatResult`.
    async fn chat_stream_anthropic<F: FnMut(DeltaKind, &str) + Send>(
        &self,
        messages: &[Value],
        tools: &[Value],
        model: &str,
        effort: Option<&str>,
        cancel: &CancellationToken,
        mut on_delta: F,
    ) -> Result<ChatResult> {
        // The same model gate as Chat's breakpoints (`auto` = Claude ids).
        let cache = self.cfg.cache_breakpoints(model);
        let mut body = crate::anthropic::body(model, messages, tools, cache, effort, true);
        let mut notes = crate::anthropic::skipped_images(messages);
        let mut dropped_replayed_thinking = false;
        let mut attempt = 0;
        let mut shown = false;
        loop {
            let (resp, recovered, model_used) = tokio::select! {
                _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                r = self.post_provider(&mut body, None, true) => r?,
            };
            for n in recovered.notes {
                if !notes.contains(&n) {
                    notes.push(n);
                }
            }
            dropped_replayed_thinking |= recovered.dropped_replayed_thinking;
            let mut stream = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut f = crate::anthropic::Folded::default();
            let mut failed: Option<String> = None;
            let fold = |f: &mut crate::anthropic::Folded, raw: &[u8], on_delta: &mut F| {
                let line = String::from_utf8_lossy(raw);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    return;
                };
                let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
                    return;
                };
                match f.event(&v) {
                    crate::anthropic::Delta::Text(t) => on_delta(DeltaKind::Content, t),
                    crate::anthropic::Delta::Thinking(t) => on_delta(DeltaKind::Reasoning, t),
                    crate::anthropic::Delta::None => {}
                }
            };
            'read: loop {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                    next = stream.next() => {
                        let Some(chunk) = next else {
                            // A last event without its newline still counts.
                            if !buf.is_empty() {
                                let rest = std::mem::take(&mut buf);
                                fold(&mut f, &rest, &mut on_delta);
                            }
                            break 'read;
                        };
                        let chunk = match chunk {
                            Ok(c) => c,
                            Err(e) => {
                                failed = Some(format!("stream dropped: {e}"));
                                break 'read;
                            }
                        };
                        on_delta(DeltaKind::Bytes, "");
                        buf.extend_from_slice(&chunk);
                        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                            let raw: Vec<u8> = buf.drain(..=pos).collect();
                            fold(&mut f, &raw, &mut on_delta);
                            if f.error.is_some() {
                                break 'read;
                            }
                        }
                    }
                }
            }
            shown |= !f.nothing_streamed();
            if let Some((kind, msg)) = &f.error {
                failed = Some(format!("{kind}: {msg}"));
            } else if failed.is_none() {
                // An answer is finished only at `message_stop`: anything
                // else is a dropped stream, never a final answer.
                failed = f.incomplete().map(|why| format!("stream dropped: {why}"));
            }
            if let Some(why) = failed {
                let dropped = why.starts_with("stream dropped");
                let reason = if dropped {
                    Some("stream dropped")
                } else if why.contains("overloaded") {
                    Some("overloaded")
                } else if why.contains("rate_limit") {
                    Some("rate limited")
                } else if why.contains("api_error") {
                    Some("server error")
                } else {
                    None
                };
                // A dropped stream is re-sent even after partial output (as
                // on the other APIs); an error event only before any.
                let retry = reason.filter(|_| dropped || f.nothing_streamed());
                if let Some(reason) = retry.filter(|_| attempt < self.cfg.max_retries) {
                    if shown {
                        // Clients clear what they showed of the lost attempt.
                        on_delta(DeltaKind::Restart, "");
                        shown = false;
                    }
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                        _ = self.retry_wait(attempt, None, reason.to_string()) => {}
                    }
                    attempt += 1;
                    continue;
                }
                return Err(anyhow!("provider error: {why}"));
            }
            let tool_calls = f
                .tool_calls
                .iter()
                .filter(|(_, name, _)| !name.trim().is_empty())
                .map(|(id, name, arguments)| ToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                })
                .collect();
            let usage = Usage {
                prompt_tokens: f.input_tokens + f.cache_read + f.cache_write,
                completion_tokens: f.output_tokens,
                cached_tokens: f.cache_read,
                cache_write_tokens: f.cache_write,
                ..Default::default()
            };
            return Ok(ChatResult {
                content: f.text,
                reasoning: f.thinking,
                tool_calls,
                thinking_blocks: f.thinking_blocks,
                dropped_reasoning_effort: false,
                model_used,
                truncated: f.truncated,
                usage: Some(usage),
                stop_reason: f.stop_reason.filter(|r| {
                    !matches!(
                        r.as_str(),
                        "end_turn" | "tool_use" | "stop_sequence" | "max_tokens"
                    )
                }),
                stop_detail: f.stop_detail,
                notes,
                dropped_replayed_thinking,
            });
        }
    }

    /// Server-side compaction (SPEC §11): POST {base_url}/compact with
    /// {model, input}; accepts {summary}|{output}|{content} responses.
    pub async fn compact_server(&self, text: &str, model: &str) -> Result<String> {
        let body = json!({"model": model, "input": text});
        let credential = self.credential().await?;
        let req = apply_headers(
            self.http.post(url(&self.cfg.base_url, "compact")),
            &self.cfg,
            &credential,
        )
        .json(&body);
        let resp = req.send().await.context("POST /compact")?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!(
                "POST /compact failed: {status}: {}",
                resp.text().await.unwrap_or_default()
            ));
        }
        let v: Value = resp.json().await.context("parse /compact")?;
        for key in ["summary", "output", "content"] {
            if let Some(s) = v[key].as_str() {
                if !s.is_empty() {
                    return Ok(s.to_string());
                }
            }
        }
        Err(anyhow!("/compact response has no summary/output/content"))
    }

    /// A compaction summary of `text`. A reply cut off at the output limit
    /// or with nothing in it is an error, so no checkpoint is made from it
    /// (#619).
    pub async fn summarize(&self, text: &str, model: &str) -> Result<String> {
        let (summary, _, cut) = self
            .complete_full(
                &[
                    json!({"role": "system", "content": SUMMARIZE_PROMPT}),
                    json!({"role": "user", "content": text}),
                ],
                model,
                None,
            )
            .await?;
        if cut {
            bail!("the summary was cut off at the model's output limit; nothing was compacted");
        }
        if summary.trim().is_empty() {
            bail!("the model returned an empty summary; nothing was compacted");
        }
        Ok(summary)
    }
}

/// `messages` with the prompt-based tool instruction appended to the
/// system prompt (one is added when there is none).
fn with_prompt_tools(messages: &[Value], tools: &[Value]) -> Vec<Value> {
    let instr = prompt_tool_instruction(tools);
    let mut msgs = messages.to_vec();
    match msgs.first_mut() {
        Some(first) if first["role"].as_str() == Some("system") => {
            let cur = first["content"].as_str().unwrap_or_default().to_string();
            first["content"] = json!(format!("{cur}{instr}"));
        }
        _ => msgs.insert(0, json!({"role": "system", "content": instr.trim_start()})),
    }
    msgs
}

/// Largest image sent inline to a vision model.
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// An attachment as a `data:` URL for a vision model: scaled down to
/// `max_px` on its longest side, encoded once and kept, so later steps
/// don't read, scale and encode it again (#616). The error says why it
/// can't be sent.
fn encoded_image(path: &str, mime: &str, max_px: u32) -> std::result::Result<String, String> {
    use base64::Engine;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<(String, u32), String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (path.to_string(), max_px);
    if let Some(url) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(url.clone());
    }
    let bytes = std::fs::read(path).map_err(|_| "the file is gone".to_string())?;
    let (bytes, mime) = match image::load_from_memory(&bytes) {
        Ok(img) if max_px > 0 && img.width().max(img.height()) > max_px => {
            let small = img.thumbnail(max_px, max_px);
            let mut out = Vec::new();
            // PNG keeps transparency; anything else becomes JPEG.
            let (format, mime) = if small.color().has_alpha() {
                (image::ImageFormat::Png, "image/png")
            } else {
                (image::ImageFormat::Jpeg, "image/jpeg")
            };
            let small = if format == image::ImageFormat::Jpeg {
                image::DynamicImage::ImageRgb8(small.to_rgb8())
            } else {
                small
            };
            small
                .write_to(&mut std::io::Cursor::new(&mut out), format)
                .map_err(|e| format!("it couldn't be scaled down ({e})"))?;
            (out, mime.to_string())
        }
        Ok(_) => (bytes, mime.to_string()),
        Err(_) if bytes.len() <= MAX_IMAGE_BYTES => (bytes, mime.to_string()),
        Err(_) => return Err("it isn't an image this build can read".into()),
    };
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "it is larger than {} MB",
            MAX_IMAGE_BYTES / 1024 / 1024
        ));
    }
    let url = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    if c.len() >= 64 {
        c.clear();
    }
    c.insert(key, url.clone());
    Ok(url)
}

/// A guess at whether a model accepts image input, from its name.
pub fn is_vision_model(model: &str) -> bool {
    let m = model.to_lowercase();
    [
        "gpt-4o",
        "gpt-4.1",
        "gpt-5",
        "o3",
        "o4",
        "claude",
        "gemini",
        "vision",
        "-vl",
        "vl-",
        "llava",
        "pixtral",
        "llama-4",
        "llama4",
        "gemma-3",
        "gemma3",
        "minicpm-v",
        "moondream",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// What a model can do and costs, per id (SPEC §4.2): from the
/// listing's own fields (OpenRouter: `pricing`, `context_length`,
/// `architecture.input_modalities`, `supported_parameters`), else the
/// built-in table for well-known families. Prices are USD per million
/// tokens; `efforts` lists the reasoning efforts it accepts ([] = effort
/// has no effect; absent = unknown).
pub fn parse_model_meta(body: &Value) -> HashMap<String, Value> {
    let windows = parse_model_windows(body);
    body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            let mut meta = known_model_meta(id);
            if let Some(c) = windows.get(id) {
                meta["context"] = json!(c);
            }
            let per_m = |v: &Value| {
                v.as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .or_else(|| v.as_f64())
                    .map(|p| (p * 1_000_000.0 * 1000.0).round() / 1000.0)
            };
            if let Some(p) = per_m(&m["pricing"]["prompt"]) {
                meta["input_price"] = json!(p);
            }
            if let Some(p) = per_m(&m["pricing"]["completion"]) {
                meta["output_price"] = json!(p);
            }
            // Anthropic's listing: the output cap per model.
            if let Some(cap) = m["max_tokens"].as_u64() {
                meta["max_output"] = json!(cap);
            }
            if let Some(mods) = m["architecture"]["input_modalities"].as_array() {
                meta["vision"] = json!(mods.iter().any(|x| x == "image"));
            }
            if let Some(params) = m["supported_parameters"].as_array() {
                let has = |p: &str| params.iter().any(|x| x == p);
                meta["tools"] = json!(has("tools"));
                if has("reasoning") || has("reasoning_effort") {
                    if meta["efforts"].is_null() {
                        meta["efforts"] = json!(["low", "medium", "high"]);
                    }
                } else {
                    meta["efforts"] = json!([]);
                }
            }
            Some((id.to_string(), meta))
        })
        .collect()
}

/// The built-in table: what well-known model families support, by id.
pub fn known_model_meta(id: &str) -> Value {
    let m = id.to_ascii_lowercase();
    let name = m.rsplit('/').next().unwrap_or(&m);
    let mut meta = json!({});
    let set = |meta: &mut Value, vision: bool, tools: bool, efforts: Option<&[&str]>| {
        meta["vision"] = json!(vision);
        meta["tools"] = json!(tools);
        if let Some(e) = efforts {
            meta["efforts"] = json!(e);
        }
    };
    if name.starts_with("gpt-5") {
        set(
            &mut meta,
            true,
            true,
            Some(&["minimal", "low", "medium", "high"]),
        );
    } else if name.starts_with("o1") || name.starts_with("o3") || name.starts_with("o4") {
        set(&mut meta, true, true, Some(&["low", "medium", "high"]));
    } else if name.starts_with("gpt-4o") || name.starts_with("gpt-4.1") {
        set(&mut meta, true, true, Some(&[]));
    } else if name.contains("claude") {
        // Claude before 3.7 rejects extended thinking.
        let efforts: Option<&[&str]> = if crate::anthropic::supports_thinking(name) {
            None
        } else {
            Some(&[])
        };
        set(&mut meta, true, true, efforts);
        if let Some(cap) = crate::anthropic::output_cap(name) {
            meta["max_output"] = json!(cap);
        }
    } else if name.contains("gemini") {
        // The compatible endpoint maps these onto its thinking levels.
        set(&mut meta, true, true, Some(&["low", "medium", "high"]));
    } else if name.contains("deepseek-r1") || name.contains("qwq") {
        set(&mut meta, false, false, Some(&[]));
    }
    meta
}

/// Longest stream line (or split event) accepted before failing (#614).
const MAX_SSE_LINE: usize = 16 * 1024 * 1024;

/// Most resends of one request after fixing what the provider rejected
/// (optional parameters, effort, Anthropic strips, an OAuth refresh).
const MAX_RESENDS: usize = 8;

/// Most tool calls one response may carry; a larger streamed index is a
/// provider protocol error, never an allocation (#1107).
const MAX_TOOL_CALLS: usize = 128;

/// Fold one streamed Chat tool-call fragment into the calls so far
/// (#614): by `index`, else by `id`, else onto the last call. A server
/// that resends the whole name or arguments so far (cumulative style)
/// replaces rather than doubles them; `incremental[i]` records that slot
/// `i` has been seen appending, after which nothing is replaced (#1106).
fn merge_tool_fragment(
    calls: &mut Vec<ToolCall>,
    incremental: &mut Vec<bool>,
    tc: &Value,
) -> Result<()> {
    let id = tc["id"].as_str().filter(|i| !i.is_empty());
    let name = tc["function"]["name"].as_str().filter(|n| !n.is_empty());
    let idx = match (tc["index"].as_u64(), id) {
        (Some(i), _) => usize::try_from(i).unwrap_or(usize::MAX),
        (None, Some(id)) => calls.iter().position(|c| c.id == id).unwrap_or(calls.len()),
        (None, None) => match (calls.last(), name) {
            // A name after the last call's arguments starts another call.
            (Some(last), Some(n))
                if !last.arguments.is_empty() && n != last.name && !n.starts_with(&last.name) =>
            {
                calls.len()
            }
            _ => calls.len().saturating_sub(1),
        },
    };
    if idx >= MAX_TOOL_CALLS {
        bail!("provider sent tool call index {idx} (at most {MAX_TOOL_CALLS} calls per response)");
    }
    while calls.len() <= idx {
        calls.push(ToolCall::default());
    }
    incremental.resize(calls.len(), false);
    let slot = &mut calls[idx];
    if let Some(id) = id {
        slot.id = id.to_string();
    }
    let appending = &mut incremental[idx];
    let mut grow = |have: &mut String, frag: &str| {
        // Cumulative only when strictly longer and the slot never appended:
        // an incremental "aaa" + "aaa" is "aaaaaa", not "aaa".
        if !*appending
            && frag.len() > have.len()
            && frag.starts_with(have.as_str())
            && !have.is_empty()
        {
            *have = frag.to_string();
        } else {
            if !have.is_empty() {
                *appending = true;
            }
            have.push_str(frag);
        }
    };
    if let Some(n) = tc["function"]["name"].as_str() {
        grow(&mut slot.name, n);
    }
    if let Some(a) = tc["function"]["arguments"].as_str() {
        grow(&mut slot.arguments, a);
    }
    Ok(())
}

fn parse_models(body: &Value) -> Vec<String> {
    body["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Per-model prices from a `/models` listing's `pricing` (USD per token,
/// as strings or numbers: `prompt`, `completion`, `input_cache_read`,
/// `input_cache_write`), as USD per million tokens. Free (0/0) and
/// unparseable entries are left out.
pub fn parse_model_prices(
    body: &Value,
    retrieved_at: &str,
) -> HashMap<String, crate::config::ModelPrice> {
    let per_m = |v: &Value| -> Option<f64> {
        let x = match v {
            Value::String(s) => s.parse::<f64>().ok()?,
            Value::Number(n) => n.as_f64()?,
            _ => return None,
        };
        (x.is_finite() && x >= 0.0).then_some(x * 1e6)
    };
    body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            let p = &m["pricing"];
            let input = per_m(&p["prompt"])?;
            let output = per_m(&p["completion"])?;
            if input == 0.0 && output == 0.0 {
                return None;
            }
            Some((
                id.to_string(),
                crate::config::ModelPrice {
                    input,
                    output,
                    cached_input: per_m(&p["input_cache_read"]),
                    cache_write_input: per_m(&p["input_cache_write"]),
                    retrieved_at: Some(retrieved_at.to_string()),
                },
            ))
        })
        .collect()
}

/// Context window per model id, from a `/models` listing's per-model
/// `context_length` (OpenRouter, LM Studio), `top_provider.context_length`,
/// `max_model_len` (vLLM) or `context_window`.
pub fn parse_model_windows(body: &Value) -> HashMap<String, u32> {
    let Some(models) = body["data"].as_array() else {
        return HashMap::new();
    };
    models
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            let n = [
                &m["context_length"],
                &m["top_provider"]["context_length"],
                &m["max_model_len"],
                &m["context_window"],
                &m["max_context_length"],
                &m["max_input_tokens"],
            ]
            .into_iter()
            .find_map(Value::as_u64)
            .filter(|n| *n >= 1024)?;
            Some((id.to_string(), n.min(u64::from(u32::MAX)) as u32))
        })
        .collect()
}

fn is_model_error(status: u16, text: &str) -> bool {
    status == 404 || is_model_error_text(&text.to_lowercase())
}

/// A (lowercased) error body saying the model doesn't exist.
pub fn is_model_error_text(t: &str) -> bool {
    t.contains("model")
        && (t.contains("not found")
            || t.contains("does not exist")
            || t.contains("not available")
            || t.contains("invalid model"))
}

/// How long the server asked us to wait: `Retry-After` (seconds or an
/// HTTP date), else the longest `x-ratelimit-reset-*` (`"1s"`, `"6m0s"`,
/// `"20ms"`).
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(v) = header("retry-after") {
        if let Ok(secs) = v.trim().parse::<f64>() {
            return Some(Duration::from_secs_f64(secs.max(0.0)));
        }
        if let Ok(at) = chrono::DateTime::parse_from_rfc2822(v.trim()) {
            let wait = at.signed_duration_since(chrono::Utc::now());
            return Some(wait.to_std().unwrap_or_default());
        }
    }
    ["x-ratelimit-reset-requests", "x-ratelimit-reset-tokens"]
        .iter()
        .filter_map(|h| header(h).and_then(parse_reset_duration))
        .max()
}

/// `"1m30s"`, `"6m0s"`, `"20ms"`, `"0.5s"`, or bare seconds.
fn parse_reset_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    if let Ok(secs) = text.parse::<f64>() {
        return Some(Duration::from_secs_f64(secs.max(0.0)));
    }
    let mut total = 0.0;
    let mut rest = text;
    while !rest.is_empty() {
        let num_len = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        let value: f64 = rest[..num_len].parse().ok()?;
        rest = &rest[num_len..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        total += value
            * match &rest[..unit_len] {
                "h" => 3600.0,
                "m" => 60.0,
                "s" => 1.0,
                "ms" => 0.001,
                _ => return None,
            };
        rest = &rest[unit_len..];
    }
    Some(Duration::from_secs_f64(total))
}

/// Whether a 400 body rejects the reasoning-effort parameter itself (and
/// not something else that happens to be "unknown" or "unsupported").
fn is_unknown_param(text: &str) -> bool {
    let is_effort_param = |p: &str| {
        let p = p.to_lowercase();
        p == "reasoning_effort" || p == "reasoning" || p.starts_with("reasoning.")
    };
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        if let Some(param) = v["error"]["param"].as_str() {
            return is_effort_param(param);
        }
    }
    let t = text.to_lowercase();
    let names_param = [
        "reasoning_effort",
        "reasoning.effort",
        "'reasoning'",
        "\"reasoning\"",
    ]
    .iter()
    .any(|p| t.contains(p));
    let rejects = [
        "unknown",
        "unrecognized",
        "unsupported",
        "not supported",
        "not permitted",
        "not allowed",
        "extra inputs",
        "extra_forbidden",
        "invalid",
    ]
    .iter()
    .any(|p| t.contains(p));
    names_param && rejects
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub models: Vec<String>,
    pub streaming: bool,
    pub tool_calling: bool,
    /// True when native tool-calling failed but the prompt-based
    /// fallback works (wizard then saves tool_mode = "prompt").
    pub prompt_fallback: bool,
}

fn is_tool_choice_rejection(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("tool_choice") && (t.contains("support") || t.contains("thinking"))
}

async fn probe_tool_call(p: &Provider, model: &str, tool: &Value, forced: bool) -> Result<bool> {
    if p.cfg.anthropic() {
        let mut b = crate::anthropic::body(
            model,
            &[json!({"role": "user", "content": "Call the probe_ping tool now."})],
            std::slice::from_ref(tool),
            false,
            None,
            false,
        );
        if forced {
            b["tool_choice"] = json!({"type": "any"});
        }
        let (resp, _, _) = p.post_provider(&mut b, None, false).await?;
        let v: Value = resp.json().await.context("parse probe response")?;
        return Ok(!crate::anthropic::response_parts(&v).1.is_empty());
    }
    let mut body = if p.cfg.responses() {
        let mut b = json!({
            "model": model,
            "input": responses_input(&[json!({"role": "user", "content": "Call the probe_ping tool now."})]),
            "tools": responses_tools(std::slice::from_ref(tool)),
            "stream": false
        });
        if forced {
            b["tool_choice"] = json!("required");
        }
        b
    } else {
        let mut b = json!({
            "model": model,
            "messages": [{"role": "user", "content": "Call the probe_ping tool now."}],
            "tools": [tool],
            "stream": false
        });
        if forced {
            b["tool_choice"] = json!("required");
        }
        b
    };
    let (resp, _, _) = p.post_provider(&mut body, None, false).await?;
    let v: Value = resp.json().await.context("parse probe response")?;
    if p.cfg.responses() {
        return Ok(!responses_tool_calls(&v).is_empty());
    }
    Ok(v["choices"][0]["message"]["tool_calls"]
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false))
}

async fn probe_prompt_tool_call(p: &Provider, model: &str, tool: &Value) -> Result<bool> {
    let instr = prompt_tool_instruction(std::slice::from_ref(tool));
    let text = p
        .complete(
            &[
                json!({"role": "system", "content": format!("You are a tool-using agent.{instr}")}),
                json!({"role": "user", "content": "Call the probe_ping tool now."}),
            ],
            model,
            None,
        )
        .await?;
    let (_, calls) = parse_prompt_tool_calls(&text);
    Ok(calls.iter().any(|c| c.name == "probe_ping"))
}

/// Probe (SPEC §4.3): connectivity via /models, then a required tool-call.
/// Thinking-mode endpoints reject a forced `tool_choice`, so on that
/// specific 400 the probe retries with `tool_choice` omitted. If native
/// tool-calling still fails, the prompt-based fallback is probed; success
/// there sets `prompt_fallback` so onboarding can save tool_mode=prompt.
pub async fn probe(cfg: &ProviderCfg, api_key: &str, model: &str) -> Result<ProbeResult> {
    let p = Provider::new(cfg.clone(), api_key.to_string())?;
    let models = p.list_models().await.unwrap_or_default();
    // One real streamed reply, so `streaming` says what the endpoint did
    // rather than what Formless hopes (#622).
    let mut deltas = 0usize;
    let streamed = p
        .chat_stream(
            &[json!({"role": "user", "content": "Reply with: ok"})],
            &[],
            model,
            None,
            &tokio_util::sync::CancellationToken::new(),
            |_, _| deltas += 1,
        )
        .await
        .is_ok()
        && deltas > 0;
    let mut result = probe_tools(&p, cfg, model, models).await?;
    result.streaming = streamed;
    Ok(result)
}

async fn probe_tools(
    p: &Provider,
    cfg: &ProviderCfg,
    model: &str,
    models: Vec<String>,
) -> Result<ProbeResult> {
    let tool = json!({
        "type": "function",
        "function": {
            "name": "probe_ping",
            "description": "Connectivity probe. Call this.",
            "parameters": {"type": "object", "properties": {}}
        }
    });
    if cfg.prompt_tools() {
        let ok = probe_prompt_tool_call(p, model, &tool).await?;
        return Ok(ProbeResult {
            models,
            streaming: true,
            tool_calling: ok,
            prompt_fallback: ok,
        });
    }
    let native = match probe_tool_call(p, model, &tool, true).await {
        Ok(v) => Ok(v),
        Err(e) if is_tool_choice_rejection(&e.to_string()) => {
            probe_tool_call(p, model, &tool, false).await
        }
        Err(e) => Err(e),
    };
    match native {
        Ok(true) => Ok(ProbeResult {
            models,
            streaming: true,
            tool_calling: true,
            prompt_fallback: false,
        }),
        Ok(false) => {
            let ok = probe_prompt_tool_call(p, model, &tool)
                .await
                .unwrap_or(false);
            Ok(ProbeResult {
                models,
                streaming: true,
                tool_calling: ok,
                prompt_fallback: ok,
            })
        }
        Err(e) => {
            if probe_prompt_tool_call(p, model, &tool)
                .await
                .unwrap_or(false)
            {
                Ok(ProbeResult {
                    models,
                    streaming: true,
                    tool_calling: true,
                    prompt_fallback: true,
                })
            } else {
                Err(e)
            }
        }
    }
}

// --- Blocking wrappers for the synchronous TUI wizard (§10.1). ---

pub fn list_models_blocking(cfg: &ProviderCfg, api_key: &str) -> Result<Vec<String>> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(Provider::new(cfg.clone(), api_key.to_string())?.list_models())
}

/// Whether the endpoint serves repeated prompt prefixes from a cache: the
/// same long prompt is sent twice and the second answer's reported cached
/// tokens are returned (None when it reports no usage at all).
pub async fn cache_probe(cfg: &ProviderCfg, api_key: &str, model: &str) -> Result<Option<Usage>> {
    let p = Provider::new(cfg.clone(), api_key.to_string())?;
    // Long enough for providers that cache only prefixes of 1024+ tokens.
    let filler = "Formless prompt-cache probe. ".repeat(300);
    let messages = [
        json!({"role": "system", "content": filler}),
        json!({"role": "user", "content": "Reply with the single word: ok"}),
    ];
    let mut last = None;
    for _ in 0..2 {
        last = p
            .chat_stream(
                &messages,
                &[],
                model,
                None,
                &CancellationToken::new(),
                |_, _| {},
            )
            .await?
            .usage;
    }
    Ok(last)
}

pub fn cache_probe_blocking(
    cfg: &ProviderCfg,
    api_key: &str,
    model: &str,
) -> Result<Option<Usage>> {
    tokio::runtime::Runtime::new()?.block_on(cache_probe(cfg, api_key, model))
}

pub fn probe_blocking(cfg: &ProviderCfg, api_key: &str, model: &str) -> Result<ProbeResult> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(probe(cfg, api_key, model))
}

#[cfg(test)]
mod tests {

    /// `provider.extra_body` adds vendor fields without touching the core
    /// ones, and Ollama gets the context Formless budgets for (#622).
    #[test]
    fn extra_body_merges_and_ollama_gets_num_ctx() {
        let mut cfg = ProviderCfg {
            base_url: "http://localhost:11434/v1".into(),
            context_tokens: 32000,
            ..Default::default()
        };
        cfg.extra_body.insert("model".into(), json!("hijacked"));
        cfg.extra_body
            .insert("options".into(), json!({"temperature": 0.1}));
        cfg.extra_body.insert("keep_alive".into(), json!("10m"));
        let mut b = json!({"model": "llama", "messages": [], "stream": true});
        apply_extra_body(&mut b, &cfg, "llama");
        assert_eq!(b["model"], "llama", "core keys are protected");
        assert_eq!(b["keep_alive"], "10m");
        assert_eq!(b["options"]["temperature"], 0.1);
        assert_eq!(b["options"]["num_ctx"], 32000);
        // An explicit num_ctx wins; other endpoints get no options.
        cfg.extra_body
            .insert("options".into(), json!({"num_ctx": 8192}));
        let mut b = json!({"model": "llama"});
        apply_extra_body(&mut b, &cfg, "llama");
        assert_eq!(b["options"]["num_ctx"], 8192);
        let other = ProviderCfg {
            base_url: "https://api.example.com/v1".into(),
            ..Default::default()
        };
        let mut b = json!({"model": "m"});
        apply_extra_body(&mut b, &other, "m");
        assert!(b.get("options").is_none());
    }

    use super::*;

    #[test]
    fn usage_reads_reported_cost_and_cache_writes() {
        let u = Usage::parse(&json!({"prompt_tokens": 9213, "completion_tokens": 3,
            "cost": 0.0020006,
            "prompt_tokens_details": {"cached_tokens": 9203, "cache_write_tokens": 10}}))
        .unwrap();
        assert_eq!(u.cached_tokens, 9203);
        assert_eq!(u.cache_write_tokens, 10);
        assert_eq!(u.cost, Some(0.0020006));
        assert_eq!(
            Usage::parse(&json!({"prompt_tokens": 1})).unwrap().cost,
            None
        );
    }

    #[test]
    fn responses_input_keeps_the_preamble_before_tool_calls() {
        let items = responses_input(&[json!({"role": "assistant", "content": "Let me check.",
            "tool_calls": [{"id": "c1", "function": {"name": "f", "arguments": "{}"}}]})]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["content"][0]["type"], "output_text");
        assert_eq!(items[0]["content"][0]["text"], "Let me check.");
        assert_eq!(items[1]["type"], "function_call");
        let bare = responses_input(&[json!({"role": "assistant", "content": null,
            "tool_calls": [{"id": "c1", "function": {"name": "f", "arguments": "{}"}}]})]);
        assert_eq!(bare.len(), 1);
    }

    #[test]
    fn cache_breakpoints_mark_the_system_prompt_and_last_stable_text() {
        let msgs = [
            json!({"role": "system", "content": "sys"}),
            json!({"role": "user", "content": "q"}),
            json!({"role": "assistant", "content": null, "tool_calls": []}),
            json!({"role": "tool", "tool_call_id": "c", "content": "out"}),
        ];
        let out = with_cache_breakpoints(&msgs);
        assert_eq!(out[0]["content"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(out[0]["content"][0]["text"], "sys");
        assert_eq!(out[1]["content"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(out[2], msgs[2]);
        assert_eq!(out[3], msgs[3], "the newest message is never marked");
        let mut cfg = ProviderCfg::default();
        assert!(cfg.cache_breakpoints("anthropic/claude-opus-5.5"));
        assert!(!cfg.cache_breakpoints("deepseek/deepseek-v4-pro"));
        cfg.cache_control = "off".into();
        assert!(!cfg.cache_breakpoints("anthropic/claude-opus-5.5"));
    }

    #[test]
    fn model_windows_come_from_the_listing() {
        let w = parse_model_windows(&json!({"data": [
            {"id": "a", "context_length": 1_000_000},
            {"id": "b", "max_model_len": 32768},
            {"id": "c", "top_provider": {"context_length": 200_000}},
            {"id": "d"},
        ]}));
        assert_eq!(w["a"], 1_000_000);
        assert_eq!(w["b"], 32768);
        assert_eq!(w["c"], 200_000);
        assert!(!w.contains_key("d"));
    }

    #[test]
    fn history_keeps_the_preamble_on_tool_call_turns() {
        let m = formless_protocol::MessageInfo {
            role: "assistant".into(),
            content: "Let me check the file first.".into(),
            meta: Some(
                json!({"tool_calls": [{"id": "c1", "type": "function",
                "function": {"name": "f", "arguments": "{}"}}]})
                .to_string(),
            ),
            id: String::new(),
            session_id: String::new(),
            seq: 1,
            created_at: String::new(),
        };
        let v = crate::storage::provider_message(&m);
        assert_eq!(v["content"], "Let me check the file first.");
        assert!(v["tool_calls"].is_array());
        let empty = formless_protocol::MessageInfo {
            content: String::new(),
            ..m
        };
        assert!(crate::storage::provider_message(&empty)["content"].is_null());
    }

    #[test]
    fn gpt_family_gets_the_tool_use_addendum() {
        for m in [
            "gpt-4.1",
            "openai/gpt-5-mini",
            "o3",
            "o4-mini",
            "chatgpt-4o-latest",
            "GPT-4o",
        ] {
            assert!(model_family_addendum(m).is_some(), "{m}");
        }
        for m in [
            "claude-sonnet-4",
            "anthropic/claude-opus-4",
            "deepseek-chat",
            "llama3",
            "",
        ] {
            assert!(model_family_addendum(m).is_none(), "{m}");
        }
    }

    #[test]
    fn detects_access_refusals() {
        for t in [
            "I can't access files on your computer.",
            "I don\u{2019}t have access to the internet, but here is what I know.",
            "As a language model I do not have the ability to browse.",
            "You can run the following command and paste the output:",
            "Could you paste the contents of the file?",
        ] {
            assert!(looks_like_access_refusal(t), "{t}");
        }
        for t in [
            "Here are the 12 TODOs I found.",
            "The page title is \"Example Domain\".",
        ] {
            assert!(!looks_like_access_refusal(t), "{t}");
        }
    }

    #[test]
    fn retry_hints_are_parsed_from_headers() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut h = HeaderMap::new();
        assert_eq!(retry_after(&h), None);
        h.insert("x-ratelimit-reset-requests", HeaderValue::from_static("1s"));
        h.insert(
            "x-ratelimit-reset-tokens",
            HeaderValue::from_static("1m30s"),
        );
        assert_eq!(retry_after(&h), Some(Duration::from_secs(90)));
        h.insert("retry-after", HeaderValue::from_static("7"));
        assert_eq!(retry_after(&h), Some(Duration::from_secs(7)));
        assert_eq!(
            parse_reset_duration("20ms"),
            Some(Duration::from_millis(20))
        );
        assert_eq!(parse_reset_duration("6m0s"), Some(Duration::from_secs(360)));
        assert_eq!(parse_reset_duration("soon"), None);
        let mut h = HeaderMap::new();
        let at = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc2822();
        h.insert("retry-after", HeaderValue::from_str(&at).unwrap());
        let wait = retry_after(&h).unwrap();
        assert!(wait > Duration::from_secs(25) && wait <= Duration::from_secs(30));
    }

    #[test]
    fn backoff_has_jitter_a_cap_and_honours_the_server() {
        let cfg = ProviderCfg {
            max_backoff_secs: 4,
            ..Default::default()
        };
        let p = Provider::new(cfg, "k".into()).unwrap();
        let waits: Vec<Duration> = (0..50).map(|_| p.backoff(10, None)).collect();
        assert!(waits.iter().all(|w| *w <= Duration::from_secs(4)));
        assert!(
            waits.iter().any(|w| *w != waits[0]),
            "full jitter varies the wait"
        );
        assert_eq!(
            p.backoff(0, Some(Duration::from_secs(20))),
            Duration::from_secs(20),
            "a server hint beats our own cap"
        );
        assert_eq!(
            p.backoff(0, Some(Duration::from_secs(3600))),
            Duration::from_secs(300)
        );
    }

    async fn rejecting_provider(
        errors: Vec<&'static str>,
    ) -> (
        Provider,
        Arc<std::sync::Mutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::default();
        let app = axum::Router::new().route(
            "/messages",
            axum::routing::post({
                let seen = seen.clone();
                move |axum::Json(body): axum::Json<Value>| {
                    let seen = seen.clone();
                    let errors = errors.clone();
                    async move {
                        let index = {
                            let mut s = seen.lock().unwrap();
                            let i = s.len();
                            s.push(body);
                            i
                        };
                        (
                            axum::http::StatusCode::BAD_REQUEST,
                            axum::Json(
                                json!({"error":{"message":errors[index.min(errors.len()-1)]}}),
                            ),
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let provider = Provider::new(
            ProviderCfg {
                base_url: format!("http://{address}"),
                api_style: "anthropic".into(),
                max_retries: 0,
                ..Default::default()
            },
            "test-key".into(),
        )
        .unwrap();
        (provider, seen, server)
    }

    #[tokio::test]
    async fn persistent_image_rejections_do_not_resend_an_unchanged_request() {
        for image in [false, true] {
            let (p, seen, server) =
                rejecting_provider(vec!["unsupported image source: persistent-image-error"]).await;
            let content = if image {
                json!([{"type":"image","source":{"type":"url","url":"https://example.com/x.png"}}])
            } else {
                json!([{"type":"text","text":"hello"}])
            };
            let mut body =
                json!({"model":"claude-x", "messages":[{"role":"user","content":content}]});
            let error = p
                .post_provider(&mut body, None, false)
                .await
                .err()
                .expect("persistent400 must fail")
                .to_string();
            assert!(
                error.contains("400") && error.contains("persistent-image-error"),
                "{error}"
            );
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), if image { 2 } else { 1 }, "{seen:?}");
            if image {
                assert!(!seen[1].to_string().contains("\"type\":\"image\""));
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn recovery_kinds_apply_once_and_resend_cap_keeps_the_last_400() {
        let (p, seen, server) = rejecting_provider(vec![
            "stream_options unsupported",
            "prompt_cache_key unsupported",
            "Unrecognized request argument reasoning_effort",
            "Invalid signature in thinking block",
            "max_tokens must exceed thinking.budget_tokens",
            "thinking unsupported",
            "max_tokens: 10000 > 4096",
            "cache_control unsupported",
            "unsupported image: terminal-image-400",
        ])
        .await;
        let mut body = json!({"model":"claude-x", "max_tokens":10000, "thinking":{"type":"enabled","budget_tokens":10000},
        "stream_options":{}, "prompt_cache_key":"x", "messages":[{"role":"user","content":[
            {"type":"thinking","thinking":"x","signature":"bad"}, {"type":"image","source":{"type":"url","url":"https://example.com/x.png"}}, {"type":"text","text":"x","cache_control":{"type":"ephemeral"}}
        ]}]});
        let error = p
            .post_provider(&mut body, Some("high"), false)
            .await
            .err()
            .expect("resend cap must fail")
            .to_string();
        assert!(
            error.contains("400") && error.contains("terminal-image-400"),
            "{error}"
        );
        assert_eq!(seen.lock().unwrap().len(), MAX_RESENDS + 1);
        server.abort();

        let (p, seen, server) =
            rejecting_provider(vec!["max_tokens: 10000 > 8192", "max_tokens: 8192 > 4096"]).await;
        let mut body = json!({"model":"claude-x", "max_tokens":10000,"messages":[]});
        let error = p
            .post_provider(&mut body, None, false)
            .await
            .err()
            .expect("same recovery kind must not repeat")
            .to_string();
        assert!(error.contains("8192 > 4096"), "{error}");
        assert_eq!(seen.lock().unwrap().len(), 2);
        server.abort();
    }

    #[test]
    fn unknown_param_needs_the_reasoning_param_named() {
        // Rejections of the parameter itself.
        assert!(is_unknown_param(
            r#"{"error":{"message":"Unrecognized request argument supplied: reasoning_effort"}}"#
        ));
        assert!(is_unknown_param(
            r#"{"error":{"message":"bad","param":"reasoning_effort"}}"#
        ));
        assert!(is_unknown_param(
            r#"{"error":{"message":"bad","param":"reasoning.effort"}}"#
        ));
        assert!(is_unknown_param(
            "Extra inputs are not permitted: 'reasoning'"
        ));
        // Unrelated 400s keep reasoning_effort.
        assert!(!is_unknown_param(
            r#"{"error":{"message":"unsupported image format"}}"#
        ));
        assert!(!is_unknown_param(
            r#"{"error":{"message":"unknown tool name: foo"}}"#
        ));
        assert!(!is_unknown_param(
            r#"{"error":{"message":"this reasoning model needs more tokens"}}"#
        ));
        assert!(!is_unknown_param(
            r#"{"error":{"message":"unknown parameter","param":"tools"}}"#
        ));
    }
}
