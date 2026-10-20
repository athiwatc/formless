//! Tool registry helpers (SPEC §7): validation, definitions, redaction.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::storage::Store;

pub const CREATE_TOOL: &str = "create_tool";
pub const RETIRE_TOOL: &str = "retire_tool";
pub const AWAIT_INVOCATION: &str = "await_invocation";
pub const CHECK_INVOCATION: &str = "check_invocation";
pub const ABANDON_INVOCATION: &str = "abandon_invocation";
pub const RESERVED: [&str; 5] = [
    CREATE_TOOL,
    RETIRE_TOOL,
    AWAIT_INVOCATION,
    CHECK_INVOCATION,
    ABANDON_INVOCATION,
];

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(anyhow!("tool name must be 1-64 chars"));
    }
    if RESERVED.contains(&name.to_lowercase().as_str()) {
        return Err(anyhow!("'{name}' is reserved"));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name
            .chars()
            .next()
            .map(|c| c.is_ascii_lowercase() || c == '_')
            .unwrap_or(false);
    if !ok {
        return Err(anyhow!(
            "tool name must be snake_case (lowercase, digits, underscores)"
        ));
    }
    Ok(())
}

pub fn compile_schema(schema: &Value) -> Result<jsonschema::Validator> {
    jsonschema::validator_for(schema).map_err(|e| anyhow!("invalid JSON Schema: {e}"))
}

pub fn validate_against(
    validator: &jsonschema::Validator,
    value: &Value,
) -> std::result::Result<(), String> {
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Schema-minimal smoke input (SPEC §7.1, §16): required fields with
/// type-appropriate empties.
pub fn minimal_input(schema: &Value) -> Value {
    match schema["type"].as_str() {
        Some("string") => json!(""),
        Some("number") | Some("integer") => json!(0),
        Some("boolean") => json!(false),
        Some("array") => json!([]),
        Some("object") | None => {
            let mut obj = serde_json::Map::new();
            if let Some(req) = schema["required"].as_array() {
                for r in req {
                    if let Some(key) = r.as_str() {
                        let sub = &schema["properties"][key];
                        obj.insert(key.to_string(), minimal_input(sub));
                    }
                }
            }
            Value::Object(obj)
        }
        _ => Value::Null,
    }
}

/// Basic secret redaction for recorded payloads (SPEC §15).
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let lk = k.to_lowercase();
                if lk.contains("key")
                    || lk.contains("token")
                    || lk.contains("secret")
                    || lk.contains("password")
                    || lk.contains("authorization")
                {
                    out.insert(k.clone(), json!("[redacted]"));
                } else {
                    out.insert(k.clone(), redact(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        Value::String(s) => {
            if s.to_lowercase().starts_with("bearer ") || (s.len() > 32 && s.starts_with("sk-")) {
                json!("[redacted]")
            } else {
                value.clone()
            }
        }
        _ => value.clone(),
    }
}

pub fn create_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": CREATE_TOOL,
            "description": "Create (or create a new version of) a Tool: persisted plain JavaScript defining `function run(input)`. Validated and smoke-tested before saving; failures are returned as errors.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "snake_case, 1-64 chars"},
                    "description": {"type": "string"},
                    "input_schema": {"type": "object", "description": "JSON Schema (draft 2020-12) for run(input)"},
                    "output_schema": {"type": "object", "description": "JSON Schema for the return value"},
                    "implementation_js": {"type": "string", "description": "Plain JS defining global function run(input)"},
                    "capabilities": {"type": "array", "items": {"type": "string"}, "description": "e.g. [\"net\", \"fs:read:/abs\", \"fs:write:/abs\", \"env:KEY\", \"process:cmd\", \"clipboard:read\", \"notify\", \"llm\"]"},
                    "memory_mb": {"type": "integer", "description": "Optional JS heap cap (default 256)"}
                },
                "required": ["name", "description", "input_schema", "output_schema", "implementation_js", "capabilities"]
            }
        }
    })
}

pub fn retire_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": RETIRE_TOOL,
            "description": "Retire a Tool: instantly uncallable everywhere, data retained, restorable by the user. State your reason.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "reason": {"type": "string"}
                },
                "required": ["name", "reason"]
            }
        }
    })
}

fn pending_definition(name: &str, description: &str) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": {"invocation_id": {"type": "string"}},
                "required": ["invocation_id"]
            }
        }
    })
}

/// Provider tool definitions: create_tool + retire_tool + every active Tool.
pub fn tool_definitions(store: &Store) -> Result<Vec<Value>> {
    let mut defs = vec![
        create_tool_definition(),
        retire_tool_definition(),
        pending_definition(AWAIT_INVOCATION, "Wait (unbounded) for a Pending Invocation handle and return its final result."),
        pending_definition(CHECK_INVOCATION, "Check a Pending Invocation handle: returns status pending, or its final result if complete."),
        pending_definition(ABANDON_INVOCATION, "Abandon a Pending Invocation handle: it still finishes and is recorded, but its result will not be delivered."),
    ];
    for info in store.list_tools(false)? {
        if let Some((_i, v)) = store.get_tool_current(&info.id)? {
            let params: Value =
                serde_json::from_str(&v.input_schema).unwrap_or_else(|_| json!({"type": "object"}));
            defs.push(json!({
                "type": "function",
                "function": {"name": info.name, "description": info.description, "parameters": params}
            }));
        }
    }
    Ok(defs)
}
