//! Bounded canonical queue inventory. Missing or malformed inventories fail closed.
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct Item {
    pub id: i64,
    pub content: String,
    pub revision: Option<i64>,
}
pub fn decode(value: Option<&Value>) -> Option<Vec<Item>> {
    let rows = value?.as_array()?;
    if rows.len() > 256 {
        return None;
    }
    let mut bytes = 0;
    let mut ids = std::collections::HashSet::new();
    rows.iter()
        .map(|row| {
            let item: Item = serde_json::from_value(row.clone()).ok()?;
            bytes += item.content.len();
            (item.id > 0
                && ids.insert(item.id)
                && item.content.len() <= 64 * 1024
                && bytes <= 1024 * 1024
                && item.revision.is_none_or(|r| r >= 0))
            .then_some(item)
        })
        .collect()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operation {
    Enqueue,
    Edit(i64),
    Up(i64),
    Down(i64),
    Remove(i64),
}
impl Operation {
    pub fn id(self) -> Option<i64> {
        match self {
            Self::Enqueue => None,
            Self::Edit(id) | Self::Up(id) | Self::Down(id) | Self::Remove(id) => Some(id),
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Enqueue => "Queue follow-up",
            Self::Edit(_) => "Edit queued message",
            Self::Up(_) => "Move queued message up",
            Self::Down(_) => "Move queued message down",
            Self::Remove(_) => "Remove queued message",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_actions::{Action, Kind, Ledger, Owner, Status, Target};
    use serde_json::json;
    use std::sync::Arc;
    #[test]
    fn inventories_are_bounded_and_missing_is_not_empty() {
        assert_eq!(decode(None), None);
        assert_eq!(decode(Some(&json!([]))), Some(vec![]));
        for rows in [
            json!({}),
            json!([{"id":0,"content":"bad"}]),
            json!([{"id":1,"content":"x"},{"id":1,"content":"y"}]),
            json!([{"id":1,"content":"x","revision":-1}]),
            json!([{"id":1,"content":"x".repeat(65537)}]),
        ] {
            assert!(decode(Some(&rows)).is_none());
        }
        assert!(
            decode(Some(&json!([{"id":1,"content":"old peer"}]))).unwrap()[0]
                .revision
                .is_none()
        );
    }
    #[test]
    fn queue_actions_gate_revision_position_run_state_and_receipts() {
        let v = json!({"session":{"id":"s"},"messages":[],"running":true,"queued_items":[{"id":1,"content":"one","revision":4},{"id":2,"content":"two","revision":0}]});
        let target = Target::decode("s", &v).unwrap();
        assert!(!target.allowed(Kind::Queue(Operation::Up(1))));
        assert!(!target.allowed(Kind::Queue(Operation::Down(2))));
        for op in [
            Operation::Enqueue,
            Operation::Edit(1),
            Operation::Down(1),
            Operation::Up(2),
            Operation::Remove(1),
        ] {
            let mut a = Action {
                operation: uuid::Uuid::new_v4(),
                owner: Owner {
                    client: uuid::Uuid::new_v4(),
                    connection: 1,
                    epoch: 0,
                    generation: 1,
                    session: "s".into(),
                },
                target: target.clone(),
                kind: Kind::Queue(op),
                text: "replacement".into(),
            };
            assert!(a.valid());
            assert_eq!(a.forecast().is_some(), op == Operation::Enqueue);
            if let Operation::Edit(_) = op {
                assert!(matches!(
                    a.request(),
                    formless_protocol::Request::EditQueuedIfCurrent { revision: 4, .. }
                ));
            }
            let ledger = Arc::new(Ledger::default());
            let d = ledger.reserve(a.clone()).unwrap();
            assert!(d.before_write());
            assert!(!d.acknowledged(&json!({})));
            let receipt = match op {
                Operation::Enqueue => json!({"queued":true}),
                Operation::Up(_) | Operation::Down(_) => json!({"id":1}),
                _ => json!({"ok":true}),
            };
            assert!(d.acknowledged(&receipt));
            if op == Operation::Enqueue {
                assert!(d.acknowledged(&json!({"started":true,"run_id":"new"})));
                assert!(!d.acknowledged(&json!({"queued":true,"started":true})));
            }
            d.lost();
            assert_eq!(ledger.status(), Status::Unknown);
            assert!(ledger.reserve(a.clone()).is_err());
            if matches!(op, Operation::Edit(_) | Operation::Enqueue) {
                a.text.clear();
                assert!(!a.valid());
            }
            a.target.queue = None;
            assert!(!a.valid());
        }
        let mut old = target.clone();
        old.queue.as_mut().unwrap()[0].revision = None;
        assert!(!old.allowed(Kind::Queue(Operation::Edit(1))));
        let mut idle = target;
        idle.running = false;
        assert!(!idle.allowed(Kind::Queue(Operation::Enqueue)));
    }
}
