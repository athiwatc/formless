//! Profile-wide pending questions. Identity never follows the selected Session.
//! Snapshot hydration is refused after an observed question/close event; closed
//! identities remain tombstoned for the connection. This is not a replay cursor.

use std::collections::{BTreeMap, HashSet};

use formless_protocol::{Event, Question, QuestionAnswer};
use serde::Deserialize as _;

pub const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_QUESTIONS: usize = 64;
const MAX_CLOSED: usize = 512;
const MAX_DEFINITION_BYTES: usize = 128 * 1024;

fn fields_size(questions: &[Question]) -> usize {
    questions
        .iter()
        .map(|q| {
            q.question.len()
                + q.header.as_ref().map_or(0, String::len)
                + q.options
                    .iter()
                    .map(|o| o.label.len() + o.description.as_ref().map_or(0, String::len))
                    .sum::<usize>()
        })
        .sum()
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Open {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<Permission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    pub id: String,
    pub session_id: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Permission {
    pub topic_id: String,
    pub attempt_id: String,
    pub harness: formless_protocol::HarnessKind,
    pub acp_agent: Option<String>,
    pub kind: String,
    pub raw_input: Option<serde_json::Value>,
    pub options: Vec<formless_protocol::HarnessPermissionOption>,
}

impl Open {
    pub fn from_event(event: &Event) -> Result<Self, &'static str> {
        if let Event::HarnessPermissionRequested {
            permission_id,
            session_id,
            topic_id,
            attempt_id,
            harness,
            acp_agent,
            title,
            kind,
            raw_input,
            options,
        } = event
        {
            if permission_id.len() > 256
                || session_id.len() > 256
                || topic_id.is_empty()
                || attempt_id.is_empty()
                || topic_id.len() > 256
                || attempt_id.len() > 256
                || options.len() > 64
                || serde_json::to_vec(event)
                    .map_err(|_| "Permission could not be read; no permission granted.")?
                    .len()
                    > MAX_DEFINITION_BYTES
            {
                return Err("Permission exceeds native review bounds. No permission granted; cancel the workflow if it cannot be reviewed.");
            }
            let mut ids = HashSet::new();
            if options.iter().any(|option| {
                option.option_id.is_empty()
                    || option.option_id.len() > 256
                    || !ids.insert(&option.option_id)
            }) {
                return Err("Permission options are ambiguous. No permission granted.");
            }
            let open = Self {
                id: permission_id.clone(),
                session_id: session_id.clone(),
                topic_id: Some(topic_id.clone()),
                attempt_id: Some(attempt_id.clone()),
                permission: Some(Permission {
                    topic_id: topic_id.clone(),
                    attempt_id: attempt_id.clone(),
                    harness: *harness,
                    acp_agent: acp_agent.clone(),
                    kind: kind.clone(),
                    raw_input: raw_input.clone(),
                    options: options.clone(),
                }),
                questions: vec![Question {
                    question: title.clone(),
                    header: None,
                    multi_select: false,
                    options: options
                        .iter()
                        .map(|option| formless_protocol::QuestionOption {
                            label: option.option_id.clone(),
                            description: Some(option.name.clone()),
                        })
                        .collect(),
                }],
            };
            open.validate()?;
            return Ok(open);
        }
        let Event::UserQuestion {
            question_id,
            session_id,
            topic_id,
            attempt_id,
            question,
            options,
            questions,
            ..
        } = event
        else {
            return Err("Not a user question");
        };
        let bytes = question_id.len()
            + session_id.len()
            + if questions.is_empty() {
                question.len() + options.iter().map(String::len).sum::<usize>()
            } else {
                fields_size(questions)
            };
        if question_id.len() > 256
            || session_id.len() > 256
            || bytes > MAX_DEFINITION_BYTES
            || questions.len() > 4
            || (questions.is_empty() && options.len() > 64)
            || questions.iter().any(|q| q.options.len() > 64)
        {
            return Err("This question is too large for the native form; use the TUI.");
        }
        let questions = if questions.is_empty() {
            vec![Question::single(question.clone(), options.clone())]
        } else {
            questions.clone()
        };
        let open = Self {
            permission: None,
            topic_id: topic_id.clone(),
            attempt_id: attempt_id.clone(),
            id: question_id.clone(),
            session_id: session_id.clone(),
            questions,
        };
        open.validate()?;
        Ok(open)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.is_empty()
            || self.session_id.is_empty()
            || self.id.len() > 256
            || self.session_id.len() > 256
            || [&self.topic_id, &self.attempt_id].iter().any(|id| {
                id.as_ref()
                    .is_some_and(|id| id.is_empty() || id.len() > 256)
            })
            || self.questions.is_empty()
            || self.questions.len() > 4
        {
            return Err("Question identity or field count is unsupported; use the TUI.");
        }
        if self.id.len() + self.session_id.len() + fields_size(&self.questions)
            > MAX_DEFINITION_BYTES
            || self.questions.iter().any(|q| q.options.len() > 64)
        {
            return Err("This question is too large for the native form; use the TUI.");
        }
        Ok(())
    }

    pub fn validate_answers(&self, answers: &[QuestionAnswer]) -> Result<(), &'static str> {
        if self.permission.is_some() && answers.iter().any(|answer| answer.text.is_some()) {
            return Err("Choose an offered permission option or deny the request; free text cannot grant permission.");
        }
        if answers.len() != self.questions.len() {
            return Err("Answer every field in its original order.");
        }
        for (question, answer) in self.questions.iter().zip(answers) {
            let mut seen = HashSet::new();
            if (!question.multi_select && answer.selected.len() > 1)
                || answer
                    .text
                    .as_ref()
                    .is_some_and(|text| text.len() > MAX_TEXT_BYTES)
                || answer.selected.iter().any(|selected| {
                    !seen.insert(selected)
                        || !question
                            .options
                            .iter()
                            .any(|option| &option.label == selected)
                })
            {
                return Err("The answer does not match this question or exceeds 64 KiB of text.");
            }
        }
        Ok(())
    }

    pub fn request(&self, answers: Vec<QuestionAnswer>) -> formless_protocol::Request {
        if self.permission.is_some() {
            formless_protocol::Request::HarnessPermissionDecision {
                permission_id: self.id.clone(),
                option_id: answers
                    .first()
                    .and_then(|answer| answer.selected.first())
                    .cloned(),
            }
        } else {
            formless_protocol::Request::AnswerQuestion {
                question_id: self.id.clone(),
                answer: QuestionAnswer::summary_of(&answers),
                answers,
            }
        }
    }

    pub fn option_label<'a>(&'a self, field: usize, index: usize) -> &'a str {
        self.permission
            .as_ref()
            .map(|permission| permission.options[index].name.as_str())
            .unwrap_or(&self.questions[field].options[index].label)
    }

    pub fn option_description(&self, field: usize, index: usize) -> Option<String> {
        self.permission
            .as_ref()
            .map(|permission| {
                match permission.options[index].kind {
                    formless_protocol::HarnessPermissionOptionKind::AllowOnce => "Allow once",
                    formless_protocol::HarnessPermissionOptionKind::AllowAlways => {
                        "Allow for this attempt only; never saved"
                    }
                    formless_protocol::HarnessPermissionOptionKind::RejectOnce => "Reject once",
                    formless_protocol::HarnessPermissionOptionKind::RejectAlways => {
                        "Reject for this attempt"
                    }
                }
                .to_string()
            })
            .or_else(|| self.questions[field].options[index].description.clone())
    }

    pub fn toggle(&self, answers: &mut [QuestionAnswer], field: usize, option: usize) {
        let (Some(question), Some(answer)) = (self.questions.get(field), answers.get_mut(field))
        else {
            return;
        };
        let Some(option) = question.options.get(option) else {
            return;
        };
        if answer.selected.contains(&option.label) {
            answer.selected.retain(|label| label != &option.label);
        } else {
            if !question.multi_select {
                answer.selected.clear();
            }
            answer.selected.push(option.label.clone());
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub connection: u64,
    pub open: Vec<Open>,
    pub blocked: HashSet<String>,
    pub inflight: HashSet<String>,
    pub notice: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Directory {
    revision: u64,
    open: BTreeMap<String, Open>,
    closed: HashSet<String>,
    notice: Option<String>,
    disabled: bool,
}

impl Directory {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    fn changed(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("question revision exhausted");
    }
    pub fn get(&self, id: &str) -> Option<&Open> {
        self.open.get(id)
    }
    pub fn state(&self, connection: u64, blocked: HashSet<String>) -> State {
        State {
            connection,
            open: self.open.values().cloned().collect(),
            blocked,
            inflight: HashSet::new(),
            notice: self.notice.clone(),
        }
    }
    pub fn event(&mut self, event: &Event) {
        self.changed();
        match event {
            Event::UserQuestionClosed { question_id } => self.close(question_id),
            Event::HarnessPermissionClosed { permission_id, .. } => self.close(permission_id),
            Event::UserQuestion { question_id, .. }
            | Event::HarnessPermissionRequested {
                permission_id: question_id,
                ..
            } if !self.disabled && !self.closed.contains(question_id) => {
                match Open::from_event(event) {
                    Ok(open) if self.open.get(&open.id).is_some_and(|old| old != &open) => {
                        self.close(&open.id);
                        self.notice =
                            Some("A question identity changed; continue in the TUI.".into());
                    }
                    Ok(open)
                        if self.open.len() < MAX_QUESTIONS || self.open.contains_key(&open.id) =>
                    {
                        self.open.insert(open.id.clone(), open);
                    }
                    Ok(_) => {
                        self.notice =
                            Some("Too many pending questions; continue in the TUI.".into());
                    }
                    Err(error) => {
                        self.close(question_id);
                        self.notice = Some(error.into());
                    }
                }
            }
            _ => {}
        }
    }
    pub fn close(&mut self, id: &str) {
        self.changed();
        if id.len() > 256 {
            self.disabled = true;
            self.open.clear();
            self.notice = Some("A question identity cannot be tracked safely; use the TUI.".into());
            return;
        }
        self.open.remove(id);
        if self.closed.len() < MAX_CLOSED || self.closed.contains(id) {
            self.closed.insert(id.into());
        }
        if self.closed.len() >= MAX_CLOSED {
            self.open.clear();
            self.disabled = true;
            self.notice = Some("Question history limit reached; reconnect or use the TUI.".into());
        }
    }
    pub fn snapshot(&mut self, events: &[serde_json::Value], revision: u64) -> bool {
        self.restore(events, revision, false)
    }
    pub fn permission_snapshot(&mut self, events: &[serde_json::Value], revision: u64) -> bool {
        self.restore(events, revision, true)
    }
    fn restore(
        &mut self,
        events: &[serde_json::Value],
        revision: u64,
        permissions_only: bool,
    ) -> bool {
        if revision != self.revision {
            return false;
        }
        // Never evict tombstones and permit an old snapshot to revive them.
        if self.disabled || self.closed.len() >= MAX_CLOSED {
            return false;
        }
        let mut replacement = Self {
            revision: self.revision,
            closed: self.closed.clone(),
            ..Self::default()
        };
        if permissions_only {
            replacement.open.extend(
                self.open
                    .iter()
                    .filter(|(_, open)| open.permission.is_none())
                    .map(|(id, open)| (id.clone(), open.clone())),
            );
        }
        for value in events.iter().filter(|value| {
            value["type"] == "harness_permission_requested"
                || (!permissions_only && value["type"] == "user_question")
        }) {
            match Event::deserialize(value) {
                Ok(event) => replacement.event(&event),
                Err(_) => {
                    replacement.notice =
                        Some("A pending question could not be read; use the TUI.".into());
                }
            }
        }
        replacement.revision = self.revision;
        for (id, old) in &self.open {
            if replacement.open.get(id).is_some_and(|new| new != old) {
                replacement.close(id);
                replacement.notice =
                    Some("A question identity changed; continue in the TUI.".into());
            }
        }
        // GetSession and PendingApprovals can be in flight together. A canonical
        // read that changes the directory also retires older reads, not just events.
        if replacement.open != self.open {
            replacement.changed();
        }
        *self = replacement;
        true
    }
}

#[derive(Debug)]
pub enum Outcome {
    Accepted,
    Closed,
    Rejected(String),
    Unknown,
}

pub fn preview(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let text: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{text}… (copy prompt for full text)")
    } else {
        text
    }
}

/// Keep harness-supplied decision text complete without invisible direction or control effects.
pub fn literal(text: &str) -> String {
    text.chars().map(|c| if (c.is_control() && c != '\n' && c != '\t') || matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') { c.escape_unicode().to_string() } else { c.to_string() }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Event {
        Event::UserQuestion {
            topic_id: None,
            attempt_id: None,
            question_id: "q".into(),
            session_id: "other-session".into(),
            question: "Legacy ไทย?".into(),
            options: vec!["yes".into(), "no".into()],
            questions: vec![],
        }
    }
    #[test]
    fn legacy_identity_and_exact_option_answers_are_preserved() {
        let open = Open::from_event(&event()).unwrap();
        assert_eq!(open.session_id, "other-session");
        let mut answers = vec![QuestionAnswer::default()];
        open.toggle(&mut answers, 0, 0);
        open.toggle(&mut answers, 0, 1);
        assert_eq!(answers[0].selected, ["no"]);
        assert!(open.validate_answers(&answers).is_ok());
        answers[0].selected.push("foreign option".into());
        assert!(open.validate_answers(&answers).is_err());
    }
    #[test]
    fn permission_uses_exact_option_ids_even_when_harness_names_collide_and_never_treats_text_as_authority(
    ) {
        let mut event = crate::fixture::harness_permission("codex-chat");
        if let Event::HarnessPermissionRequested { options, .. } = &mut event {
            options[1].name = options[0].name.clone();
        }
        let open = Open::from_event(&event).unwrap();
        let mut answers = vec![QuestionAnswer::default()];
        assert!(matches!(
            open.request(answers.clone()),
            formless_protocol::Request::HarnessPermissionDecision {
                option_id: None,
                ..
            }
        ));
        open.toggle(&mut answers, 0, 1);
        assert!(
            matches!(open.request(answers.clone()), formless_protocol::Request::HarnessPermissionDecision { permission_id, option_id: Some(option) } if permission_id == "native-permission" && option == "allow-attempt-id")
        );
        answers[0].text = Some("allow everything".into());
        assert!(open.validate_answers(&answers).is_err());
        answers[0].text = None;
        answers[0].selected = vec!["invented-option".into()];
        assert!(open.validate_answers(&answers).is_err());
    }
    #[test]
    fn permission_snapshot_preserves_questions_and_closed_permissions_cannot_be_resurrected() {
        let mut directory = Directory::default();
        directory.event(&event());
        let permission = crate::fixture::harness_permission("codex-chat");
        let json = serde_json::to_value(&permission).unwrap();
        assert!(directory.permission_snapshot(std::slice::from_ref(&json), directory.revision()));
        assert!(directory.get("q").is_some());
        assert!(directory.get("native-permission").is_some());
        let revision = directory.revision();
        directory.event(&Event::HarnessPermissionClosed {
            permission_id: "native-permission".into(),
            option_id: None,
            by: None,
        });
        assert!(!directory.permission_snapshot(std::slice::from_ref(&json), revision));
        assert!(directory.permission_snapshot(&[json], directory.revision()));
        assert!(directory.get("native-permission").is_none());
        assert!(directory.get("q").is_some());
    }
    #[test]
    fn a_late_session_snapshot_cannot_erase_replayed_permissions() {
        let mut directory = Directory::default();
        let old = directory.revision();
        let permission =
            serde_json::to_value(crate::fixture::harness_permission("codex-chat")).unwrap();
        assert!(directory.permission_snapshot(&[permission], old));
        assert!(!directory.snapshot(&[], old));
        assert!(directory.get("native-permission").is_some());
        assert!(directory.snapshot(&[], directory.revision()));
        assert!(directory.get("native-permission").is_none());
    }
    #[test]
    fn closed_or_changed_questions_cannot_be_revived_by_late_reads_or_events() {
        let mut directory = Directory::default();
        let revision = directory.revision();
        let question = event();
        let json = serde_json::to_value(&question).unwrap();
        directory.event(&question);
        directory.event(&Event::UserQuestionClosed {
            question_id: "q".into(),
        });
        assert!(!directory.snapshot(std::slice::from_ref(&json), revision));
        assert!(directory.snapshot(&[json], directory.revision()));
        directory.event(&question);
        assert!(directory.get("q").is_none());
        let mut directory = Directory::default();
        directory.event(&question);
        let mut changed = question;
        if let Event::UserQuestion { session_id, .. } = &mut changed {
            *session_id = "wrong-session".into();
        }
        directory.event(&changed);
        assert!(directory.get("q").is_none());
    }

    #[test]
    fn closed_history_is_bounded_and_exhaustion_cannot_revive_old_identities() {
        let mut directory = Directory::default();
        for index in 0..MAX_CLOSED + 20 {
            directory.event(&Event::UserQuestionClosed {
                question_id: format!("closed-{index}"),
            });
        }
        assert_eq!(directory.closed.len(), MAX_CLOSED);
        directory.event(&event());
        assert!(directory.get("q").is_none());
        assert!(directory.notice.is_some());
        assert!(!directory.snapshot(
            &[serde_json::to_value(event()).unwrap()],
            directory.revision()
        ));
    }

    #[test]
    fn an_unsupported_replacement_retires_the_old_definition() {
        let mut directory = Directory::default();
        let mut changed = event();
        directory.event(&changed);
        if let Event::UserQuestion { question, .. } = &mut changed {
            *question = "x".repeat(MAX_DEFINITION_BYTES + 1);
        }
        directory.event(&changed);
        assert!(directory.get("q").is_none());
        directory.event(&event());
        assert!(directory.get("q").is_none());
        directory.close(&"q".repeat(1000));
        assert!(directory.disabled);
        assert!(directory.closed.iter().all(|id| id.len() <= 256));
    }
}
