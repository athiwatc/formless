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
pub const UNLOAD_TOOLS: &str = "unload_tools";
pub const READ_MODULE: &str = "read_module";
pub const READ_ARTIFACT: &str = "read_artifact";
pub const ADD_MCP_SERVER: &str = "add_mcp_server";
pub const REMOVE_MCP_SERVER: &str = "remove_mcp_server";
pub const LIST_MCP_RESOURCES: &str = "list_mcp_resources";
pub const READ_MCP_RESOURCE: &str = "read_mcp_resource";
pub const LOAD_SKILL: &str = "load_skill";
pub const READ_SKILL_FILE: &str = "read_skill_file";
pub const REMEMBER: &str = "remember";
pub const FORGET: &str = "forget";
pub const ASK_USER: &str = "ask_user";
/// Most questions one `ask_user` call may ask.
pub const MAX_QUESTIONS: usize = 4;
pub const SCHEDULE_RUN: &str = "schedule_run";
pub const READ_METRICS: &str = "read_metrics";
pub const RUN_SCRIPT: &str = "run_script";
pub const UPDATE_PLAN: &str = "update_plan";
/// Most steps a plan may have, and longest step text.
pub const MAX_PLAN_ITEMS: usize = 20;
pub const MAX_PLAN_TEXT: usize = 200;
pub const RESERVED: [&str; 26] = [
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
    UNLOAD_TOOLS,
    READ_MODULE,
    READ_ARTIFACT,
    ADD_MCP_SERVER,
    REMOVE_MCP_SERVER,
    LIST_MCP_RESOURCES,
    READ_MCP_RESOURCE,
    LOAD_SKILL,
    READ_SKILL_FILE,
    REMEMBER,
    FORGET,
    ASK_USER,
    SCHEDULE_RUN,
    READ_METRICS,
    RUN_SCRIPT,
    UPDATE_PLAN,
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
    if name.to_lowercase().starts_with(crate::mcp::PREFIX) {
        return Err(anyhow!(
            "names starting with '{}' are reserved for MCP tools",
            crate::mcp::PREFIX
        ));
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
        .map(|e| {
            let holds_json = e.instance().as_str().is_some_and(|s| {
                serde_json::from_str::<Value>(s.trim())
                    .is_ok_and(|v| v.is_object() || v.is_array())
            });
            if holds_json {
                format!(
                    "{e} (you passed a string containing JSON; pass the object or array itself, not encoded as a string)"
                )
            } else {
                e.to_string()
            }
        })
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
                    "title": {"type": "string", "description": "Short human title shown to the user in lists and approval prompts, 2-5 words in sentence case (e.g. \"Last git author\"). Always set it."},
                    "description": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "1-3 lowercase topic tags (git, web, files, finance) that help find_tools; a new version keeps the old tags when omitted"},
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

/// Fixed wording: tool definitions are part of every request's cached
/// prefix, so nothing here may depend on the registry's size (a Tool
/// created in one Session would otherwise bust every Session's cache).
fn find_tools_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": FIND_TOOLS,
            "description": "Search the Tool registry by keywords (matched against names and descriptions) and load the matches into this session, so they become callable functions from your next step. The registry may hold more Tools than are listed as functions, so search before assuming a Tool does not exist, and before creating one.",
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

fn read_module_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": READ_MODULE,
            "description": "Read a shared module's source (implementation_js), description, version history and the Tools that require it. Read a module before creating a new version of it. find_tools also lists matching modules.",
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

fn read_artifact_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": READ_ARTIFACT,
            "description": "Read a large output this session stored as an artifact (the id from \"[full output ... stored as artifact <id> ...]\"). Returns a slice of the text, or with grep the matching lines with line numbers. No approval needed; only this session's artifacts.",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "offset": {"type": "integer", "description": "Character offset to start at (default 0)"},
                    "length": {"type": "integer", "description": "Characters to return (default 20000, max 100000)"},
                    "grep": {"type": "string", "description": "Return only lines containing this text (case-insensitive), up to 200"}
                },
                "required": ["id"]
            }
        }
    })
}

fn add_mcp_server_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": ADD_MCP_SERVER,
            "description": "Ask the user to connect an external MCP server, whose tools then become available (find them with find_tools). The user sees the exact command or URL, env keys and your reason, and must approve.",
            "parameters": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Short server name: letters, digits, '-' or '_'"},
                    "transport": {"type": "string", "enum": ["stdio", "http"]},
                    "command": {"type": "string", "description": "stdio: program to run"},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "url": {"type": "string", "description": "http: server URL"},
                    "env_keys": {"type": "array", "items": {"type": "string"}, "description": "stdio: environment variables the server needs"},
                    "reason": {"type": "string", "description": "Why this server is needed, shown to the user"}
                },
                "required": ["name", "transport", "reason"]
            }
        }
    })
}

fn load_skill_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": LOAD_SKILL,
            "description": "Load an enabled skill's instructions (see the Skills list in the system prompt, or find_tools) before doing the kind of task it covers. Returns its SKILL.md body and the names of its extra files.",
            "parameters": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }
        }
    })
}

fn read_skill_file_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": READ_SKILL_FILE,
            "description": "Read one of a loaded skill's extra files (reference docs, templates) by its relative path.",
            "parameters": {
                "type": "object",
                "properties": {"name": {"type": "string"}, "path": {"type": "string"}},
                "required": ["name", "path"]
            }
        }
    })
}

fn remember_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": REMEMBER,
            "description": "Save a short note (max 300 chars) about a stable user preference or fact, e.g. \"Uses pnpm, not npm\" or \"Answer in Thai\", when the user states one or asks you to remember. Never secrets, credentials or one-off task details. The user sees every note; it applies from the next Session.",
            "parameters": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "scope": {"type": "string", "description": "\"global\" (default) or \"project:<absolute path>\" for one project"}
                },
                "required": ["text"]
            }
        }
    })
}

fn update_plan_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": UPDATE_PLAN,
            "description": "For a task of 3 or more distinct steps, keep a short checklist the user sees pinned above the input. Send the whole list each time (it replaces the last one): set a step in_progress when you start it and completed when it is done, with exactly one in_progress while working. Skip it for simple tasks; send an empty list to clear it.",
            "parameters": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "maxItems": MAX_PLAN_ITEMS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string", "description": "A short imperative step"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["items"]
            }
        }
    })
}

fn ask_user_definition() -> Value {
    let option = json!({
        "type": "object",
        "properties": {
            "label": {"type": "string", "description": "1–5 words"},
            "description": {"type": "string", "description": "What picking it means"}
        },
        "required": ["label"]
    });
    json!({
        "type": "function",
        "function": {
            "name": ASK_USER,
            "description": "Ask the user and wait for the answer, without ending your Run (pending work keeps going). Use only when a wrong guess would waste real work; offer options when the choices are known (put your pick first). Ask related decisions together in `questions` (up to 4). The user may always type their own answer. Returns {\"answers\": [{question, selected, text?}]} (plus {\"answer\"} for a single top-level question). Fails at once when no user is connected: then proceed on your best assumption and say so.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "A single question (or use `questions`)"},
                    "options": {"type": "array", "items": option, "description": "Suggested answers for `question`"},
                    "multi_select": {"type": "boolean", "description": "The user may pick several options"},
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": MAX_QUESTIONS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {"type": "string"},
                                "header": {"type": "string", "description": "Short label, at most 12 characters"},
                                "options": {"type": "array", "items": option},
                                "multi_select": {"type": "boolean"}
                            },
                            "required": ["question"]
                        }
                    }
                }
            }
        }
    })
}

fn schedule_run_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": SCHEDULE_RUN,
            "description": "Schedule a prompt to run later (at) or periodically (cron, 5 fields, local time), e.g. to re-check a build or run nightly cleanup. Recurring jobs need the user's approval. Without session_id each run gets a new Session; \"current\" runs in this one.",
            "parameters": {
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "The task, self-contained"},
                    "cron": {"type": "string", "description": "e.g. \"0 3 * * *\" (03:00 daily) or @hourly"},
                    "at": {"type": "string", "description": "One-shot: RFC 3339, \"YYYY-MM-DD HH:MM\" or \"in 30m\""},
                    "session_id": {"type": "string"}
                },
                "required": ["prompt"]
            }
        }
    })
}

fn read_metrics_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": READ_METRICS,
            "description": "Registry-wide health, to clean up and explain: kind \"tools\" (filter: needs_attention | failing | unused | a name substring), \"audit\" (recent actions; filter: an action prefix such as tool., proposal., job.), \"models\" (measured model reliability and latency), \"runs\" (Run outcomes), \"builtins\" (built-in call stats), \"duplicates\" (near-duplicate Tool pairs to merge), \"mcp_chains\" (raw MCP calls you keep making back to back, worth one Tool).",
            "parameters": {
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["tools", "audit", "models", "runs", "builtins", "duplicates", "mcp_chains"]},
                    "filter": {"type": "string"},
                    "limit": {"type": "integer", "description": "Default 50, max 200"},
                    "dismiss": {"type": "array", "items": {"type": "string"}, "description": "With kind mcp_chains: a chain's two `calls`, to stop suggesting it"}
                },
                "required": ["kind"]
            }
        }
    })
}

fn forget_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": FORGET,
            "description": "Delete a memory note by its id (shown in [brackets] in the User memory list) when the user asks or it is no longer true.",
            "parameters": {
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"]
            }
        }
    })
}

fn list_mcp_resources_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": LIST_MCP_RESOURCES,
            "description": "List the resources (files, records, docs) that connected MCP servers expose. Optional server and query narrow it. Read one with read_mcp_resource.",
            "parameters": {
                "type": "object",
                "properties": {
                    "server": {"type": "string"},
                    "query": {"type": "string", "description": "Keep resources whose name, uri or description contains this"}
                }
            }
        }
    })
}

fn read_mcp_resource_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": READ_MCP_RESOURCE,
            "description": "Read one MCP resource by server and uri. Large contents are stored as an artifact (read more with read_artifact).",
            "parameters": {
                "type": "object",
                "properties": {"server": {"type": "string"}, "uri": {"type": "string"}},
                "required": ["server", "uri"]
            }
        }
    })
}

fn remove_mcp_server_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": REMOVE_MCP_SERVER,
            "description": "Ask the user to disconnect an MCP server (its tools are retired). Requires approval.",
            "parameters": {
                "type": "object",
                "properties": {"name": {"type": "string"}, "reason": {"type": "string"}},
                "required": ["name", "reason"]
            }
        }
    })
}

fn unload_tools_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": UNLOAD_TOOLS,
            "description": "Stop listing Tools this session no longer needs (fewer, more relevant functions). Takes effect at the next context compaction, so the listing stays stable until then; find_tools loads them again later.",
            "parameters": {
                "type": "object",
                "properties": {
                    "names": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["names"]
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
                    "version": {"type": "integer", "description": "Optional; defaults to the current version"},
                    "diff_from": {"type": "integer", "description": "Optional: also return what changed from this version to the one read"}
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

fn run_script_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": RUN_SCRIPT,
            "description": "Run a short JavaScript script that calls existing Tools, to loop, filter, branch or fan out in one step instead of one call per step; only its return value comes back. The script is the body of an async function: use await and return at top level. `tools` holds only the Tools named in uses, as async functions (`await tools.search_repo({pattern})`), plus tools.call(name, input); Promise.all runs them in parallel. The script itself has no Capabilities: no fetch, fs, process, env, store, llm or agents - anything that reads or changes the world must be a Tool. `input` is the input object. Pass save_as to save a successful script as a Tool; save any script you would run twice.",
            "parameters": {
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "Script body (plain JS, top-level await and return)"},
                    "uses": {"type": "array", "items": {"type": "string"}, "description": "Registry Tools the script may call; approval is asked for these before it runs"},
                    "input": {"type": "object", "description": "Optional; available to the script as `input`"},
                    "save_as": {
                        "type": "object",
                        "description": "Optional: save the script as a Tool if it succeeds (its input is `input`)",
                        "properties": {
                            "name": {"type": "string"},
                            "description": {"type": "string"},
                            "input_schema": {"type": "object"},
                            "output_schema": {"type": "object"}
                        },
                        "required": ["name", "description", "input_schema", "output_schema"]
                    }
                },
                "required": ["code", "uses"]
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

/// What a Tool's code depends on: the Tools it calls (`tools.call("x")`,
/// or a saved script's `uses` list) and the modules it loads
/// (`requireModule("y")`), each sorted and deduplicated. Only literal
/// names count; a name built at run time can't be known.
pub fn code_dependencies(code: &str) -> (Vec<String>, Vec<String>) {
    fn literal_args(code: &str, call: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (i, _) in code.match_indices(call) {
            let rest = code[i + call.len()..].trim_start();
            let Some(q) = rest
                .chars()
                .next()
                .filter(|c| matches!(c, '"' | '\'' | '`'))
            else {
                continue;
            };
            if let Some(end) = rest[1..].find(q) {
                let name = &rest[1..1 + end];
                if !name.is_empty() && !name.contains("${") {
                    out.push(name.to_string());
                }
            }
        }
        out
    }
    let mut tools = literal_args(code, "tools.call(");
    // Scripts saved as Tools list what they may call (scripts.rs).
    if let Some(i) = code.find("const __uses = ") {
        let rest = &code[i + "const __uses = ".len()..];
        if let Some(end) = rest.find(";\n") {
            if let Ok(names) = serde_json::from_str::<Vec<String>>(&rest[..end]) {
                tools.extend(names);
            }
        }
    }
    let mut modules = literal_args(code, "requireModule(");
    for list in [&mut tools, &mut modules] {
        list.sort();
        list.dedup();
    }
    (tools, modules)
}

/// Tags as stored: lowercase, without `#`, letters, digits, `-` and `_`
/// only, at most 24 characters each, no duplicates, at most 8.
pub fn normalize_tags<'a>(raw: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let tag: String = t
            .trim()
            .trim_start_matches('#')
            .to_lowercase()
            .chars()
            .map(|c| if c.is_whitespace() { '-' } else { c })
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .take(24)
            .collect();
        if !tag.is_empty() && !out.contains(&tag) && out.len() < 8 {
            out.push(tag);
        }
    }
    out
}

/// The Tools the agent may see: neither retired nor switched off.
fn agent_tools(store: &Store) -> Result<Vec<ToolInfo>> {
    let mut tools = store.list_tools(false)?;
    tools.retain(|t| !t.disabled);
    Ok(tools)
}

/// Search active Tools with the configured ranking (`tools.search`).
pub fn search_tools_with(
    store: &Store,
    query: &str,
    limit: usize,
    method: &str,
) -> Result<Vec<ToolInfo>> {
    if method.eq_ignore_ascii_case("keyword") {
        search_tools(store, query, limit)
    } else {
        search_tools_bm25(store, query, limit)
    }
}

/// Word groups treated as related when searching ("download web page"
/// should find `http_get_text`). Small and generic on purpose.
const SYNONYMS: &[&[&str]] = &[
    &[
        "fetch", "download", "get", "http", "url", "web", "page", "request", "curl", "site",
    ],
    &["count", "stat", "statistic", "number", "tally", "metric"],
    &["word", "text", "string", "character"],
    &["file", "fs", "path", "disk", "folder"],
    &["read", "load", "open", "cat"],
    &["write", "save", "store", "persist"],
    &["list", "ls", "dir", "directory", "enumerate"],
    &["search", "find", "grep", "match", "lookup", "query"],
    &["delete", "remove", "rm", "erase"],
    &["run", "exec", "execute", "command", "shell", "process"],
    &["date", "time", "clock", "timestamp"],
    &["summar", "summary", "digest", "tldr"],
    &["convert", "transform", "format", "translate"],
];

/// Lowercase, split, and strip common English suffixes so "files",
/// "filed" and "filing" meet "file".
fn stems(s: &str) -> Vec<String> {
    tokens(s)
        .into_iter()
        .map(|t| {
            for suffix in ["ing", "ies", "es", "ed", "s"] {
                if t.len() > suffix.len() + 2 {
                    if let Some(stem) = t.strip_suffix(suffix) {
                        return if suffix == "ies" {
                            format!("{stem}y")
                        } else {
                            stem.to_string()
                        };
                    }
                }
            }
            t
        })
        .collect()
}

/// BM25 search (SPEC §7.1) over each Tool's name (weighted), description
/// and input-schema property names/descriptions, with stemming, prefix
/// matches and the [`SYNONYMS`] groups at reduced weight. Ties go to the
/// more used, more reliable Tool.
pub fn search_tools_bm25(store: &Store, query: &str, limit: usize) -> Result<Vec<ToolInfo>> {
    let tools = agent_tools(store)?;
    let docs: Vec<Vec<String>> = tools
        .iter()
        .map(|t| {
            let name = stems(&t.name);
            let mut doc: Vec<String> = name.iter().chain(&name).chain(&name).cloned().collect();
            // Tags weigh like the name; the title like the description.
            for tag in &t.tags {
                let tag = stems(tag);
                doc.extend(tag.iter().chain(&tag).chain(&tag).cloned());
            }
            doc.extend(stems(&t.title));
            doc.extend(stems(&t.description));
            if let Ok(Some((_, v))) = store.get_tool_current(&t.id) {
                if let Ok(schema) = serde_json::from_str::<Value>(&v.input_schema) {
                    for (prop, def) in schema["properties"].as_object().into_iter().flatten() {
                        doc.extend(stems(prop));
                        doc.extend(stems(def["description"].as_str().unwrap_or_default()));
                    }
                }
            }
            doc
        })
        .collect();
    // Query terms with weights: the words themselves, then their synonyms.
    let mut terms: Vec<(String, f64)> = Vec::new();
    for w in stems(query) {
        for group in SYNONYMS
            .iter()
            .filter(|g| g.iter().any(|s| word_match(s, &w)))
        {
            for s in group.iter() {
                if !terms.iter().any(|(t, _)| t == s) {
                    terms.push((s.to_string(), 0.4));
                }
            }
        }
        match terms.iter_mut().find(|(t, _)| *t == w) {
            Some(t) => t.1 = 1.0,
            None => terms.push((w, 1.0)),
        }
    }
    let n = docs.len() as f64;
    let avg_len = docs.iter().map(Vec::len).sum::<usize>() as f64 / n.max(1.0);
    let (k1, b) = (1.2, 0.75);
    // A document term counts fully when equal, half when it is a prefix match.
    let tf = |doc: &[String], term: &str| -> f64 {
        doc.iter()
            .map(|d| {
                if d == term {
                    1.0
                } else if word_match(d, term) {
                    0.5
                } else {
                    0.0
                }
            })
            .sum()
    };
    let mut scored: Vec<(f64, ToolInfo)> = tools
        .into_iter()
        .zip(&docs)
        .filter_map(|(t, doc)| {
            let mut score = 0.0;
            for (term, weight) in &terms {
                let f = tf(doc, term);
                if f == 0.0 {
                    continue;
                }
                let df = docs.iter().filter(|d| tf(d, term) > 0.0).count() as f64;
                let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                let len_norm = 1.0 - b + b * doc.len() as f64 / avg_len.max(1.0);
                score += weight * idf * f * (k1 + 1.0) / (f + k1 * len_norm);
            }
            if !terms.is_empty() && score == 0.0 {
                return None;
            }
            let usage = (t.invocations as f64 + 1.0).ln() * t.success_rate.unwrap_or(0.5);
            Some((score * 10.0 + usage, t))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(scored.into_iter().take(limit).map(|(_, t)| t).collect())
}

/// Keyword search over active Tools (SPEC §7.1). Name hits outrank
/// description hits; ties go to the more used, more reliable Tool.
pub fn search_tools(store: &Store, query: &str, limit: usize) -> Result<Vec<ToolInfo>> {
    let q = tokens(query);
    let mut scored: Vec<(f64, ToolInfo)> = agent_tools(store)?
        .into_iter()
        .filter_map(|t| {
            let name = tokens(&t.name);
            let desc = tokens(&format!("{} {}", t.title, t.description));
            let mut score = 0.0;
            for w in &q {
                if name.iter().any(|n| n == w) || t.tags.iter().any(|g| g == w) {
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

/// The function definition a registry Tool is listed to the model with.
pub fn registry_definition(info: &ToolInfo, v: &formless_protocol::ToolVersionInfo) -> Value {
    let params: Value =
        serde_json::from_str(&v.input_schema).unwrap_or_else(|_| json!({"type": "object"}));
    json!({
        "type": "function",
        "function": {"name": info.name, "description": info.description, "parameters": params}
    })
}

/// The built-in tools' definitions, as listed to the model.
pub fn builtin_definitions() -> Vec<Value> {
    vec![
        create_tool_definition(),
        create_module_definition(),
        retire_tool_definition(),
        find_tools_definition(),
        read_tool_definition(),
        unload_tools_definition(),
        read_module_definition(),
        read_artifact_definition(),
        add_mcp_server_definition(),
        remove_mcp_server_definition(),
        load_skill_definition(),
        read_skill_file_definition(),
        remember_definition(),
        forget_definition(),
        ask_user_definition(),
        schedule_run_definition(),
        read_metrics_definition(),
        list_mcp_resources_definition(),
        read_mcp_resource_definition(),
        test_tool_definition(),
        run_script_definition(),
        update_plan_definition(),
        pending_definition(AWAIT_INVOCATION, "Wait (unbounded) for a Pending Invocation handle and return its final result."),
        pending_definition(CHECK_INVOCATION, "Check a Pending Invocation handle: returns status pending, or its final result if complete."),
        pending_definition(ABANDON_INVOCATION, "Abandon a Pending Invocation handle: it still finishes and is recorded, but its result will not be delivered."),
    ]
}

/// A built-in tool's input schema (its `parameters`).
pub fn builtin_schema(name: &str) -> Option<Value> {
    // Built once: the definitions don't change while the process runs (#773).
    static SCHEMAS: std::sync::OnceLock<std::collections::HashMap<String, Value>> =
        std::sync::OnceLock::new();
    SCHEMAS
        .get_or_init(|| {
            builtin_definitions()
                .into_iter()
                .filter_map(|d| {
                    let name = d["function"]["name"].as_str()?.to_string();
                    Some((name, d["function"]["parameters"].clone()))
                })
                .collect()
        })
        .get(name)
        .cloned()
}

/// Lenient input (some models, e.g. Qwen, send nested objects as JSON
/// strings): where `schema` expects an object or array and `value` holds a
/// string that parses as one, use the parsed value. Follows local `$ref`s,
/// `allOf`/`anyOf`/`oneOf` and `additionalProperties`, the shapes MCP
/// servers' schemas use (#773). Returns the paths it changed (`""` is the
/// whole input).
pub fn coerce_json_strings(schema: &Value, value: &mut Value) -> Vec<String> {
    let mut changed = Vec::new();
    coerce_at(schema, schema, value, String::new(), &mut changed, 0);
    changed
}

/// `schema` with a local `$ref` (`#/$defs/X`, `#/definitions/X`) followed.
fn resolve<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    let mut s = schema;
    for _ in 0..8 {
        match s["$ref"].as_str().and_then(|r| r.strip_prefix('#')) {
            Some(pointer) => match root.pointer(pointer) {
                Some(target) => s = target,
                None => break,
            },
            None => break,
        }
    }
    s
}

fn types_of(schema: &Value) -> Vec<&str> {
    match &schema["type"] {
        Value::String(t) => vec![t.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

/// Whether `value`'s JSON type is one `schema` names (no type: any).
fn type_fits(schema: &Value, value: &Value) -> bool {
    let types = types_of(schema);
    types.is_empty()
        || types.iter().any(|t| match *t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        })
}

fn coerce_at(
    root: &Value,
    schema: &Value,
    value: &mut Value,
    path: String,
    changed: &mut Vec<String>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    let schema = resolve(root, schema);
    // allOf: every branch applies.
    if let Some(all) = schema["allOf"].as_array() {
        for branch in all {
            coerce_at(root, branch, value, path.clone(), changed, depth + 1);
        }
    }
    // anyOf / oneOf: the first branch the coerced value fits.
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = schema[key].as_array() {
            for branch in branches {
                let branch = resolve(root, branch);
                let mut trial = value.clone();
                let mut trial_changed = Vec::new();
                coerce_at(
                    root,
                    branch,
                    &mut trial,
                    path.clone(),
                    &mut trial_changed,
                    depth + 1,
                );
                if type_fits(branch, &trial) && !types_of(branch).is_empty() {
                    if !trial_changed.is_empty() {
                        *value = trial;
                        changed.extend(trial_changed);
                    }
                    break;
                }
            }
        }
    }
    let types = types_of(schema);
    let wants = |t: &str| types.contains(&t) && !types.contains(&"string");
    if let Value::String(s) = value {
        if wants("object") || wants("array") {
            if let Ok(parsed) = serde_json::from_str::<Value>(s.trim()) {
                if (parsed.is_object() && wants("object")) || (parsed.is_array() && wants("array"))
                {
                    *value = parsed;
                    changed.push(path.clone());
                }
            }
        }
    }
    let join = |k: &str| {
        if path.is_empty() {
            k.to_string()
        } else {
            format!("{path}.{k}")
        }
    };
    match value {
        Value::Object(map) => {
            let props = schema["properties"].as_object();
            for (k, v) in map.iter_mut() {
                let sub = match props.and_then(|p| p.get(k)) {
                    Some(sub) => sub,
                    // Map-like objects: every other key's value schema.
                    None if schema["additionalProperties"].is_object() => {
                        &schema["additionalProperties"]
                    }
                    None => continue,
                };
                coerce_at(root, sub, v, join(k), changed, depth + 1);
            }
        }
        Value::Array(items) if schema["items"].is_object() => {
            for (i, v) in items.iter_mut().enumerate() {
                coerce_at(
                    root,
                    &schema["items"],
                    v,
                    format!("{path}[{i}]"),
                    changed,
                    depth + 1,
                );
            }
        }
        _ => {}
    }
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
    let tools = agent_tools(store)?;
    // Every Session's listing is frozen at its first step (in small and
    // large registries alike): Tool definitions are part of the cached
    // prompt prefix, so a Tool created or used in another Session must not
    // change this one's. A Session's own creations and `find_tools` results
    // are added to it; everything else is reached through `find_tools`.
    let shown: Vec<ToolInfo> = {
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
            // Pinned Tools are always listed (they join at this refresh
            // point, never mid-Session, so the cached prefix holds).
            let pinned: Vec<String> = tools
                .iter()
                .filter(|t| t.pinned && !loaded.contains(&t.id))
                .map(|t| t.id.clone())
                .collect();
            if let Some(s) = session_id {
                store.load_session_tools(s, &pinned, "pinned")?;
            }
            loaded.extend(pinned);
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
        // Pinned Tools first, then the rest; each group by name.
        let mut shown: Vec<ToolInfo> = tools
            .iter()
            .filter(|t| loaded.contains(&t.id))
            .cloned()
            .collect();
        shown.sort_by_key(|t| !t.pinned);
        shown
    };
    let pinned_names: std::collections::HashSet<String> = tools
        .iter()
        .filter(|t| t.pinned)
        .map(|t| t.name.to_lowercase())
        .collect();
    let mut defs = builtin_definitions();
    for info in shown {
        // A Session keeps the definition it first listed (a promotion or
        // rollback elsewhere must not rewrite its cached prefix); it picks
        // up the current one after find_tools/read_tool or compaction.
        if let Some(s) = session_id {
            if let Some((_, def)) = store.pinned_definition(s, &info.id)? {
                if let Ok(def) = serde_json::from_str(&def) {
                    defs.push(def);
                    continue;
                }
            }
        }
        if let Some((_i, v)) = store.get_tool_current(&info.id)? {
            let def = registry_definition(&info, &v);
            if let Some(s) = session_id {
                store.pin_definition(s, &info.id, v.version, &def.to_string())?;
            }
            defs.push(def);
        }
    }
    // A Tool retired since the Session listed it (an MCP server dropped
    // it, or a user retired it) stays listed, as pinned, until the next
    // refresh point (compaction): removing it would rewrite the cached
    // prefix. Calling it reports that it is retired.
    if let Some(s) = session_id {
        let builtins = defs.iter().take_while(|d| is_builtin(fn_name(d))).count();
        let mut registry: Vec<Value> = defs.split_off(builtins);
        for (_, def) in store.pinned_inactive(s)? {
            if let Ok(def) = serde_json::from_str::<Value>(&def) {
                registry.push(def);
            }
        }
        // Same order as the listing itself: pinned first, then by name.
        registry.sort_by_key(|d| {
            let name = fn_name(d).to_lowercase();
            (!pinned_names.contains(&name), name)
        });
        defs.extend(registry);
    }
    Ok(defs)
}

fn fn_name(def: &Value) -> &str {
    def["function"]["name"].as_str().unwrap_or_default()
}

/// A starting input for running a Tool by hand (SPEC §10.4): every
/// property of its input schema, filled with its default, first example or
/// enum value, else an empty value of its type.
pub fn input_skeleton(schema: &Value) -> Value {
    if let Some(v) = schema
        .get("default")
        .or_else(|| schema["examples"].get(0))
        .or_else(|| schema["enum"].get(0))
    {
        return v.clone();
    }
    let ty = match &schema["type"] {
        Value::Array(types) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("null"),
        t => t.as_str().unwrap_or(if schema.get("properties").is_some() {
            "object"
        } else {
            "null"
        }),
    };
    match ty {
        "object" => Value::Object(
            schema["properties"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| (k.clone(), input_skeleton(v)))
                .collect(),
        ),
        "array" => json!([]),
        "string" => json!(""),
        "integer" | "number" => json!(0),
        "boolean" => json!(false),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod skeleton_tests {
    use super::*;

    #[test]
    fn skeletons_follow_the_input_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "limit": {"type": "integer", "default": 5},
                "mode": {"enum": ["fast", "full"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "deep": {"type": ["boolean", "null"]},
                "opts": {"properties": {"retry": {"type": "number"}}},
            }
        });
        assert_eq!(
            input_skeleton(&schema),
            json!({"url": "", "limit": 5, "mode": "fast", "tags": [], "deep": false, "opts": {"retry": 0}})
        );
        assert_eq!(input_skeleton(&json!({})), Value::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_string_arguments_are_parsed_where_objects_are_expected() {
        let schema = json!({"type": "object", "properties": {
            "input": {"type": "object"},
            "tags": {"type": "array", "items": {"type": "string"}},
            "note": {"type": "string"},
            "nested": {"type": "object", "properties": {"inner": {"type": "object"}}}
        }});
        let mut v = json!({"input": "{\"path\": \"/etc/hosts\"}", "tags": "[\"a\"]",
            "note": "{\"stays\": 1}", "nested": {"inner": "{}"}});
        let changed = coerce_json_strings(&schema, &mut v);
        assert_eq!(v["input"]["path"], "/etc/hosts");
        assert_eq!(v["tags"][0], "a");
        assert!(v["note"].is_string(), "a string field stays a string");
        assert!(v["nested"]["inner"].is_object());
        assert_eq!(changed, ["input", "nested.inner", "tags"]);
        let mut whole = json!("{\"a\": 1}");
        coerce_json_strings(&json!({"type": "object"}), &mut whole);
        assert_eq!(whole["a"], 1);
    }

    #[test]
    fn a_json_string_in_place_of_an_object_gets_an_actionable_error() {
        let v =
            compile_schema(&json!({"type": "object", "properties": {"input": {"type": "object"}}}))
                .unwrap();
        let e = validate_against(&v, &json!({"input": "{not json"})).unwrap_err();
        assert!(!e.contains("string containing JSON"), "{e}");
        let e = validate_against(&v, &json!({"input": "{\"a\": 1}"})).unwrap_err();
        assert!(e.contains("pass the object or array itself"), "{e}");
    }

    /// MCP-shaped schemas: `$ref`, `anyOf` and map values are followed
    /// (#773).
    #[test]
    fn coercion_follows_refs_branches_and_maps() {
        let schema = serde_json::json!({
            "type": "object",
            "$defs": {"Filter": {"type": "object", "properties": {"tags": {"type": "array"}}}},
            "properties": {
                "filter": {"$ref": "#/$defs/Filter"},
                "target": {"anyOf": [{"type": "null"}, {"type": "object", "properties": {"id": {"type": "string"}}}]},
                "labels": {"type": "object", "additionalProperties": {"type": "array"}},
                "both": {"allOf": [{"type": "object"}]}
            }
        });
        let mut v = serde_json::json!({
            "filter": "{\"tags\": \"[1, 2]\"}",
            "target": "{\"id\": \"x\"}",
            "labels": {"a": "[\"p\"]"},
            "both": "{\"k\": 1}"
        });
        let changed = coerce_json_strings(&schema, &mut v);
        assert_eq!(v["filter"]["tags"], serde_json::json!([1, 2]));
        assert_eq!(v["target"]["id"], "x");
        assert_eq!(v["labels"]["a"], serde_json::json!(["p"]));
        assert_eq!(v["both"]["k"], 1);
        assert!(changed.contains(&"filter.tags".to_string()), "{changed:?}");
        // A string that isn't JSON stays as it is, for validation to report.
        let mut bad = serde_json::json!({"filter": "not json"});
        assert!(coerce_json_strings(&schema, &mut bad).is_empty());
        assert_eq!(bad["filter"], "not json");
        // Built-in schemas come from the cache.
        assert!(builtin_schema("find_tools").is_some());
        assert!(builtin_schema("nope").is_none());
    }
}
