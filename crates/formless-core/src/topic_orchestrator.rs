use super::*;
use crate::local_harness::{self, HarnessRequest, HarnessResult, InteractionHandler};
use serde::Deserialize;
#[cfg(unix)]
use std::os::unix::{
    fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    process::CommandExt,
};
use std::{
    ffi::OsString,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const CAPABILITY: &str = "FORMLESS_TOPIC_CAPABILITY";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    feature_id: String,
    title: String,
    instructions: String,
    harness: HarnessKind,
    acp_agent: Option<String>,
    model: Option<String>,
    access: StepAccess,
    #[serde(default)]
    inputs: Vec<TopicArtifact>,
}

fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn private_dir(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut current = root.canonicalize()?;
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            bail!("invalid private directory");
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                bail!("private directory is not an ordinary directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = std::fs::DirBuilder::new();
                #[cfg(unix)]
                builder.mode(0o700);
                builder.create(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
        #[cfg(unix)]
        std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(current)
}

fn private_file(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    Ok(options.open(path)?)
}

fn read_log(path: &Path) -> Result<String> {
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    if data.len() > 16 * 1024 * 1024 {
        bail!("harness output exceeds 16 MiB; inspect its private log");
    }
    Ok(String::from_utf8_lossy(&data).into())
}

struct StepLease {
    daemon: Arc<Daemon>,
    key: (String, String),
    token: CancellationToken,
}
impl Drop for StepLease {
    fn drop(&mut self) {
        self.token.cancel();
        self.daemon.topic_steps.locked().remove(&self.key);
        self.daemon.topic_activity.notify_waiters();
    }
}
struct Worktree {
    repo: PathBuf,
    path: PathBuf,
}
impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
    }
}
struct ProcessGroup {
    pid: u32,
    active: bool,
}
impl ProcessGroup {
    fn kill(&self) {
        #[cfg(unix)]
        if self.active {
            // Each launched CLI leads its own process group.
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

impl Daemon {
    pub(super) async fn start_topic_workflow(self: &Arc<Self>, topic_id: String) -> Result<Value> {
        let topic = self.with_store(|st| st.claim_topic_workflow(&topic_id))?;
        let kickoff=format!("You are the Formless topic orchestrator. Decide how to complete these independent features using topic_run_step, topic_complete_feature, topic_fail_feature and ask_user. The outline is advisory: you decide the steps and harnesses. Inspect failures and choose another harness when an adapter is unavailable. Explicitly finish every feature; a step result is not feature completion. Never merge or push. Do not use registry or shell tools to bypass step isolation.\nFeatures, workspace and advisory outline:\n{}\nAvailable harness kinds: claude, codex, muse; pi, opencode, antigravity and acp currently return adapter-not-integrated errors. ACP requires acp_agent. Inputs are exact artifact references from the selected feature's outputs.\n",serde_json::to_string_pretty(&topic)?);
        let start = async {
            let session = topic
                .topic_session_id
                .clone()
                .context("approved topic has no Topic Chat")?;
            if self.runs.locked().contains_key(&session) {
                bail!("Topic Chat already has an active model run");
            }
            let daemon = self.clone();
            tokio::spawn(async move {
                GENERATED_MESSAGE
                    .scope("topic_kickoff".into(), async move {
                        daemon
                            .handle(Request::SendMessage {
                                session_id: session,
                                content: kickoff,
                                mode: SendMode::Send,
                                client_msg_id: None,
                            })
                            .await
                    })
                    .await
            })
            .await
            .context("topic kickoff task failed")??;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = start {
            self.with_store(|st| {
                st.fail_topic_workflow(&topic_id, &format!("orchestrator start failed: {error:#}"))
            })?;
            self.broadcast(Event::TopicChanged { topic_id });
            return Err(error.context("could not start topic orchestrator"));
        }
        let current = self
            .with_reader(|st| st.get_topic(&topic_id))?
            .context("topic vanished")?;
        if current.status == TopicStatus::Cancelled {
            if let Some(session) = &current.topic_session_id {
                if let Some(run) = self.runs.locked().get(session) {
                    run.cancel_flag.store(true, Ordering::SeqCst);
                    run.cancel.cancel();
                }
            }
        }
        self.broadcast(Event::TopicChanged {
            topic_id: topic_id.clone(),
        });
        Ok(serde_json::to_value(
            self.with_reader(|st| st.get_topic(&topic_id))?
                .context("topic vanished")?,
        )?)
    }

    pub(super) fn cancel_topic_work(&self, topic_id: &str) -> Result<TopicInfo> {
        let _launch = self.topic_launch.locked();
        let topic = self.with_store(|st| st.cancel_topic(topic_id))?;
        for ((id, _), cancel) in self.topic_steps.locked().iter() {
            if id == topic_id {
                cancel.cancel();
            }
        }
        self.close_topic_interactions(topic_id, None);
        Ok(topic)
    }

    pub(super) fn topic_orchestrator_ended(&self, session: &str, status: &str) {
        match self.with_store(|st| st.finish_topic_orchestrator(session, status)) {
            Ok(Some(topic_id)) => {
                for ((id, _), cancel) in self.topic_steps.locked().iter() {
                    if id == &topic_id {
                        cancel.cancel();
                    }
                }
                self.close_topic_interactions(&topic_id, None);
                self.broadcast(Event::TopicChanged { topic_id });
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("could not finalize topic orchestrator: {error:#}"),
        }
    }

    pub(super) fn finish_topic_feature_tool(
        &self,
        session: &str,
        args: &Value,
        ready: bool,
    ) -> Result<Value> {
        let feature = args["feature_id"]
            .as_str()
            .context("feature_id is required")?;
        let field = if ready { "summary" } else { "reason" };
        let detail = args[field]
            .as_str()
            .with_context(|| format!("{field} is required"))?;
        let topic =
            self.with_store(|st| st.finish_topic_feature(session, feature, ready, detail))?;
        self.broadcast(Event::TopicChanged {
            topic_id: topic.id.clone(),
        });
        Ok(serde_json::to_value(topic)?)
    }

    pub(super) fn run_topic_step<'a>(
        self: &'a Arc<Self>,
        session: &'a str,
        args: &'a Value,
        cancel: &'a CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(self.run_topic_step_inner(session, args, cancel))
    }

    async fn run_topic_step_inner(
        self: &Arc<Self>,
        session: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let step: Step = serde_json::from_value(args.clone())?;
        local_harness::validate_acp_agent(Some(step.harness), step.acp_agent.as_deref())?;
        if step.title.trim().is_empty() || step.instructions.trim().is_empty() {
            bail!("step title and instructions must not be blank");
        }
        let topic = self
            .with_reader(|st| st.topic_for_session(session))?
            .context("not a Topic Chat")?;
        if !matches!(topic.status, TopicStatus::Running | TopicStatus::NeedsUser) {
            bail!("topic is not running");
        }
        let feature = topic
            .features
            .iter()
            .find(|feature| feature.id == step.feature_id)
            .context("feature does not belong to this topic")?;
        if step
            .inputs
            .iter()
            .any(|input| !feature.outputs.contains(input))
        {
            bail!("inputs must be exact artifacts from this feature's outputs");
        }
        if cancel.is_cancelled() {
            bail!("step cancelled before preparation");
        }
        let step_cancel = self.shutdown.child_token();
        let key = (topic.id.clone(), feature.id.clone());
        {
            let mut steps = self.topic_steps.locked();
            if steps.contains_key(&key) {
                bail!("feature already has an active attempt");
            }
            steps.insert(key.clone(), step_cancel.clone());
        }
        let _lease = StepLease {
            daemon: self.clone(),
            key,
            token: step_cancel.clone(),
        };
        let root = private_dir(
            &self.paths.data_dir,
            &PathBuf::from("topic-attempts").join(uuid::Uuid::new_v4().to_string()),
        )?;
        let workspace = root.join("workspace");
        let inputs: Vec<_> = step
            .inputs
            .iter()
            .enumerate()
            .map(|(index, input)| match input {
                TopicArtifact::File { sha256, .. } => TopicArtifact::File {
                    path: root.join("inputs").join(index.to_string()),
                    sha256: sha256.clone(),
                },
                other => other.clone(),
            })
            .collect();
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let attempt = {
            let _launch = self.topic_launch.locked();
            if step_cancel.is_cancelled() || self.shutdown.is_cancelled() {
                bail!("step cancelled before preparation");
            }
            self.with_store(|st| {
                st.begin_topic_attempt(
                    &topic.id,
                    &feature.id,
                    &step.title,
                    step.access,
                    step.harness,
                    step.model.as_deref(),
                    step.acp_agent.as_deref(),
                    &inputs,
                    &token,
                    &workspace,
                )
            })?
        };
        self.broadcast(Event::TopicChanged {
            topic_id: topic.id.clone(),
        });
        let handler = self.topic_interaction_handler(&attempt.id, &token)?;
        let execution = self
            .execute_topic_step(
                &topic,
                feature,
                &step,
                &attempt,
                &token,
                &workspace,
                &root,
                &inputs,
                handler.as_ref(),
                &step_cancel,
            )
            .await;
        let execution = handler.finish().and(execution);
        let (status, summary, outputs, harness_session) = match execution {
            Ok((result, outputs)) => {
                let saved = self
                    .with_reader(|st| st.get_topic_attempt(&attempt.id))?
                    .context("attempt vanished")?;
                let status = if saved.summary.is_some()
                    && matches!(
                        saved.status,
                        TopicAttemptStatus::Failed | TopicAttemptStatus::NeedsUser
                    ) {
                    saved.status
                } else {
                    TopicAttemptStatus::Succeeded
                };
                let summary = saved.summary.unwrap_or(result.message);
                (status, summary, outputs, result.session_id)
            }
            Err(error) => (
                if step_cancel.is_cancelled() {
                    TopicAttemptStatus::Cancelled
                } else {
                    TopicAttemptStatus::Failed
                },
                format!("{error:#}"),
                vec![],
                None,
            ),
        };
        let finished = self.with_store(|st| {
            st.finish_topic_step(
                &attempt.id,
                status,
                &summary,
                &outputs,
                harness_session.as_deref(),
            )
        })?;
        self.close_topic_interactions(&topic.id, Some(&attempt.id));
        self.broadcast(Event::TopicChanged { topic_id: topic.id });
        Ok(
            json!({"status":finished.status,"summary":finished.summary,"outputs":finished.outputs,"conversation_session_id":finished.conversation_session_id}),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_topic_step(
        self: &Arc<Self>,
        topic: &TopicInfo,
        feature: &TopicFeatureInfo,
        step: &Step,
        attempt: &TopicAttemptInfo,
        token: &str,
        workspace: &Path,
        root: &Path,
        inputs: &[TopicArtifact],
        handler: &dyn InteractionHandler,
        cancel: &CancellationToken,
    ) -> Result<(HarnessResult, Vec<TopicArtifact>)> {
        let mut worktree = None;
        let mut base = None;
        if topic.workspace_kind == WorkspaceKind::Git {
            let repo = topic
                .workspace
                .as_ref()
                .context("Git topic has no repository")?;
            let latest = feature
                .outputs
                .iter()
                .rev()
                .find_map(|artifact| {
                    if let TopicArtifact::GitRevision { revision } = artifact {
                        Some(revision.as_str())
                    } else {
                        None
                    }
                })
                .or(topic.base_revision.as_deref())
                .context("Git topic has no base revision")?;
            let revision = if step.access == StepAccess::ReadOnly {
                step.inputs
                    .iter()
                    .find_map(|artifact| {
                        if let TopicArtifact::GitRevision { revision } = artifact {
                            Some(revision.as_str())
                        } else {
                            None
                        }
                    })
                    .unwrap_or(latest)
            } else {
                latest
            };
            let pinned = git(
                repo,
                &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
            )?;
            if pinned != revision {
                bail!("Git input must be an exact full commit revision");
            }
            git(
                repo,
                &[
                    "merge-base",
                    "--is-ancestor",
                    topic
                        .base_revision
                        .as_deref()
                        .context("missing base revision")?,
                    revision,
                ],
            )?;
            let path = workspace.to_str().context("worktree path is not UTF-8")?;
            worktree = Some(Worktree {
                repo: repo.clone(),
                path: workspace.into(),
            });
            if step.access == StepAccess::Write {
                git(
                    repo,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        &format!("formless-topic-{}", attempt.id),
                        path,
                        revision,
                    ],
                )?;
            } else {
                git(repo, &["worktree", "add", "--detach", path, revision])?;
            }
            base = Some(revision.to_owned());
        } else {
            private_dir(root, Path::new("workspace"))?;
        }
        if inputs
            .iter()
            .any(|input| matches!(input, TopicArtifact::File { .. }))
        {
            private_dir(root, Path::new("inputs"))?;
        }
        for (source, target) in step.inputs.iter().zip(inputs) {
            if let (
                TopicArtifact::File { path, sha256 },
                TopicArtifact::File { path: target, .. },
            ) = (source, target)
            {
                let bytes =
                    self.with_reader(|st| st.read_topic_input_file(&attempt.id, path, sha256))?;
                std::fs::write(target, bytes)?;
                let mut permissions = std::fs::metadata(target)?.permissions();
                permissions.set_readonly(true);
                std::fs::set_permissions(target, permissions)?;
            }
        }
        let outputs_dir = self
            .paths
            .data_dir
            .canonicalize()?
            .join("topic-attempts")
            .join(&attempt.id)
            .join("outputs");
        let prompt=format!("Topic: {}\nFeature {}: {}\nStep: {}\n{}\nAccess: {:?}\nInputs: {}\nWrite file outputs only to {} and report their SHA-256 via report_result. Text outputs can be reported directly. Never merge or push. For Git Write create a clean new commit; the daemon verifies HEAD. Use the scoped Formless MCP for questions, conversations and reporting outputs.\n",topic.name,feature.id,feature.request,step.title,step.instructions,step.access,serde_json::to_string(inputs)?,outputs_dir.display());
        let message = self.with_store(|st| {
            st.append_message(&attempt.conversation_session_id, "user", &prompt)
        })?;
        self.broadcast(Event::MessageAppended {
            session_id: attempt.conversation_session_id.clone(),
            message,
        });
        let request = HarnessRequest {
            kind: step.harness,
            acp_agent: step.acp_agent.clone(),
            model: step.model.clone(),
            cwd: workspace.into(),
            prompt,
            access: step.access,
            mcp_command: executable("FORMLESS_TOPIC_MCP_EXE")?
                .map(Ok)
                .unwrap_or_else(std::env::current_exe)?,
            mcp_args: vec![
                "--profile".into(),
                self.paths.profile.clone(),
                "topic-mcp".into(),
                "--attempt".into(),
                attempt.id.clone(),
            ],
            mcp_env_vars: vec![
                CAPABILITY.into(),
                "FORMLESS_DATA_DIR".into(),
                "FORMLESS_CONFIG_DIR".into(),
                "FORMLESS_TOPIC_OUTPUTS_DIR".into(),
            ],
            executable: match step.harness {
                HarnessKind::Claude => executable("FORMLESS_TOPIC_CLAUDE_EXE")?,
                HarnessKind::Codex => executable("FORMLESS_TOPIC_CODEX_EXE")?,
                HarnessKind::Muse => executable("FORMLESS_TOPIC_MUSE_EXE")?,
                _ => None,
            },
            environment: BTreeMap::from([
                (CAPABILITY.into(), OsString::from(token)),
                (
                    "FORMLESS_TOPIC_ATTEMPT_ID".into(),
                    OsString::from(&attempt.id),
                ),
                (
                    "FORMLESS_DATA_DIR".into(),
                    std::path::absolute(self.paths.named("default")?.data_dir)?.into_os_string(),
                ),
                (
                    "FORMLESS_CONFIG_DIR".into(),
                    std::path::absolute(self.paths.named("default")?.config_dir)?.into_os_string(),
                ),
                (
                    "FORMLESS_TOPIC_OUTPUTS_DIR".into(),
                    outputs_dir.into_os_string(),
                ),
            ]),
        };
        let result = self
            .launch_topic_adapter(attempt, token, &request, handler, cancel)
            .await?;
        let mut outputs = vec![];
        if step.access == StepAccess::Write && worktree.is_some() {
            if !git(
                workspace,
                &["status", "--porcelain", "--untracked-files=all"],
            )?
            .is_empty()
            {
                bail!("Git Write step did not leave a clean worktree");
            }
            let revision = git(workspace, &["rev-parse", "HEAD"])?;
            if Some(&revision) == base.as_ref() {
                bail!("Git Write step did not create a new commit");
            }
            git(
                workspace,
                &[
                    "merge-base",
                    "--is-ancestor",
                    base.as_deref().context("missing worktree base")?,
                    &revision,
                ],
            )?;
            outputs.push(TopicArtifact::GitRevision { revision });
        }
        drop(worktree);
        Ok((result, outputs))
    }

    async fn launch_topic_adapter(
        self: &Arc<Self>,
        attempt: &TopicAttemptInfo,
        token: &str,
        request: &HarnessRequest,
        handler: &dyn InteractionHandler,
        cancel: &CancellationToken,
    ) -> Result<HarnessResult> {
        match request.kind {
            HarnessKind::Claude => {
                self.run_topic_cli(attempt, token, request, handler, cancel)
                    .await
            }
            HarnessKind::Codex => {
                self.run_topic_cli(attempt, token, request, handler, cancel)
                    .await
            }
            HarnessKind::Pi => bail!("Pi adapter not integrated yet; choose Claude or Codex"),
            HarnessKind::OpenCode => {
                bail!("OpenCode adapter not integrated yet; choose Claude or Codex")
            }
            HarnessKind::Antigravity => {
                bail!("Antigravity adapter not integrated yet; choose Claude or Codex")
            }
            HarnessKind::Acp => bail!("ACP adapter not integrated yet; choose Claude or Codex"),
            HarnessKind::Muse => local_harness::muse::run(request, handler, cancel).await,
        }
    }

    async fn run_topic_cli(
        self: &Arc<Self>,
        attempt: &TopicAttemptInfo,
        token: &str,
        request: &HarnessRequest,
        _handler: &dyn InteractionHandler,
        cancel: &CancellationToken,
    ) -> Result<HarnessResult> {
        let private = self
            .paths
            .data_dir
            .canonicalize()?
            .join("topic-attempts")
            .join(&attempt.id);
        let stdout = private.join("stdout");
        let stderr = private.join("stderr");
        let mut command = local_harness::command_spec(request)?.command();
        command
            .stdin(Stdio::null())
            .stdout(private_file(&stdout)?)
            .stderr(private_file(&stderr)?);
        #[cfg(unix)]
        command.process_group(0);
        let mut command = tokio::process::Command::from(command);
        command.kill_on_drop(true);
        let mut child = {
            let _launch = self.topic_launch.locked();
            self.with_reader(|st| st.authenticate_topic_attempt(&attempt.id, token))?;
            if cancel.is_cancelled() || self.shutdown.is_cancelled() {
                bail!("step cancelled before launch");
            }
            command.spawn().with_context(||format!("could not launch {:?}; install its official CLI or configure its executable override",request.kind))?
        };
        let mut group = ProcessGroup {
            pid: child.id().context("child has no process id")?,
            active: true,
        };
        let status = tokio::select! {
            _=cancel.cancelled()=>{group.kill();let _=child.kill().await;let _=child.wait().await;Err(anyhow!("harness interrupted"))},
            _=self.shutdown.cancelled()=>{group.kill();let _=child.kill().await;let _=child.wait().await;Err(anyhow!("daemon is shutting down"))},
            status=child.wait()=>status.map_err(anyhow::Error::from),
        };
        group.active = false;
        let stdout = read_log(&stdout)?.replace(token, "[REDACTED]");
        let stderr = read_log(&stderr)?.replace(token, "[REDACTED]");
        for (kind, content) in [("stdout", &stdout), ("stderr", &stderr)] {
            if !content.is_empty() {
                let message = self.with_store(|st| {
                    st.append_message_meta(
                        &attempt.conversation_session_id,
                        "assistant",
                        content,
                        Some(json!({"harness_log":kind}).to_string()),
                    )
                })?;
                self.broadcast(Event::MessageAppended {
                    session_id: attempt.conversation_session_id.clone(),
                    message,
                });
            }
        }
        let status = status?;
        if !status.success() {
            bail!(
                "{:?} exited with {status}; inspect the attempt conversation",
                request.kind
            );
        }
        local_harness::parse_output(request.kind, &stdout)
    }
}

fn executable(name: &str) -> Result<Option<PathBuf>> {
    std::env::var_os(name)
        .map(|value| {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                bail!("{name} must be an absolute executable path");
            }
            Ok(path)
        })
        .transpose()
}
