use std::ffi::OsString;

use super::acp::{self, AgentSpec, PermissionHandler};
use super::{CommandSpec, HarnessRequest, HarnessResult, HarnessRole};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Oldest released `agy` whose ACP negotiation has been observed live.
pub const MIN_AGY_VERSION: (u64, u64, u64) = (1, 5, 14);

/// The official `agy acp` server.
pub const SPEC: AgentSpec = AgentSpec {
    name: "Antigravity",
    program: "agy",
    args: agent_args,
    check_agent,
    not_signed_in: "Antigravity is not signed in. Run `agy` in a terminal and complete its sign-in, then retry. Formless does not handle Antigravity credentials.",
    auth_error_codes: &[-32000],
    read_only_modes: &["plan"],
    read_only_fail_closed: true,
    write_modes: &[],
    client_capabilities: acp::no_extra_capabilities,
    strip_env: &[],
    set_env: &[],
    bridge_tool: acp::no_bridge_tools,
    vendor_request: acp::no_vendor_requests,
};

/// Runs one prompt in a new `agy acp` session with the Formless bridge attached.
pub fn run(
    request: &HarnessRequest,
    permissions: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    acp::run(&SPEC, request, permissions, cancel)
}

/// Opens an `agy acp` session with the Formless bridge attached, without prompting.
pub fn open_session(request: &HarnessRequest) -> Result<String> {
    acp::open_session(&SPEC, request)
}

/// `agy acp` flags; review runs also get agy's terminal sandbox.
fn agent_args(request: &HarnessRequest) -> Vec<OsString> {
    let mut args = vec!["acp".into(), "--disable-telemetry".into()];
    if request.role == HarnessRole::Review {
        args.push("--sandbox".into());
    }
    args
}

fn check_agent(initialized: &Value) -> Result<()> {
    if initialized.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
        bail!("This Antigravity CLI does not speak ACP v1; update it with `agy update`.");
    }
    let info = initialized.get("agentInfo");
    let version = info
        .and_then(|info| info.get("version"))
        .and_then(Value::as_str)
        .and_then(acp::parse_version)
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

pub fn command_spec(_request: &HarnessRequest) -> Result<CommandSpec> {
    Err(anyhow!("Antigravity headless mode (`agy -p`) has no documented per-invocation MCP server option, and its project `.agents/mcp_config.json` is static with no documented way to pass a per-run capability to a stdio server, so Formless will not launch it as a one-shot command or modify MCP configuration. Launch Antigravity through `local_harness::antigravity::run`, which attaches the Formless MCP server per session over `agy acp`. See https://antigravity.google/docs/cli/reference/, https://antigravity.google/docs/mcp/ and https://antigravity.google/docs/ide/extensions/acp/."))
}

pub fn parse_output(output: &str) -> Result<HarnessResult> {
    let output = output.trim();
    if output.is_empty() {
        return Err(anyhow!("Antigravity returned empty output; expected a successful JSON result or stream-json result event."));
    }

    if let Ok(envelope) = serde_json::from_str::<Value>(output) {
        if envelope
            .as_object()
            .is_some_and(|object| object.contains_key("status"))
        {
            return parse_json_envelope(envelope);
        }
    }

    parse_stream_json(output)
}

fn parse_json_envelope(envelope: Value) -> Result<HarnessResult> {
    ensure_success_status(&envelope)?;
    let session_id = required_conversation_id(&envelope)?;
    let message = required_response(&envelope)?;
    Ok(HarnessResult {
        session_id: Some(session_id),
        message,
        events: vec![envelope],
    })
}

fn parse_stream_json(output: &str) -> Result<HarnessResult> {
    let mut events = Vec::new();
    let mut conversation_ids = Vec::new();
    let mut result_index = None;

    for (index, line) in output.lines().enumerate() {
        let event: Value = serde_json::from_str(line).map_err(|_| {
            anyhow!(
                "Malformed Antigravity stream-json event at line {}.",
                index + 1
            )
        })?;
        let object = event.as_object().ok_or_else(|| {
            anyhow!(
                "Antigravity stream-json event at line {} is not a JSON object.",
                index + 1
            )
        })?;
        let event_type = object.get("event").and_then(Value::as_str).ok_or_else(|| {
            anyhow!(
                "Antigravity stream-json event at line {} is missing its event type.",
                index + 1
            )
        })?;

        if result_index.is_some() {
            if event_type == "result" {
                return Err(anyhow!(
                    "Antigravity stream-json output contains duplicate result events."
                ));
            }
            return Err(anyhow!(
                "Antigravity stream-json output contains an event after its final result."
            ));
        }

        match event_type {
            "init" => {
                add_optional_conversation_id(object.get("conversation_id"), &mut conversation_ids)?
            }
            "step_update" => {
                let step = object
                    .get("step_update")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        anyhow!("Antigravity step_update event is missing its payload.")
                    })?;
                add_optional_conversation_id(step.get("conversation_id"), &mut conversation_ids)?;
            }
            "result" => result_index = Some(events.len()),
            _ => {}
        }
        events.push(event);
    }

    let result_index = result_index.ok_or_else(|| {
        anyhow!("Antigravity stream-json output is missing its final result event.")
    })?;
    let result_event = &events[result_index];
    let result = result_event
        .get("result")
        .filter(|value| value.is_object())
        .ok_or_else(|| anyhow!("Antigravity result event is missing its result object."))?;
    ensure_success_status(result)?;
    let session_id = required_conversation_id(result)?;
    conversation_ids.push(session_id.clone());
    ensure_consistent_conversation_ids(&conversation_ids)?;
    let message = required_response(result)?;

    Ok(HarnessResult {
        session_id: Some(session_id),
        message,
        events,
    })
}

fn required_conversation_id(value: &Value) -> Result<String> {
    value
        .get("conversation_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!("Antigravity SUCCESS result is missing a non-empty conversation_id.")
        })
}

fn add_optional_conversation_id(value: Option<&Value>, ids: &mut Vec<String>) -> Result<()> {
    if let Some(value) = value {
        let id = value
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| anyhow!("Antigravity output contains an invalid conversation_id."))?;
        ids.push(id.to_owned());
    }
    Ok(())
}

fn ensure_consistent_conversation_ids(ids: &[String]) -> Result<()> {
    if ids.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err(anyhow!(
            "Antigravity output contains contradictory conversation_id values."
        ));
    }
    Ok(())
}

fn ensure_success_status(value: &Value) -> Result<()> {
    match value.get("status").and_then(Value::as_str) {
        Some("SUCCESS") => Ok(()),
        Some(status)
            if matches!(
                status,
                "ERROR" | "CANCELED" | "WAITING" | "RUNNING" | "INTERRUPTED" | "INVALID"
            ) =>
        {
            Err(anyhow!(
                "Antigravity result status is {status}; only completed SUCCESS results are accepted."
            ))
        }
        Some(_) => Err(anyhow!(
            "Antigravity returned an unknown or non-success result status; only explicit SUCCESS results are accepted."
        )),
        None => Err(anyhow!(
            "Antigravity result is missing a string status; only explicit SUCCESS results are accepted."
        )),
    }
}

fn required_response(value: &Value) -> Result<String> {
    value
        .get("response")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Antigravity SUCCESS result is missing a string response."))
}

#[cfg(test)]
mod tests {
    use super::super::{
        command_spec as dispatch_command_spec, parse_output as dispatch_parse_output, HarnessKind,
        HarnessRequest, HarnessRole,
    };
    use super::parse_output;
    use std::{
        collections::BTreeMap,
        ffi::OsString,
        path::{Path, PathBuf},
    };

    // Excerpt from https://antigravity.google/docs/cli/headless/ JSON output example.
    const OFFICIAL_DOCS_JSON_ENVELOPE: &str = r#"{
      "conversation_id": "055a398f-db14-4c5f-abbb-1bf03f8120a7",
      "status": "SUCCESS",
      "response": "A git rebase rewrites the commit history.\n",
      "duration_seconds": 7.16,
      "num_turns": 1,
      "usage": {"input_tokens": 10415, "output_tokens": 657, "total_tokens": 11072}
    }"#;

    // Excerpts from https://antigravity.google/docs/cli/headless/ stream-json example.
    const OFFICIAL_DOCS_STREAM_JSON: &str = concat!(
        r#"{"event":"init","conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","init":{"cwd":"/home/user/project","tools":["run_command"],"permission_mode":"request-review"}}"#,
        "\n",
        r#"{"event":"step_update","step_update":{"conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","step_index":0,"state":"DONE","step_type":"user_input"}}"#,
        "\n",
        r#"{"event":"step_update","step_update":{"conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","step_index":3,"state":"DONE","step_type":"agent_response","text_delta":"Git rebase rewrites the commit history.\n","duration_seconds":6.28}}"#,
        "\n",
        r#"{"event":"result","result":{"conversation_id":"c3b66b04-872b-4fbe-a3a4-058a026ef20a","status":"SUCCESS","response":"Git rebase rewrites the commit history.\n","duration_seconds":6.88,"num_turns":1,"usage":{"input_tokens":10418,"output_tokens":589,"total_tokens":11007}}}"#,
        "\n"
    );

    fn request(workspace: &Path) -> HarnessRequest {
        HarnessRequest {
            kind: HarnessKind::Antigravity,
            model: Some("model-secret".into()),
            cwd: workspace.to_path_buf(),
            prompt: "prompt-secret".into(),
            role: HarnessRole::Build,
            mcp_command: PathBuf::from("/secret/mcp-command"),
            mcp_args: vec!["mcp-argument-secret".into()],
            mcp_env_vars: vec!["FORMLESS_TOPIC_CAPABILITY".into()],
            executable: Some(PathBuf::from("/secret/agy")),
            environment: BTreeMap::<OsString, OsString>::new(),
        }
    }

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("fl-antigravity-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_official_json_envelope_and_preserves_it() {
        let parsed = parse_output(OFFICIAL_DOCS_JSON_ENVELOPE).unwrap();
        assert_eq!(
            parsed.session_id.as_deref(),
            Some("055a398f-db14-4c5f-abbb-1bf03f8120a7")
        );
        assert_eq!(
            parsed.message,
            "A git rebase rewrites the commit history.\n"
        );
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0]["status"], "SUCCESS");
        assert_eq!(parsed.events[0]["usage"]["input_tokens"], 10415);
        assert_eq!(
            dispatch_parse_output(HarnessKind::Antigravity, OFFICIAL_DOCS_JSON_ENVELOPE)
                .unwrap()
                .message,
            parsed.message
        );
    }

    #[test]
    fn parses_official_stream_json_and_preserves_all_events() {
        let parsed = parse_output(OFFICIAL_DOCS_STREAM_JSON).unwrap();
        assert_eq!(
            parsed.session_id.as_deref(),
            Some("c3b66b04-872b-4fbe-a3a4-058a026ef20a")
        );
        assert_eq!(parsed.message, "Git rebase rewrites the commit history.\n");
        assert_eq!(parsed.events.len(), 4);
        assert_eq!(parsed.events[0]["event"], "init");
        assert_eq!(
            parsed.events[2]["step_update"]["step_type"],
            "agent_response"
        );
        assert_eq!(parsed.events[3]["event"], "result");
    }

    #[test]
    fn accepts_empty_response_string() {
        let parsed =
            parse_output(r#"{"conversation_id":"session-1","status":"SUCCESS","response":""}"#)
                .unwrap();
        assert_eq!(parsed.message, "");
    }

    #[test]
    fn rejects_empty_malformed_and_plain_text_output() {
        for output in ["", " \n\t", "not json", "completed successfully"] {
            assert!(parse_output(output).is_err(), "accepted {output:?}");
        }
        assert!(parse_output(r#"{"status":"SUCCESS""#).is_err());
        assert!(parse_output("{\"event\":\"init\"}\nnot json\n{\"event\":\"result\"}").is_err());
    }

    #[test]
    fn rejects_missing_status_response_or_conversation_id() {
        assert!(parse_output(r#"{"conversation_id":"session-1","response":"ok"}"#).is_err());
        assert!(parse_output(r#"{"conversation_id":"session-1","status":"SUCCESS"}"#).is_err());
        assert!(parse_output(r#"{"status":"SUCCESS","response":"ok"}"#).is_err());
        assert!(
            parse_output(r#"{"conversation_id":"  ","status":"SUCCESS","response":"ok"}"#).is_err()
        );
        assert!(parse_output(
            r#"{"conversation_id":"session-1","status":"SUCCESS","response":42}"#
        )
        .is_err());
    }

    #[test]
    fn rejects_known_and_unknown_non_success_status_without_echoing_error_payload() {
        for status in [
            "WAITING",
            "RUNNING",
            "ERROR",
            "CANCELED",
            "INTERRUPTED",
            "INVALID",
            "SECRET_STATUS",
        ] {
            let output = format!(
                r#"{{"conversation_id":"session-1","status":"{status}","response":"","error":"api_key=do-not-echo"}}"#
            );
            let error = parse_output(&output).unwrap_err().to_string();
            assert!(!error.contains("api_key=do-not-echo"));
            assert!(!error.contains("SECRET_STATUS"));
            if status != "SECRET_STATUS" {
                assert!(error.contains(status));
            }
        }
    }

    #[test]
    fn reports_stream_error_status_before_missing_final_conversation_id() {
        let output = concat!(
            r#"{"event":"init","conversation_id":"session-1"}"#,
            "\n",
            r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"provider-secret"}}"#
        );
        let error = parse_output(output).unwrap_err().to_string();
        assert!(error.contains("ERROR"));
        assert!(!error.contains("provider-secret"));
    }

    #[test]
    fn rejects_stream_without_result_duplicate_result_or_event_after_result() {
        assert!(
            parse_output(r#"{"event":"init","conversation_id":"session-1"}"#)
                .unwrap_err()
                .to_string()
                .contains("missing its final result")
        );

        let last_line = OFFICIAL_DOCS_STREAM_JSON.lines().last().unwrap();
        let duplicate = format!("{OFFICIAL_DOCS_STREAM_JSON}{last_line}");
        assert!(parse_output(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate result"));

        let after_result = format!(
            "{OFFICIAL_DOCS_STREAM_JSON}{}",
            r#"{"event":"step_update","step_update":{"step_type":"tool"}}"#
        );
        assert!(parse_output(&after_result)
            .unwrap_err()
            .to_string()
            .contains("event after its final result"));
    }

    #[test]
    fn rejects_stream_result_without_object_status_response_or_final_id() {
        for output in [
            r#"{"event":"result"}"#,
            concat!(
                r#"{"event":"init","conversation_id":"session-1"}"#,
                "\n",
                r#"{"event":"result","result":{"conversation_id":"session-1","response":"ok"}}"#
            ),
            concat!(
                r#"{"event":"init","conversation_id":"session-1"}"#,
                "\n",
                r#"{"event":"result","result":{"conversation_id":"session-1","status":"SUCCESS"}}"#
            ),
            concat!(
                r#"{"event":"init","conversation_id":"session-1"}"#,
                "\n",
                r#"{"event":"result","result":{"status":"SUCCESS","response":"ok"}}"#
            ),
        ] {
            assert!(parse_output(output).is_err(), "accepted {output}");
        }
    }

    #[test]
    fn rejects_contradictory_protocol_conversation_ids() {
        let contradictory = OFFICIAL_DOCS_STREAM_JSON.replacen(
            "c3b66b04-872b-4fbe-a3a4-058a026ef20a",
            "different-session-id",
            1,
        );
        assert!(parse_output(&contradictory)
            .unwrap_err()
            .to_string()
            .contains("contradictory conversation_id"));
    }

    #[test]
    fn ignores_unrelated_conversation_id_inside_tool_payload() {
        let output = concat!(
            r#"{"event":"init","conversation_id":"session-1"}"#,
            "\n",
            r#"{"event":"step_update","step_update":{"conversation_id":"session-1","tool":{"conversation_id":"unrelated-tool-session"}}}"#,
            "\n",
            r#"{"event":"result","result":{"conversation_id":"session-1","status":"SUCCESS","response":"ok"}}"#
        );
        assert_eq!(parse_output(output).unwrap().message, "ok");
    }

    #[test]
    fn fail_closed_command_spec_does_not_change_existing_workspace_config() {
        let workspace = TempWorkspace::new();
        let config_path = workspace.0.join(".agents/mcp_config.json");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let config = br#"{"mcpServers":{"existing":{"command":"keep-me","args":[]}}}"#;
        std::fs::write(&config_path, config).unwrap();
        let original = std::fs::read(&config_path).unwrap();

        let error = dispatch_command_spec(&request(&workspace.0))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no documented per-invocation MCP server option"));
        assert!(error.contains("agy acp"));
        assert!(error.contains("local_harness::antigravity::run"));
        assert!(error.contains("https://antigravity.google/docs/cli/reference/"));
        assert!(error.contains("https://antigravity.google/docs/mcp/"));
        assert!(!error.contains("prompt-secret"));
        assert!(!error.contains("model-secret"));
        assert!(!error.contains("/secret/mcp-command"));
        assert_eq!(std::fs::read(config_path).unwrap(), original);
    }

    #[test]
    fn harness_kind_serializes_as_antigravity() {
        assert_eq!(
            serde_json::to_string(&HarnessKind::Antigravity).unwrap(),
            "\"antigravity\""
        );
        assert_eq!(
            serde_json::from_str::<HarnessKind>("\"antigravity\"").unwrap(),
            HarnessKind::Antigravity
        );
    }
}
