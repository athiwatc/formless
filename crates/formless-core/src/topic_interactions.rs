use super::*;
use crate::local_harness::{
    HarnessPermissionRequest, HarnessUpdate, InteractionFuture, InteractionHandler,
};

pub(super) struct PendingPermission {
    topic_id: String,
    attempt_id: String,
    options: Vec<HarnessPermissionOption>,
    sender: oneshot::Sender<Option<String>>,
}

pub(super) struct TopicDecision {
    topic_id: String,
    feature_id: String,
    kind: TopicDecisionKind,
}
enum TopicDecisionKind {
    Limit(bool),
    Substitute {
        step_title: String,
        wanted: HarnessChoice,
        substitute: HarnessChoice,
        response: oneshot::Sender<String>,
    },
}
#[derive(Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HarnessChoice {
    pub harness: HarnessKind,
    pub acp_agent: Option<String>,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ApprovedSubstitute {
    pub wanted: HarnessChoice,
    pub substitute: HarnessChoice,
}
impl TopicDecision {
    fn options(&self) -> Vec<String> {
        match &self.kind {
            TopicDecisionKind::Limit(_) => vec!["Raise limit and continue".into(), "Stop".into()],
            TopicDecisionKind::Substitute { substitute, .. } => vec![
                "Sign in and retry".into(),
                format!("Use {:?}", substitute.harness),
                "Skip step".into(),
            ],
        }
    }
}
struct DecisionLease {
    daemon: Arc<Daemon>,
    id: String,
}
impl Drop for DecisionLease {
    fn drop(&mut self) {
        if self
            .daemon
            .topic_decisions
            .locked()
            .remove(&self.id)
            .is_some()
        {
            self.daemon.broadcast(Event::UserQuestionClosed {
                question_id: self.id.clone(),
                answered_by: None,
            });
        }
    }
}

pub(super) type AlwaysPermissions = Arc<StdMutex<HashMap<String, HarnessPermissionOptionKind>>>;

fn rule_eligible(kind: &str) -> bool {
    !matches!(kind, "trust_workspace")
}

pub(super) fn permission_glob(pattern: &str, value: &str) -> bool {
    let pattern: Vec<_> = pattern.chars().collect();
    let value: Vec<_> = value.chars().collect();
    let (mut p, mut v, mut star, mut retry) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            retry = v;
        } else if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if let Some(previous) = star {
            retry += 1;
            v = retry;
            p = previous + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

fn permission_option_ids(request: &HarnessPermissionRequest) -> Result<()> {
    let mut ids = std::collections::HashSet::new();
    if request
        .options
        .iter()
        .any(|option| option.option_id.is_empty() || !ids.insert(&option.option_id))
    {
        bail!("permission option IDs must be unique and nonempty");
    }
    Ok(())
}

enum RulePermission {
    Unmatched,
    Prompt,
    Decided(String),
}

struct Handler {
    daemon: Arc<Daemon>,
    attempt_id: String,
    token: String,
    always: AlwaysPermissions,
    open: StdMutex<Option<MessageInfo>>,
}

impl InteractionHandler for Handler {
    fn request_permission<'a>(
        &'a self,
        request: HarnessPermissionRequest,
        cancel: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<String>> {
        Box::pin(async move {
            if cancel.is_cancelled() || self.daemon.shutdown.is_cancelled() {
                return None;
            }
            let attempt = self
                .daemon
                .with_reader(|st| st.authenticate_topic_attempt(&self.attempt_id, &self.token))
                .ok()?;
            permission_option_ids(&request).ok()?;
            let rule = self
                .daemon
                .apply_topic_permission_rule(&attempt, &self.token, &request)
                .ok()?;
            if let RulePermission::Decided(answer) = rule {
                return Some(answer);
            }
            let key = json!([request.kind, request.title, request.raw_input]).to_string();
            let cached = (rule_eligible(&request.kind)
                && matches!(rule, RulePermission::Unmatched))
            .then(|| self.always.locked().get(&key).copied())
            .flatten();
            if let Some(kind) = cached {
                let allow = kind == HarnessPermissionOptionKind::AllowAlways;
                return request
                    .options
                    .iter()
                    .find(|option| {
                        matches!(
                            (allow, option.kind),
                            (
                                true,
                                HarnessPermissionOptionKind::AllowOnce
                                    | HarnessPermissionOptionKind::AllowAlways
                            ) | (
                                false,
                                HarnessPermissionOptionKind::RejectOnce
                                    | HarnessPermissionOptionKind::RejectAlways
                            )
                        )
                    })
                    .map(|option| option.option_id.clone());
            }
            let eligible = rule_eligible(&request.kind);
            let options = request.options.clone();
            let answer = self
                .daemon
                .wait_topic_permission(&attempt, &self.token, request, cancel)
                .await
                .ok()
                .flatten();
            if let Some(kind) = answer
                .as_ref()
                .and_then(|id| options.iter().find(|option| &option.option_id == id))
                .map(|option| option.kind)
                .filter(|kind| {
                    eligible
                        && matches!(
                            kind,
                            HarnessPermissionOptionKind::AllowAlways
                                | HarnessPermissionOptionKind::RejectAlways
                        )
                })
            {
                self.always.locked().insert(key, kind);
            }
            answer
        })
    }

    fn ask_question<'a>(
        &'a self,
        questions: Vec<Question>,
        cancel: &'a CancellationToken,
    ) -> InteractionFuture<'a, Option<Vec<QuestionAnswer>>> {
        Box::pin(async move {
            self.daemon
                .wait_topic_question(&self.attempt_id, &self.token, questions, cancel)
                .await
                .ok()
                .flatten()
        })
    }

    fn on_update(&self, update: HarnessUpdate) -> Result<()> {
        let mut open = self.open.locked();
        let (kind, content, extra, delta) = match update {
            HarnessUpdate::Message { content } => ("message", content, Value::Null, true),
            HarnessUpdate::Thought { content } => ("thought", content, Value::Null, true),
            HarnessUpdate::Plan { content } => ("plan", content, Value::Null, false),
            HarnessUpdate::ToolCall { name, input } => ("tool_call", name, input, false),
        };
        if delta && content.is_empty() {
            return Ok(());
        }
        if !delta {
            *open = None;
        }
        let message = self.daemon.with_store(|st| {
            st.transaction(|st| {
                let attempt = st.authenticate_topic_attempt(&self.attempt_id, &self.token)?;
                let mut message = match open.as_ref().filter(|_| delta) {
                    Some(message) => message.clone(),
                    None => st.append_message_meta(
                        &attempt.conversation_session_id,
                        "assistant",
                        "",
                        None,
                    )?,
                };
                let mut meta = message
                    .meta
                    .as_ref()
                    .and_then(|meta| serde_json::from_str::<Value>(meta).ok())
                    .unwrap_or(json!({"harness_update":"message"}));
                if kind == "thought" {
                    let previous = meta["thought"].as_str().unwrap_or("");
                    meta["thought"] = json!(format!("{previous}{content}"));
                } else {
                    message.content.push_str(&content);
                }
                if !delta {
                    meta = json!({"harness_update":kind,"input":extra});
                }
                message.meta = Some(meta.to_string());
                st.update_topic_message(&message)?;
                Ok(message)
            })
        })?;
        if delta {
            *open = Some(message.clone());
        }
        self.daemon.broadcast(Event::MessageAppended {
            session_id: message.session_id.clone(),
            message,
        });
        Ok(())
    }
    fn finish(&self) -> Result<()> {
        self.open.locked().take();
        Ok(())
    }
}

struct Lease {
    daemon: Arc<Daemon>,
    id: String,
    attempt_id: String,
    topic_id: String,
    permission: bool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.permission {
            self.daemon.close_topic_permission(&self.id);
        } else {
            self.daemon.questions.locked().remove(&self.id);
            if self
                .daemon
                .topic_questions
                .locked()
                .remove(&self.id)
                .is_some()
            {
                self.daemon.broadcast(Event::UserQuestionClosed {
                    answered_by: self.daemon.question_answered_by.locked().remove(&self.id),
                    question_id: self.id.clone(),
                });
            }
        }
        let _ = self
            .daemon
            .with_store(|st| st.resume_topic_attempt(&self.attempt_id));
        self.daemon.broadcast(Event::TopicChanged {
            topic_id: self.topic_id.clone(),
        });
    }
}

impl Daemon {
    pub(super) fn raise_topic_limit_question(
        &self,
        topic: &TopicInfo,
        feature: &str,
        total: bool,
    ) -> Result<()> {
        if self.topic_decisions.locked().values().any(|decision| {
            decision.topic_id == topic.id
                && decision.feature_id == feature
                && matches!(decision.kind,TopicDecisionKind::Limit(hit) if hit==total)
        }) {
            return Ok(());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let title = format!(
            "Limit reached: {}",
            if total {
                "attempts per topic"
            } else {
                "retries for this step"
            }
        );
        let message = self.with_store(|st| {
            st.append_message(
                topic
                    .topic_session_id
                    .as_ref()
                    .context("missing Topic Chat")?,
                "system-note",
                &title,
            )
        })?;
        self.broadcast(Event::MessageAppended {
            session_id: message.session_id.clone(),
            message,
        });
        self.topic_decisions.locked().insert(
            id.clone(),
            TopicDecision {
                topic_id: topic.id.clone(),
                feature_id: feature.into(),
                kind: TopicDecisionKind::Limit(total),
            },
        );
        self.broadcast(Event::UserQuestion {
            question_id: id,
            session_id: topic
                .topic_session_id
                .clone()
                .context("missing Topic Chat")?,
            topic_id: Some(topic.id.clone()),
            attempt_id: None,
            user_only: true,
            question: title.clone(),
            options: vec!["Raise limit and continue".into(), "Stop".into()],
            questions: vec![Question::single(
                title,
                vec!["Raise limit and continue".into(), "Stop".into()],
            )],
        });
        self.broadcast(Event::TopicChanged {
            topic_id: topic.id.clone(),
        });
        Ok(())
    }
    pub(super) fn answer_topic_decision(
        &self,
        id: &str,
        answer: &str,
        answers: &[QuestionAnswer],
    ) -> Result<Value> {
        let selected = if answers.is_empty() {
            answer
        } else {
            if answers.len() != 1
                || answers[0].selected.len() != 1
                || answers[0]
                    .text
                    .as_ref()
                    .is_some_and(|text| !text.trim().is_empty())
            {
                bail!("select one offered option");
            }
            &answers[0].selected[0]
        };
        let topic = self.with_store(|st| {
            let mut decisions = self.topic_decisions.locked();
            let decision = decisions.get(id).context("question no longer pending")?;
            if !decision.options().iter().any(|option| option == selected) {
                bail!("select one offered option");
            }
            if let TopicDecisionKind::Limit(total) = decision.kind {
                if selected == "Raise limit and continue" {
                    st.raise_topic_limit(&decision.topic_id, &decision.feature_id, total)?;
                }
            } else {
                let topic = st
                    .get_topic(&decision.topic_id)?
                    .context("topic vanished")?;
                if !matches!(topic.status, TopicStatus::Running | TopicStatus::NeedsUser) {
                    bail!("topic is no longer active");
                }
            }
            let decision = decisions.remove(id).context("question no longer pending")?;
            if let TopicDecisionKind::Substitute {
                step_title,
                wanted,
                substitute,
                response,
            } = decision.kind
            {
                if selected == format!("Use {:?}", substitute.harness) {
                    self.topic_substitutes.locked().insert(
                        (decision.topic_id.clone(), decision.feature_id, step_title),
                        ApprovedSubstitute { wanted, substitute },
                    );
                }
                let _ = response.send(selected.to_owned());
            }
            Ok(decision.topic_id)
        })?;
        self.broadcast(Event::UserQuestionClosed {
            question_id: id.into(),
            answered_by: Some("user".into()),
        });
        self.broadcast(Event::TopicChanged { topic_id: topic });
        Ok(json!({"answered":true}))
    }
    pub(super) async fn request_topic_harness_substitute(
        self: &Arc<Self>,
        session: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Request {
            feature_id: String,
            step_title: String,
            wanted: HarnessChoice,
            substitute: HarnessChoice,
            reason: String,
        }
        let request: Request = serde_json::from_value(args.clone())?;
        for choice in [&request.wanted, &request.substitute] {
            crate::local_harness::validate_acp_agent(
                Some(choice.harness),
                choice.acp_agent.as_deref(),
            )?;
        }
        if request.step_title.trim().is_empty()
            || request.reason.trim().is_empty()
            || request.reason.len() > 8192
        {
            bail!("step title and bounded reason are required");
        }
        if request.wanted == request.substitute {
            bail!("wanted and substitute must differ");
        }
        let topic = self
            .with_reader(|st| st.topic_for_session(session))?
            .context("not a Topic Chat")?;
        if !matches!(topic.status, TopicStatus::Running | TopicStatus::NeedsUser)
            || !topic.features.iter().any(|feature| {
                feature.id == request.feature_id
                    && matches!(
                        feature.status,
                        TopicFeatureStatus::Pending | TopicFeatureStatus::NeedsUser
                    )
            })
        {
            bail!("feature is not pending in an active topic");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (response, receiver) = oneshot::channel();
        let question = format!(
            "Use {:?} instead of {:?} for {}? {}",
            request.substitute.harness,
            request.wanted.harness,
            request.step_title.trim(),
            request.reason
        );
        let decision = TopicDecision {
            topic_id: topic.id.clone(),
            feature_id: request.feature_id,
            kind: TopicDecisionKind::Substitute {
                step_title: request.step_title.trim().into(),
                wanted: request.wanted,
                substitute: request.substitute,
                response,
            },
        };
        let options = decision.options();
        let _lease = DecisionLease {
            daemon: self.clone(),
            id: id.clone(),
        };
        {
            let _launch = self.topic_launch.locked();
            let current = self
                .with_reader(|st| st.get_topic(&topic.id))?
                .context("topic vanished")?;
            if cancel.is_cancelled()
                || self.shutdown.is_cancelled()
                || !matches!(
                    current.status,
                    TopicStatus::Running | TopicStatus::NeedsUser
                )
            {
                bail!("substitute request cancelled");
            }
            self.topic_decisions.locked().insert(id.clone(), decision);
            self.broadcast(Event::UserQuestion {
                question_id: id,
                session_id: session.into(),
                topic_id: Some(topic.id.clone()),
                attempt_id: None,
                user_only: true,
                question: question.clone(),
                options: options.clone(),
                questions: vec![Question::single(question, options)],
            });
        }
        tokio::select! {
            _=cancel.cancelled()=>bail!("substitute request cancelled"),
            _=self.shutdown.cancelled()=>bail!("daemon shutting down"),
            answer=receiver=>Ok(json!({"answer":answer.context("substitute question cancelled")?})),
        }
    }
    fn apply_topic_permission_rule(
        &self,
        attempt: &TopicAttemptInfo,
        token: &str,
        request: &HarnessPermissionRequest,
    ) -> Result<RulePermission> {
        if !rule_eligible(&request.kind) {
            return Ok(RulePermission::Prompt);
        }
        let (matched, decision) = self.with_store(|st| {
            st.transaction(|st| {
                st.authenticate_topic_attempt(&attempt.id, token)?;
                let topic = st.get_topic(&attempt.topic_id)?.context("topic vanished")?;
                let mut matches = topic.permission_rules.iter().filter(|rule| {
                    rule.harness.is_none_or(|kind| kind == attempt.harness)
                        && rule
                            .acp_agent
                            .as_ref()
                            .is_none_or(|agent| Some(agent) == attempt.acp_agent.as_ref())
                        && rule.kind.as_ref().is_none_or(|kind| kind == &request.kind)
                        && (permission_glob(&rule.pattern, &request.title)
                            || request
                                .raw_input
                                .as_ref()
                                .and_then(|raw| raw.get("command"))
                                .and_then(Value::as_str)
                                .is_some_and(|command| permission_glob(&rule.pattern, command)))
                });
                let first = matches.next();
                let rule = first.and_then(|first| {
                    if first.decision == formless_protocol::TopicRuleDecision::Reject {
                        Some(first)
                    } else {
                        Some(
                            matches
                                .find(|rule| {
                                    rule.decision == formless_protocol::TopicRuleDecision::Reject
                                })
                                .unwrap_or(first),
                        )
                    }
                });
                let Some(rule) = rule else {
                    return Ok((false, None));
                };
                let preferred = if rule.decision == formless_protocol::TopicRuleDecision::Reject {
                    HarnessPermissionOptionKind::RejectOnce
                } else {
                    HarnessPermissionOptionKind::AllowOnce
                };
                let option = request
                    .options
                    .iter()
                    .find(|option| option.kind == preferred)
                    .or_else(|| {
                        request.options.iter().find(|option| {
                            matches!(
                                (preferred, option.kind),
                                (
                                    HarnessPermissionOptionKind::AllowOnce,
                                    HarnessPermissionOptionKind::AllowAlways
                                ) | (
                                    HarnessPermissionOptionKind::RejectOnce,
                                    HarnessPermissionOptionKind::RejectAlways
                                )
                            )
                        })
                    });
                let Some(option) = option else {
                    return Ok((true, None));
                };
                let permission_id = uuid::Uuid::new_v4().to_string();
                let message = st.append_message(
                    &attempt.conversation_session_id,
                    "system-note",
                    &format!(
                        "Permission {permission_id}: option {} selected by rule:{}",
                        option.option_id, rule.rule_id
                    ),
                )?;
                Ok((
                    true,
                    Some((
                        permission_id,
                        rule.rule_id.clone(),
                        option.option_id.clone(),
                        message,
                    )),
                ))
            })
        })?;
        let Some((permission_id, rule, option, message)) = decision else {
            return Ok(if matched {
                RulePermission::Prompt
            } else {
                RulePermission::Unmatched
            });
        };
        self.broadcast(Event::MessageAppended {
            session_id: attempt.conversation_session_id.clone(),
            message,
        });
        self.broadcast(Event::HarnessPermissionRequested {
            permission_id: permission_id.clone(),
            session_id: attempt.conversation_session_id.clone(),
            topic_id: attempt.topic_id.clone(),
            attempt_id: attempt.id.clone(),
            harness: attempt.harness,
            acp_agent: attempt.acp_agent.clone(),
            title: request.title.clone(),
            kind: request.kind.clone(),
            raw_input: request.raw_input.clone(),
            options: request.options.clone(),
        });
        self.broadcast(Event::HarnessPermissionClosed {
            permission_id,
            option_id: Some(option.clone()),
            by: Some(format!("rule:{rule}")),
        });
        Ok(RulePermission::Decided(option))
    }
    pub(super) fn notify_topic_attention(
        &self,
        topic_id: Option<&str>,
        attempt_id: Option<&str>,
        prompt: &str,
    ) {
        if std::env::var("FORMLESS_TOPIC_NOTIFICATIONS")
            .ok()
            .as_deref()
            == Some("0")
        {
            return;
        }
        let topic = topic_id.and_then(|id| self.with_reader(|st| st.get_topic(id)).ok().flatten());
        let harness = attempt_id
            .and_then(|id| {
                self.with_reader(|st| st.get_topic_attempt(id))
                    .ok()
                    .flatten()
            })
            .map(|attempt| format!("{:?}", attempt.harness))
            .unwrap_or_else(|| "Topic orchestration".into());
        let title = format!(
            "Formless: {} needs you",
            topic.map(|t| t.name).unwrap_or_else(|| "Topic".into())
        );
        let body = format!("{harness}: {prompt}");
        let title = crate::secrets::redact(&title)
            .0
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect::<String>();
        let body = crate::secrets::redact(&body)
            .0
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect::<String>();
        #[cfg(test)]
        self.topic_notifications.locked().push((title, body));
        #[cfg(not(test))]
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || {
                formless_runtime::desktop_notify(&title, &body)
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!("topic notification failed: {error}"),
                Err(error) => tracing::warn!("topic notification task failed: {error}"),
            }
        });
    }
    pub(super) fn answer_topic_question_tool(&self, session: &str, args: &Value) -> Result<Value> {
        let basis = args["basis"].as_str().context("basis is required")?.trim();
        if basis.is_empty() {
            bail!("basis must quote or reference a user message or decision");
        }
        let question_id = args["question_id"]
            .as_str()
            .context("question_id is required")?;
        let topic = self
            .with_reader(|st| st.topic_for_session(session))?
            .context("not a Topic Chat")?;
        let event = self
            .pending_attention
            .locked()
            .iter()
            .find(|event| matches!(event,Event::UserQuestion{question_id:id,..} if id==question_id))
            .cloned()
            .context("question is not pending")?;
        let Event::UserQuestion {
            topic_id,
            attempt_id,
            questions,
            user_only,
            ..
        } = event
        else {
            unreachable!()
        };
        if topic_id.as_deref() != Some(topic.id.as_str()) {
            bail!("question is outside this topic");
        }
        if user_only {
            bail!("this question is user_only and must be answered by the user");
        }
        let attempt_id =
            attempt_id.context("only harness questions can be answered by this tool")?;
        let answers: Vec<QuestionAnswer> = serde_json::from_value(args["answers"].clone())?;
        if answers.len() != questions.len()
            || answers.iter().any(|answer| {
                answer.selected.is_empty()
                    && answer
                        .text
                        .as_deref()
                        .is_none_or(|text| text.trim().is_empty())
            })
        {
            bail!("supply one non-empty answer per question");
        }
        let message=self.with_store(|st|st.transaction(|st|{
            if !st.is_topic_orchestrator(session)?{bail!("topic is not active");}
            let attempt=st.get_topic_attempt(&attempt_id)?.context("attempt not found")?;
            let mut waiters=self.questions.locked();
            if !waiters.contains_key(question_id){return Ok(None);}
            let message=st.append_message_meta(session,"assistant",&format!(
                "Orchestrator answer for {:?}, attempt {}\nQuestions: {}\nAnswers: {}\nBasis: {}",
                attempt.harness,attempt_id,serde_json::to_string(&questions)?,serde_json::to_string(&answers)?,basis
            ),Some(json!({"topic_question_answer":question_id,"answered_by":"model","basis":basis}).to_string()))?;
            self.question_answered_by.locked().insert(question_id.into(),"model".into());
            if waiters.remove(question_id).unwrap().send((String::new(),answers)).is_err(){
                self.question_answered_by.locked().remove(question_id);
                bail!("question expired before delivery");
            }
            Ok(Some(message))
        }))?;
        if let Some(message) = message {
            self.broadcast(Event::MessageAppended {
                session_id: session.into(),
                message,
            });
            Ok(json!({"answered":true}))
        } else {
            Ok(json!({"answered":false}))
        }
    }

    pub(crate) fn topic_interaction_handler(
        self: &Arc<Self>,
        attempt_id: &str,
        capability_token: &str,
    ) -> Result<Arc<dyn InteractionHandler>> {
        self.with_reader(|st| st.authenticate_topic_attempt(attempt_id, capability_token))?;
        Ok(Arc::new(Handler {
            daemon: self.clone(),
            attempt_id: attempt_id.into(),
            token: capability_token.into(),
            open: StdMutex::new(None),
            always: self
                .harness_always
                .locked()
                .entry(attempt_id.into())
                .or_insert_with(|| Arc::new(StdMutex::new(HashMap::new())))
                .clone(),
        }))
    }

    async fn topic_prompt_gate(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        let gate = {
            let mut gates = self.question_gates.locked();
            gates.retain(|_, gate| gate.strong_count() > 0);
            match gates.get(session_id).and_then(std::sync::Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = Arc::new(tokio::sync::Mutex::new(()));
                    gates.insert(session_id.into(), Arc::downgrade(&gate));
                    gate
                }
            }
        };
        tokio::select! {
            _ = cancel.cancelled() => bail!("interaction cancelled"),
            _ = self.shutdown.cancelled() => bail!("daemon is shutting down"),
            guard = gate.lock_owned() => Ok(guard),
        }
    }

    async fn wait_topic_permission(
        self: &Arc<Self>,
        attempt: &TopicAttemptInfo,
        token: &str,
        request: HarnessPermissionRequest,
        cancel: &CancellationToken,
    ) -> Result<Option<String>> {
        let mut ids = std::collections::HashSet::new();
        if request
            .options
            .iter()
            .any(|option| option.option_id.is_empty() || !ids.insert(&option.option_id))
        {
            bail!("permission option IDs must be unique and nonempty");
        }
        let _gate = self
            .topic_prompt_gate(&attempt.conversation_session_id, cancel)
            .await?;
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let permission_id = uuid::Uuid::new_v4().to_string();
        let notification_title = request.title.clone();
        let (sender, mut receiver) = oneshot::channel();
        self.with_store(|st| {
            st.transaction(|st| {
                st.pause_topic_attempt(&attempt.id, token)?;
                self.harness_permissions.locked().insert(
                    permission_id.clone(),
                    PendingPermission {
                        topic_id: attempt.topic_id.clone(),
                        attempt_id: attempt.id.clone(),
                        options: request.options.clone(),
                        sender,
                    },
                );
                self.broadcast(Event::HarnessPermissionRequested {
                    permission_id: permission_id.clone(),
                    session_id: attempt.conversation_session_id.clone(),
                    topic_id: attempt.topic_id.clone(),
                    attempt_id: attempt.id.clone(),
                    harness: attempt.harness,
                    acp_agent: attempt.acp_agent.clone(),
                    title: request.title,
                    kind: request.kind,
                    raw_input: request.raw_input,
                    options: request.options,
                });
                Ok(())
            })
        })?;
        self.notify_topic_attention(
            Some(&attempt.topic_id),
            Some(&attempt.id),
            &notification_title,
        );
        let _lease = Lease {
            daemon: self.clone(),
            id: permission_id.clone(),
            attempt_id: attempt.id.clone(),
            topic_id: attempt.topic_id.clone(),
            permission: true,
        };
        self.broadcast(Event::TopicChanged {
            topic_id: attempt.topic_id.clone(),
        });
        let answer = tokio::select! {
            _ = cancel.cancelled() => self.cancel_permission_wait(&permission_id, &mut receiver),
            _ = self.shutdown.cancelled() => self.cancel_permission_wait(&permission_id, &mut receiver),
            answer = &mut receiver => answer.ok().flatten(),
        };
        Ok(answer)
    }

    fn cancel_permission_wait(
        &self,
        id: &str,
        receiver: &mut oneshot::Receiver<Option<String>>,
    ) -> Option<String> {
        if self.close_topic_permission(id) {
            None
        } else {
            receiver.try_recv().ok().flatten()
        }
    }

    fn close_topic_permission(&self, permission_id: &str) -> bool {
        let removed = self.harness_permissions.locked().remove(permission_id);
        if removed.is_some() {
            self.broadcast(Event::HarnessPermissionClosed {
                permission_id: permission_id.into(),
                option_id: None,
                by: None,
            });
        }
        removed.is_some()
    }

    pub(super) fn close_topic_decisions(&self, topic_id: &str, feature_id: Option<&str>) {
        self.topic_substitutes
            .locked()
            .retain(|(topic, feature, _), _| {
                topic != topic_id || feature_id.is_some_and(|id| id != feature)
            });
        let ids = {
            let mut decisions = self.topic_decisions.locked();
            let ids: Vec<_> = decisions
                .iter()
                .filter(|(_, decision)| {
                    decision.topic_id == topic_id
                        && feature_id.is_none_or(|id| id == decision.feature_id)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in &ids {
                decisions.remove(id);
            }
            ids
        };
        for question_id in ids {
            self.broadcast(Event::UserQuestionClosed {
                question_id,
                answered_by: None,
            });
        }
    }
    pub(super) fn close_topic_interactions(&self, topic_id: &str, attempt_id: Option<&str>) {
        if attempt_id.is_none() {
            self.close_topic_decisions(topic_id, None);
        }
        let attempts = match attempt_id {
            Some(id) => vec![id.to_owned()],
            None => self
                .with_reader(|st| st.list_topic_attempts(topic_id))
                .unwrap_or_default()
                .into_iter()
                .map(|attempt| attempt.id)
                .collect(),
        };
        for id in attempts {
            self.harness_always.locked().remove(&id);
        }
        let ids: Vec<_> = self
            .harness_permissions
            .locked()
            .iter()
            .filter(|(_, p)| {
                p.topic_id == topic_id && attempt_id.is_none_or(|id| id == p.attempt_id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.close_topic_permission(&id);
        }
        let ids: Vec<_> = self
            .topic_questions
            .locked()
            .iter()
            .filter(|(_, (topic, attempt))| {
                topic == topic_id && attempt_id.is_none_or(|id| id == attempt)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.questions.locked().remove(&id);
            if self.topic_questions.locked().remove(&id).is_some() {
                self.broadcast(Event::UserQuestionClosed {
                    answered_by: self.question_answered_by.locked().remove(&id),
                    question_id: id,
                });
            }
        }
    }

    pub(super) fn answer_harness_permission(
        &self,
        id: &str,
        option_id: Option<String>,
    ) -> Result<Value> {
        let topic_id = self.with_store(|st| {
            let mut permissions = self.harness_permissions.locked();
            let pending = permissions
                .get(id)
                .context("permission is no longer pending")?;
            if option_id
                .as_ref()
                .is_some_and(|id| !pending.options.iter().any(|option| &option.option_id == id))
            {
                bail!("permission option was not offered");
            }
            if !st.resume_topic_attempt(&pending.attempt_id)? {
                bail!("attempt is no longer waiting for permission");
            }
            let pending = permissions.remove(id).context("permission vanished")?;
            let _ = pending.sender.send(option_id.clone());
            Ok(pending.topic_id)
        })?;
        self.broadcast(Event::HarnessPermissionClosed {
            permission_id: id.into(),
            option_id,
            by: current_client().map(|(_, label)| label),
        });
        self.broadcast(Event::TopicChanged { topic_id });
        Ok(json!({"answered":true}))
    }

    async fn wait_topic_question(
        self: &Arc<Self>,
        attempt_id: &str,
        token: &str,
        questions: Vec<Question>,
        cancel: &CancellationToken,
    ) -> Result<Option<Vec<QuestionAnswer>>> {
        let questions = parse_questions(&json!({"questions":questions}))?;
        let attempt = self.with_reader(|st| st.authenticate_topic_attempt(attempt_id, token))?;
        let _gate = self
            .topic_prompt_gate(&attempt.conversation_session_id, cancel)
            .await?;
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = oneshot::channel();
        self.with_store(|st| {
            st.transaction(|st| {
                st.pause_topic_attempt(attempt_id, token)?;
                self.questions.locked().insert(id.clone(), sender);
                self.topic_questions
                    .locked()
                    .insert(id.clone(), (attempt.topic_id.clone(), attempt.id.clone()));
                self.broadcast(Event::UserQuestion {
                    user_only: false,
                    question_id: id.clone(),
                    session_id: attempt.conversation_session_id.clone(),
                    topic_id: Some(attempt.topic_id.clone()),
                    attempt_id: Some(attempt.id.clone()),
                    question: questions
                        .iter()
                        .map(|q| q.question.clone())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    options: if questions.len() == 1 {
                        questions[0]
                            .options
                            .iter()
                            .map(|o| o.label.clone())
                            .collect()
                    } else {
                        vec![]
                    },
                    questions: questions.clone(),
                });
                Ok(())
            })
        })?;
        let _lease = Lease {
            daemon: self.clone(),
            id: id.clone(),
            attempt_id: attempt_id.into(),
            topic_id: attempt.topic_id.clone(),
            permission: false,
        };
        self.broadcast(Event::TopicChanged {
            topic_id: attempt.topic_id,
        });
        let reply = prompt_reply::wait_until(
            &self.questions,
            &id,
            receiver,
            async {
                tokio::select! {_=self.shutdown.cancelled()=>{},_=cancel.cancelled()=>{}}
            },
            || {},
        )
        .await;
        Ok(reply.map(|(answer, answers)| {
            let answers = if answers.is_empty() {
                vec![questions[0].parse_reply(&answer)]
            } else {
                answers
            };
            answers
                .into_iter()
                .chain(std::iter::repeat_with(QuestionAnswer::default))
                .take(questions.len())
                .collect()
        }))
    }
}
