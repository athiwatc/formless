//! Owned observations for one logical model decision. The actual request owns
//! its attempt lease, so late stream Drop cannot write into another decision.
//! No billing hook, database operation, task spawn, or global index lives here.
use crate::{config::ProviderCfg, provider::Usage};
use formless_runtime::LockExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

const MAX_ATTEMPTS: usize = 512;
const MAX_IDENTITY_BYTES: usize = 256 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct Identity {
    pub provider: String,
    pub endpoint: String,
    pub model: String,
}
impl Identity {
    pub fn new(provider: &str, cfg: &ProviderCfg, model: &str) -> Self {
        Self {
            provider: provider.into(),
            endpoint: crate::tokens::provider_key(provider, cfg),
            model: model.into(),
        }
    }
}
/// These are not model-quality observations. Availability remains the circuit
/// breaker's responsibility, independently of routing's quality sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Neutral {
    Cancelled,
    Auth,
    RateLimit,
    Context,
    Budget,
    Accounting,
    CallerInput,
    Configuration,
    Network,
    Server,
    Unavailable,
    Unknown,
    CaptureLimit,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Outcome {
    Success(Identity),
    /// Only an explicitly typed actual-model output/response contract failure.
    /// Generic error prose or HTTP availability errors never create this.
    ContractFailure(Identity),
    Neutral(Neutral),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AttemptRecord {
    pub id: String,
    pub identity: Identity,
    pub sent: bool,
    pub status: Option<u16>,
    pub neutral: Option<Neutral>,
    #[serde(with = "usage_receipt")]
    pub usage: Option<Usage>,
    /// A cancelled/interrupted stream can report interim usage. Preserve that
    /// evidence without claiming its final cost is complete.
    pub response_complete: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub id: String,
    pub outcome: Outcome,
    pub attempts: Vec<AttemptRecord>,
    pub task_ids: Vec<String>,
    pub resource_only: bool,
    /// Bounded capture overflow makes the decision unscorable; it is not a
    /// complete zero-cost observation with some hidden requests omitted.
    pub truncated: bool,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub elapsed_ms: Option<u64>,
}
struct State {
    outcome: Outcome,
    attempts: Vec<AttemptRecord>,
    reserved: usize,
    identity_bytes: usize,
    finished: Option<(i64, u64)>,
    task_notified: bool,
    sealed: bool,
    contract: Option<Identity>,
    returned_attempt: Option<String>,
    truncated: bool,
}
/// The sink receives one final immutable receipt, after every pinned physical
/// attempt has settled/dropped. It runs without an observation mutex held.
/// Persistence/warning delivery belongs to the caller, not the billing hook.
type Sink = Arc<dyn Fn(Record) + Send + Sync>;
pub(crate) struct Call {
    id: String,
    started: std::time::Instant,
    started_at_ms: i64,
    task: Option<Arc<crate::routing_task_observation::Task>>,
    responsible: bool,
    resource_only: bool,
    state: Mutex<State>,
    sink: Sink,
}
impl Call {
    pub fn new(sink: Sink) -> Arc<Self> {
        Self::new_kind(sink, false, false)
    }
    pub fn responsible_step(sink: Sink) -> Arc<Self> {
        Self::new_kind(sink, true, false)
    }
    fn resource_only(sink: Sink) -> Arc<Self> {
        Self::new_kind(sink, false, true)
    }
    fn new_kind(sink: Sink, responsible: bool, resource_only: bool) -> Arc<Self> {
        let task = crate::routing_task_observation::current();
        if let Some(task) = &task {
            task.register_call();
        }
        Arc::new(Self {
            id: uuid::Uuid::new_v4().to_string(),
            started: std::time::Instant::now(),
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            task,
            responsible,
            resource_only,
            state: Mutex::new(State {
                outcome: Outcome::Neutral(if resource_only {
                    Neutral::Unknown
                } else {
                    Neutral::Cancelled
                }),
                attempts: Vec::new(),
                reserved: 0,
                identity_bytes: 0,
                finished: None,
                task_notified: false,
                sealed: false,
                contract: None,
                returned_attempt: None,
                truncated: false,
            }),
            sink,
        })
    }
    pub fn finish(&self, outcome: Outcome) {
        self.stop_clock();
        let identity = {
            let mut state = self.state.locked();
            if state.sealed {
                return;
            }
            state.sealed = true;
            state.outcome = outcome;
            if self.responsible {
                match validated_outcome(&state) {
                    Outcome::Success(id) | Outcome::ContractFailure(id) => {
                        state.task_notified = true;
                        Some((id, matches!(&state.outcome, Outcome::ContractFailure(_))))
                    }
                    _ => None,
                }
            } else {
                None
            }
        };
        if let (Some(task), Some((identity, contract))) = (&self.task, identity) {
            if contract {
                task.responsible_contract(identity);
            } else {
                task.responsible(identity);
            }
        }
    }
    pub fn resolved_outcome(&self) -> Outcome {
        validated_outcome(&self.state.locked())
    }
    /// Called only by compiled output validation, with the actual returned
    /// serving identity. This records evidence; the logical caller still owns
    /// cancellation/error classification and final outcome.
    pub fn claim_contract(&self, identity: Identity) {
        let mut state = self.state.locked();
        if !state.sealed
            && !state.truncated
            && state.attempts.iter().any(|a| {
                state.returned_attempt.as_ref() == Some(&a.id)
                    && a.sent
                    && a.identity == identity
                    && a.response_complete
                    && a.status == Some(200)
            })
        {
            state.contract = Some(identity);
        }
    }
    pub fn contract_identity(&self) -> Option<Identity> {
        self.state.locked().contract.clone()
    }
    /// Legacy request-health counters can record a real failed send without
    /// treating availability as model quality. This is attempted identity only;
    /// it never attests a served response or changes the correlated outcome.
    pub fn last_sent_identity(&self) -> Option<Identity> {
        let state = self.state.locked();
        if state.truncated {
            return None;
        }
        state
            .attempts
            .iter()
            .rev()
            .find(|attempt| attempt.sent)
            .map(|attempt| attempt.identity.clone())
    }
    fn stop_clock(&self) {
        self.stop_clock_at(
            std::time::Instant::now(),
            chrono::Utc::now().timestamp_millis(),
        );
    }
    fn stop_clock_at(&self, end: std::time::Instant, wall_ms: i64) {
        let elapsed = end
            .saturating_duration_since(self.started)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        self.state
            .locked()
            .finished
            .get_or_insert((wall_ms, elapsed));
    }
    pub fn attempt(self: &Arc<Self>, identity: Identity) -> Arc<Attempt> {
        // Reserve at creation, not settlement: pending streams also consume
        // the bounded metadata budget. Overflow keeps only an owner lease.
        let record = {
            let mut state = self.state.locked();
            let bytes = identity
                .provider
                .len()
                .saturating_add(identity.endpoint.len())
                .saturating_add(identity.model.len());
            if state.reserved < MAX_ATTEMPTS
                && state.identity_bytes.saturating_add(bytes) <= MAX_IDENTITY_BYTES
            {
                state.reserved += 1;
                state.identity_bytes += bytes;
                Some(AttemptRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    identity,
                    sent: false,
                    status: None,
                    neutral: None,
                    usage: None,
                    response_complete: false,
                })
            } else {
                state.truncated = true;
                None
            }
        };
        let id = record.as_ref().map(|record| record.id.clone());
        Arc::new(Attempt {
            id,
            call: self.clone(),
            state: Mutex::new(record),
        })
    }
    fn settled(&self, record: AttemptRecord) {
        {
            let mut state = self.state.locked();
            if state.attempts.len() < MAX_ATTEMPTS {
                state.attempts.push(record);
            } else {
                state.truncated = true;
            }
        }
        if self.resource_only {
            self.stop_clock();
        }
    }
}
fn validated_outcome(state: &State) -> Outcome {
    if state.truncated {
        Outcome::Neutral(Neutral::CaptureLimit)
    } else {
        match &state.outcome {
            Outcome::Success(identity) | Outcome::ContractFailure(identity)
                if !state.attempts.iter().any(|attempt| {
                    state.returned_attempt.as_ref() == Some(&attempt.id)
                        && attempt.sent
                        && attempt.identity == *identity
                        && if matches!(&state.outcome, Outcome::ContractFailure(_)) {
                            attempt.status == Some(200)
                        } else {
                            attempt
                                .status
                                .is_some_and(|status| (200..300).contains(&status))
                        }
                        && attempt.response_complete
                }) =>
            {
                Outcome::Neutral(Neutral::Unknown)
            }
            outcome => outcome.clone(),
        }
    }
}
impl Drop for Call {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|p| p.into_inner());
        let outcome = validated_outcome(state);
        if let Some(task) = &self.task {
            if state.truncated {
                task.incomplete();
            }
            if self.responsible && !state.task_notified {
                match &outcome {
                    Outcome::Success(identity) => task.responsible(identity.clone()),
                    Outcome::ContractFailure(identity) => {
                        task.responsible_contract(identity.clone())
                    }
                    Outcome::Neutral(_) => task.responsible_neutral(),
                }
            }
        }
        let record = Record {
            id: self.id.clone(),
            outcome,
            attempts: std::mem::take(&mut state.attempts),
            task_ids: self.task.as_ref().map_or_else(Vec::new, |task| task.ids()),
            resource_only: self.resource_only,
            truncated: state.truncated,
            started_at_ms: self.started_at_ms,
            finished_at_ms: state
                .finished
                .map_or_else(|| chrono::Utc::now().timestamp_millis(), |v| v.0),
            elapsed_ms: state.finished.map(|v| v.1),
        };
        (self.sink)(record);
    }
}
/// Pinned by RequestAccounting / the stream's accounting owner. The strong
/// call reference deliberately outlives the producer's dynamic task-local scope.
/// Calls keep snapshots, never strong attempt references: there is no cycle.
pub(crate) struct Attempt {
    id: Option<String>,
    call: Arc<Call>,
    state: Mutex<Option<AttemptRecord>>,
}
impl Attempt {
    /// The exact request whose parsed result is returned to the logical caller.
    /// A financially complete failed retry or incidental request cannot prove
    /// that another same-model response finished. No dynamic owner lookup.
    pub fn returned(&self) {
        self.call.state.locked().returned_attempt = self.id.clone();
    }
    pub fn sent(&self) {
        if let Some(record) = self.state.locked().as_mut() {
            record.sent = true;
        }
    }
    pub fn status(&self, status: u16) {
        if let Some(record) = self.state.locked().as_mut() {
            record.status = Some(status);
        }
    }
    pub fn neutral(&self, reason: Neutral) {
        if let Some(record) = self.state.locked().as_mut() {
            record.neutral = Some(reason);
        }
    }
    pub fn settle(&self, usage: Option<Usage>, response_complete: bool) {
        let record = {
            let mut state = self.state.locked();
            state.take().map(|mut record| {
                record.usage = usage;
                record.response_complete = response_complete;
                record
            })
        };
        if let Some(record) = record {
            self.call.settled(record);
        }
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        self.settle(None, false);
    }
}

tokio::task_local! {static ACTIVE_CALL: Arc<Call>;}
pub(crate) async fn scope<F: std::future::Future>(call: Arc<Call>, work: F) -> F::Output {
    struct StopClock {
        call: Arc<Call>,
        completed: bool,
    }
    impl Drop for StopClock {
        fn drop(&mut self) {
            self.call.stop_clock();
            if !self.completed {
                self.call.finish(Outcome::Neutral(Neutral::Cancelled));
            }
        }
    }
    // This scopes the whole logical decision (including repairs), not each
    // individual send. Cancellation stops time here; late receipts only pin data.
    let mut clock = StopClock {
        call: call.clone(),
        completed: false,
    };
    let result = ACTIVE_CALL.scope(call, work).await;
    clock.completed = true;
    result
}
/// Capture the existing owner exactly once, before admission. Every later
/// callback uses its returned pinned lease, never a task-local/global lookup.
pub(crate) fn current_call() -> Option<Arc<Call>> {
    ACTIVE_CALL.try_with(Clone::clone).ok()
}
pub(crate) fn begin_attempt(identity: Identity) -> Option<Arc<Attempt>> {
    if let Some(call) = current_call() {
        return Some(call.attempt(identity));
    }
    // Incidental model resources still belong to their exact role task, even
    // when a helper has no quality-decision wrapper. Never invent an outcome.
    let task = crate::routing_task_observation::current()?;
    let sink = task.resource_sink()?;
    Some(Call::resource_only(sink).attempt(identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(model: &str) -> Identity {
        Identity::new(
            "default",
            &ProviderCfg {
                base_url: "http://private.invalid".into(),
                ..Default::default()
            },
            model,
        )
    }
    fn capture() -> (Arc<Call>, Arc<Mutex<Vec<Record>>>) {
        let rows = Arc::new(Mutex::new(Vec::new()));
        let sink = rows.clone();
        (
            Call::new(Arc::new(move |record| {
                sink.locked().push(record);
            })),
            rows,
        )
    }
    #[test]
    fn last_attempt_pins_original_call_across_producer_finish_and_new_consumer() {
        let (producer, rows) = capture();
        let old = Arc::downgrade(&producer);
        let pending = producer.attempt(identity("actual-fallback"));
        pending.sent();
        producer.finish(Outcome::Success(identity("actual-fallback")));
        drop(producer);
        assert!(rows.locked().is_empty());
        assert!(old.upgrade().is_some());
        let (consumer, other) = capture();
        let next = consumer.attempt(identity("new-consumer"));
        next.sent();
        next.status(200);
        next.returned();
        next.settle(
            Some(Usage {
                prompt_tokens: 9,
                reported: 1,
                ..Default::default()
            }),
            true,
        );
        consumer.finish(Outcome::Success(identity("new-consumer")));
        drop(next);
        drop(consumer);
        pending.settle(
            Some(Usage {
                prompt_tokens: 3,
                reported: 1,
                ..Default::default()
            }),
            false,
        );
        drop(pending);
        assert!(old.upgrade().is_none());
        let rows = rows.locked();
        let other = other.locked();
        assert_eq!(rows.len(), 1);
        assert_eq!(other.len(), 1);
        assert_ne!(rows[0].id, other[0].id);
        assert_eq!(rows[0].attempts[0].identity.model, "actual-fallback");
        assert_eq!(rows[0].attempts[0].usage.unwrap().prompt_tokens, 3);
        assert!(!rows[0].attempts[0].response_complete);
        assert_eq!(other[0].attempts[0].usage.unwrap().prompt_tokens, 9);
    }
    #[test]
    fn concurrent_attempts_have_exact_identity_and_duplicate_settlement_is_ignored() {
        let (call, rows) = capture();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let mut starts = Vec::new();
        let mut workers = Vec::new();
        for (model, tokens) in [("a", 3), ("b", 7)] {
            let attempt = call.attempt(identity(model));
            let ready = ready_tx.clone();
            let done = done_tx.clone();
            let (start, gate) = std::sync::mpsc::channel();
            starts.push(start);
            workers.push(std::thread::spawn(move || {
                attempt.sent();
                attempt.status(200);
                if model == "b" {
                    attempt.returned();
                }
                ready.send(()).unwrap();
                gate.recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                attempt.settle(
                    Some(Usage {
                        prompt_tokens: tokens,
                        reported: 1,
                        ..Default::default()
                    }),
                    true,
                );
                attempt.settle(
                    Some(Usage {
                        prompt_tokens: 999,
                        reported: 1,
                        ..Default::default()
                    }),
                    true,
                );
                done.send(()).unwrap();
            }));
        }
        for _ in 0..2 {
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        for start in starts {
            start.send(()).unwrap();
        }
        for _ in 0..2 {
            done_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
        for worker in workers {
            worker.join().unwrap();
        }
        call.finish(Outcome::Success(identity("b")));
        drop(call);
        let rows = rows.locked();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, Outcome::Success(identity("b")));
        let attempts = &rows[0].attempts;
        assert_eq!(attempts.len(), 2);
        assert_ne!(attempts[0].id, attempts[1].id);
        for (model, tokens) in [("a", 3), ("b", 7)] {
            assert_eq!(
                attempts
                    .iter()
                    .find(|r| r.identity.model == model)
                    .unwrap()
                    .usage
                    .unwrap()
                    .prompt_tokens,
                tokens
            );
        }
    }
    #[tokio::test]
    async fn aborted_scope_drops_unsent_and_sent_leases_without_quality_failure() {
        let (call, rows) = capture();
        let weak = Arc::downgrade(&call);
        let entered = Arc::new(tokio::sync::Notify::new());
        let ready = entered.clone();
        let task = tokio::spawn(scope(call.clone(), async move {
            let pending = begin_attempt(identity("a")).unwrap();
            pending.sent();
            pending.status(429);
            pending.neutral(Neutral::RateLimit);
            let _admission = begin_attempt(identity("b")).unwrap();
            ready.notify_one();
            std::future::pending::<()>().await;
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        drop(call);
        task.abort();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
        assert!(weak.upgrade().is_none());
        let rows = rows.locked();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, Outcome::Neutral(Neutral::Cancelled));
        assert_eq!(rows[0].attempts.len(), 2);
        assert!(rows[0]
            .attempts
            .iter()
            .any(|r| r.sent && r.status == Some(429) && r.neutral == Some(Neutral::RateLimit)));
        assert!(rows[0].attempts.iter().any(|r| !r.sent));
    }
    #[test]
    fn requested_or_unserved_identity_cannot_become_a_quality_observation() {
        let (call, rows) = capture();
        let attempt = call.attempt(identity("actual"));
        attempt.sent();
        attempt.status(200);
        attempt.settle(
            Some(Usage {
                reported: 3,
                ..Default::default()
            }),
            true,
        );
        call.finish(Outcome::ContractFailure(identity("requested")));
        drop(attempt);
        drop(call);
        assert_eq!(rows.locked()[0].outcome, Outcome::Neutral(Neutral::Unknown));
    }
    #[test]
    fn complete_prior_same_model_response_cannot_attest_returned_incomplete_attempt() {
        let (call, rows) = capture();
        let model = identity("actual");
        let earlier = call.attempt(model.clone());
        earlier.sent();
        earlier.status(200);
        earlier.returned();
        earlier.settle(None, true);
        let returned = call.attempt(model.clone());
        returned.sent();
        returned.status(200);
        returned.returned();
        returned.settle(None, false);
        call.claim_contract(model.clone());
        assert!(call.contract_identity().is_none());
        call.finish(Outcome::Success(model));
        drop(earlier);
        drop(returned);
        drop(call);
        assert_eq!(rows.locked()[0].outcome, Outcome::Neutral(Neutral::Unknown));
    }
    #[test]
    fn partial_success_status_does_not_prove_a_complete_output_contract_failure() {
        let (call, rows) = capture();
        let model = identity("actual");
        let attempt = call.attempt(model.clone());
        attempt.sent();
        attempt.status(200);
        attempt.settle(
            Some(Usage {
                cost: Some(0.1),
                reported: 3,
                ..Default::default()
            }),
            false,
        );
        call.finish(Outcome::ContractFailure(model));
        drop(attempt);
        drop(call);
        let rows = rows.locked();
        assert_eq!(rows[0].outcome, Outcome::Neutral(Neutral::Unknown));
        assert_eq!(rows[0].attempts[0].usage.unwrap().cost, Some(0.1));
    }
    #[test]
    fn logical_stop_time_is_not_extended_by_a_later_accounting_owner() {
        let (call, rows) = capture();
        let attempt = call.attempt(identity("a"));
        call.stop_clock_at(
            call.started + std::time::Duration::from_millis(250),
            call.started_at_ms + 250,
        );
        call.stop_clock_at(
            call.started + std::time::Duration::from_secs(30),
            call.started_at_ms + 30000,
        );
        let start = call.started_at_ms;
        drop(call);
        assert!(rows.locked().is_empty());
        drop(attempt);
        let rows = rows.locked();
        assert_eq!(rows[0].elapsed_ms, Some(250));
        assert_eq!(rows[0].finished_at_ms, start + 250);
        assert_eq!(rows[0].started_at_ms, start);
    }
    #[test]
    fn huge_identity_does_not_escape_the_pending_metadata_byte_budget() {
        let (call, rows) = capture();
        let mut model = identity("a");
        model.model = "x".repeat(MAX_IDENTITY_BYTES + 1);
        let attempt = call.attempt(model);
        assert!(attempt.state.locked().is_none());
        assert!(call.state.locked().truncated);
        drop(call);
        assert!(rows.locked().is_empty());
        drop(attempt);
        assert_eq!(
            rows.locked()[0].outcome,
            Outcome::Neutral(Neutral::CaptureLimit)
        );
    }
    #[test]
    fn bounded_capture_overflow_is_explicit_not_invented_free_usage() {
        let (call, rows) = capture();
        let attempts = (0..=MAX_ATTEMPTS)
            .map(|_| call.attempt(identity("a")))
            .collect::<Vec<_>>();
        assert!(
            call.state.locked().truncated,
            "overflow must be known while all leases remain live"
        );
        assert_eq!(
            attempts
                .iter()
                .filter(|a| a.state.locked().is_some())
                .count(),
            MAX_ATTEMPTS
        );
        assert!(attempts[MAX_ATTEMPTS].state.locked().is_none());
        call.finish(Outcome::Success(identity("a")));
        drop(call);
        assert!(
            rows.locked().is_empty(),
            "recordless overflow still pins the original call"
        );
        drop(attempts);
        let rows = rows.locked();
        assert_eq!(rows[0].attempts.len(), MAX_ATTEMPTS);
        assert!(rows[0].truncated);
        assert_eq!(rows[0].outcome, Outcome::Neutral(Neutral::CaptureLimit));
        assert!(rows[0].attempts.iter().all(|r| r.usage.is_none()));
    }
}

#[cfg(test)]
#[path = "routing_provider_tests.rs"]
mod provider_tests;

// Private storage representation; the public billing Usage API is unchanged.
mod usage_receipt {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct Stored {
        prompt_tokens: u64,
        completion_tokens: u64,
        reasoning_tokens: u64,
        cached_tokens: u64,
        cache_write_tokens: u64,
        cost: Option<f64>,
        cost_unknown: bool,
        reported: u8,
    }
    pub fn serialize<S: serde::Serializer>(usage: &Option<Usage>, s: S) -> Result<S::Ok, S::Error> {
        usage
            .map(|u| Stored {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                reasoning_tokens: u.reasoning_tokens,
                cached_tokens: u.cached_tokens,
                cache_write_tokens: u.cache_write_tokens,
                cost: u.cost,
                cost_unknown: u.cost_unknown,
                reported: u.reported,
            })
            .serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Usage>, D::Error> {
        Ok(Option::<Stored>::deserialize(d)?.map(|u| Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            reasoning_tokens: u.reasoning_tokens,
            cached_tokens: u.cached_tokens,
            cache_write_tokens: u.cache_write_tokens,
            cost: u.cost,
            cost_unknown: u.cost_unknown,
            reported: u.reported,
        }))
    }
}
