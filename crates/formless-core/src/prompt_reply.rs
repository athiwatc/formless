//! Delivery and cancellation share the answer handler's registry mutex.
//! A delivered reply cannot be lost merely because cancellation is ready too.
use super::*;

pub(super) async fn wait<T>(
    registry: &StdMutex<HashMap<String, oneshot::Sender<T>>>,
    id: &str,
    reply: oneshot::Receiver<T>,
    cancel: &CancellationToken,
    before_cancel_claim: impl FnOnce(),
) -> Option<T> {
    wait_until(registry, id, reply, cancel.cancelled(), before_cancel_claim).await
}

pub(super) async fn wait_until<T>(
    registry: &StdMutex<HashMap<String, oneshot::Sender<T>>>,
    id: &str,
    mut reply: oneshot::Receiver<T>,
    interrupted: impl std::future::Future<Output = ()>,
    before_cancel_claim: impl FnOnce(),
) -> Option<T> {
    let received = tokio::select! {
        // Cancellation deliberately gets the ready branch in the both-ready
        // case. The registry/drain below, rather than select priority, decides
        // the winner and preserves an already delivered/acknowledged answer.
        biased;
        _ = interrupted => {
            before_cancel_claim();
            let mut waiters = registry.locked();
            // Handlers remove AND send while holding this same mutex. Either
            // their reply is now queued, or removing this sender prevents any
            // later handler from successfully acknowledging a delivery.
            waiters.remove(id);
            reply.try_recv().ok()
        },
        received = &mut reply => received.ok(),
    };
    registry.locked().remove(id);
    received
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queued_reply_wins_when_cancel_is_already_ready() {
        let (tx, rx) = oneshot::channel();
        let registry = StdMutex::new(HashMap::from([("prompt".into(), tx)]));
        let cancel = CancellationToken::new();
        assert!(registry.locked().remove("prompt").unwrap().send(17).is_ok());
        cancel.cancel();
        let reached_cancel = std::cell::Cell::new(false);
        assert_eq!(
            wait(&registry, "prompt", rx, &cancel, || reached_cancel
                .set(true))
            .await,
            Some(17)
        );
        assert!(
            reached_cancel.get(),
            "exercise the drain, not the reply select arm"
        );
        assert!(registry.locked().is_empty());
    }

    #[tokio::test]
    async fn answer_between_cancel_selection_and_registry_claim_is_not_lost() {
        let (tx, rx) = oneshot::channel();
        let registry = StdMutex::new(HashMap::from([("prompt".into(), tx)]));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let answer_acknowledged = std::cell::Cell::new(false);
        let result = wait(&registry, "prompt", rx, &cancel, || {
            // Same lock/remove/send transaction as the public answer handler,
            // precisely after cancellation selected but before it claims.
            answer_acknowledged.set(registry.locked().remove("prompt").unwrap().send(23).is_ok());
        })
        .await;
        assert!(answer_acknowledged.get());
        assert_eq!(result, Some(23));
        assert!(registry.locked().is_empty());
    }

    #[tokio::test]
    async fn cancel_claim_removes_sender_before_any_late_answer_can_acknowledge() {
        let (tx, rx) = oneshot::channel::<usize>();
        let registry = StdMutex::new(HashMap::from([("prompt".into(), tx)]));
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(wait(&registry, "prompt", rx, &cancel, || {}).await, None);
        assert!(registry.locked().remove("prompt").is_none());
    }
}
