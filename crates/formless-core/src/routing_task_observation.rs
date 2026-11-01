//! Role-task ownership. Logical completion closes the task; final evidence is
//! published only after pending HostOps/call owners have all released it.
use crate::routing_observation::{Identity, Neutral};
use formless_runtime::LockExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MAX_DEPTH: usize = 16;
const MAX_MODELS: usize = 64;
const MAX_MODEL_BYTES: usize = 256 * 1024;
const MAX_ROLE_BYTES: usize = 8192;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum TaskOutcome {
    Completed,
    ContractFailure,
    Neutral(Neutral),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskRecord {
    pub id: String,
    pub role: Option<String>,
    pub parent_ids: Vec<String>,
    pub outcome: TaskOutcome,
    pub responsible: Option<Identity>,
    pub mixed: bool,
    pub expected_calls: u64,
    pub capture_complete: bool,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub elapsed_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Event {
    Closed(TaskRecord),
    Final(TaskRecord),
}
type Sink = Arc<dyn Fn(Event) + Send + Sync>;
struct State {
    closed: Option<(TaskOutcome, i64, u64)>,
    models: BTreeSet<Identity>,
    model_bytes: usize,
    calls: u64,
    complete: bool,
    last_responsible_valid: bool,
    last_contract: bool,
}
pub(crate) struct Task {
    id: String,
    role: Option<String>,
    parent: Option<Arc<Task>>,
    depth: usize,
    started: Instant,
    started_at_ms: i64,
    state: Mutex<State>,
    sink: Sink,
    resource_sink: Option<Arc<dyn Fn(crate::routing_observation::Record) + Send + Sync>>,
}
impl Task {
    pub fn new(role: &str, parent: Option<Arc<Task>>, sink: Sink) -> Option<Arc<Self>> {
        Self::create(role, parent, sink, None)
    }
    pub fn with_recorder(
        role: &str,
        parent: Option<Arc<Task>>,
        recorder: Arc<crate::routing_recorder::Recorder>,
    ) -> Option<Arc<Self>> {
        Self::create(
            role,
            parent,
            recorder.task_sink(),
            Some(recorder.call_sink()),
        )
    }
    pub fn resource_sink(
        &self,
    ) -> Option<Arc<dyn Fn(crate::routing_observation::Record) + Send + Sync>> {
        self.resource_sink.clone()
    }
    fn create(
        role: &str,
        parent: Option<Arc<Task>>,
        sink: Sink,
        resource_sink: Option<Arc<dyn Fn(crate::routing_observation::Record) + Send + Sync>>,
    ) -> Option<Arc<Self>> {
        let depth = parent.as_ref().map_or(0, |p| p.depth + 1);
        if depth >= MAX_DEPTH {
            if let Some(parent) = parent {
                parent.incomplete();
            }
            return None;
        }
        let bounded = role.len() <= MAX_ROLE_BYTES;
        Some(Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: bounded.then(|| role.to_string()).filter(|r| !r.is_empty()),
            parent,
            depth,
            started: Instant::now(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            state: Mutex::new(State {
                closed: None,
                models: BTreeSet::new(),
                model_bytes: 0,
                calls: 0,
                complete: bounded,
                last_responsible_valid: false,
                last_contract: false,
            }),
            sink,
            resource_sink,
        }))
    }
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn ids(&self) -> Vec<String> {
        let mut ids = vec![self.id.clone()];
        let mut parent = self.parent.as_ref();
        while let Some(task) = parent {
            ids.push(task.id.clone());
            parent = task.parent.as_ref();
        }
        ids
    }
    pub fn register_call(&self) {
        {
            let mut state = self.state.locked();
            if let Some(n) = state.calls.checked_add(1) {
                state.calls = n;
            } else {
                state.complete = false;
            }
        }
        if let Some(parent) = &self.parent {
            parent.register_call();
        }
    }
    pub fn incomplete(&self) {
        self.state.locked().complete = false;
        if let Some(parent) = &self.parent {
            parent.incomplete();
        }
    }
    /// Only the responsible agent's own validated steps contribute identity.
    /// Incidental Tool-LLM and child-role work still belongs to the cost scope.
    pub fn responsible(&self, identity: Identity) {
        self.responsible_outcome(identity, false);
    }
    pub fn responsible_contract(&self, identity: Identity) {
        self.responsible_outcome(identity, true);
    }
    fn responsible_outcome(&self, identity: Identity, contract: bool) {
        let mut state = self.state.locked();
        if state.closed.is_some() {
            state.complete = false;
            return;
        }
        state.last_responsible_valid = true;
        state.last_contract = contract;
        if state.models.contains(&identity) {
            return;
        }
        let bytes = identity
            .provider
            .len()
            .saturating_add(identity.endpoint.len())
            .saturating_add(identity.model.len());
        if state.closed.is_some()
            || state.models.len() >= MAX_MODELS
            || state.model_bytes.saturating_add(bytes) > MAX_MODEL_BYTES
        {
            state.complete = false;
            return;
        }
        state.model_bytes += bytes;
        state.models.insert(identity);
    }
    pub fn responsible_neutral(&self) {
        let mut state = self.state.locked();
        state.last_responsible_valid = false;
        state.last_contract = false;
    }
    pub fn serving_identity(&self) -> Option<Identity> {
        let state = self.state.locked();
        (state.last_responsible_valid && state.models.len() == 1)
            .then(|| state.models.iter().next().unwrap().clone())
    }
    pub fn terminal_outcome(&self, completed: bool) -> TaskOutcome {
        let state = self.state.locked();
        if completed && state.last_responsible_valid {
            TaskOutcome::Completed
        } else if state.last_responsible_valid && state.last_contract {
            TaskOutcome::ContractFailure
        } else {
            TaskOutcome::Neutral(Neutral::Unknown)
        }
    }
    pub fn finish(&self, outcome: TaskOutcome) {
        let record = {
            let mut state = self.state.locked();
            if state.closed.is_some() {
                return;
            }
            state.closed = Some((
                outcome,
                chrono::Utc::now().timestamp_millis(),
                self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            ));
            self.record(&state)
        };
        (self.sink)(Event::Closed(record));
    }
    fn record(&self, state: &State) -> TaskRecord {
        TaskRecord {
            id: self.id.clone(),
            role: self.role.clone(),
            parent_ids: self.ids().into_iter().skip(1).collect(),
            outcome: state
                .closed
                .as_ref()
                .map_or(TaskOutcome::Neutral(Neutral::Cancelled), |v| match &v.0 {
                    TaskOutcome::Completed if !state.last_responsible_valid => {
                        TaskOutcome::Neutral(Neutral::Unknown)
                    }
                    other => other.clone(),
                }),
            responsible: (state.models.len() == 1)
                .then(|| state.models.iter().next().unwrap().clone()),
            mixed: state.models.len() > 1,
            expected_calls: state.calls,
            capture_complete: state.complete,
            started_at_ms: self.started_at_ms,
            finished_at_ms: state
                .closed
                .as_ref()
                .map_or_else(|| chrono::Utc::now().timestamp_millis(), |v| v.1),
            elapsed_ms: state.closed.as_ref().map(|v| v.2),
        }
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        let state = self.state.locked();
        let record = self.record(&state);
        drop(state);
        (self.sink)(Event::Final(record));
    }
}
tokio::task_local! {static ACTIVE_TASK:Option<Arc<Task>>;}
pub(crate) fn current() -> Option<Arc<Task>> {
    ACTIVE_TASK.try_with(Clone::clone).ok().flatten()
}
/// Explicit None clears inherited context when a nested scope exceeds capture
/// limits. It must not relabel child work as a parent's responsible steps.
pub(crate) async fn scope<F: std::future::Future>(task: Option<Arc<Task>>, work: F) -> F::Output {
    ACTIVE_TASK.scope(task, work).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(model: &str) -> Identity {
        Identity::new("default", &crate::config::ProviderCfg::default(), model)
    }
    fn task(role: &str, parent: Option<Arc<Task>>) -> (Arc<Task>, Arc<Mutex<Vec<Event>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        (
            Task::new(
                role,
                parent,
                Arc::new(move |event| sink.locked().push(event)),
            )
            .unwrap(),
            events,
        )
    }
    #[test]
    fn logical_close_keeps_pending_owner_and_cost_scope_until_final_release() {
        let (task, events) = task("role", None);
        let pending_owner = task.clone();
        let weak = Arc::downgrade(&task);
        task.responsible(identity("served"));
        task.register_call();
        task.finish(TaskOutcome::Completed);
        drop(task);
        assert_eq!(events.locked().len(), 1);
        assert!(
            matches!(&events.locked()[0],Event::Closed(record) if record.responsible.as_ref().unwrap().model=="served")
        );
        pending_owner.register_call();
        drop(pending_owner);
        assert!(weak.upgrade().is_none());
        let events = events.locked();
        assert_eq!(events.len(), 2);
        assert!(
            matches!(&events[1],Event::Final(record) if record.expected_calls==2&&record.capture_complete&&record.outcome==TaskOutcome::Completed)
        );
    }
    #[test]
    fn child_and_incidental_calls_count_cost_without_relabeling_parent_model() {
        let (parent, events) = task("parent", None);
        parent.responsible(identity("parent-model"));
        let (child, child_events) = task("child", Some(parent.clone()));
        child.responsible(identity("child-model"));
        child.register_call();
        assert_eq!(
            child.ids(),
            vec![child.id().to_string(), parent.id().to_string()]
        );
        parent.finish(TaskOutcome::Completed);
        drop(parent);
        assert_eq!(events.locked().len(), 1);
        child.finish(TaskOutcome::Completed);
        drop(child);
        let events = events.locked();
        assert!(
            matches!(&events[1],Event::Final(record) if record.expected_calls==1&&record.responsible.as_ref().unwrap().model=="parent-model")
        );
        assert!(
            matches!(&child_events.locked()[1],Event::Final(record) if record.responsible.as_ref().unwrap().model=="child-model")
        );
    }
    #[test]
    fn a_new_same_identity_responsible_outcome_after_close_is_incomplete() {
        let (task, events) = task("worker", None);
        task.responsible(identity("served"));
        task.finish(TaskOutcome::Completed);
        task.responsible(identity("served"));
        drop(task);
        assert!(matches!(&events.locked()[1],Event::Final(record) if !record.capture_complete));
    }
    #[test]
    fn terminal_neutral_step_cannot_borrow_earlier_success_and_later_retry_can_recover() {
        let (task, events) = task("worker", None);
        task.responsible(identity("served"));
        task.responsible_neutral();
        assert!(task.serving_identity().is_none());
        assert!(matches!(
            task.terminal_outcome(true),
            TaskOutcome::Neutral(_)
        ));
        task.responsible(identity("served"));
        assert_eq!(task.terminal_outcome(true), TaskOutcome::Completed);
        task.responsible_neutral();
        task.finish(TaskOutcome::Completed);
        drop(task);
        assert!(
            matches!(&events.locked()[1],Event::Final(record) if matches!(record.outcome,TaskOutcome::Neutral(_)))
        );
    }
    #[test]
    fn mixed_responsible_models_remain_explicitly_unattributed() {
        let (task, events) = task("role", None);
        task.responsible(identity("a"));
        task.responsible(identity("b"));
        task.finish(TaskOutcome::Completed);
        drop(task);
        assert!(
            matches!(&events.locked()[1],Event::Final(record) if record.mixed&&record.responsible.is_none())
        );
    }
}

/// One logical task lifetime. Temporary scopes/HostOps retain the Arc but do
/// not close it; cancellation/unwind of its producer still seals neutral.
pub(crate) struct Producer(pub Option<Arc<Task>>);
impl Drop for Producer {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.finish(TaskOutcome::Neutral(Neutral::Cancelled));
        }
    }
}
