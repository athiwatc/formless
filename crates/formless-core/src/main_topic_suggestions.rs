use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{bail, ensure, Context, Result};
use formless_protocol::{
    TopicFeatureProposal, TopicInfo, TopicProposalRequest, TopicStatus, TopicStepOutline,
    WorkspaceKind,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::daemon::Daemon;
use crate::storage::Store;

pub(crate) const READ_TOPIC_CONTEXT: &str = "read_topic_context";
pub(crate) const PROPOSE_TOPIC: &str = "propose_topic";
const POLICY: &str = r#"Topic suggestions:
Answer everyday chat, questions and brainstorming normally. For an explicit request to do work that belongs in a topic (such as changes in a named repo or a new accounting flow), use read_topic_context to get the persisted source user message IDs, topic tree and local working directory, then propose_topic rather than starting the proposed work. In a Topic Chat, continue the current topic's work normally; propose a child or another topic only for distinct work. Choose the closest existing topic that is neither pending approval nor rejected as parent when the request clearly belongs there. Otherwise omit the parent: from a Topic Chat this defaults to the current topic, and from Main the proposal sits directly under Main.
One user message can request multiple independent work items: put each in its own feature request linked to the same original message ID. Do not create synthetic messages. Include a workspace only when the user names one or a known repo applies; otherwise omit it to use scratch space. The working directory is a hint, not a workspace choice by itself. Use an outline only from the user's stated or standing preferences; otherwise leave it empty. Keep workflow steps within the leaf topic instead of treating steps as child topics. Any supported harness can take any step, including using the same harness for multiple steps. The outline is advisory; the Formless model orchestrating the Topic Chat makes the final decisions.
In a Topic Chat, answer a harness question only when it is not user-only and you can cite a specific user request or decision as the basis. Bring all other harness questions and permission requests to the user; never grant or deny harness permissions yourself. Approval, rejection, cancellation, permission rules, topic limits and external context access are user decisions, never assistant tool actions. Do not automatically re-propose cancelled or declined work. A proposal is pending only: the user must explicitly approve in the client's consent UI. A yes in chat is not approval to move messages, start work, run a harness, or use other tools to do the proposed work."#;

pub(crate) enum SuggestionSource {
    Main,
    Topic(String),
}

pub(crate) fn is_topic_tool(name: &str) -> bool {
    matches!(name, READ_TOPIC_CONTEXT | PROPOSE_TOPIC)
}

pub(crate) fn system_prompt<'a>(
    store: &Store,
    session_id: &str,
    prompt: &'a str,
) -> Result<Cow<'a, str>> {
    if store.topic_suggestion_source(session_id)?.is_some() && !prompt.contains(POLICY) {
        Ok(Cow::Owned(format!("{prompt}\n\n{POLICY}")))
    } else {
        Ok(Cow::Borrowed(prompt))
    }
}

pub(crate) fn definitions() -> [Value; 2] {
    [
        json!({"type": "function", "function": {
            "name": READ_TOPIC_CONTEXT,
            "description": "Read recent user message IDs and original text from this chat, the local working directory, and the topic hierarchy. Use only to prepare a topic suggestion for explicit work.",
            "parameters": {"type": "object", "properties": {}, "additionalProperties": false}
        }}),
        json!({"type": "function", "function": {
            "name": PROPOSE_TOPIC,
            "description": "Suggest a pending topic for explicit work in Main or a Topic Chat. Each independent work item gets its own feature linked to the original user message. An optional outline is advisory. This does not approve, move messages, or start work.",
            "parameters": {
                "type": "object", "additionalProperties": false,
                "required": ["name", "message_ids", "features"],
                "properties": {
                    "name": {"type": "string", "minLength": 1},
                    "message_ids": {"type": "array", "minItems": 1, "uniqueItems": true, "items": {"type": "string"}},
                    "workspace": {"type": ["string", "null"], "description": "Known absolute workspace path; omit for scratch space."},
                    "parent_topic_id": {"type": ["string", "null"], "description": "Existing parent topic ID; omit to use the current topic, or Main if called there."},
                    "outline": {"type": "array", "items": {
                        "type": "object", "additionalProperties": false,
                        "required": ["title", "instructions", "access"],
                        "properties": {
                            "title": {"type": "string"},
                            "instructions": {"type": "string"},
                            "harness": {"enum": ["claude", "codex", "pi", "opencode", "antigravity", "acp", "muse", null]},
                            "acp_agent": {"type": ["string", "null"], "description": "Required for the acp harness; omit for all other harnesses."},
                            "model": {"type": ["string", "null"]},
                            "access": {"type": "string", "enum": ["read_only", "write"]}
                        }
                    }},
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
    workspace: Option<PathBuf>,
    parent_topic_id: Option<String>,
    #[serde(default)]
    outline: Vec<TopicStepOutline>,
    features: Vec<TopicFeatureProposal>,
}

impl Daemon {
    pub(crate) fn check_topic_tool_access(
        &self,
        session_id: &str,
        agent_depth: usize,
    ) -> Result<()> {
        ensure!(
            agent_depth == 0
                && self.with_reader(|st| Ok(st.topic_suggestion_source(session_id)?.is_some()))?,
            "topic suggestion tools are only available to the Main or Topic Chat assistant"
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
                let topic = self.propose_topic_suggestion(&TopicProposalRequest {
                    source_session_id: session_id.into(),
                    name: args.name,
                    message_ids: args.message_ids,
                    workspace: args.workspace,
                    parent_topic_id: args.parent_topic_id,
                    outline: args.outline,
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
}

impl Store {
    fn topic_context(&self, session_id: &str) -> Result<Value> {
        let source = self
            .topic_suggestion_source(session_id)?
            .context("topic context is only available in Main or Topic Chat")?;
        let session = self
            .get_session(session_id)?
            .context("source chat not found")?;
        let topics = self.list_topics()?;
        let scope = source
            .topic_id()
            .map(|id| suggestion_tree_scope(&topics, id))
            .transpose()?
            .flatten();
        let visible = |topic: &&TopicInfo| {
            scope
                .as_ref()
                .is_none_or(|ids| ids.contains(topic.id.as_str()))
        };
        let tree: Vec<_> = topics
            .iter()
            .filter(visible)
            .map(|topic| {
                json!({"id": topic.id, "name": topic.name, "parent_topic_id": topic.parent_topic_id,
                    "status": topic.status})
            })
            .collect();
        let proposals: Vec<_> = topics
            .iter()
            .filter(visible)
            .filter(|topic| topic.source_session_id == session_id)
            .take(8)
            .map(|topic| {
                json!({"id": topic.id, "name": topic.name, "status": topic.status,
                "message_ids": source_ids(topic), "workspace": proposal_workspace(topic),
                "parent_topic_id": topic.parent_topic_id})
            })
            .collect();
        Ok(json!({
            "cwd": session.cwd,
            "current_topic_id": source.topic_id(),
            "messages": self.recent_topic_messages(session_id)?,
            "topics": tree,
            "proposals": proposals,
        }))
    }

    pub(crate) fn existing_topic_suggestion(
        &self,
        proposal: &TopicProposalRequest,
    ) -> Result<Option<TopicInfo>> {
        let source = &proposal.source_session_id;
        let source_chat = self
            .topic_suggestion_source(source)?
            .context("topic suggestions must originate in Main or Topic Chat")?;
        let parent = proposal
            .parent_topic_id
            .as_deref()
            .or(source_chat.topic_id());
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
        for topic in self.list_topics()?.into_iter().filter(|topic| {
            topic.source_session_id == *source
                && proposal_workspace(topic) == proposal.workspace.as_deref()
                && topic.parent_topic_id.as_deref() == parent
        }) {
            let sources = source_ids(&topic);
            if topic.status == TopicStatus::PendingApproval && sources == selected {
                ensure!(
                    self.topic_source_content_unchanged(&topic.id, source, &selected)?,
                    "a source message changed since the pending proposal; reject it and send a new request before proposing again"
                );
                for step in &proposal.outline {
                    crate::local_harness::validate_acp_agent(
                        step.harness,
                        step.acp_agent.as_deref(),
                    )?;
                }
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

fn suggestion_tree_scope<'a>(
    topics: &'a [TopicInfo],
    current_id: &str,
) -> Result<Option<HashSet<&'a str>>> {
    let by_id: HashMap<_, _> = topics
        .iter()
        .map(|topic| (topic.id.as_str(), topic))
        .collect();
    let current = by_id.get(current_id).context("source topic not found")?;
    if current.allow_external_context {
        return Ok(None);
    }

    let mut visible = HashSet::from([current.id.as_str()]);
    let mut parent = current.parent_topic_id.as_deref();
    while let Some(id) = parent {
        if !visible.insert(id) {
            break;
        }
        parent = by_id
            .get(id)
            .and_then(|topic| topic.parent_topic_id.as_deref());
    }
    Ok(Some(visible))
}

impl SuggestionSource {
    fn topic_id(&self) -> Option<&str> {
        match self {
            Self::Main => None,
            Self::Topic(id) => Some(id),
        }
    }
}

fn source_ids(topic: &TopicInfo) -> BTreeSet<&str> {
    topic
        .features
        .iter()
        .map(|feature| feature.source_message_id.as_str())
        .collect()
}

fn proposal_workspace(topic: &TopicInfo) -> Option<&std::path::Path> {
    if topic.workspace_kind == WorkspaceKind::Scratch {
        None
    } else {
        topic.workspace.as_deref()
    }
}
