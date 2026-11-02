//! A retry press belongs to one live provider attempt, never to a future one.
use super::*;

struct ActiveRequest {
    identity: Arc<()>,
    signal: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
pub(super) struct ProviderRetry {
    active: StdMutex<Option<ActiveRequest>>,
}

pub(super) struct RequestAttempt {
    owner: Arc<ProviderRetry>,
    identity: Arc<()>,
}

impl ProviderRetry {
    /// Publish the receiver before the request is polled. A press in that gap
    /// remains attached to this attempt even before select polls its receiver.
    pub(super) fn begin(self: &Arc<Self>) -> (RequestAttempt, oneshot::Receiver<()>) {
        let identity = Arc::new(());
        let (signal, receiver) = oneshot::channel();
        *self.active.locked() = Some(ActiveRequest {
            identity: identity.clone(),
            signal: Some(signal),
        });
        (
            RequestAttempt {
                owner: self.clone(),
                identity,
            },
            receiver,
        )
    }

    pub(super) fn request(&self) -> bool {
        let mut active = self.active.locked();
        let Some(request) = active.as_mut() else {
            return false;
        };
        let Some(signal) = request.signal.take() else {
            return false;
        };
        signal.send(()).is_ok()
    }
}

impl RequestAttempt {
    /// Completion and a retry press arbitrate under the same mutex. If both
    /// futures are ready, a true ACK still wins over accepting the old result.
    pub(super) fn finish(&mut self) -> bool {
        let mut active = self.owner.active.locked();
        if active
            .as_ref()
            .is_some_and(|request| Arc::ptr_eq(&request.identity, &self.identity))
        {
            return active.take().unwrap().signal.is_none();
        }
        false
    }
}

impl Drop for RequestAttempt {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_presses_do_not_retry_the_next_attempt() {
        let retry = Arc::new(ProviderRetry::default());
        assert!(!retry.request());
        let (mut attempt, mut signal) = retry.begin();
        assert_eq!(signal.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        assert!(!attempt.finish());
        assert!(!retry.request());
    }

    #[tokio::test]
    async fn press_before_first_poll_is_delivered_once_and_cannot_leak() {
        let retry = Arc::new(ProviderRetry::default());
        let (mut attempt, signal) = retry.begin();
        assert!(retry.request());
        assert!(!retry.request());
        signal.await.unwrap();
        assert!(attempt.finish());
        assert!(!retry.request());
        let (mut next, mut signal) = retry.begin();
        assert_eq!(signal.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        assert!(!next.finish());
    }

    #[test]
    fn completion_observes_claim_even_without_polling_the_signal() {
        let retry = Arc::new(ProviderRetry::default());
        let (mut attempt, _signal) = retry.begin();
        assert!(retry.request());
        assert!(attempt.finish());
        assert!(!retry.request());
    }

    #[test]
    fn old_attempt_drop_cannot_unregister_a_new_epoch() {
        let retry = Arc::new(ProviderRetry::default());
        let (old, _old_signal) = retry.begin();
        let (mut new, _new_signal) = retry.begin();
        drop(old);
        assert!(retry.request());
        assert!(new.finish());
    }

    #[tokio::test]
    async fn abort_unregisters_the_attempt() {
        let retry = Arc::new(ProviderRetry::default());
        let (ready, registered) = oneshot::channel();
        let task = tokio::spawn({
            let retry = retry.clone();
            async move {
                let (_attempt, _signal) = retry.begin();
                ready.send(()).unwrap();
                std::future::pending::<()>().await;
            }
        });
        registered.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!retry.request());
    }

    #[test]
    fn completion_and_press_never_acknowledge_a_lost_retry() {
        for _ in 0..32 {
            let retry = Arc::new(ProviderRetry::default());
            let (mut attempt, _signal) = retry.begin();
            let gate = Arc::new(std::sync::Barrier::new(2));
            let press = std::thread::spawn({
                let retry = retry.clone();
                let gate = gate.clone();
                move || {
                    gate.wait();
                    retry.request()
                }
            });
            gate.wait();
            let resend = attempt.finish();
            assert_eq!(press.join().unwrap(), resend);
            assert!(!retry.request());
        }
    }

    #[test]
    fn unwind_unregisters_the_attempt() {
        let retry = Arc::new(ProviderRetry::default());
        let result = std::panic::catch_unwind({
            let retry = retry.clone();
            move || {
                let (_attempt, _signal) = retry.begin();
                panic!("private request fixture");
            }
        });
        assert!(result.is_err());
        assert!(!retry.request());
    }
}
