//! Explicit, restrictive Session Step-mode enabling. Never approves/releases a Tool.
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
pub const MAX_OPERATIONS: usize = 256;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub client: uuid::Uuid,
    pub connection: u64,
    pub generation: u64,
    pub view_epoch: u64,
    pub operation: uuid::Uuid,
    pub session_id: String,
}
impl Action {
    pub fn valid(&self) -> bool {
        !self.client.is_nil()
            && !self.operation.is_nil()
            && !self.session_id.is_empty()
            && self.session_id.len() <= 256
            && !self.session_id.chars().any(char::is_control)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Enabled,
    ObservedEnabled,
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
    pub fn notice(&self) -> Option<&str> {
        match self {
        Self::Idle=>None,
        Self::Reserved(_)=>Some("Revalidating Step mode; Enable has not been written."),
        Self::Sent(_)=>Some("Enable Step mode may have been written; awaiting its receipt. No automatic retry."),
        Self::Finished{outcome:Outcome::Enabled,..}=>Some("Step mode enabled for the reviewed Session. No Tool was approved or released."),
        Self::Finished{outcome:Outcome::ObservedEnabled,..}=>Some("A fresh read already shows Step mode enabled. No setting was sent."),
        Self::Finished{outcome:Outcome::Rejected(reason),..}=>Some(reason),
        Self::Finished{outcome:Outcome::Unknown,..}=>Some("Step-mode outcome unknown. Inspect fresh state or Formless TUI; native enables are blocked for this client. Nothing is replayed."),
    }
    }
    pub fn action(&self) -> Option<&Action> {
        match self {
            Self::Idle => None,
            Self::Reserved(a) | Self::Sent(a) | Self::Finished { action: a, .. } => Some(a),
        }
    }
}
#[derive(Debug, Default)]
struct State {
    status: Status,
    operations: HashSet<uuid::Uuid>,
}
#[derive(Debug, Default)]
pub struct Ledger(Mutex<State>);
impl Ledger {
    pub fn status(&self) -> Status {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub(crate) fn cancel_unsent(&self, reason: &str) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Status::Reserved(action) = &state.status {
            state.status = Status::Finished {
                action: action.clone(),
                outcome: Outcome::Rejected(reason.chars().take(500).collect()),
            };
        }
    }
    pub(crate) fn reserve(self: &Arc<Self>, action: Action) -> Result<Dispatch, &'static str> {
        if !action.valid() {
            return Err("Invalid Session or operation; Enable was not queued.");
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.status.blocked() {
            return Err("A mode enable is pending or unknown. Inspect the receipt/TUI; no retry was queued.");
        }
        if state.operations.contains(&action.operation) {
            return Err("This enable operation was already spent; use a fresh control.");
        }
        if state.operations.len() >= MAX_OPERATIONS {
            return Err("Native mode receipt capacity reached for this client; use Formless TUI or a new attachment.");
        }
        state.operations.insert(action.operation);
        state.status = Status::Reserved(action.clone());
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
        let mut s = self.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = match &s.status {
            Status::Reserved(a) if a == &self.action => Some(Outcome::Rejected(
                "Enable was not written; review again explicitly.".into(),
            )),
            Status::Sent(a) if a == &self.action => Some(Outcome::Unknown),
            _ => None,
        };
        if let Some(outcome) = outcome {
            s.status = Status::Finished {
                action: self.action.clone(),
                outcome,
            };
        }
    }
}
#[derive(Clone, Debug)]
pub struct Dispatch(Arc<Owner>);
impl Dispatch {
    pub fn action(&self) -> &Action {
        &self.0.action
    }
    pub(crate) fn owned_by(&self, ledger: &Arc<Ledger>) -> bool {
        Arc::ptr_eq(&self.0.ledger, ledger)
    }
    pub(crate) fn reserved(&self) -> bool {
        self.0.ledger.status() == Status::Reserved(self.action().clone())
    }
    pub(crate) fn sent(&self) -> bool {
        self.0.ledger.status() == Status::Sent(self.action().clone())
    }
    pub(crate) fn reject(&self, reason: &str) {
        self.finish_reserved(Outcome::Rejected(reason.chars().take(500).collect()));
    }
    fn finish_reserved(&self, outcome: Outcome) {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Reserved(a) if a==self.action()) {
            s.status = Status::Finished {
                action: self.action().clone(),
                outcome,
            };
        }
    }
    pub(crate) fn observed_enabled(&self) {
        self.finish_reserved(Outcome::ObservedEnabled);
    }
    pub(crate) fn before_write(&self) -> bool {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Reserved(a)|Status::Sent(a) if a==self.action()) {
            s.status = Status::Sent(self.action().clone());
            true
        } else {
            false
        }
    }
    pub(crate) fn finish(&self, outcome: Outcome) {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Sent(a) if a==self.action()) {
            s.status = Status::Finished {
                action: self.action().clone(),
                outcome,
            };
        }
    }
}
pub(crate) fn decode(action: &Action, result: Result<Value, String>, ok: bool) -> Outcome {
    match result {
        Ok(v) if ok && v["session_id"].as_str() == Some(&action.session_id) && v["on"] == true => {
            Outcome::Enabled
        }
        Err(e) if !ok => Outcome::Rejected(e.chars().take(500).collect()),
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
            connection: 1,
            generation: 2,
            view_epoch: 3,
            operation: uuid::Uuid::new_v4(),
            session_id: "s".into(),
        }
    }
    #[test]
    fn unsent_and_possible_write_drop_are_distinct_and_unknown_survives_other_owners() {
        for sent in [false, true] {
            let l = Arc::new(Ledger::default());
            let d = l.reserve(action()).unwrap();
            if sent {
                assert!(d.before_write());
            }
            drop(d);
            assert!(
                matches!(
                    l.status(),
                    Status::Finished {
                        outcome: Outcome::Unknown,
                        ..
                    }
                ) == sent
            );
            assert_eq!(l.reserve(action()).is_err(), sent);
        }
        let l = Arc::new(Ledger::default());
        let old = l.reserve(action()).unwrap();
        old.reject("not sent");
        let next = l.reserve(action()).unwrap();
        drop(old);
        assert!(next.reserved());
    }
    #[test]
    fn receipts_require_exact_target_true_boolean_and_success() {
        let a = action();
        assert_eq!(
            decode(&a, Ok(json!({"session_id":"s","on":true})), true),
            Outcome::Enabled
        );
        for v in [
            json!({"session_id":"other","on":true}),
            json!({"session_id":"s","on":false}),
            json!({"session_id":"s","on":"true"}),
            json!({"session_id":"s"}),
        ] {
            assert_eq!(decode(&a, Ok(v), true), Outcome::Unknown);
        }
        assert_eq!(
            decode(&a, Err("missing result".into()), true),
            Outcome::Unknown
        );
        assert!(matches!(
            decode(&a, Err("rejected".into()), false),
            Outcome::Rejected(_)
        ));
    }
    #[test]
    fn operation_tombstones_remain_bounded_and_repeated_old_tokens_never_reserve() {
        let l = Arc::new(Ledger::default());
        let first = action();
        let d = l.reserve(first.clone()).unwrap();
        d.observed_enabled();
        drop(d);
        for _ in 1..MAX_OPERATIONS {
            let d = l.reserve(action()).unwrap();
            d.reject("unsent");
        }
        assert!(l.reserve(first).is_err());
        assert!(l.reserve(action()).is_err());
        assert_eq!(l.0.lock().unwrap().operations.len(), MAX_OPERATIONS);
    }
    #[test]
    fn observed_enabled_is_read_only_and_pending_reservations_are_exclusive() {
        let l = Arc::new(Ledger::default());
        let d = l.reserve(action()).unwrap();
        assert!(l.reserve(action()).is_err());
        d.observed_enabled();
        assert!(!d.before_write());
        assert!(matches!(
            l.status(),
            Status::Finished {
                outcome: Outcome::ObservedEnabled,
                ..
            }
        ));
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    fn action() -> Action {
        Action {
            client: uuid::Uuid::new_v4(),
            connection: 0,
            generation: 1,
            view_epoch: 0,
            operation: uuid::Uuid::new_v4(),
            session_id: "s".into(),
        }
    }
    #[test]
    fn invalid_mode_identities_never_spend_capacity() {
        for field in 0..5 {
            let l = Arc::new(Ledger::default());
            let mut a = action();
            match field {
                0 => a.client = uuid::Uuid::nil(),
                1 => a.operation = uuid::Uuid::nil(),
                2 => a.session_id.clear(),
                3 => a.session_id = "x".repeat(257),
                _ => a.session_id = "s\nother".into(),
            };
            assert!(l.reserve(a).is_err());
            assert!(l.0.lock().unwrap().operations.is_empty());
        }
    }
    #[test]
    fn cancel_cannot_demote_a_possibly_written_mode_enable() {
        let l = Arc::new(Ledger::default());
        let d = l.reserve(action()).unwrap();
        assert!(d.before_write());
        l.cancel_unsent("too late");
        assert!(d.sent());
        drop(d);
        assert!(matches!(
            l.status(),
            Status::Finished {
                outcome: Outcome::Unknown,
                ..
            }
        ));
    }
}
