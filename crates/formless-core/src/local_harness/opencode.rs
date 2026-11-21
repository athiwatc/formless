//! V2-only adapter for OpenCode's local CLI.
//!
//! Verified against OpenCode v2.3.2.
//!
//! Uses the V2 `opencode run --standalone --format json` interface and
//! `OPENCODE_CONFIG_CONTENT` so per-run MCP and agent settings stay in the child
//! process. V1 configuration and CLI flags are unsupported. This adapter starts
//! new runs only; it does not resume, import files, or make provider calls itself.
//! User/project OpenCode configuration is still read, never written; per-run
//! settings come from `OPENCODE_CONFIG_CONTENT`. It requires a V2 CLI with those flags.
//!
//! The command and configuration follow the official [CLI], [MCP], [agent],
//! [permission], and [configuration schema] documentation. JSON event fields
//! follow the upstream [non-interactive run renderer] and MCP tool permission
//! naming implementation. Fixture and fake-process tests do not cover a live
//! provider call or authenticated MCP server.
//!
//! [CLI]: https://opencode.ai/docs/cli/
//! [MCP]: https://opencode.ai/docs/mcp-servers/
//! [agent]: https://opencode.ai/docs/agents/
//! [permission]: https://opencode.ai/docs/permissions/
//! [configuration schema]: https://opencode.ai/config.json
//! [non-interactive run renderer]: https://github.com/anomalyco/opencode/blob/dev/packages/cli/src/run/noninteractive.ts
//! [MCP tool permission naming implementation]: https://github.com/anomalyco/opencode/blob/dev/packages/core/src/tool/mcp.ts

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use super::{CommandSpec, HarnessKind, HarnessRequest, HarnessResult, HarnessRole};

const CONFIG_ENV: &str = "OPENCODE_CONFIG_CONTENT";
const REVIEW_AGENT: &str = "formless-review";
const TERMINAL_SUCCESS_REASON: &str = "stop";

pub fn command_spec(request: &HarnessRequest) -> Result<CommandSpec> {
    validate_request(request)?;

    let mcp_command = request
        .mcp_command
        .to_str()
        .context("MCP executable path must be valid UTF-8")?;
    let mut command = Vec::with_capacity(request.mcp_args.len() + 1);
    command.push(mcp_command.to_owned());
    command.extend(request.mcp_args.iter().cloned());

    let mut mcp_environment = Map::new();
    for name in &request.mcp_env_vars {
        mcp_environment.insert(name.clone(), Value::String(format!("{{env:{name}}}")));
    }

    let build_permissions = json!([
        permission("execute", "allow"),
        permission("formless_*", "allow"),
        permission("subagent", "deny"),
    ]);
    let review_permissions = json!([
        permission("execute", "allow"),
        permission("formless_*", "allow"),
        permission("edit", "deny"),
        permission("shell", "deny"),
        permission("subagent", "deny"),
    ]);
    let review_agent = json!({
        "description": "Read-only Formless review agent",
        "mode": "primary",
        "system": "Review the requested changes for correctness, regressions, and security issues. Do not modify files. Report actionable findings with file and line references; if there are no findings, say so.",
        "permissions": review_permissions,
    });
    let configuration = json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": {
            "servers": {
                "formless": {
                    "type": "local",
                    "command": command,
                    "cwd": ".",
                    "environment": mcp_environment,
                }
            }
        },
        "agents": {
            "build": {
                "mode": "primary",
                "permissions": build_permissions,
            },
            REVIEW_AGENT: review_agent,
        }
    });
    let config_content = serde_json::to_string(&configuration)
        .context("could not serialize OpenCode run configuration")?;

    let agent = match request.role {
        HarnessRole::Build => "build",
        HarnessRole::Review => REVIEW_AGENT,
    };
    let mut args = vec![
        "run".into(),
        "--standalone".into(),
        "--format".into(),
        "json".into(),
        "--agent".into(),
        agent.into(),
    ];
    if let Some(model) = &request.model {
        args.extend(["--model".into(), model.into()]);
    }
    args.push("--".into());
    args.push(request.prompt.as_str().into());

    let mut environment = request.environment.clone();
    environment.insert(CONFIG_ENV.into(), config_content.into());

    Ok(CommandSpec {
        program: request
            .executable
            .clone()
            .unwrap_or_else(|| PathBuf::from("opencode")),
        args,
        cwd: request.cwd.clone(),
        environment,
    })
}

fn permission(action: &str, effect: &str) -> Value {
    json!({
        "action": action,
        "resource": "*",
        "effect": effect,
    })
}

fn validate_request(request: &HarnessRequest) -> Result<()> {
    if request.kind != HarnessKind::OpenCode {
        bail!("OpenCode adapter requires the OpenCode harness kind");
    }
    if request.prompt.trim().is_empty() {
        bail!("harness prompt must not be empty");
    }
    if request.prompt.contains('\0') {
        bail!("harness prompt must not contain NUL bytes");
    }
    if !request.cwd.is_absolute() {
        bail!("harness working directory must be absolute");
    }
    if !request.mcp_command.is_absolute() {
        bail!("MCP server executable must be an absolute path");
    }
    let mcp_command = request
        .mcp_command
        .to_str()
        .context("MCP executable path must be valid UTF-8")?;
    if mcp_command.contains('\0') {
        bail!("MCP executable path must not contain NUL bytes");
    }
    if request.mcp_args.iter().any(|arg| arg.contains('\0')) {
        bail!("MCP server arguments must not contain NUL bytes");
    }
    if let Some(model) = request.model.as_deref() {
        if model.trim().is_empty() {
            bail!("harness model must not be empty");
        }
        if model.contains('\0') || !valid_model(model) {
            bail!("OpenCode model must use the provider/model[#variant] format");
        }
    }
    for name in &request.mcp_env_vars {
        if !valid_environment_name(name) {
            bail!("MCP environment variable names must be shell-independent identifiers");
        }
    }
    if request
        .executable
        .as_ref()
        .is_some_and(|executable| executable.as_os_str().is_empty())
    {
        bail!("OpenCode executable path must not be empty");
    }
    Ok(())
}

fn valid_model(model: &str) -> bool {
    let Some((provider, name_and_variant)) = model.split_once('/') else {
        return false;
    };
    if provider.is_empty() || provider.contains('#') || model.trim() != model {
        return false;
    }
    let (name, variant) = name_and_variant
        .split_once('#')
        .map_or((name_and_variant, None), |(name, variant)| {
            (name, Some(variant))
        });
    !name.is_empty()
        && !name.contains('\0')
        && variant.is_none_or(|variant| !variant.is_empty() && !variant.contains('#'))
}

fn valid_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

pub fn parse_output(stdout: &str) -> Result<HarnessResult> {
    let mut events = Vec::new();
    let mut session_id: Option<String> = None;
    let mut message = String::new();
    let mut finish_reason: Option<String> = None;

    for (index, line) in stdout
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
    {
        let event: Value = serde_json::from_str(line)
            .map_err(|_| anyhow!("OpenCode CLI returned malformed JSONL event {}", index + 1))?;
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("OpenCode CLI event is missing its type"))?;

        if event_type == "error" {
            bail!("OpenCode CLI returned an error event");
        }

        if let Some(value) = event.get("sessionID") {
            let id = value
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or_else(|| anyhow!("OpenCode CLI event has an invalid session ID"))?;
            if session_id.as_deref().is_some_and(|current| current != id) {
                bail!("OpenCode CLI returned conflicting session IDs");
            }
            session_id.get_or_insert_with(|| id.to_owned());
        }

        match event_type {
            "step_start" => finish_reason = None,
            "text" => {
                let part = event
                    .get("part")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow!("OpenCode CLI returned a malformed text event"))?;
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    bail!("OpenCode CLI returned a malformed text event");
                }
                let text = part
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("OpenCode CLI returned a malformed text event"))?;
                message.push_str(text);
            }
            "step_finish" => {
                let part = event
                    .get("part")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        anyhow!("OpenCode CLI returned a malformed step-finish event")
                    })?;
                if part.get("type").and_then(Value::as_str) != Some("step-finish") {
                    bail!("OpenCode CLI returned a malformed step-finish event");
                }
                let reason = part.get("reason").and_then(Value::as_str).ok_or_else(|| {
                    anyhow!("OpenCode CLI returned a malformed step-finish event")
                })?;
                finish_reason = Some(reason.to_owned());
            }
            _ => {}
        }

        events.push(event);
    }

    if events.is_empty() {
        bail!("OpenCode CLI returned no events");
    }
    if message.trim().is_empty() {
        bail!("OpenCode CLI returned no text output");
    }
    if finish_reason.as_deref() != Some(TERMINAL_SUCCESS_REASON) {
        bail!("OpenCode CLI did not report a successful terminal step");
    }

    Ok(HarnessResult {
        session_id,
        message,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, ffi::OsString};

    const COMPLETE_OUTPUT: &str = concat!(
        "{\"type\":\"step_start\",\"sessionID\":\"session-1\",\"part\":{\"type\":\"step-start\"}}\n",
        "{\"type\":\"text\",\"sessionID\":\"session-1\",\"part\":{\"type\":\"text\",\"text\":\"hello \"}}\n",
        "{\"type\":\"reasoning\",\"sessionID\":\"session-1\",\"part\":{\"type\":\"reasoning\",\"text\":\"not user output\"}}\n",
        "{\"type\":\"text\",\"sessionID\":\"session-1\",\"part\":{\"type\":\"text\",\"text\":\"world\"}}\n",
        "{\"type\":\"step_finish\",\"sessionID\":\"session-1\",\"part\":{\"type\":\"step-finish\",\"reason\":\"stop\"}}\n",
    );

    fn request(role: HarnessRole) -> HarnessRequest {
        HarnessRequest {
            kind: HarnessKind::OpenCode,
            model: Some("provider/model#variant".into()),
            cwd: PathBuf::from("/tmp/repo"),
            prompt: "--prompt-as-one-argument".into(),
            role,
            mcp_command: PathBuf::from("/usr/bin/formless"),
            mcp_args: vec!["mcp".into(), "serve".into()],
            mcp_env_vars: vec!["FORMLESS_TOPIC_CAPABILITY".into()],
            executable: Some(PathBuf::from("/opt/test/opencode")),
            environment: BTreeMap::new(),
        }
    }

    fn config(spec: &CommandSpec) -> Value {
        serde_json::from_str(
            spec.environment
                .get(&OsString::from(CONFIG_ENV))
                .unwrap()
                .to_str()
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn command_spec_builds_isolated_v2_build_and_review_runs() {
        for (role, agent) in [
            (HarnessRole::Build, "build"),
            (HarnessRole::Review, REVIEW_AGENT),
        ] {
            let spec = super::super::command_spec(&request(role)).unwrap();
            let args: Vec<_> = spec.args.iter().map(|arg| arg.to_string_lossy()).collect();
            assert_eq!(spec.program, PathBuf::from("/opt/test/opencode"));
            assert_eq!(
                &args[..6],
                [
                    "run",
                    "--standalone",
                    "--format",
                    "json",
                    "--agent",
                    agent,
                ]
            );
            assert_eq!(
                &args[6..],
                [
                    "--model",
                    "provider/model#variant",
                    "--",
                    "--prompt-as-one-argument",
                ]
            );

            let configuration = config(&spec);
            let server = &configuration["mcp"]["servers"]["formless"];
            assert_eq!(server["type"], "local");
            assert_eq!(server["command"][0], "/usr/bin/formless");
            assert_eq!(server["command"][1], "mcp");
            assert_eq!(server["command"][2], "serve");
            assert_eq!(
                server["environment"]["FORMLESS_TOPIC_CAPABILITY"],
                "{env:FORMLESS_TOPIC_CAPABILITY}"
            );
            let build_permissions = configuration["agents"]["build"]["permissions"]
                .as_array()
                .unwrap();
            assert_eq!(
                build_permissions
                    .iter()
                    .find(|rule| rule["action"] == "subagent")
                    .unwrap()["effect"],
                "deny"
            );
            assert_eq!(
                build_permissions
                    .iter()
                    .find(|rule| rule["action"] == "formless_*")
                    .unwrap()["effect"],
                "allow"
            );
            let review = &configuration["agents"][REVIEW_AGENT];
            assert_eq!(review["mode"], "primary");
            assert_eq!(
                review["permissions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|rule| rule["action"] == "execute")
                    .unwrap()["effect"],
                "allow"
            );
            for action in ["edit", "shell", "subagent"] {
                assert_eq!(
                    review["permissions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|rule| rule["action"] == action)
                        .unwrap()["effect"],
                    "deny"
                );
            }
            assert_eq!(
                review["permissions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|rule| rule["action"] == "formless_*")
                    .unwrap()["effect"],
                "allow"
            );
        }
    }

    #[test]
    fn command_spec_keeps_secrets_out_of_config_and_uses_only_allowlisted_references() {
        let mut request = request(HarnessRole::Review);
        request.environment.insert(
            OsString::from("FORMLESS_TOPIC_CAPABILITY"),
            OsString::from("unit-test-secret"),
        );
        request.environment.insert(
            OsString::from("OTHER_SECRET"),
            OsString::from("not-allowlisted"),
        );
        request.environment.insert(
            OsString::from(CONFIG_ENV),
            OsString::from("previous config"),
        );

        let spec = command_spec(&request).unwrap();
        let config_content = spec.environment[&OsString::from(CONFIG_ENV)]
            .to_str()
            .unwrap();
        assert!(!config_content.contains("unit-test-secret"));
        assert!(!config_content.contains("not-allowlisted"));
        assert_eq!(
            spec.environment
                .get(&OsString::from("FORMLESS_TOPIC_CAPABILITY")),
            Some(&OsString::from("unit-test-secret"))
        );
        let configuration = config(&spec);
        let environment = configuration["mcp"]["servers"]["formless"]["environment"]
            .as_object()
            .unwrap();
        assert_eq!(environment.len(), 1);
        assert_eq!(
            environment["FORMLESS_TOPIC_CAPABILITY"],
            "{env:FORMLESS_TOPIC_CAPABILITY}"
        );
    }

    #[test]
    fn command_spec_validates_prompt_paths_and_model_format() {
        let mut request = request(HarnessRole::Build);
        request.prompt = " \n".into();
        assert!(command_spec(&request).is_err());
        request.prompt = "prompt".into();
        request.cwd = PathBuf::from("relative");
        assert!(command_spec(&request).is_err());
        request.cwd = PathBuf::from("/tmp/repo");
        request.mcp_command = PathBuf::from("formless");
        assert!(command_spec(&request).is_err());
        request.mcp_command = PathBuf::from("/usr/bin/formless");
        request.model = Some("model-without-provider".into());
        assert!(command_spec(&request).is_err());
        request.model = None;
        let spec = command_spec(&request).unwrap();
        let args: Vec<_> = spec.args.iter().map(|arg| arg.to_string_lossy()).collect();
        assert_eq!(&args[6..], ["--", "prompt"]);
        request.executable = None;
        assert_eq!(
            command_spec(&request).unwrap().program,
            PathBuf::from("opencode")
        );
    }

    #[test]
    fn parse_output_accepts_v2_3_2_tool_call_steps() {
        let output = r#"{"type":"tool_use","timestamp":1795274606996,"sessionID":"ses-smoke","part":{"partID":"prt-tool","sessionID":"ses-smoke","messageID":"msg-tool","type":"tool","id":"call-1","tool":"execute","state":{"status":"completed","input":{"code":"return await tools.formless.topic_read()"},"output":"null","title":"execute","metadata":{"metadata":{"toolCalls":[{"tool":"formless.topic_read","status":"completed"}],"truncated":false},"content":[{"type":"text","text":"null"}],"providerCall":{"executed":false},"providerResult":{"executed":false},"rawInput":"{}"},"time":{"start":1795274606882,"end":1795274606996}}}}
{"type":"step_finish","timestamp":1795274607002,"sessionID":"ses-smoke","part":{"id":"prt-finish","sessionID":"ses-smoke","messageID":"msg-tool","type":"step-finish","reason":"tool-calls","snapshot":"snapshot","cost":0,"tokens":{"input":700,"output":21,"reasoning":156,"cache":{"read":6656,"write":0}}}}
{"type":"step_start","timestamp":1795274620374,"sessionID":"ses-smoke","part":{"id":"prt-start","sessionID":"ses-smoke","messageID":"msg-text","type":"step-start","snapshot":"snapshot"}}
{"type":"text","timestamp":1795274622162,"sessionID":"ses-smoke","part":{"id":"prt-text","sessionID":"ses-smoke","messageID":"msg-text","type":"text","text":"I received your message literally.","time":{"start":1795274621867,"end":1795274622162}}}
{"type":"step_finish","timestamp":1795274622171,"sessionID":"ses-smoke","part":{"id":"prt-stop","sessionID":"ses-smoke","messageID":"msg-text","type":"step-finish","reason":"stop","snapshot":"snapshot","cost":0,"tokens":{"input":713,"output":47,"reasoning":215,"cache":{"read":8192,"write":0}}}}
"#;
        let result = parse_output(output).unwrap();
        assert_eq!(result.session_id.as_deref(), Some("ses-smoke"));
        assert_eq!(result.message, "I received your message literally.");
        assert_eq!(result.events.len(), 5);
        assert_eq!(result.events[0]["type"], "tool_use");
        assert_eq!(result.events[1]["part"]["reason"], "tool-calls");
    }

    #[test]
    fn parse_output_joins_only_text_parts_and_preserves_events() {
        let result = super::super::parse_output(HarnessKind::OpenCode, COMPLETE_OUTPUT).unwrap();
        assert_eq!(result.session_id.as_deref(), Some("session-1"));
        assert_eq!(result.message, "hello world");
        assert_eq!(result.events.len(), 5);
        assert_eq!(result.events[2]["type"], "reasoning");
    }

    #[test]
    fn parse_output_fails_closed_for_empty_malformed_truncated_and_nonterminal_output() {
        assert!(parse_output("").is_err());
        assert!(parse_output("\n \n").is_err());
        assert!(parse_output("not-json\n").is_err());
        assert!(parse_output(
            "{\"type\":\"text\",\"part\":{\"type\":\"text\",\"text\":\"partial\"}}\n{\"type\":\"text\""
        )
        .is_err());
        assert!(parse_output(
            "{\"type\":\"text\",\"part\":{\"type\":\"text\",\"text\":\"partial\"}}\n{\"type\":\"step_finish\",\"part\":{\"type\":\"step-finish\",\"reason\":\"tool-calls\"}}\n"
        )
        .is_err());
        assert!(parse_output(
            "{\"type\":\"step_finish\",\"part\":{\"type\":\"step-finish\",\"reason\":\"stop\"}}\n"
        )
        .is_err());
        assert!(parse_output(&format!(
            "{COMPLETE_OUTPUT}{{\"type\":\"step_start\",\"sessionID\":\"session-1\"}}\n"
        ))
        .is_err());
    }

    #[test]
    fn parse_output_rejects_errors_and_conflicting_session_ids_without_echoing_payloads() {
        let error = parse_output(
            "{\"type\":\"error\",\"sessionID\":\"session-1\",\"error\":{\"message\":\"private-payload\"}}\n",
        )
        .unwrap_err();
        assert!(!error.to_string().contains("private-payload"));

        let conflict = parse_output(
            "{\"type\":\"text\",\"sessionID\":\"session-1\",\"part\":{\"type\":\"text\",\"text\":\"partial\"}}\n{\"type\":\"step_finish\",\"sessionID\":\"session-2\",\"part\":{\"type\":\"step-finish\",\"reason\":\"stop\"}}\n",
        )
        .unwrap_err();
        assert!(conflict.to_string().contains("conflicting session IDs"));
    }

    #[cfg(unix)]
    #[test]
    fn fake_process_receives_isolated_config_and_argument_separator() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{SystemTime, UNIX_EPOCH},
        };

        struct TestDirectory(PathBuf);

        impl Drop for TestDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = TestDirectory(
            std::env::temp_dir().join(format!("formless-opencode-{}-{id}", std::process::id())),
        );
        fs::create_dir(&directory.0).unwrap();
        let executable = directory.0.join("fake-opencode");
        fs::write(
            &executable,
            "#!/bin/sh\nset -eu\n[ \"$#\" -eq 8 ]\n[ \"$1\" = run ]\n[ \"$2\" = --standalone ]\n[ \"$3\" = --format ]\n[ \"$4\" = json ]\n[ \"$5\" = --agent ]\n[ \"$6\" = build ]\n[ \"$7\" = -- ]\n[ \"$8\" = --prompt-as-one-argument ]\n[ \"$(pwd)\" = \"$EXPECTED_CWD\" ]\n[ \"$FORMLESS_TOPIC_CAPABILITY\" = unit-test-secret ]\ncase \"$OPENCODE_CONFIG_CONTENT\" in *'{env:FORMLESS_TOPIC_CAPABILITY}'*) ;; *) exit 11 ;; esac\ncase \"$OPENCODE_CONFIG_CONTENT\" in *\"$FORMLESS_TOPIC_CAPABILITY\"*) exit 12 ;; esac\nprintf '%s\\n' '{\"type\":\"text\",\"sessionID\":\"fake-session\",\"part\":{\"type\":\"text\",\"text\":\"fake response\"}}'\nprintf '%s\\n' '{\"type\":\"step_finish\",\"sessionID\":\"fake-session\",\"part\":{\"type\":\"step-finish\",\"reason\":\"stop\"}}'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).unwrap();

        let mut request = request(HarnessRole::Build);
        request.cwd = directory.0.clone();
        request.prompt = "--prompt-as-one-argument".into();
        request.model = None;
        request.executable = Some(executable);
        request.environment.insert(
            OsString::from("EXPECTED_CWD"),
            directory.0.as_os_str().to_owned(),
        );
        request.environment.insert(
            OsString::from("FORMLESS_TOPIC_CAPABILITY"),
            OsString::from("unit-test-secret"),
        );

        let output = command_spec(&request).unwrap().command().output().unwrap();
        assert!(
            output.status.success(),
            "fake process exited unsuccessfully"
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let result = parse_output(&stdout).unwrap();
        assert_eq!(result.session_id.as_deref(), Some("fake-session"));
        assert_eq!(result.message, "fake response");
    }
}
