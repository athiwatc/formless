use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{bail, ensure, Context, Result};
use formless_protocol::{
    HarnessKind, TopicFeatureProposal, TopicInfo, TopicProposalRequest, TopicStatus,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::daemon::Daemon;
use crate::storage::Store;

pub(crate) const READ_TOPIC_CONTEXT: &str = "read_topic_context";
pub(crate) const PROPOSE_TOPIC: &str = "propose_topic";
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
                let topic = self.propose_topic_request(
                    &TopicProposalRequest {
                        source_session_id: session_id.into(),
                        name: args.name,
                        message_ids: args.message_ids,
                        repo: args.repo,
                        builder: args.builder,
                        builder_model: args.builder_model,
                        reviewer: args.reviewer,
                        reviewer_model: args.reviewer_model,
                        features: args.features,
                    },
                    true,
                )?;
                ensure!(
                    topic.status == TopicStatus::PendingApproval,
                    "topic is not pending approval"
                );
                Ok(serde_json::to_value(topic)?)
            }
            _ => bail!("unknown topic suggestion tool"),
        }
    }
}

impl Store {
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

    pub(crate) fn existing_topic_suggestion(
        &self,
        proposal: &TopicProposalRequest,
    ) -> Result<Option<TopicInfo>> {
        let source = &proposal.source_session_id;
        ensure!(
            self.is_main_chat(source)? && !self.session_deleted(source)?,
            "topic suggestions must originate in Main Chat"
        );
        let selected: BTreeSet<_> = proposal.message_ids.iter().map(String::as_str).collect();
        ensure!(
            !selected.is_empty() && selected.len() == proposal.message_ids.len(),
            "select one or more unique source user messages"
        );
        let represented: BTreeSet<_> = proposal
            .features
            .iter()
            .map(|feature| feature.source_message_id.as_str())
            .collect();
        ensure!(
            represented == selected,
            "every selected source message must have a feature request"
        );
        let mut covered = BTreeSet::new();
        for topic in self
            .list_topics()?
            .into_iter()
            .filter(|topic| topic.source_session_id == *source)
        {
            let sources = source_ids(&topic);
            if topic.status == TopicStatus::PendingApproval && sources == selected {
                return Ok(Some(topic));
            }
            covered.extend(sources.intersection(&selected).copied().map(str::to_owned));
        }
        ensure!(
            covered.len() < selected.len(),
            "these messages already belong to a topic proposal; select a new explicit work request"
        );
        Ok(None)
    }
}

fn source_ids(topic: &TopicInfo) -> BTreeSet<&str> {
    topic
        .features
        .iter()
        .map(|feature| feature.source_message_id.as_str())
        .collect()
}
