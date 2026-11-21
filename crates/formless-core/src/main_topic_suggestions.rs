use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, ensure, Context, Result};
use formless_protocol::{
    HarnessKind, SessionInfo, TopicFeatureInfo, TopicFeatureProposal, TopicFeatureStatus,
    TopicInfo, TopicProposalRequest, TopicStatus,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::daemon::Daemon;
use crate::storage::Store;

pub(crate) const READ_TOPIC_CONTEXT: &str = "read_topic_context";
pub(crate) const PROPOSE_TOPIC: &str = "propose_topic";
const NAMESPACE: &str = "harness:topics";
const MAIN_KEY: &str = "main";
const TOPIC_PREFIX: &str = "topic:";
const POLICY: &str = r#"Main Chat topic suggestions:
Answer everyday chat, questions and brainstorming normally in Main. For an explicit request to do work in a named repo or project, use read_topic_context to get the persisted source user message IDs and local working directory, then propose_topic rather than starting work. One user message can request multiple independent features: put each in its own feature request, linked to that same original message ID, in one proposal. Do not create synthetic messages. Use the user's stated or standing builder and reviewer preferences and a known local repository path (the working directory is only a hint); ask in chat if the repo or workflow is unclear. Do not guess. Do not automatically re-propose cancelled or declined work. A proposal is pending only: the user must explicitly approve in the client's consent UI. A yes in chat is not approval to move messages, start work, run a harness, or use other tools to do the proposed work."#;

pub(crate) fn is_topic_tool(name: &str) -> bool {
    matches!(name, READ_TOPIC_CONTEXT | PROPOSE_TOPIC)
}

pub(crate) fn system_prompt<'a>(
    store: &Store,
    session_id: &str,
    prompt: &'a str,
) -> Result<Cow<'a, str>> {
    if store.is_main_chat(session_id)? && !prompt.contains(POLICY) {
        Ok(Cow::Owned(format!("{prompt}\n\n{POLICY}")))
    } else {
        Ok(Cow::Borrowed(prompt))
    }
}

pub(crate) fn definitions() -> [Value; 2] {
    [
        json!({"type": "function", "function": {
            "name": READ_TOPIC_CONTEXT,
            "description": "Read recent Main Chat user message IDs and their original text, the local working directory, and recent topic proposal statuses. Use only to prepare a topic suggestion for explicit project work.",
            "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
        }}),
        json!({"type": "function", "function": {
            "name": PROPOSE_TOPIC,
            "description": "Suggest a pending topic for explicit project work in Main Chat. Use the user's repo and preferred builder and reviewer. Each independent feature gets its own request linked to the original user message. This does not approve, move messages, or start work.",
            "parameters": {
                "type": "object", "additionalProperties": false,
                "required": ["name", "message_ids", "repo", "builder", "reviewer", "features"],
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "message_ids": {"type": "array", "minItems": 1, "uniqueItems": true, "items": {"type": "string"}},
                    "repo": {"type": "string", "description": "Known absolute path to the user's local Git repository root."},
                    "builder": {"type": "string", "enum": ["claude", "codex"]},
                    "builder_model": {"type": ["string", "null"]},
                    "reviewer": {"type": "string", "enum": ["claude", "codex"]},
                    "reviewer_model": {"type": ["string", "null"]},
                    "features": {"type": "array", "minItems": 1, "items": {
                        "type": "object", "additionalProperties": false,
                        "required": ["source_message_id", "request"],
                        "properties": {
                            "source_message_id": {"type": "string"},
                            "request": {"type": "string", "minLength": 1}
                        }
                    }}
                }
            }
        }}),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalArgs {
    name: String,
    message_ids: Vec<String>,
    repo: PathBuf,
    builder: HarnessKind,
    builder_model: Option<String>,
    reviewer: HarnessKind,
    reviewer_model: Option<String>,
    features: Vec<TopicFeatureProposal>,
}

impl Daemon {
    pub(crate) fn check_topic_tool_access(
        &self,
        session_id: &str,
        agent_depth: usize,
    ) -> Result<()> {
        ensure!(
            agent_depth == 0 && self.with_reader(|st| st.is_main_chat(session_id))?,
            "topic suggestion tools are only available to the Main Chat assistant"
        );
        Ok(())
    }

    pub(crate) fn execute_topic_suggestion(
        &self,
        session_id: &str,
        tool: &str,
        args: &Value,
    ) -> Result<Value> {
        match tool {
            READ_TOPIC_CONTEXT => {
                ensure!(
                    args.as_object().is_some_and(|args| args.is_empty()),
                    "read_topic_context takes no arguments"
                );
                self.with_reader(|st| st.topic_context(session_id))
            }
            PROPOSE_TOPIC => {
                let args =
                    ProposalArgs::deserialize(args).context("invalid propose_topic arguments")?;
                ensure!(
                    !args.features.is_empty(),
                    "include at least one explicit feature request"
                );
                let topic = self.propose_topic_request(&TopicProposalRequest {
                    source_session_id: session_id.into(),
                    name: args.name,
                    message_ids: args.message_ids,
                    repo: args.repo,
                    builder: args.builder,
                    builder_model: args.builder_model,
                    reviewer: args.reviewer,
                    reviewer_model: args.reviewer_model,
                    features: args.features,
                })?;
                ensure!(
                    topic.status == TopicStatus::PendingApproval,
                    "topic is not pending approval"
                );
                Ok(serde_json::to_value(topic)?)
            }
            _ => bail!("unknown topic suggestion tool"),
        }
    }

    pub(crate) fn propose_topic_request(
        &self,
        proposal: &TopicProposalRequest,
    ) -> Result<TopicInfo> {
        self.with_reader(|st| {
            ensure!(
                st.is_main_chat(&proposal.source_session_id)?,
                "topic proposals must originate in Main Chat"
            );
            Ok(())
        })?;
        let (repo, base_revision) = repository(&proposal.repo)?;
        self.with_store(|st| st.propose_topic(proposal, repo, base_revision))
    }
}

fn repository(path: &Path) -> Result<(PathBuf, String)> {
    ensure!(path.is_absolute(), "repo must be an absolute local path");
    let repo = path.canonicalize().context("local repo does not exist")?;
    ensure!(repo.is_dir(), "repo must be a directory");
    let output = Command::new("git")
        .current_dir(&repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .args(["rev-parse", "--show-toplevel", "HEAD^{commit}"])
        .output()
        .context("could not inspect local Git repository")?;
    ensure!(
        output.status.success(),
        "repo must be a local Git repository with a commit"
    );
    let stdout = String::from_utf8(output.stdout)?;
    let (root, revision) = stdout
        .trim_end()
        .rsplit_once('\n')
        .context("could not identify repo root and revision")?;
    ensure!(
        Path::new(root.trim_end_matches('\r')).canonicalize()? == repo,
        "repo must be the Git repository root"
    );
    Ok((repo, revision.into()))
}

impl Store {
    pub(crate) fn is_main_chat(&self, session_id: &str) -> Result<bool> {
        Ok(self
            .kv_get(NAMESPACE, MAIN_KEY)?
            .as_ref()
            .and_then(Value::as_str)
            == Some(session_id)
            && !self.session_deleted(session_id)?
            && self.get_session(session_id)?.is_some())
    }

    pub(crate) fn main_chat(&self) -> Result<SessionInfo> {
        self.transaction(|st| {
            if let Some(id) = st
                .kv_get(NAMESPACE, MAIN_KEY)?
                .and_then(|v| v.as_str().map(str::to_owned))
            {
                if let Some(session) = st.get_session(&id)? {
                    return Ok(session);
                }
            }
            let session = st.create_session(Some("Main".into()))?;
            st.kv_set(NAMESPACE, MAIN_KEY, &json!(session.id))?;
            Ok(session)
        })
    }

    pub(crate) fn get_topic(&self, id: &str) -> Result<Option<TopicInfo>> {
        self.kv_get(NAMESPACE, &format!("{TOPIC_PREFIX}{id}"))?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    pub(crate) fn list_topics(&self) -> Result<Vec<TopicInfo>> {
        let mut topics = Vec::new();
        let mut after = None;
        loop {
            let keys = self.kv_list_page(NAMESPACE, TOPIC_PREFIX, after.as_deref())?;
            if keys.is_empty() {
                break;
            }
            for key in &keys {
                if let Some(value) = self.kv_get(NAMESPACE, key)? {
                    topics.push(serde_json::from_value::<TopicInfo>(value)?);
                }
            }
            after = keys.into_iter().last();
        }
        topics.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        Ok(topics)
    }

    fn save_topic(&self, topic: &TopicInfo) -> Result<()> {
        self.kv_set(
            NAMESPACE,
            &format!("{TOPIC_PREFIX}{}", topic.id),
            &serde_json::to_value(topic)?,
        )
    }

    pub(crate) fn reject_topic(&self, id: &str) -> Result<TopicInfo> {
        self.transaction(|st| {
            let mut topic = st.get_topic(id)?.context("topic not found")?;
            match topic.status {
                TopicStatus::Rejected => return Ok(topic),
                TopicStatus::PendingApproval => {}
                _ => bail!("only pending topics can be rejected"),
            }
            topic.status = TopicStatus::Rejected;
            topic.updated_at = chrono::Utc::now().to_rfc3339();
            st.save_topic(&topic)?;
            Ok(topic)
        })
    }

    fn topic_context(&self, session_id: &str) -> Result<Value> {
        ensure!(
            self.is_main_chat(session_id)?,
            "topic context is only available in Main Chat"
        );
        let session = self
            .get_session(session_id)?
            .context("Main Chat not found")?;
        let proposals: Vec<_> = self
            .list_topics()?
            .into_iter()
            .filter(|topic| topic.source_session_id == session_id)
            .take(8)
            .map(|topic| {
                json!({"id": topic.id, "name": topic.name, "status": topic.status,
                "message_ids": source_ids(&topic)})
            })
            .collect();
        Ok(
            json!({"cwd": session.cwd, "messages": self.recent_topic_messages(session_id)?, "proposals": proposals}),
        )
    }

    fn propose_topic(
        &self,
        proposal: &TopicProposalRequest,
        repo: PathBuf,
        base_revision: String,
    ) -> Result<TopicInfo> {
        self.transaction(|st| {
            let source = &proposal.source_session_id;
            ensure!(st.is_main_chat(source)?, "topic proposals must originate in Main Chat");
            ensure!(!proposal.name.trim().is_empty() && proposal.name.len() <= 200, "topic name must be 1–200 bytes");
            ensure!(!proposal.message_ids.is_empty(), "select at least one source user message");
            let mut messages = BTreeMap::new();
            for id in &proposal.message_ids {
                let message = st.get_message(source, id)?
                    .filter(|message| message.role == "user" && !message.content.trim().is_empty())
                    .context("each source message must be a nonempty Main Chat user message")?;
                ensure!(messages.insert(id.as_str(), message).is_none(), "source message IDs must be unique");
            }
            let mut represented = BTreeSet::new();
            let mut requests = BTreeSet::new();
            let mut features = Vec::new();
            let fallback;
            let drafts = if proposal.features.is_empty() {
                fallback = proposal.message_ids.iter().map(|id| TopicFeatureProposal {
                    source_message_id: id.clone(), request: messages[id.as_str()].content.clone(),
                }).collect::<Vec<_>>();
                &fallback
            } else {
                &proposal.features
            };
            for draft in drafts {
                let id = draft.source_message_id.as_str();
                let message = messages.get(id).context("each feature must reference a selected source message")?;
                let request = draft.request.trim();
                ensure!(!request.is_empty(), "feature requests must not be empty");
                ensure!(requests.insert((id, request)), "feature requests for the same message must be distinct");
                represented.insert(id);
                features.push(TopicFeatureInfo {
                    id: uuid::Uuid::new_v4().to_string(), source_message_id: id.into(),
                    source_seq: message.seq, request: request.into(), status: TopicFeatureStatus::Pending,
                    active_attempt_id: None, ready_revision: None, review_round: 0, detail: None,
                });
            }
            ensure!(represented.len() == messages.len(), "every selected message needs a feature request");
            let mut handled = BTreeSet::new();
            let topics = st.list_topics()?;
            for topic in topics.iter().filter(|topic| topic.source_session_id == *source) {
                let ids = source_ids(topic);
                if topic.status == TopicStatus::PendingApproval && ids == represented {
                    return Ok(topic.clone());
                }
                handled.extend(ids);
            }
            ensure!(!represented.is_subset(&handled), "these messages already belong to a topic proposal; select a new explicit work request");
            let now = chrono::Utc::now().to_rfc3339();
            let topic = TopicInfo {
                id: uuid::Uuid::new_v4().to_string(), name: proposal.name.trim().into(),
                repo, base_revision, source_session_id: source.clone(), topic_session_id: None,
                status: TopicStatus::PendingApproval, builder: proposal.builder, builder_model: proposal.builder_model.clone(),
                reviewer: proposal.reviewer, reviewer_model: proposal.reviewer_model.clone(),
                features, attempts: vec![], created_at: now.clone(), updated_at: now,
            };
            st.save_topic(&topic)?;
            Ok(topic)
        })
    }
}

fn source_ids(topic: &TopicInfo) -> BTreeSet<&str> {
    topic
        .features
        .iter()
        .map(|feature| feature.source_message_id.as_str())
        .collect()
}
