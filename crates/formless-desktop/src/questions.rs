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
    pub id: String,
    pub session_id: String,
    pub questions: Vec<Question>,
}

impl Open {
    pub fn from_event(event: &Event) -> Result<Self, &'static str> {
        let Event::UserQuestion {
            question_id,
            session_id,
            question,
            options,
            questions,
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
            Event::UserQuestion { question_id, .. }
                if !self.disabled && !self.closed.contains(question_id) =>
            {
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
        for value in events
            .iter()
            .filter(|value| value["type"] == "user_question")
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Event {
        Event::UserQuestion {
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
