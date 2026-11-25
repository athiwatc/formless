use super::*;
use crate::local_harness::{
    self, HarnessPermissionRequest, HarnessRequest, HarnessResult, HarnessUpdate,
    InteractionFuture, InteractionHandler,
};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

const NO_ATTACHMENTS: &str =
    "Main Chat harnesses cannot accept attachments; detach them or select a provider";
const UNSUPPORTED_RUN: &str =
    "Main Chat harnesses support new text messages only; send a new message or select a provider";
const TRANSCRIPT_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn main_harness_prompts_close_on_drop_and_answered_clarifications_enter_context() {
        let root = std::env::temp_dir().join(format!("fl-main-prompts-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let session_id = daemon.with_store(Store::main_chat).unwrap().id;
        let handler = MainInteraction {
            daemon: daemon.clone(),
            session_id: session_id.clone(),
            harness: MainChatHarness {
                kind: HarnessKind::Muse,
                agent: None,
                executable: PathBuf::new(),
                argv0: None,
            },
        };
        let cancel = CancellationToken::new();
        let mut permission = handler.request_permission(
            HarnessPermissionRequest {
                title: "Trust this workspace?".into(),
                kind: "trust".into(),
                raw_input: None,
                options: vec![formless_protocol::HarnessPermissionOption {
                    option_id: "allow".into(),
                    name: "Allow once".into(),
                    kind: formless_protocol::HarnessPermissionOptionKind::AllowOnce,
                }],
            },
            &cancel,
        );
        assert!(futures_util::poll!(&mut permission).is_pending());
        assert_eq!(daemon.harness_permissions.locked().len(), 1);
        drop(permission);
        assert!(daemon.harness_permissions.locked().is_empty());
        assert!(daemon.pending_attention.locked().is_empty());

        let question = Question {
            question: "Which city?".into(),
            header: None,
            options: vec![],
            multi_select: false,
        };
        let mut abandoned = handler.ask_question(vec![question.clone()], &cancel);
        assert!(futures_util::poll!(&mut abandoned).is_pending());
        assert_eq!(daemon.questions.locked().len(), 1);
        drop(abandoned);
        assert!(daemon.questions.locked().is_empty());
        assert!(daemon.pending_attention.locked().is_empty());

        let mut answered = handler.ask_question(vec![question], &cancel);
        assert!(futures_util::poll!(&mut answered).is_pending());
        let question_id = daemon.questions.locked().keys().next().unwrap().clone();
        let reply = daemon
            .handle(Request::AnswerQuestion {
                question_id,
                answer: "Bangkok".into(),
                answers: vec![],
            })
            .await
            .unwrap();
        assert_eq!(reply, json!({"answered": true}));
        assert_eq!(answered.await.unwrap()[0].text.as_deref(), Some("Bangkok"));
        assert_eq!(
            daemon
                .with_reader(|st| st.main_harness_turns(&session_id, TRANSCRIPT_BYTES))
                .unwrap(),
            [
                ("assistant".into(), "Which city?".into()),
                ("user".into(), "Bangkok".into())
            ],
        );
        drop(handler);
        drop(daemon);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[derive(Clone)]
pub(super) struct MainChatHarness {
    kind: HarnessKind,
    agent: Option<String>,
    executable: PathBuf,
    argv0: Option<OsString>,
}

impl MainChatHarness {
    fn label(&self) -> String {
        harness_label(self.kind, self.agent.as_deref())
    }
}

fn harness_label(kind: HarnessKind, agent: Option<&str>) -> String {
    agent.map_or_else(|| format!("{kind:?}"), |agent| format!("ACP ({agent})"))
}

fn supports_read_only(kind: HarnessKind, agent: Option<&str>) -> bool {
    kind != HarnessKind::Acp
        || agent
            .and_then(local_harness::acp_agents::spec)
            .is_some_and(|spec| !spec.read_only_modes.is_empty())
}

fn supported_version(availability: &HarnessAvailability) -> bool {
    if availability.kind == HarnessKind::Acp {
        true
    } else {
        availability.version.as_deref().is_some_and(|version| {
            availability.kind != HarnessKind::OpenCode
                || local_harness::opencode::compatible_version(version)
        })
    }
}

pub(crate) fn harness_forecast(
    st: &Store,
    cfg: &Config,
    input: &ForecastInput,
) -> Result<Option<RunForecast>> {
    let MainChatBackend::Harness { harness, acp_agent } =
        st.session_main_chat_backend(input.session_id.as_deref())?
    else {
        return Ok(None);
    };
    if input.edit_message_id.is_some()
        || input.retry
        || input.resume
        || !input.options.is_empty()
        || input.model.is_some()
        || input.cwd.is_some()
    {
        return Err(RequestFailure::invalid(UNSUPPORTED_RUN).into());
    }
    let session_id = input.session_id.as_deref().context("Main Chat not found")?;
    if !input.attachments.is_empty() || !st.pending_attachments(session_id)?.is_empty() {
        return Err(RequestFailure::invalid(NO_ATTACHMENTS).into());
    }
    if cfg.limits.run_usd.is_some()
        || cfg.limits.daily_usd.is_some()
        || cfg.limits.forecast_confirm_usd.is_some()
    {
        return Err(RequestFailure::invalid(
            "Main Chat harnesses cannot enforce Formless provider spending limits or quotes; select a provider",
        ).into());
    }
    let hint = "The local harness manages its own usage; Formless cannot estimate its cost.";
    Ok(Some(RunForecast {
        fingerprint: crate::forecast::hash(&(harness, &acp_agent, input)),
        basis: ForecastBasis {
            provider_key: "main_chat_harness".into(),
            model: harness_label(harness, acp_agent.as_deref()),
            role: "main".into(),
            reasoning_effort: None,
            session_id: input.session_id.clone(),
            cwd_hash: String::new(),
            tools_hash: String::new(),
            prompt_tokens: 0,
        },
        routing_pick: None,
        comparable_runs: 0,
        comparables: Vec::new(),
        steps: None,
        tokens: None,
        cost_usd: None,
        duration_ms: None,
        context_tokens: 0,
        context_source: "harness".into(),
        will_compact: false,
        compaction_probability: None,
        cached_fraction: None,
        unpriced: true,
        warnings: vec![hint.into()],
        confirmation_required: false,
        confirmation_id: None,
        hint: hint.into(),
    }))
}

pub(super) async fn ready_harness(backend: &MainChatBackend) -> Result<Option<MainChatHarness>> {
    let MainChatBackend::Harness { harness, acp_agent } = backend else {
        return Ok(None);
    };
    if !supports_read_only(*harness, acp_agent.as_deref()) {
        return Err(RequestFailure::invalid(
            "This Main Chat harness is not ready: its ACP adapter has no supported read-only mode",
        )
        .into());
    }
    let (availability, executable, argv0) = if *harness == HarnessKind::Antigravity {
        let (availability, executable) = local_harness::antigravity::topic_readiness().await;
        (availability, executable, None)
    } else {
        let ready = local_harness::harness_readiness(*harness, acp_agent.as_deref()).await;
        (
            ready.availability,
            ready.resolved_executable,
            ready.resolved_executable_argv0,
        )
    };
    let readiness_error = || {
        RequestFailure::invalid(format!(
        "Main Chat harness {} is not ready: {}. Install or sign in, or choose a different backend.",
        harness_label(*harness, acp_agent.as_deref()),
        availability.detail.as_deref().unwrap_or("executable or sign-in unavailable"),
    ))
    };
    if !availability.installed
        || availability.signed_in == Some(false)
        || (*harness == HarnessKind::Acp && availability.signed_in != Some(true))
        || !supported_version(&availability)
    {
        return Err(readiness_error().into());
    }
    Ok(Some(MainChatHarness {
        kind: *harness,
        agent: acp_agent.clone(),
        executable: executable.ok_or_else(readiness_error)?,
        argv0,
    }))
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            tracing::warn!("could not remove Main Chat harness scratch: {error}");
        }
    }
}

struct MainInteraction {
    daemon: Arc<Daemon>,
    session_id: String,
    harness: MainChatHarness,
}

pub(super) struct MainPrompt<'a> {
    daemon: &'a Daemon,
    id: &'a str,
    permission: bool,
}

impl<'a> MainPrompt<'a> {
    pub(super) fn permission(daemon: &'a Daemon, id: &'a str) -> Self {
        Self {
            daemon,
            id,
            permission: true,
        }
    }
}

impl Drop for MainPrompt<'_> {
    fn drop(&mut self) {
        if self.permission {
            self.daemon.close_topic_permission(self.id);
        } else {
            self.daemon.questions.locked().remove(self.id);
            let answered_by = self.daemon.question_answered_by.locked().remove(self.id);
            self.daemon.broadcast(Event::UserQuestionClosed {
                question_id: self.id.into(),
                answered_by,
            });
        }
    }
}

impl InteractionHandler for MainInteraction {
    fn request_permission<'a>(
        &'a self,
        request: HarnessPermissionRequest,
        cancel: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<String>> {
        Box::pin(async move {
            let _gate = self
                .daemon
                .topic_prompt_gate(&self.session_id, cancel)
                .await
                .ok()?;
            self.daemon
                .wait_main_harness_permission(
                    &self.session_id,
                    self.harness.kind,
                    self.harness.agent.clone(),
                    request,
                    cancel,
                )
                .await
        })
    }

    fn ask_question<'a>(
        &'a self,
        questions: Vec<Question>,
        cancel: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<Vec<QuestionAnswer>>> {
        Box::pin(async move {
            let _gate = self
                .daemon
                .topic_prompt_gate(&self.session_id, cancel)
                .await
                .ok()?;
            if questions.is_empty() || cancel.is_cancelled() {
                return None;
            }
            let id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) = oneshot::channel();
            self.daemon.questions.locked().insert(id.clone(), tx);
            let pending = MainPrompt {
                daemon: &self.daemon,
                id: &id,
                permission: false,
            };
            let question = questions
                .iter()
                .map(|q| q.question.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let options = questions
                .first()
                .filter(|_| questions.len() == 1)
                .map(|q| q.options.iter().map(|o| o.label.clone()).collect())
                .unwrap_or_default();
            self.daemon.broadcast(Event::UserQuestion {
                user_only: true,
                topic_id: None,
                attempt_id: None,
                question_id: id.clone(),
                session_id: self.session_id.clone(),
                question: question.clone(),
                options,
                questions: questions.clone(),
            });
            let answer = prompt_reply::wait(&self.daemon.questions, &id, rx, cancel, || {}).await;
            drop(pending);
            let (answer, answers) = answer?;
            let answers = if answers.is_empty() && questions.len() == 1 {
                let selected = questions[0]
                    .options
                    .iter()
                    .find(|option| option.label == answer);
                vec![QuestionAnswer {
                    selected: selected
                        .map(|option| vec![option.label.clone()])
                        .unwrap_or_default(),
                    text: selected
                        .is_none()
                        .then_some(answer)
                        .filter(|answer| !answer.trim().is_empty()),
                }]
            } else {
                answers
            };
            let summary = QuestionAnswer::summary_of(&answers);
            if !summary.trim().is_empty() {
                let stored = self.daemon.with_store(|st| {
                    st.transaction(|st| {
                        let question = st.append_message(
                            &self.session_id,
                            "assistant",
                            &crate::secrets::redact(&question).0,
                        )?;
                        let answer = st.append_message(
                            &self.session_id,
                            "user",
                            &crate::secrets::redact(&summary).0,
                        )?;
                        Ok([question, answer])
                    })
                });
                match stored {
                    Ok(messages) => {
                        for message in messages {
                            self.daemon.broadcast(Event::MessageAppended {
                                session_id: self.session_id.clone(),
                                message,
                            });
                        }
                    }
                    Err(error) => {
                        tracing::warn!("could not save Main Chat clarification: {error}");
                        return None;
                    }
                }
            }
            Some(answers)
        })
    }

    fn on_update(&self, _: HarnessUpdate) -> Result<()> {
        Ok(())
    }
}

async fn run_cli(
    request: HarnessRequest,
    private: &Path,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    use std::io::Write;

    let stdout = private.join("stdout");
    let prompt = private.join("prompt");
    topic_orchestrator::private_file(&prompt)?.write_all(request.prompt.as_bytes())?;
    let mut command = local_harness::command_spec(&request)?.command();
    command
        .env_remove(local_harness::TOPIC_CAPABILITY_ENV)
        .stdin(std::fs::File::open(prompt)?)
        .stdout(topic_orchestrator::private_file(&stdout)?)
        .stderr(Stdio::null());
    if cancel.is_cancelled() {
        bail!("Main Chat harness interrupted");
    }
    let mut child = formless_runtime::process_group::SupervisedProcess::spawn(command)
        .map_err(|error| anyhow!("{error}"))?;
    loop {
        if child.has_exited()? {
            break;
        }
        tokio::select! {
            _ = cancel.cancelled() => bail!("Main Chat harness interrupted"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }
    let status = child.finish()?;
    anyhow::ensure!(
        status.success(),
        "{} exited with {status}",
        harness_label(request.kind, None)
    );
    local_harness::parse_output(request.kind, &topic_orchestrator::read_log(&stdout)?)
}

impl Daemon {
    pub(super) fn reject_main_harness_attachment(&self, session_id: &str) -> Result<()> {
        if matches!(
            self.with_reader(|st| st.session_main_chat_backend(Some(session_id)))?,
            MainChatBackend::Harness { .. }
        ) {
            return Err(RequestFailure::invalid(NO_ATTACHMENTS).into());
        }
        Ok(())
    }

    pub(super) fn main_harness_admission(
        cfg: &Config,
        harness: MainChatHarness,
    ) -> Arc<RunAdmission> {
        Arc::new(RunAdmission {
            pipeline: None,
            cfg: cfg.clone(),
            requested: harness.label(),
            resolved_model: harness.label(),
            provider_name: None,
            routing_pick: None,
            effort: None,
            recovery_messages: vec![],
            resumed_from: None,
            first_message_seq: 0,
            first: StdMutex::new(None),
            harness: Some(harness),
        })
    }

    pub(super) async fn run_main_harness(
        self: &Arc<Self>,
        session_id: &str,
        run_id: &str,
        harness: MainChatHarness,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let turns = self.with_reader(|st| st.main_harness_turns(session_id, TRANSCRIPT_BYTES))?;
        let prompt = format!(
            "You are answering Main Chat. Reply to the final user message using only this transcript. Do not modify files or state, or access other conversations. If more information is needed and no interactive question tool is available, ask in your reply.\n\n{}",
            serde_json::to_string(&turns)?,
        );
        let relative = PathBuf::from("main-chat-runs").join(run_id);
        let private = Scratch(topic_orchestrator::private_dir(
            &self.paths.data_dir,
            &relative,
        )?);
        let cwd = topic_orchestrator::private_dir(&private.0, Path::new("work"))?;
        let request = HarnessRequest {
            kind: harness.kind,
            acp_agent: harness.agent.clone(),
            model: None,
            cwd,
            prompt,
            access: StepAccess::ReadOnly,
            mcp_command: PathBuf::new(),
            mcp_args: vec![],
            mcp_env_vars: vec![],
            executable: Some(harness.executable.clone()),
            executable_argv0: harness.argv0.clone(),
            environment: BTreeMap::new(),
        };
        let label = harness.label();
        let handler = Arc::new(MainInteraction {
            daemon: self.clone(),
            session_id: session_id.to_owned(),
            harness,
        });
        let run_cancel = cancel.child_token();
        let result = match request.kind {
            HarnessKind::Claude | HarnessKind::Codex => {
                run_cli(request, &private.0, &run_cancel).await
            }
            HarnessKind::Pi => {
                local_harness::pi::run(&request, Path::new(""), handler.as_ref(), &run_cancel).await
            }
            HarnessKind::Muse => {
                local_harness::muse::run(&request, handler.as_ref(), &run_cancel).await
            }
            HarnessKind::Acp | HarnessKind::Antigravity | HarnessKind::OpenCode => {
                local_harness::run(request, handler, run_cancel).await
            }
        }?;
        if cancel.is_cancelled() {
            bail!("Main Chat harness interrupted");
        }
        anyhow::ensure!(
            !result.message.trim().is_empty(),
            "Main Chat harness returned no answer"
        );
        let message = self.with_store(|st| {
            st.append_message_meta(
                session_id,
                "assistant",
                &crate::secrets::redact(&result.message).0,
                Some(json!({"harness": label}).to_string()),
            )
        })?;
        self.broadcast(Event::MessageAppended {
            session_id: session_id.into(),
            message,
        });
        Ok(())
    }
    pub(super) async fn list_main_chat_backends(&self) -> Result<MainChatBackends> {
        let cfg = self.config()?;
        let providers = std::iter::once((crate::config::DEFAULT_PROVIDER, &cfg.provider))
            .chain(cfg.providers.iter().map(|(name, cfg)| (name.as_str(), cfg)))
            .filter(|(_, cfg)| !cfg.base_url.trim().is_empty())
            .map(|(name, cfg)| MainChatProvider {
                name: name.to_owned(),
                model: cfg.model.clone(),
            })
            .collect();
        let harnesses = local_harness::list_harnesses()
            .await
            .into_iter()
            .filter(|harness| supports_read_only(harness.kind, harness.acp_agent.as_deref()))
            .map(|mut harness| {
                if !supported_version(&harness) {
                    if let Some(version) = harness.version.take() {
                        harness.detail = Some(format!(
                            "Installed version {version}. {}",
                            harness.detail.unwrap_or_default()
                        ));
                    }
                }
                harness
            })
            .collect();
        Ok(MainChatBackends {
            selected: self.with_reader(Store::main_chat_backend)?,
            providers,
            harnesses,
        })
    }

    pub(super) fn set_main_chat_backend(&self, backend: MainChatBackend) -> Result<Value> {
        let _admission = self.forecast_admission.locked();
        let _settings = self.config_guard();
        match &backend {
            MainChatBackend::Inherit => {}
            MainChatBackend::Provider {
                name,
                model,
                effort,
            } => {
                let cfg = self.config()?;
                let provider = cfg
                    .provider_cfg_ref(Some(name))
                    .map_err(|error| RequestFailure::invalid(error.to_string()))?;
                if name.trim().is_empty() || provider.base_url.trim().is_empty() {
                    return Err(
                        RequestFailure::invalid("Select a configured Main Chat provider").into(),
                    );
                }
                let wanted = provider.resolve_alias(model.as_deref().unwrap_or(&provider.model));
                if wanted.trim().is_empty() {
                    return Err(RequestFailure::invalid("Enter a model for this provider").into());
                }
                if let Some(role) = wanted.strip_prefix("role:") {
                    let role = cfg
                        .role(role)
                        .map_err(|error| RequestFailure::invalid(error.to_string()))?;
                    if role
                        .provider
                        .as_deref()
                        .unwrap_or(crate::config::DEFAULT_PROVIDER)
                        != name
                    {
                        return Err(RequestFailure::invalid(
                            "The model's Role uses a different provider",
                        )
                        .into());
                    }
                }
                if effort
                    .as_deref()
                    .is_some_and(|value| value.trim().is_empty())
                {
                    return Err(
                        RequestFailure::invalid("Reasoning effort must not be empty").into(),
                    );
                }
            }
            MainChatBackend::Harness { harness, acp_agent } => {
                local_harness::validate_acp_agent(Some(*harness), acp_agent.as_deref())
                    .map_err(|error| RequestFailure::invalid(error.to_string()))?;
                if !supports_read_only(*harness, acp_agent.as_deref()) {
                    return Err(RequestFailure::invalid(
                        "Select a Main Chat harness that supports read-only mode",
                    )
                    .into());
                }
            }
        }
        self.with_store(|st| st.set_main_chat_backend(&backend))?;
        self.broadcast(Event::MainChatBackendChanged {
            backend: backend.clone(),
        });
        Ok(serde_json::to_value(backend)?)
    }
}
