//! One client-lifetime non-replayed Session fork reservation and receipt.
//! The ledger is shared with the UI so a full update queue cannot lose a receipt.
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub connection: u64,
    pub operation: uuid::Uuid,
    pub source_id: String,
}
impl Action {
    pub fn valid(&self) -> bool {
        !self.source_id.is_empty()
            && self.source_id.len() <= 256
            && !self.source_id.chars().any(char::is_control)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub child_id: String,
    pub fork_point_seq: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied(Receipt),
    Rejected(String),
    Unknown(String),
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
    pub fn blocked_reason(&self) -> Option<&'static str> {
        match self {
            Self::Reserved(_) | Self::Sent(_) => {
                Some("A fork is awaiting its receipt; do not submit another")
            }
            Self::Finished {
                outcome: Outcome::Unknown(_),
                ..
            } => Some("A fork outcome is unknown. Check Sessions/TUI; native Fork is blocked"),
            _ => None,
        }
    }
}
#[derive(Debug, Default)]
pub struct Ledger(Mutex<Status>);
impl Ledger {
    pub fn status(&self) -> Status {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn reserve(&self, action: Action) -> Result<(), &'static str> {
        if !action.valid() {
            return Err("Invalid source Session; no fork was queued");
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = state.blocked_reason() {
            return Err(reason);
        }
        if matches!(&*state,Status::Finished{action:previous,..} if previous.operation==action.operation)
        {
            return Err("That fork operation already finished. Review a new action explicitly");
        }
        *state = Status::Reserved(action);
        Ok(())
    }
    pub fn reserved(&self, action: &Action) -> bool {
        self.status() == Status::Reserved(action.clone())
    }
    pub fn before_write(&self, action: &Action) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match &*state {
            Status::Reserved(current) | Status::Sent(current) if current == action => {
                *state = Status::Sent(action.clone());
                true
            }
            _ => false,
        }
    }
    pub fn reject_unsent(&self, action: &Action, reason: &str) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state,Status::Reserved(current) if current==action) {
            *state = Status::Finished {
                action: action.clone(),
                outcome: Outcome::Rejected(reason.chars().take(500).collect()),
            };
        }
    }
    pub fn finish(&self, action: &Action, outcome: Outcome) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&*state,Status::Sent(current) if current==action) {
            *state = Status::Finished {
                action: action.clone(),
                outcome,
            };
        }
    }
    fn retire_connection(&self, connection: u64) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let replacement=match &*state {
            Status::Reserved(action) if action.connection==connection=>Some((action.clone(),Outcome::Rejected("Fork was not written before disconnect. Review again explicitly.".into()))),
            Status::Sent(action) if action.connection==connection=>Some((action.clone(),Outcome::Unknown("Connection ended before a verified fork receipt. Check Sessions/TUI; it may already exist. No retry was sent.".into()))),
            _=>None,
        };
        if let Some((action, outcome)) = replacement {
            *state = Status::Finished { action, outcome };
        }
    }
}
pub struct ConnectionGuard {
    ledger: Arc<Ledger>,
    connection: u64,
}
impl ConnectionGuard {
    pub fn new(ledger: Arc<Ledger>, connection: u64) -> Self {
        Self { ledger, connection }
    }
}
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.ledger.retire_connection(self.connection);
    }
}

/// A negative/malformed acknowledgement is held conservatively too: no new
/// durable child is inferred absent a fully verified source/child receipt.
pub fn decode(action: &Action, result: Result<Value, String>) -> Outcome {
    let unknown = |error: &str| Outcome::Unknown(error.chars().take(500).collect());
    let value = match result {
        Ok(value) => value,
        Err(error) => {
            let error: String = error.chars().take(300).collect();
            return unknown(&format!("Daemon did not return a verified fork receipt: {error}. Check Sessions/TUI before another fork."));
        }
    };
    let Some(child) = value["id"].as_str() else {
        return unknown("Fork receipt is malformed. Check Sessions/TUI; it may already exist.");
    };
    let Some(point) = value["fork_point_seq"].as_u64() else {
        return unknown("Fork receipt has no supported lineage bound. Check Sessions/TUI; it may already exist.");
    };
    if !action.valid()
        || child.is_empty()
        || child.len() > 256
        || child == action.source_id
        || child.chars().any(char::is_control)
        || value["forked_from"].as_str() != Some(action.source_id.as_str())
    {
        return unknown("Fork receipt has mismatched source/child identities. Check Sessions/TUI; no retry was sent.");
    }
    Outcome::Applied(Receipt {
        child_id: child.into(),
        fork_point_seq: point,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn action() -> Action {
        Action {
            connection: 3,
            operation: uuid::Uuid::new_v4(),
            source_id: "source".into(),
        }
    }
    #[test]
    fn ledger_reserves_once_and_only_verified_matching_sent_receipts_finish() {
        let ledger = Ledger::default();
        let action = action();
        ledger.reserve(action.clone()).unwrap();
        assert!(ledger.reserve(action.clone()).is_err());
        ledger.finish(
            &action,
            Outcome::Applied(Receipt {
                child_id: "child".into(),
                fork_point_seq: 0,
            }),
        );
        assert!(matches!(ledger.status(), Status::Reserved(_)));
        assert!(ledger.before_write(&action));
        ledger.reject_unsent(&action, "too late");
        assert!(matches!(ledger.status(), Status::Sent(_)));
        ledger.finish(
            &Action {
                operation: uuid::Uuid::new_v4(),
                ..action.clone()
            },
            Outcome::Rejected("foreign".into()),
        );
        assert!(matches!(ledger.status(), Status::Sent(_)));
        ledger.finish(
            &action,
            decode(
                &action,
                Ok(json!({"id":"child","forked_from":"source","fork_point_seq":0})),
            ),
        );
        assert!(matches!(
            ledger.status(),
            Status::Finished {
                outcome: Outcome::Applied(_),
                ..
            }
        ));
        ledger
            .reserve(Action {
                operation: uuid::Uuid::new_v4(),
                ..action
            })
            .unwrap();
    }
    #[test]
    fn connection_retirement_distinguishes_known_unsent_from_unknown_writes() {
        let ledger = Arc::new(Ledger::default());
        let action = action();
        ledger.reserve(action.clone()).unwrap();
        drop(ConnectionGuard::new(ledger.clone(), action.connection));
        assert!(matches!(
            ledger.status(),
            Status::Finished {
                outcome: Outcome::Rejected(_),
                ..
            }
        ));
        let action = Action {
            operation: uuid::Uuid::new_v4(),
            ..action
        };
        ledger.reserve(action.clone()).unwrap();
        assert!(ledger.before_write(&action));
        drop(ConnectionGuard::new(ledger.clone(), action.connection + 1));
        assert!(matches!(ledger.status(), Status::Sent(_)));
        drop(ConnectionGuard::new(ledger.clone(), action.connection));
        assert!(matches!(
            ledger.status(),
            Status::Finished {
                outcome: Outcome::Unknown(_),
                ..
            }
        ));
        assert!(ledger
            .reserve(Action {
                connection: 4,
                operation: uuid::Uuid::new_v4(),
                ..action
            })
            .is_err());
    }
    #[test]
    fn fork_receipts_require_exact_lineage_and_never_invent_a_failed_write() {
        let action = action();
        for value in [
            Value::Null,
            json!({"id":"source","forked_from":"source","fork_point_seq":1}),
            json!({"id":"child","forked_from":"other","fork_point_seq":1}),
            json!({"id":"child","forked_from":"source","fork_point_seq":-1}),
            json!({"id":"child","forked_from":"source"}),
        ] {
            assert!(matches!(decode(&action, Ok(value)), Outcome::Unknown(_)));
        }
        assert!(matches!(
            decode(&action, Err("not found".into())),
            Outcome::Unknown(_)
        ));
        assert!(matches!(
            decode(
                &action,
                Ok(json!({"id":"child","forked_from":"source","fork_point_seq":7}))
            ),
            Outcome::Applied(Receipt {
                fork_point_seq: 7,
                ..
            })
        ));
    }
}
