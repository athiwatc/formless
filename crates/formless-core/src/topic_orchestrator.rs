use super::*;
use crate::local_harness::{self, HarnessRequest, HarnessResult, InteractionHandler};
use serde::Deserialize;
#[cfg(unix)]
use std::os::unix::{
    fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    process::CommandExt,
};
#[cfg(windows)]
use std::os::windows::fs::FileTypeExt;
use std::{
    ffi::OsString,
    io::{Read, Write},
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

fn resolve_directory_link(path: &Path) -> Result<PathBuf> {
    let mut prefix = path.to_path_buf();
    let mut missing = Vec::new();
    let mut resolved = loop {
        match prefix.canonicalize() {
            Ok(resolved) => break resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&prefix).is_ok() {
                    bail!("directory symlink target contains an unresolvable link");
                }
                missing.push(
                    prefix
                        .components()
                        .next_back()
                        .context("missing symlink target prefix")?
                        .as_os_str()
                        .to_os_string(),
                );
                if !prefix.pop() {
                    return Err(error.into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    };
    for component in missing.into_iter().rev() {
        if component == ".." {
            resolved.pop();
        } else if component != "." {
            resolved.push(component);
        }
        match resolved.canonicalize() {
            Ok(existing) => resolved = existing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&resolved).is_ok() {
                    bail!("directory symlink target contains an unresolvable link");
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(resolved)
}

fn copy_directory(source: &Path, destination: &Path, cancel: &CancellationToken) -> Result<()> {
    let source = source.canonicalize()?;
    if !source.is_dir() || destination.starts_with(&source) {
        bail!("directory copy must be outside its source tree");
    }
    fn copy(
        source: &Path,
        target: &Path,
        source_root: &Path,
        target_root: &Path,
        cancel: &CancellationToken,
    ) -> Result<()> {
        if cancel.is_cancelled() {
            bail!("directory copy cancelled");
        }
        for entry in std::fs::read_dir(source)? {
            if cancel.is_cancelled() {
                bail!("directory copy cancelled");
            }
            let entry = entry?;
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let target = target.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                let link = std::fs::read_link(&path)?;
                if link.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::RootDir | std::path::Component::Prefix(_)
                    )
                }) {
                    bail!("directory symlink escapes the source tree: absolute targets are not allowed");
                }
                let resolved = resolve_directory_link(
                    &path.parent().context("missing source parent")?.join(&link),
                )?;
                if !resolved.starts_with(source_root) {
                    bail!("directory symlink escapes the source tree");
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&link, &target)?;
                #[cfg(windows)]
                if kind.is_symlink_dir() {
                    std::os::windows::fs::symlink_dir(&link, &target)?;
                } else {
                    std::os::windows::fs::symlink_file(&link, &target)?;
                }
            } else if kind.is_dir() {
                private_dir(target_root, target.strip_prefix(target_root)?)?;
                copy(&path, &target, source_root, target_root, cancel)?;
            } else if kind.is_file() {
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
                let mut input = options.open(&path)?;
                let metadata = input.metadata()?;
                if !metadata.is_file() {
                    bail!("directory copy encountered a non-regular file");
                }
                let mut output = private_file(&target)?;
                let mut buffer = vec![0u8; 65536];
                loop {
                    if cancel.is_cancelled() {
                        bail!("directory copy cancelled");
                    }
                    let size = input.read(&mut buffer)?;
                    if size == 0 {
                        break;
                    }
                    output.write_all(&buffer[..size])?;
                }
                std::fs::set_permissions(&target, metadata.permissions())?;
            } else {
                bail!("directory copy encountered a special file");
            }
        }
        Ok(())
    }
    copy(&source, destination, &source, destination, cancel)
}

async fn copy_directory_async(
    source: &Path,
    destination: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let (source, destination, cancel) = (
        source.to_path_buf(),
        destination.to_path_buf(),
        cancel.clone(),
    );
    tokio::task::spawn_blocking(move || copy_directory(&source, &destination, &cancel)).await?
}

fn git_input(repo: &Path, topic: &TopicInfo, revision: &str) -> Result<()> {
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
    Ok(())
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
    pub(super) async fn wait_topic_steps(
        &self,
        session: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wait {
            attempt_ids: Option<Vec<String>>,
            timeout_secs: Option<u64>,
        }
        let args: Wait = serde_json::from_value(args.clone())?;
        let timeout = args.timeout_secs.unwrap_or(600);
        if !(1..=600).contains(&timeout) {
            bail!("timeout_secs must be between 1 and 600");
        }
        let topic = self
            .with_reader(|st| st.topic_for_session(session))?
            .context("not a Topic Chat")?;
        if !matches!(
            topic.status,
            TopicStatus::Running | TopicStatus::NeedsUser | TopicStatus::Cancelled
        ) {
            bail!("topic is not active");
        }
        if args.attempt_ids.as_ref().is_some_and(|ids| {
            ids.iter()
                .any(|id| !topic.attempts.iter().any(|a| &a.id == id))
        }) {
            bail!("attempt_ids must belong to this topic");
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
        let mut timed_out = false;
        loop {
            let wake = self.topic_activity.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            let current = self
                .with_reader(|st| st.get_topic(&topic.id))?
                .context("topic was removed")?;
            let mut attempts = current
                .attempts
                .into_iter()
                .filter(|a| {
                    args.attempt_ids
                        .as_ref()
                        .is_none_or(|ids| ids.contains(&a.id))
                })
                .collect::<Vec<_>>();
            attempts.sort_by(|a, b| a.updated_at.cmp(&b.updated_at).then(a.id.cmp(&b.id)));
            let snapshot = |event: &str| json!({"event":event,"topic_status":current.status,"attempts":attempts});
            if cancel.is_cancelled()
                || self.shutdown.is_cancelled()
                || current.status == TopicStatus::Cancelled
            {
                return Ok(snapshot("cancelled"));
            }
            {
                let attention = self.pending_attention.locked().clone();
                let mut cursors = self.topic_wait_seen.locked();
                let seen = cursors.entry(session.into()).or_default();
                for event in attention {
                    match event {
                        Event::UserQuestion {
                            question_id,
                            topic_id,
                            attempt_id,
                            questions,
                            user_only,
                            ..
                        } if topic_id.as_deref() == Some(topic.id.as_str())
                            && args.attempt_ids.as_ref().is_none_or(|ids| {
                                attempt_id.as_ref().is_some_and(|id| ids.contains(id))
                            }) =>
                        {
                            if !self.questions.locked().contains_key(&question_id)
                                && !self.topic_decisions.locked().contains_key(&question_id)
                            {
                                continue;
                            }
                            if !user_only || seen.insert(format!("question:{question_id}")) {
                                return Ok(
                                    json!({"event":"question","question_id":question_id,"attempt_id":attempt_id,"questions":questions,"user_only":user_only}),
                                );
                            }
                        }
                        Event::HarnessPermissionRequested {
                            permission_id,
                            topic_id,
                            attempt_id,
                            harness,
                            acp_agent,
                            title,
                            kind,
                            ..
                        } if topic_id == topic.id
                            && args
                                .attempt_ids
                                .as_ref()
                                .is_none_or(|ids| ids.contains(&attempt_id)) =>
                        {
                            if !self
                                .harness_permissions
                                .locked()
                                .contains_key(&permission_id)
                            {
                                continue;
                            }
                            if seen.insert(format!("permission:{permission_id}")) {
                                return Ok(
                                    json!({"event":"permission","informational_only":true,"permission_id":permission_id,"attempt_id":attempt_id,"harness":harness,"acp_agent":acp_agent,"title":title,"kind":kind}),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                for attempt in &attempts {
                    let active = self
                        .topic_steps
                        .locked()
                        .contains_key(&(topic.id.clone(), attempt.id.clone()));
                    if !active
                        && attempt.status != TopicAttemptStatus::Running
                        && seen.insert(format!("attempt:{}", attempt.id))
                    {
                        return Ok(
                            json!({"event":"completed","attempt_id":attempt.id,"status":attempt.status,"summary":attempt.summary,"outputs":attempt.outputs,"conversation_session_id":attempt.conversation_session_id}),
                        );
                    }
                }
            }
            if timed_out
                || !attempts.iter().any(|a| {
                    a.status == TopicAttemptStatus::Running
                        || self
                            .topic_steps
                            .locked()
                            .contains_key(&(topic.id.clone(), a.id.clone()))
                        || a.status == TopicAttemptStatus::NeedsUser
                            && current
                                .features
                                .iter()
                                .any(|f| f.active_attempt_id.as_ref() == Some(&a.id))
                })
            {
                return Ok(snapshot("snapshot"));
            }
            tokio::select! {
                _=cancel.cancelled()=>{},
                _=self.shutdown.cancelled()=>{},
                _=&mut wake=>{},
                _=tokio::time::sleep_until(deadline)=>{timed_out=true;},
            }
        }
    }

    pub(super) async fn start_topic_workflow(self: &Arc<Self>, topic_id: String) -> Result<Value> {
        let topic = self.with_store(|st| st.claim_topic_workflow(&topic_id))?;
        let kickoff=format!("You are the Formless topic orchestrator. Launch independent features concurrently with topic_run_step, then loop on topic_wait. Steps are asynchronous and return attempt IDs, not results. Answer harness questions with topic_answer_question only when a nonempty basis quotes or references a user's message or decision; never answer user_only questions or permissions. Use ask_user if you lack a basis. Inspect the finished outputs and explicitly call topic_complete_feature or topic_fail_feature; a step result is not feature completion. The outline is advisory: you decide the steps and harnesses. Never merge or push. Never modify the source workspace or its branch. Do not use registry or shell tools to bypass step isolation.\nFeatures, workspace and advisory outline:\n{}\nAvailable harness kinds: claude, codex, antigravity and opencode; pi, acp and muse currently return adapter-not-integrated errors. ACP requires acp_agent. Inputs are exact artifact references from finalized feature outputs in this topic, as selected for this step. Combine inputs only when instructed, in an isolated step. If you end your turn with active attempts, the topic becomes NeedsUser with a durable note; nothing resumes the model automatically.\n",serde_json::to_string_pretty(&topic)?);
        let start = async {
            let session = topic
                .topic_session_id
                .clone()
                .context("approved topic has no Topic Chat")?;
            if self.runs.locked().contains_key(&session) {
                bail!("Topic Chat already has an active model run");
            }
            let availability=local_harness::list_harnesses().await;
            if self.shutdown.is_cancelled(){bail!("daemon shutting down");}
            if self.with_reader(|st|st.get_topic(&topic_id))?.is_none_or(|topic|topic.status!=TopicStatus::Running){bail!("topic stopped during readiness probes");}
            if self.runs.locked().contains_key(&session){bail!("Topic Chat already has an active model run");}
            let kickoff=format!("{kickoff}\nFresh local harness availability (ready means installed && signed_in != false; unknown auth is ready):\n{}\nNever launch an unready harness. Ask the user through topic_request_harness_substitute to sign in and retry, approve a substitute for one feature+step, or skip. Saved permission rules and topic limits are user controls, not model tools.\n",serde_json::to_string(&availability)?);
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
            self.close_topic_interactions(&topic_id, None);
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

    pub(super) fn topic_orchestrator_ended<'a>(
        &self,
        session: &str,
        run: &str,
        status: &'a str,
    ) -> &'a str {
        match self.with_store(|st| st.finish_topic_orchestrator(session, run, status)) {
            Ok(Some(topic_id)) => {
                if status != "completed" {
                    for ((id, _), cancel) in self.topic_steps.locked().iter() {
                        if id == &topic_id {
                            cancel.cancel();
                        }
                    }
                    self.close_topic_interactions(&topic_id, None);
                }
                self.broadcast(Event::TopicChanged { topic_id });
                if matches!(status, "paused" | "failed" | "error") {
                    return "abandoned";
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("could not finalize topic orchestrator: {error:#}"),
        }
        status
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
        let topic = self
            .with_reader(|st| st.topic_for_session(session))?
            .context("not a Topic Chat")?;
        let unfinalized = {
            let steps = self.topic_steps.locked();
            topic.attempts.iter().any(|attempt| {
                attempt.feature_id == feature
                    && steps.contains_key(&(topic.id.clone(), attempt.id.clone()))
            })
        };
        if unfinalized {
            bail!("feature has a live attempt; wait for CLI exit and output validation");
        }
        let topic =
            self.with_store(|st| st.finish_topic_feature(session, feature, ready, detail))?;
        if matches!(
            topic.status,
            TopicStatus::Complete | TopicStatus::Failed | TopicStatus::Cancelled
        ) {
            self.close_topic_interactions(&topic.id, None);
        } else {
            self.close_topic_decisions(&topic.id, Some(feature));
        }
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
        let mut step: Step = serde_json::from_value(args.clone())?;
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
        let substitute_key = (
            topic.id.clone(),
            feature.id.clone(),
            step.title.trim().to_owned(),
        );
        let approved = {
            let approvals = self.topic_substitutes.locked();
            approvals
                .get(&substitute_key)
                .filter(|approval| {
                    approval.wanted.harness == step.harness
                        && approval.wanted.acp_agent == step.acp_agent
                })
                .cloned()
        };
        if let Some(approval) = &approved {
            step.harness = approval.substitute.harness;
            step.acp_agent = approval.substitute.acp_agent.clone();
            step.model = approval.substitute.model.clone();
        }
        let availability =
            local_harness::harness_availability(step.harness, step.acp_agent.as_deref()).await;
        if !availability.installed || availability.signed_in == Some(false) {
            bail!("{:?} is not ready: {}. Install/sign in and retry, or ask the user with topic_request_harness_substitute",step.harness,availability.detail.as_deref().unwrap_or("not signed in"));
        }
        if step.inputs.iter().any(|input| {
            !topic
                .features
                .iter()
                .any(|feature| feature.outputs.contains(input))
        }) {
            bail!("inputs must be exact artifacts from this topic's finalized feature outputs");
        }
        if cancel.is_cancelled() {
            bail!("step cancelled before preparation");
        }
        let step_cancel = self.shutdown.child_token();
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
                TopicArtifact::Directory { .. } => TopicArtifact::Directory {
                    path: root
                        .join("inputs")
                        .join(index.to_string())
                        .to_string_lossy()
                        .into_owned(),
                },
                other => other.clone(),
            })
            .collect();
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let (attempt, lease) = {
            let _launch = self.topic_launch.locked();
            if approved.is_some()
                && self.topic_substitutes.locked().get(&substitute_key) != approved.as_ref()
            {
                bail!("substitute approval was already consumed; ask the user again");
            }
            if step_cancel.is_cancelled() || self.shutdown.is_cancelled() {
                bail!("step cancelled before preparation");
            }
            if let Some(total) = self.with_store(|st| {
                st.transaction(|st| {
                    let hit = st.topic_step_limit(&topic.id, &feature.id, &step.title)?;
                    if hit.is_some() {
                        st.mark_topic_limit(&topic.id, &feature.id)?;
                    }
                    Ok(hit)
                })
            })? {
                self.raise_topic_limit_question(&topic, &feature.id, total)?;
                bail!(
                    "Limit reached: {}; ask the user to raise the limit or stop",
                    if total {
                        "attempts per topic"
                    } else {
                        "retries for this feature and step title"
                    }
                );
            }
            let attempt = self.with_store(|st| {
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
            })?;
            let key = (topic.id.clone(), attempt.id.clone());
            self.topic_substitutes.locked().remove(&substitute_key);
            self.topic_steps
                .locked()
                .insert(key.clone(), step_cancel.clone());
            let lease = StepLease {
                daemon: self.clone(),
                key,
                token: step_cancel.clone(),
            };
            (attempt, lease)
        };
        self.broadcast(Event::TopicChanged {
            topic_id: topic.id.clone(),
        });
        let launched = json!({"attempt_id":attempt.id,"conversation_session_id":attempt.conversation_session_id});
        let run_cancel = self
            .runs
            .locked()
            .get(session)
            .map(|run| run.cancel.clone())
            .unwrap_or_else(|| cancel.clone());
        let daemon = self.clone();
        tokio::spawn(async move {
            let _lease = lease;
            let attempt_id = attempt.id.clone();
            let topic_id = topic.id.clone();
            use futures_util::FutureExt;
            let result = std::panic::AssertUnwindSafe(daemon.execute_topic_attempt(
                topic,
                step,
                attempt,
                token,
                workspace,
                root,
                inputs,
                step_cancel,
                run_cancel,
            ))
            .catch_unwind()
            .await;
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("{error:#}")),
                Err(_) => {
                    Some("attempt worker panicked; inspect its conversation before retrying".into())
                }
            };
            if let Some(error) = error {
                let saved = daemon.with_store(|st| {
                    st.finish_topic_step(&attempt_id, TopicAttemptStatus::Failed, &error, &[], None)
                });
                if let Err(error) = saved {
                    tracing::warn!("could not finalize topic attempt: {error:#}");
                }
                daemon.close_topic_interactions(&topic_id, Some(&attempt_id));
                daemon.broadcast(Event::TopicChanged { topic_id });
            }
        });
        Ok(launched)
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_topic_attempt(
        self: &Arc<Self>,
        topic: TopicInfo,
        step: Step,
        attempt: TopicAttemptInfo,
        token: String,
        workspace: PathBuf,
        root: PathBuf,
        inputs: Vec<TopicArtifact>,
        step_cancel: CancellationToken,
        cancel: CancellationToken,
    ) -> Result<()> {
        let feature = topic
            .features
            .iter()
            .find(|feature| feature.id == step.feature_id)
            .context("feature vanished")?;
        let handler = self.topic_interaction_handler(&attempt.id, &token)?;
        let execution = {
            let execution = self.execute_topic_step(
                &topic,
                feature,
                &step,
                &attempt,
                &token,
                &workspace,
                &root,
                &inputs,
                handler.clone(),
                &step_cancel,
            );
            tokio::pin!(execution);
            let execution = tokio::select! {
                biased;
                _=cancel.cancelled()=>{step_cancel.cancel();execution.await},
                result=&mut execution=>result,
            };
            execution
        };
        self.finish_topic_attempt_execution(
            &topic.id,
            &attempt.id,
            execution,
            step_cancel.is_cancelled(),
        )
    }

    pub(super) fn finish_topic_attempt_execution(
        &self,
        topic_id: &str,
        attempt_id: &str,
        execution: Result<(HarnessResult, Vec<TopicArtifact>)>,
        cancelled: bool,
    ) -> Result<()> {
        let saved = self
            .with_reader(|st| st.get_topic_attempt(attempt_id))?
            .context("attempt vanished")?;
        let (status, summary, outputs, harness_session) = match execution {
            Ok((result, outputs)) => {
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
                if cancelled {
                    TopicAttemptStatus::Cancelled
                } else {
                    TopicAttemptStatus::Failed
                },
                format!("{error:#}"),
                vec![],
                None,
            ),
        };
        self.with_store(|st| {
            st.finish_topic_step(
                attempt_id,
                status,
                &summary,
                &outputs,
                harness_session.as_deref(),
            )
        })?;
        self.close_topic_interactions(topic_id, Some(attempt_id));
        self.broadcast(Event::TopicChanged {
            topic_id: topic_id.into(),
        });
        Ok(())
    }

    pub(super) fn accept_topic_harness_result(
        &self,
        attempt_id: &str,
        result: Result<HarnessResult>,
        cancelled: bool,
    ) -> Result<HarnessResult> {
        match result {
            Ok(result) => Ok(result),
            Err(error) => {
                let saved = self
                    .with_reader(|st| st.get_topic_attempt(attempt_id))?
                    .context("attempt vanished")?;
                if cancelled || saved.status != TopicAttemptStatus::Succeeded {
                    return Err(error);
                }
                tracing::warn!(attempt_id,error=%format!("{error:#}"),"ignoring harness error after successful topic report; daemon validation still required");
                Ok(HarnessResult {
                    session_id: saved.harness_session_id,
                    message: saved.summary.unwrap_or_default(),
                    events: vec![],
                })
            }
        }
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
        handler: Arc<dyn InteractionHandler>,
        cancel: &CancellationToken,
    ) -> Result<(HarnessResult, Vec<TopicArtifact>)> {
        let mut worktree = None;
        let mut extra_worktrees = Vec::new();
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
            let revision = step
                .inputs
                .iter()
                .find_map(|artifact| {
                    if let TopicArtifact::GitRevision { revision } = artifact {
                        Some(revision.as_str())
                    } else {
                        None
                    }
                })
                .unwrap_or(latest);
            git_input(repo, topic, revision)?;
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
                        &format!("formless/topic/{}/{}", topic.id, attempt.id),
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
            let source = step
                .inputs
                .iter()
                .find_map(|input| {
                    if let TopicArtifact::Directory { path } = input {
                        Some(PathBuf::from(path))
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    (topic.workspace_kind == WorkspaceKind::Directory)
                        .then(|| topic.workspace.clone())
                        .flatten()
                });
            if let Some(source) = source {
                if step.inputs.iter().any(|input|matches!(input,TopicArtifact::Directory{path} if Path::new(path)==source)){
                    self.validate_topic_directory_input(&source)?;
                }
                copy_directory_async(&source, workspace, cancel).await?;
            }
        }
        if inputs.iter().any(|input| {
            matches!(
                input,
                TopicArtifact::File { .. }
                    | TopicArtifact::Directory { .. }
                    | TopicArtifact::GitRevision { .. }
            )
        }) {
            private_dir(root, Path::new("inputs"))?;
        }
        for (index, (source, target)) in step.inputs.iter().zip(inputs).enumerate() {
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
            if let (TopicArtifact::Directory { path }, TopicArtifact::Directory { path: target }) =
                (source, target)
            {
                let source = self.validate_topic_directory_input(Path::new(path))?;
                let destination =
                    private_dir(root, &PathBuf::from("inputs").join(index.to_string()))?;
                if Path::new(target) != destination {
                    bail!("invalid directory input destination");
                }
                copy_directory_async(&source, &destination, cancel).await?;
            }
            if let TopicArtifact::GitRevision { revision } = source {
                let repo = topic
                    .workspace
                    .as_ref()
                    .filter(|_| topic.workspace_kind == WorkspaceKind::Git)
                    .context("Git revision input requires a Git topic workspace")?;
                git_input(repo, topic, revision)?;
                let path = root.join("inputs").join(format!("git-{index}"));
                let input_worktree = Worktree {
                    repo: repo.clone(),
                    path: path.clone(),
                };
                git(
                    repo,
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        path.to_str().context("input worktree path is not UTF-8")?,
                        revision,
                    ],
                )?;
                extra_worktrees.push((revision.clone(), input_worktree));
            }
        }
        let outputs_dir = self
            .paths
            .data_dir
            .canonicalize()?
            .join("topic-attempts")
            .join(&attempt.id)
            .join("outputs");
        let git_inputs: Vec<_> = extra_worktrees
            .iter()
            .map(|(revision, tree)| json!({"revision":revision,"path":tree.path}))
            .collect();
        let prompt=format!("Topic: {}\nFeature {}: {}\nStep: {}\n{}\nAccess: {:?}\nInputs: {}\nGit input worktrees: {}\nWrite file outputs only to {} and report their SHA-256 via report_result. Text outputs can be reported directly. Never push or modify the source workspace or its branch. Combine inputs only in this private workspace when instructed. For Git Write create a clean new commit; the daemon verifies HEAD. Use the scoped Formless MCP for questions, conversations and reporting outputs.\n",topic.name,feature.id,feature.request,step.title,step.instructions,step.access,serde_json::to_string(inputs)?,serde_json::to_string(&git_inputs)?,outputs_dir.display());
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
                HarnessKind::OpenCode => executable("FORMLESS_TOPIC_OPENCODE_EXE")?,
                HarnessKind::Antigravity => executable("FORMLESS_TOPIC_ANTIGRAVITY_EXE")?,
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
            .launch_topic_adapter(attempt, token, &request, handler.clone(), cancel)
            .await;
        let result = self.accept_topic_harness_result(
            &attempt.id,
            handler.finish().and(result),
            cancel.is_cancelled(),
        )?;
        let reported = self
            .with_reader(|st| st.get_topic_attempt(&attempt.id))?
            .context("attempt vanished")?;
        let needs_revision = reported
            .outputs
            .iter()
            .any(|output| matches!(output, TopicArtifact::GitRevision { .. }));
        let head = if needs_revision || (step.access == StepAccess::Write && worktree.is_some()) {
            if worktree.is_none() {
                bail!("Git revision output requires a Git workspace");
            }
            Some(git(workspace, &["rev-parse", "HEAD"])?)
        } else {
            None
        };
        for artifact in &reported.outputs {
            if let TopicArtifact::GitRevision { revision } = artifact {
                if head.as_deref() != Some(revision.as_str()) {
                    bail!("reported GitRevision does not match workspace HEAD");
                }
            }
        }
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
            let revision = head.context("Git Write step has no HEAD")?;
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
        if step.access == StepAccess::Write && topic.workspace_kind == WorkspaceKind::Directory {
            let directory = workspace.canonicalize()?;
            if directory != workspace || !directory.is_dir() {
                bail!("Directory Write workspace must remain the attempt's private directory");
            }
            outputs.push(TopicArtifact::Directory {
                path: directory.to_string_lossy().into_owned(),
            });
        }
        drop(worktree);
        Ok((result, outputs))
    }

    fn validate_topic_directory_input(&self, path: &Path) -> Result<PathBuf> {
        let path = path.canonicalize()?;
        let private = self.paths.data_dir.canonicalize()?.join("topic-attempts");
        if !path.starts_with(&private) || !path.is_dir() {
            bail!("directory input must be a private topic attempt artifact");
        }
        Ok(path)
    }

    pub(super) async fn launch_topic_adapter(
        self: &Arc<Self>,
        attempt: &TopicAttemptInfo,
        token: &str,
        request: &HarnessRequest,
        handler: Arc<dyn InteractionHandler>,
        cancel: &CancellationToken,
    ) -> Result<HarnessResult> {
        match request.kind {
            HarnessKind::Claude => {
                self.run_topic_cli(attempt, token, request, handler.as_ref(), cancel)
                    .await
            }
            HarnessKind::Codex => {
                self.run_topic_cli(attempt, token, request, handler.as_ref(), cancel)
                    .await
            }
            HarnessKind::Pi => bail!("Pi adapter not integrated yet; choose Claude or Codex"),
            HarnessKind::OpenCode => local_harness::run(request, handler, cancel).await,
            HarnessKind::Antigravity => {
                self.run_topic_acp(
                    attempt,
                    token,
                    local_harness::antigravity::run,
                    request,
                    handler.as_ref(),
                    cancel,
                )
                .await
            }
            HarnessKind::Acp => bail!("ACP adapter not integrated yet; choose Claude or Codex"),
            HarnessKind::Muse => bail!("Muse adapter not integrated yet; choose Claude or Codex"),
        }
    }

    async fn run_topic_acp(
        self: &Arc<Self>,
        attempt: &TopicAttemptInfo,
        token: &str,
        adapter: local_harness::acp::Adapter,
        request: &HarnessRequest,
        handler: &dyn InteractionHandler,
        cancel: &CancellationToken,
    ) -> Result<HarnessResult> {
        {
            let _launch = self.topic_launch.locked();
            self.with_reader(|st| st.authenticate_topic_attempt(&attempt.id, token))?;
            if cancel.is_cancelled() || self.shutdown.is_cancelled() {
                bail!("step cancelled before launch");
            }
        }
        let run_cancel = cancel.child_token();
        let stop = run_cancel.clone();
        let shutdown = self.shutdown.clone();
        let watcher = tokio::spawn(async move {
            shutdown.cancelled().await;
            stop.cancel();
        });
        let result =
            local_harness::acp::run_interactive(adapter, request.clone(), handler, &run_cancel)
                .await;
        watcher.abort();
        if self.shutdown.is_cancelled() {
            bail!("daemon is shutting down");
        }
        result
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
