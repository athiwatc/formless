use crate::input::InputLine;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use formless_protocol::{
    HarnessKind, HarnessPermissionOption, HarnessPermissionOptionKind, MessageInfo, Request,
    SessionInfo, StepAccess, TopicArtifact, TopicAttemptInfo, TopicAttemptStatus,
    TopicFeatureStatus, TopicInfo, TopicLimits, TopicProposalRequest, TopicRuleDecision,
    TopicStatus, TopicStepOutline, WorkspaceKind,
};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::collections::HashSet;
use std::path::PathBuf;

pub const CAPABILITY: &str = formless_protocol::TOPIC_ORCHESTRATION_FEATURE;

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

/// `(topic to refresh, request, note on success)`.
pub type ControlRequest = (Option<String>, Request, String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmedAction {
    Approve,
    ApproveAndStart,
    Start,
    Cancel,
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
    /// `option_id: None` denies.
    PermissionDecision {
        permission_id: String,
        option_id: Option<String>,
    },
    Refresh,
}

/// A harness asking the user for permission. Only the user answers it.
#[derive(Clone, Debug)]
pub struct HarnessPermission {
    pub permission_id: String,
    pub session_id: String,
    pub topic_id: String,
    pub attempt_id: String,
    pub harness: HarnessKind,
    pub acp_agent: Option<String>,
    pub title: String,
    pub kind: String,
    pub raw_input: Option<serde_json::Value>,
    pub options: Vec<HarnessPermissionOption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProposalStep {
    Messages,
    Fields,
    Review,
    Sending,
}

/// Starting outlines. The outline is advice: the Formless model in the
/// Topic Chat decides the actual steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Open,
    Code,
    Accounting,
}

impl Preset {
    const ALL: [Preset; 3] = [Preset::Open, Preset::Code, Preset::Accounting];

    fn label(self) -> &'static str {
        match self {
            Preset::Open => "None (Formless plans the steps)",
            Preset::Code => "Code: Implement → Review",
            Preset::Accounting => "Accounting: Scan receipts → Do accounting → Create reports",
        }
    }

    fn steps(self) -> Vec<StepForm> {
        use HarnessKind::{Claude, Codex};
        use StepAccess::{ReadOnly, Write};
        let steps: &[(&str, &str, Option<HarnessKind>, StepAccess)] = match self {
            Preset::Open => &[],
            Preset::Code => &[
                (
                    "Implement",
                    "Make the requested change, with tests, and commit it.",
                    Some(Claude),
                    Write,
                ),
                (
                    "Review",
                    "Review the change against the request and report findings.",
                    Some(Codex),
                    ReadOnly,
                ),
            ],
            Preset::Accounting => &[
                (
                    "Scan receipts",
                    "Extract vendor, date, amount and category from each receipt.",
                    None,
                    Write,
                ),
                (
                    "Do accounting",
                    "Record the entries and reconcile the totals.",
                    None,
                    Write,
                ),
                (
                    "Create reports",
                    "Produce the requested reports from the reconciled entries.",
                    None,
                    Write,
                ),
            ],
        };
        steps
            .iter()
            .map(|(title, instructions, harness, access)| {
                StepForm::new(title, instructions, *harness, *access)
            })
            .collect()
    }
}

/// Harness choices for a step; `None` lets the orchestrator pick.
const HARNESSES: [Option<HarnessKind>; 7] = [
    None,
    Some(HarnessKind::Claude),
    Some(HarnessKind::Codex),
    Some(HarnessKind::Pi),
    Some(HarnessKind::OpenCode),
    Some(HarnessKind::Antigravity),
    Some(HarnessKind::Muse),
];

struct StepForm {
    title: InputLine,
    instructions: InputLine,
    harness: Option<HarnessKind>,
    model: InputLine,
    access: StepAccess,
}

impl StepForm {
    fn new(
        title: &str,
        instructions: &str,
        harness: Option<HarnessKind>,
        access: StepAccess,
    ) -> Self {
        let mut step = Self {
            title: InputLine::default(),
            instructions: InputLine::default(),
            harness,
            model: InputLine::default(),
            access,
        };
        step.title.set(title);
        step.instructions.set(instructions);
        step
    }

    fn outline(&self) -> Option<TopicStepOutline> {
        let title = self.title.expanded().trim().to_string();
        (!title.is_empty()).then(|| TopicStepOutline {
            title,
            instructions: self.instructions.expanded().trim().to_string(),
            harness: self.harness,
            acp_agent: None,
            model: nonblank(&self.model.expanded()),
            access: self.access,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepField {
    Title,
    Instructions,
    Harness,
    Model,
    Access,
}

const STEP_FIELDS: [StepField; 5] = [
    StepField::Title,
    StepField::Instructions,
    StepField::Harness,
    StepField::Model,
    StepField::Access,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Workspace,
    Parent,
    Preset,
    Step(usize, StepField),
}

const HEAD_FIELDS: usize = 4;

struct ProposalForm {
    source_session_id: String,
    messages: Vec<MessageInfo>,
    selected: HashSet<String>,
    message_index: usize,
    step: ProposalStep,
    name: InputLine,
    workspace: InputLine,
    /// `(parent_topic_id, label)`; Main Chat comes first only when proposing from Main Chat.
    parents: Vec<(Option<String>, String)>,
    parent_index: usize,
    preset: Preset,
    steps: Vec<StepForm>,
    field_index: usize,
    scroll: u16,
}

impl ProposalForm {
    fn new(
        source: &SessionInfo,
        messages: &[MessageInfo],
        workspace: &str,
        name: &str,
        parents: Vec<(Option<String>, String)>,
        parent: Option<&str>,
    ) -> Self {
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
        let mut name_line = InputLine::default();
        name_line.set(name);
        let mut workspace_line = InputLine::default();
        workspace_line.set(workspace);
        let parent_index = parents
            .iter()
            .position(|(id, _)| id.as_deref() == parent)
            .unwrap_or(0);
        Self {
            source_session_id: source.id.clone(),
            messages: user_messages,
            selected,
            message_index: 0,
            step: ProposalStep::Messages,
            name: name_line,
            workspace: workspace_line,
            parents,
            parent_index,
            preset: Preset::Open,
            steps: Vec::new(),
            field_index: 0,
            scroll: 0,
        }
    }

    fn request(&self) -> Option<TopicProposalRequest> {
        let name = self.name.expanded().trim().to_string();
        let message_ids: Vec<_> = self
            .messages
            .iter()
            .filter(|message| self.selected.contains(&message.id))
            .map(|message| message.id.clone())
            .collect();
        if name.is_empty() || message_ids.is_empty() {
            return None;
        }
        Some(TopicProposalRequest {
            parent_topic_id: self
                .parents
                .get(self.parent_index)
                .and_then(|(id, _)| id.clone()),
            source_session_id: self.source_session_id.clone(),
            message_ids,
            // Manual proposals submit an empty feature list. The daemon
            // interprets that as one work item per selected message.
            features: vec![],
            name,
            workspace: nonblank(&self.workspace.expanded()).map(PathBuf::from),
            outline: self.steps.iter().filter_map(StepForm::outline).collect(),
        })
    }

    fn parent_label(&self) -> &str {
        self.parents
            .get(self.parent_index)
            .map(|(_, label)| label.as_str())
            .unwrap_or("Main Chat")
    }

    fn selected_messages(&self) -> impl Iterator<Item = &MessageInfo> {
        self.messages
            .iter()
            .filter(|message| self.selected.contains(&message.id))
    }

    fn field_count(&self) -> usize {
        HEAD_FIELDS + self.steps.len() * STEP_FIELDS.len()
    }

    fn field(&self) -> Field {
        match self.field_index {
            0 => Field::Name,
            1 => Field::Workspace,
            2 => Field::Parent,
            3 => Field::Preset,
            n => {
                let n = n - HEAD_FIELDS;
                Field::Step(n / STEP_FIELDS.len(), STEP_FIELDS[n % STEP_FIELDS.len()])
            }
        }
    }

    fn cycle(&mut self, forward: bool) -> bool {
        fn next(index: usize, len: usize, forward: bool) -> usize {
            if forward {
                (index + 1) % len
            } else {
                (index + len - 1) % len
            }
        }
        match self.field() {
            Field::Parent => {
                self.parent_index = next(self.parent_index, self.parents.len().max(1), forward);
            }
            Field::Preset => {
                let index = Preset::ALL
                    .iter()
                    .position(|p| *p == self.preset)
                    .unwrap_or(0);
                self.preset = Preset::ALL[next(index, Preset::ALL.len(), forward)];
                self.steps = self.preset.steps();
            }
            Field::Step(step, StepField::Harness) => {
                let harness = &mut self.steps[step].harness;
                let index = HARNESSES.iter().position(|h| h == harness).unwrap_or(0);
                *harness = HARNESSES[next(index, HARNESSES.len(), forward)];
            }
            Field::Step(step, StepField::Access) => {
                let access = &mut self.steps[step].access;
                *access = match access {
                    StepAccess::ReadOnly => StepAccess::Write,
                    StepAccess::Write => StepAccess::ReadOnly,
                };
            }
            _ => return false,
        }
        true
    }

    fn add_step(&mut self) {
        let at = match self.field() {
            Field::Step(step, _) => step + 1,
            _ => self.steps.len(),
        };
        self.steps
            .insert(at, StepForm::new("", "", None, StepAccess::Write));
        self.field_index = HEAD_FIELDS + at * STEP_FIELDS.len();
    }

    fn remove_step(&mut self) {
        if let Field::Step(step, _) = self.field() {
            self.steps.remove(step);
            self.field_index = self.field_index.min(self.field_count() - 1);
        }
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
                let count = self.field_count();
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => return Action::Close,
                    KeyCode::Char('n') if ctrl => self.add_step(),
                    KeyCode::Char('d') if ctrl => self.remove_step(),
                    KeyCode::Tab | KeyCode::Down => {
                        self.field_index = (self.field_index + 1) % count;
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        self.field_index = (self.field_index + count - 1) % count;
                    }
                    KeyCode::Left | KeyCode::Right if self.cycle(key.code == KeyCode::Right) => {}
                    KeyCode::Enter => {
                        if self.field_index + 1 >= count {
                            self.step = ProposalStep::Review;
                        } else {
                            self.field_index += 1;
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
                KeyCode::Up => {
                    self.scroll = self.scroll.saturating_sub(1);
                    Action::None
                }
                KeyCode::Down => {
                    self.scroll = self.scroll.saturating_add(1);
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
        match self.field() {
            Field::Name => Some(&mut self.name),
            Field::Workspace => Some(&mut self.workspace),
            Field::Step(step, StepField::Title) => Some(&mut self.steps[step].title),
            Field::Step(step, StepField::Instructions) => Some(&mut self.steps[step].instructions),
            Field::Step(step, StepField::Model) => Some(&mut self.steps[step].model),
            _ => None,
        }
    }

    fn field_lines(&self) -> Vec<(bool, String, String)> {
        let mut rows = vec![
            ("Topic name".to_string(), self.name.expanded()),
            (
                "Workspace (blank = scratch folder)".to_string(),
                self.workspace.expanded(),
            ),
            ("Under".to_string(), format!("‹ {} ›", self.parent_label())),
            (
                "Outline preset".to_string(),
                format!("‹ {} ›", self.preset.label()),
            ),
        ];
        for (index, step) in self.steps.iter().enumerate() {
            let n = index + 1;
            rows.extend([
                (format!("Step {n} title"), step.title.expanded()),
                (
                    format!("Step {n} instructions"),
                    step.instructions.expanded(),
                ),
                (
                    format!("Step {n} harness"),
                    format!("‹ {} ›", step_harness_label(step.harness, None)),
                ),
                (format!("Step {n} model (optional)"), step.model.expanded()),
                (
                    format!("Step {n} access"),
                    format!("‹ {} ›", access_label(step.access)),
                ),
            ]);
        }
        rows.into_iter()
            .enumerate()
            .map(|(index, (label, value))| (index == self.field_index, label, value))
            .collect()
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
    permissions: Vec<HarnessPermission>,
    rendered_permission_id: Option<String>,
    permission_options_visible: bool,
    permission_scroll_id: Option<String>,
    permission_input_scroll: u16,
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
            permissions: Vec::new(),
            rendered_permission_id: None,
            permission_options_visible: false,
            permission_scroll_id: None,
            permission_input_scroll: 0,
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
        if !self.permissions.is_empty() {
            return;
        }
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
        self.overlay.is_some() || !self.permissions.is_empty()
    }

    pub fn push_permission(&mut self, permission: HarnessPermission) {
        if !self
            .permissions
            .iter()
            .any(|open| open.permission_id == permission.permission_id)
        {
            self.permissions.push(permission);
            self.rendered_permission_id = None;
            self.permission_options_visible = false;
            if self.permissions.len() == 1 {
                self.permission_scroll_id = None;
                self.permission_input_scroll = 0;
            }
        }
    }

    pub fn close_permission(&mut self, permission_id: &str) -> Option<HarnessPermission> {
        let index = self
            .permissions
            .iter()
            .position(|open| open.permission_id == permission_id)?;
        let permission = self.permissions.remove(index);
        self.rendered_permission_id = None;
        self.permission_options_visible = false;
        if index == 0 {
            self.permission_scroll_id = None;
            self.permission_input_scroll = 0;
        }
        Some(permission)
    }

    /// Drops prompts from a daemon that went away; returns how many.
    pub fn clear_permissions(&mut self) -> usize {
        self.rendered_permission_id = None;
        self.permission_options_visible = false;
        self.permission_scroll_id = None;
        self.permission_input_scroll = 0;
        std::mem::take(&mut self.permissions).len()
    }

    fn permission_key(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Repeat {
            return Action::None;
        }
        let Some(permission) = self.permissions.first() else {
            return Action::None;
        };
        if self.rendered_permission_id.as_deref() != Some(permission.permission_id.as_str()) {
            return Action::None;
        }
        match key.code {
            KeyCode::Up => {
                self.permission_input_scroll = self.permission_input_scroll.saturating_sub(1);
                return Action::None;
            }
            KeyCode::Down => {
                self.permission_input_scroll = self.permission_input_scroll.saturating_add(1);
                return Action::None;
            }
            KeyCode::PageUp => {
                self.permission_input_scroll = self.permission_input_scroll.saturating_sub(10);
                return Action::None;
            }
            KeyCode::PageDown => {
                self.permission_input_scroll = self.permission_input_scroll.saturating_add(10);
                return Action::None;
            }
            KeyCode::Home => {
                self.permission_input_scroll = 0;
                return Action::None;
            }
            KeyCode::End => {
                self.permission_input_scroll = u16::MAX;
                return Action::None;
            }
            _ => {}
        }
        let decide = |option_id: Option<String>| Action::PermissionDecision {
            permission_id: permission.permission_id.clone(),
            option_id,
        };
        let action = match key.code {
            KeyCode::Esc | KeyCode::Char('d' | 'D') => decide(None),
            KeyCode::Char(c @ '1'..='9') => {
                if !self.permission_options_visible {
                    return Action::None;
                }
                match permission.options.get(c as usize - '1' as usize) {
                    Some(option) => decide(Some(option.option_id.clone())),
                    None => return Action::None,
                }
            }
            _ => return Action::None,
        };
        self.permissions.remove(0);
        self.rendered_permission_id = None;
        self.permission_options_visible = false;
        self.permission_scroll_id = None;
        self.permission_input_scroll = 0;
        action
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
        workspace: &str,
        name: &str,
        parent: Option<&str>,
    ) {
        // The daemon reads a missing parent as "under the source topic", so
        // Main Chat can only be chosen when proposing from Main Chat.
        let mut parents = match parent {
            Some(_) => vec![],
            None => vec![(None, "Main Chat".to_string())],
        };
        parents.extend(
            self.topics
                .iter()
                .filter(|topic| {
                    !matches!(
                        topic.status,
                        TopicStatus::PendingApproval | TopicStatus::Rejected
                    )
                })
                .map(|topic| {
                    (
                        Some(topic.id.clone()),
                        self.topic_path(&topic.id).join(" / "),
                    )
                }),
        );
        self.proposal = Some(ProposalForm::new(
            source, messages, workspace, name, parents, parent,
        ));
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
            || matches!(action, ConfirmedAction::Cancel)
                && !matches!(
                    topic.status,
                    TopicStatus::Approved | TopicStatus::Running | TopicStatus::NeedsUser
                )
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

    /// User-only topic controls. `None` means `args` is another `/topic`
    /// command; `Some(Ok((topic, request, done)))` is ready to send.
    pub fn control_command(&self, args: &str) -> Option<Result<ControlRequest, String>> {
        let words: Vec<_> = args.split_whitespace().collect();
        let known = |id: &str| {
            if let Some(topic) = self.topic(id) {
                return Ok(topic.id.clone());
            }
            let mut named = self.topics.iter().filter(|topic| topic.name == id);
            match (named.next(), named.next()) {
                (Some(topic), None) => Ok(topic.id.clone()),
                (Some(_), Some(_)) => Err(format!("Several topics are named {id}; use its ID.")),
                (None, _) => Err(format!("No topic named {id}; use /topics.")),
            }
        };
        let result = match words.as_slice() {
            ["harnesses"] => Ok((None, Request::ListHarnesses, String::new())),
            ["context", id, flag @ ("on" | "off")] => known(id).map(|topic_id| {
                let allow = *flag == "on";
                let done = if allow {
                    "Harnesses in this topic may now read conversations outside it."
                } else {
                    "Harnesses in this topic now read only this topic branch."
                };
                (
                    Some(topic_id.clone()),
                    Request::SetTopicContextAccess { topic_id, allow },
                    done.into(),
                )
            }),
            ["context", ..] => Err("Usage: /topic context TOPIC on|off".into()),
            ["limits", id, retries, attempts] => {
                match (retries.parse::<u32>(), attempts.parse::<u32>()) {
                    (Ok(retries), Ok(attempts)) => known(id).map(|topic_id| {
                        (
                            Some(topic_id.clone()),
                            Request::SetTopicLimits {
                                topic_id,
                                limits: TopicLimits {
                                    max_retries_per_step: retries,
                                    max_attempts_per_topic: attempts,
                                },
                            },
                            format!(
                                "Limits set: {retries} retries per step, {attempts} attempts per topic."
                            ),
                        )
                    }),
                    _ => Err("Limits must be whole numbers.".into()),
                }
            }
            ["limits", ..] => Err("Usage: /topic limits TOPIC RETRIES_PER_STEP ATTEMPTS_PER_TOPIC".into()),
            [verb @ ("allow" | "deny"), id, pattern @ ..] if !pattern.is_empty() => {
                let pattern = pattern.join(" ");
                let (decision, label) = if *verb == "allow" {
                    (TopicRuleDecision::AllowOnce, "allows")
                } else {
                    (TopicRuleDecision::Reject, "denies")
                };
                known(id).map(|topic_id| {
                    (
                        Some(topic_id.clone()),
                        Request::AddTopicPermissionRule {
                            topic_id,
                            harness: None,
                            acp_agent: None,
                            kind: None,
                            pattern: pattern.clone(),
                            decision,
                        },
                        format!("Rule added: Formless {label} permission requests matching `{pattern}`."),
                    )
                })
            }
            ["allow" | "deny", ..] => Err(
                "Usage: /topic allow|deny TOPIC PATTERN (* and ? match the request title or command)".into(),
            ),
            ["unrule", id, rule_id] => known(id).and_then(|topic_id| {
                let exists = self.topic(&topic_id).is_some_and(|topic| {
                    topic.permission_rules.iter().any(|rule| rule.rule_id == *rule_id)
                });
                if !exists {
                    return Err(format!("This topic has no permission rule {rule_id}."));
                }
                Ok((
                    Some(topic_id.clone()),
                    Request::RemoveTopicPermissionRule {
                        topic_id,
                        rule_id: rule_id.to_string(),
                    },
                    "Permission rule removed.".into(),
                ))
            }),
            ["unrule", ..] => Err("Usage: /topic unrule TOPIC RULE_ID".into()),
            _ => return None,
        };
        Some(result)
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
                .ok_or_else(|| self.unavailable_action(topic_id)),
            ["cancel"] => self.open_action_for_selected(ConfirmedAction::Cancel),
            ["cancel", topic_id] => self
                .open_action(topic_id, ConfirmedAction::Cancel)
                .then_some(())
                .ok_or_else(|| self.unavailable_action(topic_id)),
            ["recover"] => self.open_action_for_selected(ConfirmedAction::Recover),
            ["recover", topic_id] => self
                .open_action(topic_id, ConfirmedAction::Recover)
                .then_some(())
                .ok_or_else(|| self.unavailable_action(topic_id)),
            ["retry", topic_id, feature_id] => self
                .open_action(
                    topic_id,
                    ConfirmedAction::Retry {
                        feature_id: feature_id.to_string(),
                    },
                )
                .then_some(())
                .ok_or_else(|| self.unavailable_action(topic_id)),
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
                .ok_or_else(|| self.unavailable_action(&topic_id))
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
                .ok_or_else(|| self.unavailable_action(topic_id)),
            [topic_id] => self
                .open_details(topic_id)
                .then_some(())
                .ok_or_else(|| self.unavailable_action(topic_id)),
            _ => Err(
                "Usage: /topic [ID|start|cancel|recover|retry TOPIC FEATURE|reject TOPIC REASON]"
                    .into(),
            ),
        }
    }

    fn unavailable_action(&self, topic_id: &str) -> String {
        match self.topic(topic_id) {
            Some(topic) => format!(
                "{} is {}; that action is not available now.",
                topic.name,
                topic_status_label(topic)
            ),
            None => format!("No topic named {topic_id}; use /topics."),
        }
    }

    fn open_action_for_selected(&mut self, action: ConfirmedAction) -> Result<(), String> {
        let Some(Node::Topic(topic_id)) = self.selected_topic_node() else {
            return Err("Select a topic with /topics first.".into());
        };
        self.open_action(&topic_id, action)
            .then_some(())
            .ok_or_else(|| self.unavailable_action(&topic_id))
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
        if !self.permissions.is_empty() {
            return self.permission_key(key);
        }
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
                    KeyCode::Char('k') if !review => {
                        self.open_action(&topic_id, ConfirmedAction::Cancel);
                        Action::None
                    }
                    KeyCode::Char('x') if !review => {
                        if let Some(feature) = self.topic(&topic_id).and_then(|topic| {
                            topic.features.iter().find(|feature| {
                                matches!(
                                    feature.status,
                                    TopicFeatureStatus::Failed
                                        | TopicFeatureStatus::NeedsUser
                                        | TopicFeatureStatus::Cancelled
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
            status: String::new(),
            section: false,
        }];
        rows.push(TreeRow {
            node: None,
            depth: 0,
            label: "Topic Chats".into(),
            status: String::new(),
            section: true,
        });
        let mut seen = HashSet::new();
        for topic in &self.topics {
            let parent_known = topic
                .parent_topic_id
                .as_deref()
                .is_some_and(|parent| self.topic(parent).is_some());
            if !parent_known {
                self.push_topic_rows(&mut rows, topic, 0, &mut seen);
            }
        }
        // A cycle has no root; still list every topic once.
        for topic in &self.topics {
            self.push_topic_rows(&mut rows, topic, 0, &mut seen);
        }
        rows
    }

    fn push_topic_rows(
        &self,
        rows: &mut Vec<TreeRow>,
        topic: &TopicInfo,
        depth: usize,
        seen: &mut HashSet<String>,
    ) {
        if !seen.insert(topic.id.clone()) {
            return;
        }
        rows.push(TreeRow {
            node: Some(Node::Topic(topic.id.clone())),
            depth,
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
                depth: depth + 1,
                label: attempt_label(attempt),
                status: attempt_status_label(attempt),
                section: false,
            });
        }
        for child in self
            .topics
            .iter()
            .filter(|child| child.parent_topic_id.as_deref() == Some(topic.id.as_str()))
        {
            self.push_topic_rows(rows, child, depth + 1, seen);
        }
    }

    /// `Topic / Step · Harness` for a harness conversation, so its
    /// questions say who is asking.
    pub fn harness_label_for_session(&self, session_id: &str) -> Option<String> {
        self.topics.iter().find_map(|topic| {
            topic
                .attempts
                .iter()
                .find(|attempt| attempt.conversation_session_id == session_id)
                .map(|attempt| {
                    format!(
                        "{} / {}",
                        self.topic_path(&topic.id).join(" / "),
                        attempt_label(attempt)
                    )
                })
        })
    }

    /// Topic names from the outermost ancestor down to `topic_id`.
    pub fn topic_path(&self, topic_id: &str) -> Vec<&str> {
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut next = self.topic(topic_id);
        while let Some(topic) = next {
            if !seen.insert(topic.id.as_str()) {
                break;
            }
            path.push(topic.name.as_str());
            next = topic
                .parent_topic_id
                .as_deref()
                .and_then(|parent| self.topic(parent));
        }
        path.reverse();
        path
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
                        if row.status.is_empty() {
                            String::new()
                        } else {
                            format!("  {}", row.status)
                        },
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
                .map(|_| format!("Main Chat / {}", self.topic_path(&topic_id).join(" / ")))
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
                })
                .map(|attempt| {
                    format!(
                        "Main Chat / {} / {} · read-only",
                        self.topic_path(&topic_id).join(" / "),
                        attempt_label(attempt)
                    )
                })
                .unwrap_or_else(|| "Harness conversation · read-only".into()),
        }
    }

    pub fn has_permission(&self) -> bool {
        !self.permissions.is_empty()
    }

    /// Drawn last in the frame: permission keys are handled before every
    /// other overlay, so the prompt must never be hidden beneath one.
    pub fn draw_permission_overlay(&mut self, frame: &mut Frame, color: bool) {
        if let Some(permission) = self.permissions.first().cloned() {
            if self.permission_scroll_id.as_deref() != Some(permission.permission_id.as_str()) {
                self.permission_scroll_id = Some(permission.permission_id.clone());
                self.permission_input_scroll = 0;
            }
            let (visible, options_visible) = self.draw_permission(frame, &permission, color);
            self.rendered_permission_id = visible.then(|| permission.permission_id.clone());
            self.permission_options_visible = options_visible;
        } else {
            self.rendered_permission_id = None;
            self.permission_options_visible = false;
            self.permission_scroll_id = None;
            self.permission_input_scroll = 0;
        }
    }

    pub fn draw_overlay(&self, frame: &mut Frame, color: bool) {
        if !self.permissions.is_empty() {
            return;
        }
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

    fn draw_permission(
        &mut self,
        frame: &mut Frame,
        permission: &HarnessPermission,
        color: bool,
    ) -> (bool, bool) {
        let area = centered(frame.area(), 80, 70);
        frame.render_widget(Clear, area);
        let harness = harness_name(permission.harness, permission.acp_agent.as_deref());
        let step = self
            .topic(&permission.topic_id)
            .and_then(|topic| {
                topic
                    .attempts
                    .iter()
                    .find(|attempt| attempt.id == permission.attempt_id)
            })
            .map(|attempt| format!(" / {}", attempt.step_title))
            .unwrap_or_default();
        let topic = self.topic_path(&permission.topic_id).join(" / ");
        let header = vec![
            Line::from(format!(
                "{}{step} · {harness}",
                if topic.is_empty() {
                    &permission.topic_id
                } else {
                    &topic
                }
            )),
            Line::styled(
                permission.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::from(format!(
                "Kind: {} · conversation {}",
                permission.kind, permission.session_id
            )),
        ];
        let mut action_lines = Vec::new();
        for (index, option) in permission.options.iter().enumerate() {
            let note = match option.kind {
                HarnessPermissionOptionKind::AllowAlways => " (this attempt only)",
                _ => "",
            };
            action_lines.push(Line::from(format!("{}. {}{note}", index + 1, option.name)));
        }
        if self.permissions.len() > 1 {
            action_lines.push(Line::from(format!(
                "{} more permission request(s) waiting.",
                self.permissions.len() - 1
            )));
        }
        action_lines.push(Line::from("1-9 choose · Esc/d deny"));
        let block = crate::ui::panel(
            format!(" {harness} asks for permission "),
            color,
            Color::Yellow,
        )
        .title_bottom(crate::ui::hint(
            "only you can answer · no answer means denied",
            color,
        ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return (false, false);
        }
        let wrap = Wrap { trim: false };
        let header_rows = Paragraph::new(header.clone())
            .wrap(wrap)
            .line_count(inner.width);
        let action_rows = Paragraph::new(action_lines.clone())
            .wrap(wrap)
            .line_count(inner.width);
        let minimum = header_rows.saturating_add(action_rows).saturating_add(3);
        if minimum > usize::from(inner.height) {
            frame.render_widget(
                Paragraph::new(
                    "Terminal too small to show permission choices. Resize to choose; Esc/d denies.",
                )
                .wrap(wrap),
                inner,
            );
            return (true, false);
        }
        let chunks = Layout::vertical([
            Constraint::Length(u16::try_from(header_rows).unwrap_or(u16::MAX)),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(u16::try_from(action_rows).unwrap_or(u16::MAX)),
        ])
        .split(inner);
        frame.render_widget(Paragraph::new(header).wrap(wrap), chunks[0]);
        let raw_lines: Vec<Line> = permission
            .raw_input
            .as_ref()
            .map(|input| serde_json::to_string_pretty(input).unwrap_or_default())
            .unwrap_or_else(|| "(no raw input)".into())
            .lines()
            .map(|line| Line::from(format!("  {line}")))
            .collect();
        let raw_input = Paragraph::new(raw_lines).wrap(wrap);
        let total_rows = raw_input.line_count(chunks[2].width);
        let max_scroll = total_rows.saturating_sub(usize::from(chunks[2].height));
        self.permission_input_scroll = self
            .permission_input_scroll
            .min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
        let scroll = usize::from(self.permission_input_scroll);
        let first = scroll.saturating_add(1).min(total_rows);
        let last = scroll
            .saturating_add(usize::from(chunks[2].height))
            .min(total_rows);
        frame.render_widget(
            Paragraph::new(format!(
                "Input rows {first}-{last} of {total_rows} · ↑/↓ PgUp/PgDn Home/End"
            )),
            chunks[1],
        );
        frame.render_widget(raw_input.scroll((self.permission_input_scroll, 0)), chunks[2]);
        frame.render_widget(
            Paragraph::new(action_lines).wrap(wrap),
            chunks[3],
        );
        (true, true)
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
                        "Tab move · ←/→ choose · Ctrl+N add step · Ctrl+D remove step · Enter next · Esc cancel"
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
                let mut lines = vec![Line::from(
                    "Name the topic and outline the work. The outline is advice; Formless decides.",
                )];
                let mut focused_line = 0;
                for (focused, label, value) in proposal.field_lines() {
                    let style = if focused {
                        focused_line = lines.len();
                        Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(if focused { "› " } else { "  " }, style),
                        Span::styled(format!("{label}: "), style),
                        Span::raw(value),
                    ]));
                }
                let scroll = (focused_line + 1).saturating_sub(inner.height as usize);
                frame.render_widget(
                    Paragraph::new(lines).scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
                    inner,
                );
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
                        Line::from(format!("Under: {}", proposal.parent_label())),
                        Line::from(format!(
                            "Workspace: {}",
                            request.workspace.as_ref().map_or_else(
                                || "Scratch folder".to_string(),
                                |path| path.display().to_string()
                            )
                        )),
                        Line::from("Outline (advice; Formless decides the steps):"),
                    ]);
                    lines.extend(outline_lines(&request.outline, "  "));
                    lines.extend([
                        Line::from("Approval moves only the selected requests. Starting needs a separate confirmation."),
                        Line::from("Nothing is merged or pushed automatically."),
                        Line::from("Requests to move:"),
                    ]);
                    for message in proposal.selected_messages() {
                        lines.push(Line::from(format!("#{} [{}]", message.seq, message.id)));
                        lines.push(Line::from(message.content.clone()));
                    }
                } else {
                    lines.push(Line::from("Choose a topic name and at least one request."));
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
            ConfirmedAction::Cancel => "Cancel topic",
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
                    "Moves the selected requests, then starts the Formless orchestrator once."
                }
                ConfirmedAction::Start => {
                    "The Formless model runs the work in the Topic Chat, using the outline as advice."
                }
                ConfirmedAction::Cancel => {
                    "Stops the orchestrator and any running harness work. Nothing starts again until you start it."
                }
                ConfirmedAction::Recover => {
                    "Marks interrupted work failed and abandons its running attempts. Retry a work item to run it again."
                }
                ConfirmedAction::Retry { .. } => {
                    "Resets this work item to pending. Start the topic again to run it."
                }
            }),
            Line::from("Nothing is merged or pushed automatically; outputs are listed in Topic Details."),
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
    let any = |status| {
        topic
            .features
            .iter()
            .any(|feature| feature.status == status)
    };
    if any(TopicFeatureStatus::NeedsUser) {
        return "Needs you".into();
    }
    if any(TopicFeatureStatus::Failed) {
        return "Failed".into();
    }
    match topic.status {
        TopicStatus::PendingApproval => "Needs approval",
        TopicStatus::Approved => "Ready to start",
        TopicStatus::Rejected => "Rejected",
        TopicStatus::Running => "Running",
        TopicStatus::NeedsUser => "Needs you",
        TopicStatus::Complete => "Complete",
        TopicStatus::Failed => "Failed",
        TopicStatus::Cancelled => "Cancelled",
    }
    .into()
}

fn attempt_status_label(attempt: &TopicAttemptInfo) -> String {
    match attempt.status {
        TopicAttemptStatus::Running => "Running",
        TopicAttemptStatus::Succeeded => "Done",
        TopicAttemptStatus::NeedsUser => "Needs you",
        TopicAttemptStatus::Failed | TopicAttemptStatus::Abandoned => "Failed",
        TopicAttemptStatus::Cancelled => "Cancelled",
    }
    .into()
}

fn feature_status_label(status: TopicFeatureStatus) -> &'static str {
    match status {
        TopicFeatureStatus::Pending => "Pending",
        TopicFeatureStatus::Running => "Running",
        TopicFeatureStatus::Ready => "Ready",
        TopicFeatureStatus::NeedsUser => "Needs you",
        TopicFeatureStatus::Failed => "Failed",
        TopicFeatureStatus::Cancelled => "Cancelled",
    }
}

pub fn harness_label(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Claude => "Claude",
        HarnessKind::Codex => "Codex",
        HarnessKind::Pi => "Pi",
        HarnessKind::OpenCode => "OpenCode",
        HarnessKind::Antigravity => "Antigravity",
        HarnessKind::Acp => "ACP",
        HarnessKind::Muse => "Muse",
    }
}

/// `ACP (agent)` for ACP harnesses, the plain name otherwise.
pub fn harness_name(harness: HarnessKind, acp_agent: Option<&str>) -> String {
    match acp_agent {
        Some(agent) if harness == HarnessKind::Acp => format!("ACP ({agent})"),
        _ => harness_label(harness).to_string(),
    }
}

fn step_harness_label(harness: Option<HarnessKind>, acp_agent: Option<&str>) -> String {
    harness.map_or_else(
        || "Formless picks".to_string(),
        |harness| harness_name(harness, acp_agent),
    )
}

fn access_label(access: StepAccess) -> &'static str {
    match access {
        StepAccess::ReadOnly => "read-only",
        StepAccess::Write => "can write",
    }
}

fn attempt_label(attempt: &TopicAttemptInfo) -> String {
    format!(
        "{} · {}",
        attempt.step_title,
        harness_name(attempt.harness, attempt.acp_agent.as_deref())
    )
}

pub fn harness_availability_line(harness: &formless_protocol::HarnessAvailability) -> String {
    let name = harness_name(harness.kind, harness.acp_agent.as_deref());
    let state = match (harness.installed, harness.signed_in) {
        (false, _) => "not installed",
        (true, Some(false)) => "signed out",
        (true, Some(true)) => "ready",
        (true, None) => "installed (sign-in unknown)",
    };
    let mut line = format!("{name}: {state}");
    if let Some(version) = &harness.version {
        line.push_str(&format!(" · {version}"));
    }
    if let Some(detail) = &harness.detail {
        line.push_str(&format!(" · {detail}"));
    }
    line
}

fn model_suffix(model: Option<&str>) -> String {
    model.map(|model| format!(" · {model}")).unwrap_or_default()
}

fn artifact_label(artifact: &TopicArtifact) -> String {
    match artifact {
        TopicArtifact::GitRevision { revision } => format!("revision {revision}"),
        TopicArtifact::File { path, sha256 } => {
            format!(
                "file {} ({})",
                path.display(),
                &sha256[..sha256.len().min(12)]
            )
        }
        TopicArtifact::Text { title, content } => {
            format!("{title}: {}", content.replace('\n', " "))
        }
        TopicArtifact::Directory { path } => format!("folder copy {path}"),
    }
}

fn outline_lines(outline: &[TopicStepOutline], indent: &str) -> Vec<Line<'static>> {
    if outline.is_empty() {
        return vec![Line::from(format!(
            "{indent}No outline: the Formless model plans the steps."
        ))];
    }
    let mut lines = Vec::new();
    for (index, step) in outline.iter().enumerate() {
        lines.push(Line::from(format!(
            "{indent}{}. {} · {}{} · {}",
            index + 1,
            step.title,
            step_harness_label(step.harness, step.acp_agent.as_deref()),
            model_suffix(step.model.as_deref()),
            access_label(step.access)
        )));
        if !step.instructions.is_empty() {
            lines.push(Line::from(format!("{indent}   {}", step.instructions)));
        }
    }
    lines
}

fn status_color(status: &str) -> Color {
    match status {
        "Needs you" | "Needs approval" | "Failed" => Color::Red,
        "Running" => Color::Yellow,
        "Ready" | "Ready to start" | "Complete" | "Done" => Color::Green,
        _ => Color::Gray,
    }
}

fn topic_lines(topic: &TopicInfo, color: bool) -> Vec<Line<'static>> {
    let workspace = match (&topic.workspace, topic.workspace_kind) {
        (Some(path), WorkspaceKind::Git) => format!("{} (git)", path.display()),
        (Some(path), _) => path.display().to_string(),
        (None, _) => "Scratch folder".into(),
    };
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
        Line::from(format!("Workspace: {workspace}")),
    ];
    if let Some(revision) = &topic.base_revision {
        lines.push(Line::from(format!("Base revision: {revision}")));
    }
    lines.push(Line::from(if topic.allow_external_context {
        "Context: harnesses may read conversations outside this topic"
    } else {
        "Context: harnesses read this topic branch only (/topic context ID on widens it)"
    }));
    lines.push(Line::from(format!(
        "Limits: {} retries per step · {} attempts per topic",
        topic.limits.max_retries_per_step, topic.limits.max_attempts_per_topic
    )));
    if !topic.permission_rules.is_empty() {
        lines.push(Line::from("Permission rules (yours only):"));
        for rule in &topic.permission_rules {
            let decision = match rule.decision {
                TopicRuleDecision::AllowOnce => "allow",
                TopicRuleDecision::Reject => "deny",
            };
            let harness = rule
                .harness
                .map(|harness| harness_name(harness, rule.acp_agent.as_deref()))
                .unwrap_or_else(|| "any harness".into());
            lines.push(Line::from(format!(
                "  {} · {decision} · {harness} · {} · `{}`",
                rule.rule_id,
                rule.kind.as_deref().unwrap_or("any kind"),
                rule.pattern
            )));
        }
    }
    lines.push(Line::from("Outline (advice; Formless decides the steps):"));
    lines.extend(outline_lines(&topic.outline, "  "));
    lines.push(Line::from("Work items:"));
    if topic.features.is_empty() {
        lines.push(Line::from("  None."));
    }
    for feature in &topic.features {
        lines.push(Line::from(format!(
            "  #{} · {} · {} · {} attempt(s)",
            feature.source_seq,
            feature.id,
            feature_status_label(feature.status),
            feature.attempt_count
        )));
        lines.push(Line::from(format!("  Request: {}", feature.request)));
        for output in &feature.outputs {
            lines.push(Line::from(format!("  Output: {}", artifact_label(output))));
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
                "    {}{} · {} · {}",
                attempt_label(attempt),
                model_suffix(attempt.model.as_deref()),
                access_label(attempt.access),
                attempt_status_label(attempt),
            )));
            if let Some(summary) = &attempt.summary {
                lines.push(Line::from(format!("    Summary: {summary}")));
            }
            for output in &attempt.outputs {
                lines.push(Line::from(format!(
                    "    Output: {}",
                    artifact_label(output)
                )));
            }
            if let Some(detail) = &attempt.detail {
                lines.push(Line::from(format!("    Details: {detail}")));
            }
        }
    }
    lines.extend([
        Line::default(),
        Line::from("Nothing is merged or pushed automatically."),
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
            parent_topic_id: None,
            name: format!("Topic {id}"),
            workspace: Some(PathBuf::from("/repo")),
            workspace_kind: formless_protocol::WorkspaceKind::Git,
            base_revision: Some("base".into()),
            outline: vec![],
            source_session_id: "main".into(),
            topic_session_id: Some(format!("topic-session-{id}")),
            status,
            features: vec![],
            attempts: vec![],
            created_at: String::new(),
            updated_at: String::new(),
            allow_external_context: false,
            permission_rules: vec![],
            limits: Default::default(),
        }
    }

    fn attempt(id: &str, session_id: &str) -> TopicAttemptInfo {
        TopicAttemptInfo {
            id: id.into(),
            topic_id: "t1".into(),
            feature_id: "f1".into(),
            step_title: "Implement".into(),
            access: StepAccess::Write,
            model: None,
            inputs: vec![],
            outputs: vec![],
            summary: None,
            harness: HarnessKind::Claude,
            acp_agent: None,
            status: TopicAttemptStatus::Running,
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
            generated_by: None,
        }
    }

    fn permission(id: &str, raw_input: serde_json::Value) -> HarnessPermission {
        HarnessPermission {
            permission_id: id.into(),
            session_id: "harness-session".into(),
            topic_id: "t1".into(),
            attempt_id: "a1".into(),
            harness: HarnessKind::Claude,
            acp_agent: None,
            title: "Run a command".into(),
            kind: "execute".into(),
            raw_input: Some(raw_input),
            options: vec![HarnessPermissionOption {
                option_id: "allow".into(),
                name: "Allow once".into(),
                kind: HarnessPermissionOptionKind::AllowOnce,
            }],
        }
    }

    fn permission_screen(state: &mut State, width: u16, height: u16) -> String {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
            .unwrap();
        terminal
            .draw(|frame| state.draw_permission_overlay(frame, false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn proposal_defaults_latest_request_and_contains_only_explicit_selections() {
        let source = session("main");
        let messages = [
            message("u1", 1, "user", "First request"),
            message("a1", 2, "assistant", "Answer"),
            message("u2", 3, "user", "Second request"),
        ];
        let parents = vec![(None, "Main Chat".to_string())];
        let form = ProposalForm::new(&source, &messages, "/repo", "Work", parents, None);
        let request = form.request().unwrap();
        assert_eq!(request.message_ids, ["u2"]);
        assert_eq!(request.source_session_id, "main");
        assert_eq!(request.workspace, Some(PathBuf::from("/repo")));
        assert!(request.outline.is_empty(), "Formless plans by default");
        assert_eq!(request.parent_topic_id, None);
    }

    #[test]
    fn permission_input_scrolls_past_long_wrapped_input_with_choices_pinned() {
        let mut fields = serde_json::Map::new();
        fields.insert("aa_long".into(), serde_json::json!("x".repeat(100)));
        for index in 0..18 {
            fields.insert(format!("field_{index:02}"), serde_json::json!(index));
        }
        fields.insert("zz_tail".into(), serde_json::json!("tail-beyond-twelve"));
        let mut state = State::default();
        state.push_permission(permission("p1", serde_json::Value::Object(fields)));

        let first = permission_screen(&mut state, 60, 18);
        assert!(first.contains("Input rows"), "{first}");
        assert!(first.contains("1. Allow once"), "{first}");
        assert!(!first.contains("tail-beyond-twelve"), "{first}");

        assert_eq!(
            state.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
            Action::None
        );
        let last = permission_screen(&mut state, 60, 18);
        assert!(last.contains("tail-beyond-twelve"), "{last}");
        assert!(last.contains("1. Allow once"), "{last}");
    }

    #[test]
    fn tiny_permission_view_does_not_accept_invisible_allow_options() {
        let mut state = State::default();
        state.push_permission(permission("p1", serde_json::json!({"command": "run"})));
        let screen = permission_screen(&mut state, 30, 8);
        assert!(screen.contains("Terminal too small"), "{screen}");
        assert!(!screen.contains("1. Allow once"), "{screen}");
        assert_eq!(
            state.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)),
            Action::None
        );
        assert_eq!(
            state.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::PermissionDecision {
                permission_id: "p1".into(),
                option_id: None,
            }
        );
    }

    #[test]
    fn presets_fill_any_outline_and_blank_workspace_means_scratch() {
        let source = session("main");
        let messages = [message("u1", 1, "user", "Do my books")];
        let parents = vec![(None, "Main Chat".to_string())];
        let mut form = ProposalForm::new(&source, &messages, "", "Books", parents, None);
        form.step = ProposalStep::Fields;
        form.field_index = 3;
        form.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        form.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        assert_eq!(form.preset, Preset::Accounting);
        form.field_index = HEAD_FIELDS + 2;
        form.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        form.key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        for c in "Email".chars() {
            form.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let request = form.request().unwrap();
        assert_eq!(request.workspace, None);
        let titles: Vec<_> = request
            .outline
            .iter()
            .map(|step| step.title.as_str())
            .collect();
        assert_eq!(
            titles,
            ["Scan receipts", "Email", "Do accounting", "Create reports"]
        );
        assert_eq!(request.outline[0].harness, Some(HarnessKind::Claude));
        assert_eq!(request.outline[1].harness, None);
    }

    #[test]
    fn topics_nest_to_any_depth_under_main_chat() {
        let mut state = State::default();
        let mut story = topic("story", TopicStatus::Running);
        story.parent_topic_id = Some("project".into());
        story.attempts.push(attempt("a1", "harness-1"));
        story.attempts[0].topic_id = "story".into();
        let mut orphan = topic("orphan", TopicStatus::Approved);
        orphan.parent_topic_id = Some("deleted".into());
        state.update_topics(vec![story, topic("project", TopicStatus::Running), orphan]);
        let rows: Vec<_> = state
            .rows()
            .into_iter()
            .filter(|row| !row.section)
            .map(|row| (row.depth, row.label))
            .collect();
        assert_eq!(
            rows,
            [
                (0, "Main Chat".to_string()),
                (0, "Topic project".to_string()),
                (1, "Topic story".to_string()),
                (2, "Implement · Claude".to_string()),
                (0, "Topic orphan".to_string()),
            ]
        );
        assert_eq!(
            state.breadcrumb(Some("harness-1")),
            "Main Chat / Topic project / Topic story / Implement · Claude · read-only"
        );
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
            .any(|row| row.label.contains("Implement · Claude")));
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
    fn topic_details_show_user_controls() {
        let mut info = topic("t1", TopicStatus::Running);
        info.limits.max_retries_per_step = 0;
        info.permission_rules
            .push(formless_protocol::TopicPermissionRule {
                rule_id: "r1".into(),
                harness: Some(HarnessKind::Claude),
                acp_agent: None,
                kind: None,
                pattern: "cargo *".into(),
                decision: TopicRuleDecision::AllowOnce,
            });
        let text: String = topic_lines(&info, false)
            .iter()
            .map(|line| line.to_string() + "\n")
            .collect();
        assert!(text.contains("this topic branch only"), "{text}");
        assert!(text.contains("0 retries per step · 20 attempts"), "{text}");
        assert!(
            text.contains("r1 · allow · Claude · any kind · `cargo *`"),
            "{text}"
        );
        assert_eq!(
            artifact_label(&TopicArtifact::Directory {
                path: "/tmp/copy".into()
            }),
            "folder copy /tmp/copy"
        );
    }

    #[test]
    fn proposals_from_a_topic_chat_cannot_target_main_chat() {
        let mut state = State::default();
        state.update_topics(vec![topic("t1", TopicStatus::Approved)]);
        let messages = vec![message("m1", 1, "user", "add X")];
        state.open_proposal(&session("main"), &messages, "", "Story", Some("t1"));
        let proposal = state.proposal.as_ref().unwrap();
        assert!(proposal.parents.iter().all(|(id, _)| id.is_some()));
        assert_eq!(
            proposal.request().unwrap().parent_topic_id.as_deref(),
            Some("t1")
        );
    }

    #[test]
    fn paste_edits_the_focused_proposal_field_only() {
        let mut state = State::default();
        let messages = vec![message("m1", 1, "user", "add X")];
        state.open_proposal(&session("main"), &messages, "/repo", "", None);
        state.paste("ignored on the message step");
        state.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        state.paste("formless\n");
        let proposal = state.proposal.as_ref().unwrap();
        assert_eq!(proposal.name.expanded(), "formless ");
    }

    #[test]
    fn starting_a_running_topic_explains_why_it_cannot() {
        let mut state = State::default();
        state.update_topics(vec![topic("t1", TopicStatus::Running)]);
        let error = state.open_topic_command("start t1").unwrap_err();
        assert!(!error.contains("No topic named"), "{error}");
        assert!(state
            .open_topic_command("start nope")
            .unwrap_err()
            .contains("No topic named"));
    }

    #[test]
    fn cancel_is_offered_only_while_a_topic_can_still_run() {
        let mut state = State::default();
        state.update_topics(vec![
            topic("p", TopicStatus::PendingApproval),
            topic("r", TopicStatus::Running),
        ]);
        assert!(state.open_topic_command("cancel p").is_err());
        assert!(state.open_topic_command("cancel r").is_ok());
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
            topic_status_label(&topic("done", TopicStatus::Complete)),
            "Complete"
        );
        assert_eq!(
            topic_status_label(&topic("asks", TopicStatus::NeedsUser)),
            "Needs you"
        );
        assert_eq!(
            topic_status_label(&topic("stopped", TopicStatus::Cancelled)),
            "Cancelled"
        );
        assert_eq!(
            topic_status_label(&topic("failed", TopicStatus::Failed)),
            "Failed"
        );
    }
}
