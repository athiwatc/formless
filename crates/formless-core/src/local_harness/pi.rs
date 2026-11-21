use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use formless_protocol::{HarnessAvailability, HarnessKind, Question, QuestionAnswer};
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch};
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    validate_request, CommandSpec, HarnessRequest, HarnessResult, HarnessUpdate,
    InteractionHandler, StepAccess,
};

pub const MCP_CONFIG_ENV: &str = "FORMLESS_PI_MCP_CONFIG";
const BRIDGE_CUSTOM_TYPE: &str = "formless-pi-bridge";
const BRIDGE_MESSAGE: &str = "formless-pi-mcp-registered";
const EXTENSION_NAME: &str = "formless-pi-extension.ts";
const EXTENSION_SOURCE: &str = include_str!("pi_extension.ts");
const SIGN_IN_UNKNOWN: &str = "Sign-in unknown: Pi's read-only auth check requires a provider or model; there is no read-only CLI check for any provider. --list-models initializes writable state; auth.json alone omits environment, custom and ambient credentials.";

pub async fn availability(executable: Option<&Path>) -> HarnessAvailability {
    let override_executable = std::env::var_os("FORMLESS_TOPIC_PI_EXE");
    let search_path = std::env::var_os("PATH");
    availability_with_path(
        executable,
        override_executable.as_deref(),
        search_path.as_deref(),
    )
    .await
}

async fn availability_with_path(
    executable: Option<&Path>,
    override_executable: Option<&OsStr>,
    search_path: Option<&OsStr>,
) -> HarnessAvailability {
    let program = executable
        .or_else(|| override_executable.map(Path::new))
        .unwrap_or_else(|| Path::new("pi"));
    let resolved = find_pi_executable(program, search_path);
    let mut availability = HarnessAvailability {
        kind: HarnessKind::Pi,
        acp_agent: None,
        installed: resolved.is_some(),
        signed_in: None,
        version: None,
        detail: Some(SIGN_IN_UNKNOWN.into()),
    };
    let version = match resolved {
        Some(program) => probe_version(&program).await,
        None => Err("Pi executable was not found or is not executable"),
    };
    match version {
        Ok(version) => availability.version = Some(version),
        Err(detail) => availability.detail = Some(format!("{detail}. {SIGN_IN_UNKNOWN}")),
    }
    availability
}

fn find_pi_executable(program: &Path, search_path: Option<&OsStr>) -> Option<PathBuf> {
    fn candidate(path: &Path) -> Option<PathBuf> {
        #[cfg(windows)]
        if path.extension().is_none() {
            if let Some(path) = candidate(&path.with_extension("exe")) {
                return Some(path);
            }
        }
        if !path.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
            if unsafe {
                libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS)
            } != 0
            {
                return None;
            }
        }
        Some(path.to_owned())
    }

    if program.as_os_str().is_empty() {
        return None;
    }
    if program.is_absolute() || program.components().count() > 1 {
        return candidate(program);
    }
    for directory in std::env::split_paths(search_path?) {
        let path = directory.join(program);
        if let Some(path) = candidate(&path) {
            return Some(path);
        }
    }
    None
}

async fn probe_version(executable: &Path) -> std::result::Result<String, &'static str> {
    let mut child = tokio::process::Command::new(executable)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Pi version probe could not start")?;
    let stdout = child.stdout.take().expect("piped Pi version stdout");
    let result = timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        stdout
            .take(129)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| "Pi version output could not be read")?;
        if bytes.len() > 128 {
            return Err("Pi version output exceeded its limit");
        }
        if !child
            .wait()
            .await
            .map_err(|_| "Pi version probe could not be reaped")?
            .success()
        {
            return Err("Pi version probe exited unsuccessfully");
        }
        let version = std::str::from_utf8(&bytes)
            .ok()
            .map(str::trim)
            .filter(|value| {
                value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
            })
            .filter(|value| {
                let core = value.split(['-', '+']).next().unwrap_or_default();
                let components: Vec<_> = core.split('.').collect();
                components.len() == 3
                    && components.iter().all(|part| {
                        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
            })
            .ok_or("Pi version output was not a version number")?;
        Ok(version.to_owned())
    })
    .await
    .unwrap_or(Err("Pi version probe timed out"));
    if result.is_err() {
        let _ = child.kill().await;
    }
    result
}

const WRITE_GUIDANCE: &str =
    "Access: Write. Implement the requested changes in the working directory.";
const READ_ONLY_GUIDANCE: &str =
    "Access: ReadOnly. Inspect the requested changes and report findings without modifying files.";

pub fn stage_extension(attempt_private_dir: &Path) -> Result<PathBuf> {
    if !attempt_private_dir.is_absolute() {
        bail!("Pi extension staging directory must be absolute");
    }

    let dir_metadata = fs::symlink_metadata(attempt_private_dir)
        .context("Pi extension staging directory does not exist or cannot be inspected")?;
    if dir_metadata.file_type().is_symlink() || !dir_metadata.is_dir() {
        bail!("Pi extension staging path must be a real directory, not a symlink");
    }

    let canonical_dir = fs::canonicalize(attempt_private_dir)
        .context("failed to canonicalize Pi extension staging directory")?;
    if !canonical_dir.is_absolute() {
        bail!("canonical Pi extension staging directory must be absolute");
    }
    if canonical_dir.as_os_str() != attempt_private_dir.as_os_str() {
        bail!("Pi extension staging directory must be canonical and must not traverse symlinks");
    }

    let extension_path = canonical_dir.join(EXTENSION_NAME);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut extension_file = options.open(&extension_path).with_context(|| {
        format!(
            "failed to stage Pi extension at {}; the attempt-private path must be unused",
            extension_path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        extension_file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    extension_file
        .write_all(EXTENSION_SOURCE.as_bytes())
        .context("failed to write staged Pi extension")?;
    extension_file
        .sync_all()
        .context("failed to flush staged Pi extension")?;
    Ok(extension_path)
}

pub fn command_spec(_request: &HarnessRequest) -> Result<CommandSpec> {
    bail!("Pi requires interactive RPC execution; call local_harness::pi::run with an attempt-private staged extension from pi::stage_extension")
}

pub fn command_spec_with_extension(
    request: &HarnessRequest,
    extension_path: &Path,
) -> Result<CommandSpec> {
    validate_request(request)?;
    if request.kind != super::HarnessKind::Pi {
        bail!("Pi command specification requires HarnessKind::Pi");
    }
    if request
        .environment
        .contains_key(&OsString::from(MCP_CONFIG_ENV))
        || request
            .mcp_env_vars
            .iter()
            .any(|name| name == MCP_CONFIG_ENV)
    {
        bail!("{MCP_CONFIG_ENV} is reserved for Pi adapter configuration");
    }

    let extension_path = canonical_extension_path(extension_path)?;
    let command = request
        .mcp_command
        .to_str()
        .context("MCP executable path must be valid UTF-8 for Pi")?;
    let cwd = request
        .cwd
        .to_str()
        .context("harness working directory must be valid UTF-8 for Pi")?;
    let server_name = format!("formless_{}", Uuid::new_v4().simple());
    let mcp_config = json!({
        "name": server_name,
        "command": command,
        "args": request.mcp_args,
        "cwd": cwd,
        "envVars": request.mcp_env_vars,
    })
    .to_string();

    let mut args = vec![
        "--mode".into(),
        "rpc".into(),
        "--no-session".into(),
        "--no-extensions".into(),
        "-e".into(),
        "builtin:mcp".into(),
        "-e".into(),
        extension_path.as_os_str().to_owned(),
    ];
    if let Some(model) = &request.model {
        args.extend(["--model".into(), model.into()]);
    }
    args.extend([
        "--append-system-prompt".into(),
        match request.access {
            StepAccess::Write => WRITE_GUIDANCE,
            StepAccess::ReadOnly => READ_ONLY_GUIDANCE,
        }
        .into(),
    ]);
    if request.access == StepAccess::ReadOnly {
        args.extend([
            "--tools".into(),
            format!("read,grep,find,ls,mcp__{server_name}__*").into(),
        ]);
    }

    let mut environment = request.environment.clone();
    environment.insert(OsString::from(MCP_CONFIG_ENV), OsString::from(mcp_config));
    Ok(CommandSpec {
        program: request
            .executable
            .clone()
            .unwrap_or_else(|| PathBuf::from("pi")),
        args,
        cwd: request.cwd.clone(),
        environment,
    })
}

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const STDERR_LIMIT: usize = 8192;

struct PiChild(tokio::process::Child);

impl std::ops::Deref for PiChild {
    type Target = tokio::process::Child;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for PiChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl PiChild {
    fn terminate(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.id() {
            terminate_unix_tree(pid);
        }
        let _ = self.0.start_kill();
    }
}

impl Drop for PiChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(unix)]
fn terminate_unix_tree(root: u32) {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // Retain the waitable leader's identity until group cleanup is complete.
    if unsafe {
        libc::waitid(
            libc::P_PID,
            root as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        return;
    }
    unsafe {
        libc::killpg(root as libc::pid_t, libc::SIGSTOP);
    }
    let mut stopped = vec![(root, root)];
    let mut frontier = BTreeSet::from([root]);
    // Freeze parents before discovering their children, so they cannot fork or reap them.
    for _ in 0..64 {
        let Ok(snapshot) = std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid=,pgid="])
            .output()
        else {
            break;
        };
        if !snapshot.status.success() {
            break;
        }
        let mut next = BTreeSet::new();
        for line in String::from_utf8_lossy(&snapshot.stdout).lines() {
            let fields: Vec<u32> = line
                .split_whitespace()
                .filter_map(|field| field.parse().ok())
                .collect();
            let [pid, parent, group] = fields.as_slice() else {
                continue;
            };
            if !frontier.contains(parent) || *pid == root {
                continue;
            }
            unsafe {
                libc::kill(*pid as libc::pid_t, libc::SIGSTOP);
                if pid == group {
                    libc::killpg(*group as libc::pid_t, libc::SIGSTOP);
                }
            }
            stopped.push((*pid, *group));
            next.insert(*pid);
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    // Kill leaves first while their stopped parents still retain their PID identities.
    for (pid, group) in stopped.into_iter().rev() {
        unsafe {
            if pid == group {
                libc::killpg(group as libc::pid_t, libc::SIGKILL);
            }
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

pub async fn run(
    request: &HarnessRequest,
    staged_extension: &Path,
    handler: &dyn InteractionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    if cancel.is_cancelled() {
        bail!("Pi run cancelled");
    }
    let spec = command_spec_with_extension(request, staged_extension)?;
    let mut command = tokio::process::Command::from(spec.command());
    #[cfg(unix)]
    {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            bail!("Pi process supervision requires waitable children");
        }
        command.process_group(0);
    }
    let mut child = PiChild(
        command
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start Pi RPC process")?,
    );
    let mut stdin = child.stdin.take().context("Pi stdin unavailable")?;
    let stdout = child.stdout.take().context("Pi stdout unavailable")?;
    let mut stderr = child.stderr.take().context("Pi stderr unavailable")?;
    let tail = Arc::new(Mutex::new(Vec::new()));
    let captured_tail = tail.clone();
    let stderr_task = tokio::spawn(async move {
        let mut buffer = [0; 4096];
        loop {
            let count = stderr.read(&mut buffer).await?;
            if count == 0 {
                return std::io::Result::Ok(());
            }
            let mut tail = captured_tail.lock().unwrap();
            tail.extend_from_slice(&buffer[..count]);
            let excess = tail.len().saturating_sub(STDERR_LIMIT);
            tail.drain(..excess);
        }
    });
    let (input, mut commands) = mpsc::unbounded_channel::<Value>();
    let (writer_error, errors) = watch::channel(false);
    let mut writer = tokio::spawn(async move {
        let result = async {
            while let Some(command) = commands.recv().await {
                let mut bytes = serde_json::to_vec(&command)?;
                bytes.push(b'\n');
                stdin.write_all(&bytes).await?;
                stdin.flush().await?;
            }
            stdin.shutdown().await?;
            anyhow::Ok(())
        }
        .await;
        if result.is_err() {
            let _ = writer_error.send(true);
        }
        result
    });
    let outcome = rpc_session(stdout, input, errors, &request.prompt, handler, cancel).await;
    if outcome.is_err() {
        child.terminate();
    }
    let write_result = match timeout(SHUTDOWN_TIMEOUT, &mut writer).await {
        Ok(result) => result
            .context("Pi RPC writer failed")
            .and_then(|result| result),
        Err(_) => {
            writer.abort();
            Err(anyhow::anyhow!("Pi RPC stdin did not close in time"))
        }
    };
    let exit = match timeout(SHUTDOWN_TIMEOUT, child.wait()).await {
        Ok(status) => status.context("failed waiting for Pi RPC process"),
        Err(_) => {
            child.terminate();
            let _ = child.kill().await;
            Err(anyhow::anyhow!("Pi RPC process did not exit in time"))
        }
    };
    let mut stderr_task = stderr_task;
    if timeout(SHUTDOWN_TIMEOUT, &mut stderr_task).await.is_err() {
        stderr_task.abort();
    }
    let stderr_tail = String::from_utf8_lossy(&tail.lock().unwrap()).into_owned();
    let finish = || -> Result<HarnessResult> {
        let result = outcome?;
        write_result?;
        let status = exit?;
        if !status.success() {
            bail!("Pi RPC process exited unsuccessfully: {status}");
        }
        Ok(result)
    };
    finish().map_err(|error| {
        let context = format!("Pi RPC execution failed: {error}; stderr tail: {stderr_tail}");
        error.context(context)
    })
}

async fn rpc_session(
    stdout: tokio::process::ChildStdout,
    input: mpsc::UnboundedSender<Value>,
    mut writer_errors: watch::Receiver<bool>,
    prompt: &str,
    handler: &dyn InteractionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    let send = |value| input.send(value).context("Pi RPC stdin closed");
    send(json!({"id": "state-1", "type": "get_state"}))?;
    send(json!({"id": "prompt-1", "type": "prompt", "message": prompt}))?;
    let mut lines = BufReader::new(stdout).lines();
    let mut events = Vec::new();
    let mut dialogs = FuturesUnordered::new();
    let mut open = BTreeSet::new();
    let mut prompt_accepted = false;
    let mut state_received = false;
    let mut session_id = None;
    let mut settled = false;
    let mut cancelling = false;
    let mut cancel_deadline = Instant::now();
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled(), if !cancelling => {
                cancelling = true;
                cancel_deadline = Instant::now() + SHUTDOWN_TIMEOUT;
                for id in &open {
                    send(json!({"type": "extension_ui_response", "id": id, "cancelled": true}))?;
                }
                dialogs.clear();
                open.clear();
                send(json!({"id": "abort-1", "type": "abort"}))?;
            }
            _ = tokio::time::sleep_until(cancel_deadline), if cancelling => {
                bail!("Pi run cancelled (abort timed out)");
            }
            changed = writer_errors.changed() => {
                changed.context("Pi RPC writer stopped unexpectedly")?;
                bail!("Pi RPC stdin write failed");
            }
            Some((id, response)) = dialogs.next(), if !dialogs.is_empty() => {
                open.remove(&id);
                if let Some(response) = response {
                    send(response)?;
                }
            }
            line = lines.next_line() => {
                let Some(line) = line.context("failed reading Pi RPC stdout")? else {
                    if cancelling { bail!("Pi run cancelled"); }
                    bail!("Pi RPC process exited before successful settle");
                };
                if line.is_empty() { continue; }
                let event: Value = serde_json::from_str(&line).context("Pi RPC returned invalid JSONL")?;
                if !event.is_object() { bail!("Pi RPC event was not an object"); }
                match event["type"].as_str() {
                    Some("response") => match event["id"].as_str() {
                        Some("abort-1") if cancelling => bail!("Pi run cancelled"),
                        Some("prompt-1") => {
                            if event["command"] != "prompt" || event["success"] != true {
                                bail!("Pi RPC prompt was rejected");
                            }
                            let disposition = event["data"]["disposition"].as_str().unwrap_or("missing");
                            if disposition != "started" {
                                bail!("Pi RPC prompt did not start an agent run (disposition: {disposition}); extension commands and handled inputs cannot complete a Formless attempt");
                            }
                            prompt_accepted = true;
                        }
                        Some("state-1") => {
                            if event["command"] != "get_state" || event["success"] != true {
                                bail!("Pi RPC get_state failed");
                            }
                            session_id = event["data"]["sessionId"].as_str().map(str::to_owned);
                            state_received = true;
                        }
                        _ => {}
                    },
                    Some("extension_ui_request") => {
                        if event["method"] == "notify" {
                            if handler.on_update(HarnessUpdate::Message {
                                content: event["message"].as_str().context("Pi notification missing message")?.to_owned(),
                            }).is_err() {
                                tracing::warn!("Pi notification update handler failed");
                            }
                        } else if let Some(dialog) = Dialog::from_event(&event)? {
                            if cancelling {
                                send(dialog.cancelled())?;
                            } else {
                                if !open.insert(dialog.id.clone()) { bail!("Pi RPC reused an open dialog id"); }
                                dialogs.push(dialog.answer(handler, cancel));
                            }
                        }
                    }
                    Some("agent_start") => settled = false,
                    Some("agent_settled") => {
                        if cancelling { bail!("Pi run cancelled"); }
                        if event["aborted"] != false { bail!("Pi RPC run aborted"); }
                        settled = true;
                    }
                    _ => {}
                }
                if !cancelling {
                    for update in stream_updates(&event) {
                        if handler.on_update(update).is_err() {
                            tracing::warn!("Pi stream update handler failed");
                        }
                    }
                }
                events.push(event);
                if settled && prompt_accepted && state_received && !cancel.is_cancelled() {
                    for id in &open {
                        send(json!({"type": "extension_ui_response", "id": id, "cancelled": true}))?;
                    }
                    let output = events.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
                    let mut result = parse_output(&output)?;
                    result.session_id = session_id;
                    return Ok(result);
                }
            }
        }
    }
}

struct Dialog {
    id: String,
    method: String,
    question: Question,
    deadline: Option<Instant>,
}

impl Dialog {
    fn from_event(event: &Value) -> Result<Option<Self>> {
        let method = event["method"]
            .as_str()
            .context("Pi UI request missing method")?;
        if !matches!(method, "confirm" | "select" | "input" | "editor") {
            return Ok(None);
        }
        let id = event["id"]
            .as_str()
            .context("Pi UI request missing id")?
            .to_owned();
        let title = event["title"]
            .as_str()
            .context("Pi UI request missing title")?;
        let options = match method {
            "confirm" => vec!["Yes".to_owned(), "No".to_owned()],
            "select" => event["options"]
                .as_array()
                .context("Pi select missing options")?
                .iter()
                .map(|option| {
                    option
                        .as_str()
                        .map(str::to_owned)
                        .context("Pi select option was not text")
                })
                .collect::<Result<Vec<_>>>()?,
            _ => Vec::new(),
        };
        let text = if method == "confirm" {
            event["message"]
                .as_str()
                .context("Pi confirm missing message")?
        } else {
            title
        };
        let mut question = Question::single(text.to_owned(), options);
        question.header = Some(title.to_owned());
        let deadline = event
            .get("timeout")
            .map(|value| -> Result<Option<Instant>> {
                if value.as_f64().is_some_and(|millis| millis <= 0.0) {
                    return Ok(None);
                }
                let millis = value
                    .as_u64()
                    .context("Pi UI timeout was not milliseconds")?;
                Instant::now()
                    .checked_add(Duration::from_millis(millis))
                    .context("Pi UI timeout too large")
                    .map(Some)
            })
            .transpose()?
            .flatten();
        Ok(Some(Self {
            id,
            method: method.to_owned(),
            question,
            deadline,
        }))
    }

    fn cancelled(&self) -> Value {
        json!({"type": "extension_ui_response", "id": self.id, "cancelled": true})
    }

    fn response(&self, answers: Option<Vec<QuestionAnswer>>) -> Value {
        let Some(mut answers) = answers.filter(|answers| answers.len() == 1) else {
            return self.cancelled();
        };
        let answer = answers.remove(0);
        let mut response = json!({"type": "extension_ui_response", "id": self.id});
        match self.method.as_str() {
            "confirm" | "select" => {
                let Some(selected) = answer.selected.first().filter(|selected| {
                    answer.selected.len() == 1
                        && self
                            .question
                            .options
                            .iter()
                            .any(|option| &option.label == *selected)
                }) else {
                    return self.cancelled();
                };
                if self.method == "confirm" {
                    response["confirmed"] = json!(selected == "Yes");
                } else {
                    response["value"] = json!(selected);
                }
            }
            _ => {
                let Some(text) = answer.text else {
                    return self.cancelled();
                };
                response["value"] = json!(text);
            }
        }
        response
    }

    async fn answer(
        self,
        handler: &dyn InteractionHandler,
        cancel: &CancellationToken,
    ) -> (String, Option<Value>) {
        if self
            .deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            return (self.id, None);
        }
        let cancel = cancel.child_token();
        let _guard = cancel.clone().drop_guard();
        let deadline = self.deadline;
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => Some(self.cancelled()),
            _ = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => None,
            answer = handler.ask_question(vec![self.question.clone()], &cancel) => Some(self.response(answer)),
        };
        (self.id, response)
    }
}

fn stream_updates(event: &Value) -> Vec<HarnessUpdate> {
    match event["type"].as_str() {
        Some("tool_execution_start") => vec![HarnessUpdate::ToolCall {
            name: event["toolName"].as_str().unwrap_or_default().to_owned(),
            input: event["args"].clone(),
        }],
        Some("message_end") if event["message"]["role"] == "assistant" => {
            let mut updates = Vec::new();
            if let Some(blocks) = event["message"]["content"].as_array() {
                for block in blocks {
                    if block["type"] == "thinking" {
                        if let Some(content) = block["thinking"].as_str() {
                            updates.push(HarnessUpdate::Thought {
                                content: content.to_owned(),
                            });
                        }
                    }
                }
            }
            let content = assistant_text(&event["message"]);
            if !content.is_empty() {
                updates.push(HarnessUpdate::Message { content });
            }
            updates
        }
        _ => Vec::new(),
    }
}

fn canonical_extension_path(extension_path: &Path) -> Result<PathBuf> {
    if !extension_path.is_absolute() {
        bail!("staged Pi extension path must be absolute");
    }
    let metadata = fs::symlink_metadata(extension_path)
        .context("staged Pi extension does not exist or cannot be inspected")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("staged Pi extension must be a regular file, not a symlink");
    }
    let canonical = fs::canonicalize(extension_path)
        .context("failed to canonicalize staged Pi extension path")?;
    if canonical.as_os_str() != extension_path.as_os_str() {
        bail!("staged Pi extension path must be canonical");
    }
    if fs::read(&canonical).context("failed to read staged Pi extension")?
        != EXTENSION_SOURCE.as_bytes()
    {
        bail!("staged Pi extension does not match the embedded Formless extension");
    }
    Ok(canonical)
}

pub fn parse_output(stdout: &str) -> Result<HarnessResult> {
    let mut events = Vec::new();
    let mut session_id = None;
    let mut bridge_registered = false;
    let mut settled = None;
    let mut final_assistant = None;
    let mut agent_end = None;

    for (index, raw_line) in stdout.split('\n').enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line)
            .with_context(|| format!("Pi CLI returned invalid JSONL event {}", index + 1))?;
        if !event.is_object() {
            bail!("Pi CLI JSONL event {} was not an object", index + 1);
        }

        let event_position = events.len();
        match event.get("type").and_then(Value::as_str) {
            Some("agent_start") => {
                settled = None;
                agent_end = None;
                final_assistant = None;
            }
            Some("turn_start" | "error") => {
                settled = None;
            }
            Some("session") => {
                session_id = event.get("id").and_then(Value::as_str).map(str::to_owned);
            }
            Some("message_end") => {
                let message = event
                    .get("message")
                    .filter(|message| message.is_object())
                    .with_context(|| {
                        format!("Pi CLI JSONL event {} had no message object", index + 1)
                    })?;
                if message["role"].as_str() == Some("custom")
                    && message["customType"].as_str() == Some(BRIDGE_CUSTOM_TYPE)
                    && message["content"].as_str() == Some(BRIDGE_MESSAGE)
                {
                    bridge_registered = true;
                }
                if message["role"].as_str() == Some("assistant") {
                    let stop_reason = message
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned();
                    let text = assistant_text(message);
                    final_assistant = Some((text, stop_reason, event_position));
                }
            }
            Some("agent_end") => {
                agent_end = event
                    .get("willRetry")
                    .and_then(Value::as_bool)
                    .map(|will_retry| (will_retry, event_position));
            }
            Some("agent_settled") => {
                settled = event
                    .get("aborted")
                    .and_then(Value::as_bool)
                    .map(|aborted| (aborted, event_position));
            }
            _ => {}
        }
        events.push(event);
    }

    if events.is_empty() {
        bail!("Pi CLI returned empty JSONL output");
    }
    if !bridge_registered {
        bail!("Pi CLI output did not confirm Formless MCP bridge registration; check Pi compatibility and extension-loading diagnostics");
    }
    let (will_retry, agent_end_position) =
        agent_end.context("Pi CLI JSONL did not contain a complete agent_end event")?;
    if will_retry {
        bail!("Pi CLI ended while another agent run was still scheduled");
    }
    let (aborted, settled_position) = match settled {
        Some(settled) => settled,
        None => bail!("Pi CLI JSONL did not contain a complete agent_settled event"),
    };
    if aborted {
        bail!("Pi CLI reported that the run was aborted");
    }
    if settled_position < agent_end_position {
        bail!("Pi CLI agent_settled event preceded its final agent_end event");
    }
    let (message, stop_reason, assistant_position) =
        final_assistant.context("Pi CLI JSONL contained no final assistant message")?;
    if assistant_position > agent_end_position {
        bail!("Pi CLI final assistant message did not complete before agent_end");
    }
    if stop_reason != "stop" {
        bail!(
            "Pi CLI final assistant message did not finish normally (stop reason: {stop_reason})"
        );
    }
    if message.trim().is_empty() {
        bail!("Pi CLI final assistant message contained no text");
    }

    Ok(HarnessResult {
        session_id,
        message,
        events,
    })
}

fn assistant_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod rpc_tests {
    #[cfg(unix)]
    use super::tests::{request, staged_extension};
    use super::*;
    use crate::local_harness::{HarnessPermissionRequest, InteractionFuture};
    use tokio::sync::Notify;

    // Concurrent fork/exec can briefly inherit writable shell-fixture descriptors (ETXTBSY).
    #[cfg(unix)]
    static FAKE_PROCESS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct RecordingHandler {
        answer: bool,
        wait: bool,
        questions: Mutex<Vec<Question>>,
        updates: Mutex<Vec<HarnessUpdate>>,
        reject_updates: bool,
        asked: Notify,
    }

    impl RecordingHandler {
        fn new(answer: bool, wait: bool) -> Self {
            Self {
                answer,
                wait,
                questions: Mutex::new(Vec::new()),
                updates: Mutex::new(Vec::new()),
                reject_updates: false,
                asked: Notify::new(),
            }
        }
    }

    impl InteractionHandler for RecordingHandler {
        fn request_permission<'a>(
            &'a self,
            _: HarnessPermissionRequest,
            _: &'a CancellationToken,
        ) -> InteractionFuture<'a, Option<String>> {
            panic!("Pi must not invent core permission prompts")
        }

        fn ask_question<'a>(
            &'a self,
            questions: Vec<Question>,
            cancel: &'a CancellationToken,
        ) -> InteractionFuture<'a, Option<Vec<QuestionAnswer>>> {
            Box::pin(async move {
                self.questions.lock().unwrap().extend(questions.clone());
                self.asked.notify_one();
                if self.wait {
                    cancel.cancelled().await;
                    return None;
                }
                self.answer.then(|| {
                    questions
                        .into_iter()
                        .map(|question| {
                            if question.options.is_empty() {
                                QuestionAnswer {
                                    selected: vec![],
                                    text: Some("literal answer".into()),
                                }
                            } else {
                                let label = if question.options.iter().any(|o| o.label == "Yes") {
                                    "Yes"
                                } else {
                                    "Review"
                                };
                                QuestionAnswer {
                                    selected: vec![label.into()],
                                    text: None,
                                }
                            }
                        })
                        .collect()
                })
            })
        }

        fn on_update(&self, update: HarnessUpdate) -> Result<()> {
            self.updates.lock().unwrap().push(update);
            if self.reject_updates {
                bail!("update rejected");
            }
            Ok(())
        }
    }

    fn ui(method: &str) -> Value {
        json!({"type":"extension_ui_request","id":method,"method":method,"title":"Probe title","message":"Allow probe?","options":["Build","Review"]})
    }

    #[test]
    fn maps_dialog_questions_answers_and_invalid_or_absent_answers() {
        for method in ["confirm", "select", "input", "editor"] {
            let dialog = Dialog::from_event(&ui(method)).unwrap().unwrap();
            assert_eq!(dialog.question.header.as_deref(), Some("Probe title"));
            assert!(!dialog.question.multi_select);
            let answer = match method {
                "confirm" => QuestionAnswer {
                    selected: vec!["Yes".into()],
                    text: None,
                },
                "select" => QuestionAnswer {
                    selected: vec!["Review".into()],
                    text: None,
                },
                _ => QuestionAnswer {
                    selected: vec![],
                    text: Some("literal @answer\nnext".into()),
                },
            };
            let response = dialog.response(Some(vec![answer]));
            assert_eq!(response["id"], method);
            assert_eq!(response["type"], "extension_ui_response");
            if method == "confirm" {
                assert_eq!(dialog.question.question, "Allow probe?");
                assert_eq!(
                    dialog
                        .question
                        .options
                        .iter()
                        .map(|o| o.label.as_str())
                        .collect::<Vec<_>>(),
                    ["Yes", "No"]
                );
                assert_eq!(response["confirmed"], true);
                assert_eq!(
                    dialog.response(Some(vec![QuestionAnswer {
                        selected: vec!["No".into()],
                        text: None
                    }]))["confirmed"],
                    false
                );
            } else if method == "select" {
                assert_eq!(response["value"], "Review");
            } else {
                assert!(dialog.question.options.is_empty());
                assert_eq!(response["value"], "literal @answer\nnext");
            }
            for answers in [None, Some(vec![]), Some(vec![QuestionAnswer::default()])] {
                assert_eq!(dialog.response(answers)["cancelled"], true);
            }
        }
        assert!(Dialog::from_event(&ui("notify")).unwrap().is_none());
        assert!(Dialog::from_event(&json!({"method":"confirm"})).is_err());
        assert!(
            Dialog::from_event(&json!({"method":"select","id":"x","title":"x","options":[1]}))
                .is_err()
        );
    }

    #[test]
    fn maps_assistant_thought_text_and_tool_updates_without_deltas_or_duplicates() {
        let updates = stream_updates(
            &json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"reason"},{"type":"text","text":"answer"}]}}),
        );
        assert!(matches!(&updates[0], HarnessUpdate::Thought { content } if content == "reason"));
        assert!(matches!(&updates[1], HarnessUpdate::Message { content } if content == "answer"));
        let updates = stream_updates(
            &json!({"type":"tool_execution_start","toolName":"read","args":{"path":"file"}}),
        );
        assert!(
            matches!(&updates[0], HarnessUpdate::ToolCall { name, input } if name == "read" && input["path"] == "file")
        );
        assert!(stream_updates(&json!({"type":"message_update"})).is_empty());
    }

    #[tokio::test]
    async fn dialog_timeout_sends_nothing_and_cancels_handler() {
        let handler = RecordingHandler::new(true, true);
        let mut event = ui("confirm");
        event["timeout"] = json!(10);
        let dialog = Dialog::from_event(&event).unwrap().unwrap();
        let (_, response) = timeout(
            Duration::from_secs(1),
            dialog.answer(&handler, &CancellationToken::new()),
        )
        .await
        .unwrap();
        assert!(response.is_none());
        assert_eq!(handler.questions.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn dialog_nonpositive_timeouts_still_ask_the_handler() {
        for millis in [0, -1] {
            let mut event = ui("confirm");
            event["timeout"] = json!(millis);
            let dialog = Dialog::from_event(&event).unwrap().unwrap();
            assert!(dialog.deadline.is_none());
            let handler = RecordingHandler::new(true, false);
            let (_, response) = timeout(
                Duration::from_secs(1),
                dialog.answer(&handler, &CancellationToken::new()),
            )
            .await
            .unwrap();
            assert_eq!(response.unwrap()["confirmed"], true);
            assert_eq!(handler.questions.lock().unwrap().len(), 1);
        }
    }

    #[cfg(unix)]
    fn fake_pi(body: &str) -> (PathBuf, PathBuf, HarnessRequest) {
        use std::os::unix::fs::PermissionsExt;
        let (dir, extension) = staged_extension();
        let executable = dir.join("fake-pi");
        fs::write(&executable, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut request = request(StepAccess::Write);
        request.cwd = dir.clone();
        request.executable = Some(executable);
        request.prompt = "@alice literal RPC prompt".into();
        (dir, extension, request)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_availability_reports_version_without_probing_credentials() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let (dir, _, request) = fake_pi(
            r#"[ "$#" -eq 1 ] && [ "$1" = --version ] || exit 99
printf '1.6.0\r\n'
printf 'private diagnostic' >&2"#,
        );
        let result = availability(request.executable.as_deref()).await;
        assert_eq!(result.kind, HarnessKind::Pi);
        assert_eq!(result.acp_agent, None);
        assert!(result.installed);
        assert_eq!(result.version.as_deref(), Some("1.6.0"));
        assert_eq!(result.signed_in, None);
        assert_eq!(result.detail.as_deref(), Some(SIGN_IN_UNKNOWN));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_availability_preserves_symlink_shim_name() {
        use std::os::unix::fs::symlink;
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let (dir, _, request) = fake_pi(
            r#"[ "${0##*/}" = pi ] || exit 9
printf '1.6.0\n'"#,
        );
        let shim = dir.join("pi");
        symlink(request.executable.unwrap(), &shim).unwrap();
        assert_eq!(find_pi_executable(&shim, None), Some(shim.clone()));
        for explicit in [Some(shim.as_path()), None] {
            let result = availability_with_path(explicit, None, Some(dir.as_os_str())).await;
            assert!(result.installed);
            assert_eq!(result.version.as_deref(), Some("1.6.0"));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_availability_obeys_explicit_override_and_path_lookup() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let (dir, _, request) = fake_pi("printf '1.6.0\\n'");
        let program = dir.join("pi");
        fs::rename(request.executable.unwrap(), &program).unwrap();
        let path = dir.as_os_str();
        let missing = dir.join("missing");
        for (explicit, configured, search) in [
            (Some(program.as_path()), Some(missing.as_os_str()), None),
            (None, Some(program.as_os_str()), None),
            (None, None, Some(path)),
        ] {
            let result = availability_with_path(explicit, configured, search).await;
            assert!(result.installed);
            assert_eq!(result.version.as_deref(), Some("1.6.0"));
        }
        for (explicit, configured, search) in [
            (None, Some(missing.as_os_str()), Some(path)),
            (Some(missing.as_path()), None, Some(path)),
            (Some(dir.as_path()), None, Some(path)),
            (Some(Path::new("")), None, Some(path)),
            (None, None, None),
        ] {
            let result = availability_with_path(explicit, configured, search).await;
            assert!(!result.installed);
            assert_eq!(result.version, None);
            assert_eq!(result.signed_in, None);
            assert!(result.detail.unwrap().contains("not found"));
        }
        fs::set_permissions(&program, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            !availability_with_path(None, None, Some(path))
                .await
                .installed
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_availability_keeps_installed_on_invalid_or_failed_version_probe() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        for (body, detail) in [
            ("exit 0", "not a version number"),
            ("printf 'private diagnostic'", "not a version number"),
            ("printf '1.6.0\\n'; exit 7", "exited unsuccessfully"),
            ("printf '%0130d' 1", "exceeded its limit"),
            ("exec 1>&-; exec sleep 20", "timed out"),
        ] {
            let (dir, _, request) = fake_pi(body);
            let start = Instant::now();
            let result = availability(request.executable.as_deref()).await;
            assert!(start.elapsed() < Duration::from_secs(5));
            assert!(result.installed);
            assert_eq!(result.version, None);
            assert_eq!(result.signed_in, None);
            let message = result.detail.unwrap();
            assert!(message.contains(detail), "{message}");
            assert!(!message.contains("private diagnostic"));
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[tokio::test]
    #[ignore = "requires FORMLESS_TOPIC_PI_EXE pointing at a real Pi installation"]
    async fn live_pi_availability() {
        let result = availability(None).await;
        println!("live Pi availability: {result:?}");
        assert!(result.installed);
        assert!(result.version.is_some());
        assert_eq!(result.signed_in, None);
    }

    #[cfg(unix)]
    const START: &str = r#"
IFS= read -r state
printf '%s\n' '{"type":"response","id":"state-1","command":"get_state","success":true,"data":{"sessionId":"rpc-session"}}'
IFS= read -r prompt
printf '{"type":"received_prompt","value":%s}\n' "$prompt"
printf '%s\n' '{"type":"response","id":"prompt-1","command":"prompt","success":true,"data":{"disposition":"started"}}'
printf '%s\n' '{"type":"message_end","message":{"role":"custom","customType":"formless-pi-bridge","content":"formless-pi-mcp-registered"}}'
"#;

    #[cfg(unix)]
    const FINISH: &str = r#"
printf '%s\r\n' '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"stopReason":"stop"}}' '{"type":"agent_end","willRetry":false}' '{"type":"agent_settled","aborted":false}'
while IFS= read -r remaining; do :; done
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_rpc_succeeds_when_update_handler_always_errors() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let body = format!("{START}\nprintf '%s\\n' '{{\"type\":\"extension_ui_request\",\"id\":\"notice\",\"method\":\"notify\",\"message\":\"notification\"}}'\n{FINISH}");
        let (dir, extension, request) = fake_pi(&body);
        let mut handler = RecordingHandler::new(true, false);
        handler.reject_updates = true;
        let result = timeout(
            Duration::from_secs(10),
            run(&request, &extension, &handler, &CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.message, "done");
        assert_eq!(result.session_id.as_deref(), Some("rpc-session"));
        let updates = handler.updates.lock().unwrap();
        for text in ["notification", "done"] {
            assert!(updates.iter().any(
                |update| matches!(update, HarnessUpdate::Message { content } if content == text)
            ));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_rpc_relays_answers_none_notifications_and_literal_prompt() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        for answer in [true, false] {
            let mut body = START.to_owned();
            for method in ["confirm", "select", "input", "editor"] {
                body.push_str(&format!("printf '%s\\n' '{}'\nIFS= read -r answer\nprintf '{{\"type\":\"received_answer\",\"value\":%s}}\\n' \"$answer\"\n", ui(method)));
            }
            body.push_str("printf '%s\\n' '{\"type\":\"extension_ui_request\",\"id\":\"notice\",\"method\":\"notify\",\"message\":\"notification\"}'\n");
            body.push_str(FINISH);
            let (dir, extension, request) = fake_pi(&body);
            let handler = RecordingHandler::new(answer, false);
            let result = timeout(
                Duration::from_secs(10),
                run(&request, &extension, &handler, &CancellationToken::new()),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(result.message, "done");
            assert_eq!(result.session_id.as_deref(), Some("rpc-session"));
            assert_eq!(handler.questions.lock().unwrap().len(), 4);
            assert!(handler.updates.lock().unwrap().iter().any(|update| matches!(update, HarnessUpdate::Message { content } if content == "notification")));
            let prompt = result
                .events
                .iter()
                .find(|e| e["type"] == "received_prompt")
                .unwrap();
            assert_eq!(prompt["value"]["message"], request.prompt);
            let responses: Vec<_> = result
                .events
                .iter()
                .filter(|e| e["type"] == "received_answer")
                .collect();
            assert_eq!(responses.len(), 4);
            if answer {
                assert_eq!(responses[0]["value"]["confirmed"], true);
                assert_eq!(responses[1]["value"]["value"], "Review");
                assert_eq!(responses[2]["value"]["value"], "literal answer");
            } else {
                assert!(responses.iter().all(|e| e["value"]["cancelled"] == true));
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_rpc_drains_stdout_while_handler_pending_and_cancels_all_dialogs_before_abort() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let body = format!(
            r#"{START}
printf '%s\n' '{confirm}' '{select}'
i=0
while [ "$i" -lt 4000 ]; do
    printf '%s\n' '{{"type":"extension_ui_request","method":"notify","id":"notice","message":"pipe drain probe"}}'
    i=$((i + 1))
done
IFS= read -r first
IFS= read -r second
IFS= read -r abort
printf '%s\n' '{{"type":"extension_ui_request","method":"input","id":"late","title":"Late input"}}'
IFS= read -r late
printf '%s\n' "$first" "$second" "$abort" "$late" > cancel-replies.jsonl
printf '%s\n' '{{"type":"response","command":"abort","id":"abort-1","success":true}}'
"#,
            confirm = ui("confirm"),
            select = ui("select")
        );
        let (dir, extension, request) = fake_pi(&body);
        let handler = RecordingHandler::new(true, true);
        let cancel = CancellationToken::new();
        let (result, ()) = timeout(Duration::from_secs(10), async {
            tokio::join!(run(&request, &extension, &handler, &cancel), async {
                loop {
                    if handler.updates.lock().unwrap().len() == 4000 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                cancel.cancel();
            })
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert_eq!(handler.questions.lock().unwrap().len(), 2);
        let replies: Vec<Value> = fs::read_to_string(dir.join("cancel-replies.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 4);
        assert_eq!(
            replies[0],
            json!({"type":"extension_ui_response","id":"confirm","cancelled":true})
        );
        assert_eq!(
            replies[1],
            json!({"type":"extension_ui_response","id":"select","cancelled":true})
        );
        assert_eq!(replies[2], json!({"id":"abort-1","type":"abort"}));
        assert_eq!(
            replies[3],
            json!({"type":"extension_ui_response","id":"late","cancelled":true})
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_rpc_kills_unresponsive_process_after_abort_deadline() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let body = format!(
            r#"{START}
printf '%s\n' '{confirm}'
IFS= read -r cancelled
IFS= read -r abort
exec sleep 30
"#,
            confirm = ui("confirm")
        );
        let (dir, extension, request) = fake_pi(&body);
        let handler = RecordingHandler::new(true, true);
        let cancel = CancellationToken::new();
        let (result, ()) = timeout(Duration::from_secs(12), async {
            tokio::join!(run(&request, &extension, &handler, &cancel), async {
                handler.asked.notified().await;
                cancel.cancel();
            })
        })
        .await
        .unwrap();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cancelled (abort timed out)"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess fixture for detached descendant cleanup"]
    fn detached_sleep_fixture() {
        use std::os::unix::process::CommandExt;
        let path = std::env::var_os("PI_DETACHED_PID_FILE").expect("PI_DETACHED_PID_FILE");
        assert!(unsafe { libc::setsid() } >= 0);
        fs::write(path, std::process::id().to_string()).unwrap();
        panic!("{}", std::process::Command::new("sleep").arg("60").exec());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_rpc_terminates_detached_sleep_on_cancel_and_drop() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        for drop_run in [false, true] {
            let executable = std::env::current_exe().unwrap();
            let body = format!(
                r#"{START}
'{executable}' --exact local_harness::pi::rpc_tests::detached_sleep_fixture --ignored > /dev/null 2>&1 &
while [ ! -s detached.pid ]; do sleep 0.01; done
printf '%s\n' '{confirm}'
while IFS= read -r command; do :; done
wait
"#,
                executable = executable.display(),
                confirm = ui("confirm")
            );
            let (dir, extension, mut request) = fake_pi(&body);
            request.environment.insert(
                "PI_DETACHED_PID_FILE".into(),
                dir.join("detached.pid").into_os_string(),
            );
            let handler = RecordingHandler::new(true, true);
            let cancel = CancellationToken::new();
            let mut execution = Box::pin(run(&request, &extension, &handler, &cancel));
            tokio::select! {
                result = &mut execution => panic!("Pi unexpectedly completed: {result:?}"),
                _ = handler.asked.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("detached fixture did not start"),
            }
            let pid: libc::pid_t = fs::read_to_string(dir.join("detached.pid"))
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(unsafe { libc::getpgid(pid) }, pid);
            if drop_run {
                drop(execution);
            } else {
                cancel.cancel();
                let error = timeout(Duration::from_secs(8), execution)
                    .await
                    .unwrap()
                    .unwrap_err();
                assert!(error.to_string().contains("cancelled"));
            }
            timeout(Duration::from_secs(3), async {
                loop {
                    let status = std::process::Command::new("/bin/ps")
                        .args(["-p", &pid.to_string(), "-o", "stat="])
                        .output()
                        .unwrap();
                    let state = String::from_utf8_lossy(&status.stdout);
                    if !status.status.success() || state.trim().starts_with('Z') {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("detached sleep survived Pi cleanup");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_rpc_rejects_early_exit_malformed_json_prompt_rejection_and_nonzero_exit() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let cases = [
            ("IFS= read -r state\nIFS= read -r prompt\nprintf '%s' 'early failure' >&2\nexit 7".to_owned(), "before successful settle"),
            (format!("{START}\nprintf '%s\\n' 'not json'"), "invalid JSONL"),
            (format!("{START}\nprintf '%s\\n' '{{\"type\":\"response\",\"id\":\"prompt-1\",\"command\":\"prompt\",\"success\":false}}'"), "prompt was rejected"),
            (format!("{START}\n{FINISH}\nexit 9"), "exited unsuccessfully"),
        ];
        for (body, expected) in cases {
            let (dir, extension, request) = fake_pi(&body);
            let handler = RecordingHandler::new(true, false);
            let result = timeout(
                Duration::from_secs(10),
                run(&request, &extension, &handler, &CancellationToken::new()),
            )
            .await
            .unwrap();
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.contains(expected), "{error}");
            if expected == "before successful settle" {
                assert!(error.contains("early failure"));
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_rpc_rejects_handled_or_queued_prompt_without_waiting_for_settle() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        for disposition in ["handled", "queued", "missing"] {
            let response = json!({"id":"prompt-1","type":"response","command":"prompt","success":true,"data":{"disposition":disposition}});
            let body = format!(
                r#"
IFS= read -r state
IFS= read -r prompt
printf '%s\n' '{response}'
while IFS= read -r command; do :; done
"#
            );
            let (dir, extension, mut request) = fake_pi(&body);
            request.prompt = "/mcp".into();
            let handler = RecordingHandler::new(true, false);
            let error = timeout(
                Duration::from_secs(2),
                run(&request, &extension, &handler, &CancellationToken::new()),
            )
            .await
            .expect("handled prompt waited for an agent_settled that never comes")
            .unwrap_err();
            assert!(error
                .to_string()
                .contains(&format!("disposition: {disposition}")));
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_rpc_zero_timeout_relays_answer_and_stderr_tail_is_bounded() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let mut dialog = ui("confirm");
        dialog["timeout"] = json!(0);
        let body = format!(
            r#"{START}
printf '%s\n' '{dialog}'
IFS= read -r answer
printf '{{"type":"received_answer","value":%s}}\n' "$answer"
printf '%s\n' '{{"type":"extension_ui_request","method":"select","id":"select","title":"Pick","options":["Build","Review"]}}'
IFS= read -r answer
printf '{{"type":"received_answer","value":%s}}\n' "$answer"
{FINISH}
"#
        );
        let (dir, extension, request) = fake_pi(&body);
        let handler = RecordingHandler::new(true, false);
        let result = timeout(
            Duration::from_secs(10),
            run(&request, &extension, &handler, &CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        let responses: Vec<_> = result
            .events
            .iter()
            .filter(|e| e["type"] == "received_answer")
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["value"]["id"], "confirm");
        assert_eq!(responses[0]["value"]["confirmed"], true);
        assert_eq!(responses[1]["value"]["id"], "select");
        fs::remove_dir_all(dir).unwrap();

        let (dir, extension, request) = fake_pi("IFS= read -r state\nIFS= read -r prompt\ni=0\nwhile [ \"$i\" -lt 2000 ]; do printf '%s' 'stderr buffer probe ' >&2; i=$((i + 1)); done\nprintf '%s' 'TAIL-MARKER' >&2\nexit 1");
        let error = timeout(
            Duration::from_secs(10),
            run(&request, &extension, &handler, &CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("TAIL-MARKER"));
        assert!(error.len() < STDERR_LIMIT + 200);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn precancelled_rpc_does_not_spawn() {
        let _guard = FAKE_PROCESS_LOCK.lock().await;
        let (dir, extension, mut request) = fake_pi("exit 99");
        request.executable = Some(dir.join("nonexistent"));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = run(
            &request,
            &extension,
            &RecordingHandler::new(true, false),
            &cancel,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires real Pi and the localhost mock configured in PI_RPC_SMOKE_DIR"]
    async fn live_rpc_question_relay() {
        let dir = PathBuf::from(std::env::var_os("PI_RPC_SMOKE_DIR").expect("PI_RPC_SMOKE_DIR"));
        let extension = stage_extension(&dir).unwrap();
        let mut request = request(StepAccess::Write);
        request.cwd = dir.clone();
        request.prompt = "@alice literal RPC smoke.".into();
        request.model = Some("formless-smoke/mock".into());
        request.executable = Some(dir.join("pi-wrapper"));
        request.mcp_command = dir.join("fake-mcp");
        request.mcp_args = vec![];
        request.mcp_env_vars = vec![];
        request.environment.clear();
        request
            .environment
            .insert("PI_CODING_AGENT_DIR".into(), dir.clone().into_os_string());
        let handler = RecordingHandler::new(true, false);
        let result = timeout(
            Duration::from_secs(30),
            run(&request, &extension, &handler, &CancellationToken::new()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(handler.questions.lock().unwrap().len(), 3);
        assert_eq!(result.message, "Local UI smoke completed.");
        assert!(result.session_id.is_some());
        let tool = result
            .events
            .iter()
            .find(|e| e["type"] == "tool_execution_end" && e["toolName"] == "ui_probe")
            .unwrap();
        assert_eq!(tool["result"]["details"]["confirmed"], true);
        assert_eq!(tool["result"]["details"]["selected"], "Review");
        assert_eq!(tool["result"]["details"]["input"], "literal answer");
        println!("live pi::run session_id: {:?}", result.session_id);
        for event in &result.events {
            if matches!(
                event["type"].as_str(),
                Some("response" | "extension_ui_request" | "tool_execution_end" | "agent_settled")
            ) {
                println!("{event}");
            }
        }
        fs::remove_file(extension).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
    };

    use serde_json::{json, Value};
    use uuid::Uuid;

    use super::{
        command_spec, command_spec_with_extension, parse_output, stage_extension,
        BRIDGE_CUSTOM_TYPE, BRIDGE_MESSAGE, EXTENSION_NAME, EXTENSION_SOURCE, MCP_CONFIG_ENV,
        READ_ONLY_GUIDANCE, WRITE_GUIDANCE,
    };
    use crate::local_harness::{HarnessKind, HarnessRequest, StepAccess};

    pub(super) fn request(access: StepAccess) -> HarnessRequest {
        HarnessRequest {
            kind: HarnessKind::Pi,
            acp_agent: None,
            model: Some("configured-model".into()),
            cwd: std::env::temp_dir().join("formless repo"),
            prompt: "Make this change".into(),
            access,
            mcp_command: std::env::temp_dir().join("formless mcp"),
            mcp_args: vec!["serve".into(), "--literal $value".into()],
            mcp_env_vars: vec!["FORMLESS_TOPIC_CAPABILITY".into()],
            executable: Some(PathBuf::from("pi-test")),
            environment: BTreeMap::from([(
                OsString::from("FORMLESS_TOPIC_CAPABILITY"),
                OsString::from("secret-token"),
            )]),
        }
    }

    #[cfg(windows)]
    #[test]
    fn executable_lookup_windows_explicit_extensionless_path() {
        let dir = temp_dir();
        let extensionless = dir.join("pi");
        let exe = dir.join("pi.exe");
        let cmd = dir.join("pi.cmd");
        fs::write(&cmd, "@exit /b 0\r\n").unwrap();
        assert_eq!(super::find_pi_executable(&extensionless, None), None);
        assert_eq!(super::find_pi_executable(&cmd, None), Some(cmd));
        fs::write(&exe, "lookup fixture").unwrap();
        assert_eq!(
            super::find_pi_executable(&extensionless, None),
            Some(exe.clone())
        );
        assert_eq!(
            super::find_pi_executable(Path::new("pi"), Some(dir.as_os_str())),
            Some(exe)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("formless-pi-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }

    #[test]
    fn rejects_unfinished_work_after_a_settled_response() {
        for event_type in ["agent_start", "turn_start", "error"] {
            let mut events = success_events("earlier response");
            events.push(json!({"type": event_type}));
            assert!(parse_output(&jsonl(&events)).is_err());
        }
    }

    pub(super) fn staged_extension() -> (PathBuf, PathBuf) {
        let dir = temp_dir();
        let extension = stage_extension(&dir).unwrap();
        (dir, extension)
    }

    fn bridge_event() -> Value {
        json!({
            "type": "message_end",
            "message": {
                "role": "custom",
                "customType": BRIDGE_CUSTOM_TYPE,
                "content": BRIDGE_MESSAGE,
            }
        })
    }

    fn assistant_event(text: &str, stop_reason: &str) -> Value {
        json!({
            "type": "message_end",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "not returned"},
                    {"type": "text", "text": text},
                    {"type": "toolCall", "id": "call-1", "name": "hidden"},
                ],
                "stopReason": stop_reason,
            }
        })
    }

    fn jsonl(events: &[Value]) -> String {
        events
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn success_events(message: &str) -> Vec<Value> {
        vec![
            json!({"type": "session", "id": "session-123"}),
            bridge_event(),
            assistant_event(message, "stop"),
            json!({"type": "agent_end", "willRetry": false}),
            json!({"type": "agent_settled", "aborted": false}),
        ]
    }

    #[test]
    fn stage_extension_creates_private_non_overwriting_file() {
        let dir = temp_dir();
        let extension = stage_extension(&dir).unwrap();
        assert!(extension.is_absolute());
        assert_eq!(extension, fs::canonicalize(&extension).unwrap());
        assert_eq!(fs::read_to_string(&extension).unwrap(), EXTENSION_SOURCE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&extension).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(stage_extension(&dir).is_err());
        assert_eq!(fs::read_to_string(&extension).unwrap(), EXTENSION_SOURCE);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stage_extension_rejects_invalid_and_symlink_directories() {
        assert!(stage_extension(Path::new("relative")).is_err());
        let missing = std::env::temp_dir().join(format!("formless-pi-missing-{}", Uuid::new_v4()));
        assert!(stage_extension(&missing).is_err());
        let dir = temp_dir();
        let file = dir.join("not-a-directory");
        fs::write(&file, "file").unwrap();
        assert!(stage_extension(&file).is_err());
        fs::remove_dir_all(dir).unwrap();
        let dir = temp_dir();
        assert!(stage_extension(&dir.join(".")).is_err());
        fs::remove_dir_all(dir).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let dir = temp_dir();
            let alias = dir.with_extension("alias");
            symlink(&dir, &alias).unwrap();
            assert!(stage_extension(&alias).is_err());
            fs::remove_file(alias).unwrap();
            fs::remove_dir_all(dir).unwrap();

            let dir = temp_dir();
            let target = dir.join("kept-file");
            let extension = dir.join(EXTENSION_NAME);
            fs::write(&target, "keep").unwrap();
            symlink(&target, &extension).unwrap();
            assert!(stage_extension(&dir).is_err());
            assert_eq!(fs::read_to_string(target).unwrap(), "keep");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn command_spec_preserves_literal_arguments_model_cwd_and_environment() {
        let (dir, extension) = staged_extension();
        let mut request = request(StepAccess::Write);
        request.prompt = "--literal 'quoted' $HOME\nline next".into();
        request.model = Some("model with spaces".into());
        let spec = command_spec_with_extension(&request, &extension).unwrap();
        let args = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(spec.program, PathBuf::from("pi-test"));
        assert_eq!(spec.cwd, request.cwd);
        assert_eq!(
            args[0..8],
            [
                "--mode",
                "rpc",
                "--no-session",
                "--no-extensions",
                "-e",
                "builtin:mcp",
                "-e",
                extension.to_str().unwrap(),
            ]
        );
        assert_eq!(args[8..10], ["--model", "model with spaces"]);
        assert_eq!(args[10], "--append-system-prompt");
        assert_eq!(args[11], WRITE_GUIDANCE);
        assert!(!args.iter().any(|arg| arg == "--tools"));
        assert_eq!(args.len(), 12);
        assert!(!args.iter().any(|arg| arg == &request.prompt));
        assert_eq!(
            spec.environment
                .get(&OsString::from("FORMLESS_TOPIC_CAPABILITY")),
            Some(&OsString::from("secret-token"))
        );
        let config: Value = serde_json::from_str(
            spec.environment[&OsString::from(MCP_CONFIG_ENV)]
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(config["command"], request.mcp_command.to_str().unwrap());
        assert_eq!(config["args"], json!(["serve", "--literal $value"]));
        assert_eq!(config["cwd"], request.cwd.to_str().unwrap());
        assert_eq!(config["envVars"], json!(["FORMLESS_TOPIC_CAPABILITY"]));
        assert!(config["name"].as_str().unwrap().starts_with("formless_"));
        assert!(!spec.environment[&OsString::from(MCP_CONFIG_ENV)]
            .to_string_lossy()
            .contains("secret-token"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_only_command_limits_builtin_tools_and_keeps_its_mcp_server() {
        let (dir, extension) = staged_extension();
        let spec = command_spec_with_extension(&request(StepAccess::ReadOnly), &extension).unwrap();
        let args = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let access_prompt = args
            .iter()
            .position(|arg| arg == "--append-system-prompt")
            .unwrap();
        assert_eq!(args[access_prompt + 1], READ_ONLY_GUIDANCE);
        let tools = args.iter().position(|arg| arg == "--tools").unwrap();
        assert!(args[tools + 1].starts_with("read,grep,find,ls,mcp__formless_"));
        assert!(args[tools + 1].ends_with("__*"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn command_spec_omits_optional_model_and_uses_default_executable() {
        let (dir, extension) = staged_extension();
        let mut request = request(StepAccess::Write);
        request.model = None;
        request.executable = None;
        let spec = command_spec_with_extension(&request, &extension).unwrap();
        assert_eq!(spec.program, PathBuf::from("pi"));
        assert!(!spec.args.iter().any(|arg| arg == "--model"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rpc_command_never_puts_prompts_or_transport_flags_in_argv() {
        let (dir, extension) = staged_extension();
        for prompt in [
            "@alice please review the parser",
            "@existing-file.txt",
            "@@literal $HOME\nnext line",
            "\n@alice keep the leading newline",
            "Review @alice's parser",
        ] {
            let mut request = request(StepAccess::Write);
            request.prompt = prompt.into();
            let spec = command_spec_with_extension(&request, &extension).unwrap();
            let config: Value = serde_json::from_str(
                spec.environment[&OsString::from(MCP_CONFIG_ENV)]
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            assert!(config.get("restoreLeadingAt").is_none());
            assert!(!spec.args.iter().any(|arg| arg == prompt));
            assert!(!spec.args.iter().any(|arg| arg == "--"));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn command_spec_requires_explicit_staged_extension_and_rejects_symlinks() {
        let request = request(StepAccess::Write);
        assert!(command_spec(&request)
            .unwrap_err()
            .to_string()
            .contains("attempt-private staged extension"));
        assert!(crate::local_harness::command_spec(&request)
            .unwrap_err()
            .to_string()
            .contains("pi::run"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let (dir, extension) = staged_extension();
            let alias = dir.join("extension-alias.ts");
            symlink(&extension, &alias).unwrap();
            assert!(command_spec_with_extension(&request, &alias).is_err());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn command_spec_rejects_acp_agent_for_pi() {
        let (dir, extension) = staged_extension();
        let mut request = request(StepAccess::Write);
        request.acp_agent = Some("configured-agent".into());
        assert!(command_spec_with_extension(&request, &extension)
            .unwrap_err()
            .to_string()
            .contains("acp_agent"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parse_output_returns_final_assistant_text_and_retains_events() {
        let output = format!("{}\n", jsonl(&success_events("final answer")));
        let result = parse_output(&output).unwrap();
        assert_eq!(result.session_id.as_deref(), Some("session-123"));
        assert_eq!(result.message, "final answer");
        assert_eq!(result.events.len(), 5);
    }

    #[test]
    fn parse_output_accepts_crlf_and_unicode_line_separators_in_text() {
        let mut events = success_events("paragraph next section");
        let output = jsonl(&events).replace('\n', "\r\n");
        let result = parse_output(&output).unwrap();
        assert_eq!(result.message, "paragraph next section");
        events.clear();
    }

    #[test]
    fn parse_output_uses_later_success_after_recovered_error_and_tool_turn() {
        let output = jsonl(&[
            json!({"type": "session", "id": "session-retry"}),
            bridge_event(),
            assistant_event("transient error text", "error"),
            json!({"type": "agent_end", "willRetry": true}),
            assistant_event("tool request", "toolUse"),
            json!({"type": "tool_execution_end", "isError": false}),
            assistant_event("recovered final", "stop"),
            json!({"type": "agent_end", "willRetry": false}),
            json!({"type": "agent_settled", "aborted": false}),
        ]);
        assert_eq!(parse_output(&output).unwrap().message, "recovered final");
    }

    #[test]
    fn parse_output_rejects_empty_malformed_or_missing_bridge_and_final() {
        assert!(parse_output("").unwrap_err().to_string().contains("empty"));
        assert!(parse_output("not json\n").is_err());
        assert!(parse_output("\n").is_err());
        assert!(parse_output("[]").is_err());

        let missing_bridge = jsonl(&[
            json!({"type": "session", "id": "session-123"}),
            assistant_event("answer", "stop"),
            json!({"type": "agent_end", "willRetry": false}),
            json!({"type": "agent_settled", "aborted": false}),
        ]);
        assert!(parse_output(&missing_bridge)
            .unwrap_err()
            .to_string()
            .contains("bridge registration"));

        let missing_final = jsonl(&[
            json!({"type": "session", "id": "session-123"}),
            bridge_event(),
            json!({"type": "agent_end", "willRetry": false}),
            json!({"type": "agent_settled", "aborted": false}),
        ]);
        assert!(parse_output(&missing_final).is_err());
    }

    #[test]
    fn parse_output_rejects_aborted_error_and_partial_tool_turns() {
        for output in [
            jsonl(&[
                json!({"type": "session", "id": "s"}),
                bridge_event(),
                assistant_event("answer", "stop"),
                json!({"type": "agent_end", "willRetry": false}),
                json!({"type": "agent_settled", "aborted": true}),
            ]),
            jsonl(&[
                json!({"type": "session", "id": "s"}),
                bridge_event(),
                assistant_event("failure", "error"),
                json!({"type": "agent_end", "willRetry": false}),
                json!({"type": "agent_settled", "aborted": false}),
            ]),
            jsonl(&[
                json!({"type": "session", "id": "s"}),
                bridge_event(),
                assistant_event("incomplete tool turn", "toolUse"),
                json!({"type": "agent_end", "willRetry": false}),
                json!({"type": "agent_settled", "aborted": false}),
            ]),
            jsonl(&[
                json!({"type": "session", "id": "s"}),
                bridge_event(),
                assistant_event("aborted", "aborted"),
                json!({"type": "agent_end", "willRetry": false}),
                json!({"type": "agent_settled", "aborted": false}),
            ]),
            jsonl(&[
                json!({"type": "session", "id": "s"}),
                bridge_event(),
                assistant_event("", "stop"),
                json!({"type": "agent_end", "willRetry": false}),
                json!({"type": "agent_settled", "aborted": false}),
            ]),
            jsonl(&[
                json!({"type": "session", "id": "s"}),
                bridge_event(),
                assistant_event("truncated", "stop"),
            ]),
        ] {
            assert!(parse_output(&output).is_err());
        }
    }
}
