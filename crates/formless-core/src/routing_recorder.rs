//! Drop callbacks only enqueue bounded immutable receipts. A daemon drains a
//! batch, releases this queue mutex, then persists it outside caller/index locks.
use crate::{routing_observation::Record, routing_task_observation::Event};
use formless_runtime::LockExt;
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
const MAX_RECEIPTS: usize = 32;
#[derive(Debug, Clone)]
pub(crate) enum Receipt {
    Call(Record),
    Task(Event),
}
pub(crate) struct Recorder {
    queue: Mutex<VecDeque<Receipt>>,
    dropped: AtomicU64,
    serial: Arc<tokio::sync::Semaphore>,
}
pub(crate) struct Batch {
    pub receipts: Vec<Receipt>,
    pub dropped: u64,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl Default for Recorder {
    fn default() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            dropped: AtomicU64::new(0),
            serial: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}
impl Recorder {
    fn push(&self, receipt: Receipt) {
        let mut queue = self.queue.locked();
        if queue.len() < MAX_RECEIPTS {
            queue.push_back(receipt);
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn call_sink(self: &Arc<Self>) -> Arc<dyn Fn(Record) + Send + Sync> {
        let recorder = self.clone();
        Arc::new(move |record| recorder.push(Receipt::Call(record)))
    }
    pub fn task_sink(self: &Arc<Self>) -> Arc<dyn Fn(Event) + Send + Sync> {
        let recorder = self.clone();
        Arc::new(move |record| recorder.push(Receipt::Task(record)))
    }
    pub fn try_take(&self) -> Option<Batch> {
        let permit = self.serial.clone().try_acquire_owned().ok()?;
        let receipts = self.queue.locked().drain(..).collect();
        Some(Batch {
            receipts,
            dropped: self.dropped.swap(0, Ordering::Relaxed),
            _permit: permit,
        })
    }
    pub async fn take(&self) -> Batch {
        // Only one detached batch may exist. The permit is not a queue/index
        // mutex and may span Store I/O; producers continue to enqueue safely.
        let permit = self
            .serial
            .clone()
            .acquire_owned()
            .await
            .expect("routing recorder semaphore is never closed");
        let receipts = {
            let mut queue = self.queue.locked();
            queue.drain(..).collect()
        };
        Batch {
            receipts,
            dropped: self.dropped.swap(0, Ordering::Relaxed),
            _permit: permit,
        }
    }
    /// A failed transaction must not silently discard its detached batch. IDs
    /// make persistence idempotent; bounded overflow is reported as missing data.
    pub fn retry(&self, batch: Batch) {
        let mut queue = self.queue.locked();
        self.dropped.fetch_add(batch.dropped, Ordering::Relaxed);
        for receipt in batch.receipts.into_iter().rev() {
            if queue.len() < MAX_RECEIPTS {
                queue.push_front(receipt);
            } else {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing_observation::{Call, Neutral, Outcome};
    #[tokio::test]
    async fn sink_under_an_external_lock_only_queues_and_detached_batch_does_not_hold_it() {
        let recorder = Arc::new(Recorder::default());
        let outside = Mutex::new(());
        {
            let _guard = outside.locked();
            let call = Call::new(recorder.call_sink());
            call.finish(Outcome::Neutral(Neutral::Cancelled));
            drop(call);
        }
        let batch = recorder.take().await;
        assert_eq!(batch.receipts.len(), 1);
        assert!(recorder.queue.try_lock().is_ok());
        assert!(outside.try_lock().is_ok());
        // A persistence failure puts the original exact receipt back once.
        let id = match &batch.receipts[0] {
            Receipt::Call(record) => record.id.clone(),
            _ => unreachable!(),
        };
        recorder.retry(batch);
        let retry = recorder.take().await;
        assert!(matches!(&retry.receipts[0],Receipt::Call(record) if record.id==id));
    }
    #[tokio::test]
    async fn responsible_call_and_pending_owner_preserve_exact_role_task_until_final_receipt() {
        use crate::routing_observation::Identity;
        use crate::routing_task_observation::{self, Task, TaskOutcome};
        let recorder = Arc::new(Recorder::default());
        let task = Task::new("role", None, recorder.task_sink()).unwrap();
        let weak = Arc::downgrade(&task);
        let actual = Identity::new(
            "default",
            &crate::config::ProviderCfg::default(),
            "actual-served",
        );
        let pending = routing_task_observation::scope(Some(task.clone()), async {
            let call = Call::responsible_step(recorder.call_sink());
            let attempt = call.attempt(actual.clone());
            attempt.sent();
            attempt.status(200);
            attempt.returned();
            attempt.settle(
                Some(crate::provider::Usage {
                    prompt_tokens: 3,
                    reported: 1,
                    ..Default::default()
                }),
                true,
            );
            call.finish(Outcome::Success(actual));
            task.finish(TaskOutcome::Completed);
            drop(call);
            attempt
        })
        .await;
        drop(task);
        assert!(weak.upgrade().is_some());
        let first = recorder.take().await;
        assert_eq!(first.receipts.len(), 1);
        assert!(
            matches!(&first.receipts[0],Receipt::Task(Event::Closed(record)) if record.responsible.as_ref().unwrap().model=="actual-served")
        );
        drop(first);
        drop(pending);
        assert!(weak.upgrade().is_none());
        let final_batch = recorder.take().await;
        assert_eq!(final_batch.receipts.len(), 2);
        assert!(
            matches!(&final_batch.receipts[0],Receipt::Call(record) if record.task_ids.len()==1&&record.attempts[0].usage.unwrap().prompt_tokens==3)
        );
        assert!(
            matches!(&final_batch.receipts[1],Receipt::Task(Event::Final(record)) if record.expected_calls==1&&record.capture_complete)
        );
    }
    #[tokio::test]
    async fn queue_overflow_is_counted_and_never_fabricates_zero_cost_records() {
        let recorder = Arc::new(Recorder::default());
        for _ in 0..MAX_RECEIPTS + 1 {
            drop(Call::new(recorder.call_sink()));
        }
        let batch = recorder.take().await;
        assert_eq!(batch.receipts.len(), MAX_RECEIPTS);
        assert_eq!(batch.dropped, 1);
        drop(batch);
        assert!(recorder.take().await.receipts.is_empty());
    }
}
