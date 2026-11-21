use crate::input::InputLine;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use formless_protocol::{
    HarnessKind, MessageInfo, SessionInfo, TopicAttemptInfo, TopicAttemptStatus,
    TopicFeatureStatus, TopicInfo, TopicProposalRequest, TopicStatus,
};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::collections::HashSet;
use std::path::PathBuf;

pub const CAPABILITY: &str = "topic_orchestration_v1";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Main,
    Topic(String),
    Attempt {
        topic_id: String,
        attempt_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatContext {
    Historical,
    Main,
    Topic(String),
    Harness {
        topic_id: String,
        attempt_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmedAction {
    Approve,
    ApproveAndStart,
    Start,
    Recover,
    Retry { feature_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    OpenMain,
    OpenTopic(String),
    OpenAttempt {
        topic_id: String,
        attempt_id: String,
    },
    Propose(TopicProposalRequest),
    Confirm {
        topic_id: String,
        action: ConfirmedAction,
    },
    Reject {
        topic_id: String,
        reason: String,
    },
    Refresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProposalStep {
    Messages,
    Fields,
    Review,
    Sending,
}

struct ProposalForm {
    source_session_id: String,
    messages: Vec<MessageInfo>,
    selected: HashSet<String>,
    message_index: usize,
    step: ProposalStep,
    fields: [InputLine; 4],
    field_index: usize,
    builder: HarnessKind,
    reviewer: HarnessKind,
    scroll: u16,
}

impl ProposalForm {
    fn new(source: &SessionInfo, messages: &[MessageInfo], repo: &str, name: &str) -> Self {
        let mut user_messages: Vec<_> = messages
            .iter()
            .filter(|message| message.role == "user" && message.session_id == source.id)
            .cloned()
            .collect();
        user_messages.sort_by_key(|message| message.seq);
        let selected = user_messages
            .last()
            .map(|message| HashSet::from([message.id.clone()]))
            .unwrap_or_default();
        let mut fields: [InputLine; 4] = std::array::from_fn(|_| InputLine::default());
        fields[0].set(name);
        fields[1].set(repo);
        Self {
            source_session_id: source.id.clone(),
            messages: user_messages,
            selected,
            message_index: 0,
            step: ProposalStep::Messages,
            fields,
            field_index: 0,
            builder: HarnessKind::Claude,
            reviewer: HarnessKind::Codex,
            scroll: 0,
        }
    }

    fn request(&self) -> Option<TopicProposalRequest> {
        let name = self.fields[0].expanded().trim().to_string();
        let repo = self.fields[1].expanded().trim().to_string();
        let message_ids: Vec<_> = self
            .messages
            .iter()
            .filter(|message| self.selected.contains(&message.id))
            .map(|message| message.id.clone())
            .collect();
        if name.is_empty() || repo.is_empty() || message_ids.is_empty() {
            return None;
        }
        Some(TopicProposalRequest {
            source_session_id: self.source_session_id.clone(),
            message_ids,
            features: vec![],
            name,
            repo: PathBuf::from(repo),
            builder: self.builder,
            builder_model: nonblank(&self.fields[2].expanded()),
            reviewer: self.reviewer,
            reviewer_model: nonblank(&self.fields[3].expanded()),
        })
    }

    fn selected_messages(&self) -> impl Iterator<Item = &MessageInfo> {
        self.messages
            .iter()
            .filter(|message| self.selected.contains(&message.id))
    }

    fn key(&mut self, key: KeyEvent) -> Action {
        match self.step {
            ProposalStep::Messages => match key.code {
                KeyCode::Esc => Action::Close,
                KeyCode::Up => {
                    self.message_index = self.message_index.saturating_sub(1);
                    Action::None
                }
                KeyCode::Down => {
                    self.message_index =
                        (self.message_index + 1).min(self.messages.len().saturating_sub(1));
                    Action::None
                }
                KeyCode::Char(' ') => {
                    if let Some(message) = self.messages.get(self.message_index) {
                        if !self.selected.remove(&message.id) {
                            self.selected.insert(message.id.clone());
                        }
                    }
                    Action::None
                }
                KeyCode::Enter => {
                    self.step = ProposalStep::Fields;
                    Action::None
                }
                _ => Action::None,
            },
            ProposalStep::Fields => {
                if key.code == KeyCode::Esc {
                    return Action::Close;
                }
                match key.code {
                    KeyCode::Tab | KeyCode::Down => {
                        self.field_index = (self.field_index + 1) % 6;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        self.field_index = (self.field_index + 5) % 6;
                    }
                    KeyCode::Left | KeyCode::Right if self.field_index == 2 => {
                        self.builder = if self.builder == HarnessKind::Claude {
                            HarnessKind::Codex
                        } else {
                            HarnessKind::Claude
                        };
                    }
                    KeyCode::Left | KeyCode::Right if self.field_index == 4 => {
                        self.reviewer = if self.reviewer == HarnessKind::Claude {
                            HarnessKind::Codex
                        } else {
                            HarnessKind::Claude
                        };
                    }
                    KeyCode::Enter => {
                        if self.field_index == 5 {
                            self.step = ProposalStep::Review;
                        } else {
                            self.field_index = (self.field_index + 1) % 6;
                        }
                    }
                    KeyCode::Backspace => {
                        if let Some(field) = self.text_field_mut() {
                            field.backspace();
                        }
                    }
                    KeyCode::Delete => {
                        if let Some(field) = self.text_field_mut() {
                            field.delete();
                        }
                    }
                    KeyCode::Left => {
                        if let Some(field) = self.text_field_mut() {
                            field.left();
                        }
                    }
                    KeyCode::Right => {
                        if let Some(field) = self.text_field_mut() {
                            field.right();
                        }
                    }
                    KeyCode::Home => {
                        if let Some(field) = self.text_field_mut() {
                            field.home();
                        }
                    }
                    KeyCode::End => {
                        if let Some(field) = self.text_field_mut() {
                            field.end();
                        }
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        if let Some(field) = self.text_field_mut() {
                            field.insert_char(c);
                        }
                    }
                    _ => {}
                }
                Action::None
            }
            ProposalStep::Review => match key.code {
                KeyCode::Esc => Action::Close,
                KeyCode::Backspace => {
                    self.step = ProposalStep::Fields;
                    Action::None
                }
                KeyCode::Enter => match self.request() {
                    Some(request) => {
                        self.step = ProposalStep::Sending;
                        Action::Propose(request)
                    }
                    None => Action::None,
                },
                _ => Action::None,
            },
            ProposalStep::Sending => Action::None,
        }
    }

    fn text_field_mut(&mut self) -> Option<&mut InputLine> {
        match self.field_index {
            0 => Some(&mut self.fields[0]),
            1 => Some(&mut self.fields[1]),
            3 => Some(&mut self.fields[2]),
            5 => Some(&mut self.fields[3]),
            _ => None,
        }
    }
}

fn nonblank(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[derive(Clone, Debug)]
enum Overlay {
    Picker,
    Proposal,
    Review {
        topic_id: String,
        scroll: u16,
    },
    Details {
        topic_id: String,
        scroll: u16,
    },
    Confirm {
        topic_id: String,
        action: ConfirmedAction,
    },
    Reject {
        topic_id: String,
        reason: Box<InputLine>,
    },
}

pub struct State {
    enabled: Option<bool>,
    main: Option<SessionInfo>,
    topics: Vec<TopicInfo>,
    selected: Node,
    overlay: Option<Overlay>,
    proposal: Option<ProposalForm>,
    pending_reviewed: HashSet<String>,
    start_attempted: HashSet<String>,
    last_refresh: Option<std::time::Instant>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            enabled: None,
            main: None,
            topics: Vec::new(),
            selected: Node::Main,
            overlay: None,
            proposal: None,
            pending_reviewed: HashSet::new(),
            start_attempted: HashSet::new(),
            last_refresh: None,
        }
    }
}

impl State {
    pub fn enabled(&self) -> bool {
        self.enabled == Some(true)
    }

    pub fn capability_known(&self) -> bool {
        self.enabled.is_some()
    }

    pub fn set_capability(&mut self, enabled: bool) {
        self.enabled = Some(enabled);
        if !enabled {
            self.overlay = None;
            self.proposal = None;
        }
    }

    pub fn disconnected(&mut self) {
        self.enabled = None;
    }

    /// Pasted text goes to the overlay's focused text field, never the
    /// composer it covers.
    pub fn paste(&mut self, text: &str) {
        let text: String = text
            .replace(['\r', '\n'], " ")
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        match self.overlay.as_mut() {
            Some(Overlay::Proposal) => {
                if let Some(proposal) = self.proposal.as_mut() {
                    if matches!(proposal.step, ProposalStep::Fields) {
                        if let Some(field) = proposal.text_field_mut() {
                            field.insert_str(&text);
                        }
                    }
                }
            }
            Some(Overlay::Reject { reason, .. }) => reason.insert_str(&text),
            _ => {}
        }
    }

    pub fn overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    #[cfg(test)]
    pub fn selected(&self) -> &Node {
        &self.selected
    }

    pub fn select(&mut self, node: Node) {
        if self.node_exists(&node) {
            self.selected = node;
        }
    }

    pub fn main(&self) -> Option<&SessionInfo> {
        self.main.as_ref()
    }

    pub fn mark_refreshed(&mut self) {
        self.last_refresh = Some(std::time::Instant::now());
    }

    pub fn needs_refresh(&self) -> bool {
        self.enabled()
            && self
                .last_refresh
                .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(5))
    }

    pub fn set_main(&mut self, session: SessionInfo) {
        self.main = Some(session);
    }

    pub fn update_topics(&mut self, topics: Vec<TopicInfo>) -> Option<String> {
        self.topics = topics;
        if !self.node_exists(&self.selected) {
            self.selected = Node::Main;
        }
        self.topics
            .iter()
            .find(|topic| {
                topic.status == TopicStatus::PendingApproval
                    && !self.pending_reviewed.contains(&topic.id)
            })
            .map(|topic| topic.id.clone())
    }

    pub fn mark_pending_reviewed(&mut self, topic_id: &str) {
        self.pending_reviewed.insert(topic_id.to_string());
    }

    pub fn upsert_topic(&mut self, topic: TopicInfo, select: bool) {
        let topic_id = topic.id.clone();
        if let Some(existing) = self
            .topics
            .iter_mut()
            .find(|existing| existing.id == topic.id)
        {
            *existing = topic;
        } else {
            self.topics.push(topic);
        }
        if select {
            self.selected = Node::Topic(topic_id);
        }
    }

    pub fn topic(&self, topic_id: &str) -> Option<&TopicInfo> {
        self.topics.iter().find(|topic| topic.id == topic_id)
    }

    pub fn topic_for_session(&self, session_id: &str) -> Option<Node> {
        if self.main.as_ref().is_some_and(|main| main.id == session_id) {
            return Some(Node::Main);
        }
        for topic in &self.topics {
            if topic.topic_session_id.as_deref() == Some(session_id) {
                return Some(Node::Topic(topic.id.clone()));
            }
            if let Some(attempt) = topic
                .attempts
                .iter()
                .find(|attempt| attempt.conversation_session_id == session_id)
            {
                return Some(Node::Attempt {
                    topic_id: topic.id.clone(),
                    attempt_id: attempt.id.clone(),
                });
            }
        }
        None
    }

    pub fn context_for_session(&self, session_id: Option<&str>) -> ChatContext {
        let Some(session_id) = session_id else {
            return ChatContext::Historical;
        };
        match self.topic_for_session(session_id) {
            Some(Node::Main) => ChatContext::Main,
            Some(Node::Topic(topic_id)) => ChatContext::Topic(topic_id),
            Some(Node::Attempt {
                topic_id,
                attempt_id,
            }) => ChatContext::Harness {
                topic_id,
                attempt_id,
            },
            None => ChatContext::Historical,
        }
    }

    pub fn is_harness_session(&self, session_id: &str) -> bool {
        matches!(
            self.context_for_session(Some(session_id)),
            ChatContext::Harness { .. }
        )
    }

    pub fn current_is_harness(&self, session_id: Option<&str>) -> bool {
        session_id.is_some_and(|id| self.is_harness_session(id))
    }

    pub fn claim_start(&mut self, topic_id: &str) -> bool {
        self.start_attempted.insert(topic_id.to_string())
    }

    pub fn release_start(&mut self, topic_id: &str) {
        self.start_attempted.remove(topic_id);
    }

    pub fn open_picker(&mut self) {
        self.overlay = Some(Overlay::Picker);
    }

    pub fn open_details(&mut self, topic_id: &str) -> bool {
        if self.topic(topic_id).is_none() {
            return false;
        }
        self.selected = Node::Topic(topic_id.to_string());
        self.overlay = Some(
            if self
                .topic(topic_id)
                .is_some_and(|topic| topic.status == TopicStatus::PendingApproval)
            {
                self.mark_pending_reviewed(topic_id);
                Overlay::Review {
                    topic_id: topic_id.to_string(),
                    scroll: 0,
                }
            } else {
                Overlay::Details {
                    topic_id: topic_id.to_string(),
                    scroll: 0,
                }
            },
        );
        true
    }

    pub fn open_review(&mut self, topic_id: &str) -> bool {
        if self.topic(topic_id).is_none() {
            return false;
        }
        self.mark_pending_reviewed(topic_id);
        self.selected = Node::Topic(topic_id.to_string());
        self.overlay = Some(Overlay::Review {
            topic_id: topic_id.to_string(),
            scroll: 0,
        });
        true
    }

    pub fn open_proposal(
        &mut self,
        source: &SessionInfo,
        messages: &[MessageInfo],
        repo: &str,
        name: &str,
    ) {
        self.proposal = Some(ProposalForm::new(source, messages, repo, name));
        self.overlay = Some(Overlay::Proposal);
    }

    pub fn proposal_pending(&mut self) {
        if let Some(proposal) = self.proposal.as_mut() {
            proposal.step = ProposalStep::Sending;
        }
    }

    pub fn proposal_is_sending(&self) -> bool {
        matches!(self.overlay, Some(Overlay::Proposal))
            && self
                .proposal
                .as_ref()
                .is_some_and(|proposal| proposal.step == ProposalStep::Sending)
    }

    pub fn proposal_failed(&mut self) {
        if let Some(proposal) = self.proposal.as_mut() {
            proposal.step = ProposalStep::Review;
        }
    }

    pub fn close_overlay(&mut self) {
        self.overlay = None;
        self.proposal = None;
    }

    pub fn open_action(&mut self, topic_id: &str, action: ConfirmedAction) -> bool {
        let Some(topic) = self.topic(topic_id) else {
            return false;
        };
        if matches!(
            action,
            ConfirmedAction::Approve | ConfirmedAction::ApproveAndStart
        ) && topic.status != TopicStatus::PendingApproval
            || matches!(action, ConfirmedAction::Start) && topic.status != TopicStatus::Approved
        {
            return false;
        }
        self.overlay = Some(Overlay::Confirm {
            topic_id: topic_id.to_string(),
            action,
        });
        true
    }

    pub fn open_reject(&mut self, topic_id: &str) -> bool {
        if self.topic(topic_id).is_none() {
            return false;
        }
        let mut reason = Box::<InputLine>::default();
        reason.set("Rejected by user");
        self.overlay = Some(Overlay::Reject {
            topic_id: topic_id.to_string(),
            reason,
        });
        true
    }

    pub fn open_topic_command(&mut self, args: &str) -> Result<(), String> {
        let words: Vec<_> = args.split_whitespace().collect();
        match words.as_slice() {
            [] => {
                self.open_picker();
                Ok(())
            }
            ["start"] => self.open_action_for_selected(ConfirmedAction::Start),
            ["start", topic_id] => self
                .open_action(topic_id, ConfirmedAction::Start)
                .then_some(())
                .ok_or_else(|| format!("No topic named {topic_id}; use /topics.")),
            ["recover"] => self.open_action_for_selected(ConfirmedAction::Recover),
            ["recover", topic_id] => self
                .open_action(topic_id, ConfirmedAction::Recover)
                .then_some(())
                .ok_or_else(|| format!("No topic named {topic_id}; use /topics.")),
            ["retry", topic_id, feature_id] => self
                .open_action(
                    topic_id,
                    ConfirmedAction::Retry {
                        feature_id: feature_id.to_string(),
                    },
                )
                .then_some(())
                .ok_or_else(|| format!("No topic named {topic_id}; use /topics.")),
            ["retry", feature_id] => {
                let Some(Node::Topic(topic_id)) = self.selected_topic_node() else {
                    return Err("Select a topic with /topics first.".into());
                };
                self.open_action(
                    &topic_id,
                    ConfirmedAction::Retry {
                        feature_id: feature_id.to_string(),
                    },
                )
                .then_some(())
                .ok_or_else(|| format!("No topic named {topic_id}; use /topics."))
            }
            ["reject", topic_id, rest @ ..] if !rest.is_empty() => {
                let reason = rest.join(" ");
                self.overlay = Some(Overlay::Reject {
                    topic_id: topic_id.to_string(),
                    reason: {
                        let mut line = Box::<InputLine>::default();
                        line.set(&reason);
                        line
                    },
                });
                Ok(())
            }
            ["reject", topic_id] => self
                .open_reject(topic_id)
                .then_some(())
                .ok_or_else(|| format!("No topic named {topic_id}; use /topics.")),
            [topic_id] => self
                .open_details(topic_id)
                .then_some(())
                .ok_or_else(|| format!("No topic named {topic_id}; use /topics.")),
            _ => Err(
                "Usage: /topic [ID|start|recover|retry TOPIC FEATURE|reject TOPIC REASON]".into(),
            ),
        }
    }

    fn open_action_for_selected(&mut self, action: ConfirmedAction) -> Result<(), String> {
        let Some(Node::Topic(topic_id)) = self.selected_topic_node() else {
            return Err("Select a topic with /topics first.".into());
        };
        self.open_action(&topic_id, action)
            .then_some(())
            .ok_or_else(|| format!("No topic named {topic_id}; use /topics."))
    }

    fn selected_topic_id(&self) -> Option<String> {
        match self.selected_topic_node() {
            Some(Node::Topic(id)) => Some(id),
            _ => None,
        }
    }

    fn selected_topic_node(&self) -> Option<Node> {
        match &self.selected {
            Node::Topic(_) => Some(self.selected.clone()),
            Node::Attempt { topic_id, .. } => Some(Node::Topic(topic_id.clone())),
            Node::Main => None,
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> Action {
        let Some(overlay) = self.overlay.clone() else {
            return Action::None;
        };
        if let Overlay::Proposal = overlay {
            let Some(proposal) = self.proposal.as_mut() else {
                self.overlay = None;
                return Action::None;
            };
            let action = proposal.key(key);
            if matches!(action, Action::Close) {
                self.close_overlay();
            }
            return action;
        }
        let review_overlay = matches!(overlay, Overlay::Review { .. });
        match overlay {
            Overlay::Picker => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.close_overlay();
                    Action::Close
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.move_selection(false);
                    Action::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.move_selection(true);
                    Action::None
                }
                KeyCode::Char('r') => Action::Refresh,
                KeyCode::Enter => self.activate_selected(),
                KeyCode::Char('d') => match self.selected.clone() {
                    Node::Topic(id) => {
                        self.open_details(&id);
                        Action::None
                    }
                    Node::Attempt { topic_id, .. } => {
                        self.open_details(&topic_id);
                        Action::None
                    }
                    Node::Main => Action::OpenMain,
                },
                _ => Action::None,
            },
            Overlay::Review { topic_id, scroll } | Overlay::Details { topic_id, scroll } => {
                let review = review_overlay;
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.overlay = Some(Overlay::Picker);
                        Action::Close
                    }
                    KeyCode::Up => {
                        self.set_detail_scroll(&topic_id, review, scroll.saturating_sub(1));
                        Action::None
                    }
                    KeyCode::Down => {
                        self.set_detail_scroll(&topic_id, review, scroll.saturating_add(1));
                        Action::None
                    }
                    KeyCode::PageUp => {
                        self.set_detail_scroll(&topic_id, review, scroll.saturating_sub(10));
                        Action::None
                    }
                    KeyCode::PageDown => {
                        self.set_detail_scroll(&topic_id, review, scroll.saturating_add(10));
                        Action::None
                    }
                    KeyCode::Char('a') if review => {
                        self.open_action(&topic_id, ConfirmedAction::Approve);
                        Action::None
                    }
                    KeyCode::Char('s') if review => {
                        self.open_action(&topic_id, ConfirmedAction::ApproveAndStart);
                        Action::None
                    }
                    KeyCode::Char('r') if review => {
                        self.open_reject(&topic_id);
                        Action::None
                    }
                    KeyCode::Char('s') if !review => {
                        self.open_action(&topic_id, ConfirmedAction::Start);
                        Action::None
                    }
                    KeyCode::Char('c') if !review => {
                        self.open_action(&topic_id, ConfirmedAction::Recover);
                        Action::None
                    }
                    KeyCode::Char('x') if !review => {
                        if let Some(feature) = self.topic(&topic_id).and_then(|topic| {
                            topic.features.iter().find(|feature| {
                                matches!(
                                    feature.status,
                                    TopicFeatureStatus::Failed | TopicFeatureStatus::NeedsUser
                                )
                            })
                        }) {
                            self.open_action(
                                &topic_id,
                                ConfirmedAction::Retry {
                                    feature_id: feature.id.clone(),
                                },
                            );
                        }
                        Action::None
                    }
                    _ => Action::None,
                }
            }
            Overlay::Confirm { topic_id, action } => match key.code {
                KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                    self.overlay = Some(
                        if self
                            .topic(&topic_id)
                            .is_some_and(|topic| topic.status == TopicStatus::PendingApproval)
                        {
                            Overlay::Review {
                                topic_id,
                                scroll: 0,
                            }
                        } else {
                            Overlay::Details {
                                topic_id,
                                scroll: 0,
                            }
                        },
                    );
                    Action::Close
                }
                KeyCode::Enter | KeyCode::Char('y' | 'Y') => {
                    self.overlay = None;
                    Action::Confirm { topic_id, action }
                }
                _ => Action::None,
            },
            Overlay::Reject {
                topic_id,
                mut reason,
            } => match key.code {
                KeyCode::Esc => {
                    self.overlay = Some(Overlay::Review {
                        topic_id,
                        scroll: 0,
                    });
                    Action::Close
                }
                KeyCode::Enter => {
                    let reason = reason.expanded().trim().to_string();
                    self.overlay = None;
                    Action::Reject { topic_id, reason }
                }
                KeyCode::Backspace => {
                    reason.backspace();
                    self.overlay = Some(Overlay::Reject { topic_id, reason });
                    Action::None
                }
                KeyCode::Left => {
                    reason.left();
                    self.overlay = Some(Overlay::Reject { topic_id, reason });
                    Action::None
                }
                KeyCode::Right => {
                    reason.right();
                    self.overlay = Some(Overlay::Reject { topic_id, reason });
                    Action::None
                }
                KeyCode::Char(c)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    reason.insert_char(c);
                    self.overlay = Some(Overlay::Reject { topic_id, reason });
                    Action::None
                }
                _ => Action::None,
            },
            Overlay::Proposal => Action::None,
        }
    }

    fn set_detail_scroll(&mut self, topic_id: &str, review: bool, scroll: u16) {
        self.overlay = Some(if review {
            Overlay::Review {
                topic_id: topic_id.to_string(),
                scroll,
            }
        } else {
            Overlay::Details {
                topic_id: topic_id.to_string(),
                scroll,
            }
        });
    }

    fn activate_selected(&mut self) -> Action {
        match self.selected.clone() {
            Node::Main => Action::OpenMain,
            Node::Topic(topic_id) => {
                let Some(topic) = self.topic(&topic_id) else {
                    return Action::None;
                };
                if topic.status == TopicStatus::PendingApproval {
                    self.open_review(&topic_id);
                    Action::None
                } else if topic.topic_session_id.is_some() {
                    Action::OpenTopic(topic_id)
                } else {
                    self.open_details(&topic_id);
                    Action::None
                }
            }
            Node::Attempt {
                topic_id,
                attempt_id,
            } => Action::OpenAttempt {
                topic_id,
                attempt_id,
            },
        }
    }

    fn node_exists(&self, node: &Node) -> bool {
        match node {
            Node::Main => true,
            Node::Topic(id) => self.topics.iter().any(|topic| topic.id == *id),
            Node::Attempt {
                topic_id,
                attempt_id,
            } => self.topics.iter().any(|topic| {
                topic.id == *topic_id && topic.attempts.iter().any(|a| a.id == *attempt_id)
            }),
        }
    }

    pub fn rows(&self) -> Vec<TreeRow> {
        let mut rows = vec![TreeRow {
            node: Some(Node::Main),
            depth: 0,
            label: "Main Chat".into(),
            status: self
                .main
                .as_ref()
                .map(|session| session.name.clone())
                .unwrap_or_default(),
            section: false,
        }];
        rows.push(TreeRow {
            node: None,
            depth: 0,
            label: "Topic Chats".into(),
            status: String::new(),
            section: true,
        });
        for topic in &self.topics {
            rows.push(TreeRow {
                node: Some(Node::Topic(topic.id.clone())),
                depth: 0,
                label: topic.name.clone(),
                status: topic_status_label(topic),
                section: false,
            });
            for attempt in &topic.attempts {
                rows.push(TreeRow {
                    node: Some(Node::Attempt {
                        topic_id: topic.id.clone(),
                        attempt_id: attempt.id.clone(),
                    }),
                    depth: 1,
                    label: format!(
                        "{} {} · round {}",
                        harness_label(attempt.harness),
                        role_label(attempt.role),
                        attempt.round
                    ),
                    status: attempt_status_label(attempt),
                    section: false,
                });
            }
        }
        rows
    }

    fn move_selection(&mut self, down: bool) {
        let rows = self.rows();
        let selectable: Vec<_> = rows.iter().filter_map(|row| row.node.as_ref()).collect();
        if selectable.is_empty() {
            self.selected = Node::Main;
            return;
        }
        let current = selectable
            .iter()
            .position(|node| **node == self.selected)
            .unwrap_or(0);
        let next = if down {
            (current + 1).min(selectable.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.selected = selectable[next].clone();
    }

    pub fn draw_rail(
        &self,
        frame: &mut Frame,
        area: Rect,
        current_session: Option<&str>,
        color: bool,
    ) {
        let rows = self.rows();
        let visible = area.height.saturating_sub(2) as usize;
        let selected = rows
            .iter()
            .position(|row| row.node.as_ref() == Some(&self.selected))
            .unwrap_or(0);
        let top = selected.saturating_sub(visible.saturating_sub(1).min(6));
        let title = if self.enabled() {
            " Conversations "
        } else {
            " Conversations · unavailable "
        };
        let items: Vec<_> = rows
            .iter()
            .skip(top)
            .take(visible)
            .map(|row| {
                let selected = row.node.as_ref() == Some(&self.selected);
                let current =
                    row.node.as_ref().and_then(|node| self.session_id(node)) == current_session;
                let indent = "  ".repeat(row.depth);
                let label = format!(
                    "{indent}{}{:<1}",
                    if current { "● " } else { "  " },
                    row.label
                );
                let style = if row.section {
                    Style::default().add_modifier(Modifier::BOLD).fg(if color {
                        Color::Cyan
                    } else {
                        Color::Reset
                    })
                } else if selected {
                    Style::default()
                        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
                        .fg(if color { Color::Cyan } else { Color::Reset })
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(label, style),
                    Span::styled(
                        row.status.clone(),
                        Style::default().fg(if color {
                            status_color(&row.status)
                        } else {
                            Color::Reset
                        }),
                    ),
                ]))
            })
            .collect();
        frame.render_widget(
            List::new(items).block(
                crate::ui::panel(title, color, Color::Cyan)
                    .title_bottom(crate::ui::hint("/topics", color)),
            ),
            area,
        );
    }

    pub fn session_id(&self, node: &Node) -> Option<&str> {
        match node {
            Node::Main => self.main.as_ref().map(|session| session.id.as_str()),
            Node::Topic(id) => self
                .topic(id)
                .and_then(|topic| topic.topic_session_id.as_deref()),
            Node::Attempt {
                topic_id,
                attempt_id,
            } => self
                .topic(topic_id)
                .and_then(|topic| topic.attempts.iter().find(|a| a.id == *attempt_id))
                .map(|attempt| attempt.conversation_session_id.as_str()),
        }
    }

    pub fn breadcrumb(&self, current_session: Option<&str>) -> String {
        match self.context_for_session(current_session) {
            ChatContext::Historical => "Historical Session".into(),
            ChatContext::Main => "Main Chat".into(),
            ChatContext::Topic(topic_id) => self
                .topic(&topic_id)
                .map(|topic| format!("Main Chat / Topic Chats / {}", topic.name))
                .unwrap_or_else(|| "Topic Chat".into()),
            ChatContext::Harness {
                topic_id,
                attempt_id,
            } => self
                .topic(&topic_id)
                .and_then(|topic| {
                    topic
                        .attempts
                        .iter()
                        .find(|attempt| attempt.id == attempt_id)
                        .map(|attempt| {
                            format!(
                                "Main Chat / Topic Chats / {} / {} {} · read-only",
                                topic.name,
                                harness_label(attempt.harness),
                                role_label(attempt.role)
                            )
                        })
                })
                .unwrap_or_else(|| "Harness conversation · read-only".into()),
        }
    }

    pub fn draw_overlay(&self, frame: &mut Frame, color: bool) {
        let Some(overlay) = self.overlay.as_ref() else {
            return;
        };
        let area = centered(frame.area(), 88, 88);
        frame.render_widget(Clear, area);
        match overlay {
            Overlay::Picker => self.draw_picker(frame, area, color),
            Overlay::Proposal => self.draw_proposal(frame, area, color),
            Overlay::Review { topic_id, scroll } => {
                self.draw_topic(frame, area, topic_id, *scroll, color, true)
            }
            Overlay::Details { topic_id, scroll } => {
                self.draw_topic(frame, area, topic_id, *scroll, color, false)
            }
            Overlay::Confirm { topic_id, action } => {
                self.draw_confirmation(frame, area, topic_id, action, color)
            }
            Overlay::Reject { topic_id, reason } => {
                self.draw_reject(frame, area, topic_id, reason, color)
            }
        }
    }

    fn draw_picker(&self, frame: &mut Frame, area: Rect, color: bool) {
        let block = crate::ui::panel(" Main Chat / Topic Chats ", color, Color::Cyan).title_bottom(
            crate::ui::hint(
                "↑/↓ select · Enter open · d details · r refresh · Esc close",
                color,
            ),
        );
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width >= 88 {
            let chunks =
                Layout::horizontal([Constraint::Percentage(38), Constraint::Percentage(62)])
                    .split(inner);
            self.draw_picker_rows(frame, chunks[0], color);
            if let Some(topic_id) = self.selected_topic_id() {
                self.draw_topic_details(frame, chunks[1], &topic_id, 0, color);
            } else {
                frame.render_widget(
                    Paragraph::new("Main Chat is the canonical source for topic requests.")
                        .wrap(Wrap { trim: false }),
                    chunks[1].inner(Margin::new(1, 1)),
                );
            }
        } else {
            self.draw_picker_rows(frame, inner, color);
        }
    }

    fn draw_picker_rows(&self, frame: &mut Frame, area: Rect, color: bool) {
        let rows = self.rows();
        let mut state = ListState::default().with_selected(
            rows.iter()
                .position(|row| row.node.as_ref() == Some(&self.selected)),
        );
        let items = rows
            .into_iter()
            .map(|row| {
                let selected = row.node.as_ref() == Some(&self.selected);
                let label = format!("{}{}", "  ".repeat(row.depth), row.label);
                let style = if row.section {
                    Style::default()
                        .add_modifier(Modifier::BOLD)
                        .fg(Color::Cyan)
                } else if selected {
                    Style::default()
                        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
                        .fg(if color { Color::Cyan } else { Color::Reset })
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(label, style),
                    Span::styled(format!("  {}", row.status), crate::ui::dim(color)),
                ]))
            })
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items).block(crate::ui::panel(" Conversations ", color, Color::Cyan)),
            area,
            &mut state,
        );
    }

    fn draw_proposal(&self, frame: &mut Frame, area: Rect, color: bool) {
        let Some(proposal) = self.proposal.as_ref() else {
            return;
        };
        let block = crate::ui::panel(" Propose Topic Chat ", color, Color::Cyan).title_bottom(
            crate::ui::hint(
                match proposal.step {
                    ProposalStep::Messages => "Space select · Enter fields · Esc cancel",
                    ProposalStep::Fields => {
                        "Tab move · Enter next/review · ←/→ harness · Esc cancel"
                    }
                    ProposalStep::Review => "Enter propose only · Backspace edit · Esc cancel",
                    ProposalStep::Sending => {
                        "Submitting proposal · no messages moved and no workflow started"
                    }
                },
                color,
            ),
        );
        let inner = block.inner(area);
        frame.render_widget(block, area);
        match proposal.step {
            ProposalStep::Messages => {
                let mut lines = vec![Line::styled(
                    "Select one or more user requests to move after explicit approval:",
                    Style::default().add_modifier(Modifier::BOLD),
                )];
                if proposal.messages.is_empty() {
                    lines.push(Line::from("No user requests are available in Main Chat."));
                }
                for (index, message) in proposal.messages.iter().enumerate() {
                    let checked = proposal.selected.contains(&message.id);
                    lines.push(Line::from(vec![
                        Span::styled(
                            if index == proposal.message_index { "› " } else { "  " },
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(if checked { "[x] " } else { "[ ] " }, Style::default()),
                        Span::styled(format!("#{} {}", message.seq, message.id), crate::ui::dim(color)),
                    ]));
                    lines.push(Line::from(format!("    {}", message.content.replace('\n', " "))));
                }
                // One header line, then two unwrapped lines per message.
                let selected_end = 3 + 2 * proposal.message_index;
                let scroll = selected_end.saturating_sub(inner.height as usize);
                frame.render_widget(
                    Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
                    inner,
                );
            }
            ProposalStep::Fields => {
                let items = [
                    ("Topic name", proposal.fields[0].expanded()),
                    ("Repository", proposal.fields[1].expanded()),
                    ("Builder", harness_label(proposal.builder).to_string()),
                    ("Builder model (optional)", proposal.fields[2].expanded()),
                    ("Reviewer", harness_label(proposal.reviewer).to_string()),
                    ("Reviewer model (optional)", proposal.fields[3].expanded()),
                ];
                let mut lines = vec![Line::from("Configure the isolated workflow:")];
                for (index, (label, value)) in items.iter().enumerate() {
                    let prefix = if proposal.field_index == index { "› " } else { "  " };
                    let style = if proposal.field_index == index {
                        Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, style),
                        Span::styled(format!("{label}: "), style),
                        Span::raw(value),
                    ]));
                }
                frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
            }
            ProposalStep::Review => {
                let request = proposal.request();
                let mut lines = vec![Line::styled(
                    "Review before creating the proposal",
                    Style::default().add_modifier(Modifier::BOLD),
                )];
                if let Some(request) = request {
                    lines.extend([
                        Line::from(format!("Topic: {}", request.name)),
                        Line::from(format!("Repository: {}", request.repo.display())),
                        Line::from(format!("Builder: {}{}", harness_label(request.builder), model_suffix(request.builder_model.as_deref()))),
                        Line::from(format!("Reviewer: {}{}", harness_label(request.reviewer), model_suffix(request.reviewer_model.as_deref()))),
                        Line::from(format!("Selected requests: {}", request.message_ids.join(", "))),
                        Line::from("Workflow: approval moves only the selected request rows. Start runs only after a separate explicit confirmation."),
                        Line::from("Ready revisions are shown for review; nothing is automatically merged."),
                        Line::from("Requests to move:"),
                    ]);
                    for message in proposal.selected_messages() {
                        lines.push(Line::from(format!("#{} [{}]", message.seq, message.id)));
                        lines.push(Line::from(message.content.clone()));
                    }
                } else {
                    lines.push(Line::from("Choose a topic name, repository and at least one request."));
                }
                frame.render_widget(
                    Paragraph::new(lines)
                        .wrap(Wrap { trim: false })
                        .scroll((proposal.scroll, 0)),
                    inner,
                );
            }
            ProposalStep::Sending => frame.render_widget(
                Paragraph::new("Submitting a proposal. No message moves or workflow starts until a later confirmed approval.")
                    .wrap(Wrap { trim: false }),
                inner,
            ),
        }
    }

    fn draw_topic(
        &self,
        frame: &mut Frame,
        area: Rect,
        topic_id: &str,
        scroll: u16,
        color: bool,
        review: bool,
    ) {
        let title = if review {
            " Review Topic Proposal "
        } else {
            " Topic Details "
        };
        let footer = if review {
            "a approve (move only) · s approve & start · r reject · ↑/↓ scroll · Esc close"
        } else {
            "s start · c recover · x retry failed feature · ↑/↓ scroll · Esc close"
        };
        let block = crate::ui::panel(title, color, Color::Cyan)
            .title_bottom(crate::ui::hint(footer, color));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.draw_topic_details(frame, inner, topic_id, scroll, color);
    }

    fn draw_topic_details(
        &self,
        frame: &mut Frame,
        area: Rect,
        topic_id: &str,
        scroll: u16,
        color: bool,
    ) {
        let Some(topic) = self.topic(topic_id) else {
            frame.render_widget(Paragraph::new("Topic is no longer available."), area);
            return;
        };
        frame.render_widget(
            Paragraph::new(topic_lines(topic, color))
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0)),
            area,
        );
    }

    fn draw_confirmation(
        &self,
        frame: &mut Frame,
        area: Rect,
        topic_id: &str,
        action: &ConfirmedAction,
        color: bool,
    ) {
        let Some(topic) = self.topic(topic_id) else {
            return;
        };
        let verb = match action {
            ConfirmedAction::Approve => "Approve selected requests (move messages only)",
            ConfirmedAction::ApproveAndStart => {
                "Approve, move the requests, and start the workflow"
            }
            ConfirmedAction::Start => "Start topic workflow",
            ConfirmedAction::Recover => "Recover topic workflow",
            ConfirmedAction::Retry { .. } => "Retry topic feature",
        };
        let mut lines = topic_lines(topic, color);
        lines.extend([
            Line::default(),
            Line::styled(
                verb,
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Yellow),
            ),
            Line::from(match action {
                ConfirmedAction::Approve => {
                    "Moves the selected requests into this topic. Harness work does not start yet."
                }
                ConfirmedAction::ApproveAndStart => {
                    "Moves the selected requests, then starts the builder and reviewer once."
                }
                ConfirmedAction::Start => {
                    "Starts the builder and reviewer for every pending feature."
                }
                ConfirmedAction::Recover => {
                    "Resumes interrupted harness work from the last recorded state."
                }
                ConfirmedAction::Retry { .. } => "Starts a new attempt for this feature only.",
            }),
            Line::from("No automatic merge will occur; inspect ready revisions in Topic Details."),
            Line::from("Enter/y confirm · Esc/n cancel"),
        ]);
        frame.render_widget(Clear, area);
        let block = crate::ui::panel(" Confirm Topic Action ", color, Color::Yellow);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn draw_reject(
        &self,
        frame: &mut Frame,
        area: Rect,
        topic_id: &str,
        reason: &InputLine,
        color: bool,
    ) {
        let mut lines = vec![Line::from(format!(
            "Reject proposal for {}",
            self.topic(topic_id)
                .map(|t| t.name.as_str())
                .unwrap_or(topic_id)
        ))];
        lines.push(Line::from(
            "A rejection does not move the selected request rows.",
        ));
        lines.push(Line::from(format!("Reason: {}", reason.expanded())));
        lines.push(Line::from("Enter reject · Esc cancel"));
        let block = crate::ui::panel(" Reject Topic Proposal ", color, Color::Red);
        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub node: Option<Node>,
    pub depth: usize,
    pub label: String,
    pub status: String,
    pub section: bool,
}

pub fn topic_status_label(topic: &TopicInfo) -> String {
    if topic
        .features
        .iter()
        .any(|feature| feature.status == TopicFeatureStatus::NeedsUser)
    {
        return "Needs you".into();
    }
    if topic
        .features
        .iter()
        .any(|feature| feature.status == TopicFeatureStatus::Failed)
    {
        return "Failed".into();
    }
    if topic
        .features
        .iter()
        .any(|feature| feature.status == TopicFeatureStatus::Building)
    {
        return "Building".into();
    }
    if topic
        .features
        .iter()
        .any(|feature| feature.status == TopicFeatureStatus::Reviewing)
    {
        return "Reviewing".into();
    }
    match topic.status {
        TopicStatus::PendingApproval => "Needs approval".into(),
        TopicStatus::Approved => "Ready".into(),
        TopicStatus::Rejected => "Rejected".into(),
        TopicStatus::Running => "Building".into(),
        TopicStatus::Complete => "Ready".into(),
        TopicStatus::Failed => "Failed".into(),
    }
}

fn attempt_status_label(attempt: &TopicAttemptInfo) -> String {
    match attempt.status {
        TopicAttemptStatus::Building => "Building",
        TopicAttemptStatus::AwaitingReview => "Awaiting review",
        TopicAttemptStatus::Reviewing => "Reviewing",
        TopicAttemptStatus::Passed => "Ready",
        TopicAttemptStatus::Findings => "Reviewing",
        TopicAttemptStatus::NeedsUser => "Needs you",
        TopicAttemptStatus::Failed | TopicAttemptStatus::Abandoned => "Failed",
    }
    .into()
}

fn feature_status_label(status: TopicFeatureStatus) -> &'static str {
    match status {
        TopicFeatureStatus::Pending => "Pending",
        TopicFeatureStatus::Building => "Building",
        TopicFeatureStatus::Reviewing => "Reviewing",
        TopicFeatureStatus::Ready => "Ready",
        TopicFeatureStatus::NeedsUser => "Needs you",
        TopicFeatureStatus::Failed => "Failed",
    }
}

fn harness_label(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Claude => "Claude",
        HarnessKind::Codex => "Codex",
    }
}

fn role_label(role: formless_protocol::HarnessRole) -> &'static str {
    match role {
        formless_protocol::HarnessRole::Build => "Build",
        formless_protocol::HarnessRole::Review => "Review",
    }
}

fn model_suffix(model: Option<&str>) -> String {
    model.map(|model| format!(" · {model}")).unwrap_or_default()
}

fn status_color(status: &str) -> Color {
    match status {
        "Needs you" | "Needs approval" | "Failed" => Color::Red,
        "Building" | "Reviewing" | "Awaiting review" => Color::Yellow,
        "Ready" => Color::Green,
        _ => Color::Gray,
    }
}

fn topic_lines(topic: &TopicInfo, color: bool) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                topic.name.clone(),
                Style::default().add_modifier(Modifier::BOLD).fg(if color {
                    Color::Cyan
                } else {
                    Color::Reset
                }),
            ),
            Span::raw(format!("  · {}", topic_status_label(topic))),
        ]),
        Line::from(format!("Repository: {}", topic.repo.display())),
        Line::from(format!("Base revision: {}", topic.base_revision)),
        Line::from(format!(
            "Builder: {}{}",
            harness_label(topic.builder),
            model_suffix(topic.builder_model.as_deref())
        )),
        Line::from(format!(
            "Reviewer: {}{}",
            harness_label(topic.reviewer),
            model_suffix(topic.reviewer_model.as_deref())
        )),
        Line::from("Requests / features:"),
    ];
    if topic.features.is_empty() {
        lines.push(Line::from("  No feature requests."));
    }
    for feature in &topic.features {
        lines.push(Line::from(format!(
            "  #{} · source {} · {} · review round {}",
            feature.source_seq,
            feature.source_message_id,
            feature_status_label(feature.status),
            feature.review_round
        )));
        lines.push(Line::from(format!("  Request: {}", feature.request)));
        if let Some(revision) = &feature.ready_revision {
            lines.push(Line::from(format!("  Ready revision: {revision}")));
        }
        if let Some(detail) = &feature.detail {
            lines.push(Line::from(format!("  Details: {detail}")));
        }
        for attempt in topic
            .attempts
            .iter()
            .filter(|attempt| attempt.feature_id == feature.id)
        {
            lines.push(Line::from(format!(
                "    {} {} · round {} · {} · conversation {}",
                harness_label(attempt.harness),
                role_label(attempt.role),
                attempt.round,
                attempt_status_label(attempt),
                attempt.conversation_session_id
            )));
            if let Some(revision) = &attempt.revision {
                lines.push(Line::from(format!("    Revision: {revision}")));
            }
            if let Some(detail) = &attempt.detail {
                lines.push(Line::from(format!("    Details: {detail}")));
            }
        }
    }
    lines.extend([
        Line::default(),
        Line::from("No automatic merge: ready revisions remain available for manual review."),
    ]);
    lines
}

fn centered(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let width = area
        .width
        .saturating_mul(width_pct)
        .div_ceil(100)
        .min(area.width);
    let height = area
        .height
        .saturating_mul(height_pct)
        .div_ceil(100)
        .min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            forked_from: None,
            fork_point_seq: None,
            name: id.into(),
            model: None,
            reasoning_effort: None,
            created_at: String::new(),
            updated_at: String::new(),
            cwd: Some("/repo".into()),
            message_count: 0,
            preview: None,
            last_run_status: None,
            pinned: false,
            archived: false,
            tags: Vec::new(),
            instructions: None,
        }
    }

    fn topic(id: &str, status: TopicStatus) -> TopicInfo {
        TopicInfo {
            id: id.into(),
            name: format!("Topic {id}"),
            repo: PathBuf::from("/repo"),
            base_revision: "base".into(),
            source_session_id: "main".into(),
            topic_session_id: Some(format!("topic-session-{id}")),
            status,
            builder: HarnessKind::Claude,
            builder_model: None,
            reviewer: HarnessKind::Codex,
            reviewer_model: None,
            features: vec![],
            attempts: vec![],
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn attempt(id: &str, session_id: &str) -> TopicAttemptInfo {
        TopicAttemptInfo {
            id: id.into(),
            topic_id: "t1".into(),
            feature_id: "f1".into(),
            round: 1,
            role: formless_protocol::HarnessRole::Build,
            harness: HarnessKind::Claude,
            status: TopicAttemptStatus::Building,
            revision: None,
            harness_session_id: None,
            conversation_session_id: session_id.into(),
            detail: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn message(id: &str, seq: i64, role: &str, content: &str) -> MessageInfo {
        MessageInfo {
            id: id.into(),
            session_id: "main".into(),
            seq,
            role: role.into(),
            content: content.into(),
            created_at: String::new(),
            meta: None,
        }
    }

    #[test]
    fn proposal_defaults_latest_request_and_contains_only_explicit_selections() {
        let source = session("main");
        let messages = [
            message("u1", 1, "user", "First request"),
            message("a1", 2, "assistant", "Answer"),
            message("u2", 3, "user", "Second request"),
        ];
        let form = ProposalForm::new(&source, &messages, "/repo", "Work");
        let request = form.request().unwrap();
        assert_eq!(request.message_ids, ["u2"]);
        assert_eq!(request.source_session_id, "main");
        assert_eq!(request.builder, HarnessKind::Claude);
        assert_eq!(request.reviewer, HarnessKind::Codex);
    }

    #[test]
    fn topic_list_preserves_stable_selection_and_exposes_attempt_children() {
        let mut state = State::default();
        let mut first = topic("t1", TopicStatus::Running);
        first.attempts.push(attempt("a1", "harness-1"));
        state.update_topics(vec![first.clone(), topic("t2", TopicStatus::Complete)]);
        state.selected = Node::Attempt {
            topic_id: "t1".into(),
            attempt_id: "a1".into(),
        };
        let pending = state.update_topics(vec![topic("t2", TopicStatus::Complete), first]);
        assert_eq!(
            state.selected(),
            &Node::Attempt {
                topic_id: "t1".into(),
                attempt_id: "a1".into(),
            }
        );
        assert_eq!(pending, None);
        assert!(state
            .rows()
            .iter()
            .any(|row| row.label.contains("Claude Build")));
        assert!(state.is_harness_session("harness-1"));
    }

    #[test]
    fn pending_topics_are_reviewed_once_and_never_approved_by_refresh() {
        let mut state = State::default();
        let pending = topic("t1", TopicStatus::PendingApproval);
        assert_eq!(
            state.update_topics(vec![pending.clone()]),
            Some("t1".into())
        );
        state.open_review("t1");
        state.mark_pending_reviewed("t1");
        assert_eq!(state.update_topics(vec![pending]), None);
        assert!(matches!(state.overlay, Some(Overlay::Review { .. })));
    }

    #[test]
    fn a_reviewed_pending_topic_does_not_hide_later_ones() {
        let mut state = State::default();
        state.mark_pending_reviewed("t1");
        let topics = vec![
            topic("t1", TopicStatus::PendingApproval),
            topic("t2", TopicStatus::PendingApproval),
        ];
        assert_eq!(state.update_topics(topics), Some("t2".into()));
    }

    #[test]
    fn paste_edits_the_focused_proposal_field_only() {
        let mut state = State::default();
        let messages = vec![message("m1", 1, "user", "add X")];
        state.open_proposal(&session("main"), &messages, "/repo", "");
        state.paste("ignored on the message step");
        state.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.paste("formless\n");
        let proposal = state.proposal.as_ref().unwrap();
        assert_eq!(proposal.fields[0].expanded(), "formless ");
    }

    #[test]
    fn one_topic_start_is_claimed_once_even_across_refreshes() {
        let mut state = State::default();
        assert!(state.claim_start("t1"));
        assert!(!state.claim_start("t1"));
    }

    #[test]
    fn old_daemon_without_capability_disables_topic_ui() {
        let mut state = State::default();
        state.open_picker();
        state.set_capability(false);
        assert!(!state.enabled());
        assert!(!state.overlay_open());
    }

    #[test]
    fn statuses_distinguish_attention_and_workflow_progress() {
        assert_eq!(
            topic_status_label(&topic("needs", TopicStatus::PendingApproval)),
            "Needs approval"
        );
        assert_eq!(
            topic_status_label(&topic("ready", TopicStatus::Complete)),
            "Ready"
        );
        assert_eq!(
            topic_status_label(&topic("failed", TopicStatus::Failed)),
            "Failed"
        );
    }
}
