//! Stateful harness integration over the Agent Client Protocol (ACP v1) on stdio.
//!
//! The Formless bridge is registered per session through `session/new.mcpServers`, so no
//! harness settings are written. Its environment (for example a run capability) is sent
//! over the ACP pipe and removed from the agent process environment. Each harness supplies
//! an [`AgentSpec`] with its command line, version check, sign-in hint and read-only mode.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

use formless_runtime::process_group::SupervisedProcess;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{
    HarnessPermissionRequest, HarnessRequest, HarnessResult, HarnessUpdate, InteractionHandler,
    StepAccess,
};

const PROTOCOL_VERSION: u64 = 1;
const CANCELLED: &str = "The harness run was cancelled.";
const POLL: Duration = Duration::from_millis(50);
/// How long the agent may take to end a turn after `session/cancel` before it is killed.
#[cfg(not(test))]
const CANCEL_GRACE: Duration = Duration::from_secs(5);
#[cfg(test)]
const CANCEL_GRACE: Duration = Duration::from_millis(200);
/// ACP tool kinds a read-only run rejects without asking; other kinds go to the handler.
const WRITE_TOOL_KINDS: &[&str] = &["edit", "delete", "move"];

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
    /// Session modes for write runs, most preferred first, selected like
    /// `read_only_modes`; the run fails if none can be set. Empty keeps the agent's default.
    pub write_modes: &'static [&'static str],
    /// Adds harness-specific entries (for example `elicitation`) to the
    /// `clientCapabilities` sent in `initialize`.
    pub client_capabilities: fn(&mut Value),
    /// Extra variables removed from the agent environment, besides the bridge's.
    pub strip_env: &'static [&'static str],
    /// Variables set on the agent process after the request environment, e.g. to turn
    /// off a self-updater.
    pub set_env: &'static [(&'static str, &'static str)],
    /// Identifies approval requests for this run's Formless bridge tools (for example by
    /// MCP server name), which are approved once without asking, whatever the run's access.
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

/// For [`AgentSpec::client_capabilities`] when the harness needs nothing beyond ACP's.
pub fn no_extra_capabilities(_: &mut Value) {}

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

/// Forwards ACP approvals, questions and updates to the daemon's [`InteractionHandler`].
///
/// [`run`] is blocking, so each request is driven to completion on `runtime`; call `run`
/// from a blocking thread (for example `spawn_blocking`), never from inside the runtime.
pub struct Interactions<'a> {
    pub handler: &'a dyn InteractionHandler,
    pub runtime: tokio::runtime::Handle,
}

impl PermissionHandler for Interactions<'_> {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String> {
        let options = request
            .options
            .iter()
            .filter_map(|option| serde_json::from_value::<WireOption>(option.clone()).ok())
            .map(|option| formless_protocol::HarnessPermissionOption {
                option_id: option.option_id,
                name: option.name,
                kind: option.kind,
            })
            .collect();
        let text = |key: &str| {
            request
                .tool_call
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let permission = HarnessPermissionRequest {
            title: text("title"),
            kind: text("kind"),
            raw_input: permission_raw_input(request.tool_call),
            options,
        };
        self.runtime
            .block_on(self.handler.request_permission(permission, request.cancel))
            .or_else(|| option_of_kind(request.options, &["reject_once", "reject_always"]))
    }

    fn on_update(&mut self, update: &Value) {
        if let Some(update) = harness_update(&update["update"]) {
            let _ = self.handler.on_update(update);
        }
    }

    fn ask_question(&mut self, questions: &Value, cancel: &CancellationToken) -> Option<Value> {
        let questions: Vec<formless_protocol::Question> =
            serde_json::from_value(questions.clone()).ok()?;
        let answers = self
            .runtime
            .block_on(self.handler.ask_question(questions, cancel))?;
        serde_json::to_value(answers).ok()
    }
}

/// A blocking ACP adapter entry point such as `antigravity::run`.
pub type Adapter =
    fn(&HarnessRequest, &mut dyn PermissionHandler, &CancellationToken) -> Result<HarnessResult>;
pub type ProcessSpawner =
    dyn FnMut(Command, &CancellationToken) -> Result<SupervisedProcess> + Send;
pub type GuardedAdapter = fn(
    &HarnessRequest,
    &mut dyn PermissionHandler,
    &CancellationToken,
    &mut ProcessSpawner,
) -> Result<HarnessResult>;

/// Runs `adapter` on a blocking thread while `handler` answers its permission requests,
/// questions and updates on the caller's task, so `handler` need not be `'static`.
/// Cancelling `cancel` stops the run; this returns only after the adapter has returned.
pub async fn run_interactive(
    adapter: Adapter,
    request: HarnessRequest,
    handler: &dyn InteractionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    let (sender, mut asks) = tokio::sync::mpsc::unbounded_channel();
    let run_cancel = cancel.child_token();
    let blocking_cancel = run_cancel.clone();
    let runtime = tokio::runtime::Handle::current();
    let mut job = tokio::task::spawn_blocking(move || {
        let forward = Forward(sender);
        let mut interactions = Interactions {
            handler: &forward,
            runtime,
        };
        adapter(&request, &mut interactions, &blocking_cancel)
    });
    let result = loop {
        tokio::select! {
            biased;
            Some(ask) = asks.recv() => answer(handler, ask, &run_cancel).await,
            result = &mut job => break result,
        }
    };
    while let Ok(ask) = asks.try_recv() {
        answer(handler, ask, &run_cancel).await;
    }
    result.map_err(|error| anyhow!("ACP adapter task failed: {error}"))?
}

pub async fn run_interactive_with_spawn<F>(
    adapter: GuardedAdapter,
    request: HarnessRequest,
    handler: &dyn InteractionHandler,
    cancel: &CancellationToken,
    mut spawn: F,
) -> Result<HarnessResult>
where
    F: FnMut(Command, &CancellationToken) -> Result<SupervisedProcess> + Send + 'static,
{
    let (sender, mut asks) = tokio::sync::mpsc::unbounded_channel();
    let run_cancel = cancel.child_token();
    let _cancel_on_drop = run_cancel.clone().drop_guard();
    let blocking_cancel = run_cancel.clone();
    let runtime = tokio::runtime::Handle::current();
    let mut job = tokio::task::spawn_blocking(move || {
        let forward = Forward(sender);
        let mut interactions = Interactions {
            handler: &forward,
            runtime,
        };
        adapter(&request, &mut interactions, &blocking_cancel, &mut spawn)
    });
    let result = loop {
        tokio::select! {
            biased;
            Some(ask) = asks.recv() => answer(handler, ask, &run_cancel).await,
            result = &mut job => break result,
        }
    };
    while let Ok(ask) = asks.try_recv() {
        answer(handler, ask, &run_cancel).await;
    }
    result.map_err(|error| anyhow!("ACP adapter task failed: {error}"))?
}

enum Ask {
    Permission(
        HarnessPermissionRequest,
        tokio::sync::oneshot::Sender<Option<String>>,
    ),
    Question(
        Vec<formless_protocol::Question>,
        tokio::sync::oneshot::Sender<Option<Vec<formless_protocol::QuestionAnswer>>>,
    ),
    Update(HarnessUpdate),
}

/// A failed update (e.g. the attempt already reported its result) never fails the run.
async fn answer(handler: &dyn InteractionHandler, ask: Ask, cancel: &CancellationToken) {
    match ask {
        Ask::Permission(request, reply) => {
            let _ = reply.send(handler.request_permission(request, cancel).await);
        }
        Ask::Question(questions, reply) => {
            let _ = reply.send(handler.ask_question(questions, cancel).await);
        }
        Ask::Update(update) => {
            let _ = handler.on_update(update);
        }
    }
}

/// Hands each interaction from the blocking adapter thread to `run_interactive`'s task.
struct Forward(tokio::sync::mpsc::UnboundedSender<Ask>);

impl InteractionHandler for Forward {
    fn request_permission<'a>(
        &'a self,
        request: HarnessPermissionRequest,
        cancel: &'a CancellationToken,
    ) -> super::InteractionFuture<'a, Option<String>> {
        Box::pin(async move {
            let (reply, answer) = tokio::sync::oneshot::channel();
            self.0.send(Ask::Permission(request, reply)).ok()?;
            tokio::select! {
                answer = answer => answer.ok().flatten(),
                _ = cancel.cancelled() => None,
            }
        })
    }

    fn ask_question<'a>(
        &'a self,
        questions: Vec<formless_protocol::Question>,
        cancel: &'a CancellationToken,
    ) -> super::InteractionFuture<'a, Option<Vec<formless_protocol::QuestionAnswer>>> {
        Box::pin(async move {
            let (reply, answer) = tokio::sync::oneshot::channel();
            self.0.send(Ask::Question(questions, reply)).ok()?;
            tokio::select! {
                answer = answer => answer.ok().flatten(),
                _ = cancel.cancelled() => None,
            }
        })
    }

    fn on_update(&self, update: HarnessUpdate) -> Result<()> {
        self.0
            .send(Ask::Update(update))
            .map_err(|_| anyhow!("interaction handler is closed"))
    }
}

/// The tool call's `rawInput`, with any vendor `_meta` kept under `_meta`; a non-object
/// input is wrapped as `{"input": ...}` so the metadata has somewhere to go.
fn permission_raw_input(tool_call: &Value) -> Option<Value> {
    let input = tool_call.get("rawInput").cloned();
    let Some(meta) = tool_call.get("_meta").cloned() else {
        return input;
    };
    let mut raw = match input {
        Some(Value::Object(object)) => object,
        Some(Value::Null) | None => serde_json::Map::new(),
        Some(other) => serde_json::Map::from_iter([("input".to_owned(), other)]),
    };
    raw.insert("_meta".into(), meta);
    Some(Value::Object(raw))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireOption {
    option_id: String,
    name: String,
    kind: formless_protocol::HarnessPermissionOptionKind,
}

/// Maps an ACP `session/update` payload to the daemon's live update, when it has one.
fn harness_update(update: &Value) -> Option<HarnessUpdate> {
    let text = || update["content"]["text"].as_str().map(str::to_owned);
    match update["sessionUpdate"].as_str()? {
        "agent_message_chunk" => Some(HarnessUpdate::Message { content: text()? }),
        "agent_thought_chunk" => Some(HarnessUpdate::Thought { content: text()? }),
        "tool_call" => Some(HarnessUpdate::ToolCall {
            name: update["title"].as_str().unwrap_or_default().to_owned(),
            input: update.get("rawInput").cloned().unwrap_or(Value::Null),
        }),
        "plan" => {
            let entries = update["entries"].as_array()?;
            let content = entries
                .iter()
                .filter_map(|entry| entry["content"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            Some(HarnessUpdate::Plan { content })
        }
        _ => None,
    }
}

/// Rejects every approval request; used when no user is available to answer.
pub struct RejectAll;

impl PermissionHandler for RejectAll {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String> {
        option_of_kind(request.options, &["reject_once", "reject_always"])
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
    let mut spawn = |command, _cancel: &CancellationToken| {
        SupervisedProcess::spawn(command).map_err(|error| anyhow!("{error}"))
    };
    run_with_spawn(spec, request, permissions, cancel, &mut spawn)
}

pub fn run_with_spawn(
    spec: &AgentSpec,
    request: &HarnessRequest,
    permissions: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
    spawn: &mut ProcessSpawner,
) -> Result<HarnessResult> {
    let mut agent = AgentProcess::spawn_with(spec, request, cancel, spawn)?;
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

/// What an ACP `initialize` handshake says about an installed harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The executable started and answered ACP.
    pub installed: bool,
    /// It also passed the spec's protocol and version check.
    pub compatible: bool,
    pub version: Option<String>,
    pub detail: Option<String>,
}

#[cfg(not(test))]
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
#[cfg(test)]
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Starts the agent and runs ACP `initialize` only: no session, no sign-in, no bridge.
/// Sign-in is not visible at this step, so callers probe it separately.
pub fn probe(
    spec: &AgentSpec,
    executable: Option<PathBuf>,
    environment: &BTreeMap<OsString, OsString>,
) -> Probe {
    let cwd = std::env::temp_dir();
    let request = HarnessRequest {
        kind: super::HarnessKind::Acp,
        acp_agent: None,
        model: None,
        prompt: "probe".into(),
        access: StepAccess::Write,
        mcp_command: cwd.clone(),
        mcp_args: vec![],
        mcp_env_vars: vec![],
        cwd,
        executable,
        executable_argv0: None,
        environment: environment.clone(),
    };
    let mut agent = match AgentProcess::spawn(spec, &request) {
        Ok(agent) => agent,
        Err(error) => {
            return Probe {
                installed: false,
                compatible: false,
                version: None,
                detail: Some(format!("{error:#}")),
            }
        }
    };
    let cancel = CancellationToken::new();
    let timer = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(PROBE_TIMEOUT);
        timer.cancel();
    });
    let initialized = agent
        .pipes()
        .and_then(|(reader, writer)| probe_lines(spec, reader, writer, &cancel));
    drop(agent);
    probe_result(spec, initialized)
}

pub(super) struct SessionProbe {
    pub version: Option<String>,
    pub auth_methods: Vec<String>,
    pub session: Result<std::result::Result<Value, i64>>,
}

/// A bounded readiness check that records auth methods but does not authenticate.
pub(super) fn probe_session(
    spec: &AgentSpec,
    request: &HarnessRequest,
    timeout: Duration,
) -> SessionProbe {
    let mut version = None;
    let mut auth_methods = Vec::new();
    let cancel = CancellationToken::new();
    let timer = cancel.clone();
    let (stop, stopped) = mpsc::channel();
    std::thread::spawn(move || {
        if stopped.recv_timeout(timeout).is_err() {
            timer.cancel();
        }
    });
    let session = (|| {
        let mut agent = AgentProcess::spawn(spec, request)?;
        let (reader, writer) = agent.pipes()?;
        let mut rpc = Rpc::new(spec, reader, writer, &cancel, StepAccess::Write);
        let initialized = match rpc.initialize_response(spec, &mut RejectAll)? {
            Ok(initialized) => initialized,
            Err(code) => return Ok(Err(code)),
        };
        auth_methods = initialized
            .get("authMethods")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|method| method.get("id").and_then(Value::as_str).map(str::to_owned))
            .collect();
        version = initialized
            .pointer("/agentInfo/version")
            .or_else(|| initialized.pointer("/_meta/agentVersion"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        (spec.check_agent)(&initialized)?;
        rpc.call(
            "session/new",
            json!({"cwd": request.cwd.to_str().context("probe cwd must be UTF-8")?, "mcpServers": []}),
            &mut RejectAll,
        )
    })();
    let _ = stop.send(());
    SessionProbe {
        version,
        auth_methods,
        session: if cancel.is_cancelled() {
            Err(anyhow!("ACP readiness probe timed out."))
        } else {
            session
        },
    }
}

fn probe_lines<R: BufRead + Send + 'static, W: Write>(
    spec: &AgentSpec,
    reader: R,
    writer: W,
    cancel: &CancellationToken,
) -> Result<Value> {
    Rpc::new(spec, reader, writer, cancel, StepAccess::Write).initialize(spec, &mut RejectAll)
}

fn probe_result(spec: &AgentSpec, initialized: Result<Value>) -> Probe {
    let initialized = match initialized {
        Ok(initialized) => initialized,
        Err(error) => {
            return Probe {
                installed: false,
                compatible: false,
                version: None,
                detail: Some(format!(
                    "{} did not answer ACP initialize: {error:#}",
                    spec.name
                )),
            }
        }
    };
    let version = initialized
        .pointer("/agentInfo/version")
        .or_else(|| initialized.pointer("/_meta/agentVersion"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let checked = (spec.check_agent)(&initialized);
    Probe {
        installed: true,
        compatible: checked.is_ok(),
        version,
        detail: checked.err().map(|error| format!("{error:#}")),
    }
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

fn anchor_relative_search_path(path: &OsStr, base: &Path) -> Result<OsString> {
    let entries = if path.is_empty() {
        vec![base.to_path_buf()]
    } else {
        std::env::split_paths(path)
            .map(|entry| {
                if entry.is_absolute() {
                    entry
                } else {
                    base.join(entry)
                }
            })
            .collect::<Vec<_>>()
    };
    std::env::join_paths(entries).context("could not anchor relative ACP PATH entries")
}

// Keep tool and shebang-interpreter lookup daemon-relative; topic workspaces
// are not implicit PATH trust sources.
fn path_for_agent_directory(path: &OsStr) -> Result<OsString> {
    let entries = std::env::split_paths(path).collect::<Vec<_>>();
    if !path.is_empty() && entries.iter().all(|entry| entry.is_absolute()) {
        return Ok(path.to_os_string());
    }
    let base = std::env::current_dir().context("could not resolve daemon working directory")?;
    anchor_relative_search_path(path, &base)
}

fn is_path_environment_variable(name: &OsStr) -> bool {
    #[cfg(windows)]
    {
        name.to_string_lossy().eq_ignore_ascii_case("PATH")
    }
    #[cfg(not(windows))]
    {
        name == OsStr::new("PATH")
    }
}

/// The agent and every process it starts (MCP servers, tools); dropping it kills them all.
pub(super) struct AgentProcess(SupervisedProcess);

pub(super) fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(super) fn find_executable(
    program: &str,
    environment: &BTreeMap<OsString, OsString>,
) -> Option<PathBuf> {
    find_executable_with_platform(program, environment, cfg!(windows))
}

fn find_executable_with_platform(
    program: &str,
    environment: &BTreeMap<OsString, OsString>,
    windows: bool,
) -> Option<PathBuf> {
    let path = environment
        .get(OsStr::new("PATH"))
        .cloned()
        .or_else(|| std::env::var_os("PATH"))?;
    let suffixes: Vec<_> = if windows {
        if Path::new(program).extension().is_some() {
            vec![String::new()]
        } else {
            let extensions = environment
                .get(OsStr::new("PATHEXT"))
                .cloned()
                .or_else(|| std::env::var_os("PATHEXT"))
                .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
            extensions
                .to_string_lossy()
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(str::to_owned)
                .collect()
        }
    } else {
        vec![String::new()]
    };
    for directory in std::env::split_paths(&path) {
        for suffix in &suffixes {
            let candidate = directory.join(format!("{program}{suffix}"));
            if is_executable(&candidate) {
                return std::path::absolute(candidate).ok();
            }
        }
    }
    None
}

fn agent_program(
    program: &str,
    request: &HarnessRequest,
    process_environment: &BTreeMap<OsString, OsString>,
    windows: bool,
) -> PathBuf {
    if let Some(executable) = &request.executable {
        return executable.clone();
    }
    if !windows {
        return PathBuf::from(program);
    }
    let mut environment = process_environment.clone();
    environment.extend(request.environment.clone());
    find_executable_with_platform(program, &environment, windows)
        .unwrap_or_else(|| PathBuf::from(program))
}

impl AgentProcess {
    pub(super) fn spawn(spec: &AgentSpec, request: &HarnessRequest) -> Result<Self> {
        let mut spawn = |command, _cancel: &CancellationToken| {
            SupervisedProcess::spawn(command).map_err(|error| anyhow!("{error}"))
        };
        Self::spawn_with(spec, request, &CancellationToken::new(), &mut spawn)
    }

    fn spawn_with(
        spec: &AgentSpec,
        request: &HarnessRequest,
        cancel: &CancellationToken,
        spawn: &mut ProcessSpawner,
    ) -> Result<Self> {
        validate(request)?;
        let process_environment = std::env::vars_os().collect();
        let program = agent_program(spec.program, request, &process_environment, cfg!(windows));
        let mut command = Command::new(&program);
        #[cfg(unix)]
        if let Some(argv0) = &request.executable_argv0 {
            use std::os::unix::process::CommandExt;
            command.arg0(argv0);
        }
        let mut child_environment = std::env::vars_os()
            .filter(|(name, _)| {
                !request
                    .mcp_env_vars
                    .iter()
                    .map(String::as_str)
                    .chain(spec.strip_env.iter().copied())
                    .any(|var| name.as_os_str() == std::ffi::OsStr::new(var))
            })
            .collect::<Vec<_>>();
        child_environment.extend(agent_environment(request).into_iter().filter(|(name, _)| {
            !spec
                .strip_env
                .iter()
                .any(|var| name.as_os_str() == std::ffi::OsStr::new(var))
        }));
        child_environment.extend(
            spec.set_env
                .iter()
                .map(|(name, value)| (OsString::from(*name), OsString::from(*value))),
        );
        for (name, value) in &mut child_environment {
            if is_path_environment_variable(name) {
                *value = path_for_agent_directory(value)?;
            }
        }
        command
            .args((spec.args)(request))
            .current_dir(&request.cwd)
            .env_clear()
            .envs(child_environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let process = spawn(command, cancel).map_err(|error| {
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
    let mut rpc = Rpc::new(spec, reader, writer, cancel, request.access);
    let initialized = rpc.initialize(spec, permissions)?;
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

    if request.access == StepAccess::ReadOnly {
        let purpose = "keep a review session read-only";
        let fail_closed = spec.read_only_fail_closed;
        let modes = spec.read_only_modes;
        select_mode(
            spec,
            &mut rpc,
            &session,
            &session_id,
            modes,
            fail_closed,
            purpose,
            permissions,
        )?;
    } else if !spec.write_modes.is_empty() {
        let purpose = "start a write session";
        let modes = spec.write_modes;
        select_mode(
            spec,
            &mut rpc,
            &session,
            &session_id,
            modes,
            true,
            purpose,
            permissions,
        )?;
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

#[allow(clippy::too_many_arguments)]
fn select_mode<W: Write>(
    spec: &AgentSpec,
    rpc: &mut Rpc<W>,
    session: &Value,
    session_id: &str,
    candidates: &[&str],
    fail_closed: bool,
    purpose: &str,
    permissions: &mut dyn PermissionHandler,
) -> Result<()> {
    let refused = |detail: String| anyhow!("{} {detail}, so Formless cannot {purpose}.", spec.name);
    let available_modes: Vec<&str> = session
        .pointer("/modes/availableModes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|mode| mode.get("id").and_then(Value::as_str))
        .collect();
    for &mode in candidates {
        if let Some(option) = find_config_option(session, "mode", mode) {
            return rpc.set_config(session_id, &option, mode, permissions);
        }
        if available_modes.contains(&mode) {
            return set_mode(rpc, session_id, mode, permissions).map_err(|code| {
                refused(format!("refused its `{mode}` mode (JSON-RPC error {code})"))
            });
        }
    }
    let attempt = match candidates.first() {
        Some(&mode) => set_mode(rpc, session_id, mode, permissions)
            .map_err(|code| format!("refused its `{mode}` mode (JSON-RPC error {code})")),
        None => Err("has no suitable mode over ACP".to_owned()),
    };
    match attempt {
        Err(detail) if fail_closed => Err(refused(detail)),
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

/// The first option whose kind is listed, in the order of `kinds`.
fn option_of_kind(options: &[Value], kinds: &[&str]) -> Option<String> {
    kinds
        .iter()
        .find_map(|&kind| {
            options
                .iter()
                .find(|option| option.get("kind").and_then(Value::as_str) == Some(kind))
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
    access: StepAccess,
}

impl<W: Write> Rpc<W> {
    fn new<R: BufRead + Send + 'static>(
        spec: &AgentSpec,
        reader: R,
        writer: W,
        cancel: &CancellationToken,
        access: StepAccess,
    ) -> Self {
        Rpc {
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
            access,
        }
    }

    fn initialize(
        &mut self,
        spec: &AgentSpec,
        permissions: &mut dyn PermissionHandler,
    ) -> Result<Value> {
        self.initialize_response(spec, permissions)?
            .map_err(|code| {
                anyhow!(
                    "{} rejected ACP initialize (JSON-RPC error {code}).",
                    spec.name
                )
            })
    }

    fn initialize_response(
        &mut self,
        spec: &AgentSpec,
        permissions: &mut dyn PermissionHandler,
    ) -> Result<std::result::Result<Value, i64>> {
        let mut client_capabilities = json!({
            "fs": {"readTextFile": false, "writeTextFile": false},
            "terminal": false
        });
        (spec.client_capabilities)(&mut client_capabilities);
        self.call(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": client_capabilities,
                "clientInfo": {"name": "formless", "version": env!("CARGO_PKG_VERSION")}
            }),
            permissions,
        )
    }

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
        let writes = tool_call
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| WRITE_TOOL_KINDS.contains(&kind));
        let choice = if self.cancel.is_cancelled() {
            None
        } else if (self.bridge_tool)(tool_call) {
            option_of_kind(options, &["allow_once", "allow_always"])
        } else if self.access == StepAccess::ReadOnly && writes {
            option_of_kind(options, &["reject_once", "reject_always"])
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

    fn request(access: StepAccess) -> HarnessRequest {
        HarnessRequest {
            kind: HarnessKind::Antigravity,
            acp_agent: None,
            model: None,
            cwd: PathBuf::from("/work/repo"),
            prompt: "prompt text".into(),
            access,
            mcp_command: PathBuf::from("/opt/formless/bin/formless"),
            mcp_args: vec!["mcp".into(), "--run".into(), "run-7".into()],
            mcp_env_vars: vec!["FORMLESS_TOPIC_CAPABILITY".into()],
            executable: None,
            executable_argv0: None,
            environment: BTreeMap::from([
                ("FORMLESS_TOPIC_CAPABILITY".into(), "cap-secret".into()),
                ("HOME".into(), "/home/user".into()),
            ]),
        }
    }

    #[cfg(unix)]
    #[test]
    fn windows_pathext_skips_extensionless_shims_and_preserves_precedence_and_override() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "formless-acp-launch-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let extensionless = first.join("gemini");
        std::fs::write(&extensionless, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&extensionless, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cmd_shim = first.join("gemini.CMD");
        std::fs::write(&cmd_shim, "fixture").unwrap();
        std::fs::set_permissions(&cmd_shim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let exe = second.join("gemini.EXE");
        std::fs::write(&exe, "fixture").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut request = request(StepAccess::Write);
        request.environment.insert(
            "PATH".into(),
            std::env::join_paths([first.as_os_str(), second.as_os_str()]).unwrap(),
        );
        request
            .environment
            .insert("PATHEXT".into(), ".EXE;.CMD".into());
        let expected = std::path::absolute(&cmd_shim).unwrap();
        let process_environment = BTreeMap::new();
        assert_eq!(
            find_executable_with_platform("gemini", &request.environment, true),
            Some(expected.clone())
        );
        assert_eq!(
            agent_program("gemini", &request, &process_environment, true),
            expected
        );

        let override_path = root.join("explicit-agent.exe");
        request.executable = Some(override_path.clone());
        assert_eq!(
            agent_program("gemini", &request, &process_environment, true),
            override_path
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relative_path_entries_are_anchored_to_the_daemon_directory() {
        let daemon_cwd = std::env::temp_dir().join("formless-daemon-cwd");
        let relative = PathBuf::from("repository/bin");
        let absolute = std::env::temp_dir().join("system/bin");
        let path = std::env::join_paths([relative.as_os_str(), absolute.as_os_str()]).unwrap();

        let anchored = anchor_relative_search_path(&path, &daemon_cwd).unwrap();

        assert_eq!(
            std::env::split_paths(&anchored).collect::<Vec<_>>(),
            vec![daemon_cwd.join(&relative), absolute.clone()]
        );

        let path_with_empty_entries = std::env::join_paths([
            relative.as_os_str(),
            OsStr::new(""),
            absolute.as_os_str(),
            OsStr::new(""),
        ])
        .unwrap();
        let anchored_with_empty_entries =
            anchor_relative_search_path(&path_with_empty_entries, &daemon_cwd).unwrap();
        assert_eq!(
            std::env::split_paths(&anchored_with_empty_entries).collect::<Vec<_>>(),
            vec![
                daemon_cwd.join(&relative),
                daemon_cwd.clone(),
                absolute.clone(),
                daemon_cwd.clone(),
            ]
        );

        let anchored_empty = anchor_relative_search_path(OsStr::new(""), &daemon_cwd).unwrap();
        assert_eq!(anchored_empty.as_os_str(), daemon_cwd.as_os_str());

        let absolute_only = absolute.as_os_str().to_os_string();
        assert_eq!(
            path_for_agent_directory(&absolute_only).unwrap(),
            absolute_only
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_launch_uses_readiness_executable_over_workspace_relative_path() {
        use std::os::unix::fs::PermissionsExt;

        let process_cwd = std::env::current_dir().unwrap();
        let target = process_cwd.join("target");
        let target_preexisted = target.exists();
        let name = format!(
            "formless-acp-path-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        );
        let relative_bin = PathBuf::from("target").join(&name);
        let daemon_bin = process_cwd.join(&relative_bin);
        let workspace = std::env::temp_dir().join(format!("formless-{name}"));
        let workspace_bin = workspace.join(&relative_bin);
        let global_bin = workspace.join("global");
        let marker = workspace.join("selected");
        let marker_complete = workspace.join("selected.complete");
        for directory in [&daemon_bin, &workspace_bin, &global_bin] {
            std::fs::create_dir_all(directory).unwrap();
        }

        for (directory, selected) in [
            (&daemon_bin, "daemon"),
            (&workspace_bin, "workspace"),
            (&global_bin, "global"),
        ] {
            let executable = directory.join("gemini");
            std::fs::write(
                &executable,
                format!(
                    "#!/bin/sh\nprintf '%s' '{selected}' > \"$ACP_LAUNCH_MARKER\"\n: > \"$ACP_LAUNCH_MARKER_COMPLETE\"\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let mut request = request(StepAccess::Write);
        request.kind = HarnessKind::Acp;
        request.acp_agent = Some("gemini".into());
        request.cwd = workspace.clone();
        request.environment.insert(
            "PATH".into(),
            std::env::join_paths([relative_bin.as_os_str(), OsStr::new("global")]).unwrap(),
        );
        request.environment.insert(
            "ACP_LAUNCH_MARKER".into(),
            marker.as_os_str().to_os_string(),
        );
        request.environment.insert(
            "ACP_LAUNCH_MARKER_COMPLETE".into(),
            marker_complete.as_os_str().to_os_string(),
        );
        request.executable = Some(std::fs::canonicalize(daemon_bin.join("gemini")).unwrap());
        assert_eq!(
            agent_program("gemini", &request, &BTreeMap::new(), false),
            request.executable.clone().unwrap()
        );

        let spec = crate::local_harness::acp_agents::spec("gemini").unwrap();
        let agent = AgentProcess::spawn(spec, &request).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker_complete.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            marker_complete.exists(),
            "workspace PATH executable did not finish writing its marker"
        );
        let selected = std::fs::read_to_string(&marker).unwrap();
        drop(agent);
        assert_eq!(selected, "daemon");

        std::fs::remove_dir_all(&daemon_bin).unwrap();
        std::fs::remove_dir_all(&workspace).unwrap();
        if !target_preexisted {
            let _ = std::fs::remove_dir(&target);
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
        let (result, sent) = drive_lines(&request(StepAccess::Write), &lines, &mut handler);
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
        let (result, sent) = drive_lines(&request(StepAccess::Write), &lines, &mut RejectAll);
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
            &request(StepAccess::Write),
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
        let (result, sent) = drive_lines(&request(StepAccess::ReadOnly), &lines, &mut handler);
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
        let (result, sent) = drive_lines(&request(StepAccess::ReadOnly), &lines, &mut RejectAll);
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
        let (result, sent) = drive_spec(&spec, &request(StepAccess::Write), &lines, &mut Answer);
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
    fn bridge_tools_are_allowed_and_other_review_tools_go_to_the_handler() {
        fn formless_server(tool_call: &Value) -> bool {
            tool_call["title"] == "formless.report"
        }
        let spec = AgentSpec {
            bridge_tool: formless_server,
            ..SPEC
        };
        let bridge =
            permission(90, "other").replace(r#""title":"tool""#, r#""title":"formless.report""#);
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            ok(3),
            bridge,
            permission(91, "execute"),
            update("done"),
            stop(4, "end_turn"),
        ];
        let (result, sent) = drive_spec(
            &spec,
            &request(StepAccess::ReadOnly),
            &lines,
            &mut RejectAll,
        );
        result.unwrap();
        let answer = |id: u64| {
            sent.iter()
                .find(|message| message["id"] == id)
                .unwrap()
                .clone()
        };
        assert_eq!(answer(90)["result"]["outcome"]["optionId"], "allow");
        assert_eq!(answer(91)["result"]["outcome"]["optionId"], "deny");

        let mut handler = Choose("deny", 0);
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            ok(3),
            permission(91, "execute"),
            update("done"),
            stop(4, "end_turn"),
        ];
        let (result, sent) =
            drive_spec(&spec, &request(StepAccess::ReadOnly), &lines, &mut handler);
        result.unwrap();
        assert_eq!(handler.1, 1, "execute in review is the user's decision");
        let answer = sent.iter().find(|message| message["id"] == 91).unwrap();
        assert_eq!(answer["result"]["outcome"]["optionId"], "deny");
    }

    #[test]
    fn write_runs_select_spec_write_mode_and_extra_client_capabilities() {
        fn elicitation(capabilities: &mut Value) {
            capabilities["elicitation"] = json!({"form": {}});
        }
        let spec = AgentSpec {
            write_modes: &["accept-edits"],
            client_capabilities: elicitation,
            ..SPEC
        };
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            ok(3),
            update("done"),
            stop(4, "end_turn"),
        ];
        let (result, sent) = drive_spec(&spec, &request(StepAccess::Write), &lines, &mut RejectAll);
        result.unwrap();
        assert_eq!(
            sent[0]["params"]["clientCapabilities"]["elicitation"],
            json!({"form": {}})
        );
        assert_eq!(sent[0]["params"]["clientCapabilities"]["terminal"], false);
        assert_eq!(sent[2]["method"], "session/set_config_option");
        assert_eq!(sent[2]["params"]["value"], "accept-edits");

        let refused =
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"unknown mode"}}"#;
        let session = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-1"}}"#;
        let lines = [LIVE_INITIALIZE.into(), session.into(), refused.into()];
        let (result, _) = drive_spec(&spec, &request(StepAccess::Write), &lines, &mut RejectAll);
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cannot start a write session"));
    }

    #[test]
    fn probe_reports_version_and_compatibility_from_initialize() {
        let probe_input = |line: &str| {
            let mut written = Vec::new();
            let initialized = probe_lines(
                &SPEC,
                Cursor::new(format!("{line}\n")),
                &mut written,
                &CancellationToken::new(),
            );
            let sent: Value =
                serde_json::from_str(String::from_utf8(written).unwrap().lines().next().unwrap())
                    .unwrap();
            assert_eq!(sent["method"], "initialize");
            probe_result(&SPEC, initialized)
        };
        assert_eq!(
            probe_input(LIVE_INITIALIZE),
            Probe {
                installed: true,
                compatible: true,
                version: Some("1.5.14".into()),
                detail: None,
            }
        );
        let old = probe_input(&LIVE_INITIALIZE.replace("1.5.14", "1.4.0"));
        assert!(old.installed && !old.compatible);
        assert_eq!(old.version.as_deref(), Some("1.4.0"));
        assert!(old.detail.unwrap().contains("agy update"));
        let rejected =
            probe_input(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"x"}}"#);
        assert!(!rejected.installed && !rejected.compatible);
    }

    #[test]
    fn probe_reports_a_missing_executable_as_not_installed() {
        let probe = probe(
            &SPEC,
            Some(PathBuf::from("/nonexistent/formless-test-agy")),
            &BTreeMap::new(),
        );
        assert!(!probe.installed && !probe.compatible);
        assert!(probe.version.is_none());
    }

    #[test]
    fn permission_raw_input_keeps_vendor_meta() {
        let meta = json!({"x.ai/tool": {"read_only": false}});
        assert_eq!(
            permission_raw_input(&json!({"rawInput": {"a": 1}, "_meta": meta})),
            Some(json!({"a": 1, "_meta": meta}))
        );
        assert_eq!(
            permission_raw_input(&json!({"rawInput": "ls", "_meta": meta})),
            Some(json!({"input": "ls", "_meta": meta}))
        );
        assert_eq!(
            permission_raw_input(&json!({"_meta": meta})),
            Some(json!({"_meta": meta}))
        );
        assert_eq!(
            permission_raw_input(&json!({"rawInput": [1]})),
            Some(json!([1]))
        );
        assert_eq!(permission_raw_input(&json!({})), None);
    }

    #[test]
    fn interactions_forward_permissions_questions_and_updates_to_the_daemon() {
        use crate::local_harness::InteractionFuture;
        use std::sync::Mutex;
        #[derive(Default)]
        struct Daemon {
            permissions: Mutex<Vec<HarnessPermissionRequest>>,
            updates: Mutex<Vec<HarnessUpdate>>,
        }
        impl InteractionHandler for Daemon {
            fn request_permission<'a>(
                &'a self,
                request: HarnessPermissionRequest,
                _: &'a CancellationToken,
            ) -> InteractionFuture<'a, Option<String>> {
                let choice = request.options[1].option_id.clone();
                self.permissions.lock().unwrap().push(request);
                Box::pin(async move { Some(choice) })
            }
            fn ask_question<'a>(
                &'a self,
                questions: Vec<formless_protocol::Question>,
                _: &'a CancellationToken,
            ) -> InteractionFuture<'a, Option<Vec<formless_protocol::QuestionAnswer>>> {
                Box::pin(async move {
                    Some(vec![formless_protocol::QuestionAnswer {
                        selected: vec![],
                        text: Some(questions[0].question.clone()),
                    }])
                })
            }
            fn on_update(&self, update: HarnessUpdate) -> Result<()> {
                self.updates.lock().unwrap().push(update);
                Ok(())
            }
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let daemon = Daemon::default();
        let mut interactions = Interactions {
            handler: &daemon,
            runtime: runtime.handle().clone(),
        };
        let lines = [
            LIVE_INITIALIZE.into(),
            SESSION_NEW.into(),
            permission(90, "execute"),
            update("done"),
            stop(3, "end_turn"),
        ];
        let (result, sent) = drive_lines(&request(StepAccess::Write), &lines, &mut interactions);
        result.unwrap();
        let answer = sent.iter().find(|message| message["id"] == 90).unwrap();
        assert_eq!(answer["result"]["outcome"]["optionId"], "deny");
        let permissions = daemon.permissions.lock().unwrap();
        assert_eq!(permissions[0].kind, "execute");
        assert_eq!(permissions[0].title, "tool");
        assert_eq!(
            permissions[0].options[0].kind,
            formless_protocol::HarnessPermissionOptionKind::AllowOnce
        );
        assert!(matches!(
            daemon.updates.lock().unwrap().as_slice(),
            [HarnessUpdate::Message { content }] if content == "done"
        ));
        let answers = interactions
            .ask_question(&json!([{"question": "which?"}]), &CancellationToken::new())
            .unwrap();
        assert_eq!(answers, json!([{"selected": [], "text": "which?"}]));
    }

    #[tokio::test]
    async fn run_interactive_survives_rejected_updates() {
        use crate::local_harness::InteractionFuture;
        struct Reported;
        impl InteractionHandler for Reported {
            fn request_permission<'a>(
                &'a self,
                _: HarnessPermissionRequest,
                _: &'a CancellationToken,
            ) -> InteractionFuture<'a, Option<String>> {
                Box::pin(async { Some("allow".into()) })
            }
            fn ask_question<'a>(
                &'a self,
                _: Vec<formless_protocol::Question>,
                _: &'a CancellationToken,
            ) -> InteractionFuture<'a, Option<Vec<formless_protocol::QuestionAnswer>>> {
                Box::pin(async { None })
            }
            fn on_update(&self, _: HarnessUpdate) -> Result<()> {
                bail!("attempt already reported")
            }
        }
        fn adapter(
            request: &HarnessRequest,
            permissions: &mut dyn PermissionHandler,
            _: &CancellationToken,
        ) -> Result<HarnessResult> {
            let lines = [
                LIVE_INITIALIZE.into(),
                SESSION_NEW.into(),
                permission(90, "execute"),
                update("done"),
                stop(3, "end_turn"),
            ];
            drive_lines(request, &lines, permissions).0
        }
        let result = run_interactive(
            adapter,
            request(StepAccess::Write),
            &Reported,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.message, "done");
    }

    #[tokio::test]
    async fn aborting_guarded_run_cancels_blocking_adapter_before_spawn() {
        use crate::local_harness::InteractionFuture;
        use std::{fs, path::PathBuf, thread};

        struct NoInteractions;
        impl InteractionHandler for NoInteractions {
            fn request_permission<'a>(
                &'a self,
                _: HarnessPermissionRequest,
                _: &'a CancellationToken,
            ) -> InteractionFuture<'a, Option<String>> {
                Box::pin(async { None })
            }
            fn ask_question<'a>(
                &'a self,
                _: Vec<formless_protocol::Question>,
                _: &'a CancellationToken,
            ) -> InteractionFuture<'a, Option<Vec<formless_protocol::QuestionAnswer>>> {
                Box::pin(async { None })
            }
            fn on_update(&self, _: HarnessUpdate) -> Result<()> {
                Ok(())
            }
        }

        fn wait_then_spawn(
            request: &HarnessRequest,
            _: &mut dyn PermissionHandler,
            cancel: &CancellationToken,
            spawn: &mut ProcessSpawner,
        ) -> Result<HarnessResult> {
            let path = |name: &str| -> Result<PathBuf> {
                Ok(PathBuf::from(
                    request
                        .environment
                        .get(&OsString::from(name))
                        .context("missing abort-test path")?,
                ))
            };
            let started = path("ACP_ABORT_STARTED")?;
            let release = path("ACP_ABORT_RELEASE")?;
            let finished = path("ACP_ABORT_FINISHED")?;
            let spawn_called = path("ACP_ABORT_SPAWN_CALLED")?;
            let spawn_marker = path("ACP_ABORT_SPAWN_MARKER")?;
            fs::write(started, "ready")?;
            while !release.exists() {
                thread::sleep(Duration::from_millis(5));
            }
            if cancel.is_cancelled() {
                fs::write(finished, "cancelled")?;
                bail!("cancelled before spawn");
            }
            fs::write(finished, "not-cancelled")?;
            let mut command = Command::new("touch");
            command.arg(spawn_marker);
            let _process = spawn(command, cancel)?;
            fs::write(spawn_called, "spawned")?;
            bail!("adapter reached spawn after wrapper was dropped")
        }

        let dir = std::env::temp_dir().join(format!("formless-acp-abort-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let started = dir.join("started");
        let release = dir.join("release");
        let finished = dir.join("finished");
        let spawn_called = dir.join("spawn-called");
        let spawn_marker = dir.join("spawn-marker");
        let mut request = request(StepAccess::Write);
        request.environment.extend([
            ("ACP_ABORT_STARTED".into(), started.clone().into_os_string()),
            ("ACP_ABORT_RELEASE".into(), release.clone().into_os_string()),
            (
                "ACP_ABORT_FINISHED".into(),
                finished.clone().into_os_string(),
            ),
            (
                "ACP_ABORT_SPAWN_CALLED".into(),
                spawn_called.clone().into_os_string(),
            ),
            (
                "ACP_ABORT_SPAWN_MARKER".into(),
                spawn_marker.clone().into_os_string(),
            ),
        ]);
        let caller_cancel = CancellationToken::new();
        let cancel = caller_cancel.clone();
        let handler = NoInteractions;
        let spawn_called_for_closure = spawn_called.clone();
        let task = tokio::spawn(async move {
            run_interactive_with_spawn(
                wait_then_spawn,
                request,
                &handler,
                &cancel,
                move |command, _| {
                    fs::write(&spawn_called_for_closure, "spawned")?;
                    SupervisedProcess::spawn(command).map_err(|error| anyhow!(error))
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("blocking adapter did not start");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        fs::write(&release, "continue").unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while fs::read_to_string(&finished).ok().as_deref() != Some("cancelled") {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("blocking adapter did not observe cancellation");
        assert_eq!(fs::read_to_string(finished).unwrap(), "cancelled");
        assert!(!spawn_called.exists());
        assert!(!spawn_marker.exists());
        assert!(!caller_cancel.is_cancelled());
        fs::remove_dir_all(dir).unwrap();
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
        let (result, _) = drive_lines(&request(StepAccess::Write), &lines, &mut handler);
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
        let (result, sent) = drive_spec(
            &spec,
            &request(StepAccess::ReadOnly),
            &lines,
            &mut RejectAll,
        );
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
        let (result, sent) = drive_spec(
            &spec,
            &request(StepAccess::ReadOnly),
            &lines,
            &mut RejectAll,
        );
        result.unwrap();
        assert_eq!(sent[2]["method"], "session/prompt");

        let strict = AgentSpec {
            read_only_modes: &[],
            ..SPEC
        };
        let (result, _) = drive_spec(
            &strict,
            &request(StepAccess::ReadOnly),
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
            permission(90, "edit"),
            update("done"),
            stop(4, "end_turn"),
        ];
        let mut handler = Choose("allow", 0);
        let (result, sent) = drive_lines(&request(StepAccess::ReadOnly), &lines, &mut handler);
        result.unwrap();
        assert_eq!(sent[2]["method"], "session/set_mode");
        assert_eq!(handler.1, 0, "review runs never ask about edits");
        let answer = sent.iter().find(|message| message["id"] == 90).unwrap();
        assert_eq!(answer["result"]["outcome"]["optionId"], "deny");
    }

    #[test]
    fn selects_requested_model_or_fails() {
        let mut with_model = request(StepAccess::Write);
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
        let (result, sent) = drive_lines(&request(StepAccess::Write), &lines, &mut RejectAll);
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
                drive_lines(&request(StepAccess::Write), &[initialize], &mut RejectAll);
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
            let (result, _) = drive_lines(&request(StepAccess::Write), &all, &mut RejectAll);
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
        let (result, sent) = drive_lines(&request(StepAccess::Write), &lines, &mut RejectAll);
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
            &request(StepAccess::Write),
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
            (SPEC.args)(&request(StepAccess::Write)),
            ["acp", "--disable-telemetry"]
        );
        assert_eq!(
            (SPEC.args)(&request(StepAccess::ReadOnly)),
            ["acp", "--disable-telemetry", "--sandbox"]
        );
    }

    #[test]
    fn capability_stays_out_of_agy_environment() {
        let request = request(StepAccess::Write);
        let environment = agent_environment(&request);
        assert!(!environment.contains_key(&OsString::from("FORMLESS_TOPIC_CAPABILITY")));
        assert_eq!(environment[&OsString::from("HOME")], "/home/user");
    }

    #[test]
    fn validates_bridge_environment() {
        let mut missing = request(StepAccess::Write);
        missing.environment.clear();
        assert!(validate(&missing)
            .unwrap_err()
            .to_string()
            .contains("has no value"));
        let mut relative = request(StepAccess::Write);
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
        let mut request = request(StepAccess::Write);
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
