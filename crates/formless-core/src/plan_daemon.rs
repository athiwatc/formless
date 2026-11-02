//! The Session checklist is owned by its current main-agent Run.
use super::*;

/// A separate publication guard outlives RunCtl removal. It cannot strand a
/// Session when capture or event delivery unwinds, or clear a newer Run's gate.
pub(super) struct RunPublication<'a> {
    daemon: &'a Daemon,
    key: (String, String),
}
impl Drop for RunPublication<'_> {
    fn drop(&mut self) {
        let mut publications = self.daemon.run_publications.locked();
        if let Some(count) = publications.get_mut(&self.key) {
            if *count > 1 {
                *count -= 1;
            } else {
                publications.remove(&self.key);
            }
        }
    }
}

impl Daemon {
    pub(super) fn begin_run_publication<'a>(
        &'a self,
        session: &str,
        run: &str,
    ) -> RunPublication<'a> {
        let key = (session.into(), run.into());
        *self
            .run_publications
            .locked()
            .entry(key.clone())
            .or_default() += 1;
        RunPublication { daemon: self, key }
    }
    pub(super) fn session_is_publishing(&self, session: &str) -> bool {
        self.run_publications
            .locked()
            .keys()
            .any(|(owner, _)| owner == session)
    }

    pub(super) fn update_plan_impl(
        &self,
        session_id: &str,
        run_id: &str,
        agent_depth: usize,
        args: &Value,
    ) -> Result<String> {
        if agent_depth != 0 {
            anyhow::bail!("Sub-agents cannot replace the Session plan. Return suggested steps to the main agent instead.");
        }
        let items: Vec<PlanItem> = serde_json::from_value(args["items"].clone())
            .context("items must be a list of {content, status}")?;
        if items.len() > toolreg::MAX_PLAN_ITEMS {
            anyhow::bail!("at most {} steps", toolreg::MAX_PLAN_ITEMS);
        }
        if items
            .iter()
            .filter(|item| item.status == "in_progress")
            .count()
            > 1
        {
            anyhow::bail!("at most one plan step may be in_progress");
        }
        let mut items = items;
        for item in &mut items {
            if !matches!(
                item.status.as_str(),
                "pending" | "in_progress" | "completed"
            ) {
                anyhow::bail!(
                    "status must be pending, in_progress or completed, not {:?}",
                    item.status
                );
            }
            item.content = item
                .content
                .trim()
                .chars()
                .take(toolreg::MAX_PLAN_TEXT)
                .collect();
            if item.content.is_empty() {
                anyhow::bail!("a step needs content");
            }
        }
        // Admission serializes a new Run with this final ownership check/write.
        // A still-running background call has no authority after its Run ends.
        let _admission = self.forecast_admission.locked();
        if !self.runs.locked().get(session_id).is_some_and(|run| {
            run.run_id == run_id
                && !run.cancel_flag.load(Ordering::SeqCst)
                && !run.cancel.is_cancelled()
        }) {
            anyhow::bail!("update_plan requires the Session's current active main-agent Run; stale or background calls cannot replace its plan");
        }
        let plan_state = self.with_store(|store| {
            store.transaction(|store| {
                store.set_run_plan(session_id, run_id, &items)?;
                store.session_plan_state(session_id)
            })
        })?;
        self.broadcast(Event::PlanUpdated {
            session_id: session_id.into(),
            items: items.clone(),
            plan_state,
        });
        let done = items
            .iter()
            .filter(|item| item.status == "completed")
            .count();
        Ok(if items.is_empty() {
            "plan cleared".into()
        } else {
            format!("plan updated ({done}/{} done)", items.len())
        })
    }

    /// Status comes from durable Run state, including restart interruption;
    /// publication never rewrites or invents the checklist's item statuses.
    pub(super) fn publish_session_plan(&self, session_id: &str) {
        match self.with_reader(|store| store.session_plan_snapshot(session_id)) {
            Ok((items, plan_state)) if !items.is_empty() || plan_state.is_some() => {
                self.broadcast(Event::PlanUpdated {
                    session_id: session_id.into(),
                    items,
                    plan_state,
                });
            }
            Ok(_) => (),
            Err(error) => tracing::warn!("could not publish Session plan lifecycle: {error:#}"),
        }
    }
}

#[cfg(test)]
#[path = "plan_daemon_tests.rs"]
mod tests;
