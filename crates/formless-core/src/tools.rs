//! Tool registry helpers (SPEC §7): validation, definitions, redaction.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::storage::Store;
use formless_protocol::ToolInfo;

pub const CREATE_TOOL: &str = "create_tool";
pub const CREATE_MODULE: &str = "create_module";
/// Not a built-in any more (sub-agents are the `agent` Capability), but
/// still reserved so a registry Tool cannot impersonate it.
pub const SPAWN_SUBAGENT: &str = "spawn_subagent";
pub const RETIRE_TOOL: &str = "retire_tool";
pub const AWAIT_INVOCATION: &str = "await_invocation";
pub const CHECK_INVOCATION: &str = "check_invocation";
pub const ABANDON_INVOCATION: &str = "abandon_invocation";
pub const FIND_TOOLS: &str = "find_tools";
pub const READ_TOOL: &str = "read_tool";
pub const TEST_TOOL: &str = "test_tool";
pub const RESERVED: [&str; 10] = [
    CREATE_TOOL,
    CREATE_MODULE,
    SPAWN_SUBAGENT,
    RETIRE_TOOL,
    AWAIT_INVOCATION,
    CHECK_INVOCATION,
    ABANDON_INVOCATION,
    FIND_TOOLS,
    READ_TOOL,
    TEST_TOOL,
];

/// Built-in (non-registry) tools, dispatched by the daemon itself.
pub fn is_builtin(name: &str) -> bool {
    name != SPAWN_SUBAGENT && RESERVED.contains(&name)
}

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
            "description": "Create (or create a new version of) a Tool: persisted plain JavaScript defining `function run(input)`. Validated and smoke-tested before saving; failures are returned as errors. Tools are reused across tasks and projects: first find_tools to check whether an existing Tool handles this with different input (read_tool it to build a new version), and pass every task-specific value (paths, URLs, names, queries) through input_schema instead of hardcoding it.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "snake_case, 1-64 chars; names the general operation (read_text_file), never one project or file"},
                    "description": {"type": "string"},
                    "input_schema": {"type": "object", "description": "JSON Schema (draft 2020-12) for run(input)"},
                    "output_schema": {"type": "object", "description": "JSON Schema for the return value"},
                    "implementation_js": {"type": "string", "description": "Plain JS defining global function run(input) (may be async). Throw on failure; never return {error: ...}."},
                    "capabilities": {"type": "array", "items": {"type": "string"}, "description": "e.g. [\"net\", \"net:$input.url\", \"fs:read:/abs\", \"fs:read:$input.path\", \"fs:write:$input.out\", \"env:KEY\", \"process:cmd\", \"clipboard:read\", \"notify\", \"llm\"]. $input.<field> scopes (fs:read, fs:write, net only) are granted per call for the path(s)/host in that input field."},
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

fn create_module_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": CREATE_MODULE,
            "description": "Create (or add a version of) a shared JS module that Tools load with requireModule(name). implementation_js sets module.exports (e.g. module.exports = { helper }). Modules are shared across all Tools and versioned immutably.",
            "parameters": {
                "type": "object",
                "required": ["name", "implementation_js"],
                "properties": {
                    "name": {"type": "string", "pattern": "^[a-z][a-z0-9_-]{0,63}$"},
                    "description": {"type": "string"},
                    "implementation_js": {"type": "string", "description": "Plain JS setting module.exports."}
                }
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

fn find_tools_definition(total: usize, listed: usize) -> Value {
    let scope = if listed < total {
        format!(" The registry holds {total} Tools; only {listed} are listed as functions right now, so search before assuming a Tool does not exist.")
    } else {
        String::new()
    };
    json!({
        "type": "function",
        "function": {
            "name": FIND_TOOLS,
            "description": format!("Search the Tool registry by keywords (matched against names and descriptions) and load the matches into this session, so they become callable functions from your next step.{scope} Search before creating a Tool."),
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Keywords for the operation you need, e.g. \"read file lines\" or \"http json\". Empty lists the most-used Tools."},
                    "limit": {"type": "integer", "description": "Max results (default 8, max 25)"}
                },
                "required": ["query"]
            }
        }
    })
}

fn read_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": READ_TOOL,
            "description": "Read a Tool's full definition: implementation_js, input/output schemas, capabilities, memory_mb, version history, metrics and recent failures. Read a Tool before creating a new version of it, so you change it rather than rewrite it from memory.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "version": {"type": "integer", "description": "Optional; defaults to the current version"}
                },
                "required": ["name"]
            }
        }
    })
}

fn test_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TEST_TOOL,
            "description": "Dry-run a Tool on a real input without saving it or recording metrics: either an existing Tool (name only) or an unsaved draft (the create_tool fields). Runs live under the declared Capabilities, with approval as for a normal call. Returns the output or error, console logs, and whether the output matches output_schema. Test drafts here before create_tool.",
            "parameters": {
                "type": "object",
                "properties": {
                    "input": {"description": "The input to run the Tool with"},
                    "name": {"type": "string"},
                    "description": {"type": "string"},
                    "input_schema": {"type": "object"},
                    "output_schema": {"type": "object"},
                    "implementation_js": {"type": "string", "description": "Draft code; omit to test the saved Tool called name"},
                    "capabilities": {"type": "array", "items": {"type": "string"}},
                    "memory_mb": {"type": "integer"}
                },
                "required": ["input"]
            }
        }
    })
}

fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Loose word match: equal, or one is a prefix of the other (both at least
/// 3 chars), so "files" finds "file" and "parse" finds "parser".
fn word_match(a: &str, b: &str) -> bool {
    a == b || (a.len() >= 3 && b.len() >= 3 && (a.starts_with(b) || b.starts_with(a)))
}

/// Keyword search over active Tools (SPEC §7.1). Name hits outrank
/// description hits; ties go to the more used, more reliable Tool.
pub fn search_tools(store: &Store, query: &str, limit: usize) -> Result<Vec<ToolInfo>> {
    let q = tokens(query);
    let mut scored: Vec<(f64, ToolInfo)> = store
        .list_tools(false)?
        .into_iter()
        .filter_map(|t| {
            let name = tokens(&t.name);
            let desc = tokens(&t.description);
            let mut score = 0.0;
            for w in &q {
                if name.iter().any(|n| n == w) {
                    score += 3.0;
                } else if name.iter().any(|n| word_match(n, w)) {
                    score += 2.0;
                } else if desc.iter().any(|d| word_match(d, w)) {
                    score += 1.0;
                }
            }
            if !q.is_empty() && score == 0.0 {
                return None;
            }
            let usage = (t.invocations as f64 + 1.0).ln() * t.success_rate.unwrap_or(0.5);
            Some((score * 10.0 + usage, t))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(scored.into_iter().take(limit).map(|(_, t)| t).collect())
}

/// Provider tool definitions: the built-ins plus registry Tools, sorted by
/// name. Small registries are listed whole. Past `inline_limit`, a Session
/// sees the Tools it has loaded (found, created or called) plus the
/// most-used ones, frozen the first time it lists Tools, and reaches the
/// rest through `find_tools` (SPEC §7.1). The list therefore changes only
/// when the Session loads a Tool, which keeps the request prefix cacheable.
pub fn tool_definitions(
    store: &Store,
    session_id: Option<&str>,
    inline_limit: usize,
) -> Result<Vec<Value>> {
    let tools = store.list_tools(false)?;
    let total = tools.len();
    let shown: Vec<ToolInfo> = if total <= inline_limit {
        tools
    } else {
        let mut loaded: std::collections::HashSet<String> = match session_id {
            Some(s) => store.session_tool_ids(s)?,
            None => Default::default(),
        };
        loaded.retain(|id| tools.iter().any(|t| &t.id == id));
        let frozen = match session_id {
            Some(s) => store.session_has_filler(s)?,
            None => false,
        };
        if !frozen {
            let mut rest: Vec<&ToolInfo> =
                tools.iter().filter(|t| !loaded.contains(&t.id)).collect();
            rest.sort_by_key(|t| std::cmp::Reverse(t.invocations));
            let filler: Vec<String> = rest
                .into_iter()
                .take(inline_limit.saturating_sub(loaded.len()))
                .map(|t| t.id.clone())
                .collect();
            if let Some(s) = session_id {
                store.load_session_tools(s, &filler, "filler")?;
            }
            loaded.extend(filler);
        }
        // list_tools is name-ordered, so the listing is too.
        tools
            .into_iter()
            .filter(|t| loaded.contains(&t.id))
            .collect()
    };
    let mut defs = vec![
        create_tool_definition(),
        create_module_definition(),
        retire_tool_definition(),
        find_tools_definition(total, shown.len()),
        read_tool_definition(),
        test_tool_definition(),
        pending_definition(AWAIT_INVOCATION, "Wait (unbounded) for a Pending Invocation handle and return its final result."),
        pending_definition(CHECK_INVOCATION, "Check a Pending Invocation handle: returns status pending, or its final result if complete."),
        pending_definition(ABANDON_INVOCATION, "Abandon a Pending Invocation handle: it still finishes and is recorded, but its result will not be delivered."),
    ];
    for info in shown {
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
