use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use super::{
    acp::{PermissionHandler, PermissionRequest},
    acp_agents, antigravity, validate_acp_agent, HarnessKind, HarnessPermissionRequest,
    HarnessRequest, HarnessResult, HarnessUpdate, InteractionHandler,
};

/// Runs an interactive ACP harness in a fresh, attempt-scoped session.
pub async fn run(
    request: HarnessRequest,
    handler: Arc<dyn InteractionHandler>,
    cancel: CancellationToken,
) -> Result<HarnessResult> {
    validate_acp_agent(Some(request.kind), request.acp_agent.as_deref())?;
    if !matches!(request.kind, HarnessKind::Acp | HarnessKind::Antigravity) {
        bail!("{:?} interactive adapter not integrated yet", request.kind);
    }
    if cancel.is_cancelled() {
        bail!("harness cancelled before launch");
    }
    let runtime = Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut adapter = Adapter {
            handler,
            runtime,
            cancel: cancel.clone(),
            error: None,
        };
        let result = match request.kind {
            HarnessKind::Acp => acp_agents::run(
                request.acp_agent.as_deref().context("missing acp_agent")?,
                &request,
                &mut adapter,
                &cancel,
            ),
            HarnessKind::Antigravity => antigravity::run(&request, &mut adapter, &cancel),
            _ => unreachable!(),
        };
        match adapter.error {
            Some(error) => Err(error),
            None => result,
        }
    })
    .await
    .context("harness worker failed")?
}

struct Adapter {
    handler: Arc<dyn InteractionHandler>,
    runtime: Handle,
    cancel: CancellationToken,
    error: Option<anyhow::Error>,
}

impl PermissionHandler for Adapter {
    fn decide(&mut self, request: &PermissionRequest<'_>) -> Option<String> {
        let options = match serde_json::from_value(Value::Array(request.options.to_vec())) {
            Ok(options) => options,
            Err(error) => {
                self.error = Some(error.into());
                self.cancel.cancel();
                return None;
            }
        };
        let permission = HarnessPermissionRequest {
            title: request.tool_call["title"]
                .as_str()
                .unwrap_or("Tool call")
                .into(),
            kind: request.tool_call["kind"].as_str().unwrap_or("other").into(),
            raw_input: request.tool_call.get("rawInput").cloned(),
            options,
        };
        let choice = self.runtime.block_on(async {
            tokio::select! {
                biased;
                _ = request.cancel.cancelled() => None,
                answer = self.handler.request_permission(permission, request.cancel) => answer,
            }
        });
        choice.or_else(|| {
            ["reject_once", "reject_always"]
                .into_iter()
                .find_map(|kind| {
                    request
                        .options
                        .iter()
                        .find(|option| option["kind"] == kind)?
                        .get("optionId")?
                        .as_str()
                        .map(str::to_owned)
                })
        })
    }

    fn on_update(&mut self, params: &Value) {
        let update = &params["update"];
        let text = || {
            update["content"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        };
        let mapped = match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") if update["content"]["type"] == "text" => {
                HarnessUpdate::Message { content: text() }
            }
            Some("agent_thought_chunk") if update["content"]["type"] == "text" => {
                HarnessUpdate::Thought { content: text() }
            }
            Some("tool_call" | "tool_call_update") => HarnessUpdate::ToolCall {
                name: update["title"]
                    .as_str()
                    .or_else(|| update["toolCallId"].as_str())
                    .unwrap_or("Tool call")
                    .into(),
                input: update.get("rawInput").cloned().unwrap_or(Value::Null),
            },
            Some("plan") => HarnessUpdate::Plan {
                content: update.get("entries").unwrap_or(&Value::Null).to_string(),
            },
            _ => return,
        };
        if let Err(error) = self.handler.on_update(mapped) {
            self.error = Some(error);
            self.cancel.cancel();
        }
    }
}

#[cfg(test)]
#[path = "interaction_tests.rs"]
mod tests;
