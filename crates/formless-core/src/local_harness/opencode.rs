//! OpenCode V2 over the shared ACP client; verified against OpenCode 2.3.2.
//!
//! See <https://opencode.ai/docs/acp/>, <https://opencode.ai/config.json> and
//! <https://opencode.ai/docs/permissions/>. Settings are supplied only through
//! OPENCODE_CONFIG_CONTENT; user/project configuration is read, never written.
//! MCP credentials travel through ACP, not configuration or the agent environment.
//!
//! MCP actions are normalized as server_tool. There is no MCP-only permission
//! wildcard: *_* asks for all such actions (and built-ins containing underscores),
//! followed by the trusted formless_* exception. Other built-ins remain allowed in
//! Build. Review additionally denies edit/shell and native subagents are disabled.
//! Formless must authorize every bridge operation independently.
//! OpenCode flattens namespaces: configured server names beginning formless_ also
//! collide with that exception and must not be treated as untrusted servers.
//! Rules are last-match-wins, verified by the ignored real-binary test. "Always"
//! decisions stay in core's attempt cache and become allow-once replies to OpenCode.
//!
//! Code-mode approvals identify execute, not the inner action. They always require
//! the user; JavaScript is never used to recognize a trusted bridge invocation.
//! V1, run/JSONL, importing and resuming sessions are not supported here.
//! Question forms currently support the observed optional single-choice oneOf and
//! paired custom-text schema; other schemas (including arrays) are declined.
//!
//! Live smoke: set FORMLESS_OPENCODE_BIN and FORMLESS_OPENCODE_SMOKE_DIR (a persistent
//! directory containing work/), then run the ignored real_binary_acp_permissions_and_read_only
//! test with --nocapture. It uses fresh HOME/XDG directories and the default free model.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{mpsc, Arc},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use formless_protocol::{HarnessAvailability, Question, QuestionAnswer, QuestionOption};
use formless_runtime::process_group::SupervisedProcess;
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use super::acp::{self, AgentSpec, PermissionHandler};
use super::{HarnessRequest, HarnessResult, InteractionHandler};

pub const SPEC: AgentSpec = AgentSpec {
    name: "OpenCode",
    program: "opencode",
    args: |_| vec!["acp".into()],
    check_agent,
    not_signed_in:
        "OpenCode requires provider authentication. Configure OpenCode in a terminal, then retry.",
    read_only_modes: &["formless-review"],
    read_only_fail_closed: true,
    write_modes: &["build"],
    client_capabilities: |caps| caps["elicitation"] = json!({"form": {}}),
    strip_env: &[],
    set_env: &[],
    bridge_tool: acp::no_bridge_tools,
    vendor_request,
};

#[cfg(not(test))]
const AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const AVAILABILITY_TIMEOUT: Duration = Duration::from_millis(200);

/// Queries only the CLI version: credential discovery initializes providers and
/// performs network requests, so readiness must not run it.
pub fn availability(
    executable: Option<PathBuf>,
    environment: &BTreeMap<OsString, OsString>,
) -> HarnessAvailability {
    let version = probe_version(executable, environment);
    let (installed, version, error) = match version {
        Ok(version) => {
            let compatible =
                acp::parse_version(&version).is_some_and(|v| v.0 == 2 && v >= (2, 3, 2));
            (
                true,
                Some(version),
                (!compatible).then_some("This adapter requires OpenCode V2 >= 2.3.2."),
            )
        }
        Err(error) => (error.installed, None, Some(error.detail)),
    };
    let detail = "OpenCode can use its free default model without credentials. Credential status is unknown: auth list initializes providers and makes network requests.";
    HarnessAvailability {
        kind: super::HarnessKind::OpenCode,
        acp_agent: None,
        installed,
        signed_in: None,
        version,
        detail: Some(match error {
            Some(error) => format!("{error} {detail}"),
            None => detail.to_owned(),
        }),
    }
}

#[derive(Clone, Copy)]
struct ProbeError {
    installed: bool,
    detail: &'static str,
}

struct ProbeDirectory(PathBuf);

impl ProbeDirectory {
    fn create() -> std::io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("formless-opencode-probe-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn probe_version(
    executable: Option<PathBuf>,
    environment: &BTreeMap<OsString, OsString>,
) -> std::result::Result<String, ProbeError> {
    let directory = ProbeDirectory::create().map_err(|_| ProbeError {
        installed: false,
        detail: "OpenCode version probe could not create a private working directory.",
    })?;
    let mut command = Command::new(executable.unwrap_or_else(|| SPEC.program.into()));
    command
        .arg("--version")
        .current_dir(&directory.0)
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let deadline = std::time::Instant::now() + AVAILABILITY_TIMEOUT;
    let mut process = SupervisedProcess::spawn(command).map_err(|_| ProbeError {
        installed: false,
        detail: "OpenCode executable could not be started.",
    })?;
    let (_, stdout, _) = process.take_pipes();
    let stdout = stdout.ok_or(ProbeError {
        installed: true,
        detail: "OpenCode version output is unavailable.",
    })?;
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let result = stdout.take(4097).read_to_end(&mut output).map(|_| output);
        let _ = send.send(result);
    });
    let output = receive
        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        .map_err(|_| ProbeError {
            installed: true,
            detail: "OpenCode version probe timed out or disconnected.",
        })?
        .map_err(|_| ProbeError {
            installed: true,
            detail: "OpenCode version output could not be read.",
        })?;
    if output.len() > 4096 {
        return Err(ProbeError {
            installed: true,
            detail: "OpenCode version output exceeded the limit.",
        });
    }
    let status = loop {
        if let Some(status) = process.try_wait().map_err(|_| ProbeError {
            installed: true,
            detail: "OpenCode version process status could not be read.",
        })? {
            break status;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(ProbeError {
                installed: true,
                detail: "OpenCode version probe timed out or disconnected.",
            });
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    };
    drop(process);
    if !status.success() {
        return Err(ProbeError {
            installed: true,
            detail: "OpenCode version command exited unsuccessfully.",
        });
    }
    let version = std::str::from_utf8(&output)
        .ok()
        .and_then(|s| s.trim().strip_prefix("opencode v"))
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
        .filter(|s| acp::parse_version(s).is_some())
        .ok_or(ProbeError {
            installed: true,
            detail: "OpenCode did not report a recognizable CLI version.",
        })?;
    Ok(version.to_owned())
}

/// Runs the shared blocking ACP driver off the async core's executor.
pub async fn run(
    request: &HarnessRequest,
    interactions: Arc<dyn InteractionHandler>,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    let request = prepare(request)?;
    let runtime = tokio::runtime::Handle::current();
    let cancel = cancel.child_token();
    let _cancel_on_drop = cancel.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        let mut permissions = AttemptScoped(acp::Interactions {
            handler: interactions.as_ref(),
            runtime,
        });
        acp::run(&SPEC, &request, &mut permissions, &cancel)
    })
    .await
    .context("OpenCode ACP task failed")?
}

pub fn open_session(request: &HarnessRequest) -> Result<String> {
    acp::open_session(&SPEC, &prepare(request)?)
}

struct AttemptScoped<'a>(acp::Interactions<'a>);

fn once_option(options: &[Value], allow: bool) -> Option<String> {
    let kind = if allow { "allow_once" } else { "reject_once" };
    options.iter().find(|o| o["kind"] == kind)?["optionId"]
        .as_str()
        .map(str::to_owned)
}

impl PermissionHandler for AttemptScoped<'_> {
    fn decide(&mut self, request: &acp::PermissionRequest<'_>) -> Option<String> {
        let reject = || once_option(request.options, false);
        if request.cancel.is_cancelled() {
            return reject();
        }
        let decision = self.0.decide(request);
        if request.cancel.is_cancelled() {
            return reject();
        }
        let Some(option) = decision.as_ref().and_then(|id| {
            request
                .options
                .iter()
                .find(|o| o["optionId"].as_str() == Some(id))
        }) else {
            return reject();
        };
        match option["kind"].as_str() {
            // Core owns attempt-scoped "always"; never persist it in OpenCode.
            Some("allow_once" | "allow_always") => once_option(request.options, true),
            _ => reject(),
        }
    }

    fn ask_question(&mut self, questions: &Value, cancel: &CancellationToken) -> Option<Value> {
        self.0.ask_question(questions, cancel)
    }

    fn on_update(&mut self, params: &Value) {
        self.0.on_update(params);
    }
}

fn check_agent(initialized: &Value) -> Result<()> {
    if initialized["protocolVersion"].as_u64() != Some(1)
        || initialized["agentInfo"]["name"] != "OpenCode"
    {
        bail!("OpenCode must identify itself and support ACP v1; install OpenCode V2.");
    }
    let version = initialized["agentInfo"]["version"]
        .as_str()
        .and_then(acp::parse_version)
        .context("OpenCode did not report a recognizable version; install OpenCode V2 >= 2.3.2.")?;
    if version.0 != 2 || version < (2, 3, 2) {
        bail!("This adapter requires OpenCode V2 >= 2.3.2, the oldest verified release.");
    }
    Ok(())
}

fn config() -> Value {
    let mut rules = vec![
        json!({"action":"*","resource":"*","effect":"allow"}),
        json!({"action":"*_*","resource":"*","effect":"ask"}),
        json!({"action":"formless_*","resource":"*","effect":"allow"}),
        json!({"action":"subagent","resource":"*","effect":"deny"}),
        json!({"action":"task","resource":"*","effect":"deny"}),
    ];
    let build = rules.clone();
    for action in ["edit", "shell"] {
        rules.push(json!({"action":action,"resource":"*","effect":"deny"}));
    }
    json!({
        "$schema": "https://opencode.ai/config.json",
        "default_agent": "build",
        "agents": {
            "build": {"permissions": build},
            "formless-review": {
                "mode": "primary",
                "description": "Read-only Formless review",
                "system": "Review only. Do not modify files or state. Report through the Formless MCP tools, whose role authorization remains authoritative.",
                "permissions": rules
            }
        }
    })
}

fn prepare(request: &HarnessRequest) -> Result<HarnessRequest> {
    super::validate_acp_agent(Some(request.kind), request.acp_agent.as_deref())?;
    if request.kind != super::HarnessKind::OpenCode {
        bail!("OpenCode adapter requires an OpenCode harness request.");
    }
    acp::validate(request)?;
    let mut environment = request.environment.clone();
    environment.insert("PWD".into(), request.cwd.as_os_str().to_owned());
    environment.insert(
        "OPENCODE_CONFIG_CONTENT".into(),
        config().to_string().into(),
    );
    Ok(HarnessRequest {
        kind: request.kind,
        acp_agent: request.acp_agent.clone(),
        model: request.model.clone(),
        cwd: request.cwd.clone(),
        prompt: request.prompt.clone(),
        access: request.access,
        mcp_command: request.mcp_command.clone(),
        mcp_args: request.mcp_args.clone(),
        mcp_env_vars: request.mcp_env_vars.clone(),
        executable: request.executable.clone(),
        environment,
    })
}

struct Field {
    name: String,
    custom: bool,
    question: Question,
}

fn fields(params: &Value) -> Option<Vec<Field>> {
    if params["mode"] != "form" || params["requestedSchema"]["type"] != "object" {
        return None;
    }
    let schema = &params["requestedSchema"];
    if schema.as_object()?.keys().any(|key| {
        !["type", "properties", "required", "additionalProperties"].contains(&key.as_str())
    }) || schema
        .get("required")
        .is_some_and(|r| r.as_array().is_none_or(|r| !r.is_empty()))
        || schema
            .get("additionalProperties")
            .is_some_and(|v| !v.is_boolean())
    {
        return None;
    }
    let properties = schema["properties"].as_object()?;
    let mut fields = Vec::new();
    for (name, property) in properties {
        if name.ends_with("_custom") {
            let parent = name.strip_suffix("_custom")?;
            if !properties.contains_key(parent)
                || !string_property(property)
                || property.get("oneOf").is_some()
            {
                return None;
            }
            continue;
        }
        if !string_property(property) {
            return None;
        }
        let choices = property["oneOf"].as_array()?;
        if choices.is_empty() {
            return None;
        }
        let mut options: Vec<QuestionOption> = Vec::new();
        for choice in choices {
            if choice
                .as_object()?
                .keys()
                .any(|key| !["const", "title", "description"].contains(&key.as_str()))
            {
                return None;
            }
            let label = choice["const"].as_str()?.to_owned();
            optional_string(choice, "title")?;
            if label.trim().is_empty() || options.iter().any(|o| o.label == label) {
                return None;
            }
            options.push(QuestionOption {
                label,
                description: optional_string(choice, "description")?,
            });
        }
        fields.push(Field {
            name: name.clone(),
            custom: properties.contains_key(&format!("{name}_custom")),
            question: Question {
                question: property["description"].as_str()?.to_owned(),
                header: optional_string(property, "title")?,
                options,
                multi_select: false,
            },
        });
    }
    if fields.is_empty() {
        return None;
    }
    Some(fields)
}

fn optional_string(value: &Value, name: &str) -> Option<Option<String>> {
    match value.get(name) {
        None => Some(None),
        Some(value) => value.as_str().map(|s| Some(s.to_owned())),
    }
}

fn string_property(value: &Value) -> bool {
    value["type"] == "string"
        && value.as_object().is_some_and(|p| {
            p.keys()
                .all(|key| ["type", "title", "description", "oneOf"].contains(&key.as_str()))
        })
}

fn answer(fields: &[Field], value: Value) -> Option<Value> {
    let answers: Vec<QuestionAnswer> = serde_json::from_value(value).ok()?;
    if answers.is_empty() {
        return Some(json!({"action":"cancel"}));
    }
    if answers.len() != fields.len() {
        return None;
    }
    let mut content = Map::new();
    for (field, answer) in fields.iter().zip(answers) {
        let text = answer.text.filter(|s| !s.trim().is_empty());
        if answer.selected.is_empty() && text.is_none() {
            return Some(json!({"action":"cancel"}));
        }
        if answer.selected.len() > 1
            || answer
                .selected
                .iter()
                .any(|label| !field.question.options.iter().any(|o| &o.label == label))
            || (text.is_some() && (!field.custom || !answer.selected.is_empty()))
        {
            return None;
        }
        if let Some(text) = text {
            content.insert(format!("{}_custom", field.name), json!(text));
        } else {
            content.insert(field.name.clone(), json!(answer.selected.first()?));
        }
    }
    Some(json!({"action":"accept","content":content}))
}

fn vendor_request(
    method: &str,
    params: &Value,
    handler: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Option<std::result::Result<Value, i64>> {
    if method != "elicitation/create" {
        return None;
    }
    Some(Ok(match fields(params) {
        None => json!({"action":"decline"}),
        Some(fields) if !cancel.is_cancelled() => {
            let questions: Vec<_> = fields.iter().map(|f| &f.question).collect();
            match handler.ask_question(&json!(questions), cancel) {
                Some(value) if !cancel.is_cancelled() => {
                    answer(&fields, value).unwrap_or_else(|| json!({"action":"decline"}))
                }
                _ => json!({"action":"cancel"}),
            }
        }
        Some(_) => json!({"action":"cancel"}),
    }))
}

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;
