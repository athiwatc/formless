//! Stateful harness integration over the Agent Client Protocol (ACP v1) on stdio.
//!
//! The Formless bridge is registered per session through `session/new.mcpServers`, so no
//! harness settings are written. Its environment (for example a run capability) is sent
//! over the ACP pipe and removed from the agent process environment. Each harness supplies
//! an [`AgentSpec`] with its command line, version check, sign-in hint and read-only mode.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use formless_runtime::process_group::SupervisedProcess;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{HarnessRequest, HarnessResult, HarnessRole};

const PROTOCOL_VERSION: u64 = 1;
const CANCELLED: &str = "The harness run was cancelled.";
const POLL: Duration = Duration::from_millis(50);
/// How long the agent may take to end a turn after `session/cancel` before it is killed.
#[cfg(not(test))]
const CANCEL_GRACE: Duration = Duration::from_secs(5);
#[cfg(test)]
const CANCEL_GRACE: Duration = Duration::from_millis(200);
/// ACP tool kinds a read-only run may approve; every other kind is rejected.
const READ_ONLY_TOOL_KINDS: &[&str] = &["read", "search", "think", "fetch"];

/// How one harness is launched and checked over ACP.
pub struct AgentSpec {
    /// Harness name used in errors, e.g. "Antigravity".
    pub name: &'static str,
    /// Executable used when the request names none.
    pub program: &'static str,
    /// Command-line arguments that start the ACP stdio server for this request.
    pub args: fn(&HarnessRequest) -> Vec<OsString>,
    /// Validates the `initialize` result (protocol, version, capabilities).
    pub check_agent: fn(&Value) -> Result<()>,
    /// Returned when `session/new` reports one of `auth_error_codes`.
    pub not_signed_in: &'static str,
    /// JSON-RPC session errors that indicate missing authentication or configuration.
    pub auth_error_codes: &'static [i64],
    /// Read-only session modes for review runs, most preferred first. The first one the
    /// session advertises (a `mode` config option, or `modes.availableModes`) is selected;
    /// if none is advertised, `session/set_mode` is tried with the first candidate.
    pub read_only_modes: &'static [&'static str],
    /// Fail a review run when no read-only mode can be selected. When false, the run
    /// relies on the spec's arguments (such as a sandbox) and the read-only tool allow-list.
    pub read_only_fail_closed: bool,
    /// Extra variables removed from the agent environment, besides the bridge's.
    pub strip_env: &'static [&'static str],
    /// Variables set on the agent process after the request environment, e.g. to turn
    /// off a self-updater.
    pub set_env: &'static [(&'static str, &'static str)],
    /// Identifies approval requests for the Formless bridge's own tools, which review
    /// runs may approve whatever their kind.
    pub bridge_tool: fn(&Value) -> bool,
    /// Serves agent requests the shared loop does not handle, such as a vendor question
    /// method. `None` answers JSON-RPC -32601; `Some` is sent as the result or error.
    pub vendor_request: VendorRequest,
}

/// See [`AgentSpec::vendor_request`].
pub type VendorRequest = fn(
    method: &str,
    params: &Value,
    handler: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Option<std::result::Result<Value, i64>>;

/// For [`AgentSpec::vendor_request`] when the harness sends no vendor requests.
pub fn no_vendor_requests(
    _: &str,
    _: &Value,
    _: &mut dyn PermissionHandler,
    _: &CancellationToken,
) -> Option<std::result::Result<Value, i64>> {
    None
}

/// For [`AgentSpec::bridge_tool`] when the harness gives no reliable way to tell.
pub fn no_bridge_tools(_: &Value) -> bool {
    false
}

/// A `session/request_permission` from the agent, passed through to the user.
pub struct PermissionRequest<'a> {
    pub tool_call: &'a Value,
    pub options: &'a [Value],
    /// The run's cancellation token.
    pub cancel: &'a CancellationToken,
}

/// Decides harness tool approvals. Returns the chosen `optionId`, or `None` to cancel.
/// A handler that waits for a person must stop waiting and return `None` once
/// `request.cancel` fires; the run cannot be cancelled while `decide` blocks.
pub trait PermissionHandler {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String>;

    /// Receives each `session/update` (`{sessionId, update}`) as it streams in.
    fn on_update(&mut self, _update: &Value) {}

    /// Asks the user a harness question translated by a spec's `vendor_request`.
    /// Returns the answer, or `None` when unanswered or once `cancel` fires.
    fn ask_question(&mut self, _question: &Value, _cancel: &CancellationToken) -> Option<Value> {
        None
    }
}

/// Rejects every approval request; used when no user is available to answer.
pub struct RejectAll;

impl PermissionHandler for RejectAll {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String> {
        reject_option(request.options)
    }
}

/// Runs one prompt in a new ACP session with the Formless bridge attached.
///
/// When `cancel` fires during the turn, Formless sends ACP `session/cancel`, answers any
/// further approval request as cancelled, and returns an error once the turn ends or after
/// a short grace period. The agent and every process it started are killed when this returns.
pub fn run(
    spec: &AgentSpec,
    request: &HarnessRequest,
    permissions: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    let mut agent = AgentProcess::spawn(spec, request)?;
    let (reader, writer) = agent.pipes()?;
    drive(spec, reader, writer, request, permissions, cancel, true)
}

/// Negotiates ACP and opens a session with the Formless bridge attached, without prompting.
/// Returns the agent's session id.
pub fn open_session(spec: &AgentSpec, request: &HarnessRequest) -> Result<String> {
    let mut agent = AgentProcess::spawn(spec, request)?;
    let (reader, writer) = agent.pipes()?;
    drive(
        spec,
        reader,
        writer,
        request,
        &mut RejectAll,
        &CancellationToken::new(),
        false,
    )
    .map(|result| {
        result
            .session_id
            .expect("opened sessions always have an id")
    })
}

pub(super) fn validate(request: &HarnessRequest) -> Result<()> {
    if request.prompt.trim().is_empty() {
        bail!("harness prompt must not be empty");
    }
    if !request.cwd.is_absolute() {
        bail!("harness working directory must be absolute");
    }
    if !request.mcp_command.is_absolute() {
        bail!("MCP server executable must be an absolute path");
    }
    request
        .mcp_command
        .to_str()
        .context("MCP executable path must be valid UTF-8")?;
    request
        .cwd
        .to_str()
        .context("harness working directory must be valid UTF-8")?;
    if request
        .model
        .as_deref()
        .is_some_and(|model| model.trim().is_empty())
    {
        bail!("harness model must not be empty");
    }
    for name in &request.mcp_env_vars {
        let value = request
            .environment
            .get(&OsString::from(name))
            .with_context(|| format!("MCP environment variable {name} has no value"))?;
        value
            .to_str()
            .with_context(|| format!("MCP environment variable {name} must be valid UTF-8"))?;
    }
    Ok(())
}

/// The agent process environment: the request environment without the bridge's variables.
pub(super) fn agent_environment(request: &HarnessRequest) -> BTreeMap<OsString, OsString> {
    let mut environment = request.environment.clone();
    for name in &request.mcp_env_vars {
        environment.remove(&OsString::from(name));
    }
    environment
}

/// The agent and every process it starts (MCP servers, tools); dropping it kills them all.
pub(super) struct AgentProcess(SupervisedProcess);

impl AgentProcess {
    pub(super) fn spawn(spec: &AgentSpec, request: &HarnessRequest) -> Result<Self> {
        validate(request)?;
        let program = request
            .executable
            .clone()
            .unwrap_or_else(|| PathBuf::from(spec.program));
        let mut command = Command::new(&program);
        command
            .args((spec.args)(request))
            .current_dir(&request.cwd)
            .env_clear()
            .envs(std::env::vars_os().filter(|(name, _)| {
                !request
                    .mcp_env_vars
                    .iter()
                    .map(String::as_str)
                    .chain(spec.strip_env.iter().copied())
                    .any(|var| name.as_os_str() == std::ffi::OsStr::new(var))
            }))
            .envs(agent_environment(request).into_iter().filter(|(name, _)| {
                !spec
                    .strip_env
                    .iter()
                    .any(|var| name.as_os_str() == std::ffi::OsStr::new(var))
            }))
            .envs(spec.set_env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let process = SupervisedProcess::spawn(command).map_err(|error| {
            anyhow!(
                "failed to start {} ({error}); is it installed and on PATH?",
                spec.name
            )
        })?;
        Ok(Self(process))
    }

    pub(super) fn pipes(&mut self) -> Result<(BufReader<ChildStdout>, ChildStdin)> {
        let (stdin, stdout, _) = self.0.take_pipes();
        let stdout = stdout.context("agent stdout unavailable")?;
        let stdin = stdin.context("agent stdin unavailable")?;
        Ok((BufReader::new(stdout), stdin))
    }
}

pub(super) fn drive<R: BufRead + Send + 'static, W: Write>(
    spec: &AgentSpec,
    reader: R,
    writer: W,
    request: &HarnessRequest,
    permissions: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
    prompt: bool,
) -> Result<HarnessResult> {
    validate(request)?;
    let mut rpc = Rpc {
        lines: read_lines(reader),
        writer,
        cancel: cancel.clone(),
        name: spec.name,
        bridge_tool: spec.bridge_tool,
        vendor_request: spec.vendor_request,
        prompting: false,
        cancel_deadline: None,
        next_id: 0,
        events: Vec::new(),
        message: String::new(),
        session_id: None,
        role: request.role,
    };

    let initialized = rpc
        .call(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": {"readTextFile": false, "writeTextFile": false},
                    "terminal": false
                },
                "clientInfo": {"name": "formless", "version": env!("CARGO_PKG_VERSION")}
            }),
            permissions,
        )?
        .map_err(|code| {
            anyhow!(
                "{} rejected ACP initialize (JSON-RPC error {code}).",
                spec.name
            )
        })?;
    (spec.check_agent)(&initialized)?;

    let env: Vec<Value> = request
        .mcp_env_vars
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "value": request.environment[&OsString::from(name)].to_str().expect("validated"),
            })
        })
        .collect();
    let session = rpc
        .call(
            "session/new",
            json!({
                "cwd": request.cwd.to_str().expect("validated"),
                "mcpServers": [{
                    "name": "formless",
                    "command": request.mcp_command.to_str().expect("validated"),
                    "args": request.mcp_args,
                    "env": env,
                }]
            }),
            permissions,
        )?
        .map_err(|code| match code {
            code if spec.auth_error_codes.contains(&code) => {
                let methods: Vec<&str> = initialized
                    .get("authMethods")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|method| method.get("id").and_then(Value::as_str))
                    .collect();
                if methods.is_empty() {
                    anyhow!(spec.not_signed_in)
                } else {
                    anyhow!(
                        "{} (sign-in methods it offers: {}.)",
                        spec.not_signed_in,
                        methods.join(", ")
                    )
                }
            }
            code => anyhow!(
                "{} rejected session/new with the Formless MCP server (JSON-RPC error {code}).",
                spec.name
            ),
        })?;
    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .with_context(|| {
            format!(
                "{} session/new response is missing its sessionId.",
                spec.name
            )
        })?
        .to_owned();
    rpc.session_id = Some(session_id.clone());

    if request.role == HarnessRole::Review {
        select_read_only_mode(spec, &mut rpc, &session, &session_id, permissions)?;
    }
    if let Some(model) = &request.model {
        let option = find_config_option(&session, "model", model).with_context(|| {
            format!(
                "This {} session does not offer the requested model over ACP.",
                spec.name
            )
        })?;
        rpc.set_config(&session_id, &option, model, permissions)?;
    }

    if !prompt {
        return Ok(HarnessResult {
            session_id: Some(session_id),
            message: String::new(),
            events: rpc.events,
        });
    }

    rpc.message.clear();
    let turn = rpc
        .call(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": request.prompt}]
            }),
            permissions,
        )?
        .map_err(|code| anyhow!("{} prompt turn failed (JSON-RPC error {code}).", spec.name))?;
    match turn.get("stopReason").and_then(Value::as_str) {
        _ if rpc.cancel.is_cancelled() => bail!(CANCELLED),
        Some("end_turn") => {}
        Some(reason @ ("max_tokens" | "max_turn_requests" | "refusal" | "cancelled")) => {
            bail!(
                "{} ended the turn without completing it (stopReason {reason}).",
                spec.name
            )
        }
        _ => bail!(
            "{} prompt response has no recognised stopReason.",
            spec.name
        ),
    }
    if rpc.message.trim().is_empty() {
        bail!(
            "{} completed the turn without a response message.",
            spec.name
        );
    }
    Ok(HarnessResult {
        session_id: Some(session_id),
        message: rpc.message,
        events: rpc.events,
    })
}

fn select_read_only_mode<W: Write>(
    spec: &AgentSpec,
    rpc: &mut Rpc<W>,
    session: &Value,
    session_id: &str,
    permissions: &mut dyn PermissionHandler,
) -> Result<()> {
    let refused = |detail: String| {
        anyhow!(
            "{} {detail}, so Formless cannot keep a review session read-only.",
            spec.name
        )
    };
    let available_modes: Vec<&str> = session
        .pointer("/modes/availableModes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|mode| mode.get("id").and_then(Value::as_str))
        .collect();
    for &mode in spec.read_only_modes {
        if let Some(option) = find_config_option(session, "mode", mode) {
            return rpc.set_config(session_id, &option, mode, permissions);
        }
        if available_modes.contains(&mode) {
            return set_mode(rpc, session_id, mode, permissions).map_err(|code| {
                refused(format!("refused its `{mode}` mode (JSON-RPC error {code})"))
            });
        }
    }
    let attempt = match spec.read_only_modes.first() {
        Some(&mode) => set_mode(rpc, session_id, mode, permissions)
            .map_err(|code| format!("refused its `{mode}` mode (JSON-RPC error {code})")),
        None => Err("has no read-only mode over ACP".to_owned()),
    };
    match attempt {
        Err(detail) if spec.read_only_fail_closed => Err(refused(detail)),
        _ => Ok(()),
    }
}

fn set_mode<W: Write>(
    rpc: &mut Rpc<W>,
    session_id: &str,
    mode: &str,
    permissions: &mut dyn PermissionHandler,
) -> std::result::Result<(), String> {
    match rpc.call(
        "session/set_mode",
        json!({"sessionId": session_id, "modeId": mode}),
        permissions,
    ) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(code)) => Err(code.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

/// Parses `major.minor.patch`, ignoring any pre-release or build suffix.
pub fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// The id of a select option offering `value`, preferring its id over its category.
fn find_config_option(session: &Value, category: &str, value: &str) -> Option<String> {
    session
        .get("configOptions")?
        .as_array()?
        .iter()
        .filter(|option| option.get("id").and_then(Value::as_str) == Some(category))
        .chain(
            session
                .get("configOptions")?
                .as_array()?
                .iter()
                .filter(|option| {
                    option.get("id").and_then(Value::as_str) != Some(category)
                        && option.get("category").and_then(Value::as_str) == Some(category)
                }),
        )
        .find(|option| {
            option
                .get("options")
                .and_then(Value::as_array)
                .is_some_and(|choices| {
                    choices.iter().any(|choice| {
                        choice.get("value").and_then(Value::as_str) == Some(value)
                            || choice.get("options").and_then(Value::as_array).is_some_and(
                                |group| {
                                    group.iter().any(|choice| {
                                        choice.get("value").and_then(Value::as_str) == Some(value)
                                    })
                                },
                            )
                    })
                })
        })?
        .get("id")?
        .as_str()
        .map(str::to_owned)
}

fn reject_option(options: &[Value]) -> Option<String> {
    options
        .iter()
        .find(|option| option.get("kind").and_then(Value::as_str) == Some("reject_once"))
        .or_else(|| {
            options
                .iter()
                .find(|option| option.get("kind").and_then(Value::as_str) == Some("reject_always"))
        })
        .and_then(|option| option.get("optionId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Reads agent lines on a thread so the client can notice cancellation while waiting.
fn read_lines<R: BufRead + Send + 'static>(
    mut reader: R,
) -> mpsc::Receiver<std::io::Result<String>> {
    let (sender, lines) = mpsc::channel();
    std::thread::spawn(move || loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if sender.send(Ok(line)).is_err() {
                    break;
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error));
                break;
            }
        }
    });
    lines
}

struct Rpc<W> {
    lines: mpsc::Receiver<std::io::Result<String>>,
    writer: W,
    cancel: CancellationToken,
    name: &'static str,
    bridge_tool: fn(&Value) -> bool,
    vendor_request: VendorRequest,
    prompting: bool,
    cancel_deadline: Option<Instant>,
    next_id: u64,
    events: Vec<Value>,
    message: String,
    session_id: Option<String>,
    role: HarnessRole,
}

impl<W: Write> Rpc<W> {
    fn send(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .and_then(|()| self.writer.flush())
            .with_context(|| format!("{} closed its input", self.name))
    }

    fn set_config(
        &mut self,
        session_id: &str,
        config_id: &str,
        value: &str,
        permissions: &mut dyn PermissionHandler,
    ) -> Result<()> {
        self.call(
            "session/set_config_option",
            json!({"sessionId": session_id, "configId": config_id, "value": value}),
            permissions,
        )?
        .map_err(|code| {
            anyhow!(
                "{} rejected setting {config_id} (JSON-RPC error {code}).",
                self.name
            )
        })?;
        Ok(())
    }

    /// Sends a request and serves agent traffic until its response. `Err(code)` is a JSON-RPC error.
    fn call(
        &mut self,
        method: &str,
        params: Value,
        permissions: &mut dyn PermissionHandler,
    ) -> Result<std::result::Result<Value, i64>> {
        if self.cancel.is_cancelled() {
            bail!(CANCELLED);
        }
        self.next_id += 1;
        let id = self.next_id;
        self.prompting = method == "session/prompt";
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            self.check_cancel()?;
            let line = match self.lines.recv_timeout(POLL) {
                Ok(line) => line.with_context(|| format!("failed to read from {}", self.name))?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) if self.cancel.is_cancelled() => {
                    bail!(CANCELLED)
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("{} exited before answering {method}.", self.name)
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(&line)
                .map_err(|_| anyhow!("{} sent a malformed JSON-RPC message.", self.name))?;
            match (
                message.get("method").and_then(Value::as_str),
                message.get("id"),
            ) {
                (Some(agent_method), Some(request_id)) => {
                    let request_id = request_id.clone();
                    let response = self.serve(agent_method, &message, permissions);
                    self.send(&match response {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": request_id, "result": result}),
                        Err(code) => json!({
                            "jsonrpc": "2.0",
                            "id": request_id,
                            "error": {"code": code, "message": "not supported by Formless"}
                        }),
                    })?;
                }
                (Some("session/update"), None) => self.record_update(message, permissions)?,
                (Some(_), None) => self.events.push(message),
                (None, Some(response_id)) => {
                    if response_id.as_u64() != Some(id) {
                        bail!("{} answered a request Formless did not send.", self.name);
                    }
                    if let Some(result) = message.get("result") {
                        return Ok(Ok(result.clone()));
                    }
                    let code = message
                        .pointer("/error/code")
                        .and_then(Value::as_i64)
                        .with_context(|| {
                            format!(
                                "{} sent a response with neither result nor error.",
                                self.name
                            )
                        })?;
                    return Ok(Err(code));
                }
                (None, None) => bail!(
                    "{} sent a JSON-RPC message without method or id.",
                    self.name
                ),
            }
        }
    }

    /// Asks the agent to end the turn once cancelled, and gives up after the grace period.
    fn check_cancel(&mut self) -> Result<()> {
        if !self.cancel.is_cancelled() {
            return Ok(());
        }
        let Some(session_id) = self.session_id.clone().filter(|_| self.prompting) else {
            bail!(CANCELLED);
        };
        match self.cancel_deadline {
            None => {
                self.send(&json!({
                    "jsonrpc": "2.0",
                    "method": "session/cancel",
                    "params": {"sessionId": session_id}
                }))?;
                self.cancel_deadline = Some(Instant::now() + CANCEL_GRACE);
            }
            Some(deadline) if Instant::now() >= deadline => bail!(CANCELLED),
            Some(_) => {}
        }
        Ok(())
    }

    fn record_update(
        &mut self,
        message: Value,
        permissions: &mut dyn PermissionHandler,
    ) -> Result<()> {
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if let Some(session_id) = &self.session_id {
            if params.get("sessionId").and_then(Value::as_str) != Some(session_id) {
                bail!("{} sent an update for a different session.", self.name);
            }
        }
        let update = &params["update"];
        if update["sessionUpdate"] == "agent_message_chunk" && update["content"]["type"] == "text" {
            if let Some(text) = update["content"]["text"].as_str() {
                self.message.push_str(text);
            }
        }
        permissions.on_update(&params);
        self.events.push(params);
        Ok(())
    }

    fn serve(
        &mut self,
        method: &str,
        message: &Value,
        permissions: &mut dyn PermissionHandler,
    ) -> std::result::Result<Value, i64> {
        if method != "session/request_permission" {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            self.events
                .push(json!({"agentRequest": {"method": method, "params": params}}));
            return (self.vendor_request)(method, &params, permissions, &self.cancel)
                .unwrap_or(Err(-32601));
        }
        let params = message.get("params").ok_or(-32602)?;
        self.events.push(json!({"permissionRequest": params}));
        let tool_call = params.get("toolCall").ok_or(-32602)?;
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .ok_or(-32602)?;
        let read_only_safe = tool_call
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| READ_ONLY_TOOL_KINDS.contains(&kind))
            || (self.bridge_tool)(tool_call);
        let choice = if self.cancel.is_cancelled() {
            None
        } else if self.role == HarnessRole::Review && !read_only_safe {
            reject_option(options)
        } else {
            permissions
                .decide(&PermissionRequest {
                    tool_call,
                    options,
                    cancel: &self.cancel,
                })
                .filter(|id| {
                    options
                        .iter()
                        .any(|option| option.get("optionId").and_then(Value::as_str) == Some(id))
                })
        };
        Ok(match choice {
            Some(option_id) => json!({"outcome": {"outcome": "selected", "optionId": option_id}}),
            None => json!({"outcome": {"outcome": "cancelled"}}),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_harness::antigravity::{MIN_AGY_VERSION, SPEC};
    use crate::local_harness::HarnessKind;
    use std::io::Cursor;

    // Captured from the released `agy acp` 1.5.14 `initialize` response (live, unauthenticated).
    const LIVE_INITIALIZE: &str = r#"{"jsonrpc":"2.0","result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":true,"audio":false,"embeddedContext":true},"sessionCapabilities":{"resume":{},"close":{},"load":{},"list":{},"additionalDirectories":{}}},"agentInfo":{"name":"Antigravity ACP Server","version":"1.5.14"},"authMethods":[{"id":"oauth-personal","name":"Log in with Google"}]},"id":1}"#;
    // Captured from the released `agy acp` 1.5.14 `session/new` response without a sign-in.
    const LIVE_AUTH_REQUIRED: &str =
        r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"Authentication required"},"id":2}"#;

    // Fixture (not observed live): ACP v1 schema shapes for an authenticated session.
    const SESSION_NEW: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1","configOptions":[{"id":"mode","category":"mode","type":"select","currentValue":"accept-edits","options":[{"value":"accept-edits","name":"Accept edits"},{"value":"plan","name":"Plan"}]},{"id":"model","category":"model","type":"select","currentValue":"m1","options":[{"value":"m1","name":"M1"},{"value":"model-secret","name":"Requested"}]}]}}"#;

    fn request(role: HarnessRole) -> HarnessRequest {
        HarnessRequest {
            kind: HarnessKind::Antigravity,
            model: None,
            cwd: PathBuf::from("/work/repo"),
            prompt: "prompt text".into(),
            role,
            mcp_command: PathBuf::from("/opt/formless/bin/formless"),
            mcp_args: vec!["mcp".into(), "--run".into(), "run-7".into()],
            mcp_env_vars: vec!["FORMLESS_TOPIC_CAPABILITY".into()],
            executable: None,
            environment: BTreeMap::from([
                ("FORMLESS_TOPIC_CAPABILITY".into(), "cap-secret".into()),
                ("HOME".into(), "/home/user".into()),
            ]),
        }
    }

    fn update(text: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"sess-1","update":{{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":"{text}"}}}}}}}}"#
        )
    }

    fn permission(id: u64, kind: &str) -> String {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"session/request_permission","params":{{"sessionId":"sess-1","toolCall":{{"toolCallId":"call-{id}","kind":"{kind}","title":"tool"}},"options":[{{"optionId":"allow","name":"Allow once","kind":"allow_once"}},{{"optionId":"deny","name":"Reject","kind":"reject_once"}}]}}}}"#
        )
    }

    fn stop(id: u64, reason: &str) -> String {
        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"stopReason":"{reason}"}}}}"#)
    }

    fn ok(id: u64) -> String {
        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{}}}}"#)
    }

    struct Choose(&'static str, usize);

    impl PermissionHandler for Choose {
        fn decide(&mut self, _: &PermissionRequest<'_>) -> Option<String> {
            self.1 += 1;
            Some(self.0.into())
        }
    }

    fn drive_lines(
        request: &HarnessRequest,
        lines: &[String],
        permissions: &mut dyn PermissionHandler,
    ) -> (Result<HarnessResult>, Vec<Value>) {
        drive_spec(&SPEC, request, lines, permissions)
    }

    fn drive_spec(
        spec: &AgentSpec,
        request: &HarnessRequest,
        lines: &[String],
        permissions: &mut dyn PermissionHandler,
    ) -> (Result<HarnessResult>, Vec<Value>) {
        let input = lines.join("\n") + "\n";
        let mut written = Vec::new();
        let result = drive(
            spec,
            Cursor::new(input),
            &mut written,
            request,
            permissions,
            &CancellationToken::new(),
            true,
        );
        let sent = String::from_utf8(written)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (result, sent)
    }

    #[test]
    fn registers_bridge_per_session_and_collects_the_turn() {
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            update("Hello "),
            permission(90, "read"),
            update("world"),
            stop(3, "end_turn"),
        ];
        let mut handler = Choose("allow", 0);
        let (result, sent) = drive_lines(&request(HarnessRole::Build), &lines, &mut handler);
        let result = result.unwrap();
        assert_eq!(result.session_id.as_deref(), Some("sess-1"));
        assert_eq!(result.message, "Hello world");
        assert_eq!(handler.1, 1);

        assert_eq!(sent[0]["method"], "initialize");
        assert_eq!(sent[0]["params"]["protocolVersion"], 1);
        assert_eq!(sent[1]["method"], "session/new");
        assert_eq!(
            sent[1]["params"],
            json!({
                "cwd": "/work/repo",
                "mcpServers": [{
                    "name": "formless",
                    "command": "/opt/formless/bin/formless",
                    "args": ["mcp", "--run", "run-7"],
                    "env": [{"name": "FORMLESS_TOPIC_CAPABILITY", "value": "cap-secret"}]
                }]
            })
        );
        assert_eq!(sent[2]["method"], "session/prompt");
        assert_eq!(sent[2]["params"]["prompt"][0]["text"], "prompt text");
        assert_eq!(
            sent[3],
            json!({"jsonrpc": "2.0", "id": 90, "result": {"outcome": {"outcome": "selected", "optionId": "allow"}}})
        );
        let events = serde_json::to_string(&result.events).unwrap();
        assert!(!events.contains("cap-secret"));
    }

    #[test]
    fn default_handler_rejects_approvals() {
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            permission(90, "edit"),
            update("done"),
            stop(3, "end_turn"),
        ];
        let (result, sent) = drive_lines(&request(HarnessRole::Build), &lines, &mut RejectAll);
        result.unwrap();
        assert_eq!(sent[3]["result"]["outcome"]["optionId"], "deny");
    }

    #[test]
    fn unknown_option_from_handler_is_sent_as_cancelled() {
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            permission(90, "read"),
            update("done"),
            stop(3, "end_turn"),
        ];
        let (_, sent) = drive_lines(
            &request(HarnessRole::Build),
            &lines,
            &mut Choose("not-offered", 0),
        );
        assert_eq!(sent[3]["result"]["outcome"]["outcome"], "cancelled");
    }

    #[test]
    fn review_sets_plan_mode_and_rejects_writes_without_asking() {
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            ok(3),
            permission(90, "edit"),
            update("looks good"),
            stop(4, "end_turn"),
        ];
        let mut handler = Choose("allow", 0);
        let (result, sent) = drive_lines(&request(HarnessRole::Review), &lines, &mut handler);
        result.unwrap();
        assert_eq!(
            sent[2],
            json!({"jsonrpc": "2.0", "id": 3, "method": "session/set_config_option", "params": {"sessionId": "sess-1", "configId": "mode", "value": "plan"}})
        );
        assert_eq!(sent[4]["result"]["outcome"]["optionId"], "deny");
        assert_eq!(handler.1, 0);
    }

    #[test]
    fn review_fails_closed_without_plan_mode() {
        let session = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}"#;
        let refused =
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"unknown mode"}}"#;
        let lines = [LIVE_INITIALIZE.into(), session.into(), refused.into()];
        let (result, sent) = drive_lines(&request(HarnessRole::Review), &lines, &mut RejectAll);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cannot keep a review session read-only"));
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[2]["method"], "session/set_mode");
        assert_eq!(
            sent[2]["params"],
            json!({"sessionId": "sess-1", "modeId": "plan"})
        );
    }

    #[test]
    fn vendor_requests_reach_the_spec_and_its_question_handler() {
        struct Answer;
        impl PermissionHandler for Answer {
            fn decide(&mut self, _: &PermissionRequest<'_>) -> Option<String> {
                None
            }
            fn ask_question(&mut self, question: &Value, _: &CancellationToken) -> Option<Value> {
                Some(json!({"answer": question["question"]}))
            }
        }
        fn ask(
            method: &str,
            params: &Value,
            handler: &mut dyn PermissionHandler,
            cancel: &CancellationToken,
        ) -> Option<std::result::Result<Value, i64>> {
            (method == "_vendor/ask").then(|| handler.ask_question(params, cancel).ok_or(-32000))
        }
        let spec = AgentSpec {
            vendor_request: ask,
            ..SPEC
        };
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            r#"{"jsonrpc":"2.0","id":80,"method":"_vendor/ask","params":{"question":"which?"}}"#
                .into(),
            r#"{"jsonrpc":"2.0","id":81,"method":"_vendor/other","params":{}}"#.into(),
            update("ok"),
            stop(3, "end_turn"),
        ];
        let (result, sent) = drive_spec(&spec, &request(HarnessRole::Build), &lines, &mut Answer);
        result.unwrap();
        let reply = |id: u64| {
            sent.iter()
                .find(|message| message["id"] == id)
                .unwrap()
                .clone()
        };
        assert_eq!(reply(80)["result"], json!({"answer": "which?"}));
        assert_eq!(reply(81)["error"]["code"], -32601);
    }

    #[test]
    fn streams_each_update_to_the_handler() {
        struct Collect(Vec<Value>);
        impl PermissionHandler for Collect {
            fn decide(&mut self, _: &PermissionRequest<'_>) -> Option<String> {
                None
            }
            fn on_update(&mut self, update: &Value) {
                self.0.push(update.clone());
            }
        }
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            update("a"),
            update("b"),
            stop(3, "end_turn"),
        ];
        let mut handler = Collect(Vec::new());
        let (result, _) = drive_lines(&request(HarnessRole::Build), &lines, &mut handler);
        assert_eq!(result.unwrap().message, "ab");
        let texts: Vec<_> = handler
            .0
            .iter()
            .map(|update| update["update"]["content"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(texts, ["a", "b"]);
    }

    #[test]
    fn review_selects_first_advertised_mode_candidate() {
        let spec = AgentSpec {
            read_only_modes: &["read-only", "plan"],
            ..SPEC
        };
        let session = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1","modes":{"currentModeId":"code","availableModes":[{"id":"code"},{"id":"plan"}]}}}"#;
        let lines = [
            LIVE_INITIALIZE.into(),
            session.into(),
            ok(3),
            update("done"),
            stop(4, "end_turn"),
        ];
        let (result, sent) =
            drive_spec(&spec, &request(HarnessRole::Review), &lines, &mut RejectAll);
        result.unwrap();
        assert_eq!(sent[2]["method"], "session/set_mode");
        assert_eq!(sent[2]["params"]["modeId"], "plan");
    }

    #[test]
    fn review_without_mode_continues_only_when_spec_allows() {
        let spec = AgentSpec {
            read_only_modes: &[],
            read_only_fail_closed: false,
            ..SPEC
        };
        let session = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}"#;
        let lines = [
            LIVE_INITIALIZE.into(),
            session.into(),
            update("done"),
            stop(3, "end_turn"),
        ];
        let (result, sent) =
            drive_spec(&spec, &request(HarnessRole::Review), &lines, &mut RejectAll);
        result.unwrap();
        assert_eq!(sent[2]["method"], "session/prompt");

        let strict = AgentSpec {
            read_only_modes: &[],
            ..SPEC
        };
        let (result, _) = drive_spec(
            &strict,
            &request(HarnessRole::Review),
            &lines,
            &mut RejectAll,
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cannot keep a review session read-only"));
    }

    #[test]
    fn review_falls_back_to_session_set_mode() {
        let session = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}"#;
        let lines = [
            LIVE_INITIALIZE.into(),
            session.into(),
            ok(3),
            permission(90, "other"),
            update("done"),
            stop(4, "end_turn"),
        ];
        let mut handler = Choose("allow", 0);
        let (result, sent) = drive_lines(&request(HarnessRole::Review), &lines, &mut handler);
        result.unwrap();
        assert_eq!(sent[2]["method"], "session/set_mode");
        assert_eq!(handler.1, 0, "review runs never ask about non-read tools");
        let answer = sent.iter().find(|message| message["id"] == 90).unwrap();
        assert_eq!(answer["result"]["outcome"]["optionId"], "deny");
    }

    #[test]
    fn selects_requested_model_or_fails() {
        let mut with_model = request(HarnessRole::Build);
        with_model.model = Some("model-secret".into());
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            ok(3),
            update("hi"),
            stop(4, "end_turn"),
        ];
        let (result, sent) = drive_lines(&with_model, &lines, &mut RejectAll);
        result.unwrap();
        assert_eq!(sent[2]["params"]["configId"], "model");
        assert_eq!(sent[2]["params"]["value"], "model-secret");

        with_model.model = Some("missing-model".into());
        let (result, _) = drive_lines(&with_model, &lines[..2], &mut RejectAll);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("does not offer the requested model"));
    }

    #[test]
    fn unauthenticated_session_asks_user_to_sign_in_with_agy() {
        let lines = [LIVE_INITIALIZE.into(), LIVE_AUTH_REQUIRED.into()];
        let (result, sent) = drive_lines(&request(HarnessRole::Build), &lines, &mut RejectAll);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("not signed in"));
        assert!(error.contains("Run `agy`"));
        assert!(error.contains("oauth-personal"), "{error}");
        assert!(!error.contains("cap-secret"));
        assert!(!sent
            .iter()
            .any(|message| message["method"] == "authenticate"));
    }

    #[test]
    fn rejects_unsupported_agent_versions() {
        for (initialize, expected) in [
            (
                LIVE_INITIALIZE.replace("1.5.14", "1.5.13"),
                "older than 1.5.14",
            ),
            (
                LIVE_INITIALIZE.replace(r#""protocolVersion":1"#, r#""protocolVersion":2"#),
                "does not speak ACP v1",
            ),
            (
                LIVE_INITIALIZE.replace("1.5.14", "nightly"),
                "recognisable agentInfo version",
            ),
        ] {
            let (result, sent) =
                drive_lines(&request(HarnessRole::Build), &[initialize], &mut RejectAll);
            assert!(
                result.unwrap_err().to_string().contains(expected),
                "{expected}"
            );
            assert_eq!(sent.len(), 1);
        }
        assert_eq!(parse_version("1.6.0-beta+5"), Some((1, 6, 0)));
        assert!(parse_version("1.6.0").unwrap() > MIN_AGY_VERSION);
    }

    #[test]
    fn incomplete_turns_are_errors() {
        for (lines, expected) in [
            (
                vec![update("partial"), stop(3, "refusal")],
                "stopReason refusal",
            ),
            (vec![stop(3, "end_turn")], "without a response message"),
            (
                vec![update("partial")],
                "exited before answering session/prompt",
            ),
            (vec!["not json".into()], "malformed"),
            (vec![stop(7, "end_turn")], "did not send"),
            (
                vec![update("x").replace("sess-1", "other")],
                "different session",
            ),
        ] {
            let mut all = vec![LIVE_INITIALIZE.to_string(), SESSION_NEW.to_string()];
            all.extend(lines);
            let (result, _) = drive_lines(&request(HarnessRole::Build), &all, &mut RejectAll);
            assert!(
                result.unwrap_err().to_string().contains(expected),
                "{expected}"
            );
        }
    }

    #[test]
    fn unsupported_client_methods_get_method_not_found() {
        let read = r#"{"jsonrpc":"2.0","id":91,"method":"fs/read_text_file","params":{"sessionId":"sess-1","path":"/etc/passwd"}}"#;
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            read.into(),
            update("ok"),
            stop(3, "end_turn"),
        ];
        let (result, sent) = drive_lines(&request(HarnessRole::Build), &lines, &mut RejectAll);
        result.unwrap();
        assert_eq!(sent[3]["id"], 91);
        assert_eq!(sent[3]["error"]["code"], -32601);
    }

    /// Cancels the run when asked for its first approval, then declines it.
    struct CancelOnAsk(CancellationToken);

    impl PermissionHandler for CancelOnAsk {
        fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String> {
            assert!(!request.cancel.is_cancelled());
            self.0.cancel();
            assert!(request.cancel.is_cancelled());
            Some("allow".into())
        }
    }

    fn drive_cancellable(
        lines: &[String],
        permissions: &mut dyn PermissionHandler,
        cancel: &CancellationToken,
        keep_open: bool,
    ) -> (Result<HarnessResult>, Vec<Value>) {
        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        for line in lines {
            sender.send(format!("{line}\n").into_bytes()).unwrap();
        }
        if !keep_open {
            drop(sender);
        } else {
            std::mem::forget(sender);
        }
        struct Channel(mpsc::Receiver<Vec<u8>>, Vec<u8>);
        impl std::io::Read for Channel {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.1.is_empty() {
                    match self.0.recv() {
                        Ok(bytes) => self.1 = bytes,
                        Err(_) => return Ok(0),
                    }
                }
                let n = buf.len().min(self.1.len());
                buf[..n].copy_from_slice(&self.1[..n]);
                self.1.drain(..n);
                Ok(n)
            }
        }
        let mut written = Vec::new();
        let result = drive(
            &SPEC,
            std::io::BufReader::new(Channel(receiver, Vec::new())),
            &mut written,
            &request(HarnessRole::Build),
            permissions,
            cancel,
            true,
        );
        let sent = String::from_utf8(written)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (result, sent)
    }

    #[test]
    fn cancelling_a_turn_sends_session_cancel_and_declines_approvals() {
        let cancel = CancellationToken::new();
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            permission(90, "read"),
            permission(91, "read"),
            stop(3, "cancelled"),
        ];
        let mut handler = CancelOnAsk(cancel.clone());
        let (result, sent) = drive_cancellable(&lines, &mut handler, &cancel, false);
        assert_eq!(result.unwrap_err().to_string(), CANCELLED);
        let cancels: Vec<_> = sent
            .iter()
            .filter(|message| message["method"] == "session/cancel")
            .collect();
        assert_eq!(cancels.len(), 1);
        assert_eq!(cancels[0]["params"]["sessionId"], "sess-1");
        assert!(cancels[0].get("id").is_none());
        let answer = sent.iter().find(|message| message["id"] == 91).unwrap();
        assert_eq!(answer["result"]["outcome"]["outcome"], "cancelled");
    }

    #[test]
    fn cancelled_turn_that_never_ends_gives_up_after_the_grace_period() {
        let cancel = CancellationToken::new();
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            permission(90, "read"),
        ];
        let started = Instant::now();
        let mut handler = CancelOnAsk(cancel.clone());
        let (result, sent) = drive_cancellable(&lines, &mut handler, &cancel, true);
        assert_eq!(result.unwrap_err().to_string(), CANCELLED);
        assert!(started.elapsed() >= CANCEL_GRACE);
        assert!(sent
            .iter()
            .any(|message| message["method"] == "session/cancel"));
    }

    #[test]
    fn cancelling_before_the_turn_stops_without_prompting() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (result, sent) =
            drive_cancellable(&[LIVE_INITIALIZE.into()], &mut RejectAll, &cancel, true);
        assert_eq!(result.unwrap_err().to_string(), CANCELLED);
        assert!(sent.is_empty());
    }

    #[test]
    fn review_runs_use_the_agy_sandbox() {
        assert_eq!(
            (SPEC.args)(&request(HarnessRole::Build)),
            ["acp", "--disable-telemetry"]
        );
        assert_eq!(
            (SPEC.args)(&request(HarnessRole::Review)),
            ["acp", "--disable-telemetry", "--sandbox"]
        );
    }

    #[test]
    fn capability_stays_out_of_agy_environment() {
        let request = request(HarnessRole::Build);
        let environment = agent_environment(&request);
        assert!(!environment.contains_key(&OsString::from("FORMLESS_TOPIC_CAPABILITY")));
        assert_eq!(environment[&OsString::from("HOME")], "/home/user");
    }

    #[test]
    fn validates_bridge_environment() {
        let mut missing = request(HarnessRole::Build);
        missing.environment.clear();
        assert!(validate(&missing)
            .unwrap_err()
            .to_string()
            .contains("has no value"));
        let mut relative = request(HarnessRole::Build);
        relative.mcp_command = PathBuf::from("formless");
        assert!(validate(&relative).is_err());
    }

    /// Opt-in live check against an installed, signed-in `agy`:
    /// `FORMLESS_LIVE_AGY=1 cargo test -p formless-core live_agy_acp -- --ignored --nocapture`.
    /// Set `FORMLESS_AGY_BIN` to use a binary that is not on PATH.
    #[test]
    #[ignore = "needs an installed Antigravity CLI; set FORMLESS_LIVE_AGY=1"]
    fn live_agy_acp_registers_bridge() {
        if std::env::var_os("FORMLESS_LIVE_AGY").is_none() {
            eprintln!("FORMLESS_LIVE_AGY not set; skipping");
            return;
        }
        let workspace =
            std::env::temp_dir().join(format!("formless-live-agy-{}", std::process::id()));
        std::fs::create_dir_all(&workspace).unwrap();
        let marker = workspace.join("bridge-started");
        let mut request = request(HarnessRole::Build);
        request.cwd = workspace.clone();
        request.executable = std::env::var_os("FORMLESS_AGY_BIN").map(PathBuf::from);
        request.mcp_command = PathBuf::from("/bin/sh");
        request.mcp_args = vec![
            "-c".into(),
            format!(
                "[ \"$FORMLESS_TOPIC_CAPABILITY\" = live-cap ] && echo ok > '{}'; cat >/dev/null",
                marker.display()
            ),
        ];
        request.environment =
            BTreeMap::from([("FORMLESS_TOPIC_CAPABILITY".into(), "live-cap".into())]);

        let mut agent = AgentProcess::spawn(&SPEC, &request).unwrap();
        let (reader, writer) = agent.pipes().unwrap();
        let outcome = drive(
            &SPEC,
            reader,
            writer,
            &request,
            &mut RejectAll,
            &CancellationToken::new(),
            false,
        )
        .map(|result| result.session_id.unwrap());
        std::thread::sleep(std::time::Duration::from_secs(5));
        let started = marker.exists();
        drop(agent);
        let _ = std::fs::remove_dir_all(&workspace);
        match outcome {
            Ok(session_id) => {
                eprintln!("live: ACP v1 negotiated, session {session_id} opened, bridge started with capability: {started}");
                assert!(
                    started,
                    "agy opened the session but did not start the per-session MCP server"
                );
            }
            Err(error) => {
                eprintln!("live: {error}; bridge started: {started}");
                assert!(error.to_string().contains("not signed in"), "{error}");
                assert!(!started);
            }
        }
    }
}
