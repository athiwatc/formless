//! Grok Build profile hooks for the shared ACP launcher.

use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

use anyhow::{bail, Context, Result};
use formless_protocol::{Question, QuestionAnswer};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::acp::{self, AgentSpec, PermissionHandler};
use super::{HarnessRequest, HarnessResult, HarnessRole};

pub const MIN_VERSION: (u64, u64, u64) = (1, 0, 92);
pub const AUTH_HINT: &str =
    "Grok Build requires authentication. Run `grok login` or set XAI_API_KEY in the harness environment, then retry.";
pub const READ_ONLY_MODE: &str = "plan";

pub const SPEC: AgentSpec = AgentSpec {
    name: "Grok Build",
    program: "grok",
    args: |request| args(request).into_iter().map(OsString::from).collect(),
    check_agent,
    not_signed_in: AUTH_HINT,
    read_only_modes: &[READ_ONLY_MODE],
    read_only_fail_closed: true,
    write_modes: &[],
    client_capabilities: acp::no_extra_capabilities,
    strip_env: &[],
    set_env: &[("GROK_DISABLE_AUTOUPDATER", "1")],
    bridge_tool: acp::no_bridge_tools,
    vendor_request,
};

pub fn run(
    request: &HarnessRequest,
    handler: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    acp::run(&SPEC, request, handler, cancel)
}

pub fn program(request: &HarnessRequest) -> PathBuf {
    request.executable.clone().unwrap_or_else(|| "grok".into())
}

pub fn args(request: &HarnessRequest) -> Vec<&'static str> {
    let mut args = vec!["--permission-mode", "default"];
    if request.role == HarnessRole::Review {
        args.extend(["--sandbox", "read-only"]);
    }
    args.extend(["agent", "--no-leader", "stdio"]);
    args
}

/// Overrides layered onto the launcher's inherited, bridge-filtered environment.
pub fn environment(request: &HarnessRequest) -> BTreeMap<OsString, OsString> {
    let mut environment = request.environment.clone();
    for name in &request.mcp_env_vars {
        environment.remove(&OsString::from(name));
    }
    environment.insert("GROK_DISABLE_AUTOUPDATER".into(), "1".into());
    environment
}

pub fn check_agent(initialized: &Value) -> Result<()> {
    if initialized.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
        bail!("Grok Build does not speak ACP v1; update it with `grok update`.");
    }
    let version = initialized
        .pointer("/_meta/agentVersion")
        .and_then(Value::as_str)
        .and_then(parse_version)
        .context("Grok Build ACP did not report a recognisable _meta.agentVersion.")?;
    if version < MIN_VERSION {
        bail!("Grok Build is older than 1.0.92, the oldest version verified with Formless over ACP; run `grok update`.");
    }
    Ok(())
}

/// Preserves Grok's tool metadata when projecting an ACP permission to the wire.
pub fn permission_raw_input(tool_call: &Value) -> Value {
    let mut input = tool_call.get("rawInput").cloned().unwrap_or(Value::Null);
    if let Some(metadata) = tool_call.get("_meta") {
        if !input.is_object() {
            input = serde_json::json!({"input": input});
        }
        input["_meta"] = metadata.clone();
    }
    input
}

pub fn questions(params: &Value) -> Result<Vec<Question>> {
    let entries = params
        .get("questions")
        .and_then(Value::as_array)
        .context("Grok question request must contain questions.")?;
    if !(1..=4).contains(&entries.len()) {
        bail!("Grok question request must contain one to four questions.");
    }
    let mut questions: Vec<Question> = Vec::new();
    for entry in entries {
        let mut entry = entry
            .as_object()
            .cloned()
            .context("Invalid Grok question.")?;
        if entry.contains_key("multi_select") {
            bail!("Invalid Grok question selection field.");
        }
        let multi_select = entry.remove("multiSelect").unwrap_or(Value::Bool(false));
        entry.insert("multi_select".into(), multi_select);
        let question: Question = serde_json::from_value(Value::Object(entry))
            .map_err(|_| anyhow::anyhow!("Invalid Grok question."))?;
        if question.question.trim().is_empty()
            || questions.iter().any(|q| q.question == question.question)
            || question.options.iter().enumerate().any(|(index, option)| {
                option.label.trim().is_empty()
                    || question.options[..index]
                        .iter()
                        .any(|prior| prior.label == option.label)
            })
        {
            bail!("Invalid or duplicate Grok question or option.");
        }
        questions.push(question);
    }
    Ok(questions)
}

pub fn question_reply(questions: &[Question], answers: &[QuestionAnswer]) -> Result<Value> {
    if questions.is_empty() || questions.len() != answers.len() {
        bail!("Grok question answer count does not match.");
    }
    let mut mapped = serde_json::Map::new();
    for (question, answer) in questions.iter().zip(answers) {
        if (!question.multi_select && answer.selected.len() > 1)
            || answer.selected.iter().enumerate().any(|(index, label)| {
                !question.options.iter().any(|option| option.label == *label)
                    || answer.selected[..index].contains(label)
            })
        {
            bail!("Invalid Grok question selection.");
        }
        let mut parts = answer.selected.clone();
        if let Some(text) = &answer.text {
            if !text.trim().is_empty() {
                parts.push(text.clone());
            }
        }
        if parts.is_empty() {
            bail!("Grok question answer is empty.");
        }
        mapped.insert(question.question.clone(), Value::String(parts.join(", ")));
    }
    Ok(serde_json::json!({"outcome": "accepted", "answers": mapped}))
}

pub fn vendor_request(
    method: &str,
    params: &Value,
    handler: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Option<Result<Value, i64>> {
    if method != "_x.ai/ask_user_question" {
        return None;
    }
    Some((|| {
        let questions = questions(params).map_err(|_| -32602)?;
        let cancelled = || serde_json::json!({"outcome": "cancelled"});
        if cancel.is_cancelled() {
            return Ok(cancelled());
        }
        let request = serde_json::json!({"questions": questions});
        let response = handler.ask_question(&request, cancel);
        if cancel.is_cancelled() {
            return Ok(cancelled());
        }
        let Some(response) = response else {
            return Ok(cancelled());
        };
        let answers: Vec<QuestionAnswer> = serde_json::from_value(response).map_err(|_| -32602)?;
        question_reply(&questions, &answers).map_err(|_| -32602)
    })())
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Cursor, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    use formless_runtime::process_group::SupervisedProcess;
    use serde_json::json;

    use super::*;
    use crate::local_harness::HarnessKind;

    const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"mcpCapabilities":{"http":true,"sse":true}},"authMethods":[{"id":"xai.api_key","name":"xai.api_key","description":"XAI_API_KEY or api_key/env_key in config.toml"}],"_meta":{"agentVersion":"1.0.92","defaultAuthMethodId":"xai.api_key"}}}"#;
    const SESSION: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"smoke-session","configOptions":[{"id":"model","category":"model","type":"select","currentValue":"grok-4.6","options":[{"value":"grok-4.6","name":"grok-4.6"}]}]}}"#;

    fn drive_spec(
        role: HarnessRole,
        lines: Vec<Value>,
        handler: &mut dyn PermissionHandler,
    ) -> (Result<HarnessResult>, Vec<Value>) {
        let mut request = request(role);
        request.environment.insert(
            "FORMLESS_TOPIC_CAPABILITY".into(),
            "fixture-capability".into(),
        );
        let input = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let mut written = Vec::new();
        let result = acp::drive(
            &SPEC,
            Cursor::new(input),
            &mut written,
            &request,
            handler,
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

    fn initialization() -> Vec<Value> {
        vec![
            serde_json::from_str(INITIALIZE).unwrap(),
            serde_json::from_str(SESSION).unwrap(),
        ]
    }

    fn finish(lines: &mut Vec<Value>, id: u64, reason: &str) {
        lines.push(json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"smoke-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Smoke complete."}}}}));
        lines.push(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":reason}}));
    }

    fn permission() -> Value {
        json!({"jsonrpc":"2.0","id":90,"method":"session/request_permission","params":{"sessionId":"smoke-session","toolCall":{"toolCallId":"smoke_call_1","kind":"other","title":"formless_smoke__echo","rawInput":{"variant":"UseTool","tool_name":"formless_smoke__echo","tool_input":{"text":"mcp-smoke"}},"_meta":{"x.ai/tool":{"version":1,"name":"use_tool","read_only":false}}},"options":[{"optionId":"allow-once","name":"allow once","kind":"allow_once"},{"optionId":"reject-once","name":"reject once","kind":"reject_once"}]}})
    }

    struct Choose(Option<&'static str>, usize);
    impl PermissionHandler for Choose {
        fn decide(&mut self, request: &acp::PermissionRequest<'_>) -> Option<String> {
            self.1 += 1;
            assert_eq!(
                permission_raw_input(request.tool_call)["_meta"]["x.ai/tool"]["read_only"],
                false
            );
            self.0.map(String::from)
        }
    }

    #[test]
    fn shared_driver_routes_mcp_permissions_and_attaches_bridge_without_config_edits() {
        for choice in [Some("allow-once"), Some("reject-once"), None] {
            let mut lines = initialization();
            lines.push(permission());
            finish(
                &mut lines,
                3,
                if choice == Some("allow-once") {
                    "end_turn"
                } else {
                    "cancelled"
                },
            );
            let mut handler = Choose(choice, 0);
            let (result, sent) = drive_spec(HarnessRole::Build, lines, &mut handler);
            assert_eq!(handler.1, 1);
            assert_eq!(result.is_ok(), choice == Some("allow-once"));
            assert_eq!(
                sent[1]["params"]["mcpServers"][0]["env"][0]["value"],
                "fixture-capability"
            );
            let reply = sent.iter().find(|message| message["id"] == 90).unwrap();
            if let Some(choice) = choice {
                assert_eq!(reply["result"]["outcome"]["optionId"], choice);
            } else {
                assert_eq!(reply["result"]["outcome"]["outcome"], "cancelled");
            }
        }
    }

    #[test]
    fn shared_driver_uses_grok_version_and_auth_hint() {
        let mut lines = initialization();
        lines[1] = json!({"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"private-auth-payload"}});
        let error = drive_spec(HarnessRole::Build, lines, &mut Choose(None, 0))
            .0
            .unwrap_err()
            .to_string();
        assert!(error.contains(AUTH_HINT));
        assert!(!error.contains("private-auth-payload"));
        let mut lines = initialization();
        lines[0]["result"]["_meta"]["agentVersion"] = json!("1.0.91");
        assert!(drive_spec(HarnessRole::Build, lines, &mut Choose(None, 0))
            .0
            .is_err());
    }

    #[test]
    fn shared_driver_review_falls_back_to_plan_and_routes_other_tools_to_handler() {
        for mode_result in [
            json!({"result":{}}),
            json!({"error":{"code":-32602,"message":"Invalid params"}}),
        ] {
            let mut lines = initialization();
            let mut response = mode_result;
            response["jsonrpc"] = json!("2.0");
            response["id"] = json!(3);
            lines.push(response.clone());
            lines.push(permission());
            finish(&mut lines, 4, "cancelled");
            let mut handler = Choose(Some("reject-once"), 0);
            let (result, sent) = drive_spec(HarnessRole::Review, lines, &mut handler);
            assert!(result.is_err());
            assert_eq!(handler.1, usize::from(response.get("result").is_some()));
            assert_eq!(sent[2]["method"], "session/set_mode");
            assert_eq!(sent[2]["params"]["modeId"], "plan");
            if response.get("result").is_some() {
                assert!(sent.iter().any(|message| message["id"] == 90
                    && message["result"]["outcome"]["optionId"] == "reject-once"));
            } else {
                assert!(!sent
                    .iter()
                    .any(|message| message["method"] == "session/prompt"));
            }
        }
    }

    #[test]
    fn shared_driver_review_other_and_execute_require_handler_approval() {
        for kind in ["other", "execute"] {
            let mut lines = initialization();
            lines.push(json!({"jsonrpc":"2.0","id":3,"result":{}}));
            let mut permission = permission();
            permission["params"]["toolCall"]["kind"] = json!(kind);
            lines.push(permission);
            finish(&mut lines, 4, "end_turn");
            let mut handler = Choose(Some("allow-once"), 0);
            let (result, sent) = drive_spec(HarnessRole::Review, lines.clone(), &mut handler);
            assert!(result.is_ok());
            assert_eq!(handler.1, 1);
            assert!(sent.iter().any(|message| message["id"] == 90
                && message["result"]["outcome"]["optionId"] == "allow-once"));

            lines.last_mut().unwrap()["result"]["stopReason"] = json!("cancelled");
            let (result, sent) = drive_spec(HarnessRole::Review, lines, &mut acp::RejectAll);
            assert!(result.is_err());
            assert!(sent.iter().any(|message| message["id"] == 90
                && message["result"]["outcome"]["optionId"] == "reject-once"));
        }
    }

    #[test]
    fn shared_driver_surfaces_native_questions_and_returns_vendor_replies() {
        for answer in [Some(json!([{"selected":["Local (Recommended)"]}])), None] {
            let mut lines = initialization();
            lines.push(json!({"jsonrpc":"2.0","id":90,"method":"_x.ai/ask_user_question","params":{"sessionId":"smoke-session","toolCallId":"question_call","questions":[{"question":"Which smoke option should be used?","options":[{"label":"Local (Recommended)","description":"Use the local smoke fixture"},{"label":"Skip","description":"Skip the smoke fixture"}],"multiSelect":false}],"mode":"default"}}));
            finish(&mut lines, 3, "end_turn");
            let accepted = answer.is_some();
            let mut handler = Answers(answer);
            let (result, sent) = drive_spec(HarnessRole::Build, lines, &mut handler);
            assert_eq!(result.unwrap().message, "Smoke complete.");
            let reply = sent.iter().find(|message| message["id"] == 90).unwrap();
            assert_eq!(
                reply["result"]["outcome"],
                if accepted { "accepted" } else { "cancelled" }
            );
            if accepted {
                assert_eq!(
                    reply["result"]["answers"]["Which smoke option should be used?"],
                    "Local (Recommended)"
                );
            }
        }
    }

    fn request(role: HarnessRole) -> HarnessRequest {
        HarnessRequest {
            kind: HarnessKind::Antigravity,
            model: None,
            cwd: PathBuf::from("/workspace"),
            prompt: "review".into(),
            role,
            mcp_command: PathBuf::from("/usr/bin/formless"),
            mcp_args: Vec::new(),
            mcp_env_vars: vec!["FORMLESS_TOPIC_CAPABILITY".into()],
            executable: None,
            environment: BTreeMap::new(),
        }
    }

    #[test]
    fn launch_hooks_are_access_scoped_and_keep_credentials_out_of_argv() {
        let mut request = request(HarnessRole::Build);
        request.environment.extend([
            ("XAI_API_KEY".into(), "fixture-key".into()),
            ("GROK_HOME".into(), "/private/grok".into()),
            (
                "FORMLESS_TOPIC_CAPABILITY".into(),
                "fixture-capability".into(),
            ),
            ("GROK_DISABLE_AUTOUPDATER".into(), "0".into()),
        ]);
        assert_eq!(program(&request), PathBuf::from("grok"));
        assert_eq!(
            args(&request),
            [
                "--permission-mode",
                "default",
                "agent",
                "--no-leader",
                "stdio"
            ]
        );
        let env = environment(&request);
        assert_eq!(env[&OsString::from("XAI_API_KEY")], "fixture-key");
        assert_eq!(env[&OsString::from("GROK_HOME")], "/private/grok");
        assert_eq!(env[&OsString::from("GROK_DISABLE_AUTOUPDATER")], "1");
        assert!(!env.contains_key(&OsString::from("FORMLESS_TOPIC_CAPABILITY")));
        request.role = HarnessRole::Review;
        request.executable = Some("/opt/grok".into());
        assert_eq!(program(&request), PathBuf::from("/opt/grok"));
        assert_eq!(
            args(&request),
            [
                "--permission-mode",
                "default",
                "--sandbox",
                "read-only",
                "agent",
                "--no-leader",
                "stdio"
            ]
        );
        assert_eq!(READ_ONLY_MODE, "plan");
        assert!(AUTH_HINT.contains("grok login") && AUTH_HINT.contains("XAI_API_KEY"));
    }

    #[test]
    fn empty_environment_still_disables_updates() {
        assert_eq!(SPEC.set_env, &[("GROK_DISABLE_AUTOUPDATER", "1")]);
        let env = environment(&request(HarnessRole::Build));
        assert_eq!(env.len(), 1);
        assert_eq!(env[&OsString::from("GROK_DISABLE_AUTOUPDATER")], "1");
    }

    #[test]
    fn permission_fixture_preserves_metadata_for_the_shared_user_handler() {
        let fixture = r#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"smoke-session","toolCall":{"toolCallId":"smoke_call_1","kind":"other","title":"formless_smoke__echo","rawInput":{"variant":"UseTool","tool_name":"formless_smoke__echo","tool_input":{"text":"mcp-smoke"}},"_meta":{"x.ai/tool":{"version":1,"name":"use_tool","read_only":false}}},"options":[{"optionId":"allow-once","name":"allow once","kind":"allow_once"},{"optionId":"reject-once","name":"reject once","kind":"reject_once"}]}}"#;
        let message: Value = serde_json::from_reader(Cursor::new(fixture)).unwrap();
        let input = permission_raw_input(&message["params"]["toolCall"]);
        assert_eq!(input["tool_name"], "formless_smoke__echo");
        assert_eq!(input["_meta"]["x.ai/tool"]["read_only"], false);
        assert_eq!(message["params"]["options"][0]["optionId"], "allow-once");
        struct UserChoice;
        impl acp::PermissionHandler for UserChoice {
            fn decide(&mut self, request: &acp::PermissionRequest<'_>) -> Option<String> {
                assert_eq!(request.tool_call["title"], "formless_smoke__echo");
                assert_eq!(
                    permission_raw_input(request.tool_call)["_meta"]["x.ai/tool"]["read_only"],
                    false
                );
                Some(request.options[0]["optionId"].as_str().unwrap().into())
            }
        }
        let params = &message["params"];
        let cancel = CancellationToken::new();
        let request = acp::PermissionRequest {
            tool_call: &params["toolCall"],
            options: params["options"].as_array().unwrap(),
            cancel: &cancel,
        };
        assert_eq!(
            acp::PermissionHandler::decide(&mut UserChoice, &request).as_deref(),
            Some("allow-once")
        );
        assert_eq!(permission_raw_input(&json!({})), Value::Null);
        assert_eq!(
            permission_raw_input(
                &json!({"rawInput": "input", "_meta": {"x.ai/tool": {"read_only": false}}})
            )["input"],
            "input"
        );
    }

    #[test]
    fn native_question_fixture_is_custom_and_translates_the_user_answer() {
        let fixture = r#"{"jsonrpc":"2.0","id":0,"method":"_x.ai/ask_user_question","params":{"sessionId":"smoke-session","toolCallId":"question_call","questions":[{"question":"Which smoke option should be used?","options":[{"label":"Local (Recommended)","description":"Use the local smoke fixture"},{"label":"Skip","description":"Skip the smoke fixture"}],"multiSelect":false}],"mode":"default"}}"#;
        let message: Value = serde_json::from_reader(Cursor::new(fixture)).unwrap();
        assert_eq!(message["method"], "_x.ai/ask_user_question");
        assert_eq!(
            message["params"]["questions"][0]["question"],
            "Which smoke option should be used?"
        );
        let parsed = questions(&message["params"]).unwrap();
        assert!(!parsed[0].multi_select);
        assert_eq!(
            parsed[0].options[0].description.as_deref(),
            Some("Use the local smoke fixture")
        );
        let mut handler = Answers(Some(json!([{"selected": ["Local (Recommended)"]}])));
        assert_eq!(
            vendor_request(
                "_x.ai/ask_user_question",
                &message["params"],
                &mut handler,
                &CancellationToken::new()
            )
            .unwrap()
            .unwrap(),
            json!({"outcome":"accepted", "answers":{"Which smoke option should be used?":"Local (Recommended)"}})
        );
    }

    struct Answers(Option<Value>);

    impl PermissionHandler for Answers {
        fn decide(&mut self, _: &acp::PermissionRequest<'_>) -> Option<String> {
            panic!("question fixture unexpectedly requested permission")
        }
        fn ask_question(&mut self, request: &Value, _: &CancellationToken) -> Option<Value> {
            let questions: Vec<Question> =
                serde_json::from_value(request["questions"].clone()).unwrap();
            assert!(!questions.is_empty());
            self.0.take()
        }
    }

    #[test]
    fn question_hook_handles_multi_select_free_text_decline_and_cancellation() {
        let params = json!({"questions":[{"question":"Pick", "options":[{"label":"A"},{"label":"B"}], "multiSelect":true}, {"question":"Explain"}]});
        let mut handler = Answers(Some(
            json!([{"selected":["A","B"], "text":"extra"}, {"text":"free answer"}]),
        ));
        assert_eq!(
            vendor_request(
                "_x.ai/ask_user_question",
                &params,
                &mut handler,
                &CancellationToken::new()
            )
            .unwrap()
            .unwrap(),
            json!({"outcome":"accepted", "answers":{"Pick":"A, B, extra", "Explain":"free answer"}})
        );
        assert_eq!(
            vendor_request(
                "_x.ai/ask_user_question",
                &params,
                &mut handler,
                &CancellationToken::new()
            )
            .unwrap()
            .unwrap(),
            json!({"outcome":"cancelled"})
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        handler.0 = Some(json!([]));
        assert_eq!(
            vendor_request("_x.ai/ask_user_question", &params, &mut handler, &cancel)
                .unwrap()
                .unwrap(),
            json!({"outcome":"cancelled"})
        );
        assert!(handler.0.is_some());
        assert!(vendor_request("_x.ai/unknown", &params, &mut handler, &cancel).is_none());
        struct CancelOnQuestion;
        impl PermissionHandler for CancelOnQuestion {
            fn decide(&mut self, _: &acp::PermissionRequest<'_>) -> Option<String> {
                panic!("question fixture unexpectedly requested permission")
            }
            fn ask_question(&mut self, _: &Value, cancel: &CancellationToken) -> Option<Value> {
                cancel.cancel();
                Some(json!([]))
            }
        }
        assert_eq!(
            vendor_request(
                "_x.ai/ask_user_question",
                &params,
                &mut CancelOnQuestion,
                &CancellationToken::new()
            )
            .unwrap()
            .unwrap(),
            json!({"outcome":"cancelled"})
        );
    }

    #[test]
    fn question_hook_rejects_malformed_requests_and_answers() {
        let question = json!({"question":"Pick", "options":[{"label":"A"},{"label":"B"}], "multiSelect":false});
        for params in [
            Value::Null,
            json!({"questions":[]}),
            json!({"questions":vec![question.clone();5]}),
            json!({"questions":[{}]}),
            json!({"questions":[{"question":"Pick","multiSelect":"yes"}]}),
            json!({"questions":[question.clone(), question.clone()]}),
            json!({"questions":[{"question":"Pick", "options":[{"label":"A"},{"label":"A"}]}]}),
        ] {
            let mut handler = Answers(Some(json!([])));
            assert_eq!(
                vendor_request(
                    "_x.ai/ask_user_question",
                    &params,
                    &mut handler,
                    &CancellationToken::new()
                )
                .unwrap(),
                Err(-32602)
            );
            assert!(handler.0.is_some());
        }
        let params = json!({"questions":[question]});
        for answer in [
            Value::Null,
            json!([]),
            json!([{}]),
            json!([{"selected":["unknown"]}]),
            json!([{"selected":["A","B"]}]),
            json!([{"selected":["A","A"]}]),
            json!([{"text":"   "}]),
            json!([{"selected":false}]),
        ] {
            let mut handler = Answers(Some(answer));
            assert_eq!(
                vendor_request(
                    "_x.ai/ask_user_question",
                    &params,
                    &mut handler,
                    &CancellationToken::new()
                )
                .unwrap(),
                Err(-32602)
            );
        }
    }

    #[test]
    fn captured_initialize_transcript_uses_grok_metadata_not_agent_info() {
        let mut transcript = Cursor::new(format!("{INITIALIZE}\n"));
        let mut line = String::new();
        transcript.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        check_agent(&response["result"]).unwrap();
        assert!(response["result"].get("agentInfo").is_none());
        let mut newer = response["result"].clone();
        newer["_meta"]["agentVersion"] = json!("1.1.0");
        check_agent(&newer).unwrap();
    }

    #[test]
    fn unsupported_or_missing_version_and_protocol_fail_closed_without_payloads() {
        for initialized in [
            Value::Null,
            json!({}),
            json!({"protocolVersion": 2, "_meta": {"agentVersion": "1.0.92"}}),
            json!({"protocolVersion": 1, "agentInfo": {"version": "1.0.92"}}),
            json!({"protocolVersion": 1, "_meta": {"agentVersion": "1.0.91"}}),
            json!({"protocolVersion": 1, "_meta": {"agentVersion": ""}}),
            json!({"protocolVersion": 1, "_meta": {"agentVersion": "private-payload"}}),
            json!({"protocolVersion": 1, "_meta": {"agentVersion": "1.0.92-alpha"}}),
            json!({"protocolVersion": 1, "_meta": {"agentVersion": "1.0.92.1"}}),
        ] {
            let error = check_agent(&initialized).unwrap_err().to_string();
            assert!(!error.contains("private-payload"));
        }
    }

    #[test]
    #[ignore = "requires Grok and loopback provider/MCP; set FORMLESS_LIVE_GROK=1"]
    fn live_grok_acp_turn() {
        if std::env::var_os("FORMLESS_LIVE_GROK").is_none() {
            eprintln!("FORMLESS_LIVE_GROK not set; skipping");
            return;
        }
        let mut request = request(HarnessRole::Build);
        request.cwd = std::env::current_dir().unwrap();
        request.executable = std::env::var_os("FORMLESS_GROK_BIN").map(PathBuf::from);
        request.mcp_command = PathBuf::from("/usr/bin/python3");
        request.mcp_args = vec![std::env::var("FORMLESS_GROK_MCP_SCRIPT")
            .expect("set FORMLESS_GROK_MCP_SCRIPT to the loopback MCP stub")];
        request.mcp_env_vars = vec!["SMOKE_MCP_LOG".into(), "SMOKE_CAPABILITY".into()];
        request.environment = std::env::vars_os().collect();
        struct LiveUser {
            permissions: usize,
            questions: usize,
        }
        impl PermissionHandler for LiveUser {
            fn decide(&mut self, request: &acp::PermissionRequest<'_>) -> Option<String> {
                self.permissions += 1;
                request
                    .options
                    .iter()
                    .find(|option| option["kind"] == "allow_once")?["optionId"]
                    .as_str()
                    .map(String::from)
            }
            fn ask_question(&mut self, request: &Value, _: &CancellationToken) -> Option<Value> {
                self.questions += 1;
                assert_eq!(
                    request["questions"][0]["question"],
                    "Which smoke option should be used?"
                );
                Some(json!([{"selected":["Local (Recommended)"]}]))
            }
        }
        let mut handler = LiveUser {
            permissions: 0,
            questions: 0,
        };
        let result = run(&request, &mut handler, &CancellationToken::new()).unwrap();
        assert_eq!(result.message, "Smoke complete.");
        assert_eq!(handler.permissions, 1);
        assert_eq!(handler.questions, 1);
    }

    #[test]
    #[ignore = "requires installed Grok Build; set FORMLESS_LIVE_GROK=1"]
    fn live_grok_acp_initialize() {
        if std::env::var_os("FORMLESS_LIVE_GROK").is_none() {
            eprintln!("FORMLESS_LIVE_GROK not set; skipping");
            return;
        }
        let mut request = request(HarnessRole::Build);
        request.executable = std::env::var_os("FORMLESS_GROK_BIN").map(PathBuf::from);
        let mut command = Command::new(program(&request));
        command
            .args(args(&request))
            .envs(environment(&request))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = SupervisedProcess::spawn(command).unwrap();
        let (stdin, stdout, _) = process.take_pipes();
        let mut stdin = stdin.unwrap();
        let stdout = stdout.unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":1,"clientCapabilities":{}}})).unwrap();
        stdin.flush().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let line = rx
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("Grok initialize timed out")
                .unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] == 1 {
                check_agent(&response["result"]).unwrap();
                break;
            }
        }
    }
}
