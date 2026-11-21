use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Cursor, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use formless_runtime::process_group::SupervisedProcess;
use serde_json::json;

use super::*;
use crate::local_harness::{
    acp::{PermissionRequest, RejectAll},
    HarnessKind,
};

fn captured(id: &str) -> Value {
    serde_json::from_str::<Value>(include_str!("captured.json")).unwrap()[id].clone()
}

fn request(access: StepAccess) -> HarnessRequest {
    HarnessRequest {
        kind: HarnessKind::Acp,
        acp_agent: Some("gemini".into()),
        model: None,
        cwd: std::env::current_dir().unwrap(),
        prompt: "fixture prompt".into(),
        access,
        mcp_command: std::env::current_exe().unwrap(),
        mcp_args: vec!["mcp".into()],
        mcp_env_vars: vec![],
        executable: None,
        environment: BTreeMap::new(),
    }
}

fn drive_spec(
    spec: &AgentSpec,
    access: StepAccess,
    lines: &[Value],
    handler: &mut dyn PermissionHandler,
) -> (Result<HarnessResult>, Vec<Value>) {
    let input = lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut output = Vec::new();
    let result = acp::drive(
        spec,
        Cursor::new(input),
        &mut output,
        &request(access),
        handler,
        &CancellationToken::new(),
        true,
    );
    let sent = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (result, sent)
}

fn session(fixture: &Value) -> Value {
    if fixture["session_new"].get("result").is_some() {
        fixture["session_new"].clone()
    } else {
        // Authenticated session shape is synthetic where the live agent required login.
        json!({"jsonrpc":"2.0", "id":2, "result":{"sessionId":"fixture-session"}})
    }
}

fn reply(id: u64, result: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "result":result})
}

fn update(session: &str, value: Value) -> Value {
    json!({"jsonrpc":"2.0", "method":"session/update", "params":{"sessionId":session, "update":value}})
}

fn options() -> Value {
    json!([
        {"optionId":"yes", "kind":"allow_once", "name":"Allow this time", "_meta":{"vendor":"opaque"}},
        {"optionId":"no", "kind":"reject_once", "name":"Decline", "extra":[1,2]}
    ])
}

fn permission(session: &str, kind: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":90, "method":"session/request_permission", "params":{
        "sessionId":session, "toolCall":{"toolCallId":"call-1", "kind":kind, "title":"A tool", "_meta":{"opaque":true}},
        "options":options()
    }})
}

#[derive(Default)]
struct Handler {
    decisions: usize,
    updates: Vec<Value>,
}

impl PermissionHandler for Handler {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String> {
        assert_eq!(request.options, options().as_array().unwrap());
        assert_eq!(
            request.tool_call,
            &permission("unused", "read")["params"]["toolCall"]
        );
        self.decisions += 1;
        Some("yes".into())
    }

    fn on_update(&mut self, update: &Value) {
        self.updates.push(update.clone());
    }
}

fn assert_agent(id: &str, program: &str, args: &[&str], review_args: &[&str]) {
    let spec = spec(id).unwrap();
    assert_eq!(spec.program, program);
    assert_eq!(
        (spec.args)(&request(StepAccess::Write)),
        args.iter()
            .map(|arg| OsString::from(*arg))
            .collect::<Vec<_>>()
    );
    let expected: Vec<_> = args
        .iter()
        .chain(review_args)
        .map(|arg| OsString::from(*arg))
        .collect();
    assert_eq!((spec.args)(&request(StepAccess::ReadOnly)), expected);
    assert!(spec.set_env.is_empty());
    let fixture = captured(id);
    let initialized = &fixture["initialize"]["result"];
    (spec.check_agent)(initialized).unwrap();
    let mut no_version = initialized.clone();
    if let Some(info) = no_version
        .get_mut("agentInfo")
        .and_then(Value::as_object_mut)
    {
        info.remove("version");
    }
    (spec.check_agent)(&no_version).unwrap();
    let mut bad = initialized.clone();
    bad["protocolVersion"] = json!(2);
    assert!((spec.check_agent)(&bad).is_err());
    assert!((spec.check_agent)(&json!({})).is_err());
    if initialized.get("agentInfo").is_some() {
        bad = initialized.clone();
        bad["agentInfo"]["name"] = json!("wrong-agent");
        assert!((spec.check_agent)(&bad).is_err());
    }

    let auth_error = if fixture["session_new"].get("error").is_some() {
        fixture["session_new"].clone()
    } else {
        json!({"jsonrpc":"2.0", "id":2, "error":{"code":-32000,"message":"Authentication required"}})
    };
    let (result, _) = drive_spec(
        spec,
        StepAccess::Write,
        &[fixture["initialize"].clone(), auth_error],
        &mut RejectAll,
    );
    let error = result.unwrap_err().to_string();
    assert!(error.contains(spec.not_signed_in), "{id}: {error}");
    for method in initialized["authMethods"].as_array().into_iter().flatten() {
        assert!(
            error.contains(method["id"].as_str().unwrap()),
            "{id}: {error}"
        );
    }
    let (result, _) = drive_spec(
        spec,
        StepAccess::Write,
        &[
            fixture["initialize"].clone(),
            json!({"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"private error payload"}}),
        ],
        &mut RejectAll,
    );
    let error = result.unwrap_err().to_string();
    assert!(error.contains("-32601"));
    assert!(!error.contains("private error payload"));

    let session = session(&fixture);
    let session_id = session["result"]["sessionId"].as_str().unwrap();
    let updates = [
        json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}),
        json!({"sessionUpdate":"tool_call","toolCallId":"call-1","title":"Tool","status":"pending"}),
        json!({"sessionUpdate":"plan","entries":[{"content":"Step","priority":"high","status":"pending"}]}),
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}),
    ].map(|value| update(session_id, value));
    let mut lines = vec![fixture["initialize"].clone(), session.clone()];
    let prompt_id = if spec.write_modes.is_empty() {
        3
    } else {
        lines.push(reply(3, json!({})));
        4
    };
    lines.push(permission(session_id, "read"));
    lines.extend(updates.clone());
    lines.push(reply(prompt_id, json!({"stopReason":"end_turn"})));
    let mut handler = Handler::default();
    let (result, sent) = drive_spec(spec, StepAccess::Write, &lines, &mut handler);
    assert_eq!(result.unwrap().message, "done");
    assert_eq!(handler.decisions, 1);
    assert_eq!(
        handler.updates,
        updates.map(|value| value["params"].clone())
    );
    let response = sent.iter().find(|value| value["id"] == 90).unwrap();
    assert_eq!(response["result"]["outcome"]["optionId"], "yes");
    if let Some(mode) = spec.write_modes.first() {
        assert_eq!(sent[2]["params"]["configId"], "mode");
        assert_eq!(sent[2]["params"]["value"], *mode);
        assert_ne!(*mode, "bypass");
    }

    let mut empty_turn = vec![fixture["initialize"].clone(), session.clone()];
    if !spec.write_modes.is_empty() {
        empty_turn.push(reply(3, json!({})));
    }
    empty_turn.push(reply(prompt_id, json!({"stopReason":"end_turn"})));
    let (result, _) = drive_spec(spec, StepAccess::Write, &empty_turn, &mut RejectAll);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("without a response message"));

    let mut review_session = session.clone();
    let mode = spec.read_only_modes.first().copied();
    if let Some(mode) = mode {
        if review_session["result"].get("configOptions").is_none() {
            review_session["result"]["modes"] = json!({"availableModes":[{"id":mode}]});
        }
    }
    let mut lines = vec![fixture["initialize"].clone(), review_session];
    let prompt_id = if mode.is_some() {
        lines.push(reply(3, json!({})));
        4
    } else {
        3
    };
    lines.push(permission(session_id, "edit"));
    lines.push(update(
        session_id,
        json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"review"}}),
    ));
    lines.push(reply(prompt_id, json!({"stopReason":"end_turn"})));
    let (result, sent) = drive_spec(spec, StepAccess::ReadOnly, &lines, &mut RejectAll);
    assert_eq!(result.unwrap().message, "review");
    let response = sent.iter().find(|value| value["id"] == 90).unwrap();
    assert_eq!(response["result"]["outcome"]["optionId"], "no");
    if let Some(mode) = mode {
        assert_eq!(
            sent[2]["params"]
                .get("value")
                .or_else(|| sent[2]["params"].get("modeId")),
            Some(&json!(mode))
        );
    }
}

macro_rules! agent_tests {
    ($($id:ident: $program:literal, [$($arg:literal),*], [$($review:literal),*];)*) => {
        $(#[test] fn $id() { assert_agent(stringify!($id), $program, &[$($arg),*], &[$($review),*]); })*
    };
}

agent_tests! {
    gemini: "gemini", ["--acp"], ["--approval-mode", "plan"];
    copilot: "copilot", ["--acp", "--stdio"], ["--plan"];
    cursor: "cursor-agent", ["acp"], ["--mode", "plan"];
    droid: "droid", ["exec", "--output-format", "acp"], [];
    kiro: "kiro-cli", ["acp"], [];
    junie: "junie", ["--acp=true"], [];
    qwen: "qwen", ["--acp"], ["--approval-mode", "plan"];
    kimi: "kimi", ["acp"], ["--plan"];
    vibe: "vibe-acp", [], [];
    auggie: "auggie", ["--acp"], [];
    goose: "goose", ["acp"], [];
    cline: "cline", ["--acp"], ["--plan"];
    devin: "devin", ["acp"], [];
}

#[test]
fn registry_and_unknown_ids() {
    let fixtures: Value = serde_json::from_str(include_str!("captured.json")).unwrap();
    for id in fixtures.as_object().unwrap().keys() {
        assert!(agent_ids().contains(&id.as_str()));
        assert!(spec(id).is_some());
    }
    for id in ["", "missing", "Gemini"] {
        assert!(spec(id).is_none());
        let request = request(StepAccess::Write);
        assert!(run(id, &request, &mut RejectAll, &CancellationToken::new())
            .unwrap_err()
            .to_string()
            .contains("Unknown ACP agent"));
        assert!(open_session(id, &request)
            .unwrap_err()
            .to_string()
            .contains("Unknown ACP agent"));
    }
}

#[test]
fn junie_prefers_mode_id_over_category_and_never_selects_brave_mode() {
    let fixture = captured("junie");
    let mut session = fixture["session_new"].clone();
    let id = session["result"]["sessionId"].as_str().unwrap().to_owned();
    let modes = session["result"]["configOptions"].as_array_mut().unwrap();
    modes.reverse();
    modes[0]["options"]
        .as_array_mut()
        .unwrap()
        .push(json!({"value":"plan","name":"Decoy"}));
    let (result, sent) = drive_spec(
        &JUNIE,
        StepAccess::ReadOnly,
        &[
            fixture["initialize"].clone(),
            session,
            reply(3, json!({})),
            update(
                &id,
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}),
            ),
            reply(4, json!({"stopReason":"end_turn"})),
        ],
        &mut RejectAll,
    );
    result.unwrap();
    assert_eq!(sent[2]["params"]["configId"], "mode");
    assert_eq!(sent[2]["params"]["value"], "plan");
}

#[test]
fn category_mode_fallback_preserves_antigravity_and_filters_candidates() {
    let fixture = captured("junie");
    let mut session = fixture["session_new"].clone();
    session["result"]["configOptions"] = json!([
        {"id":"unrelated","category":"mode","options":[{"value":"on"}]},
        {"id":"execution","category":"mode","options":[{"value":"plan"}]}
    ]);
    let id = session["result"]["sessionId"].as_str().unwrap();
    let lines = vec![
        fixture["initialize"].clone(),
        session.clone(),
        reply(3, json!({})),
        update(
            id,
            json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}),
        ),
        reply(4, json!({"stopReason":"end_turn"})),
    ];
    let (result, sent) = drive_spec(&JUNIE, StepAccess::ReadOnly, &lines, &mut RejectAll);
    result.unwrap();
    assert_eq!(sent[2]["params"]["configId"], "execution");
    assert_eq!(sent[2]["params"]["value"], "plan");
}

#[test]
fn strict_profiles_fail_closed_and_devin_can_select_ask() {
    for spec in [&JUNIE, &DEVIN] {
        let id = if spec.name == "Junie" {
            "junie"
        } else {
            "devin"
        };
        let fixture = captured(id);
        let session = reply(
            2,
            json!({"sessionId":"fixture-session","configOptions":[{"id":"mode","options":[{"value":"bypass"}]}]}),
        );
        let (result, sent) = drive_spec(
            spec,
            StepAccess::ReadOnly,
            &[
                fixture["initialize"].clone(),
                session,
                json!({"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"no mode"}}),
            ],
            &mut RejectAll,
        );
        assert!(result.unwrap_err().to_string().contains("read-only"));
        assert_eq!(sent[2]["params"]["modeId"], "plan");
    }
    let fixture = captured("devin");
    let session = reply(
        2,
        json!({"sessionId":"fixture-session","modes":{"availableModes":[{"id":"ask"}]}}),
    );
    let (result, sent) = drive_spec(
        &DEVIN,
        StepAccess::ReadOnly,
        &[
            fixture["initialize"].clone(),
            session,
            reply(3, json!({})),
            update(
                "fixture-session",
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}),
            ),
            reply(4, json!({"stopReason":"end_turn"})),
        ],
        &mut RejectAll,
    );
    result.unwrap();
    assert_eq!(sent[2]["params"]["modeId"], "ask");
}

/// Live initialize only, not a prompt test. Point the gate at a directory of installed
/// CLI executables (or symlinks); missing binaries are skipped, but an empty dir fails.
#[test]
#[ignore = "live ACP initialize; requires FORMLESS_LIVE_ACP_BIN_DIR"]
fn live_acp_agents_initialize_installed_binaries() {
    live_initialize(StepAccess::Write);
}

/// Live read-only argv acceptance only; does not verify prompt enforcement.
#[test]
#[ignore = "live read-only ACP initialize; requires FORMLESS_LIVE_ACP_BIN_DIR"]
fn live_acp_agents_initialize_read_only_installed_binaries() {
    live_initialize(StepAccess::ReadOnly);
}

fn live_initialize(access: StepAccess) {
    let Some(bin_dir) = std::env::var_os("FORMLESS_LIVE_ACP_BIN_DIR") else {
        return;
    };
    let bin_dir = PathBuf::from(bin_dir);
    let root = std::env::temp_dir().join(format!(
        "formless-live-acp-{}-{access:?}",
        std::process::id()
    ));
    let mut checked = 0;
    for id in agent_ids() {
        let spec = spec(id).unwrap();
        let binary = bin_dir.join(spec.program);
        if !binary.is_file() {
            continue;
        }
        let home = root.join(id);
        std::fs::create_dir_all(&home).unwrap();
        let mut command = Command::new(binary);
        command
            .args((spec.args)(&request(access)))
            .env_clear()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(&home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = SupervisedProcess::spawn(command).unwrap();
        let (stdin, stdout, _) = process.take_pipes();
        let mut stdin = stdin.unwrap();
        let stdout = stdout.unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if value["id"] == 1 {
                        let _ = sender.send(value);
                        break;
                    }
                }
            }
        });
        writeln!(stdin, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}
        }})).unwrap();
        stdin.flush().unwrap();
        let response = receiver
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        (spec.check_agent)(&response["result"]).unwrap_or_else(|error| panic!("{id}: {error}"));
        println!("live initialize {access:?}: {id} OK");
        checked += 1;
    }
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        checked > 0,
        "no installed ACP binaries in {}",
        bin_dir.display()
    );
}
