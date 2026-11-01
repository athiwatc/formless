//! Explicit persistence boundaries; no database work from an accounting Drop.
use super::*;
impl Daemon {
    pub(crate) fn flush_routing(&self) -> Result<()> {
        let Some(batch) = self.routing_recorder.try_take() else {
            return Ok(());
        };
        if batch.dropped > 0 {
            self.broadcast(Event::Note{session_id:None,text:format!("Routing observations lost {} receipts because the bounded queue was full; affected incomplete tasks are not scored.",batch.dropped)});
        }
        if batch.receipts.is_empty() {
            return Ok(());
        }
        match self.with_store(|st| st.record_routing(&batch.receipts)) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.routing_recorder.retry(batch);
                self.broadcast(Event::Note{session_id:None,text:format!("Could not save routing observations; retained a bounded retry batch: {error:#}")});
                Err(error)
            }
        }
    }
    pub(super) fn resolve_model_named(
        &self,
        requested: &str,
        cfg: &Config,
        provider: &str,
        complex: bool,
    ) -> Result<String> {
        // Callers resolve aliases once when constructing their target. Do not
        // turn chained aliases into a different model here.
        if !ProviderCfg::is_auto_model(requested) {
            return Ok(requested.to_string());
        }
        self.flush_routing()?;
        let mut choice=self.with_reader(|st|st.routing_auto_choice(cfg,provider,complex))?
            .context("provider.model is auto but no routing candidates are configured; set routing_models (or fallback_models)")?;
        choice.explanation["kind"] = json!(if complex {
            "auto (complex task)"
        } else {
            "auto (simple task)"
        });
        self.note_routing("", choice.explanation);
        Ok(choice.model)
    }
}

impl Daemon {
    pub(super) fn routing_error(
        call: &crate::routing_observation::Call,
        error: &anyhow::Error,
        cancelled: bool,
    ) -> crate::routing_observation::Outcome {
        use crate::routing_observation::{Neutral, Outcome};
        if cancelled {
            return Outcome::Neutral(Neutral::Cancelled);
        }
        if error
            .downcast_ref::<RequestFailure>()
            .is_some_and(|e| e.code == codes::BUDGET)
        {
            return Outcome::Neutral(Neutral::Budget);
        }
        if error
            .downcast_ref::<crate::provider::UsageAccountingFailure>()
            .is_some()
        {
            return Outcome::Neutral(Neutral::Accounting);
        }
        call.contract_identity()
            .map_or(Outcome::Neutral(Neutral::Unknown), Outcome::ContractFailure)
    }
    pub(super) fn finish_chat_observation(
        call: &crate::routing_observation::Call,
        provider: &Provider,
        result: &Result<crate::provider::ChatResult>,
        cancelled: bool,
    ) {
        use crate::routing_observation::{Identity, Neutral, Outcome};
        let outcome = match result {
            Ok(r)
                if !cancelled
                    && !r.truncated
                    && !matches!(
                        r.stop_reason.as_deref(),
                        Some(
                            "model_context_window_exceeded"
                                | "max_tokens"
                                | "length"
                                | "content_filter"
                                | "refusal"
                                | "pause_turn"
                        )
                    ) =>
            {
                Outcome::Success(Identity::new(
                    provider.name(),
                    provider.settings(),
                    &r.model_used,
                ))
            }
            Ok(_) => Outcome::Neutral(if cancelled {
                Neutral::Cancelled
            } else {
                Neutral::Unknown
            }),
            Err(error) => Self::routing_error(call, error, cancelled),
        };
        call.finish(outcome);
    }
    pub(super) fn routing_task_for_run(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Option<Arc<crate::routing_task_observation::Task>> {
        let admitted = { self.run_admissions.locked().get(run_id).cloned() };
        let cfg = admitted
            .as_ref()
            .map(|a| a.cfg.clone())
            .or_else(|| self.config().ok())?;
        let once = { self.next_run_model.locked().get(session_id).cloned() };
        let model = admitted
            .as_ref()
            .map(|a| a.requested.clone())
            .or(once)
            .unwrap_or_else(|| self.session_model_effort(session_id, &cfg).0);
        let selected = if admitted.is_some() {
            model
        } else {
            cfg.provider.resolve_alias(&model)
        };
        crate::routing_task_observation::Task::with_recorder(
            selected.strip_prefix("role:").unwrap_or(""),
            None,
            self.routing_recorder.clone(),
        )
    }
}
