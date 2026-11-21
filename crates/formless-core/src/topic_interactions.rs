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

pub(super) type AlwaysPermissions = Arc<StdMutex<HashMap<String, HarnessPermissionOptionKind>>>;

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
            let key = json!([request.kind, request.title, request.raw_input]).to_string();
            let cached = self.always.locked().get(&key).copied();
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
                    matches!(
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

    pub(super) fn close_topic_interactions(&self, topic_id: &str, attempt_id: Option<&str>) {
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
