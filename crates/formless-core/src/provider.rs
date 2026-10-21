use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::config::ProviderCfg;
use crate::Paths;

/// The Tool authoring contract and host API (SPEC §7.5), shared by the
/// bootstrap prompt and the optimiser so they never drift apart.
macro_rules! host_api_doc {
    () => {
        "Tool code is plain JavaScript defining `function run(input) { ... }` (or `async function run`) returning the output value. On failure, throw an Error: a returned {error: ...} is counted as a failure anyway, and swallowing errors hides broken Tools from measurement. Host API (all Capability-gated; data options take encoding \"utf8\" (default) or \"base64\" for binary): fetch(url, {method, headers, body, bodyEncoding, encoding}) -> {status, ok, url, headers, body, text(), json()} (redirects followed only within the granted hosts); fetchChunks(url, opts, size, cb) streams the body in chunks and returns status; fs.read(path, {encoding}), fs.write(path, data, {encoding}), fs.append(path, data, {encoding}), fs.exists(path), fs.stat(path) -> {type, size, modified_ms} or null, fs.list(dir, {details}) -> names or [{name, type, size}], fs.mkdir(path), fs.remove(path, {recursive}), fs.rename(from, to), fs.readChunks(path, size, cb, {encoding}), fs.readLines(path, cb); env.get(key); process.run(cmd, argsArray, {cwd, env, stdin, stdinEncoding, encoding}) -> {code, signal, stdout, stderr} (no shell); tools.call(name, input); clipboard.read(), clipboard.write(text); notify(title, body); llm.complete(prompt, {system, model}) -> text; agents.run(task, {system, model}) -> text; requireModule(name) / modules.load(name), modules.list(); emit(chunk) streams large output to an artifact; console.log, sleep(ms), setTimeout/setInterval/clearTimeout (real timers; run() may await them), cryptoRandom(), randomBytes(n) -> base64. Nothing else exists: anything more (URL parsing, base64 decoding to bytes, hashing, compression, formats) you write in JS."
    };
}
pub(crate) use host_api_doc;

/// Bootstrap system prompt (SPEC §7.7). Versioned; v2.1 adds generic-Tool
/// guidance and input-bound Capability scopes; v2.2 adds the find/read/test
/// built-ins, throw-on-failure and the extended host API.
pub const SYSTEM_PROMPT: &str = concat!("You are Formless, a general-purpose agent.\n\
You start with only built-ins: `create_tool`, `create_module`, `retire_tool`, `find_tools`, `read_tool` and `test_tool`. Everything else you build yourself: small, minimally-capabled Tools created with create_tool, persisted and reused across tasks and projects. Never expect a Tool to exist - find it or build it.\n\
The registry can hold far more Tools than are listed to you as functions. Before creating a Tool, find_tools for it; matches become callable. To change a Tool, read_tool it first and create a new version (same name) from its real code. Test a draft with test_tool on real input before saving it, then call the saved Tool. Shared JS helpers (parsers, encoders, hashing, URL handling) belong in modules via create_module, loaded inside Tools with requireModule(name). To delegate a focused task to a sub-agent with the same Tools, build a Tool with the \"agent\" Capability, e.g. `function run(i){ return agents.run(i.task, {model: i.model}); }`; several such calls in one turn run in parallel.\n\
Make Tools generic: a Tool does one kind of thing (read_text_file, http_get_json, csv_column_sum), and everything task- or project-specific - paths, URLs, hosts, queries, names, column/field names - comes in through its input. Never hardcode such values in implementation_js, the Tool name, or its description. If an existing Tool nearly fits, create a more general new version of it (existing input fields keep working, new ones optional) rather than a new narrow Tool. Specialise only when the logic itself is inherently specific to one project.\n\
", host_api_doc!(), "\n\
Stream anything large (fs.readChunks / fetchChunks / fs.append / emit); never load it whole. Declare only the Capabilities a Tool needs, as strings like \"net\", \"net:example.com\", \"fs:read:/abs/path\", \"fs:write:/abs/path\", input-bound scopes \"fs:read:$input.path\", \"fs:write:$input.out\", \"net:$input.url\" (granted per call for exactly the path(s)/host named by that input field - string or array of strings - so a generic Tool stays least-privilege; prefer these over fixed project paths), \"env:KEY\", \"process:cmd\", \"clipboard:read\", \"clipboard:write\", \"notify\", \"llm\", \"agent\" (or \"llm:model\"/\"agent:model\"). Prefer retiring a broken Tool and creating a new version over silent workarounds.");

const SUMMARIZE_PROMPT: &str = "Summarise the following conversation excerpt \
faithfully and compactly, preserving facts, decisions, names, and open \
threads. Output only the summary.";

fn url(base: &str, path: &str) -> String {
    format!("{}/{path}", base.trim_end_matches('/'))
}

fn apply_headers(
    mut req: reqwest::RequestBuilder,
    cfg: &ProviderCfg,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    for (k, v) in &cfg.extra_headers {
        req = req.header(k, v);
    }
    req
}

pub struct Provider {
    cfg: ProviderCfg,
    api_key: String,
    /// When set, credentials are re-resolved before each HTTP request so
    /// an OAuth token expiring during a long Run is refreshed in place.
    credential_paths: Option<Paths>,
    http: reqwest::Client,
}

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
    pub dropped_reasoning_effort: bool,
    /// Model that actually served the request (failover routing).
    pub model_used: String,
    /// Output stopped at the token limit (`finish_reason: "length"`): the
    /// answer or the tool-call arguments are cut off.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    Content,
    Reasoning,
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
                    if let Some(t) = c["text"].as_str() {
                        out.push_str(t);
                    }
                }
            }
        }
    }
    out
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

impl Provider {
    pub fn new(cfg: ProviderCfg, api_key: String) -> Result<Self> {
        let http = reqwest::Client::builder().build()?;
        Ok(Self {
            cfg,
            api_key,
            credential_paths: None,
            http,
        })
    }

    /// Construct a provider whose credential is resolved from a profile
    /// before every request. The initial resolution also fails fast for a
    /// missing OAuth login rather than deferring that to the first POST.
    pub async fn for_profile(cfg: ProviderCfg, paths: Paths) -> Result<Self> {
        let api_key = crate::auth::resolve_provider_token(&paths, &cfg).await?;
        let http = reqwest::Client::builder().build()?;
        Ok(Self {
            cfg,
            api_key,
            credential_paths: Some(paths),
            http,
        })
    }

    async fn credential(&self) -> Result<String> {
        match &self.credential_paths {
            Some(paths) => crate::auth::resolve_provider_token(paths, &self.cfg).await,
            None => Ok(self.api_key.clone()),
        }
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let credential = self.credential().await?;
        let req = apply_headers(
            self.http.get(url(&self.cfg.base_url, "models")),
            &self.cfg,
            &credential,
        );
        let resp = req.send().await.context("GET /models")?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!(
                "GET /models failed: {status}: {}",
                resp.text().await.unwrap_or_default()
            ));
        }
        let body: Value = resp.json().await.context("parse /models")?;
        Ok(parse_models(&body))
    }

    /// Non-streaming completion (compaction, probe helpers).
    pub async fn complete(
        &self,
        messages: &[Value],
        model: &str,
        effort: Option<&str>,
    ) -> Result<String> {
        let mut body = if self.cfg.responses() {
            json!({"model": model, "input": responses_input(messages), "stream": false})
        } else {
            json!({"model": model, "messages": messages, "stream": false})
        };
        let resp = self.post_chat(&mut body, effort).await?;
        let v: Value = resp.json().await.context("parse chat completion")?;
        if self.cfg.responses() {
            return Ok(responses_text(&v));
        }
        Ok(v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// POST /chat/completions with bounded retries (SPEC §4.4). Returns the
    /// raw response on success. If a 400 looks like an unknown-parameter
    /// rejection and reasoning_effort was set, retries once without it and
    /// flags `dropped` via the returned marker in ChatResult paths.
    async fn post_chat(&self, body: &mut Value, effort: Option<&str>) -> Result<reqwest::Response> {
        self.post_chat_flagged(body, effort)
            .await
            .map(|(r, _, _)| r)
    }

    async fn post_chat_flagged(
        &self,
        body: &mut Value,
        effort: Option<&str>,
    ) -> Result<(reqwest::Response, bool, String)> {
        self.post_provider(body, effort, true).await
    }

    async fn post_provider(
        &self,
        body: &mut Value,
        effort: Option<&str>,
        allow_failover: bool,
    ) -> Result<(reqwest::Response, bool, String)> {
        let endpoint = if self.cfg.responses() {
            "responses"
        } else {
            "chat/completions"
        };
        let requested = body["model"].as_str().unwrap_or_default().to_string();
        let mut models = vec![requested.clone()];
        if allow_failover {
            for m in &self.cfg.fallback_models {
                if !m.trim().is_empty() && !models.contains(m) {
                    models.push(m.clone());
                }
            }
        }
        let mut dropped = false;
        let mut last_err = String::new();
        for model in &models {
            let mut with_effort = effort.is_some();
            for attempt in 0..=self.cfg.max_retries {
                let mut b = body.clone();
                b["model"] = json!(model);
                if with_effort {
                    if self.cfg.responses() {
                        b["reasoning"] = json!({"effort": effort.unwrap()});
                    } else {
                        b["reasoning_effort"] = json!(effort.unwrap());
                    }
                }
                let credential = self.credential().await?;
                let req = apply_headers(
                    self.http.post(url(&self.cfg.base_url, endpoint)),
                    &self.cfg,
                    &credential,
                )
                .json(&b);
                match req.send().await {
                    Ok(resp) => {
                        let status = resp.status();
                        if status.is_success() {
                            return Ok((resp, dropped, model.clone()));
                        }
                        let text = resp.text().await.unwrap_or_default();
                        if status.as_u16() == 400 && with_effort && is_unknown_param(&text) {
                            with_effort = false;
                            dropped = true;
                            continue;
                        }
                        last_err = format!("{status}: {text}");
                        if is_model_error(status.as_u16(), &text) && models.len() > 1 {
                            break; // fail over to the next model
                        }
                        if !(status.as_u16() == 429 || status.is_server_error()) {
                            return Err(anyhow!("provider error: {last_err}"));
                        }
                    }
                    Err(e) => last_err = e.to_string(),
                }
                if attempt < self.cfg.max_retries {
                    tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt as u32))).await;
                }
            }
            // Retryable failures exhausted for this model: fail over.
            if !last_err.is_empty() && models.len() > 1 {
                tracing::warn!("provider model {model} failed ({last_err}); failing over");
            }
        }
        Err(anyhow!("provider request failed after retries: {last_err}"))
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
        mut on_delta: F,
    ) -> Result<ChatResult> {
        // Prompt-based fallback: describe tools in-band, send none natively,
        // and buffer content so tool-call JSON never streams to the user.
        let prompt_mode = self.cfg.prompt_tools() && !tools.is_empty();
        let augmented: Vec<Value>;
        let messages: &[Value] = if prompt_mode {
            let instr = prompt_tool_instruction(tools);
            let mut msgs: Vec<Value> = messages.to_vec();
            if let Some(first) = msgs.first_mut() {
                if first["role"].as_str() == Some("system") {
                    let cur = first["content"].as_str().unwrap_or_default().to_string();
                    first["content"] = json!(format!("{cur}{instr}"));
                    augmented = msgs;
                    &augmented
                } else {
                    msgs.insert(0, json!({"role": "system", "content": instr.trim_start()}));
                    augmented = msgs;
                    &augmented
                }
            } else {
                augmented = vec![json!({"role": "system", "content": instr.trim_start()})];
                &augmented
            }
        } else {
            messages
        };
        let effective_tools: &[Value] = if prompt_mode { &[] } else { tools };
        let mut body = if self.cfg.responses() {
            let mut b = json!({"model": model, "input": responses_input(messages), "stream": true});
            if !effective_tools.is_empty() {
                b["tools"] = json!(responses_tools(effective_tools));
            }
            b
        } else {
            let mut b = json!({"model": model, "messages": messages, "stream": true});
            if !effective_tools.is_empty() {
                b["tools"] = json!(effective_tools);
            }
            b
        };
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut truncated = false;
        let mut attempt = 0;
        let (dropped, model_used) = 'request: loop {
            // Waiting for the response headers is cancellable too (a slow or
            // stuck endpoint must not outlive an interrupt).
            let (resp, dropped, model_used) = tokio::select! {
                _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                r = self.post_chat_flagged(&mut body, effort) => r?,
            };
            let mut stream = resp.bytes_stream();
            // Raw bytes: a chunk boundary can split a multibyte character, so
            // only complete lines are decoded.
            let mut buf: Vec<u8> = Vec::new();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                    next = stream.next() => {
                        let Some(chunk) = next else { break };
                        let chunk = chunk.context("stream read")?;
                        buf.extend_from_slice(&chunk);
                        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                            let raw: Vec<u8> = buf.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&raw);
                            let line = line.trim();
                            if let Some(data) = line.strip_prefix("data:") {
                                let data = data.trim();
                                if data == "[DONE]" { continue; }
                                if let Ok(v) = serde_json::from_str::<Value>(data) {
                                    // Mid-stream failures: Responses `error` / `response.failed` /
                                    // `response.incomplete` events, or a Chat Completions `{"error": ...}`
                                    // chunk. Retry only before anything streamed, so the user never sees
                                    // duplicated output; otherwise the Run fails loudly (SPEC §4.4).
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
                                            tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt as u32))).await;
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
                                            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                                                if let Some(d) = v["delta"].as_str() {
                                                    reasoning.push_str(d);
                                                    on_delta(DeltaKind::Reasoning, d);
                                                }
                                            }
                                            "response.output_item.added" | "response.output_item.done" => {
                                                let item = &v["item"];
                                                if item["type"].as_str() == Some("function_call") {
                                                    let idx = v["output_index"].as_u64().unwrap_or(tool_calls.len() as u64) as usize;
                                                    while tool_calls.len() <= idx {
                                                        tool_calls.push(ToolCall::default());
                                                    }
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
                                                let idx = v["output_index"].as_u64().unwrap_or(tool_calls.len() as u64) as usize;
                                                while tool_calls.len() <= idx {
                                                    tool_calls.push(ToolCall::default());
                                                }
                                                if let Some(d) = v["delta"].as_str() {
                                                    tool_calls[idx].arguments.push_str(d);
                                                }
                                            }
                                            "response.completed" => {
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
                                    if v["choices"][0]["finish_reason"].as_str() == Some("length") {
                                        truncated = true;
                                    }
                                    let delta = &v["choices"][0]["delta"];
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
                                            let idx = tc["index"].as_u64().unwrap_or(tool_calls.len() as u64) as usize;
                                            while tool_calls.len() <= idx {
                                                tool_calls.push(ToolCall::default());
                                            }
                                            let slot = &mut tool_calls[idx];
                                            if let Some(id) = tc["id"].as_str() {
                                                if !id.is_empty() { slot.id = id.to_string(); }
                                            }
                                            if let Some(n) = tc["function"]["name"].as_str() {
                                                slot.name.push_str(n);
                                            }
                                            if let Some(a) = tc["function"]["arguments"].as_str() {
                                                slot.arguments.push_str(a);
                                            }
                                        }
                                    }
                                    // Non-streaming-shaped servers may put message.tool_calls in one chunk.
                                    if let Some(tcs) = v["choices"][0]["message"]["tool_calls"].as_array() {
                                        for tc in tcs {
                                            tool_calls.push(ToolCall {
                                                id: tc["id"].as_str().unwrap_or_default().to_string(),
                                                name: tc["function"]["name"].as_str().unwrap_or_default().to_string(),
                                                arguments: tc["function"]["arguments"].as_str().unwrap_or_default().to_string(),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            break (dropped, model_used);
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
        for (i, tc) in tool_calls.iter_mut().enumerate() {
            if tc.id.is_empty() {
                tc.id = format!("call_{i}");
            }
        }
        Ok(ChatResult {
            content,
            reasoning,
            tool_calls,
            dropped_reasoning_effort: dropped,
            model_used,
            truncated,
        })
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

    pub async fn summarize(&self, text: &str, model: &str) -> Result<String> {
        self.complete(
            &[
                json!({"role": "system", "content": SUMMARIZE_PROMPT}),
                json!({"role": "user", "content": text}),
            ],
            model,
            None,
        )
        .await
    }
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

fn is_model_error(status: u16, text: &str) -> bool {
    if status == 404 {
        return true;
    }
    let t = text.to_lowercase();
    t.contains("model")
        && (t.contains("not found")
            || t.contains("does not exist")
            || t.contains("not available")
            || t.contains("invalid model"))
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
    let tool = json!({
        "type": "function",
        "function": {
            "name": "probe_ping",
            "description": "Connectivity probe. Call this.",
            "parameters": {"type": "object", "properties": {}}
        }
    });
    if cfg.prompt_tools() {
        let ok = probe_prompt_tool_call(&p, model, &tool).await?;
        return Ok(ProbeResult {
            models,
            streaming: true,
            tool_calling: ok,
            prompt_fallback: ok,
        });
    }
    let native = match probe_tool_call(&p, model, &tool, true).await {
        Ok(v) => Ok(v),
        Err(e) if is_tool_choice_rejection(&e.to_string()) => {
            probe_tool_call(&p, model, &tool, false).await
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
            let ok = probe_prompt_tool_call(&p, model, &tool)
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
            if probe_prompt_tool_call(&p, model, &tool)
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

pub fn probe_blocking(cfg: &ProviderCfg, api_key: &str, model: &str) -> Result<ProbeResult> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(probe(cfg, api_key, model))
}

#[cfg(test)]
mod tests {
    use super::*;

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
