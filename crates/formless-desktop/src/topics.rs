//! Desktop topic presentation. The daemon owns moves, work and review handoffs.
use formless_protocol::{
    HarnessKind, HarnessRole, Request, SessionInfo, TopicFeatureStatus, TopicInfo,
    TopicProposalRequest, TopicStatus,
};

#[derive(Debug, Clone)]
pub enum Action {
    Main,
    List,
    Propose(TopicProposalRequest),
    Approve(String),
    MoveAndStart(String),
    Reject(String),
    Start(String),
    Retry { topic: String, feature: String },
    Recover(String),
}

impl Action {
    pub fn request(&self) -> Request {
        match self {
            Self::Main => Request::GetMainChat,
            Self::List => Request::ListTopics,
            Self::Propose(proposal) => Request::ProposeTopic {
                proposal: proposal.clone(),
            },
            Self::Approve(id) | Self::MoveAndStart(id) => Request::ApproveTopic {
                approval: formless_protocol::TopicApprovalRequest {
                    topic_id: id.clone(),
                },
            },
            Self::Reject(id) => Request::RejectTopic {
                rejection: formless_protocol::TopicRejectionRequest {
                    topic_id: id.clone(),
                    reason: "Kept in the original chat by the user".into(),
                },
            },
            Self::Start(id) => Request::StartTopicWorkflow {
                topic_id: id.clone(),
            },
            Self::Retry { topic, feature } => Request::RetryTopicFeature {
                retry: formless_protocol::TopicFeatureRetryRequest {
                    topic_id: topic.clone(),
                    feature_id: feature.clone(),
                },
            },
            Self::Recover(id) => Request::RecoverTopic {
                recovery: formless_protocol::TopicRecoveryRequest {
                    topic_id: id.clone(),
                },
            },
        }
    }

    pub fn decode(&self, value: serde_json::Value) -> Result<Reply, String> {
        match self {
            Self::Main => serde_json::from_value(value).map(Reply::Main),
            Self::List => serde_json::from_value(value).map(Reply::Inventory),
            _ => serde_json::from_value(value).map(|t| Reply::Topic(Box::new(t))),
        }
        .map_err(|e| format!("Could not read topic response: {e}"))
    }
}

#[derive(Debug)]
pub enum Reply {
    Main(SessionInfo),
    Inventory(Vec<TopicInfo>),
    Topic(Box<TopicInfo>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    pub seq: i64,
    pub preview: String,
}

/// Only persisted user messages can become features. No draft or inferred IDs.
pub fn candidates(value: &serde_json::Value) -> Vec<Candidate> {
    value["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            if m["role"].as_str()? != "user" {
                return None;
            }
            let id = m["id"].as_str()?;
            if id.trim().is_empty() {
                return None;
            }
            Some(Candidate {
                id: id.to_string(),
                seq: m["seq"].as_i64()?,
                preview: m["content"].as_str()?.chars().take(160).collect(),
            })
        })
        .rev()
        .take(100)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub fn harness(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Claude => "Claude",
        HarnessKind::Codex => "Codex",
    }
}
pub fn role(role: HarnessRole) -> &'static str {
    match role {
        HarnessRole::Build => "Build",
        HarnessRole::Review => "Review",
    }
}
pub fn status(topic: &TopicInfo) -> &'static str {
    if topic
        .features
        .iter()
        .any(|f| f.status == TopicFeatureStatus::NeedsUser)
    {
        return "Needs you";
    }
    match topic.status {
        TopicStatus::PendingApproval => "Awaiting approval",
        TopicStatus::Approved => "Ready to start",
        TopicStatus::Rejected => "Kept in main chat",
        TopicStatus::Running => "In progress",
        TopicStatus::Complete => "Ready for you",
        TopicStatus::Failed => "Needs attention",
    }
}
pub fn feature_status(status: TopicFeatureStatus) -> &'static str {
    match status {
        TopicFeatureStatus::Pending => "Waiting",
        TopicFeatureStatus::Building => "Building",
        TopicFeatureStatus::Reviewing => "In review",
        TopicFeatureStatus::Ready => "Ready for you",
        TopicFeatureStatus::NeedsUser => "Needs you",
        TopicFeatureStatus::Failed => "Failed",
    }
}

#[derive(Default)]
pub struct State {
    pub supported: bool,
    pub inventory_loaded: bool,
    pub main: Option<SessionInfo>,
    pub topics: Vec<TopicInfo>,
    pub selected: Option<String>,
    pub pending: Option<uuid::Uuid>,
    pub notice: Option<String>,
    pub candidates: Vec<Candidate>,
    pub candidate_session: Option<String>,
}
impl State {
    pub fn apply(&mut self, reply: Reply) {
        match reply {
            Reply::Main(main) => self.main = Some(main),
            Reply::Inventory(topics) => {
                self.topics = topics;
                self.inventory_loaded = true;
            }
            Reply::Topic(topic) => {
                self.selected = Some(topic.id.clone());
                if let Some(old) = self.topics.iter_mut().find(|t| t.id == topic.id) {
                    *old = *topic;
                } else {
                    self.topics.push(*topic);
                }
            }
        }
    }
    pub fn selected(&self) -> Option<&TopicInfo> {
        self.topics
            .iter()
            .find(|t| Some(&t.id) == self.selected.as_ref())
    }
    /// Daemon suggestions use the same consent view as a manual proposal.
    /// Merely receiving a proposal never approves or starts it.
    pub fn pending_for(&self, source: Option<&str>) -> Option<&TopicInfo> {
        self.topics.iter().find(|t| {
            t.status == TopicStatus::PendingApproval && Some(t.source_session_id.as_str()) == source
        })
    }
    pub fn attempt_for(
        &self,
        session: &str,
    ) -> Option<(&TopicInfo, &formless_protocol::TopicAttemptInfo)> {
        self.topics.iter().find_map(|t| {
            t.attempts
                .iter()
                .find(|a| a.conversation_session_id == session)
                .map(|a| (t, a))
        })
    }
    pub fn disconnect(&mut self) {
        self.inventory_loaded = false;
        if self.pending.take().is_some() {
            self.notice = Some("Connection changed. The last action's outcome is unknown; refresh before trying again. Nothing was replayed.".into());
        }
        self.candidates.clear();
        self.candidate_session = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_saved_user_ids_are_selectable_and_large_previews_are_bounded() {
        let messages = candidates(&json!({"messages":[
            {"id":"assistant","seq":0,"role":"assistant","content":"not a request"},
            {"id":"request","seq":1,"role":"user","content":"ก".repeat(1000)},
            {"id":" ","seq":2,"role":"user","content":"empty identity"},
            {"seq":2,"role":"user","content":"no identity"}
        ]}));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "request");
        assert_eq!(messages[0].preview.chars().count(), 160);
    }
    #[test]
    fn attention_does_not_hide_independently_ready_features_and_external_ids_are_not_transcripts() {
        let (_, topic) = crate::fixture::topic_workspace();
        assert_eq!(status(&topic), "Needs you");
        assert_eq!(feature_status(topic.features[0].status), "Ready for you");
        let mut state = State::default();
        state.apply(Reply::Topic(Box::new(topic)));
        assert!(state.attempt_for("claude-chat").is_some());
        assert!(state.attempt_for("external-claude").is_none());
        state.pending = Some(uuid::Uuid::new_v4());
        state.disconnect();
        assert!(state.pending.is_none());
        assert!(state.notice.unwrap().contains("unknown"));
    }

    #[test]
    fn daemon_proposals_only_prompt_the_source_chat_and_never_imply_consent() {
        let (_, mut topic) = crate::fixture::topic_workspace();
        topic.status = TopicStatus::PendingApproval;
        let mut state = State::default();
        state.apply(Reply::Inventory(vec![topic]));
        assert!(state.pending_for(Some("main")).is_some());
        assert!(state.pending_for(Some("other")).is_none());
        assert!(state.pending_for(None).is_none());
        assert!(state.selected.is_none());
        assert!(state.pending.is_none());
    }
}
