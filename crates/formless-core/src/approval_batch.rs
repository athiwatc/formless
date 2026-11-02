//! An approval prepass owns its batch. No session-global mutable cursor can
//! attach another task's prompt or remove a concurrently waiting batch.
use super::*;

pub(super) struct Batch {
    id: String,
    calls: Vec<(usize, String, HeldCall)>,
}

impl Batch {
    pub(super) fn new(calls: Vec<(usize, String, HeldCall)>) -> Option<Self> {
        (calls.len() > 1).then(|| Self {
            id: uuid::Uuid::new_v4().to_string(),
            calls,
        })
    }

    pub(super) fn for_call(&self, ordinal: usize, call_id: &str) -> Option<ApprovalBatch> {
        let at = self
            .calls
            .iter()
            .position(|(original, id, _)| *original == ordinal && id == call_id)?;
        Some(ApprovalBatch {
            batch_id: self.id.clone(),
            index: at + 1,
            calls: self.calls.iter().map(|(_, _, call)| call.clone()).collect(),
        })
    }
}
