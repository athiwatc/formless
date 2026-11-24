use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{mpsc, Mutex},
    time::Duration,
};

use super::*;
use crate::local_harness::{
    acp::PermissionRequest, HarnessKind, HarnessPermissionRequest, HarnessUpdate,
    InteractionFuture, StepAccess,
};

fn request(access: StepAccess) -> HarnessRequest {
    HarnessRequest {
        kind: HarnessKind::OpenCode,
        acp_agent: None,
        model: None,
        cwd: PathBuf::from("/work/repo"),
        prompt: "prompt-secret".into(),
        access,
        mcp_command: PathBuf::from("/usr/bin/python3"),
        mcp_args: vec!["mcp.py".into()],
        mcp_env_vars: vec!["PROBE_ENV".into()],
        executable: Some(PathBuf::from("/custom/opencode")),
        environment: BTreeMap::from([
            ("PROBE_ENV".into(), "cap-secret".into()),
            ("PWD".into(), "/wrong".into()),
        ]),
    }
}

fn question() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/opencode-question.json")).unwrap()
}

#[derive(Default)]
struct Handler {
    choice: Option<String>,
    answer: Option<Value>,
    questions: Vec<Value>,
    permissions: Vec<Value>,
    updates: Vec<Value>,
}

impl PermissionHandler for Handler {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String> {
        self.permissions.push(request.tool_call.clone());
        self.choice.clone()
    }

    fn ask_question(&mut self, question: &Value, _: &CancellationToken) -> Option<Value> {
        self.questions.push(question.clone());
        self.answer.clone()
    }

    fn on_update(&mut self, update: &Value) {
        self.updates.push(update.clone());
    }
}

#[test]
fn config_matches_live_permission_fixture_and_environment_is_private() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/opencode-config.json")).unwrap();
    assert_eq!(config(), fixture);
    assert!(!config().to_string().contains("{env:"));
    assert!(!config().to_string().contains("cap-secret"));
    assert!(config().get("mcp").is_none());
    let request = request(StepAccess::Write);
    let prepared = prepare(&request).unwrap();
    assert_eq!(prepared.executable, request.executable);
    assert_eq!(prepared.environment[&OsString::from("PWD")], "/work/repo");
    assert_eq!(
        prepared.environment[&OsString::from("PROBE_ENV")],
        "cap-secret"
    );
    assert_eq!((SPEC.args)(&prepared), vec![OsString::from("acp")]);
    assert_eq!(SPEC.write_modes, ["build"]);
    assert_eq!(SPEC.read_only_modes, ["formless-review"]);
    const { assert!(SPEC.read_only_fail_closed) };
    assert_eq!(SPEC.program, "opencode");
    assert!(crate::local_harness::command_spec(&request).is_err());
    assert!(crate::local_harness::parse_output(HarnessKind::OpenCode, "{}").is_err());
    assert_eq!(
        serde_json::to_value(HarnessKind::OpenCode).unwrap(),
        "opencode"
    );
}

#[test]
fn initialize_requires_opencode_v2_and_acp_v1() {
    for value in [
        json!({}),
        json!({"protocolVersion":2,"agentInfo":{"name":"OpenCode","version":"2.3.2"}}),
        json!({"protocolVersion":1,"agentInfo":{"name":"other","version":"2.3.2"}}),
        json!({"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"1.9.0"}}),
        json!({"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"3.0.0"}}),
        json!({"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"2.3.1"}}),
        json!({"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"bad"}}),
    ] {
        assert!(check_agent(&value).is_err());
    }
    check_agent(&json!({"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"2.3.2"}}))
        .unwrap();
}

fn ask(params: &Value, handler: &mut Handler) -> Value {
    vendor_request(
        "elicitation/create",
        params,
        handler,
        &CancellationToken::new(),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn real_question_capture_translates_choices_and_free_text() {
    let mut handler = Handler {
        answer: Some(json!([{"selected":["Blue"],"text":null}])),
        ..Handler::default()
    };
    assert_eq!(
        ask(&question(), &mut handler),
        json!({"action":"accept","content":{"q0":"Blue"}})
    );
    let questions: Vec<Question> = serde_json::from_value(handler.questions[0].clone()).unwrap();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].question, "Choose Red or Blue.");
    assert_eq!(questions[0].header.as_deref(), Some("Color choice"));
    assert_eq!(questions[0].options[1].label, "Blue");
    assert!(!questions[0].multi_select);
    handler.answer = Some(json!([{"selected":[],"text":"Purple"}]));
    assert_eq!(
        ask(&question(), &mut handler),
        json!({"action":"accept","content":{"q0_custom":"Purple"}})
    );
}

#[test]
fn unanswered_or_cancelled_questions_never_accept() {
    for answer in [None, Some(json!([])), Some(json!([{"selected":[]}]))] {
        let mut handler = Handler {
            answer,
            ..Handler::default()
        };
        assert_eq!(ask(&question(), &mut handler), json!({"action":"cancel"}));
    }
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut handler = Handler::default();
    let result = vendor_request("elicitation/create", &question(), &mut handler, &cancel)
        .unwrap()
        .unwrap();
    assert_eq!(result["action"], "cancel");
    assert!(handler.questions.is_empty());
    assert!(vendor_request("other", &question(), &mut handler, &cancel).is_none());
}

#[test]
fn unsupported_schemas_and_invalid_answers_are_declined() {
    let original = question();
    let mut unsupported = Vec::new();
    let mut changed = original.clone();
    changed["requestedSchema"]["properties"]["q0"]["type"] = json!("array");
    unsupported.push(changed);
    let mut changed = original.clone();
    changed["requestedSchema"]["properties"]["q0"]["minLength"] = json!(3);
    unsupported.push(changed);
    let mut changed = original.clone();
    changed["requestedSchema"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("q0");
    unsupported.push(changed);
    let mut changed = original.clone();
    changed["requestedSchema"]["properties"] = json!({});
    unsupported.push(changed);
    let mut changed = original.clone();
    changed["requestedSchema"]["allOf"] = json!([]);
    unsupported.push(changed);
    for params in unsupported {
        let mut handler = Handler::default();
        assert_eq!(ask(&params, &mut handler)["action"], "decline");
        assert!(handler.questions.is_empty());
    }
    for answer in [
        json!([{"selected":["Not offered"]}]),
        json!([{"selected":["Red","Blue"]}]),
        json!([{"selected":["Blue"],"text":"Purple"}]),
        json!({"not":"answers"}),
        json!([{"selected":["Blue"]},{"selected":["Red"]}]),
    ] {
        let mut handler = Handler {
            answer: Some(answer),
            ..Handler::default()
        };
        assert_eq!(ask(&original, &mut handler)["action"], "decline");
    }
}

fn drive(
    access: StepAccess,
    choice: Option<&str>,
    name: &str,
) -> (HarnessResult, Handler, Vec<Value>) {
    let mut handler = Handler {
        choice: choice.map(str::to_owned),
        ..Handler::default()
    };
    let session = json!({
        "jsonrpc":"2.0","id":2,
        "result":{"sessionId":"s","configOptions":[{
            "id":"mode","category":"mode","type":"select","currentValue":"user-default",
            "options":[{"value":"build","name":"Build"},{"value":"formless-review","name":"Review"}]
        }]}
    });
    let input = [
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"OpenCode","version":"2.3.2"}}}),
        session,
        json!({"jsonrpc":"2.0","id":3,"result":{}}),
        json!({"jsonrpc":"2.0","id":90,"method":"session/request_permission","params":{
            "sessionId":"s",
            "toolCall":{"name":name,"title":"execute","kind":"other","rawInput":{"code":"return await tools.formless.echo_env()"}},
            "options":[{"optionId":"once","kind":"allow_once","name":"Allow once"},{"optionId":"reject","kind":"reject_once","name":"Reject"}]
        }}),
        json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}}}),
        json!({"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}),
    ].map(|v| v.to_string()).join("\n") + "\n";
    let mut output = Vec::new();
    let result = acp::drive(
        &SPEC,
        Cursor::new(input),
        &mut output,
        &request(access),
        &mut handler,
        &CancellationToken::new(),
        true,
    )
    .unwrap();
    let sent = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    (result, handler, sent)
}

#[test]
fn execute_permissions_reach_user_in_both_modes_even_when_js_mentions_formless() {
    for access in [StepAccess::Write, StepAccess::ReadOnly] {
        for choice in [Some("once"), Some("reject"), Some("invalid"), None] {
            let (_, handler, sent) = drive(access, choice, "execute");
            assert_eq!(handler.permissions.len(), 1);
            assert!(!(SPEC.bridge_tool)(&handler.permissions[0]));
            let reply = sent.iter().find(|v| v["id"] == 90).unwrap();
            if matches!(choice, Some("once" | "reject")) {
                assert_eq!(reply["result"]["outcome"]["optionId"], choice.unwrap());
            } else {
                assert_eq!(reply["result"]["outcome"]["outcome"], "cancelled");
            }
            assert_eq!(
                sent[0]["params"]["clientCapabilities"]["elicitation"],
                json!({"form":{}})
            );
            assert_eq!(sent[1]["params"]["mcpServers"][0]["name"], "formless");
            assert_eq!(
                sent[1]["params"]["mcpServers"][0]["env"][0]["value"],
                "cap-secret"
            );
            assert_eq!(sent[2]["method"], "session/set_config_option");
            assert_eq!(
                sent[2]["params"]["value"],
                if access == StepAccess::Write {
                    "build"
                } else {
                    "formless-review"
                }
            );
        }
    }
}

#[test]
fn bridge_looking_names_also_never_bypass_the_user() {
    let (_, handler, _) = drive(StepAccess::Write, Some("reject"), "formless_echo_env");
    assert_eq!(handler.permissions.len(), 1);
}

#[derive(Default)]
struct CoreHandler {
    choice: Option<String>,
    answer: Option<Vec<QuestionAnswer>>,
    permissions: Mutex<Vec<HarnessPermissionRequest>>,
    questions: Mutex<Vec<Vec<Question>>>,
}

impl InteractionHandler for CoreHandler {
    fn request_permission<'a>(
        &'a self,
        request: HarnessPermissionRequest,
        _: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<String>> {
        self.permissions.lock().unwrap().push(request);
        Box::pin(async { self.choice.clone() })
    }

    fn ask_question<'a>(
        &'a self,
        questions: Vec<Question>,
        _: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<Vec<QuestionAnswer>>> {
        self.questions.lock().unwrap().push(questions);
        Box::pin(async { self.answer.clone() })
    }

    fn on_update(&self, _: HarnessUpdate) -> Result<()> {
        Ok(())
    }
}

#[test]
fn shared_interactions_use_core_types_and_keep_always_attempt_scoped() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let options = json!([
        {"optionId":"once","name":"Allow once","kind":"allow_once"},
        {"optionId":"always","name":"Always allow","kind":"allow_always"},
        {"optionId":"reject","name":"Reject","kind":"reject_once"}
    ]);
    let tool_call = json!({"name":"execute","title":"execute","kind":"other","rawInput":{"code":"return await tools.other.echo_env()"}});
    let cancel = CancellationToken::new();
    for (choice, expected) in [
        (Some("once"), "once"),
        (Some("always"), "once"),
        (Some("reject"), "reject"),
        (Some("invalid"), "reject"),
        (None, "reject"),
    ] {
        let core = CoreHandler {
            choice: choice.map(str::to_owned),
            ..CoreHandler::default()
        };
        let mut permissions = AttemptScoped(acp::Interactions {
            handler: &core,
            runtime: runtime.handle().clone(),
        });
        assert_eq!(
            permissions
                .decide(&PermissionRequest {
                    tool_call: &tool_call,
                    options: options.as_array().unwrap(),
                    cancel: &cancel,
                })
                .as_deref(),
            Some(expected)
        );
        let seen = core.permissions.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].title, "execute");
        assert_eq!(seen[0].kind, "other");
        assert_eq!(
            seen[0].raw_input.as_ref().unwrap()["code"],
            tool_call["rawInput"]["code"]
        );
        assert_eq!(seen[0].options[1].option_id, "always");
    }
    let core = CoreHandler {
        answer: Some(vec![QuestionAnswer {
            selected: vec!["Blue".into()],
            text: None,
        }]),
        ..CoreHandler::default()
    };
    let mut permissions = AttemptScoped(acp::Interactions {
        handler: &core,
        runtime: runtime.handle().clone(),
    });
    let result = vendor_request("elicitation/create", &question(), &mut permissions, &cancel)
        .unwrap()
        .unwrap();
    assert_eq!(result, json!({"action":"accept","content":{"q0":"Blue"}}));
    assert_eq!(
        core.questions.lock().unwrap()[0][0].options[1].label,
        "Blue"
    );
}

#[test]
fn validation_rejects_empty_requests_and_invalid_acp_agent_metadata() {
    let mut req = request(StepAccess::Write);
    req.prompt.clear();
    assert!(prepare(&req).is_err());
    req.prompt = "ok".into();
    req.acp_agent = Some("wrong-kind".into());
    assert!(prepare(&req).is_err());
    req.acp_agent = None;
    req.cwd = PathBuf::from("relative");
    assert!(prepare(&req).is_err());
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[cfg(unix)]
#[test]
fn availability_checks_version_without_discovering_credentials_and_bounds_hanging_probes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("fl-opencode-probe-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let executable = dir.join("opencode");
    std::fs::copy(fixture("opencode-availability.py"), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut path =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    path.insert(0, dir.clone());
    let mut environment = BTreeMap::from([
        ("AVAILABILITY_ENV".into(), "provided".into()),
        ("PATH".into(), std::env::join_paths(path).unwrap()),
        (
            "AVAILABILITY_CWD_FILE".into(),
            dir.join("probe-cwd").into_os_string(),
        ),
        (
            "ANTHROPIC_API_KEY".into(),
            "dummy-credential-not-for-logs".into(),
        ),
    ]);
    for (mode, installed, version) in [
        ("normal", true, Some("2.3.2")),
        ("old", true, Some("1.0.0")),
        ("empty", true, None),
        ("error", true, None),
        ("nonzero", true, None),
        ("malformed", true, None),
        ("oversized", true, None),
    ] {
        environment.insert("AVAILABILITY_MODE".into(), mode.into());
        let result = availability(Some(executable.clone()), &environment);
        assert_eq!(result.kind, HarnessKind::OpenCode);
        assert_eq!(result.acp_agent, None);
        assert_eq!(result.installed, installed, "{mode}: {result:?}");
        assert_eq!(result.version.as_deref(), version);
        assert_eq!(result.signed_in, None);
        let detail = result.detail.unwrap();
        assert!(detail.contains("free default model"));
        assert!(!detail.contains("dummy-credential"));
        if mode == "nonzero" {
            assert!(detail.contains("exited unsuccessfully"));
        }
        if mode == "normal" {
            let cwd = PathBuf::from(std::fs::read_to_string(dir.join("probe-cwd")).unwrap());
            let temp_dir = std::fs::canonicalize(std::env::temp_dir()).unwrap();
            assert_ne!(cwd, temp_dir);
            assert!(cwd.starts_with(temp_dir));
            assert!(!cwd.exists());
        }
    }
    environment.insert("AVAILABILITY_MODE".into(), "normal".into());
    assert!(availability(None, &environment).installed);
    environment.remove(&OsString::from("ANTHROPIC_API_KEY"));
    assert_eq!(availability(None, &environment).signed_in, None);
    assert!(!availability(Some(dir.join("missing")), &environment).installed);
    let pid_file = dir.join("probe-pid");
    environment.insert("AVAILABILITY_MODE".into(), "hang".into());
    environment.insert("AVAILABILITY_PID".into(), pid_file.clone().into_os_string());
    let started = std::time::Instant::now();
    let result = availability(Some(executable.clone()), &environment);
    assert!(result.installed);
    assert!(result.detail.unwrap().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid = std::fs::read_to_string(pid_file)
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn availability_kills_outliving_helper_after_stdout_closes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("fl-opencode-orphan-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let executable = dir.join("opencode");
    std::fs::copy(fixture("opencode-availability.py"), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let helper_pid = dir.join("helper-pid");
    let environment = BTreeMap::from([
        ("AVAILABILITY_ENV".into(), "provided".into()),
        ("AVAILABILITY_MODE".into(), "orphan".into()),
        (
            "AVAILABILITY_HELPER_PID".into(),
            helper_pid.clone().into_os_string(),
        ),
    ]);

    let started = std::time::Instant::now();
    let result = availability(Some(executable), &environment);
    assert!(result.installed);
    assert_eq!(result.version.as_deref(), Some("2.3.2"));
    assert!(started.elapsed() < Duration::from_secs(3));

    let pid = std::fs::read_to_string(helper_pid)
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let alive = || {
        (unsafe { libc::kill(pid, 0) }) == 0
            && std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map_or(true, |stat| !stat.contains(") Z "))
    };
    while alive() {
        assert!(
            std::time::Instant::now() < deadline,
            "helper survived the probe"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn aborting_run_cancels_the_blocking_task_without_cancelling_caller() {
    use std::os::unix::fs::PermissionsExt;
    let _env = crate::credential_env::EnvGuard::isolated();
    let dir = std::env::temp_dir().join(format!("fl-opencode-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let executable = dir.join("opencode");
    std::fs::copy(fixture("opencode-acp.py"), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let probe = dir.join("cancel-probe");
    let mut request = request(StepAccess::Write);
    request.cwd = dir.clone();
    request.executable = Some(executable);
    request
        .environment
        .insert("CANCEL_PROBE".into(), probe.clone().into_os_string());
    let caller_cancel = CancellationToken::new();
    let cancel = caller_cancel.clone();
    let core = Arc::new(CoreHandler::default());
    let weak = Arc::downgrade(&core);
    let task = tokio::spawn(async move { run(&request, core, &cancel).await });
    let ready = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while std::fs::read_to_string(&probe).unwrap_or_default() != "ready" {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    ready.expect("fake ACP executable did not reach the prompt");
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while weak.strong_count() != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("blocking ACP task survived aborting the run");
    assert_eq!(std::fs::read_to_string(&probe).unwrap(), "cancelled");
    assert!(!caller_cancel.is_cancelled());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn pre_cancelled_run_does_not_start_opencode() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("fl-opencode-pre-cancel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let executable = dir.join("opencode");
    std::fs::copy(fixture("opencode-acp.py"), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let marker = dir.join("started");
    let mut request = request(StepAccess::Write);
    request.cwd = dir.clone();
    request.executable = Some(executable);
    request.environment.insert(
        "OPENCODE_START_MARKER".into(),
        marker.clone().into_os_string(),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();

    assert!(run(&request, Arc::new(CoreHandler::default()), &cancel)
        .await
        .is_err());
    assert!(!marker.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn public_dispatch_runs_executable_override_with_core_interactions() {
    use std::os::unix::fs::PermissionsExt;
    let _env = crate::credential_env::EnvGuard::isolated();
    let dir = std::env::temp_dir().join(format!("fl-opencode-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let executable = dir.join("opencode");
    std::fs::copy(fixture("opencode-acp.py"), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut request = request(StepAccess::Write);
    request.cwd = dir.clone();
    request.executable = Some(executable);
    request.environment.insert(
        "QUESTION_FIXTURE".into(),
        fixture("opencode-question.json").into_os_string(),
    );
    let core = Arc::new(CoreHandler {
        choice: Some("always".into()),
        answer: Some(vec![QuestionAnswer {
            selected: vec!["Blue".into()],
            text: None,
        }]),
        ..CoreHandler::default()
    });
    let result = crate::local_harness::run(&request, core.clone(), &CancellationToken::new()).await;
    std::fs::remove_dir_all(&dir).unwrap();
    let result = result.unwrap();
    assert_eq!(result.session_id.as_deref(), Some("fake-session"));
    assert_eq!(result.message, "done");
    assert_eq!(core.permissions.lock().unwrap().len(), 1);
    assert_eq!(core.questions.lock().unwrap().len(), 1);
}

// Uses the shared ACP client, not a test-only protocol implementation.
#[test]
#[ignore = "requires FORMLESS_OPENCODE_BIN and live access to the default free model"]
fn real_binary_acp_permissions_and_read_only() {
    let binary = PathBuf::from(
        std::env::var_os("FORMLESS_OPENCODE_BIN").expect("set FORMLESS_OPENCODE_BIN"),
    );
    let root = PathBuf::from(
        std::env::var_os("FORMLESS_OPENCODE_SMOKE_DIR")
            .expect("set FORMLESS_OPENCODE_SMOKE_DIR to a persistent artifact directory"),
    );
    assert!(root.is_dir());
    for case in ["formless", "other", "review"] {
        let dir = (1..)
            .find_map(|n| {
                let dir = root.join(format!("t{n}"));
                match std::fs::create_dir(&dir) {
                    Ok(()) => Some(dir),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(error) => panic!("{error}"),
                }
            })
            .unwrap();
        let access = if case == "review" {
            StepAccess::ReadOnly
        } else {
            StepAccess::Write
        };
        let mut request = request(access);
        request.executable = Some(binary.clone());
        request.cwd = root.join("work");
        assert!(request.cwd.is_dir());
        request.environment.clear();
        for name in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
        ] {
            let path = dir.join(name);
            std::fs::create_dir(&path).unwrap();
            request
                .environment
                .insert(name.into(), path.into_os_string());
        }
        request
            .environment
            .insert("PROBE_ENV".into(), "formless".into());
        let calls = dir.join("mcp-calls.jsonl");
        request.mcp_args = vec![
            fixture("opencode-mcp.py").display().to_string(),
            calls.display().to_string(),
        ];
        let marker = dir.join("must-not-exist");
        let review_file = dir.join("review.txt");
        std::fs::write(&review_file, "original").unwrap();
        let review_code = format!(
            r#"const result = {{}}; for (const [name,input] of [["shell",{{command:{command}}}],["edit",{{filePath:{file},oldString:"original",newString:"changed"}}],["subagent",{{}}]]) {{ try {{ await tools[name](input); result[name] = "UNEXPECTED_SUCCESS"; }} catch (e) {{ result[name] = String(e); }} }} return JSON.stringify(result);"#,
            command = json!(format!("touch {}", marker.display())),
            file = json!(review_file)
        );
        request.prompt = match case {
            "review" => format!("Use execute exactly once to run the following JavaScript verbatim. It is an authorized test that the tools are unavailable in review mode. Return its result; do not substitute a natural-language refusal. Code: {review_code}"),
            "formless" => "Call tools.formless.echo_env() exactly once through execute, and report its result. Also use the shell tool to run pwd once (read-only). Do not call other tools.".to_owned(),
            _ => "Call tools.other.echo_env() exactly once through execute and report its result. Do not call other tools.".to_owned(),
        };
        let mut prepared = prepare(&request).unwrap();
        let mut tested_config = config();
        tested_config["mcp"] = json!({"servers":{"other":{
            "type":"local",
            "command":["/usr/bin/python3",fixture("opencode-mcp.py"),calls],
            "environment":{"PROBE_ENV":"other"}
        }}});
        prepared.environment.insert(
            "OPENCODE_CONFIG_CONTENT".into(),
            tested_config.to_string().into(),
        );
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_vec_pretty(&tested_config).unwrap(),
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let timeout_cancel = cancel.clone();
        let (finished, wait) = mpsc::channel();
        let timer = std::thread::spawn(move || {
            if wait.recv_timeout(Duration::from_secs(120)).is_err() {
                timeout_cancel.cancel();
            }
        });
        let mut handler = Handler {
            choice: Some("once".into()),
            ..Handler::default()
        };
        let result = acp::run(&SPEC, &prepared, &mut handler, &cancel);
        let _ = finished.send(());
        timer.join().unwrap();
        std::fs::write(
            dir.join("updates.json"),
            serde_json::to_vec_pretty(&handler.updates).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("permissions.json"),
            serde_json::to_vec_pretty(&handler.permissions).unwrap(),
        )
        .unwrap();
        let result =
            result.unwrap_or_else(|e| panic!("{case}: {e:#}; captures: {}", dir.display()));
        std::fs::write(
            dir.join("events.json"),
            serde_json::to_vec_pretty(&result.events).unwrap(),
        )
        .unwrap();
        if case == "review" {
            assert!(
                handler
                    .updates
                    .iter()
                    .any(|v| v["update"]["status"] == "completed"
                        && v["update"]["content"].as_array().is_some_and(|items| items
                            .iter()
                            .any(|item| {
                                item["content"]["text"]
                                    .as_str()
                                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                                    .is_some_and(|value| {
                                        ["shell", "edit", "subagent"].iter().all(|name| {
                                            value[name].as_str().is_some_and(|s| {
                                                let s = s.to_ascii_lowercase();
                                                s.contains("not found")
                                                    || s.contains("not available")
                                                    || s.contains("unknown tool")
                                                    || s.contains("no tool")
                                            })
                                        })
                                    })
                            }))),
                "review catalog did not prove shell/edit unavailable: {}",
                dir.display()
            );
            assert!(!marker.exists());
            assert_eq!(std::fs::read_to_string(&review_file).unwrap(), "original");
            assert!(handler.permissions.is_empty());
        } else {
            let calls = std::fs::read_to_string(&calls).unwrap();
            let calls: Vec<Value> = calls
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            assert_eq!(calls, [json!({"server":case})]);
            assert_eq!(handler.permissions.len(), usize::from(case == "other"));
            if case == "other" {
                assert_eq!(handler.permissions[0]["name"], "execute");
                assert_eq!(handler.permissions[0]["kind"], "other");
            } else {
                let shell_id = handler
                    .updates
                    .iter()
                    .find(|u| u["update"]["rawInput"]["command"] == "pwd")
                    .unwrap()["update"]["toolCallId"]
                    .clone();
                assert!(
                    handler
                        .updates
                        .iter()
                        .any(|u| u["update"]["status"] == "completed"
                            && (u["update"]["title"]
                                .as_str()
                                .is_some_and(|s| s.contains("pwd"))
                                || u["update"]["toolCallId"] == shell_id
                                || u["update"]["rawOutput"]["metadata"]["toolCalls"]
                                    .as_array()
                                    .is_some_and(|calls| calls
                                        .iter()
                                        .any(|call| call["tool"] == "shell"
                                            && call["status"] == "completed")))),
                    "Write shell did not complete"
                );
            }
        }
        println!(
            "{case}: permissions={}, success; captures={}",
            handler.permissions.len(),
            dir.display()
        );
    }
}
