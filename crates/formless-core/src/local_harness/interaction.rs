use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tokio_util::sync::CancellationToken;

use super::{
    acp, acp_agents, antigravity, opencode, validate_acp_agent, HarnessKind, HarnessRequest,
    HarnessResult, InteractionHandler,
};

/// Runs an interactive ACP harness in a fresh, attempt-scoped session.
pub async fn run(
    request: HarnessRequest,
    handler: Arc<dyn InteractionHandler>,
    cancel: CancellationToken,
) -> Result<HarnessResult> {
    validate_acp_agent(Some(request.kind), request.acp_agent.as_deref())?;
    if !matches!(
        request.kind,
        HarnessKind::Acp | HarnessKind::Antigravity | HarnessKind::OpenCode
    ) {
        bail!("{:?} interactive adapter not integrated yet", request.kind);
    }
    if cancel.is_cancelled() {
        bail!("harness cancelled before launch");
    }
    if request.kind == HarnessKind::OpenCode {
        return opencode::run(&request, handler, &cancel).await;
    }
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut adapter = acp::Interactions {
            handler: handler.as_ref(),
            runtime,
        };
        match request.kind {
            HarnessKind::Acp => acp_agents::run(
                request.acp_agent.as_deref().context("missing acp_agent")?,
                &request,
                &mut adapter,
                &cancel,
            ),
            HarnessKind::Antigravity => antigravity::run(&request, &mut adapter, &cancel),
            _ => unreachable!(),
        }
    })
    .await
    .context("harness worker failed")?
}

#[cfg(test)]
#[path = "interaction_tests.rs"]
mod tests;
