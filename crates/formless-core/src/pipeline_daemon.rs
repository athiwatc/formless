//! Run-only pipeline admission and identity-bound replay. No Session settings
//! are edited, and no connection handling or output queue policy lives here.
use super::*;
use crate::pipeline::CompiledRunOptions;
use crate::pipeline_policy::{Registry, RunPolicy};
use formless_protocol::RunOptions;

pub(super) struct CachedPipeline {
    digest: String,
    sent: SentEntry,
}
impl Daemon {
    pub(super) fn pipeline_policy(&self, run: &str) -> Result<Option<Arc<RunPolicy>>> {
        let live = self.pipeline_policies.locked().lookup(run);
        match live {
            Some(policy) => Ok(Some(policy)),
            None if self.with_reader(|st| st.pipeline_policy_marker(run))?.is_some() => {
                Err(RequestFailure::invalid("Run-only pipeline policy is no longer available; submit a new pipeline Run with its options instead of resuming").into())
            }
            None => Ok(None),
        }
    }
    pub(super) fn policy_from_registry(
        registry: &StdMutex<Registry>,
        store: &StdMutex<Store>,
        run: &str,
    ) -> Result<Option<Arc<RunPolicy>>> {
        if let Some(policy) = registry.locked().lookup(run) {
            return Ok(Some(policy));
        }
        if store.locked().pipeline_policy_marker(run)?.is_some() {
            return Err(RequestFailure::invalid("Run-only pipeline policy is unavailable; resubmit a new pipeline Run with its options").into());
        }
        Ok(None)
    }
    pub(super) async fn start_pipeline_message(
        self: &Arc<Self>,
        session_id: String,
        content: String,
        options: RunOptions,
        client_msg_id: Option<String>,
    ) -> Result<Value> {
        // Also validate remote callers before claiming a replay identity.
        let policy = Arc::new(RunPolicy::new(
            CompiledRunOptions::compile(options)
                .map_err(|e| RequestFailure::invalid(e.to_string()))?,
        ));
        let Some(intent) = client_msg_id else {
            return self.start_pipeline_now(session_id, content, policy);
        };
        if intent.is_empty() || intent.len() > 128 {
            return Err(RequestFailure::invalid(
                "pipeline client_msg_id must contain 1–128 UTF-8 bytes",
            )
            .into());
        }
        let who = current_client();
        let key = (who.as_ref().map(|(id, _)| *id), session_id.clone(), intent);
        let digest = crate::forecast::hash(&(&content, policy.options.options()));
        let (mut receiver, owner) = {
            let mut keys = self.pipeline_keys.locked();
            keys.retain(|_, entry| {
                entry.sent.outcome.borrow().is_none() || entry.sent.at.elapsed().as_secs() < 600
            });
            if let Some(entry) = keys.get(&key) {
                if entry.digest != digest {
                    return Err(RequestFailure::invalid(
                        "pipeline send identity was already used for different content or options",
                    )
                    .into());
                }
                (entry.sent.outcome.clone(), None)
            } else {
                if keys.len() >= 256 {
                    return Err(RequestFailure::busy(
                        "pipeline replay capacity is full; wait before submitting another intent",
                    )
                    .into());
                }
                let (tx, rx) = tokio::sync::watch::channel(None);
                keys.insert(
                    key.clone(),
                    CachedPipeline {
                        digest,
                        sent: SentEntry {
                            at: std::time::Instant::now(),
                            outcome: rx.clone(),
                        },
                    },
                );
                (rx, Some(tx))
            }
        };
        if let Some(tx) = owner {
            let this = self.clone();
            tokio::spawn(async move {
                use futures_util::FutureExt;
                let execute = async {
                    match std::panic::AssertUnwindSafe(async {
                        this.start_pipeline_now(session_id, content, policy)
                    })
                    .catch_unwind()
                    .await
                    {
                        Ok(result) => result.map_err(CachedSendFailure::capture),
                        Err(_) => Err(CachedSendFailure::capture(
                            RequestFailure::internal("internal error while admitting pipeline Run")
                                .into(),
                        )),
                    }
                };
                let result = match who {
                    Some(who) => CLIENT.scope(who, execute).await,
                    None => execute.await,
                };
                if let Some(entry) = this.pipeline_keys.locked().get_mut(&key) {
                    entry.sent.at = std::time::Instant::now();
                }
                let _ = tx.send(Some(result));
            });
        }
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result.map_err(CachedSendFailure::into_error);
            }
            receiver
                .changed()
                .await
                .context("pipeline admission worker ended without an outcome")?;
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn validate_pipeline_answer(
        &self,
        policy: &Arc<RunPolicy>,
        provider: &Provider,
        context: &[Value],
        model: &str,
        effort: Option<&str>,
        result: &mut crate::provider::ChatResult,
        session: &str,
        run: &str,
        cfg: &Config,
        cancel: &CancellationToken,
    ) -> Result<()> {
        use crate::provider::StructuredOutputError;
        let failure = match policy.options.validate_final(&result.content) {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };
        if !matches!(
            failure.downcast_ref::<StructuredOutputError>(),
            Some(StructuredOutputError::InvalidJson | StructuredOutputError::SchemaViolation)
        ) {
            return Err(anyhow!("schema: {failure}"));
        }
        if !policy.claim_repair() {
            return Err(anyhow!(
                "schema: final answer mismatch; this Run already used its one repair attempt"
            ));
        }
        let mut messages = context.to_vec();
        messages.push(json!({"role":"user","content":crate::provenance::frame("invalid_output", &[], &result.content)}));
        messages.push(json!({"role":"user","content":"The previous candidate failed output validation. Return one corrected JSON value satisfying the requested schema. Do not call Tools, include Markdown, or add explanatory prose."}));
        self.broadcast(Event::Note {
            session_id: Some(session.into()),
            text: "Final output did not match its schema; making the one paid repair attempt."
                .into(),
        });
        let routing_call =
            crate::routing_observation::Call::responsible_step(self.routing_recorder.call_sink());
        let repair = crate::routing_observation::scope(
            routing_call.clone(),
            counted(
                "schema-repair",
                session,
                run,
                provider.completion_for_validation(
                    &messages,
                    model,
                    effort,
                    policy.options.completion(),
                ),
            ),
        );
        let repaired = tokio::select! {
            _ = cancel.cancelled() => Err(anyhow!("interrupted")),
            repaired = tokio::time::timeout(std::time::Duration::from_secs(120), repair) => repaired.context("schema repair exceeded its 120-second deadline").and_then(|result| result),
        };
        let repaired = match repaired {
            Ok(repaired) => repaired,
            Err(error) => {
                routing_call.finish(Self::routing_error(
                    &routing_call,
                    &error,
                    cancel.is_cancelled(),
                ));
                drop(routing_call);
                self.flush_routing()?;
                return Err(error);
            }
        };
        // The physical success hook has already persisted known charges even
        // if terminal/JSON/schema validation below fails.
        if let Ok(usage) = self.usage_info(None, Some(run), cfg) {
            self.broadcast(Event::RunUsage {
                session_id: session.into(),
                run_id: run.into(),
                usage,
            });
        }
        let (repaired, terminal) = repaired;
        let validation = if let Some(error) = terminal {
            Err(anyhow!("schema: repair failed: {error}"))
        } else if repaired.output_limited {
            Err(anyhow!("schema: repair output was incomplete"))
        } else {
            policy
                .options
                .validate_final(&repaired.text)
                .map_err(|error| {
                    if matches!(
                        error.downcast_ref::<StructuredOutputError>(),
                        Some(
                            StructuredOutputError::InvalidJson
                                | StructuredOutputError::SchemaViolation
                        )
                    ) {
                        routing_call.claim_contract(crate::routing_observation::Identity::new(
                            provider.name(),
                            provider.settings(),
                            &repaired.model_used,
                        ));
                    }
                    anyhow!("schema: final answer still mismatches after one repair: {error}")
                })
                .map(|_| ())
        };
        use crate::routing_observation::{Identity, Neutral, Outcome};
        let observed = if cancel.is_cancelled() {
            Outcome::Neutral(Neutral::Cancelled)
        } else if let Some(identity) = routing_call.contract_identity() {
            Outcome::ContractFailure(identity)
        } else if validation.is_ok() {
            Outcome::Success(Identity::new(
                provider.name(),
                provider.settings(),
                &repaired.model_used,
            ))
        } else {
            Outcome::Neutral(Neutral::Unknown)
        };
        routing_call.finish(observed);
        drop(routing_call);
        self.flush_routing()?;
        validation?;
        result.content = repaired.text;
        result.model_used = repaired.model_used;
        result.stop_reason = None;
        result.stop_detail = None;
        result.truncated = false;
        Ok(())
    }
    fn start_pipeline_now(
        self: &Arc<Self>,
        session_id: String,
        content: String,
        policy: Arc<RunPolicy>,
    ) -> Result<Value> {
        let _admission = self.forecast_admission.locked();
        let _settings = self.config_guard();
        if self.runs.locked().contains_key(&session_id) {
            return Err(RequestFailure::busy(
                "Session has an active Run; pipeline options cannot steer or queue onto it",
            )
            .into());
        }
        self.pipeline_policies
            .locked()
            .ensure_capacity()
            .map_err(|e| RequestFailure::busy(e.to_string()))?;
        let cfg = self.config()?;
        let input = ForecastInput {
            session_id: Some(session_id.clone()),
            draft: content.clone(),
            model: policy.options.options().model.clone(),
            options: policy.options.options().clone(),
            ..Default::default()
        };
        let (message, run, admission) = self.with_store(|st| {
            st.transaction(|st| {
                st.get_session(&session_id)?
                    .context(RequestFailure::not_found("session not found"))?;
                let forecast = self.consented_forecast(st, &cfg, &input)?;
                if crate::runs::is_correction(&content) {
                    st.mark_last_run_followup(&session_id, "corrected")?;
                }
                let message = Self::append_user_message_store(st, &session_id, &content, None)?;
                let mut admission = self.prepare_run_admission(st, &cfg, &session_id, &forecast)?;
                let owned = Arc::get_mut(&mut admission)
                    .context("new pipeline admission already shared")?;
                owned.pipeline = Some(policy.clone());
                if let Some(effort) = &policy.options.options().reasoning_effort {
                    owned.effort = Some(effort.clone());
                }
                let run = st.create_run(&session_id)?;
                st.record_pipeline_policy(&run, &policy.marker())?;
                st.record_forecast(&run, &forecast)?;
                self.pipeline_policies.locked().insert(&run, &policy)?;
                Ok((message, run, admission))
            })
        })?;
        self.broadcast(Event::MessageAppended {
            session_id: session_id.clone(),
            message,
        });
        self.launch_prepared_run(session_id, run.clone(), admission)?;
        Ok(json!({"started":true,"run_id":run}))
    }
}
