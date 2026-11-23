use super::*;
use crate::local_harness::{
    HarnessPermissionRequest, HarnessRequest, HarnessUpdate, InteractionFuture, InteractionHandler,
};
use formless_protocol::{Question, QuestionAnswer, TopicAttemptInfo, TopicAttemptStatus};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct TopicInteractions {
    permissions: Mutex<Vec<HarnessPermissionRequest>>,
    questions: Mutex<Vec<Vec<Question>>>,
    updates: Mutex<Vec<HarnessUpdate>>,
}

impl InteractionHandler for TopicInteractions {
    fn request_permission<'a>(
        &'a self,
        request: HarnessPermissionRequest,
        _: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<String>> {
        self.permissions.lock().unwrap().push(request);
        Box::pin(async { Some("once".into()) })
    }

    fn ask_question<'a>(
        &'a self,
        questions: Vec<Question>,
        _: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<Vec<QuestionAnswer>>> {
        self.questions.lock().unwrap().push(questions);
        Box::pin(async {
            Some(vec![QuestionAnswer {
                selected: vec!["Blue".into()],
                text: None,
            }])
        })
    }

    fn on_update(&self, update: HarnessUpdate) -> anyhow::Result<()> {
        self.updates.lock().unwrap().push(update);
        Ok(())
    }
}

fn attempt() -> TopicAttemptInfo {
    TopicAttemptInfo {
        id: "dispatch-attempt".into(),
        topic_id: "dispatch-topic".into(),
        feature_id: "dispatch-feature".into(),
        step_title: "dispatch step".into(),
        access: StepAccess::Write,
        model: None,
        inputs: vec![],
        outputs: vec![],
        summary: None,
        harness: HarnessKind::OpenCode,
        acp_agent: None,
        status: TopicAttemptStatus::Running,
        conversation_session_id: "dispatch-session".into(),
        harness_session_id: None,
        detail: None,
        created_at: "now".into(),
        updated_at: "now".into(),
    }
}

fn request(f: &Fixture, cancel_probe: Option<PathBuf>) -> HarnessRequest {
    use std::os::unix::fs::PermissionsExt;

    let cwd = f.root.join("opencode-topic-cwd");
    let data = f.root.join("opencode-topic-data");
    let config = f.root.join("opencode-topic-config");
    let outputs = f.root.join("opencode-topic-outputs");
    for path in [&cwd, &data, &config, &outputs] {
        std::fs::create_dir_all(path).unwrap();
    }
    let executable = f.root.join("fake-opencode-topic");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opencode-acp.py"),
        &executable,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();

    let token = "topic-capability-test-value";
    let mcp_env_vars = vec![
        "FORMLESS_TOPIC_CAPABILITY".into(),
        "FORMLESS_DATA_DIR".into(),
        "FORMLESS_CONFIG_DIR".into(),
        "FORMLESS_TOPIC_OUTPUTS_DIR".into(),
    ];
    let mut environment = BTreeMap::from([
        ("FORMLESS_TOPIC_CAPABILITY".into(), OsString::from(token)),
        (
            "FORMLESS_TOPIC_ATTEMPT_ID".into(),
            OsString::from("dispatch-attempt"),
        ),
        ("FORMLESS_DATA_DIR".into(), data.clone().into_os_string()),
        (
            "FORMLESS_CONFIG_DIR".into(),
            config.clone().into_os_string(),
        ),
        (
            "FORMLESS_TOPIC_OUTPUTS_DIR".into(),
            outputs.clone().into_os_string(),
        ),
        ("PWD".into(), OsString::from("/wrong/pwd")),
        (
            "QUESTION_FIXTURE".into(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/opencode-question.json")
                .into_os_string(),
        ),
        (
            "MCP_CAPTURE_FILE".into(),
            f.root.join("opencode-mcp-capture.json").into_os_string(),
        ),
        (
            "TOPIC_PROMPT_MARKER".into(),
            OsString::from("topic-dispatch-prompt-marker"),
        ),
    ]);
    if let Some(path) = cancel_probe {
        environment.insert("CANCEL_PROBE".into(), path.into_os_string());
    }
    HarnessRequest {
        kind: HarnessKind::OpenCode,
        acp_agent: None,
        model: None,
        cwd,
        prompt: "topic-dispatch-prompt-marker".into(),
        access: StepAccess::Write,
        mcp_command: std::env::current_exe().unwrap(),
        mcp_args: vec![
            "--profile".into(),
            "topic-test".into(),
            "topic-mcp".into(),
            "--attempt".into(),
            "dispatch-attempt".into(),
        ],
        mcp_env_vars,
        executable: Some(executable),
        environment: std::mem::take(&mut environment),
    }
}

#[tokio::test]
async fn topic_dispatch_routes_opencode_request_and_typed_interactions() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let request = request(&f, None);
    let handler = Arc::new(TopicInteractions::default());
    let cancel = CancellationToken::new();
    let attempt = attempt();
    let result = f
        .daemon()
        .launch_topic_adapter(
            &attempt,
            "topic-capability-test-value",
            &request,
            handler.clone(),
            &cancel,
        )
        .await
        .unwrap();

    assert_eq!(result.message, "done");
    assert_eq!(handler.permissions.lock().unwrap().len(), 1);
    assert_eq!(handler.permissions.lock().unwrap()[0].title, "execute");
    assert_eq!(handler.questions.lock().unwrap().len(), 1);
    assert!(matches!(
        handler.updates.lock().unwrap().as_slice(),
        [HarnessUpdate::Message { content }] if content == "done"
    ));
    let bridge: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.root.join("opencode-mcp-capture.json")).unwrap())
            .unwrap();
    assert_eq!(bridge["name"], "formless");
    assert_eq!(
        bridge["command"],
        request.mcp_command.to_string_lossy().as_ref()
    );
    assert_eq!(bridge["args"], serde_json::json!(request.mcp_args.clone()));
    assert_eq!(
        bridge["env"],
        serde_json::json!([
            {"name":"FORMLESS_TOPIC_CAPABILITY","value":"topic-capability-test-value"},
            {"name":"FORMLESS_DATA_DIR","value":request.environment[std::ffi::OsStr::new("FORMLESS_DATA_DIR")].to_string_lossy()},
            {"name":"FORMLESS_CONFIG_DIR","value":request.environment[std::ffi::OsStr::new("FORMLESS_CONFIG_DIR")].to_string_lossy()},
            {"name":"FORMLESS_TOPIC_OUTPUTS_DIR","value":request.environment[std::ffi::OsStr::new("FORMLESS_TOPIC_OUTPUTS_DIR")].to_string_lossy()}
        ])
    );
}

#[tokio::test]
async fn topic_dispatch_forwards_cancellation_to_opencode_acp() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let probe = f.root.join("opencode-cancel-probe");
    let request = request(&f, Some(probe.clone()));
    let handler = Arc::new(TopicInteractions::default());
    let cancel = CancellationToken::new();
    let daemon = f.daemon().clone();
    let attempt = attempt();
    let child_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        daemon
            .launch_topic_adapter(
                &attempt,
                "topic-capability-test-value",
                &request,
                handler,
                &child_cancel,
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while std::fs::read_to_string(&probe).unwrap_or_default() != "ready" {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fake ACP executable did not start the prompt");
    cancel.cancel();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(probe).unwrap(), "cancelled");
}
