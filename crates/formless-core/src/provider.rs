use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::config::ProviderCfg;

/// Bootstrap system prompt (SPEC §7.7). Versioned; frozen in v1.
pub const SYSTEM_PROMPT: &str = "You are Formless, a general-purpose agent.\n\
You start with only `create_tool` (and `retire_tool`). Build small, single-purpose, minimally-capabled Tools on demand with create_tool, test each by calling it, and reuse Tools across tasks. Never expect a Tool to exist - build it.\n\
Tool code is plain JavaScript defining `function run(input) { ... }` returning the output value. Host API (all Capability-gated): fetch(url, opts) -> {status, ok, body, text(), json()}; fetchChunks(url, opts, size, cb); fs.read(path), fs.write(path, string), fs.exists(path), fs.list(dir), fs.readChunks(path, size, cb), fs.readLines(path, cb); env.get(key); process.run(cmd, argsArray) -> {code, stdout, stderr}; tools.call(name, input); clipboard.read(), clipboard.write(text); notify(title, body); llm.complete(prompt, {system, model}) -> text; emit(chunk) streams large output to an artifact; console.log, sleep(ms), setTimeout, cryptoRandom(). Nothing else exists.\n\
Stream anything large (fs.readChunks / fetchChunks / emit); never load it whole. Declare only the Capabilities a Tool needs, as strings like \"net\", \"net:example.com\", \"fs:read:/abs/path\", \"fs:write:/abs/path\", \"env:KEY\", \"process:cmd\", \"clipboard:read\", \"clipboard:write\", \"notify\", \"llm\" (or \"llm:model\"). Prefer retiring a broken Tool and creating a new version over silent workarounds.";

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    Content,
    Reasoning,
}

impl Provider {
    pub fn new(cfg: ProviderCfg, api_key: String) -> Result<Self> {
        let http = reqwest::Client::builder().build()?;
        Ok(Self { cfg, api_key, http })
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let req = apply_headers(
            self.http.get(url(&self.cfg.base_url, "models")),
            &self.cfg,
            &self.api_key,
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
        let mut body = json!({"model": model, "messages": messages, "stream": false});
        let resp = self.post_chat(&mut body, effort).await?;
        let v: Value = resp.json().await.context("parse chat completion")?;
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
        self.post_chat_flagged(body, effort).await.map(|(r, _)| r)
    }

    async fn post_chat_flagged(
        &self,
        body: &mut Value,
        effort: Option<&str>,
    ) -> Result<(reqwest::Response, bool)> {
        let mut dropped = false;
        let mut with_effort = effort.is_some();
        let mut last_err = String::new();
        for attempt in 0..=self.cfg.max_retries {
            let mut b = body.clone();
            if with_effort {
                b["reasoning_effort"] = json!(effort.unwrap());
            }
            let req = apply_headers(
                self.http.post(url(&self.cfg.base_url, "chat/completions")),
                &self.cfg,
                &self.api_key,
            )
            .json(&b);
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok((resp, dropped));
                    }
                    let text = resp.text().await.unwrap_or_default();
                    if status.as_u16() == 400 && with_effort && is_unknown_param(&text) {
                        with_effort = false;
                        dropped = true;
                        continue;
                    }
                    last_err = format!("{status}: {text}");
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
        let mut body = json!({"model": model, "messages": messages, "stream": true});
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        let (resp, dropped) = self.post_chat_flagged(&mut body, effort).await?;
        let mut stream = resp.bytes_stream();
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut buf = String::new();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(anyhow!("interrupted")),
                next = stream.next() => {
                    let Some(chunk) = next else { break };
                    let chunk = chunk.context("stream read")?;
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(pos) = buf.find('\n') {
                        let line: String = buf.drain(..=pos).collect();
                        let line = line.trim();
                        if let Some(data) = line.strip_prefix("data:") {
                            let data = data.trim();
                            if data == "[DONE]" { continue; }
                            if let Ok(v) = serde_json::from_str::<Value>(data) {
                                let delta = &v["choices"][0]["delta"];
                                if let Some(d) = delta["content"].as_str() {
                                    if !d.is_empty() {
                                        content.push_str(d);
                                        on_delta(DeltaKind::Content, d);
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
        })
    }

    /// Server-side compaction (SPEC §11): POST {base_url}/compact with
    /// {model, input}; accepts {summary}|{output}|{content} responses.
    pub async fn compact_server(&self, text: &str, model: &str) -> Result<String> {
        let body = json!({"model": model, "input": text});
        let req = apply_headers(
            self.http.post(url(&self.cfg.base_url, "compact")),
            &self.cfg,
            &self.api_key,
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

fn is_unknown_param(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("reasoning_effort")
        || t.contains("unknown")
        || t.contains("unrecognized")
        || t.contains("unsupported")
}

#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub models: Vec<String>,
    pub streaming: bool,
    pub tool_calling: bool,
}

fn is_tool_choice_rejection(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("tool_choice") && (t.contains("support") || t.contains("thinking"))
}

async fn probe_tool_call(p: &Provider, model: &str, tool: &Value, forced: bool) -> Result<bool> {
    let mut body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Call the probe_ping tool now."}],
        "tools": [tool],
        "stream": false
    });
    if forced {
        body["tool_choice"] = json!("required");
    }
    let (resp, _) = p.post_chat_flagged(&mut body, None).await?;
    let v: Value = resp.json().await.context("parse probe response")?;
    Ok(v["choices"][0]["message"]["tool_calls"]
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false))
}

/// Probe (SPEC §4.3): connectivity via /models, then a required tool-call.
/// Thinking-mode endpoints reject a forced `tool_choice`, so on that
/// specific 400 the probe retries with `tool_choice` omitted (the model
/// may still call the tool voluntarily). Errors only on connectivity;
/// tool-calling support is reported in the result.
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
    let tool_calling = match probe_tool_call(&p, model, &tool, true).await {
        Ok(v) => v,
        Err(e) if is_tool_choice_rejection(&e.to_string()) => {
            probe_tool_call(&p, model, &tool, false).await?
        }
        Err(e) => return Err(e),
    };
    Ok(ProbeResult {
        models,
        streaming: true,
        tool_calling,
    })
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
