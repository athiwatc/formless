//! Safe, testable command construction for supported local coding CLIs.

pub mod acp;
pub mod acp_agents;
pub mod antigravity;
mod interaction;

pub use interaction::run;

use std::{collections::BTreeMap, ffi::OsString, fmt, path::PathBuf, process::Command};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub use formless_protocol::{HarnessKind, StepAccess};

pub mod muse;

#[path = "local_harness_probe.rs"]
mod readiness;
pub(crate) use readiness::harness_readiness;
pub use readiness::{
    acp_probe, blocking_probe, claude_probe, codex_probe, harness_availability, list_harnesses,
    ProbeContext,
};

pub struct HarnessPermissionRequest {
    pub title: String,
    pub kind: String,
    pub raw_input: Option<Value>,
    pub options: Vec<formless_protocol::HarnessPermissionOption>,
}

#[derive(Debug, Clone)]
pub enum HarnessUpdate {
    Message { content: String },
    Thought { content: String },
    ToolCall { name: String, input: Value },
    Plan { content: String },
}

pub type InteractionFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

pub trait InteractionHandler: Send + Sync {
    fn request_permission<'a>(
        &'a self,
        request: HarnessPermissionRequest,
        cancel: &'a tokio_util::sync::CancellationToken,
    ) -> InteractionFuture<'a, Option<String>>;
    fn ask_question<'a>(
        &'a self,
        questions: Vec<formless_protocol::Question>,
        cancel: &'a tokio_util::sync::CancellationToken,
    ) -> InteractionFuture<'a, Option<Vec<formless_protocol::QuestionAnswer>>>;
    fn on_update(&self, update: HarnessUpdate) -> Result<()>;
    fn finish(&self) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct HarnessRequest {
    pub kind: HarnessKind,
    pub acp_agent: Option<String>,
    pub model: Option<String>,
    pub cwd: PathBuf,
    pub prompt: String,
    pub access: StepAccess,
    pub mcp_command: PathBuf,
    pub mcp_args: Vec<String>,
    pub mcp_env_vars: Vec<String>,
    pub executable: Option<PathBuf>,
    pub executable_argv0: Option<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
}

impl fmt::Debug for HarnessRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessRequest")
            .field("kind", &self.kind)
            .field("acp_agent", &self.acp_agent)
            .field("model", &self.model)
            .field("cwd", &self.cwd)
            .field("prompt_bytes", &self.prompt.len())
            .field("access", &self.access)
            .field("mcp_command", &self.mcp_command)
            .field("mcp_args", &self.mcp_args)
            .field("mcp_env_vars", &self.mcp_env_vars)
            .field("executable", &self.executable)
            .field(
                "environment_keys",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("program", &self.program)
            .field("argument_count", &self.args.len())
            .field("cwd", &self.cwd)
            .field(
                "environment_keys",
                &self.environment.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl CommandSpec {
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).current_dir(&self.cwd);
        command.envs(&self.environment);
        command
    }
}

pub struct HarnessResult {
    pub session_id: Option<String>,
    pub message: String,
    pub events: Vec<Value>,
}

impl fmt::Debug for HarnessResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessResult")
            .field("session_id", &self.session_id)
            .field("message_bytes", &self.message.len())
            .field("event_count", &self.events.len())
            .finish()
    }
}

pub(crate) fn validate_acp_agent(kind: Option<HarnessKind>, agent: Option<&str>) -> Result<()> {
    if (kind == Some(HarnessKind::Acp)) != agent.is_some()
        || agent.is_some_and(|id| id.trim().is_empty())
    {
        bail!("acp_agent is required and must be nonempty exactly when the harness is acp");
    }
    if let Some(id) = agent {
        if acp_agents::spec(id).is_none() {
            bail!(
                "Unknown ACP agent `{id}`; available agents: {}",
                acp_agents::agent_ids().join(", ")
            );
        }
    }
    Ok(())
}

fn validate_request(request: &HarnessRequest) -> Result<()> {
    validate_acp_agent(Some(request.kind), request.acp_agent.as_deref())?;
    if request.kind == HarnessKind::Acp {
        bail!("ACP is interactive; use local_harness::run instead of command_spec");
    }
    if matches!(request.kind, HarnessKind::Pi | HarnessKind::OpenCode) {
        bail!(
            "{:?} adapter not integrated yet; use Claude or Codex for now",
            request.kind
        );
    }
    if request.prompt.trim().is_empty() {
        bail!("harness prompt must not be empty");
    }
    if !request.cwd.is_absolute() {
        bail!("harness working directory must be absolute");
    }
    if !request.mcp_command.is_absolute() {
        bail!("MCP server executable must be an absolute path");
    }
    if request
        .model
        .as_deref()
        .is_some_and(|model| model.trim().is_empty())
    {
        bail!("harness model must not be empty");
    }
    for name in &request.mcp_env_vars {
        let mut chars = name.chars();
        if !chars
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
            || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        {
            bail!("MCP environment variable names must be shell-independent identifiers");
        }
    }
    Ok(())
}

pub fn command_spec(request: &HarnessRequest) -> Result<CommandSpec> {
    if request.kind == HarnessKind::Antigravity {
        validate_acp_agent(Some(request.kind), request.acp_agent.as_deref())?;
        return antigravity::command_spec(request);
    }
    validate_request(request)?;
    if request.kind == HarnessKind::Muse {
        bail!("Muse Code requires the interactive harness runner");
    }
    if !matches!(request.kind, HarnessKind::Claude | HarnessKind::Codex) {
        bail!(
            "{:?} adapter not integrated yet; use Claude or Codex for now",
            request.kind
        );
    }
    let program = request
        .executable
        .clone()
        .unwrap_or_else(|| match request.kind {
            HarnessKind::Claude => PathBuf::from("claude"),
            HarnessKind::Codex => PathBuf::from("codex"),
            _ => unreachable!("unsupported adapters rejected above"),
        });
    let mut args = Vec::<OsString>::new();
    match request.kind {
        HarnessKind::Claude => {
            args.extend([
                "-p".into(),
                "--output-format".into(),
                "json".into(),
                "--strict-mcp-config".into(),
                "--mcp-config".into(),
                claude_mcp_config(request)?.into(),
            ]);
            if request.access == StepAccess::ReadOnly {
                args.extend([
                    "--permission-mode".into(),
                    "plan".into(),
                    "--tools".into(),
                    "Read,Glob,Grep".into(),
                ]);
            }
            if let Some(model) = &request.model {
                args.extend(["--model".into(), model.into()]);
            }
            args.push("--".into());
            args.push(request.prompt.as_str().into());
        }
        HarnessKind::Codex => {
            args.extend([
                "exec".into(),
                "--json".into(),
                "--sandbox".into(),
                match request.access {
                    StepAccess::Write => "workspace-write".into(),
                    StepAccess::ReadOnly => "read-only".into(),
                },
                "-c".into(),
                format!(
                    "mcp_servers.formless.command={}",
                    toml::Value::String(request.mcp_command.to_string_lossy().into_owned())
                )
                .into(),
                "-c".into(),
                format!(
                    "mcp_servers.formless.args={}",
                    toml::Value::Array(
                        request
                            .mcp_args
                            .iter()
                            .cloned()
                            .map(toml::Value::String)
                            .collect()
                    )
                )
                .into(),
                "-c".into(),
                "mcp_servers.formless.required=true".into(),
                "-c".into(),
                format!(
                    "mcp_servers.formless.env_vars={}",
                    toml::Value::Array(
                        request
                            .mcp_env_vars
                            .iter()
                            .cloned()
                            .map(toml::Value::String)
                            .collect()
                    )
                )
                .into(),
            ]);
            if let Some(model) = &request.model {
                args.extend(["--model".into(), model.into()]);
            }
            args.push("--".into());
            args.push(request.prompt.as_str().into());
        }
        _ => unreachable!("unsupported adapters rejected above"),
    }
    Ok(CommandSpec {
        program,
        args,
        cwd: request.cwd.clone(),
        environment: request.environment.clone(),
    })
}

fn claude_mcp_config(request: &HarnessRequest) -> Result<String> {
    let command = request
        .mcp_command
        .to_str()
        .context("MCP executable path must be valid UTF-8")?;
    Ok(json!({
        "mcpServers": {
            "formless": {
                "type": "stdio",
                "command": command,
                "args": request.mcp_args,
            }
        }
    })
    .to_string())
}

pub fn parse_output(kind: HarnessKind, stdout: &str) -> Result<HarnessResult> {
    match kind {
        HarnessKind::Claude => parse_claude(stdout),
        HarnessKind::Codex => parse_codex(stdout),
        HarnessKind::Antigravity => antigravity::parse_output(stdout),
        HarnessKind::Muse => bail!("Muse Code requires the interactive harness runner"),
        _ => bail!("{kind:?} adapter not integrated yet; use Claude or Codex for now"),
    }
}

fn parse_claude(stdout: &str) -> Result<HarnessResult> {
    let value: Value = serde_json::from_str(stdout).context("Claude CLI returned invalid JSON")?;
    if value["is_error"].as_bool() == Some(true)
        || value["subtype"]
            .as_str()
            .is_some_and(|subtype| subtype == "error" || subtype.starts_with("error_"))
    {
        bail!("Claude CLI returned an error result");
    }
    let message = value
        .get("result")
        .and_then(Value::as_str)
        .context("Claude CLI JSON did not contain a result string")?
        .to_owned();
    if message.trim().is_empty() {
        bail!("Claude CLI returned an empty result");
    }
    Ok(HarnessResult {
        session_id: value
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        message,
        events: vec![value],
    })
}

fn parse_codex(stdout: &str) -> Result<HarnessResult> {
    let mut events = Vec::new();
    let mut session_id = None;
    let mut candidate_message = None;
    let mut completed_message = None;
    let mut completed = false;
    for (index, line) in stdout
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
    {
        let event: Value = serde_json::from_str(line)
            .with_context(|| format!("Codex CLI returned invalid JSONL event {}", index + 1))?;
        match event.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                session_id = event
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Some("turn.started") => {
                candidate_message = None;
                completed_message = None;
                completed = false;
            }
            Some("item.completed") => {
                let item = &event["item"];
                if matches!(
                    item["type"].as_str(),
                    Some("agent_message" | "assistant_message")
                ) {
                    if let Some(text) = item["text"].as_str() {
                        candidate_message = Some(text.to_owned());
                    }
                }
            }
            Some("turn.completed") => {
                completed = true;
                completed_message = event
                    .get("last_agent_message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| candidate_message.take());
            }
            Some("turn.failed" | "error") => {
                bail!("Codex CLI reported a failed turn: {}", event);
            }
            _ => {}
        }
        events.push(event);
    }
    let message = completed_message
        .filter(|message| !message.trim().is_empty())
        .filter(|_| completed)
        .context("Codex CLI JSONL contained no successful completed turn and final message")?;
    Ok(HarnessResult {
        session_id,
        message,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(kind: HarnessKind, access: StepAccess) -> HarnessRequest {
        HarnessRequest {
            kind,
            acp_agent: None,
            model: Some("configured-model".into()),
            cwd: PathBuf::from("/tmp/repo"),
            prompt: "do the work".into(),
            access,
            mcp_command: PathBuf::from("/usr/bin/formless"),
            mcp_args: vec![
                "topics".into(),
                "mcp".into(),
                "--attempt".into(),
                "abc".into(),
            ],
            mcp_env_vars: vec!["FORMLESS_TOPIC_CAPABILITY".into()],
            executable: Some(PathBuf::from("/opt/test/harness")),
            executable_argv0: None,
            environment: BTreeMap::new(),
        }
    }

    #[test]
    fn claude_command_uses_print_json_and_only_the_explicit_mcp_config() {
        let spec = command_spec(&request(HarnessKind::Claude, StepAccess::Write)).unwrap();
        let args: Vec<_> = spec.args.iter().map(|arg| arg.to_string_lossy()).collect();
        assert_eq!(spec.program, PathBuf::from("/opt/test/harness"));
        assert_eq!(
            args[0..4],
            ["-p", "--output-format", "json", "--strict-mcp-config"]
        );
        assert_eq!(args[4], "--mcp-config");
        let config: Value = serde_json::from_str(&args[5]).unwrap();
        assert_eq!(
            config["mcpServers"]["formless"]["command"],
            "/usr/bin/formless"
        );
        assert_eq!(config["mcpServers"]["formless"]["args"][3], "abc");
        assert_eq!(args.last().unwrap(), "do the work");
    }

    #[test]
    fn codex_command_uses_scoped_config_and_role_sandbox() {
        let mut build_request = request(HarnessKind::Codex, StepAccess::Write);
        build_request.environment.insert(
            OsString::from("FORMLESS_TOPIC_CAPABILITY"),
            OsString::from("must-not-appear-in-argv"),
        );
        let build_spec = command_spec(&build_request).unwrap();
        assert_eq!(
            build_spec
                .environment
                .get(&OsString::from("FORMLESS_TOPIC_CAPABILITY")),
            Some(&OsString::from("must-not-appear-in-argv"))
        );
        let build: Vec<_> = build_spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect();
        let review = command_spec(&request(HarnessKind::Codex, StepAccess::ReadOnly)).unwrap();
        let review: Vec<_> = review
            .args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect();
        assert_eq!(
            &build[..4],
            ["exec", "--json", "--sandbox", "workspace-write"]
        );
        assert_eq!(&review[..4], ["exec", "--json", "--sandbox", "read-only"]);
        assert!(build
            .iter()
            .any(|arg| arg.starts_with("mcp_servers.formless.command=")));
        assert!(build
            .iter()
            .any(|arg| arg.starts_with("mcp_servers.formless.args=[")));
        assert!(build
            .iter()
            .any(|arg| arg == "mcp_servers.formless.required=true"));
        assert!(build
            .iter()
            .any(|arg| arg == "mcp_servers.formless.env_vars=[\"FORMLESS_TOPIC_CAPABILITY\"]"));
        assert!(build.iter().any(|arg| arg == "configured-model"));
        assert!(!build
            .iter()
            .any(|arg| arg.contains("must-not-appear-in-argv")));
        assert!(!build.iter().any(|arg| arg.contains("danger-full-access")));
    }

    #[test]
    fn harness_results_parse_success_and_reject_empty_or_malformed_responses() {
        let claude = parse_output(
            HarnessKind::Claude,
            r#"{"result":"ready","session_id":"claude-1"}"#,
        )
        .unwrap();
        assert_eq!(claude.message, "ready");
        assert_eq!(claude.session_id.as_deref(), Some("claude-1"));

        let codex = parse_output(
            HarnessKind::Codex,
            "{\"type\":\"thread.started\",\"thread_id\":\"codex-1\"}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"reviewed\"}}\n{\"type\":\"turn.completed\"}\n",
        )
        .unwrap();
        assert_eq!(codex.message, "reviewed");
        assert_eq!(codex.session_id.as_deref(), Some("codex-1"));
        assert!(parse_output(HarnessKind::Claude, "{}").is_err());
        assert!(parse_output(HarnessKind::Claude, r#"{"result":" "}"#).is_err());
        assert!(parse_output(
            HarnessKind::Claude,
            r#"{"is_error":true,"result":"denied"}"#
        )
        .is_err());
        assert!(parse_output(
            HarnessKind::Claude,
            r#"{"subtype":"error_max_turns","result":"truncated"}"#
        )
        .is_err());
        assert!(parse_output(HarnessKind::Codex, "not json\n").is_err());
        assert!(parse_output(HarnessKind::Codex, "\n").is_err());
        assert!(parse_output(
            HarnessKind::Codex,
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"interim\"}}\n"
        )
        .is_err());
        assert!(parse_output(
            HarnessKind::Codex,
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"partial\"}}\n{\"type\":\"turn.failed\"}\n"
        )
        .is_err());
        assert!(parse_output(
            HarnessKind::Codex,
            "{\"type\":\"turn.started\"}\n{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"old\"}}\n{\"type\":\"turn.completed\"}\n{\"type\":\"turn.started\"}\n"
        )
        .is_err());
    }

    #[test]
    fn command_builder_rejects_non_absolute_paths_and_blank_prompts() {
        let mut request = request(HarnessKind::Claude, StepAccess::Write);
        request.prompt.clear();
        assert!(command_spec(&request).is_err());
        request.prompt = "ok".into();
        request.mcp_command = PathBuf::from("formless");
        assert!(command_spec(&request).is_err());
    }

    #[test]
    fn leading_dash_prompts_are_after_an_end_of_options_separator() {
        for kind in [HarnessKind::Claude, HarnessKind::Codex] {
            let mut request = request(kind, StepAccess::Write);
            request.prompt = "- implement X".into();
            let spec = command_spec(&request).unwrap();
            let args: Vec<_> = spec.args.iter().map(|arg| arg.to_string_lossy()).collect();
            assert_eq!(args[args.len() - 2], "--");
            assert_eq!(args.last().unwrap(), "- implement X");
        }
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;
    #[test]
    fn topic_access_restrictions_and_unintegrated_adapters() {
        for kind in [
            HarnessKind::Claude,
            HarnessKind::Codex,
            HarnessKind::Pi,
            HarnessKind::OpenCode,
            HarnessKind::Acp,
            HarnessKind::Muse,
        ] {
            let request = HarnessRequest {
                kind,
                acp_agent: (kind == HarnessKind::Acp).then(|| "gemini".into()),
                model: None,
                cwd: std::env::current_dir().unwrap(),
                prompt: "inspect".into(),
                access: StepAccess::ReadOnly,
                mcp_command: std::env::current_exe().unwrap(),
                mcp_args: vec![],
                mcp_env_vars: vec![],
                executable: None,
                executable_argv0: None,
                environment: BTreeMap::new(),
            };
            if matches!(kind, HarnessKind::Claude | HarnessKind::Codex) {
                let args = command_spec(&request).unwrap().args;
                if kind == HarnessKind::Claude {
                    assert!(args.windows(2).any(|p| p == ["--permission-mode", "plan"]));
                    assert!(args.windows(2).any(|p| p == ["--tools", "Read,Glob,Grep"]));
                } else {
                    assert!(args.windows(2).any(|p| p == ["--sandbox", "read-only"]));
                }
            } else if kind == HarnessKind::Acp {
                assert!(command_spec(&request)
                    .unwrap_err()
                    .to_string()
                    .contains("local_harness::run"));
            } else if kind == HarnessKind::Antigravity {
                assert!(command_spec(&request)
                    .unwrap_err()
                    .to_string()
                    .contains("antigravity::run"));
            } else {
                let expected = if kind == HarnessKind::Muse {
                    "requires the interactive harness runner"
                } else {
                    "adapter not integrated yet"
                };
                assert!(command_spec(&request)
                    .unwrap_err()
                    .to_string()
                    .contains(expected));
                assert!(parse_output(kind, "{}")
                    .unwrap_err()
                    .to_string()
                    .contains(expected));
            }
        }
        let antigravity = HarnessRequest {
            kind: HarnessKind::Antigravity,
            acp_agent: None,
            model: None,
            cwd: std::env::current_dir().unwrap(),
            prompt: "prompt".into(),
            access: StepAccess::ReadOnly,
            mcp_command: PathBuf::from("formless"),
            mcp_args: vec![],
            mcp_env_vars: vec![],
            executable: None,
            executable_argv0: None,
            environment: BTreeMap::new(),
        };
        assert!(command_spec(&antigravity)
            .unwrap_err()
            .to_string()
            .contains("local_harness::antigravity::run"));
    }
}
