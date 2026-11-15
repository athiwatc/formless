//! Validation-reader live-text batching. Canonical/control updates remain ordered.
use super::*;
use tokio::time::Instant;

pub(super) const MAX_EVENTS: usize = 256;
pub(super) const MAX_BYTES: usize = 64 * 1024;
pub(super) const INTERVAL: Duration = Duration::from_millis(16);

struct Held {
    batch: StreamBatch,
    bytes: usize,
    started: Instant,
}
#[derive(Default)]
pub(super) struct Delivery {
    held: Option<Held>,
}

fn delta_size(batch: &StreamBatch) -> Option<usize> {
    if batch.baseline.is_some() || batch.status.is_some() || batch.reload || batch.events.is_empty()
    {
        return None;
    }
    let Event::MessageDelta {
        session_id, run_id, ..
    } = batch.events.first()?
    else {
        return None;
    };
    if session_id != &batch.owner.session_id {
        return None;
    }
    let mut size = 0usize;
    for event in &batch.events {
        let Event::MessageDelta {
            session_id: s,
            run_id: r,
            delta,
        } = event
        else {
            return None;
        };
        if s != session_id || r != run_id {
            return None;
        }
        size = size.checked_add(delta.len())?;
    }
    (size > 0).then_some(size)
}
fn send(tx: &SyncSender<Update>, update: Update) -> Result<(), String> {
    tx.try_send(update).map_err(|_| {
        "Desktop update queue unavailable; reconnecting to reload canonical state.".into()
    })
}
impl Delivery {
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.held.as_ref().map(|h| h.started + INTERVAL)
    }
    fn flush(&mut self, tx: &SyncSender<Update>) -> Result<(), String> {
        if let Some(held) = self.held.take() {
            send(tx, Update::Transcript(held.batch))?;
        }
        Ok(())
    }
    pub(super) fn flush_due(
        &mut self,
        tx: &SyncSender<Update>,
        now: Instant,
        owns: impl Fn(&StreamOwner) -> bool,
    ) -> Result<(), String> {
        if self.held.as_ref().is_some_and(|h| !owns(&h.batch.owner)) {
            self.held = None;
        }
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.flush(tx)?;
        }
        Ok(())
    }
    pub(super) fn before_io(
        &mut self,
        tx: &SyncSender<Update>,
        now: Instant,
        writes: bool,
        owns: impl Fn(&StreamOwner) -> bool,
    ) -> Result<(), String> {
        self.flush_due(tx, now, owns)?;
        // A new RPC write can block. Flush an admitted prefix first, before
        // the caller marks a queued mutation as possibly written.
        if writes {
            self.flush(tx)?;
        }
        Ok(())
    }
    fn offer(
        &mut self,
        tx: &SyncSender<Update>,
        update: Update,
        now: Instant,
        owns: impl Fn(&StreamOwner) -> bool,
    ) -> Result<(), String> {
        let update = match update {
            Update::Transcript(batch) => {
                if let Some(bytes) = delta_size(&batch) {
                    // Only the added delay is owned here. Retired live text is
                    // discarded; important reload/baseline updates still pass.
                    if !owns(&batch.owner) {
                        return Ok(());
                    }
                    if bytes > MAX_BYTES || batch.events.len() > MAX_EVENTS {
                        self.flush(tx)?;
                        return send(tx, Update::Transcript(batch));
                    }
                    let compatible=self.held.as_ref().is_some_and(|h| {
                        h.batch.owner==batch.owner &&
                        matches!((&h.batch.events[0],&batch.events[0]),(Event::MessageDelta{run_id:a,..},Event::MessageDelta{run_id:b,..}) if a==b) &&
                        h.batch.events.len()+batch.events.len()<=MAX_EVENTS &&
                        h.bytes.checked_add(bytes).is_some_and(|n|n<=MAX_BYTES)
                    });
                    if compatible {
                        let h = self.held.as_mut().unwrap();
                        h.bytes += bytes;
                        h.batch.events.extend(batch.events);
                    } else {
                        self.flush(tx)?;
                        self.held = Some(Held {
                            batch,
                            bytes,
                            started: now,
                        });
                    }
                    return Ok(());
                }
                Update::Transcript(batch)
            }
            other => other,
        };
        // A metadata, canonical, mutation or lifecycle boundary cannot overtake
        // preceding admitted text. Do not merge or delay that boundary itself.
        self.flush(tx)?;
        send(tx, update)
    }
    pub(super) fn drain(
        &mut self,
        tx: &SyncSender<Update>,
        engine: &mut Engine,
        now: Instant,
    ) -> Result<(), String> {
        self.flush_due(tx, now, |owner| engine.owns_stream(owner))?;
        // Keep the existing Vec allocation, and restore it even on queue failure.
        let mut updates = std::mem::take(&mut engine.updates);
        let result = updates
            .drain(..)
            .try_for_each(|u| self.offer(tx, u, now, |owner| engine.owns_stream(owner)));
        engine.updates = updates;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> StreamOwner {
        StreamOwner {
            client: uuid::Uuid::new_v4(),
            load: 1,
            connection: 2,
            view_epoch: 3,
            generation: 4,
            session_id: "s".into(),
            basis: uuid::Uuid::new_v4(),
        }
    }
    fn batch(owner: &StreamOwner, text: &str) -> StreamBatch {
        StreamBatch {
            owner: owner.clone(),
            baseline: None,
            events: vec![Event::MessageDelta {
                session_id: owner.session_id.clone(),
                run_id: "r".into(),
                delta: text.into(),
            }],
            reload: false,
            status: None,
        }
    }
    fn offer(d: &mut Delivery, tx: &SyncSender<Update>, b: StreamBatch, now: Instant) {
        d.offer(tx, Update::Transcript(b), now, |_| true).unwrap();
    }
    fn receive(rx: &Receiver<Update>) -> StreamBatch {
        match rx.try_recv().unwrap() {
            Update::Transcript(b) => b,
            _ => panic!("transcript expected"),
        }
    }
    fn texts(b: &StreamBatch) -> Vec<&str> {
        b.events
            .iter()
            .map(|e| match e {
                Event::MessageDelta { delta, .. } => delta.as_str(),
                _ => panic!("delta expected"),
            })
            .collect()
    }
    #[test]
    fn deadline_is_from_first_chunk_and_preserves_each_exact_utf8_event() {
        let (tx, rx) = mpsc::sync_channel(16);
        let mut d = Delivery::default();
        let o = owner();
        let now = Instant::now();
        offer(&mut d, &tx, batch(&o, "ก"), now);
        offer(
            &mut d,
            &tx,
            batch(&o, "😀\n"),
            now + Duration::from_millis(10),
        );
        assert_eq!(d.deadline(), Some(now + INTERVAL));
        d.flush_due(&tx, now + INTERVAL - Duration::from_millis(1), |_| true)
            .unwrap();
        assert!(rx.try_recv().is_err());
        d.flush_due(&tx, now + INTERVAL, |_| true).unwrap();
        let b = receive(&rx);
        assert_eq!(b.owner, o);
        assert_eq!(texts(&b), ["ก", "😀\n"]);
        assert!(d.deadline().is_none());
    }
    #[test]
    fn event_and_payload_bounds_flush_instead_of_dropping_or_growing() {
        let (tx, rx) = mpsc::sync_channel(16);
        let mut d = Delivery::default();
        let o = owner();
        let now = Instant::now();
        for _ in 0..MAX_EVENTS + 1 {
            offer(&mut d, &tx, batch(&o, "x"), now);
        }
        let b = receive(&rx);
        assert_eq!(b.events.len(), MAX_EVENTS);
        assert_eq!(texts(&b).concat(), "x".repeat(MAX_EVENTS));
        assert_eq!(d.held.as_ref().unwrap().batch.events.len(), 1);
        d.flush(&tx).unwrap();
        receive(&rx);
        offer(&mut d, &tx, batch(&o, &"a".repeat(MAX_BYTES)), now);
        offer(&mut d, &tx, batch(&o, "b"), now);
        assert_eq!(texts(&receive(&rx)).concat(), "a".repeat(MAX_BYTES));
        assert_eq!(d.held.as_ref().unwrap().bytes, 1);
        offer(&mut d, &tx, batch(&o, &"c".repeat(MAX_BYTES + 1)), now);
        assert_eq!(texts(&receive(&rx)), ["b"]);
        assert_eq!(texts(&receive(&rx)).concat(), "c".repeat(MAX_BYTES + 1));
        assert!(d.deadline().is_none());
    }
    #[test]
    fn all_owner_dimensions_and_run_changes_are_delivery_boundaries() {
        for field in [
            "client",
            "connection",
            "epoch",
            "generation",
            "session",
            "basis",
            "load",
            "run",
        ] {
            let (tx, rx) = mpsc::sync_channel(16);
            let mut d = Delivery::default();
            let o = owner();
            let now = Instant::now();
            offer(&mut d, &tx, batch(&o, "before"), now);
            let mut next = batch(&o, "after");
            match field {
                "client" => next.owner.client = uuid::Uuid::new_v4(),
                "connection" => next.owner.connection += 1,
                "epoch" => next.owner.view_epoch += 1,
                "generation" => next.owner.generation += 1,
                "session" => {
                    next.owner.session_id = "other".into();
                    if let Event::MessageDelta { session_id, .. } = &mut next.events[0] {
                        *session_id = "other".into();
                    }
                }
                "basis" => next.owner.basis = uuid::Uuid::new_v4(),
                "load" => next.owner.load += 1,
                _ => {
                    if let Event::MessageDelta { run_id, .. } = &mut next.events[0] {
                        *run_id = "other".into();
                    }
                }
            }
            offer(&mut d, &tx, next, now);
            assert_eq!(texts(&receive(&rx)), ["before"], "{field}");
            d.flush(&tx).unwrap();
            assert_eq!(texts(&receive(&rx)), ["after"]);
        }
    }
    #[test]
    fn status_reload_baseline_and_nontranscript_updates_never_overtake_text() {
        for boundary in ["status", "reload", "baseline", "notice"] {
            let (tx, rx) = mpsc::sync_channel(16);
            let mut d = Delivery::default();
            let o = owner();
            let now = Instant::now();
            offer(&mut d, &tx, batch(&o, "prefix"), now);
            let mut b = batch(&o, "");
            b.events.clear();
            match boundary {
                "status" => {
                    b.status = Some(StreamStatus {
                        steps: Default::default(),
                        step_mode: None,
                        running: true,
                        unfinished_run: None,
                    })
                }
                "reload" => b.reload = true,
                "baseline" => b.baseline = Some(
                    replies::session_snapshot(
                        "s",
                        &serde_json::json!({"session":{"id":"s"},"messages":[],"running":false}),
                    )
                    .unwrap(),
                ),
                _ => {}
            }
            let update = if boundary == "notice" {
                Update::Notice("control".into())
            } else {
                Update::Transcript(b)
            };
            d.offer(&tx, update, now, |_| true).unwrap();
            assert_eq!(texts(&receive(&rx)), ["prefix"]);
            match rx.try_recv().unwrap() {
                Update::Notice(s) => assert_eq!(s, "control"),
                Update::Transcript(b) => {
                    assert!(b.status.is_some() || b.reload || b.baseline.is_some())
                }
                _ => panic!("boundary"),
            };
            assert!(d.deadline().is_none());
        }
    }
    #[test]
    fn retirement_discards_only_held_live_text_and_keeps_reload_delivery() {
        let (tx, rx) = mpsc::sync_channel(16);
        let mut d = Delivery::default();
        let o = owner();
        let now = Instant::now();
        offer(&mut d, &tx, batch(&o, "retired"), now);
        d.flush_due(&tx, now + INTERVAL, |_| false).unwrap();
        assert!(rx.try_recv().is_err());
        d.offer(&tx, Update::Transcript(batch(&o, "late")), now, |_| false)
            .unwrap();
        assert!(d.deadline().is_none());
        let mut reload = batch(&o, "");
        reload.events.clear();
        reload.reload = true;
        d.offer(&tx, Update::Transcript(reload), now, |_| false)
            .unwrap();
        assert!(receive(&rx).reload);
    }
    #[test]
    fn queue_failure_is_fail_closed_and_does_not_retain_the_failed_payload() {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.try_send(Update::Notice("already full".into())).unwrap();
        let mut d = Delivery::default();
        let o = owner();
        let now = Instant::now();
        offer(&mut d, &tx, batch(&o, "pending"), now);
        assert!(d.flush_due(&tx, now + INTERVAL, |_| true).is_err());
        assert!(d.deadline().is_none());
        assert!(matches!(rx.try_recv(), Ok(Update::Notice(_))));
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn ordinary_production_updates_are_immediate_and_keep_their_order() {
        let (tx, rx) = mpsc::sync_channel(16);
        let mut d = Delivery::default();
        let mut e = Engine::default();
        e.updates.push(Update::Notice("first".into()));
        e.updates.push(Update::Connection {
            ready: true,
            notice: "second".into(),
        });
        d.drain(&tx, &mut e, Instant::now()).unwrap();
        assert!(matches!(rx.try_recv(),Ok(Update::Notice(s)) if s=="first"));
        assert!(matches!(
            rx.try_recv(),
            Ok(Update::Connection { ready: true, .. })
        ));
        assert!(d.deadline().is_none());
        assert!(e.updates.is_empty());
    }
}

#[cfg(test)]
mod burst_regression {
    use super::*;
    #[test]
    fn admitted_microburst_fits_the_ui_channel_without_losing_source_events() {
        let owner = StreamOwner {
            client: uuid::Uuid::new_v4(),
            load: 1,
            connection: 1,
            view_epoch: 1,
            generation: 1,
            session_id: "s".into(),
            basis: uuid::Uuid::new_v4(),
        };
        let update = |index| {
            Update::Transcript(StreamBatch {
                owner: owner.clone(),
                baseline: None,
                events: vec![Event::MessageDelta {
                    session_id: "s".into(),
                    run_id: "r".into(),
                    delta: format!("{index}:ก😀"),
                }],
                reload: false,
                status: None,
            })
        };
        let (old_tx, _old_rx) = mpsc::sync_channel(QUEUE);
        let mut old = Engine::default();
        for i in 0..100 {
            old.updates.push(update(i));
        }
        assert!(
            deliver(&old_tx, &mut old).is_err(),
            "per-chunk delivery saturates the 16-slot channel"
        );
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let mut delivery = Delivery::default();
        let now = Instant::now();
        for i in 0..100 {
            delivery.offer(&tx, update(i), now, |_| true).unwrap();
        }
        delivery.flush_due(&tx, now + INTERVAL, |_| true).unwrap();
        let Update::Transcript(batch) = rx.try_recv().unwrap() else {
            panic!("batched transcript")
        };
        assert_eq!(batch.events.len(), 100);
        for (i, event) in batch.events.into_iter().enumerate() {
            assert!(matches!(event,Event::MessageDelta{delta,..} if delta==format!("{i}:ก😀")));
        }
        assert!(rx.try_recv().is_err());
    }
}

#[cfg(test)]
mod writer_boundary {
    use super::*;
    #[test]
    fn held_prefix_is_flushed_before_an_outgoing_write_even_before_deadline() {
        let o = StreamOwner {
            client: uuid::Uuid::new_v4(),
            load: 1,
            connection: 1,
            view_epoch: 1,
            generation: 1,
            session_id: "s".into(),
            basis: uuid::Uuid::new_v4(),
        };
        let now = Instant::now();
        let (tx, rx) = mpsc::sync_channel(1);
        let mut d = Delivery::default();
        d.offer(
            &tx,
            Update::Transcript(StreamBatch {
                owner: o,
                baseline: None,
                events: vec![Event::MessageDelta {
                    session_id: "s".into(),
                    run_id: "r".into(),
                    delta: "prefix".into(),
                }],
                reload: false,
                status: None,
            }),
            now,
            |_| true,
        )
        .unwrap();
        d.before_io(&tx, now, false, |_| true).unwrap();
        assert!(rx.try_recv().is_err());
        d.before_io(&tx, now, true, |_| true).unwrap();
        assert!(matches!(rx.try_recv(), Ok(Update::Transcript(_))));
        assert!(d.deadline().is_none());
    }
}
