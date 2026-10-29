//! The native Anthropic Messages API (`api_style = "anthropic"`, SPEC
//! §4.1): Chat-Completions-shaped messages become Messages requests, and
//! the event stream is folded back into text, thinking, tool calls and
//! usage.

use serde_json::{json, Value};

/// The `anthropic-version` header the requests are written for.
pub const VERSION: &str = "2023-06-01";

/// Output budget when nothing else asks for one.
pub const MAX_TOKENS: u64 = 16_384;

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

/// `data:image/png;base64,…` → an image block.
fn image_block(url: &str) -> Option<Value> {
    let rest = url.strip_prefix("data:")?;
    let (media, data) = rest.split_once(";base64,")?;
    Some(json!({"type": "image", "source": {"type": "base64", "media_type": media, "data": data}}))
}

/// A user/assistant message's content as Anthropic blocks.
fn content_blocks(content: &Value) -> Vec<Value> {
    match content {
        Value::String(s) if !s.is_empty() => vec![json!({"type": "text", "text": s})],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p["type"].as_str() {
                Some("text") => p["text"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(|t| json!({"type": "text", "text": t})),
                Some("image_url") => image_block(
                    p["image_url"]["url"].as_str().or_else(|| p["image_url"].as_str())?,
                ),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Chat-Completions messages and tools → `(system, messages, tools)`.
/// System messages join the top-level `system`; `tool` results become
/// `tool_result` blocks on a user turn; consecutive turns of one role
/// merge (the API wants them alternating). With `cache`, breakpoints go
/// on the end of the system prompt and of the last message, the stable
/// prefix (§4.1).
pub fn request(messages: &[Value], tools: &[Value], cache: bool) -> (Value, Vec<Value>, Vec<Value>) {
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
            "tool" => push(
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": m["tool_call_id"],
                    "content": m["content"].as_str().unwrap_or_default(),
                })],
            ),
            "assistant" => {
                // Thinking blocks first, signatures intact: required back on
                // tool-use turns.
                let mut blocks: Vec<Value> = m["thinking_blocks"].as_array().cloned().unwrap_or_default();
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
    // The first turn must be the user's.
    if out.first().is_some_and(|m| m["role"] == "assistant") {
        out.insert(0, json!({"role": "user", "content": [{"type": "text", "text": "(continue)"}]}));
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

/// The full request body.
pub fn body(
    model: &str,
    messages: &[Value],
    tools: &[Value],
    cache: bool,
    effort: Option<&str>,
    stream: bool,
) -> Value {
    let (system, msgs, tools) = request(messages, tools, cache);
    let mut b = json!({"model": model, "messages": msgs, "max_tokens": MAX_TOKENS, "stream": stream});
    if system.as_array().is_some_and(|s| !s.is_empty()) {
        b["system"] = system;
    }
    if !tools.is_empty() {
        b["tools"] = Value::Array(tools);
    }
    if let Some(budget) = thinking_budget(effort) {
        b["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        // The answer needs room beyond the thinking.
        b["max_tokens"] = json!((budget + 8_192).max(MAX_TOKENS));
    }
    b
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
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// The stream's `error` event: (type, message).
    pub error: Option<(String, String)>,
    /// Block index → what it is, while it streams.
    blocks: Vec<Value>,
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
                        block["signature"] = json!(cur + d["signature"].as_str().unwrap_or_default());
                    }
                    "input_json_delta" => {
                        let cur = block["_json"].as_str().unwrap_or_default().to_string();
                        block["_json"] = json!(cur + d["partial_json"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let i = v["index"].as_u64().unwrap_or(0) as usize;
                let Some(block) = self.blocks.get(i).cloned() else {
                    return Delta::None;
                };
                match block["type"].as_str().unwrap_or_default() {
                    "tool_use" => {
                        let args = block["_json"].as_str().filter(|a| !a.trim().is_empty()).unwrap_or("{}");
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
                if v["delta"]["stop_reason"] == "max_tokens" {
                    self.truncated = true;
                }
                if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                    self.output_tokens = n;
                }
            }
            "error" => {
                self.error = Some((
                    v["error"]["type"].as_str().unwrap_or("error").to_string(),
                    v["error"]["message"].as_str().unwrap_or_default().to_string(),
                ));
            }
            _ => {}
        }
        Delta::None
    }

    /// Nothing the user saw yet: safe to re-send the request.
    pub fn nothing_streamed(&self) -> bool {
        self.text.is_empty() && self.thinking.is_empty() && self.tool_calls.is_empty()
    }
}

/// A non-streamed Messages response: (text, tool calls, usage object).
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
        let tools = vec![json!({"type": "function", "function": {"name": "read", "description": "Read", "parameters": {"type": "object"}}})];
        let b = body("claude-x", &msgs, &tools, true, Some("low"), true);
        assert_eq!(b["system"][0]["text"], "You are Formless.");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0]["content"][1]["type"], "image");
        assert_eq!(m[0]["content"][1]["source"]["media_type"], "image/png");
        assert_eq!(m[1]["content"][0]["signature"], "sig", "thinking echoed first");
        assert_eq!(m[1]["content"][1]["input"]["p"], 1);
        assert_eq!(m[2]["role"], "user");
        assert_eq!(m[2]["content"][0]["type"], "tool_result");
        assert_eq!(m[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(m.last().unwrap()["content"].as_array().unwrap().last().unwrap()["cache_control"]["type"], "ephemeral");
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
        assert_eq!(f.tool_calls, [("tu1".into(), "read".into(), "{\"p\":2}".into())]);
        assert_eq!(f.thinking_blocks[0]["signature"], "abc");
        assert_eq!((f.input_tokens, f.output_tokens, f.cache_read), (10, 33, 7));
        assert!(!f.truncated);
        assert_eq!(thinking_budget(None), None);
    }
}
