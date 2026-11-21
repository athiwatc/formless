//! Desktop topic presentation. The Formless model owns planning and handoffs.
use formless_protocol::{
    HarnessKind, Request, SessionInfo, StepAccess, TopicArtifact, TopicAttemptStatus,
    TopicFeatureStatus, TopicInfo, TopicProposalRequest, TopicStatus, TopicStepOutline,
};
use std::collections::{HashMap, HashSet};

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
    Cancel(String),
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
            Self::Cancel(id) => Request::CancelTopic {
                topic_id: id.clone(),
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
        HarnessKind::Pi => "Pi",
        HarnessKind::OpenCode => "OpenCode",
        HarnessKind::Antigravity => "Antigravity",
        HarnessKind::Acp => "ACP",
        HarnessKind::Muse => "Muse",
    }
}
pub fn access(access: StepAccess) -> &'static str {
    match access {
        StepAccess::ReadOnly => "Read only",
        StepAccess::Write => "Can write",
    }
}
pub const HARNESSES: [HarnessKind; 7] = [
    HarnessKind::Claude,
    HarnessKind::Codex,
    HarnessKind::Pi,
    HarnessKind::OpenCode,
    HarnessKind::Antigravity,
    HarnessKind::Acp,
    HarnessKind::Muse,
];

pub fn chosen_harness(kind: Option<HarnessKind>, agent: Option<&str>) -> String {
    let label = kind.map(harness).unwrap_or("Formless chooses");
    match (kind, agent) {
        (Some(HarnessKind::Acp), Some(agent)) => format!("{label} / {agent}"),
        _ => label.into(),
    }
}

#[derive(Clone, Copy)]
pub enum Preset {
    Code,
    Accounting,
}

pub fn preset(preset: Preset) -> Vec<TopicStepOutline> {
    let step = |title: &str, instructions: &str, harness, access| TopicStepOutline {
        title: title.into(),
        instructions: instructions.into(),
        harness,
        acp_agent: None,
        model: None,
        access,
    };
    match preset {
        Preset::Code => vec![
            step("Implement", "Implement the selected work item and check the result.", Some(HarnessKind::Claude), StepAccess::Write),
            step("Review", "Review the outputs independently. Report findings and suggested changes; do not merge anything.", Some(HarnessKind::Codex), StepAccess::ReadOnly),
        ],
        Preset::Accounting => vec![
            step("Scan receipts", "Extract dates, vendors, totals, taxes and currencies from supplied receipts. Flag unreadable details.", None, StepAccess::ReadOnly),
            step("Do accounting", "Organize the entries for the user's accounting needs. Ask about missing categories or rules.", None, StepAccess::Write),
            step("Create reports", "Prepare the requested accounting reports from the checked entries and explain unresolved details.", None, StepAccess::Write),
        ],
    }
}

pub fn artifact(artifact: &TopicArtifact) -> String {
    match artifact {
        TopicArtifact::GitRevision { revision } => {
            format!("Git revision: {revision} · no automatic merge")
        }
        TopicArtifact::File { path, sha256 } => {
            format!("File: {} · SHA-256 {sha256}", path.display())
        }
        TopicArtifact::Text { title, content } => format!("{title}\n{content}"),
    }
}

pub fn attempt_status(status: TopicAttemptStatus) -> &'static str {
    match status {
        TopicAttemptStatus::Running => "Working",
        TopicAttemptStatus::Succeeded => "Finished",
        TopicAttemptStatus::Failed => "Failed",
        TopicAttemptStatus::NeedsUser => "Needs you",
        TopicAttemptStatus::Cancelled => "Cancelled",
        TopicAttemptStatus::Abandoned => "Interrupted",
    }
}
pub fn status(topic: &TopicInfo) -> &'static str {
    match topic.status {
        TopicStatus::PendingApproval => "Awaiting approval",
        TopicStatus::Approved => "Ready to start",
        TopicStatus::Rejected => "Kept in main chat",
        TopicStatus::Running => "Working",
        TopicStatus::NeedsUser => "Needs you",
        TopicStatus::Complete => "Ready for you",
        TopicStatus::Failed => "Needs attention",
        TopicStatus::Cancelled => "Cancelled",
    }
}
pub fn feature_status(status: TopicFeatureStatus) -> &'static str {
    match status {
        TopicFeatureStatus::Pending => "Waiting",
        TopicFeatureStatus::Running => "Working",
        TopicFeatureStatus::Ready => "Ready for you",
        TopicFeatureStatus::NeedsUser => "Needs you",
        TopicFeatureStatus::Failed => "Failed",
        TopicFeatureStatus::Cancelled => "Cancelled",
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
    /// Parents are persisted by the daemon, never inferred from user labels.
    /// Iterative traversal keeps deep hierarchies safe; malformed/orphaned
    /// records remain reachable instead of disappearing from navigation.
    pub fn tree(&self) -> Vec<(usize, &TopicInfo)> {
        let visible = self
            .topics
            .iter()
            .filter(|topic| topic.status != TopicStatus::Rejected)
            .collect::<Vec<_>>();
        let ids = visible
            .iter()
            .map(|topic| topic.id.as_str())
            .collect::<HashSet<_>>();
        let mut children = HashMap::<&str, Vec<&TopicInfo>>::new();
        let mut roots = Vec::new();
        for topic in &visible {
            match topic
                .parent_topic_id
                .as_deref()
                .filter(|id| ids.contains(id) && *id != topic.id)
            {
                Some(parent) => children.entry(parent).or_default().push(topic),
                None => roots.push(*topic),
            }
        }
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = roots
            .into_iter()
            .rev()
            .map(|topic| (0, topic))
            .collect::<Vec<_>>();
        for fallback in visible {
            if stack.is_empty() && !seen.contains(fallback.id.as_str()) {
                stack.push((0, fallback));
            }
            while let Some((depth, topic)) = stack.pop() {
                if !seen.insert(topic.id.as_str()) {
                    continue;
                }
                result.push((depth, topic));
                if let Some(children) = children.get(topic.id.as_str()) {
                    stack.extend(children.iter().rev().map(|child| (depth + 1, *child)));
                }
            }
        }
        result
    }

    pub fn breadcrumbs(&self, id: &str) -> Vec<&TopicInfo> {
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(id);
        while let Some(id) = current {
            let Some(topic) = self
                .topics
                .iter()
                .find(|topic| topic.id == id && topic.status != TopicStatus::Rejected)
            else {
                break;
            };
            if !seen.insert(topic.id.as_str()) {
                break;
            }
            path.push(topic);
            current = topic.parent_topic_id.as_deref();
        }
        path.reverse();
        path
    }

    pub fn for_session(&self, session: Option<&str>) -> Option<&TopicInfo> {
        let session = session?;
        self.topics.iter().find(|topic| {
            topic.topic_session_id.as_deref() == Some(session)
                && topic.status != TopicStatus::Rejected
        })
    }

    pub fn displayed(&self, session: Option<&str>) -> Option<&TopicInfo> {
        self.selected()
            .or_else(|| self.pending_for(session))
            .or_else(|| self.for_session(session))
    }
    pub fn apply(&mut self, reply: Reply) {
        match reply {
            Reply::Main(main) => self.main = Some(main),
            Reply::Inventory(topics) => {
                self.topics = topics;
                self.inventory_loaded = true;
            }
            Reply::Topic(topic) => {
                self.selected = (topic.status != TopicStatus::Rejected).then(|| topic.id.clone());
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
            .find(|t| Some(&t.id) == self.selected.as_ref() && t.status != TopicStatus::Rejected)
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
    fn orchestrator_is_working_before_it_launches_a_harness() {
        let (_, mut topic) = crate::fixture::topic_workspace();
        topic.status = TopicStatus::Running;
        for feature in &mut topic.features {
            feature.status = TopicFeatureStatus::Pending;
        }
        for attempt in &mut topic.attempts {
            attempt.status = formless_protocol::TopicAttemptStatus::Abandoned;
        }
        assert!(!topic.attempts.is_empty());
        assert_eq!(status(&topic), "Working");
        topic.attempts.clear();
        assert_eq!(status(&topic), "Working");
    }
    #[test]
    fn accounting_is_not_a_build_review_workflow_and_code_is_only_a_preset() {
        let accounting = preset(Preset::Accounting);
        assert_eq!(
            accounting
                .iter()
                .map(|step| step.title.as_str())
                .collect::<Vec<_>>(),
            ["Scan receipts", "Do accounting", "Create reports"]
        );
        assert!(accounting.iter().all(|step| step.harness.is_none()));
        let code = preset(Preset::Code);
        assert_eq!(code[0].harness, Some(HarnessKind::Claude));
        assert_eq!(code[0].access, StepAccess::Write);
        assert_eq!(code[1].harness, Some(HarnessKind::Codex));
        assert_eq!(code[1].access, StepAccess::ReadOnly);
    }
    #[test]
    fn hierarchy_uses_parent_ids_with_no_fixed_labels_or_depth_and_keeps_orphans_reachable() {
        let (_, root) = crate::fixture::topic_workspace();
        let mut state = State::default();
        let mut parent = None;
        for depth in 0..128 {
            let mut topic = root.clone();
            topic.id = format!("node-{depth}");
            topic.name = format!("User name {depth}");
            topic.parent_topic_id = parent;
            topic.attempts.clear();
            parent = Some(topic.id.clone());
            state.topics.push(topic);
        }
        state.topics.reverse();
        let tree = state.tree();
        assert_eq!(tree.len(), 128);
        assert_eq!(tree[0].1.id, "node-0");
        assert_eq!(tree[127].0, 127);
        assert_eq!(state.breadcrumbs("node-127").len(), 128);
        state.topics[0].parent_topic_id = Some("missing-parent".into());
        state.topics[1].parent_topic_id = Some("node-126".into());
        assert_eq!(
            state.tree().len(),
            128,
            "orphans and cycles stay accessible without recursion"
        );
        assert_eq!(state.breadcrumbs("node-126").len(), 1);
    }
    #[test]
    fn outputs_are_generic_and_do_not_claim_every_result_is_a_reviewed_revision() {
        let file = TopicArtifact::File {
            path: "/outputs/report.pdf".into(),
            sha256: "verified-hash".into(),
        };
        assert!(artifact(&file).contains("report.pdf"));
        assert!(artifact(&file).contains("verified-hash"));
        let report = TopicArtifact::Text {
            title: "Accounting summary".into(),
            content: "Three receipts reconciled".into(),
        };
        assert!(artifact(&report).contains("Three receipts reconciled"));
        assert!(!artifact(&report).contains("revision"));
        assert!(artifact(&TopicArtifact::GitRevision {
            revision: "abc123".into()
        })
        .contains("no automatic merge"));
    }
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
