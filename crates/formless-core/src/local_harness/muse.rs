//! Meta Muse Code's Muse Session Protocol (MSP) over stdio.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsString,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use formless_protocol::{
    HarnessPermissionOption, HarnessPermissionOptionKind, Question, QuestionAnswer, QuestionOption,
};
use formless_runtime::process_group::SupervisedChild;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    validate_request, CommandSpec, HarnessKind, HarnessPermissionRequest, HarnessRequest,
    HarnessResult, HarnessUpdate, InteractionHandler, StepAccess,
};

const START_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FRAME: u64 = 4 * 1024 * 1024;
const TRUST_ONCE: &str = "trust_once";

pub async fn run(
    request: &HarnessRequest,
    handler: &dyn InteractionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    anyhow::ensure!(
        request.kind == HarnessKind::Muse,
        "expected a Muse Code request"
    );
    validate_request(request)?;
    tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("Muse Code run cancelled"),
        result = async {
            let workspace = request.cwd.to_str().context("Muse Code requires a UTF-8 workspace path")?;
            let trust = HarnessPermissionRequest {
                title: "Trust this Muse Code workspace for one run?".into(),
                kind: "trust_workspace".into(),
                raw_input: Some(json!({
                    "path": workspace,
                    "detail": "Muse Code may load this project's rules, skills, and MCP servers.",
                })),
                options: vec![
                    HarnessPermissionOption {
                        option_id: TRUST_ONCE.into(),
                        name: "Trust workspace for this run".into(),
                        kind: HarnessPermissionOptionKind::AllowOnce,
                    },
                    HarnessPermissionOption {
                        option_id: "reject".into(),
                        name: "Do not run Muse Code".into(),
                        kind: HarnessPermissionOptionKind::RejectOnce,
                    },
                ],
            };
            anyhow::ensure!(
                handler.request_permission(trust, cancel).await.as_deref() == Some(TRUST_ONCE),
                "Muse Code requires one-run trust for the attempt workspace"
            );
            let read_only = request.access == StepAccess::ReadOnly;
            let launch = launch_spec(request, read_only, true);
            run_session(request, launch, read_only, handler, cancel).await
        } => result,
    }
}

struct Launch {
    command: CommandSpec,
    server: String,
}

fn launch_spec(request: &HarnessRequest, read_only: bool, trust_workspace: bool) -> Launch {
    let server = format!("formless_{}", Uuid::new_v4().simple());
    let selector = format!("mcp__{server}__*");
    let mut tools = vec!["read_file", "request_user_input", &selector];
    let mut args: Vec<OsString> = [
        "serve",
        "--no-session-log",
        "--sandbox-network",
        "restricted",
        "--contributor-mode",
        "off",
        "-c",
        "approval_judge=\"off\"",
        "-c",
        "hooks.enabled=false",
        "-c",
        "agents.execution_capacity=1",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    if trust_workspace {
        args.push("--trust-workspace".into());
    }
    if read_only {
        args.extend(["--disable-write".into(), "--disable-shell".into()]);
    } else {
        tools.extend(["write_file", "edit_file", "shell"]);
    }
    // Unknown selections stop the turn before the model sees its input, even
    // if Muse silently withholds the bridge.
    args.extend(["-c".into(), format!("run.toolset={}", json!(tools)).into()]);
    if let Some(model) = &request.model {
        args.push(format!("--model={model}").into());
    }
    let mut environment = request.environment.clone();
    for name in &request.mcp_env_vars {
        environment.remove(std::ffi::OsStr::new(name));
    }
    environment.insert("MUSE_NO_AUTO_UPDATE".into(), "1".into());
    Launch {
        command: CommandSpec {
            program: request
                .executable
                .clone()
                .unwrap_or_else(|| PathBuf::from("muse")),
            args,
            cwd: request.cwd.clone(),
            environment,
        },
        server,
    }
}

fn session_config(request: &HarnessRequest, server: &str) -> Result<Value> {
    let command = request
        .mcp_command
        .to_str()
        .context("MCP executable path must be valid UTF-8")?;
    let mut env = BTreeMap::new();
    for name in &request.mcp_env_vars {
        let key = std::ffi::OsStr::new(name);
        let value = request
            .environment
            .get(key)
            .cloned()
            .or_else(|| std::env::var_os(key))
            .and_then(|value| value.into_string().ok())
            .context("Muse Code bridge needs a valid UTF-8 value for each declared variable")?;
        env.insert(name, value);
    }
    Ok(json!({ "mcpServers": { (server): {
        "transport": "stdio",
        "command": command,
        "args": request.mcp_args,
        "env": env,
        "mode": "required",
    }}}))
}

async fn run_session(
    request: &HarnessRequest,
    launch: Launch,
    read_only: bool,
    handler: &dyn InteractionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    let cwd = request
        .cwd
        .to_str()
        .context("Muse Code requires a UTF-8 workspace path")?;
    let config = session_config(request, &launch.server)?;
    let mut command = launch.command.command();
    for name in &request.mcp_env_vars {
        command.env_remove(name);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = SupervisedChild::spawn(command.into()).map_err(|_| {
        anyhow::anyhow!("Could not start Muse Code; install the `muse` CLI and check PATH")
    })?;
    let (input, output, _) = child.take_pipes();
    let mut peer = Msp {
        input: input.context("Muse Code has no stdin pipe")?,
        output: BufReader::new(output.context("Muse Code has no stdout pipe")?),
        next_id: 0,
    };
    let session = peer.start_session(cwd, config, read_only).await?;
    let (turn, early_events) = peer.start_turn(&session, &request.prompt).await?;
    let message = peer
        .run_turn(&session, &turn, early_events, read_only, handler, cancel)
        .await?;
    drop(peer); // EOF is the official stdio host shutdown.
    tokio::time::timeout(Duration::from_secs(5), child.exited())
        .await
        .context("Muse Code did not stop after the turn")??;
    require_success(child.finish().await.context("Could not reap Muse Code")?)?;
    Ok(HarnessResult {
        session_id: Some(session),
        message,
        events: Vec::new(),
    })
}

fn require_success(status: std::process::ExitStatus) -> Result<()> {
    anyhow::ensure!(status.success(), "Muse Code exited unsuccessfully");
    Ok(())
}

struct Msp<R, W> {
    input: W,
    output: R,
    next_id: u64,
}

impl<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin> Msp<R, W> {
    async fn send(&mut self, value: Value) -> Result<()> {
        let mut frame = serde_json::to_vec(&value).context("could not encode Muse Code request")?;
        frame.push(b'\n');
        self.input
            .write_all(&frame)
            .await
            .context("could not send Muse Code request")?;
        self.input
            .flush()
            .await
            .context("could not flush Muse Code request")
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        Ok(id)
    }

    async fn receive(&mut self) -> Result<Value> {
        let mut frame = Vec::new();
        if (&mut self.output)
            .take(MAX_FRAME + 1)
            .read_until(b'\n', &mut frame)
            .await
            .context("could not read Muse Code response")?
            == 0
        {
            bail!("Muse Code disconnected before completing the run");
        }
        if frame.len() as u64 > MAX_FRAME || frame.last() != Some(&b'\n') {
            bail!("Muse Code sent an incomplete or oversized MSP frame");
        }
        let value: Value = serde_json::from_slice(&frame)
            .map_err(|_| anyhow::anyhow!("Muse Code sent malformed MSP JSON"))?;
        if value["jsonrpc"] != "2.0" {
            bail!("Muse Code sent an unsupported MSP envelope");
        }
        Ok(value)
    }

    async fn startup_request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.await_admission(method, params, None).await
    }

    async fn await_admission(
        &mut self,
        method: &str,
        params: Value,
        mut early_events: Option<&mut VecDeque<Value>>,
    ) -> Result<Value> {
        tokio::time::timeout(START_TIMEOUT, async {
            let id = self.send_request(method, params).await?;
            loop {
                let event = self.receive().await?;
                if event.get("method").is_none() {
                    if event["id"].as_u64() != Some(id) {
                        bail!("Muse Code sent an unexpected response during startup");
                    }
                    return response_result(event, method);
                }
                if let Some(events) = &mut early_events {
                    anyhow::ensure!(
                        events.len() < 8,
                        "Muse Code sent too many events before accepting the turn"
                    );
                    events.push_back(event);
                } else if event.get("id").is_some() {
                    bail!("Muse Code sent an unexpected request during startup");
                }
            }
        })
        .await
        .with_context(|| format!("Muse Code timed out during {method}"))?
    }

    async fn start_session(&mut self, cwd: &str, config: Value, read_only: bool) -> Result<String> {
        let initialized = self.startup_request("initialize", json!({
            "clientInfo": {"name": "formless", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"requestedCapabilities": ["sessionMcp"], "userInputDialogs": true}
        })).await?;
        if initialized["schema"]["version"] != 1
            || initialized["sessionDurability"] != "ephemeral"
            || !initialized["grantedCapabilities"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|cap| cap == "sessionMcp"))
        {
            bail!("Muse Code does not support ephemeral MSP v1 sessions with an MCP bridge");
        }
        if !read_only && initialized["sandboxAvailability"] != "available" {
            bail!("Muse Code cannot enforce its workspace sandbox");
        }
        self.send(json!({"jsonrpc": "2.0", "method": "initialized"}))
            .await?;
        let started = self
            .startup_request(
                "session/start",
                json!({
                    "commandId": Uuid::now_v7().to_string(),
                    "workspaceRoot": cwd,
                    "approvalMode": "promptUnmatched",
                    "config": config,
                }),
            )
            .await?;
        let session = &started["session"];
        if session["path"] != "" {
            bail!("Muse Code did not confirm a memory-only session");
        }
        if session["approvalMode"]["mode"] != "promptUnmatched" {
            bail!("Muse Code did not confirm the requested approval policy");
        }
        Ok(identifier(session, "sessionId")?.to_owned())
    }

    async fn start_turn(
        &mut self,
        session: &str,
        prompt: &str,
    ) -> Result<(String, VecDeque<Value>)> {
        let command_id = Uuid::now_v7().to_string();
        let mut early_events = VecDeque::new();
        let started = self
            .await_admission(
                "turn/start",
                json!({
                    "commandId": command_id,
                    "sessionId": session,
                    "input": [{"type": "text", "text": prompt}],
                }),
                Some(&mut early_events),
            )
            .await?;
        if started["status"] != "accepted"
            || started["disposition"] != "started"
            || started["commandId"] != command_id
        {
            bail!("Muse Code did not start the requested turn");
        }
        Ok((identifier(&started, "turnId")?.to_owned(), early_events))
    }

    async fn run_turn(
        &mut self,
        session: &str,
        turn: &str,
        mut early_events: VecDeque<Value>,
        read_only: bool,
        handler: &dyn InteractionHandler,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let mut output = TurnOutput::default();
        let mut pending = BTreeMap::new();
        loop {
            let event = match early_events.pop_front() {
                Some(event) => event,
                None => self.receive().await?,
            };
            if event.get("method").is_none() {
                let method = event["id"]
                    .as_u64()
                    .and_then(|id| pending.remove(&id))
                    .context("Muse Code sent an unexpected MSP response")?;
                if response_result(event, method)?["status"] != "accepted" {
                    bail!("Muse Code did not accept {method}");
                }
                continue;
            }
            let method = event["method"]
                .as_str()
                .context("Muse Code sent an invalid MSP method")?;
            let params = &event["params"];
            if let Some(id) = event.get("id") {
                if params["sessionId"] != session
                    || params["turnId"] != turn
                    || !(id.is_string() || id.is_number())
                {
                    bail!("Muse Code sent a request for an unexpected turn");
                }
                let (command, request) = match method {
                    "approval/request" => {
                        let approval: Approval = decode(params, "approval request")?;
                        let mut seen = BTreeSet::new();
                        if approval.current_requirement_id.approval_id != approval.approval_id
                            || !approval.available_choices.iter().all(|choice| {
                                !choice.choice_id.is_empty() && seen.insert(&choice.choice_id)
                            })
                        {
                            bail!("Muse Code sent inconsistent approval choices");
                        }
                        self.send(json!({"jsonrpc": "2.0", "id": id, "result": {}}))
                            .await?;
                        let selected = handler
                            .request_permission(approval.for_formless(read_only), cancel)
                            .await;
                        let choice = approval
                            .available_choices
                            .iter()
                            .find(|choice| {
                                selected.as_deref() == Some(&choice.choice_id)
                                    && approval.can_offer(choice, read_only)
                            })
                            .or_else(|| {
                                approval.available_choices.iter().find(|choice| {
                                    matches!(choice.decision.as_str(), "denied" | "abort")
                                        && approval.can_offer(choice, read_only)
                                })
                            })
                            .context("Muse Code did not provide a one-time denial option")?;
                        (
                            "approval/decide",
                            approval.decide(session, &choice.choice_id),
                        )
                    }
                    "userInput/request" => {
                        let question: Questions = decode(params, "question request")?;
                        question.validate()?;
                        self.send(json!({"jsonrpc": "2.0", "id": id, "result": {}}))
                            .await?;
                        // Taking responsibility for the dialog stops its unattended
                        // timeout while the user is deciding in Formless.
                        self.send(
                            json!({"jsonrpc": "2.0", "method": "userInput/engaged", "params": {
                                "sessionId": session, "userInputId": question.user_input_id
                            }}),
                        )
                        .await?;
                        handler
                            .ask_question(question.for_formless(), cancel)
                            .await
                            .and_then(|answers| question.answer(session, &answers))
                            .map(|params| ("userInput/answer", params))
                            .unwrap_or_else(|| ("userInput/cancel", question.cancel(session)))
                    }
                    _ => bail!("Muse Code sent an unsupported server request"),
                };
                let id = self.send_request(command, request).await?;
                pending.insert(id, command);
                continue;
            }
            if params["sessionId"] != session {
                continue;
            }
            if method == "turn/completed" && params["turnId"] == turn {
                if !pending.is_empty() {
                    bail!("Muse Code completed the turn with unconfirmed decisions");
                }
                return output.complete(params);
            }
            if let Some(message) = output.message(method, params, turn) {
                handler.on_update(HarnessUpdate::Message {
                    content: message.to_owned(),
                })?;
            }
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(value: &Value, kind: &str) -> Result<T> {
    serde_json::from_value(value.clone())
        .map_err(|_| anyhow::anyhow!("Muse Code sent an invalid {kind}"))
}

fn identifier<'a>(object: &'a Value, key: &str) -> Result<&'a str> {
    object[key]
        .as_str()
        .filter(|id| Uuid::parse_str(id).is_ok())
        .with_context(|| format!("Muse Code did not send a valid {key}"))
}

fn response_result(mut response: Value, method: &str) -> Result<Value> {
    if let Some(error) = response.get("error") {
        // Harness diagnostics can include user input or local environment.
        let code = error["code"].as_i64().unwrap_or_default();
        bail!("Muse Code rejected {method} (MSP error {code})");
    }
    response
        .get_mut("result")
        .map(Value::take)
        .with_context(|| format!("Muse Code returned no result for {method}"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Approval {
    approval_id: String,
    current_requirement_id: Requirement,
    subject: Value,
    tool_name: String,
    protected_write: bool,
    available_choices: Vec<Choice>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Requirement {
    approval_id: String,
    source_index: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Choice {
    choice_id: String,
    decision: String,
    scope: String,
    label: String,
}

impl Approval {
    fn for_formless(&self, read_only: bool) -> HarnessPermissionRequest {
        HarnessPermissionRequest {
            title: format!("Muse Code requests permission: {}", self.tool_name),
            kind: self.subject["kind"].as_str().unwrap_or("tool").to_owned(),
            raw_input: Some(json!({
                "subject": self.subject,
                "requirement": self.current_requirement_id.source_index,
                "protected_write": self.protected_write,
            })),
            options: self
                .available_choices
                .iter()
                .filter(|choice| self.can_offer(choice, read_only))
                .map(|choice| HarnessPermissionOption {
                    option_id: choice.choice_id.clone(),
                    name: choice.label.clone(),
                    kind: if choice.decision == "approved" {
                        HarnessPermissionOptionKind::AllowOnce
                    } else {
                        HarnessPermissionOptionKind::RejectOnce
                    },
                })
                .collect(),
        }
    }

    fn can_offer(&self, choice: &Choice, read_only: bool) -> bool {
        if choice.scope != "once" {
            return false;
        }
        if matches!(choice.decision.as_str(), "denied" | "abort") {
            return true;
        }
        if choice.decision != "approved" || self.protected_write {
            return false;
        }
        match self.subject["kind"].as_str() {
            Some("shell") => !read_only && self.tool_name == "shell",
            Some("fileAccess") => {
                let permitted_access = match self.subject["access"].as_str() {
                    Some("read") => true,
                    Some("write") => !read_only,
                    _ => false,
                };
                permitted_access
                    && self.subject["path"].as_str().is_some_and(|path| {
                        let mut components = Path::new(path).components();
                        matches!(components.next(), Some(Component::Normal(_)))
                            && components.all(|part| matches!(part, Component::Normal(_)))
                    })
            }
            _ => false,
        }
    }

    fn decide(&self, session: &str, choice: &str) -> Value {
        json!({
            "commandId": Uuid::now_v7().to_string(),
            "sessionId": session,
            "approvalId": self.approval_id,
            "requirementId": self.current_requirement_id,
            "choiceId": choice,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Questions {
    user_input_id: String,
    questions: Vec<MspQuestion>,
}

#[derive(Deserialize)]
struct MspQuestion {
    id: String,
    header: String,
    question: String,
    options: Vec<QuestionOption>,
    selection: QuestionSelection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionSelection {
    mode: String,
    min_selections: Option<usize>,
    max_selections: Option<usize>,
}

impl Questions {
    fn for_formless(&self) -> Vec<Question> {
        self.questions
            .iter()
            .map(|question| Question {
                question: question.question.clone(),
                header: Some(question.header.clone()),
                options: question.options.clone(),
                multi_select: question.selection.mode == "multiple",
            })
            .collect()
    }

    fn validate(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        anyhow::ensure!(
            (1..=3).contains(&self.questions.len())
                && self.questions.iter().all(|question| {
                    let mut labels = BTreeSet::new();
                    !question.id.is_empty()
                        && ids.insert(&question.id)
                        && matches!(question.selection.mode.as_str(), "single" | "multiple")
                        && (2..=3).contains(&question.options.len())
                        && question.options.iter().all(|option| {
                            !option.label.trim().is_empty() && labels.insert(&option.label)
                        })
                }),
            "Muse Code sent an unsupported question request"
        );
        Ok(())
    }

    fn cancel(&self, session: &str) -> Value {
        json!({"commandId": Uuid::now_v7().to_string(), "sessionId": session, "userInputId": self.user_input_id})
    }

    fn answer(&self, session: &str, replies: &[QuestionAnswer]) -> Option<Value> {
        if self.questions.len() != replies.len() {
            return None;
        }
        let mut answers = Vec::new();
        for (question, reply) in self.questions.iter().zip(replies) {
            let chosen = &reply.selected;
            let text = reply.text.as_deref().filter(|text| !text.trim().is_empty());
            if text.is_some_and(|text| text.chars().count() > 500) {
                return None;
            }
            if let (true, Some(text)) = (chosen.is_empty(), text) {
                answers.push(json!({"questionId": question.id, "freeText": text}));
                continue;
            }
            let distinct: BTreeSet<_> = chosen.iter().collect();
            if distinct.len() != chosen.len()
                || !chosen
                    .iter()
                    .all(|label| question.options.iter().any(|option| option.label == *label))
            {
                return None;
            }
            let mut answer = match question.selection.mode.as_str() {
                "single" if chosen.len() == 1 => {
                    json!({"questionId": question.id, "selectedLabel": chosen[0]})
                }
                "multiple"
                    if chosen.len() >= question.selection.min_selections.unwrap_or(1)
                        && chosen.len()
                            <= question
                                .selection
                                .max_selections
                                .unwrap_or(question.options.len()) =>
                {
                    json!({"questionId": question.id, "selectedLabels": chosen})
                }
                _ => return None,
            };
            if let Some(note) = text {
                answer["note"] = json!(note);
            }
            answers.push(answer);
        }
        let mut params = self.cancel(session);
        params["answers"] = json!(answers);
        Some(params)
    }
}

#[derive(Default)]
struct TurnOutput {
    live_messages: BTreeMap<String, bool>,
    final_answer: Option<String>,
}

impl TurnOutput {
    fn message<'a>(&mut self, method: &str, params: &'a Value, turn: &str) -> Option<&'a str> {
        let item = &params["item"];
        match method {
            "item/started" if item["kind"] == "agentMessage" && item["turnId"] == turn => {
                self.live_messages
                    .insert(item["itemId"].as_str()?.to_owned(), false);
                None
            }
            "item/delta" if params.get("field").is_none_or(|field| field == "text") => {
                let streamed = self.live_messages.get_mut(params["itemId"].as_str()?)?;
                let text = params["delta"].as_str()?;
                *streamed |= !text.is_empty();
                Some(text)
            }
            "item/completed" if item["kind"] == "agentMessage" && item["turnId"] == turn => {
                let had_stream = self
                    .live_messages
                    .remove(item["itemId"].as_str()?)
                    .unwrap_or(false);
                let text = item["text"].as_str()?;
                if item["messagePhase"] == "finalAnswer" && !text.trim().is_empty() {
                    self.final_answer = Some(text.to_owned());
                }
                // Some providers emit the completed item without starting and
                // streaming it first.
                (!had_stream).then_some(text)
            }
            _ => None,
        }
    }

    fn complete(self, params: &Value) -> Result<String> {
        if params["terminal"] != "completed" {
            if params["error"]["kind"] == "configError"
                && params["error"]["message"].as_str().is_some_and(|message| {
                    message.contains("Required MCP server")
                        || message.contains("unknown tool `mcp__formless_")
                })
            {
                bail!("Muse Code did not load the required Formless bridge; check workspace trust and bridge startup");
            }
            bail!("Muse Code did not complete the turn; check sign-in and the Formless bridge");
        }
        self.final_answer
            .context("Muse Code did not return a final answer")
    }
}

#[cfg(test)]
mod tests {
    use super::super::InteractionFuture;
    use super::*;

    fn request() -> HarnessRequest {
        let cwd = std::env::temp_dir();
        HarnessRequest {
            kind: HarnessKind::Muse,
            acp_agent: None,
            model: None,
            cwd: cwd.clone(),
            prompt: "-fixture prompt not for argv".into(),
            access: StepAccess::ReadOnly,
            mcp_command: cwd.join("fixture bridge"),
            mcp_args: vec!["mcp".into()],
            mcp_env_vars: vec!["FORMLESS_FIXTURE_TOKEN".into()],
            executable: None,
            environment: BTreeMap::from([(
                "FORMLESS_FIXTURE_TOKEN".into(),
                "fixture secret".into(),
            )]),
        }
    }

    #[test]
    fn launch_scopes_tools_and_keeps_prompt_and_capability_off_argv() {
        let mut req = request();
        req.model = Some("-fixture-model".into());
        let read = launch_spec(&req, true, false);
        let write = launch_spec(&req, false, true);
        let args = &read.command.args;
        assert_eq!(read.command.program, PathBuf::from("muse"));
        assert_ne!(read.server, write.server);
        assert!(args.iter().any(|arg| arg == "--model=-fixture-model"));
        assert!(args.iter().any(|arg| arg == "--no-session-log"));
        assert!(!args.iter().any(|arg| arg == "--trust-workspace"));
        assert!(write
            .command
            .args
            .iter()
            .any(|arg| arg == "--trust-workspace"));
        assert!(args.iter().any(|arg| arg == "approval_judge=\"off\""));
        assert!(args.iter().any(|arg| arg == "--disable-shell"));
        assert!(args.iter().any(|arg| arg == "--disable-write"));
        assert!(!write
            .command
            .args
            .iter()
            .any(|arg| arg == "--disable-shell" || arg == "--disable-write"));
        for launch in [&read, &write] {
            let args = &launch.command.args;
            let tools = args
                .iter()
                .find_map(|arg| arg.to_str()?.strip_prefix("run.toolset="))
                .unwrap();
            assert!(tools.contains(&format!("mcp__{}__*", launch.server)));
            assert!(tools.contains("request_user_input"));
            assert_eq!(tools.contains("\"shell\""), launch.server == write.server);
            assert!(args
                .iter()
                .all(|arg| !arg.to_string_lossy().contains("fixture secret")
                    && !arg.to_string_lossy().contains(&req.prompt)));
            assert!(!launch
                .command
                .environment
                .contains_key(std::ffi::OsStr::new("FORMLESS_FIXTURE_TOKEN")));
            let cfg = session_config(&req, &launch.server).unwrap();
            let server = &cfg["mcpServers"][&launch.server];
            assert_eq!(server["mode"], "required");
            assert_eq!(server["env"]["FORMLESS_FIXTURE_TOKEN"], "fixture secret");
        }
    }

    fn approval() -> Value {
        json!({
            "sessionId": "session",
            "turnId": "turn",
            "approvalId": "approval",
            "currentRequirementId": {"approvalId": "approval", "sourceIndex": 1},
            "subject": {"kind": "shell", "command": "printf fixture"},
            "toolName": "shell",
            "protectedWrite": false,
            "availableChoices": [
                {"choiceId": "fixture_opaque_choice", "decision": "approved", "scope": "once", "label": "Allow once"},
                {"choiceId": "always", "decision": "approvedPolicyAmendment", "scope": "localPersistent", "label": "Always allow"},
                {"choiceId": "reject", "decision": "denied", "scope": "once", "label": "Reject"},
            ],
        })
    }

    fn questions() -> Value {
        json!({
            "sessionId": "session",
            "turnId": "turn",
            "userInputId": "questions",
            "questions": [{
                "id": "format",
                "header": "Format",
                "question": "Which format?",
                "options": [{"label": "CSV"}, {"label": "PDF"}],
                "selection": {"mode": "single"}
            }],
        })
    }

    #[derive(Default)]
    struct Hooks {
        stream: std::sync::Mutex<Vec<String>>,
        trust: Option<&'static str>,
        trust_path: std::sync::Mutex<Option<Value>>,
    }

    impl InteractionHandler for Hooks {
        fn request_permission<'a>(
            &'a self,
            request: HarnessPermissionRequest,
            _cancel: &'a CancellationToken,
        ) -> InteractionFuture<'a, Option<String>> {
            if request.kind == "trust_workspace" {
                assert_eq!(
                    request
                        .options
                        .iter()
                        .map(|option| option.kind)
                        .collect::<Vec<_>>(),
                    [
                        HarnessPermissionOptionKind::AllowOnce,
                        HarnessPermissionOptionKind::RejectOnce
                    ]
                );
                *self.trust_path.lock().unwrap() =
                    request.raw_input.map(|input| input["path"].clone());
                return Box::pin(async { self.trust.map(str::to_owned) });
            }
            assert_eq!(request.kind, "shell");
            assert_eq!(
                request
                    .options
                    .iter()
                    .map(|option| option.option_id.as_str())
                    .collect::<Vec<_>>(),
                ["fixture_opaque_choice", "reject"]
            );
            Box::pin(async { Some("always".into()) })
        }

        fn ask_question<'a>(
            &'a self,
            questions: Vec<Question>,
            _cancel: &'a CancellationToken,
        ) -> InteractionFuture<'a, Option<Vec<QuestionAnswer>>> {
            assert_eq!(questions[0].question, "Which format?");
            assert_eq!(questions[0].header.as_deref(), Some("Format"));
            Box::pin(async {
                Some(vec![QuestionAnswer {
                    selected: vec!["CSV".into()],
                    text: None,
                }])
            })
        }

        fn on_update(&self, update: HarnessUpdate) -> Result<()> {
            if let HarnessUpdate::Message { content } = update {
                self.stream.lock().unwrap().push(content);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn workspace_trust_requires_the_explicit_allow_once_id_before_launch() {
        let mut req = request();
        req.executable = Some(req.cwd.join(format!("missing-{}", Uuid::new_v4())));
        let cancel = CancellationToken::new();
        for trust in [None, Some("reject"), Some("always"), Some(TRUST_ONCE)] {
            let hooks = Hooks {
                trust,
                ..Hooks::default()
            };
            let error = run(&req, &hooks, &cancel).await.unwrap_err().to_string();
            let expected = if trust == Some(TRUST_ONCE) {
                "Could not start Muse Code"
            } else {
                "requires one-run trust"
            };
            assert!(error.contains(expected));
            assert_eq!(*hooks.trust_path.lock().unwrap(), Some(json!(req.cwd)));
        }
        cancel.cancel();
        let handler = Hooks::default();
        assert!(run(&req, &handler, &cancel)
            .await
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        assert!(handler.trust_path.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn msp_maps_dialogs_denies_unoffered_approvals_and_streams_assistant_text() {
        let (client, server) = tokio::io::duplex(4096);
        let (client_input, client_output) = tokio::io::split(client);
        let (server_input, server_output) = tokio::io::split(server);
        let mut client = Msp {
            input: client_output,
            output: BufReader::new(client_input),
            next_id: 0,
        };
        let mut server = Msp {
            input: server_output,
            output: BufReader::new(server_input),
            next_id: 0,
        };
        let hooks = Hooks::default();
        let cancel = CancellationToken::new();
        let script = async {
            server.send(json!({"jsonrpc": "2.0", "id": "approval-rpc", "method": "approval/request", "params": approval()})).await.unwrap();
            let receipt = server.receive().await.unwrap();
            assert_eq!(receipt["id"], "approval-rpc");
            assert_eq!(receipt["result"], json!({}));
            let decide = server.receive().await.unwrap();
            assert_eq!(decide["method"], "approval/decide");
            assert_eq!(decide["params"]["choiceId"], "reject");
            assert_eq!(decide["params"]["requirementId"]["sourceIndex"], 1);
            server
                .send(
                    json!({"jsonrpc": "2.0", "id": decide["id"], "result": {"status": "accepted"}}),
                )
                .await
                .unwrap();

            server.send(json!({"jsonrpc": "2.0", "id": 9, "method": "userInput/request", "params": questions()})).await.unwrap();
            assert_eq!(server.receive().await.unwrap()["id"], 9);
            assert_eq!(
                server.receive().await.unwrap()["method"],
                "userInput/engaged"
            );
            let answer = server.receive().await.unwrap();
            assert_eq!(answer["method"], "userInput/answer");
            assert_eq!(
                answer["params"]["answers"],
                json!([{"questionId": "format", "selectedLabel": "CSV"}])
            );
            server
                .send(
                    json!({"jsonrpc": "2.0", "id": answer["id"], "result": {"status": "accepted"}}),
                )
                .await
                .unwrap();

            let item = json!({"itemId": "message", "kind": "agentMessage", "turnId": "turn", "messagePhase": "finalAnswer", "text": "Fixture done"});
            for (method, params) in [
                (
                    "item/started",
                    json!({"sessionId": "session", "item": item}),
                ),
                (
                    "item/delta",
                    json!({"sessionId": "session", "itemId": "untracked", "delta": "reasoning not for UI"}),
                ),
                (
                    "item/delta",
                    json!({"sessionId": "session", "itemId": "message", "field": "text", "delta": "Fixture done"}),
                ),
                (
                    "item/completed",
                    json!({"sessionId": "session", "item": item}),
                ),
                (
                    "turn/completed",
                    json!({"sessionId": "session", "turnId": "turn", "terminal": "completed"}),
                ),
            ] {
                server
                    .send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
                    .await
                    .unwrap();
            }
        };
        let (result, ()) = tokio::time::timeout(START_TIMEOUT, async {
            tokio::join!(
                client.run_turn("session", "turn", VecDeque::new(), false, &hooks, &cancel),
                script
            )
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap(), "Fixture done");
        assert_eq!(*hooks.stream.lock().unwrap(), ["Fixture done"]);
    }

    #[tokio::test]
    async fn turn_admission_keeps_early_events_and_uses_the_returned_id() {
        let (client, server) = tokio::io::duplex(4096);
        let (client_input, client_output) = tokio::io::split(client);
        let (server_input, server_output) = tokio::io::split(server);
        let mut client = Msp {
            input: client_output,
            output: BufReader::new(client_input),
            next_id: 0,
        };
        let mut server = Msp {
            input: server_output,
            output: BufReader::new(server_input),
            next_id: 0,
        };
        let turn = Uuid::now_v7().to_string();
        let fixture = async {
            let request = server.receive().await.unwrap();
            assert_eq!(request["method"], "turn/start");
            assert_ne!(request["params"]["commandId"], turn);
            server
                .send(
                    json!({"jsonrpc": "2.0", "method": "turn/started", "params": {"turnId": turn}}),
                )
                .await
                .unwrap();
            server.send(json!({"jsonrpc": "2.0", "id": request["id"], "result": {
                "status": "accepted", "disposition": "started", "commandId": request["params"]["commandId"], "turnId": turn
            }})).await.unwrap();
        };
        let (admission, ()) = tokio::join!(client.start_turn("session", "prompt"), fixture);
        let (accepted_turn, events) = admission.unwrap();
        assert_eq!(accepted_turn, turn);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["params"]["turnId"], turn);
    }

    #[tokio::test]
    async fn msp_rejects_invalid_incomplete_and_oversized_frames_without_echoing_them() {
        for (bytes, expected) in [
            (b"\"not for errors\" {\n".to_vec(), "malformed MSP JSON"),
            (
                b"{\"jsonrpc\":\"2.0\"}".to_vec(),
                "incomplete or oversized MSP frame",
            ),
            (
                vec![b'a'; MAX_FRAME as usize + 1],
                "incomplete or oversized MSP frame",
            ),
        ] {
            let mut client = Msp {
                input: tokio::io::sink(),
                output: bytes.as_slice(),
                next_id: 0,
            };
            let error = client.receive().await.unwrap_err().to_string();
            assert!(error.contains(expected));
            assert!(!error.contains("not for errors"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_successful_msp_terminal_still_requires_a_successful_exit() {
        use std::os::unix::process::ExitStatusExt;
        assert!(require_success(std::process::ExitStatus::from_raw(0)).is_ok());
        assert!(require_success(std::process::ExitStatus::from_raw(1 << 8)).is_err());
    }

    #[test]
    fn unsupported_access_and_invalid_replies_fail_closed() {
        let approval: Approval = decode(&approval(), "fixture").unwrap();
        assert!(!approval.can_offer(&approval.available_choices[0], true));
        assert!(!approval.can_offer(&approval.available_choices[1], false));
        let question: Questions = decode(&questions(), "fixture").unwrap();
        assert!(question
            .answer(
                "session",
                &[QuestionAnswer {
                    selected: vec!["unoffered".into()],
                    text: None
                }]
            )
            .is_none());
        assert_eq!(
            question
                .answer(
                    "session",
                    &[QuestionAnswer {
                        selected: Vec::new(),
                        text: Some("Markdown".into())
                    }]
                )
                .unwrap()["answers"],
            json!([{"questionId": "format", "freeText": "Markdown"}])
        );
        for mode in ["failed", "cancelled", "completed"] {
            assert!(TurnOutput::default()
                .complete(&json!({"terminal": mode}))
                .is_err());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires the installed Muse Code CLI and python3; uses Echo, not Meta credentials"]
    async fn live_muse_msp_bridge() {
        use std::{fs, os::unix::fs::PermissionsExt};

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let root = Scratch(std::env::temp_dir().join(format!("muse-msp-{}", Uuid::new_v4())));
        fs::create_dir(&root.0).unwrap();
        let workspace = root.0.join("workspace");
        fs::create_dir(&workspace).unwrap();
        let bridge = root.0.join("fixture");
        let marker = root.0.join("bridge-listed-tools");
        fs::write(&bridge, r#"#!/usr/bin/env python3
import json, os, sys
assert os.environ["FORMLESS_FIXTURE_TOKEN"] == "fixture secret"
for line in sys.stdin:
    msg = json.loads(line)
    if msg["method"] == "initialize":
        result = {"protocolVersion": msg["params"]["protocolVersion"],
                  "serverInfo": {"name": "fixture", "version": "1"}, "capabilities": {"tools": {}}}
    elif msg["method"] == "tools/list":
        with open(sys.argv[1], "w") as out:
            out.write("ready")
        result = {"tools": [{"name": "fixture", "description": "fixture", "inputSchema": {"type": "object"}}]}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
"#).unwrap();
        fs::set_permissions(&bridge, fs::Permissions::from_mode(0o700)).unwrap();
        let mut req = request();
        req.cwd = workspace;
        req.mcp_command = bridge;
        req.mcp_args = vec![marker.to_str().unwrap().into()];
        req.environment.extend([
            ("HOME".into(), root.0.as_os_str().to_owned()),
            (
                "XDG_CONFIG_HOME".into(),
                root.0.join("config").into_os_string(),
            ),
            ("XDG_DATA_HOME".into(), root.0.join("data").into_os_string()),
        ]);
        let echo = |req: &HarnessRequest, read_only, trust_workspace| {
            let mut launch = launch_spec(req, read_only, trust_workspace);
            launch
                .command
                .args
                .extend(["--provider".into(), "echo".into()]);
            launch
        };
        let handler = Hooks::default();
        let cancel = CancellationToken::new();
        for read_only in [true, false] {
            let result = run_session(
                &req,
                echo(&req, read_only, true),
                read_only,
                &handler,
                &cancel,
            )
            .await
            .unwrap();
            assert_eq!(result.message, format!("echo: {}", req.prompt));
            assert_eq!(fs::read_to_string(&marker).unwrap(), "ready");
            fs::remove_file(&marker).unwrap();
        }
        assert!(!root.0.join("config/muse/trust.json").exists());

        let missing_trust = run_session(&req, echo(&req, true, false), true, &handler, &cancel)
            .await
            .unwrap_err();
        assert!(missing_trust
            .to_string()
            .contains("required Formless bridge"));
        assert!(!marker.exists());

        req.mcp_command = root.0.join("missing");
        let missing_bridge = run_session(&req, echo(&req, true, true), true, &handler, &cancel)
            .await
            .unwrap_err();
        assert!(missing_bridge
            .to_string()
            .contains("required Formless bridge"));
    }
}
