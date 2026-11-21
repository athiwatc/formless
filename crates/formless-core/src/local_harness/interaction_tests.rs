use std::sync::Mutex;

use formless_protocol::{Question, QuestionAnswer, StepAccess};
use serde_json::json;

use super::*;
use crate::local_harness::InteractionFuture;

#[derive(Default)]
struct Handler {
    choice: Option<String>,
    permissions: Mutex<Vec<HarnessPermissionRequest>>,
    updates: Mutex<Vec<HarnessUpdate>>,
    fail_updates: bool,
}

impl InteractionHandler for Handler {
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
        _: Vec<Question>,
        _: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<Vec<QuestionAnswer>>> {
        Box::pin(async { None })
    }

    fn on_update(&self, update: HarnessUpdate) -> Result<()> {
        if self.fail_updates {
            bail!("fixture update failed");
        }
        self.updates.lock().unwrap().push(update);
        Ok(())
    }
}

#[test]
fn acp_selector_validation_uses_registry() {
    for id in acp_agents::agent_ids() {
        validate_acp_agent(Some(HarnessKind::Acp), Some(id)).unwrap();
    }
    validate_acp_agent(None, None).unwrap();
    for (kind, id) in [
        (Some(HarnessKind::Acp), None),
        (Some(HarnessKind::Acp), Some("")),
        (Some(HarnessKind::Acp), Some(" ")),
        (Some(HarnessKind::Claude), Some("gemini")),
        (None, Some("gemini")),
    ] {
        assert!(validate_acp_agent(kind, id).is_err());
    }
    let error = validate_acp_agent(Some(HarnessKind::Acp), Some("not-installed-profile"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("Unknown ACP agent `not-installed-profile`"));
    for id in acp_agents::agent_ids() {
        assert!(error.contains(id));
    }
}

#[cfg(unix)]
mod fixture {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf};

    use super::*;

    struct Agent(PathBuf);

    impl Drop for Agent {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn options() -> Value {
        json!([
            {"optionId":"yes","kind":"allow_always","name":"Allow for this attempt","_meta":{"vendor":"opaque"}},
            {"optionId":"no","kind":"reject_once","name":"Reject","extra":[1,2]}
        ])
    }

    impl Agent {
        fn new(access: StepAccess) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "formless-acp-topic-fixture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            let agent = Self(path);
            let captured: Value =
                serde_json::from_str(include_str!("acp_agents/captured.json")).unwrap();
            let mut script = String::from("#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > argv\n");
            let mut exchange = |value: Value| {
                script.push_str(&format!("IFS= read -r line\nprintf '%s\\n' '{value}'\n"));
            };
            exchange(captured["gemini"]["initialize"].clone());
            exchange(
                json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"fixture","configOptions":[
                {"id":"mode","options":[{"value":"plan"}]}]}}),
            );
            let prompt_id = if access == StepAccess::ReadOnly {
                exchange(json!({"jsonrpc":"2.0","id":3,"result":{}}));
                4
            } else {
                3
            };
            script.push_str("IFS= read -r line\n");
            for update in [
                json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}),
                json!({"sessionUpdate":"tool_call","toolCallId":"call","title":"Read file","rawInput":{"path":"input"}}),
                json!({"sessionUpdate":"plan","entries":[{"content":"Inspect input","status":"pending","priority":"high"}]}),
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"answer"}}),
            ] {
                let notification = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"fixture","update":update}});
                script.push_str(&format!("printf '%s\\n' '{notification}'\n"));
            }
            let permission = json!({"jsonrpc":"2.0","id":90,"method":"session/request_permission","params":{
                "sessionId":"fixture","toolCall":{"title":"Read file","kind":"read","rawInput":{"path":"input"}},"options":options()}});
            let done = json!({"jsonrpc":"2.0","id":prompt_id,"result":{"stopReason":"end_turn"}});
            script.push_str(&format!("printf '%s\\n' '{permission}'\nIFS= read -r line\nprintf '%s\\n' \"$line\" > permission-reply\nprintf '%s\\n' '{done}'\n"));
            fs::write(agent.0.join("agent"), script).unwrap();
            fs::set_permissions(agent.0.join("agent"), fs::Permissions::from_mode(0o700)).unwrap();
            agent
        }

        fn request(&self, access: StepAccess) -> HarnessRequest {
            HarnessRequest {
                kind: HarnessKind::Acp,
                acp_agent: Some("gemini".into()),
                model: None,
                cwd: self.0.clone(),
                prompt: "inspect".into(),
                access,
                mcp_command: std::env::current_exe().unwrap(),
                mcp_args: vec![],
                mcp_env_vars: vec![],
                executable: Some(self.0.join("agent")),
                environment: BTreeMap::new(),
            }
        }
    }

    #[tokio::test]
    async fn acp_dispatch_maps_updates_and_preserves_permission_options() {
        for access in [StepAccess::Write, StepAccess::ReadOnly] {
            for choice in [Some("yes".to_owned()), None] {
                let agent = Agent::new(access);
                let handler = Arc::new(Handler {
                    choice: choice.clone(),
                    ..Default::default()
                });
                let result = run(
                    agent.request(access),
                    handler.clone(),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
                assert_eq!(result.message, "answer");
                let permission: Value = serde_json::from_str(
                    &fs::read_to_string(agent.0.join("permission-reply")).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    permission["result"]["outcome"]["optionId"],
                    choice.as_deref().unwrap_or("no")
                );
                let permissions = handler.permissions.lock().unwrap();
                assert_eq!(permissions.len(), 1);
                assert_eq!(permissions[0].title, "Read file");
                assert_eq!(permissions[0].kind, "read");
                assert_eq!(permissions[0].raw_input, Some(json!({"path":"input"})));
                let mut forwarded = serde_json::to_value(&permissions[0].options).unwrap();
                for option in forwarded.as_array_mut().unwrap() {
                    let object = option.as_object_mut().unwrap();
                    let id = object.remove("option_id").unwrap();
                    object.insert("optionId".into(), id);
                }
                assert_eq!(forwarded, options());
                let updates = handler.updates.lock().unwrap();
                assert!(
                    matches!(&updates[0], HarnessUpdate::Thought { content } if content == "thinking")
                );
                assert!(
                    matches!(&updates[1], HarnessUpdate::ToolCall { name, input } if name == "Read file" && input == &json!({"path":"input"}))
                );
                assert!(
                    matches!(&updates[2], HarnessUpdate::Plan { content } if serde_json::from_str::<Value>(content).unwrap()[0]["content"] == "Inspect input")
                );
                assert!(
                    matches!(&updates[3], HarnessUpdate::Message { content } if content == "answer")
                );
                let argv = fs::read_to_string(agent.0.join("argv")).unwrap();
                assert_eq!(
                    argv.contains("--approval-mode\nplan"),
                    access == StepAccess::ReadOnly
                );
            }
        }
    }

    #[tokio::test]
    async fn acp_dispatch_rejects_invalid_selectors_before_launch() {
        let agent = Agent::new(StepAccess::Write);
        for id in [None, Some(""), Some("unknown-agent")] {
            let mut request = agent.request(StepAccess::Write);
            request.acp_agent = id.map(str::to_owned);
            assert!(run(
                request,
                Arc::new(Handler::default()),
                CancellationToken::new()
            )
            .await
            .is_err());
        }
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(run(
            agent.request(StepAccess::Write),
            Arc::new(Handler::default()),
            cancel
        )
        .await
        .is_err());
        assert!(!agent.0.join("argv").exists());
    }

    #[tokio::test]
    async fn acp_dispatch_propagates_update_errors() {
        let agent = Agent::new(StepAccess::Write);
        let handler = Arc::new(Handler {
            fail_updates: true,
            ..Default::default()
        });
        let error = run(
            agent.request(StepAccess::Write),
            handler,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "fixture update failed");
    }
}
