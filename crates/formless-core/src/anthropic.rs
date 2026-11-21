//! The native Anthropic Messages API (`api_style = "anthropic"`, SPEC
//! §4.1): Chat-Completions-shaped messages become Messages requests, and
//! the event stream is folded back into text, thinking, tool calls and
//! usage.

use serde_json::{json, Value};

/// The `anthropic-version` header the requests are written for.
pub const VERSION: &str = "2023-06-01";

/// Output budget when nothing else asks for one.
pub const MAX_TOKENS: u64 = 16_384;

/// Largest image the API accepts inline (base64-decoded bytes).
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Image media types the API accepts.
const IMAGE_TYPES: [&str; 4] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Smallest thinking budget the API accepts.
const MIN_THINKING: u64 = 1_024;

/// Extended-thinking budget for a reasoning effort (None = no thinking).
pub fn thinking_budget(effort: Option<&str>) -> Option<u64> {
    match effort?.trim().to_ascii_lowercase().as_str() {
        "minimal" => Some(1_024),
        "low" => Some(4_096),
        "medium" => Some(12_000),
        "high" => Some(32_000),
        _ => None,
    }
}

/// A model id without a router prefix (`anthropic/claude-…`), lowercased.
fn bare(model: &str) -> String {
    let m = model.to_ascii_lowercase();
    m.rsplit('/').next().unwrap_or_default().to_string()
}

/// Output caps the endpoint's `/models` listing reported (`max_tokens`
/// per model), preferred over the built-in table.
static LISTED_CAPS: std::sync::Mutex<Option<std::collections::HashMap<String, u64>>> =
    std::sync::Mutex::new(None);

/// Remember the output caps a `/models` listing reported.
pub fn note_output_caps(caps: impl IntoIterator<Item = (String, u64)>) {
    let mut g = LISTED_CAPS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(Default::default).extend(caps);
}

/// The most output tokens `model` may be asked for: the listing's own
/// figure, else the documented cap of its family (None = unknown).
pub fn output_cap(model: &str) -> Option<u64> {
    let listed = LISTED_CAPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|caps| caps.get(model).copied());
    listed.or_else(|| family_output_cap(&bare(model)))
}

fn family_output_cap(m: &str) -> Option<u64> {
    let caps: [(&str, u64); 9] = [
        ("claude-3-haiku", 4_096),
        ("claude-3-opus", 4_096),
        ("claude-3-sonnet", 4_096),
        ("claude-3-5-haiku", 8_192),
        ("claude-3-5-sonnet", 8_192),
        ("claude-3-7-sonnet", 64_000),
        ("claude-opus-4-0", 32_000),
        ("claude-opus-4-1", 32_000),
        ("claude-opus-4-2", 32_000),
    ];
    if let Some((_, c)) = caps.iter().find(|(p, _)| m.starts_with(p)) {
        return Some(*c);
    }
    // `claude-opus-4-20250514`: the 4.0 release under its dated id.
    if m.starts_with("claude-opus-4-2025") {
        return Some(32_000);
    }
    if m.starts_with("claude-sonnet-4")
        || m.starts_with("claude-haiku-4")
        || m.starts_with("claude-opus-4-5")
    {
        return Some(64_000);
    }
    None
}

/// Does `model` accept extended thinking? Every Claude model before 3.7
/// rejects it; unknown models are assumed to (a rejection is retried
/// without it).
pub fn supports_thinking(model: &str) -> bool {
    let m = bare(model);
    let before_3_7 = (m.starts_with("claude-3-") && !m.starts_with("claude-3-7"))
        || m.starts_with("claude-2")
        || m.starts_with("claude-instant");
    !before_3_7
}

/// `data:image/png;base64,…` or an http(s) URL → an image block; an
/// image the API would reject (type or size) → why it was left out.
fn image_block(url: &str) -> Result<Value, String> {
    let (scheme, body) = url.split_once(':').unwrap_or_default();
    if (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && body.starts_with("//")
    {
        return Ok(json!({"type": "image", "source": {"type": "url", "url": url}}));
    }
    let invalid = || "image not sent: not an inline image or http(s) URL".to_string();
    if !scheme.eq_ignore_ascii_case("data") {
        return Err(invalid());
    }
    let (metadata, data) = body.split_once(',').ok_or_else(invalid)?;
    let (media, encoding) = metadata.rsplit_once(';').ok_or_else(invalid)?;
    if !encoding.eq_ignore_ascii_case("base64") {
        return Err(invalid());
    }
    let media = media.to_ascii_lowercase();
    if !IMAGE_TYPES.contains(&media.as_str()) {
        return Err(format!(
            "image not sent: {media} is not supported by this API (JPEG, PNG, GIF or WebP only)"
        ));
    }
    // base64 → bytes is 3/4 of the length.
    let bytes = data.len() / 4 * 3;
    if bytes > MAX_IMAGE_BYTES {
        return Err(format!(
            "image not sent: {} MB is over this API's 5 MB limit",
            bytes.div_ceil(1024 * 1024)
        ));
    }
    Ok(json!({"type": "image", "source": {"type": "base64", "media_type": media, "data": data}}))
}

/// A user/assistant message's content as Anthropic blocks. Images the
/// API would reject become a short text line instead (never a 400).
fn content_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) if !s.is_empty() => vec![json!({"type": "text", "text": s})],
        Value::Array(parts) => {
            parts
                .iter()
                .filter_map(|p| match p["type"].as_str() {
                    Some("text") => p["text"]
                        .as_str()
                        .filter(|t| !t.is_empty())
                        .map(|t| json!({"type": "text", "text": t})),
                    Some("image_url") => {
                        let url = p["image_url"]["url"]
                            .as_str()
                            .or_else(|| p["image_url"].as_str())?;
                        Some(image_block(url).unwrap_or_else(
                            |why| json!({"type": "text", "text": format!("[{why}]")}),
                        ))
                    }
                    Some("file")=>p["file"]["file_data"].as_str()
                        .and_then(|s|s.strip_prefix("data:application/pdf;base64,"))
                        .map(|data|json!({"type":"document","source":{"type":"base64","media_type":"application/pdf","data":data}})),
                    _ => None,
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Images a request leaves out (unsupported type or size), as notes for
/// the user.
pub fn skipped_images(messages: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for m in messages {
        for p in m["content"].as_array().into_iter().flatten() {
            if p["type"] == "image_url" {
                let url = p["image_url"]["url"]
                    .as_str()
                    .or_else(|| p["image_url"].as_str())
                    .unwrap_or_default();
                if let Err(why) = image_block(url) {
                    if !out.contains(&why) {
                        out.push(why);
                    }
                }
            }
        }
    }
    out
}

/// Chat-Completions messages and tools → `(system, messages, tools)`.
/// System messages join the top-level `system`; `tool` results become
/// `tool_result` blocks on a user turn; consecutive turns of one role
/// merge (the API wants them alternating), with a user turn's
/// `tool_result` blocks first, as the API requires after `tool_use` (a
/// steer or note stored between a call and its result follows them).
/// With `cache`, breakpoints go on the end of the system prompt and of
/// the last message, the stable prefix (§4.1).
pub fn request(
    messages: &[Value],
    tools: &[Value],
    cache: bool,
) -> (Value, Vec<Value>, Vec<Value>) {
    let mut system: Vec<Value> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    let mut push = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last["role"] == role => {
                last["content"].as_array_mut().unwrap().extend(blocks);
            }
            _ => out.push(json!({"role": role, "content": blocks})),
        }
    };
    for m in messages {
        match m["role"].as_str().unwrap_or("user") {
            "system" => system.extend(content_blocks(&m["content"])),
            "tool" => {
                let mut result = json!({
                    "type": "tool_result",
                    "tool_use_id": m["tool_call_id"],
                    "content": m["content"].as_str().unwrap_or_default(),
                });
                if m["_is_error"] == true {
                    result["is_error"] = json!(true);
                }
                push("user", vec![result]);
            }
            "assistant" => {
                // Thinking blocks first, signatures intact: required back on
                // tool-use turns.
                let mut blocks: Vec<Value> =
                    m["thinking_blocks"].as_array().cloned().unwrap_or_default();
                blocks.extend(content_blocks(&m["content"]));
                for tc in m["tool_calls"].as_array().into_iter().flatten() {
                    let args = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc["id"],
                        "name": tc["function"]["name"],
                        "input": serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
                    }));
                }
                push("assistant", blocks);
            }
            _ => push("user", content_blocks(&m["content"])),
        }
    }
    for turn in out.iter_mut().filter(|t| t["role"] == "user") {
        let blocks = turn["content"].as_array_mut().unwrap();
        // Stable: parallel results keep their order, as does the text.
        blocks.sort_by_key(|b| b["type"] != "tool_result");
    }
    // The first turn must be the user's.
    if out.first().is_some_and(|m| m["role"] == "assistant") {
        out.insert(
            0,
            json!({"role": "user", "content": [{"type": "text", "text": "(continue)"}]}),
        );
    }
    if cache {
        if let Some(b) = system.last_mut() {
            b["cache_control"] = json!({"type": "ephemeral"});
        }
        if let Some(b) = out
            .last_mut()
            .and_then(|m| m["content"].as_array_mut())
            .and_then(|c| c.last_mut())
        {
            b["cache_control"] = json!({"type": "ephemeral"});
        }
    }
    let tools = tools
        .iter()
        .map(|t| {
            let f = &t["function"];
            json!({
                "name": f["name"],
                "description": f["description"],
                "input_schema": if f["parameters"].is_object() { f["parameters"].clone() } else { json!({"type": "object"}) },
            })
        })
        .collect();
    (Value::Array(system), out, tools)
}

/// The full request body. `max_tokens` stays within the model's output
/// cap; thinking is asked for only from models that support it, with a
/// budget that fits under that cap (else not at all).
pub fn body(
    model: &str,
    messages: &[Value],
    tools: &[Value],
    cache: bool,
    effort: Option<&str>,
    stream: bool,
) -> Value {
    let (system, msgs, tools) = request(messages, tools, cache);
    let cap = output_cap(model);
    let mut max = cap.map_or(MAX_TOKENS, |c| c.min(MAX_TOKENS));
    let mut thinking = None;
    if let Some(budget) = thinking_budget(effort).filter(|_| supports_thinking(model)) {
        // The answer needs room beyond the thinking.
        let wanted = (budget + 8_192).max(MAX_TOKENS);
        max = cap.map_or(wanted, |c| wanted.min(c));
        let budget = budget.min(max.saturating_sub(MIN_THINKING));
        if budget >= MIN_THINKING {
            thinking = Some(budget);
        }
    }
    let mut b = json!({"model": model, "messages": msgs, "max_tokens": max, "stream": stream});
    if system.as_array().is_some_and(|s| !s.is_empty()) {
        b["system"] = system;
    }
    if !tools.is_empty() {
        b["tools"] = Value::Array(tools);
    }
    if let Some(budget) = thinking {
        b["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
    }
    b
}

/// How a 400 was worked around: the body was changed, and the request
/// can be sent once more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Replayed thinking (signatures) removed from the history.
    ReplayedThinking,
    /// Extended thinking turned off.
    Thinking,
    /// `max_tokens` lowered to the limit the error named.
    MaxTokens,
    /// The thinking budget lowered under `max_tokens`.
    ThinkingBudget,
    /// `cache_control` breakpoints removed.
    CacheControl,
    /// Image blocks replaced with a text line.
    Images,
}

impl Recovery {
    /// What the user is told.
    pub fn note(self) -> &'static str {
        match self {
            Self::ReplayedThinking => {
                "The provider rejected the replayed thinking from earlier turns; resent without it."
            }
            Self::Thinking => "The model does not accept extended thinking; resent without it.",
            Self::MaxTokens => {
                "The provider rejected the output limit; resent with the model's maximum."
            }
            Self::ThinkingBudget => {
                "The thinking budget did not fit under the output limit; resent with a smaller one."
            }
            Self::CacheControl => {
                "The provider rejected prompt-cache breakpoints; resent without them."
            }
            Self::Images => "The provider rejected an image; resent without images.",
        }
    }
}

/// Does any block of the body's messages satisfy `f`?
fn any_block(body: &Value, f: impl Fn(&Value) -> bool) -> bool {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .any(f)
}

fn is_thinking_block(b: &Value) -> bool {
    matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking"))
}

/// Remove `cache_control` everywhere in `v`; true when there was one.
pub fn strip_cache_control(v: &mut Value) -> bool {
    match v {
        Value::Object(o) => {
            let mut found = o.remove("cache_control").is_some();
            for x in o.values_mut() {
                found |= strip_cache_control(x);
            }
            found
        }
        Value::Array(a) => a.iter_mut().fold(false, |f, x| strip_cache_control(x) | f),
        _ => false,
    }
}

/// Remove only cache annotations in actual request slots. In particular,
/// JSON schemas may legitimately have cache_control keys in constants/defaults.
pub(crate) fn strip_request_cache_control(body: &mut Value) -> bool {
    fn remove(value: &mut Value) -> bool {
        value
            .as_object_mut()
            .is_some_and(|o| o.remove("cache_control").is_some())
    }
    let mut found = remove(body);
    if let Some(messages) = body.as_array_mut() {
        for message in messages {
            found |= remove(message);
        }
    }
    for key in ["tools", "system"] {
        if let Some(values) = body.get_mut(key).and_then(Value::as_array_mut) {
            for value in values {
                found |= remove(value);
            }
        }
    }
    crate::tokens::content_parts_mut(body, |parts| {
        for part in parts {
            found |= remove(part);
        }
    });
    found
}

/// The limit a `max_tokens` rejection names (`max_tokens: 40192 >
/// 8192, which is the maximum …`).
fn named_limit(text: &str) -> Option<u64> {
    let after = &text[text.find('>')? + 1..];
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|n| *n > 0)
}

/// Change `body` so a request rejected with this 400 can be sent again:
/// each fix applies at most once, since it removes what it targets.
pub fn recover(body: &mut Value, error: &str) -> Option<Recovery> {
    let lower = error.to_ascii_lowercase();
    if lower.contains("signature") || (lower.contains("thinking") && lower.contains("block")) {
        if any_block(body, is_thinking_block) {
            for m in body["messages"].as_array_mut().into_iter().flatten() {
                if let Some(c) = m["content"].as_array_mut() {
                    c.retain(|b| !is_thinking_block(b));
                }
            }
            return Some(Recovery::ReplayedThinking);
        }
    }
    // "`max_tokens` must be greater than `thinking.budget_tokens`": fit the
    // budget under the output limit and keep thinking (#1089).
    if lower.contains("budget_tokens") && lower.contains("max_tokens") {
        let max = body["max_tokens"].as_u64().unwrap_or(MAX_TOKENS);
        let budget = body["thinking"]["budget_tokens"].as_u64()?;
        let fitted = max.saturating_sub(MIN_THINKING).min(budget);
        if fitted >= MIN_THINKING && fitted < budget {
            body["thinking"]["budget_tokens"] = json!(fitted);
            return Some(Recovery::ThinkingBudget);
        }
        body.as_object_mut()?.remove("thinking");
        return Some(Recovery::Thinking);
    }
    if lower.contains("thinking") && body.get("thinking").is_some() {
        body.as_object_mut()?.remove("thinking");
        return Some(Recovery::Thinking);
    }
    if lower.contains("max_tokens") {
        let current = body["max_tokens"].as_u64().unwrap_or(MAX_TOKENS);
        let limit = named_limit(error).unwrap_or(4_096);
        if limit < current {
            body["max_tokens"] = json!(limit);
            let budget = body["thinking"]["budget_tokens"].as_u64();
            if let Some(budget) = budget.filter(|b| *b >= limit) {
                let fitted = limit.saturating_sub(MIN_THINKING).min(budget);
                if fitted >= MIN_THINKING {
                    body["thinking"]["budget_tokens"] = json!(fitted);
                } else {
                    body.as_object_mut()?.remove("thinking");
                }
            }
            return Some(Recovery::MaxTokens);
        }
    }
    if lower.contains("cache_control") && strip_request_cache_control(body) {
        return Some(Recovery::CacheControl);
    }
    if lower.contains("image") && any_block(body, |b| b["type"] == "image") {
        for m in body["messages"].as_array_mut().into_iter().flatten() {
            for b in m["content"].as_array_mut().into_iter().flatten() {
                if b["type"] == "image" {
                    *b = json!({"type": "text", "text": "[image not sent: the provider rejected it]"});
                }
            }
        }
        return Some(Recovery::Images);
    }
    None
}

/// One streamed answer, folded from the event stream.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Folded {
    pub text: String,
    pub thinking: String,
    /// `thinking` / `redacted_thinking` blocks with their signatures, to
    /// send back on the next request of a tool-use turn.
    pub thinking_blocks: Vec<Value>,
    /// (id, name, arguments JSON) per tool_use block, in order.
    pub tool_calls: Vec<(String, String, String)>,
    pub truncated: bool,
    /// The final `stop_reason` (`end_turn`, `tool_use`, `max_tokens`,
    /// `refusal`, `pause_turn`, `model_context_window_exceeded`, …).
    pub stop_reason: Option<String>,
    /// A refusal's `stop_details` category, when given.
    pub stop_detail: Option<String>,
    /// `message_stop` arrived: the answer is complete.
    pub stopped: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// The stream's `error` event: (type, message).
    pub error: Option<(String, String)>,
    /// Block index → what it is, while it streams.
    blocks: Vec<Value>,
    /// Blocks started and not yet stopped.
    open_blocks: usize,
}

/// What one event added that the user should see streaming.
pub enum Delta<'a> {
    Text(&'a str),
    Thinking(&'a str),
    None,
}

impl Folded {
    /// Fold one event; returns the visible text it carried.
    pub fn event<'a>(&mut self, v: &'a Value) -> Delta<'a> {
        match v["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let u = &v["message"]["usage"];
                self.input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
                self.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                self.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            "content_block_start" => {
                let i = v["index"].as_u64().unwrap_or(0) as usize;
                if self.blocks.len() <= i {
                    self.blocks.resize(i + 1, Value::Null);
                }
                let mut b = v["content_block"].clone();
                if b["type"] == "tool_use" {
                    b["_json"] = json!("");
                }
                if b["type"] == "thinking" && b["thinking"].is_null() {
                    b["thinking"] = json!("");
                }
                self.blocks[i] = b;
                self.open_blocks += 1;
            }
            "content_block_delta" => {
                let i = v["index"].as_u64().unwrap_or(0) as usize;
                let d = &v["delta"];
                let Some(block) = self.blocks.get_mut(i) else {
                    return Delta::None;
                };
                match d["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or_default();
                        self.text.push_str(t);
                        return Delta::Text(t);
                    }
                    "thinking_delta" => {
                        let t = d["thinking"].as_str().unwrap_or_default();
                        let cur = block["thinking"].as_str().unwrap_or_default().to_string();
                        block["thinking"] = json!(cur + t);
                        self.thinking.push_str(t);
                        return Delta::Thinking(t);
                    }
                    "signature_delta" => {
                        let cur = block["signature"].as_str().unwrap_or_default().to_string();
                        block["signature"] =
                            json!(cur + d["signature"].as_str().unwrap_or_default());
                    }
                    "input_json_delta" => {
                        let cur = block["_json"].as_str().unwrap_or_default().to_string();
                        block["_json"] =
                            json!(cur + d["partial_json"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                self.open_blocks = self.open_blocks.saturating_sub(1);
                let i = v["index"].as_u64().unwrap_or(0) as usize;
                let Some(block) = self.blocks.get(i).cloned() else {
                    return Delta::None;
                };
                match block["type"].as_str().unwrap_or_default() {
                    "tool_use" => {
                        let args = block["_json"]
                            .as_str()
                            .filter(|a| !a.trim().is_empty())
                            .unwrap_or("{}");
                        self.tool_calls.push((
                            block["id"].as_str().unwrap_or_default().to_string(),
                            block["name"].as_str().unwrap_or_default().to_string(),
                            args.to_string(),
                        ));
                    }
                    "thinking" | "redacted_thinking" => self.thinking_blocks.push(block),
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.truncated = r == "max_tokens";
                    self.stop_reason = Some(r.to_string());
                }
                let details = &v["delta"]["stop_details"];
                if let Some(c) = details["category"]
                    .as_str()
                    .or_else(|| details["type"].as_str())
                {
                    self.stop_detail = Some(c.to_string());
                }
                if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                    self.output_tokens = n;
                }
            }
            "message_stop" => self.stopped = true,
            "error" => {
                self.error = Some((
                    v["error"]["type"].as_str().unwrap_or("error").to_string(),
                    v["error"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                ));
            }
            _ => {}
        }
        Delta::None
    }

    /// Why the stream is not a finished answer: it ended before
    /// `message_stop`, or with a content block still open (a tool call
    /// cut off mid-arguments). None when complete.
    pub fn incomplete(&self) -> Option<&'static str> {
        if !self.stopped {
            return Some("stream ended before message_stop");
        }
        (self.open_blocks > 0).then_some("stream ended with a content block still open")
    }

    /// Nothing the user saw yet: safe to re-send the request.
    pub fn nothing_streamed(&self) -> bool {
        self.text.is_empty() && self.thinking.is_empty() && self.tool_calls.is_empty()
    }
}

/// A non-streamed answer that must not be used as text: refused or cut
/// off at the output limit (a compaction summary from either would be
/// checkpointed as if whole).
pub fn unusable_stop(v: &Value) -> Option<String> {
    match v["stop_reason"].as_str()? {
        "refusal" => Some("the model refused the request".into()),
        "max_tokens" => Some("the answer was cut off at the output token limit".into()),
        "model_context_window_exceeded" => {
            Some("the request exceeded the model's context window".into())
        }
        _ => None,
    }
}

/// A non-streamed Messages response: (text, tool calls).
pub fn response_parts(v: &Value) -> (String, Vec<(String, String, String)>) {
    let mut text = String::new();
    let mut calls = Vec::new();
    for b in v["content"].as_array().into_iter().flatten() {
        match b["type"].as_str() {
            Some("text") => text.push_str(b["text"].as_str().unwrap_or_default()),
            Some("tool_use") => calls.push((
                b["id"].as_str().unwrap_or_default().to_string(),
                b["name"].as_str().unwrap_or_default().to_string(),
                b["input"].to_string(),
            )),
            _ => {}
        }
    }
    (text, calls)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_carry_system_tools_results_images_and_cache_points() {
        let msgs = vec![
            json!({"role": "system", "content": "You are Formless."}),
            json!({"role": "user", "content": [{"type": "text", "text": "look"}, {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}]}),
            json!({"role": "assistant", "content": null, "thinking_blocks": [{"type": "thinking", "thinking": "hm", "signature": "sig"}],
                   "tool_calls": [{"id": "t1", "type": "function", "function": {"name": "read", "arguments": "{\"p\":1}"}}]}),
            json!({"role": "tool", "tool_call_id": "t1", "content": "42"}),
            json!({"role": "system-note", "content": "x"}),
        ];
        let tools = vec![
            json!({"type": "function", "function": {"name": "read", "description": "Read", "parameters": {"type": "object"}}}),
        ];
        let b = body("claude-x", &msgs, &tools, true, Some("low"), true);
        assert_eq!(b["system"][0]["text"], "You are Formless.");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0]["content"][1]["type"], "image");
        assert_eq!(m[0]["content"][1]["source"]["media_type"], "image/png");
        assert_eq!(
            m[1]["content"][0]["signature"], "sig",
            "thinking echoed first"
        );
        assert_eq!(m[1]["content"][1]["input"]["p"], 1);
        assert_eq!(m[2]["role"], "user");
        assert_eq!(m[2]["content"][0]["type"], "tool_result");
        assert_eq!(m[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(
            m.last().unwrap()["content"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(b["thinking"]["budget_tokens"], 4096);
        assert!(b["max_tokens"].as_u64().unwrap() > 4096);
    }

    #[test]
    fn the_event_stream_folds_into_text_thinking_and_calls() {
        let events = [
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "cache_read_input_tokens": 7}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "plan"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "abc"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Hi"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "tu1", "name": "read"}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"p\":"}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "2}"}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 33}}),
        ];
        let mut f = Folded::default();
        for e in &events {
            f.event(e);
        }
        assert_eq!((f.text.as_str(), f.thinking.as_str()), ("Hi", "plan"));
        assert_eq!(
            f.tool_calls,
            [("tu1".into(), "read".into(), "{\"p\":2}".into())]
        );
        assert_eq!(f.thinking_blocks[0]["signature"], "abc");
        assert_eq!((f.input_tokens, f.output_tokens, f.cache_read), (10, 33, 7));
        assert!(!f.truncated);
        assert_eq!(thinking_budget(None), None);
    }

    #[test]
    fn output_caps_and_thinking_support_shape_the_body() {
        // A model capped at 8,192 that has no thinking: never above its cap.
        let b = body(
            "claude-3-5-haiku-20241022",
            &[json!({"role": "user", "content": "x"})],
            &[],
            false,
            Some("high"),
            true,
        );
        assert!(b["max_tokens"].as_u64().unwrap() <= 8_192);
        assert!(b.get("thinking").is_none(), "3.5 rejects thinking");
        // A thinking model whose listing reports a small cap: the budget fits under it.
        note_output_caps([("claude-test-8k".to_string(), 8_192)]);
        let b = body("claude-test-8k", &[], &[], false, Some("high"), true);
        let (max, budget) = (
            b["max_tokens"].as_u64().unwrap(),
            b["thinking"]["budget_tokens"].as_u64().unwrap(),
        );
        assert!(
            max <= 8_192 && budget < max && budget >= 1_024,
            "{max} {budget}"
        );
        // Unknown models keep the defaults.
        let b = body("claude-x", &[], &[], false, Some("high"), true);
        assert_eq!(b["max_tokens"], 40_192);
        assert!(
            !supports_thinking("anthropic/claude-3-opus") && supports_thinking("claude-3-7-sonnet")
        );
    }

    #[test]
    fn rejections_are_recovered_once_each() {
        let mut b = body(
            "claude-x",
            &[json!({"role": "user", "content": "x"})],
            &[],
            true,
            Some("high"),
            true,
        );
        let e = "max_tokens: 40192 > 8192, which is the maximum allowed number of output tokens";
        assert_eq!(recover(&mut b, e), Some(Recovery::MaxTokens));
        assert_eq!(b["max_tokens"], 8_192);
        assert!(b["thinking"]["budget_tokens"].as_u64().unwrap() < 8_192);
        assert_eq!(recover(&mut b, e), None, "already at the limit");
        assert_eq!(
            recover(&mut b, "cache_control not allowed"),
            Some(Recovery::CacheControl)
        );
        assert!(!b.to_string().contains("cache_control"));
        assert_eq!(recover(&mut b, "cache_control not allowed"), None);
        assert_eq!(
            recover(&mut b, "thinking: not supported"),
            Some(Recovery::Thinking)
        );
        assert_eq!(recover(&mut b, "thinking: not supported"), None);
        assert_eq!(recover(&mut b, "something else"), None);
        assert_eq!(
            recover(&mut b, "unsupported image source"),
            None,
            "image mentions alone never resend"
        );
        b["messages"][0]["content"] =
            json!([{"type":"image", "source":{"type":"url","url":"https://example.com/x.png"}}]);
        assert_eq!(
            recover(&mut b, "unsupported image source"),
            Some(Recovery::Images)
        );
        assert_eq!(
            recover(&mut b, "unsupported image source"),
            None,
            "already removed"
        );
    }

    /// Anthropic's real wordings each take their own branch (#1089).
    #[test]
    fn budget_errors_fit_thinking_instead_of_dropping_it() {
        let msgs = [json!({"role": "user", "content": "x"})];
        let mut b = body("claude-x", &msgs, &[], false, Some("high"), true);
        b["max_tokens"] = json!(16_000);
        b["thinking"]["budget_tokens"] = json!(16_000);
        let e = "max_tokens must be greater than thinking.budget_tokens. Please consult our documentation";
        assert_eq!(recover(&mut b, e), Some(Recovery::ThinkingBudget));
        let budget = b["thinking"]["budget_tokens"].as_u64().unwrap();
        assert!((MIN_THINKING..16_000).contains(&budget), "{budget}");

        let mut b = body("claude-x", &msgs, &[], false, Some("high"), true);
        assert_eq!(
            recover(&mut b, "thinking: Extra inputs are not permitted"),
            Some(Recovery::Thinking)
        );
        assert!(b.get("thinking").is_none());

        let replay = [
            json!({"role": "user", "content": "x"}),
            json!({"role": "assistant", "content": null,
                   "thinking_blocks": [{"type": "thinking", "thinking": "t", "signature": "bad"}],
                   "tool_calls": [{"id": "t", "type": "function", "function": {"name": "f", "arguments": "{}"}}]}),
            json!({"role": "tool", "tool_call_id": "t", "content": "1"}),
        ];
        let mut b = body("claude-x", &replay, &[], false, Some("high"), true);
        assert_eq!(
            recover(
                &mut b,
                "messages.1.content.0: Invalid `signature` in `thinking` block"
            ),
            Some(Recovery::ReplayedThinking)
        );
        assert!(b.get("thinking").is_some(), "thinking itself kept");
    }

    #[test]
    fn unsupported_images_become_text_and_urls_stay() {
        let big = "A".repeat(MAX_IMAGE_BYTES / 3 * 4 + 8);
        let parts = json!([
            {"type": "image_url", "image_url": {"url": "data:image/svg+xml;base64,AAAA"}},
            {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{big}")}},
            {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}},
            {"type": "image_url", "image_url": {"url": "data:image/JPEG;base64,AAAA"}},
        ]);
        let blocks = content_blocks(&parts);
        assert_eq!(blocks[0]["type"], "text");
        assert!(blocks[1]["text"].as_str().unwrap().contains("5 MB"));
        assert_eq!(blocks[2]["source"]["type"], "url");
        assert_eq!(blocks[3]["source"]["media_type"], "image/jpeg");
        assert_eq!(
            skipped_images(&[json!({"role": "user", "content": parts})]).len(),
            2
        );
    }

    #[test]
    fn image_schemes_and_base64_marker_are_ascii_case_insensitive() {
        let urls = [
            "HTTPS://example.com/Diagram.PNG",
            "Http://example.org/A.png",
        ];
        let parts = json!([
            {"type": "image_url", "image_url": {"url": urls[0]}},
            {"type": "image_url", "image_url": urls[1]},
            {"type": "image_url", "image_url": {"url": "DATA:image/png;base64,aAbB+/0="}},
            {"type": "image_url", "image_url": {"url": "data:IMAGE/PNG;BASE64,aAbB+/0="}},
            {"type": "image_url", "image_url": {"url": "DATA:image/svg+xml;BASE64,AAAA"}},
            {"type": "image_url", "image_url": {"url": "DATA:image/png,AAAA"}},
            {"type": "image_url", "image_url": {"url": "HTTPS:example.org/A.png"}},
        ]);
        let blocks = content_blocks(&parts);
        for (block, url) in blocks.iter().zip(urls) {
            assert_eq!(block["source"], json!({"type": "url", "url": url}));
        }
        for block in &blocks[2..4] {
            assert_eq!(
                block["source"],
                json!({"type": "base64", "media_type": "image/png", "data": "aAbB+/0="})
            );
        }
        assert!(blocks[4..].iter().all(|block| block["type"] == "text"));
    }

    #[test]
    fn incomplete_streams_and_stop_reasons_are_told_apart() {
        let mut f = Folded::default();
        f.event(&json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "t", "name": "x"}}));
        assert!(f.incomplete().is_some());
        f.event(&json!({"type": "message_stop"}));
        assert_eq!(
            f.incomplete(),
            Some("stream ended with a content block still open")
        );
        let mut f = Folded::default();
        f.event(&json!({"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_details": {"category": "cyber"}}}));
        f.event(&json!({"type": "message_stop"}));
        assert_eq!(
            (
                f.stop_reason.as_deref(),
                f.stop_detail.as_deref(),
                f.incomplete()
            ),
            (Some("refusal"), Some("cyber"), None)
        );
        assert!(unusable_stop(&json!({"stop_reason": "max_tokens"})).is_some());
        assert!(unusable_stop(&json!({"stop_reason": "end_turn"})).is_none());
    }
}
