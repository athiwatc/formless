//! Stateful Antigravity integration over the official `agy acp` server (ACP v1).
//!
//! The Formless bridge is registered per session through `session/new.mcpServers`, so no
//! Antigravity settings are written. Its environment (for example a run capability) is sent
//! over the ACP pipe and removed from the `agy` process environment.

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

use super::super::{HarnessRequest, HarnessResult, HarnessRole};

const PROTOCOL_VERSION: u64 = 1;
/// Oldest released `agy` whose ACP negotiation has been observed live.
pub const MIN_AGY_VERSION: (u64, u64, u64) = (1, 5, 14);
const JSON_RPC_AUTH_REQUIRED: i64 = -32000;
const CANCELLED: &str = "The Antigravity run was cancelled.";
const POLL: Duration = Duration::from_millis(50);
/// How long Antigravity may take to end a turn after `session/cancel` before it is killed.
#[cfg(not(test))]
const CANCEL_GRACE: Duration = Duration::from_secs(5);
#[cfg(test)]
const CANCEL_GRACE: Duration = Duration::from_millis(200);
const WRITE_TOOL_KINDS: &[&str] = &["edit", "delete", "move", "execute"];

/// A `session/request_permission` from Antigravity, passed through to the user.
pub struct PermissionRequest<'a> {
    pub tool_call: &'a Value,
    pub options: &'a [Value],
    /// The run's cancellation token.
    pub cancel: &'a CancellationToken,
}

/// Decides Antigravity tool approvals. Returns the chosen `optionId`, or `None` to cancel.
/// A handler that waits for a person must stop waiting and return `None` once
/// `request.cancel` fires; the run cannot be cancelled while `decide` blocks.
pub trait PermissionHandler {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String>;
}

/// Rejects every approval request; used when no user is available to answer.
pub struct RejectAll;

impl PermissionHandler for RejectAll {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String> {
        reject_option(request.options)
    }
}

/// Runs one prompt in a new Antigravity ACP session with the Formless bridge attached.
///
/// When `cancel` fires during the turn, Formless sends ACP `session/cancel`, answers any
/// further approval request as cancelled, and returns an error once the turn ends or after
/// a short grace period. `agy` and every process it started are killed when this returns.
pub fn run(
    request: &HarnessRequest,
    permissions: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    let mut agent = AgentProcess::spawn(request)?;
    let (reader, writer) = agent.pipes()?;
    drive(reader, writer, request, permissions, cancel, true)
}

/// Negotiates ACP and opens a session with the Formless bridge attached, without prompting.
/// Returns the Antigravity session id.
pub fn open_session(request: &HarnessRequest) -> Result<String> {
    let mut agent = AgentProcess::spawn(request)?;
    let (reader, writer) = agent.pipes()?;
    drive(
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

/// The `agy acp` process environment: the request environment without the bridge's variables.
pub(super) fn agent_environment(request: &HarnessRequest) -> BTreeMap<OsString, OsString> {
    let mut environment = request.environment.clone();
    for name in &request.mcp_env_vars {
        environment.remove(&OsString::from(name));
    }
    environment
}

/// `agy acp` and every process it starts (MCP servers, tools); dropping it kills them all.
struct AgentProcess(SupervisedProcess);

impl AgentProcess {
    fn spawn(request: &HarnessRequest) -> Result<Self> {
        validate(request)?;
        let program = request
            .executable
            .clone()
            .unwrap_or_else(|| PathBuf::from("agy"));
        let mut command = Command::new(&program);
        command
            .args(["acp", "--disable-telemetry"])
            .current_dir(&request.cwd)
            .env_clear()
            .envs(std::env::vars_os().filter(|(name, _)| {
                !request
                    .mcp_env_vars
                    .iter()
                    .any(|var| OsString::from(var) == *name)
            }))
            .envs(agent_environment(request))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let process = SupervisedProcess::spawn(command).map_err(|error| {
            anyhow!("failed to start `agy acp` ({error}); is the Antigravity CLI installed and on PATH?")
        })?;
        Ok(Self(process))
    }

    fn pipes(&mut self) -> Result<(BufReader<ChildStdout>, ChildStdin)> {
        let (stdin, stdout, _) = self.0.take_pipes();
        let stdout = stdout.context("agy acp stdout unavailable")?;
        let stdin = stdin.context("agy acp stdin unavailable")?;
        Ok((BufReader::new(stdout), stdin))
    }
}

pub(super) fn drive<R: BufRead + Send + 'static, W: Write>(
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
        .map_err(|code| anyhow!("Antigravity rejected ACP initialize (JSON-RPC error {code})."))?;
    check_agent(&initialized)?;

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
            JSON_RPC_AUTH_REQUIRED => anyhow!(
                "Antigravity is not signed in. Run `agy` in a terminal and complete its sign-in, then retry. Formless does not handle Antigravity credentials."
            ),
            code => anyhow!("Antigravity rejected session/new with the Formless MCP server (JSON-RPC error {code})."),
        })?;
    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .context("Antigravity session/new response is missing its sessionId.")?
        .to_owned();
    rpc.session_id = Some(session_id.clone());

    if request.role == HarnessRole::Review {
        let mode = find_config_option(&session, "mode", "plan").context(
            "This Antigravity version does not offer a plan mode over ACP, so Formless cannot keep a review session read-only.",
        )?;
        rpc.set_config(&session_id, &mode, "plan", permissions)?;
    }
    if let Some(model) = &request.model {
        let option = find_config_option(&session, "model", model).with_context(|| {
            "This Antigravity session does not offer the requested model over ACP.".to_string()
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
        .map_err(|code| anyhow!("Antigravity prompt turn failed (JSON-RPC error {code})."))?;
    match turn.get("stopReason").and_then(Value::as_str) {
        _ if rpc.cancel.is_cancelled() => bail!(CANCELLED),
        Some("end_turn") => {}
        Some(reason @ ("max_tokens" | "max_turn_requests" | "refusal" | "cancelled")) => {
            bail!("Antigravity ended the turn without completing it (stopReason {reason}).")
        }
        _ => bail!("Antigravity prompt response has no recognised stopReason."),
    }
    if rpc.message.trim().is_empty() {
        bail!("Antigravity completed the turn without a response message.");
    }
    Ok(HarnessResult {
        session_id: Some(session_id),
        message: rpc.message,
        events: rpc.events,
    })
}

fn check_agent(initialized: &Value) -> Result<()> {
    if initialized.get("protocolVersion").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
        bail!("This Antigravity CLI does not speak ACP v1; update it with `agy update`.");
    }
    let info = initialized.get("agentInfo");
    let version = info
        .and_then(|info| info.get("version"))
        .and_then(Value::as_str)
        .and_then(parse_version)
        .context("Antigravity ACP did not report a recognisable agentInfo version.")?;
    if version < MIN_AGY_VERSION {
        let (major, minor, patch) = MIN_AGY_VERSION;
        bail!(
            "Antigravity CLI {}.{}.{} is older than {major}.{minor}.{patch}, the oldest version verified with Formless over ACP; update it with `agy update`.",
            version.0,
            version.1,
            version.2
        );
    }
    Ok(())
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

/// The id of a select config option in `category` that offers `value`.
fn find_config_option(session: &Value, category: &str, value: &str) -> Option<String> {
    session
        .get("configOptions")?
        .as_array()?
        .iter()
        .filter(|option| option.get("category").and_then(Value::as_str) == Some(category))
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
            .context("agy acp closed its input")
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
            anyhow!("Antigravity rejected setting {config_id} (JSON-RPC error {code}).")
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
                Ok(line) => line.context("failed to read from agy acp")?,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) if self.cancel.is_cancelled() => {
                    bail!(CANCELLED)
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("agy acp exited before answering {method}.")
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(&line)
                .map_err(|_| anyhow!("agy acp sent a malformed JSON-RPC message."))?;
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
                (Some("session/update"), None) => self.record_update(message)?,
                (Some(_), None) => self.events.push(message),
                (None, Some(response_id)) => {
                    if response_id.as_u64() != Some(id) {
                        bail!("agy acp answered a request Formless did not send.");
                    }
                    if let Some(result) = message.get("result") {
                        return Ok(Ok(result.clone()));
                    }
                    let code = message
                        .pointer("/error/code")
                        .and_then(Value::as_i64)
                        .context("agy acp sent a response with neither result nor error.")?;
                    return Ok(Err(code));
                }
                (None, None) => bail!("agy acp sent a JSON-RPC message without method or id."),
            }
        }
    }

    /// Asks Antigravity to end the turn once cancelled, and gives up after the grace period.
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

    fn record_update(&mut self, message: Value) -> Result<()> {
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if let Some(session_id) = &self.session_id {
            if params.get("sessionId").and_then(Value::as_str) != Some(session_id) {
                bail!("agy acp sent an update for a different session.");
            }
        }
        let update = &params["update"];
        if update["sessionUpdate"] == "agent_message_chunk" && update["content"]["type"] == "text" {
            if let Some(text) = update["content"]["text"].as_str() {
                self.message.push_str(text);
            }
        }
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
            return Err(-32601);
        }
        let params = message.get("params").ok_or(-32602)?;
        self.events.push(json!({"permissionRequest": params}));
        let tool_call = params.get("toolCall").ok_or(-32602)?;
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .ok_or(-32602)?;
        let is_write = tool_call
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| WRITE_TOOL_KINDS.contains(&kind));
        let choice = if self.cancel.is_cancelled() {
            None
        } else if self.role == HarnessRole::Review && is_write {
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
        let input = lines.join("\n") + "\n";
        let mut written = Vec::new();
        let result = drive(
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
        let lines = [LIVE_INITIALIZE.into(), session.into()];
        let (result, sent) = drive_lines(&request(HarnessRole::Review), &lines, &mut RejectAll);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cannot keep a review session read-only"));
        assert_eq!(sent.len(), 2);
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

        let mut agent = AgentProcess::spawn(&request).unwrap();
        let (reader, writer) = agent.pipes().unwrap();
        let outcome = drive(
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
