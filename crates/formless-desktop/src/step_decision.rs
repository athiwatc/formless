//! Explicit complete-prompt Step decisions. Shared receipts survive UI queue loss.
use crate::steps::Inventory;
use formless_protocol::{PendingStep, PendingSteps, Request, STEP_PREVIEW_BYTES};
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Continue,
    Skip,
    Edit(String),
    All,
}
impl Choice {
    pub fn wire(&self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Skip => "skip",
            Self::Edit(_) => "edit",
            Self::All => "all",
        }
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if let Self::Edit(text) = self {
            if text.len() > STEP_PREVIEW_BYTES {
                return Err("Edited arguments exceed the native 4 KiB limit; use Formless TUI.");
            }
            if !serde_json::from_str::<Value>(text).is_ok_and(|v| v.is_object()) {
                return Err("Edited arguments must be a JSON object; nothing was sent.");
            }
        }
        Ok(())
    }
}
pub fn complete(prompt: &PendingStep) -> bool {
    if [
        prompt.session_id.len(),
        prompt.run_id.len(),
        prompt.step_id.len(),
        prompt.tool_name.len(),
    ]
    .into_iter()
    .any(|n| n > 512)
    {
        return false;
    }
    if prompt.arguments_bytes != prompt.arguments_preview.len()
        || prompt.arguments_bytes > STEP_PREVIEW_BYTES
    {
        return false;
    }
    let Ok(value) = serde_json::to_value(PendingSteps {
        prompts: vec![prompt.clone()],
        more: false,
    }) else {
        return false;
    };
    matches!(
        Inventory::decode(&prompt.session_id, Some(&value)),
        Inventory::Snapshot(_)
    )
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub client: uuid::Uuid,
    pub connection: u64,
    pub generation: u64,
    pub epoch: u64,
    pub operation: uuid::Uuid,
    pub prompt: PendingStep,
    pub choice: Choice,
}
impl Action {
    pub fn valid(&self) -> bool {
        complete(&self.prompt) && self.choice.validate().is_ok()
    }
    pub fn request(&self) -> Request {
        Request::StepDecision {
            step_id: self.prompt.step_id.clone(),
            action: self.choice.wire().into(),
            arguments: if let Choice::Edit(text) = &self.choice {
                Some(text.clone())
            } else {
                None
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted,
    Rejected(String),
    Unknown,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Reserved(Action),
    Sent(Action),
    Finished {
        action: Action,
        outcome: Outcome,
    },
}
impl Status {
    pub fn notice(&self) -> Option<&'static str> {
        match self {
            Self::Reserved(_)=>Some("A native Step decision is being revalidated; it has not been written yet."),
            Self::Sent(_)=>Some("A native Step decision may have been written and is awaiting its receipt. No retry will be sent."),
            Self::Finished{outcome:Outcome::Unknown,..}=>Some("A native Step decision outcome is unknown. It may already have applied; native retries are blocked for this client. Inspect Formless TUI."),
            _=>None,
        }
    }
    pub fn blocked(&self) -> bool {
        matches!(
            self,
            Self::Reserved(_)
                | Self::Sent(_)
                | Self::Finished {
                    outcome: Outcome::Unknown,
                    ..
                }
        )
    }
}
#[derive(Debug, Default)]
pub struct Ledger(Mutex<Status>);
impl Ledger {
    pub fn status(&self) -> Status {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn cancel_unsent(&self, reason: &str) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Status::Reserved(action) = &*state {
            *state = Status::Finished {
                action: action.clone(),
                outcome: Outcome::Rejected(reason.chars().take(500).collect()),
            };
        }
    }
    pub(crate) fn reserve(self: &Arc<Self>, action: Action) -> Result<Dispatch, &'static str> {
        if !action.valid() {
            return Err("A complete, supported Step prompt and valid decision are required.");
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.blocked() {
            return Err("A Step decision is pending or its outcome is unknown. Do not retry; inspect Formless TUI.");
        }
        if matches!(&*state,Status::Finished{action:old,..} if old.operation==action.operation) {
            return Err("This decision operation already finished; review again explicitly.");
        }
        if matches!(&*state,Status::Finished{action:old,outcome:Outcome::Accepted} if old.prompt.step_id==action.prompt.step_id)
        {
            return Err("This Step decision was already accepted; no retry was queued.");
        }
        *state = Status::Reserved(action.clone());
        Ok(Dispatch(Arc::new(Owner {
            ledger: self.clone(),
            action,
        })))
    }
}
#[derive(Debug)]
struct Owner {
    ledger: Arc<Ledger>,
    action: Action,
}
impl Drop for Owner {
    fn drop(&mut self) {
        let mut state = self.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = match &*state {
            Status::Reserved(a) if a == &self.action => Some(Outcome::Rejected(
                "Decision was not written; review again explicitly.".into(),
            )),
            Status::Sent(a) if a == &self.action => Some(Outcome::Unknown),
            _ => None,
        };
        if let Some(outcome) = outcome {
            *state = Status::Finished {
                action: self.action.clone(),
                outcome,
            };
        }
    }
}
/// Last dispatch owner classifies an abandoned queued/read action as unsent,
/// or an abandoned possibly-written action as unknown. The ledger holds no
/// dispatch Arc, so this does not create a reference cycle.
#[derive(Clone, Debug)]
pub struct Dispatch(Arc<Owner>);
impl Dispatch {
    pub fn action(&self) -> &Action {
        &self.0.action
    }
    pub(crate) fn owned_by(&self, ledger: &Arc<Ledger>) -> bool {
        Arc::ptr_eq(&self.0.ledger, ledger)
    }
    pub(crate) fn sent(&self) -> bool {
        self.0.ledger.status() == Status::Sent(self.action().clone())
    }
    pub(crate) fn reserved(&self) -> bool {
        self.0.ledger.status() == Status::Reserved(self.action().clone())
    }
    pub(crate) fn reject(&self, reason: &str) {
        let mut state = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state,Status::Reserved(a) if a==self.action()) {
            *state = Status::Finished {
                action: self.action().clone(),
                outcome: Outcome::Rejected(reason.chars().take(500).collect()),
            };
        }
    }
    pub(crate) fn before_write(&self) -> bool {
        let mut state = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state,Status::Reserved(a)|Status::Sent(a) if a==self.action()) {
            *state = Status::Sent(self.action().clone());
            true
        } else {
            false
        }
    }
    pub(crate) fn finish(&self, outcome: Outcome) {
        let mut state = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state,Status::Sent(a) if a==self.action()) {
            *state = Status::Finished {
                action: self.action().clone(),
                outcome,
            };
        }
    }
}
pub fn decode(action: &Action, result: Result<Value, String>, response_ok: bool) -> Outcome {
    match result {
        Ok(value)
            if response_ok
                && value["step_id"].as_str() == Some(action.prompt.step_id.as_str())
                && value["action"].as_str() == Some(action.choice.wire()) =>
        {
            Outcome::Accepted
        }
        Err(error) if !response_ok => Outcome::Rejected(error.chars().take(500).collect()),
        _ => Outcome::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn action() -> Action {
        Action {
            client: uuid::Uuid::new_v4(),
            connection: 3,
            generation: 2,
            epoch: 7,
            operation: uuid::Uuid::new_v4(),
            prompt: PendingStep {
                session_id: "s".into(),
                run_id: "r".into(),
                step_id: "p".into(),
                tool_name: "tool".into(),
                arguments_preview: "{}".into(),
                arguments_bytes: 2,
                index: 0,
                count: 1,
            },
            choice: Choice::Continue,
        }
    }
    #[test]
    fn native_actions_require_complete_identity_and_bounded_object_edits() {
        let mut a = action();
        assert!(a.valid());
        a.prompt.arguments_bytes = 9000;
        assert!(!a.valid());
        a = action();
        a.prompt.step_id.clear();
        assert!(!a.valid());
        for text in ["[]", "null", "true", "not json"] {
            assert!(Choice::Edit(text.into()).validate().is_err());
        }
        assert!(Choice::Edit(" ".repeat(STEP_PREVIEW_BYTES + 1))
            .validate()
            .is_err());
        assert!(Choice::Edit("{\"ไทย\":1}".into()).validate().is_ok());
    }
    #[test]
    fn last_owner_drop_distinguishes_unsent_read_from_possible_write() {
        let ledger = Arc::new(Ledger::default());
        let a = action();
        let first = ledger.reserve(a.clone()).unwrap();
        let copy = first.clone();
        drop(first);
        assert!(matches!(ledger.status(), Status::Reserved(_)));
        drop(copy);
        assert!(matches!(
            ledger.status(),
            Status::Finished {
                outcome: Outcome::Rejected(_),
                ..
            }
        ));
        let next = ledger
            .reserve(Action {
                operation: uuid::Uuid::new_v4(),
                ..a
            })
            .unwrap();
        assert!(next.before_write());
        drop(next);
        assert!(matches!(
            ledger.status(),
            Status::Finished {
                outcome: Outcome::Unknown,
                ..
            }
        ));
        assert!(ledger.reserve(action()).is_err());
    }
    #[test]
    fn cancelled_old_dispatch_cannot_retire_a_new_review() {
        let ledger = Arc::new(Ledger::default());
        let first = ledger.reserve(action()).unwrap();
        ledger.cancel_unsent("cancelled before write");
        let next = ledger.reserve(action()).unwrap();
        drop(first);
        assert!(next.reserved());
        assert!(next.before_write());
        ledger.cancel_unsent("too late");
        assert!(next.sent());
        next.finish(Outcome::Accepted);
        drop(next);
        assert!(matches!(
            ledger.status(),
            Status::Finished {
                outcome: Outcome::Accepted,
                ..
            }
        ));
    }
    #[test]
    fn receipts_are_exact_and_known_accepted_steps_cannot_be_retried() {
        let ledger = Arc::new(Ledger::default());
        let a = action();
        let d = ledger.reserve(a.clone()).unwrap();
        assert!(ledger.reserve(action()).is_err());
        assert!(d.before_write());
        assert_eq!(
            decode(&a, Ok(json!({"step_id":"p","action":"continue"})), true),
            Outcome::Accepted
        );
        for value in [
            json!({"step_id":"other","action":"continue"}),
            json!({"step_id":"p","action":"skip"}),
            json!({}),
        ] {
            assert_eq!(decode(&a, Ok(value), true), Outcome::Unknown);
        }
        assert_eq!(
            decode(&a, Err("negative ack".into()), true),
            Outcome::Unknown
        );
        assert!(matches!(
            decode(&a, Err("rejected".into()), false),
            Outcome::Rejected(_)
        ));
        d.finish(Outcome::Accepted);
        assert!(ledger
            .reserve(Action {
                operation: uuid::Uuid::new_v4(),
                ..a
            })
            .is_err());
    }
}
