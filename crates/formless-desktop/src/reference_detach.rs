//! Client-lifetime reservations and non-replayed reference-removal receipts.
use crate::pending_files::Report;
use serde_json::Value;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
pub const MAX_OPERATIONS: usize = 256;
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub client: uuid::Uuid,
    pub connection: u64,
    pub epoch: u64,
    pub form: uuid::Uuid,
    pub read: uuid::Uuid,
    pub operation: uuid::Uuid,
    pub session_id: String,
    pub index: u32,
    pub report: Arc<Report>,
}
impl Action {
    pub fn valid(&self) -> bool {
        !self.client.is_nil()
            && !self.form.is_nil()
            && !self.read.is_nil()
            && !self.operation.is_nil()
            && self.report.session_id() == self.session_id
            && self.report.is_reference(self.index)
    }
    pub(crate) fn request(&self) -> formless_protocol::Request {
        formless_protocol::Request::DetachReviewedReference {
            session_id: self.session_id.clone(),
            expected_attachments: self.report.expected(),
            index: self.index,
            operation_id: self.operation.to_string(),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Removed,
    Rejected(String),
    Unknown,
}
#[derive(Clone, Debug, Default, PartialEq)]
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
    pub fn action(&self) -> Option<&Action> {
        match self {
            Self::Idle => None,
            Self::Reserved(a) | Self::Sent(a) | Self::Finished { action: a, .. } => Some(a),
        }
    }
    pub fn busy(&self) -> bool {
        matches!(self, Self::Reserved(_) | Self::Sent(_))
    }
    pub fn blocked(&self) -> bool {
        self.busy()
            || matches!(
                self,
                Self::Finished {
                    outcome: Outcome::Unknown,
                    ..
                }
            )
    }
    pub fn notice(&self) -> Option<&str> {
        match self{
        Self::Idle=>None,Self::Reserved(_)=>Some("Reference removal queued but not written; no automatic retry."),Self::Sent(_)=>Some("Reference removal may have been written; awaiting its exact receipt. No automatic retry."),
        Self::Finished{outcome:Outcome::Removed,..}=>Some("One reviewed reference occurrence was removed. Target and artifact files were not deleted. Read pending files again explicitly."),
        Self::Finished{outcome:Outcome::Rejected(e),..}=>Some(e),Self::Finished{outcome:Outcome::Unknown,..}=>Some("Reference-removal outcome is unknown. It may already be removed. Native removal is blocked for this Client; inspect/continue in Formless TUI. Nothing is replayed."),
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
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Status::Reserved(a) = &s.status {
            s.status = Status::Finished {
                action: a.clone(),
                outcome: Outcome::Rejected(reason.chars().take(500).collect()),
            };
        }
    }
    pub(crate) fn reserve(self: &Arc<Self>, action: Action) -> Result<Dispatch, &'static str> {
        if !action.valid() {
            return Err("Invalid reference review; no removal was queued.");
        }
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if s.status.blocked() {
            return Err(
                "A reference removal is pending or unknown. Use Formless TUI; no retry was queued.",
            );
        }
        if s.operations.contains(&action.operation) {
            return Err(
                "This reference-removal identity was already spent; review a fresh snapshot.",
            );
        }
        if s.operations.len() >= MAX_OPERATIONS {
            return Err(
                "Native reference receipt capacity reached for this Client; use Formless TUI.",
            );
        }
        s.operations.insert(action.operation);
        s.status = Status::Reserved(action.clone());
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
                "Reference removal was not written; review again explicitly.".into(),
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
    pub(crate) fn owned_by(&self, l: &Arc<Ledger>) -> bool {
        Arc::ptr_eq(&self.0.ledger, l)
    }
    pub(crate) fn reserved(&self) -> bool {
        self.0.ledger.status() == Status::Reserved(self.action().clone())
    }
    pub(crate) fn sent(&self) -> bool {
        self.0.ledger.status() == Status::Sent(self.action().clone())
    }
    pub(crate) fn reject(&self, reason: &str) {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Reserved(a)if a==self.action()) {
            s.status = Status::Finished {
                action: self.action().clone(),
                outcome: Outcome::Rejected(reason.chars().take(500).collect()),
            };
        }
    }
    pub(crate) fn before_write(&self) -> bool {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Reserved(a)if a==self.action()) {
            s.status = Status::Sent(self.action().clone());
            true
        } else {
            false
        }
    }
    pub(crate) fn finish(&self, outcome: Outcome) {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Sent(a)if a==self.action()) {
            s.status = Status::Finished {
                action: self.action().clone(),
                outcome,
            };
        }
    }
}
pub(crate) fn acknowledged(action: &Action, value: &Value) -> bool {
    value["removed"].as_bool() == Some(true)
        && value["session_id"].as_str() == Some(&action.session_id)
        && value["index"].as_u64() == Some(u64::from(action.index))
        && value["operation_id"].as_str() == Some(action.operation.to_string().as_str())
}
pub(crate) fn decode(action: &Action, result: Result<Value, String>, ok: bool) -> Outcome {
    if ok && result.as_ref().is_ok_and(|v| acknowledged(action, v)) {
        Outcome::Removed
    } else {
        Outcome::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn action() -> Action {
        Action{client:uuid::Uuid::new_v4(),connection:3,epoch:4,form:uuid::Uuid::new_v4(),read:uuid::Uuid::new_v4(),operation:uuid::Uuid::new_v4(),session_id:"s".into(),index:1,report:Arc::new(Report::decode("s",json!({"session_id":"s","attachments":[{"id":"stored","name":"stored","path":"/private/stored","mime":"text/plain","bytes":1},{"reference":true,"name":"folder ไทย😀","path":"/private/ref","dir":true,"bytes":0,"future":true}]})).unwrap())}
    }
    #[test]
    fn reference_reservation_requires_complete_scoped_metadata_and_never_spends_invalid_ids() {
        for n in 0..8 {
            let l = Arc::new(Ledger::default());
            let mut a = action();
            match n {
                0 => a.client = uuid::Uuid::nil(),
                1 => a.form = uuid::Uuid::nil(),
                2 => a.read = uuid::Uuid::nil(),
                3 => a.operation = uuid::Uuid::nil(),
                4 => a.session_id = "other".into(),
                5 => a.index = 0,
                6 => a.index = 2,
                _ => a.session_id.clear(),
            };
            assert!(l.reserve(a).is_err());
            assert!(l.0.lock().unwrap().operations.is_empty());
        }
    }
    #[test]
    fn reference_drop_distinguishes_unsent_from_possible_write_and_never_demotes_unknown() {
        for written in [false, true] {
            let l = Arc::new(Ledger::default());
            let d = l.reserve(action()).unwrap();
            if written {
                assert!(d.before_write());
                assert!(!d.before_write());
            }
            l.cancel_unsent("cancelled before write");
            drop(d);
            assert_eq!(
                matches!(
                    l.status(),
                    Status::Finished {
                        outcome: Outcome::Unknown,
                        ..
                    }
                ),
                written
            );
            assert_eq!(l.reserve(action()).is_err(), written);
        }
        let l = Arc::new(Ledger::default());
        let old = l.reserve(action()).unwrap();
        old.reject("unsent");
        let next = l.reserve(action()).unwrap();
        drop(old);
        assert!(next.reserved());
    }
    #[test]
    fn reference_receipt_requires_exact_session_index_operation_and_true_boolean() {
        let a = action();
        let good = json!({"removed":true,"session_id":"s","index":1,"operation_id":a.operation.to_string()});
        assert_eq!(decode(&a, Ok(good.clone()), true), Outcome::Removed);
        for n in 0..6 {
            let mut v = good.clone();
            match n {
                0 => v["removed"] = json!(false),
                1 => v["removed"] = json!("true"),
                2 => v["session_id"] = json!("other"),
                3 => v["index"] = json!(0),
                4 => v["operation_id"] = json!(uuid::Uuid::new_v4().to_string()),
                _ => v["operation_id"] = Value::Null,
            };
            assert_eq!(decode(&a, Ok(v), true), Outcome::Unknown);
        }
        assert_eq!(
            decode(
                &a,
                Err("rejected before commit or failed after commit".into()),
                false
            ),
            Outcome::Unknown
        );
        assert_eq!(decode(&a, Ok(good), false), Outcome::Unknown);
    }
    #[test]
    fn reference_tombstones_survive_finished_receipts_and_capacity_never_evicts_them() {
        let l = Arc::new(Ledger::default());
        let first = action();
        for n in 0..MAX_OPERATIONS {
            let d = l
                .reserve(if n == 0 { first.clone() } else { action() })
                .unwrap();
            d.reject("not written");
        }
        assert!(l.reserve(first).is_err());
        assert!(l.reserve(action()).is_err());
        assert_eq!(l.0.lock().unwrap().operations.len(), MAX_OPERATIONS);
    }
    #[test]
    fn reference_wire_payload_preserves_whole_inventory_and_one_selected_duplicate_position() {
        let a = action();
        let formless_protocol::Request::DetachReviewedReference {
            session_id,
            index,
            operation_id,
            expected_attachments,
        } = a.request()
        else {
            panic!("new guarded API only")
        };
        assert_eq!(session_id, a.session_id);
        assert_eq!(index, 1);
        assert_eq!(operation_id, a.operation.to_string());
        assert_eq!(expected_attachments, a.report.expected());
        assert!(expected_attachments[1]["future"].as_bool().unwrap());
    }
}
