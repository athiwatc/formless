use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{validate_request, CommandSpec, HarnessRequest, HarnessResult, HarnessRole};

pub const MCP_CONFIG_ENV: &str = "FORMLESS_PI_MCP_CONFIG";
const BRIDGE_CUSTOM_TYPE: &str = "formless-pi-bridge";
const BRIDGE_MESSAGE: &str = "formless-pi-mcp-registered";
const EXTENSION_NAME: &str = "formless-pi-extension.ts";
const EXTENSION_SOURCE: &str = include_str!("pi_extension.ts");

const BUILD_GUIDANCE: &str =
    "Role: Build. Implement the requested changes in the working directory.";
const REVIEW_GUIDANCE: &str =
    "Role: Review. Inspect the requested changes and report findings without modifying files.";

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
    bail!("Pi requires an attempt-private staged extension path; call local_harness::pi::stage_extension and then command_spec_with_extension")
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
    let restore_leading_at = request.prompt.starts_with('@');
    let mcp_config = json!({
        "name": server_name,
        "command": command,
        "args": request.mcp_args,
        "cwd": cwd,
        "envVars": request.mcp_env_vars,
        "restoreLeadingAt": restore_leading_at,
    })
    .to_string();

    let mut args = vec![
        "--mode".into(),
        "json".into(),
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
        match request.role {
            HarnessRole::Build => BUILD_GUIDANCE,
            HarnessRole::Review => REVIEW_GUIDANCE,
        }
        .into(),
    ]);
    if request.role == HarnessRole::Review {
        args.extend([
            "--tools".into(),
            format!("read,grep,find,ls,mcp__{server_name}__*").into(),
        ]);
    }
    let prompt = if restore_leading_at {
        format!("\n{}", request.prompt)
    } else {
        request.prompt.clone()
    };
    args.extend(["--".into(), prompt.into()]);

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
        BRIDGE_CUSTOM_TYPE, BRIDGE_MESSAGE, BUILD_GUIDANCE, EXTENSION_NAME, EXTENSION_SOURCE,
        MCP_CONFIG_ENV, REVIEW_GUIDANCE,
    };
    use crate::local_harness::{HarnessKind, HarnessRequest, HarnessRole};

    fn request(role: HarnessRole) -> HarnessRequest {
        HarnessRequest {
            kind: HarnessKind::Pi,
            model: Some("configured-model".into()),
            cwd: std::env::temp_dir().join("formless repo"),
            prompt: "Make this change".into(),
            role,
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

    fn staged_extension() -> (PathBuf, PathBuf) {
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
        let mut request = request(HarnessRole::Build);
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
                "json",
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
        assert_eq!(args[11], BUILD_GUIDANCE);
        let prompt_index = args.len() - 1;
        assert_eq!(args[prompt_index - 1], "--");
        assert_eq!(args[prompt_index], request.prompt);
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
    fn review_command_limits_builtin_tools_and_keeps_its_mcp_server() {
        let (dir, extension) = staged_extension();
        let spec = command_spec_with_extension(&request(HarnessRole::Review), &extension).unwrap();
        let args = spec
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let role_prompt = args
            .iter()
            .position(|arg| arg == "--append-system-prompt")
            .unwrap();
        assert_eq!(args[role_prompt + 1], REVIEW_GUIDANCE);
        let tools = args.iter().position(|arg| arg == "--tools").unwrap();
        assert!(args[tools + 1].starts_with("read,grep,find,ls,mcp__formless_"));
        assert!(args[tools + 1].ends_with("__*"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn command_spec_omits_optional_model_and_uses_default_executable() {
        let (dir, extension) = staged_extension();
        let mut request = request(HarnessRole::Build);
        request.model = None;
        request.executable = None;
        let spec = command_spec_with_extension(&request, &extension).unwrap();
        assert_eq!(spec.program, PathBuf::from("pi"));
        assert!(!spec.args.iter().any(|arg| arg == "--model"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn command_spec_escapes_only_leading_at_prompts_for_literal_input_restoration() {
        let (dir, extension) = staged_extension();
        for prompt in [
            "@alice please review the parser",
            "@existing-file.txt",
            "@@literal $HOME\nnext line",
            "\n@alice keep the leading newline",
            "Review @alice's parser",
        ] {
            let mut request = request(HarnessRole::Build);
            request.prompt = prompt.into();
            let spec = command_spec_with_extension(&request, &extension).unwrap();
            let config: Value = serde_json::from_str(
                spec.environment[&OsString::from(MCP_CONFIG_ENV)]
                    .to_str()
                    .unwrap(),
            )
            .unwrap();
            let escaped = prompt.starts_with('@');
            assert_eq!(config["restoreLeadingAt"], escaped);
            let expected = if escaped {
                format!("\n{prompt}")
            } else {
                prompt.to_owned()
            };
            assert_eq!(spec.args.last(), Some(&OsString::from(expected)));
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn command_spec_requires_explicit_staged_extension_and_rejects_symlinks() {
        let request = request(HarnessRole::Build);
        assert!(command_spec(&request)
            .unwrap_err()
            .to_string()
            .contains("attempt-private staged extension"));

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
