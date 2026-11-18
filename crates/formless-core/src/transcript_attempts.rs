//! Future-attempt correlation. No prefix retention or canonical replay journal.
//! Lock order: per-attempt -> producer clock -> subscribers -> event filter.
//! A barrier takes only the clock AFTER GetSession releases the store lock.
use super::*;

fn has_reader(subs: &Subs, session: &str) -> bool {
    subs.locked().iter().any(|sub| {
        let filter = sub.filter.locked();
        filter.transcript_attempts
            && filter
                .sessions
                .as_ref()
                .is_none_or(|sessions| sessions.contains(session))
    })
}

pub(super) struct Clock(StdMutex<TranscriptBarrier>);
impl Default for Clock {
    fn default() -> Self {
        Self(StdMutex::new(TranscriptBarrier {
            epoch: uuid::Uuid::new_v4().to_string(),
            cursor: 0,
        }))
    }
}
impl Clock {
    pub(super) fn barrier(&self) -> TranscriptBarrier {
        self.0.locked().clone()
    }
    fn emit(&self, subs: &Subs, state: &State, attempt: &str, change: TranscriptAttemptChange) {
        let mut clock = self.0.locked();
        // Never wrap within an epoch. Existing consumers must retire on the
        // changed epoch; a new barrier can only admit a later fresh start.
        if clock.cursor == u64::MAX {
            clock.epoch = uuid::Uuid::new_v4().to_string();
            clock.cursor = 0;
        }
        clock.cursor += 1;
        send_all(
            subs,
            Event::TranscriptAttempt {
                update: TranscriptAttemptEvent {
                    epoch: clock.epoch.clone(),
                    cursor: clock.cursor,
                    session_id: state.session.clone(),
                    run_id: state.run.clone(),
                    turn_id: state.turn.clone(),
                    attempt_id: attempt.into(),
                    change,
                },
            },
        );
    }
}
struct State {
    clock: Arc<Clock>,
    subs: Subs,
    session: String,
    run: String,
    turn: String,
    base_seq: i64,
    active: Option<String>,
    bytes: u64,
    enabled: bool,
    visible: bool,
    // Owner return/drop is final; a provider-attempt close alone is not.
    retired: bool,
}
impl State {
    fn start(&mut self) {
        if self.retired || !self.enabled {
            return;
        }
        self.bytes = 0;
        let attempt = uuid::Uuid::new_v4().to_string();
        self.clock.emit(
            &self.subs,
            self,
            &attempt,
            TranscriptAttemptChange::Started {
                base_seq: self.base_seq,
            },
        );
        self.active = Some(attempt);
    }
    fn retire(&mut self, reason: TranscriptAttemptClose) {
        if self.retired {
            return;
        }
        // Linearize retirement before publishing the final close. Retained
        // callback sinks cannot enable, restart or append after this point.
        self.retired = true;
        self.enabled = false;
        self.close(reason);
    }
    fn close(&mut self, reason: TranscriptAttemptClose) {
        if let Some(attempt) = self.active.take() {
            self.clock.emit(
                &self.subs,
                self,
                &attempt,
                TranscriptAttemptChange::Closed {
                    bytes: self.bytes,
                    reason,
                },
            );
        }
    }
}
#[derive(Clone)]
pub(super) struct Sink(Arc<StdMutex<State>>);
impl Sink {
    pub(super) fn delta(&self, text: &str) {
        let mut state = self.0.locked();
        if state.retired {
            return;
        }
        let Some(attempt) = state.active.clone() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let Some(next) = state.bytes.checked_add(text.len() as u64) else {
            state.close(TranscriptAttemptClose::Dropped);
            state.enabled = false;
            return;
        };
        state.clock.emit(
            &state.subs,
            &state,
            &attempt,
            TranscriptAttemptChange::Delta {
                offset: state.bytes,
                text: text.into(),
            },
        );
        state.bytes = next;
    }
    pub(super) fn restart(&self) {
        let mut state = self.0.locked();
        if state.retired {
            return;
        }
        state.close(TranscriptAttemptClose::Restarted);
        // A peer that opted in after the old attempt began may admit this
        // fresh retry, but never receive the old attempt's missing prefix.
        if !state.enabled && state.visible {
            let wanted = has_reader(&state.subs, &state.session);
            state.enabled = wanted;
        }
        state.start();
    }
}
/// Only this owner closes on drop. Cloned callback sinks cannot prolong an
/// abandoned attempt or publish after its owner closes it.
pub(super) struct Attempt(Sink);
impl Attempt {
    pub(super) fn start(
        clock: Arc<Clock>,
        subs: Subs,
        session: &str,
        run: &str,
        turn: &str,
        base_seq: i64,
        visible: bool,
    ) -> Self {
        // No per-chunk allocation or subscriber scan for legacy-only Runs.
        // A subscriber joining mid-attempt waits for a future fresh attempt.
        let enabled = visible && has_reader(&subs, session);
        let mut state = State {
            clock,
            subs,
            session: session.into(),
            run: run.into(),
            turn: turn.into(),
            base_seq,
            active: None,
            bytes: 0,
            enabled,
            visible,
            retired: false,
        };
        state.start();
        Self(Sink(Arc::new(StdMutex::new(state))))
    }
    pub(super) fn sink(&self) -> Sink {
        self.0.clone()
    }
    pub(super) fn returned(&mut self) {
        self.0 .0.locked().retire(TranscriptAttemptClose::Returned);
    }
}
impl Drop for Attempt {
    fn drop(&mut self) {
        self.0 .0.locked().retire(TranscriptAttemptClose::Dropped);
    }
}

// Shared synchronous read: do not recursively construct the large async
// request handler just to obtain the existing canonical snapshot.
impl Daemon {
    pub(super) fn transcript_snapshot(&self, id: String) -> Result<Value> {
        let (session, messages, queued, last, latest_run, last_recap, running, steerable_run_id) =
            self.with_store(|st| {
                st.read_transaction(|st| {
                    let s = st
                        .get_session(&id)?
                        .filter(|_| !st.session_deleted(&id).unwrap_or(false))
                        .context(RequestFailure::not_found("session not found"))?;
                    let m = st.list_messages(&id)?;
                    let q = (st.queued_items(&id)?, st.session_plan_snapshot(&id)?);
                    let l = st.unfinished_last_run(&id)?;
                    let latest = st
                        .last_run(&id)?
                        .map(|(rid, _)| st.inspect_run(&rid))
                        .transpose()?
                        .flatten();
                    let last_recap = st.latest_recorded_run_recap(&id)?;
                    // Hold the writer mutex through this sample: terminal
                    // capture commits its recap before releasing controls.
                    #[cfg(test)]
                    if let Some(hook) = self.session_snapshot_test.locked().clone() {
                        hook();
                    }
                    let runs = self.runs.locked();
                    let control = runs.get(&id);
                    let running = control.is_some() || self.session_is_publishing(&id);
                    let steerable_run_id = control
                        .and_then(reviewed_send::steerable_run_id)
                        .map(str::to_string);
                    Ok((s, m, q, l, latest, last_recap, running, steerable_run_id))
                })
            })?;
        // An unfinished last Run (interrupted/paused) gets a banner
        // with Resume / Abandon in clients (SPEC §5.4).
        let last_run = last.filter(|_| !running).map(
            |(run_id, status, error)| json!({"run_id": run_id, "status": status, "error": error}),
        );
        // `queued`: the texts (as before); `queued_items` adds ids
        // for editing.
        let (queued, (plan, plan_state)) = queued;
        let texts: Vec<&str> = queued.iter().map(|q| q.content.as_str()).collect();
        Ok(
            json!({"session": session, "messages": messages, "queued": texts, "queued_items": queued, "plan": plan, "plan_state": plan_state, "last_recap": last_recap, "step_mode": self.step_sessions.locked().contains(&id), "unfinished_run": last_run, "running": running, "steerable_run_id": steerable_run_id,
                "latest_run": latest_run, "pending_events": self.pending_attention.locked().clone(), "pending_steps": self.pending_steps_snapshot(&id) }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn subscriber(
        capacity: usize,
        opted: bool,
    ) -> (Subs, mpsc::Receiver<ServerMessage>, CancellationToken) {
        let (tx, rx) = mpsc::channel(capacity);
        let kick = CancellationToken::new();
        let filter = EventFilter {
            sessions: None,
            deltas: !opted,
            transcript_attempts: opted,
            label: None,
        };
        (
            Arc::new(StdMutex::new(vec![Subscriber {
                conn: 1,
                tx: tx.into(),
                filter: Arc::new(StdMutex::new(filter)),
                kick: kick.clone(),
                gaps: BTreeMap::new(),
            }])),
            rx,
            kick,
        )
    }
    fn update(rx: &mut mpsc::Receiver<ServerMessage>) -> TranscriptAttemptEvent {
        match rx.try_recv().unwrap() {
            ServerMessage::Event(Event::TranscriptAttempt { update }) => update,
            other => panic!("unexpected {other:?}"),
        }
    }
    #[test]
    fn starts_precede_deltas_barriers_exclude_delayed_starts_and_return_is_not_persistence() {
        let clock = Arc::new(Clock::default());
        let (subs, mut rx, _) = subscriber(8, true);
        let mut attempt = Attempt::start(clock.clone(), subs, "s", "r", "t", 7, true);
        let barrier = clock.barrier();
        let start = update(&mut rx);
        assert_eq!(start.cursor, barrier.cursor);
        assert_eq!(start.epoch, barrier.epoch);
        assert_eq!(
            start.change,
            TranscriptAttemptChange::Started { base_seq: 7 }
        );
        attempt.sink().delta("ไทย😀");
        attempt.sink().delta("x");
        attempt.returned();
        let a = update(&mut rx);
        let b = update(&mut rx);
        let closed = update(&mut rx);
        assert_eq!(
            a.change,
            TranscriptAttemptChange::Delta {
                offset: 0,
                text: "ไทย😀".into()
            }
        );
        assert_eq!(
            b.change,
            TranscriptAttemptChange::Delta {
                offset: 13,
                text: "x".into()
            }
        );
        assert_eq!(
            closed.change,
            TranscriptAttemptChange::Closed {
                bytes: 14,
                reason: TranscriptAttemptClose::Returned
            }
        );
        for event in [&a, &b, &closed] {
            assert_eq!(event.attempt_id, start.attempt_id);
            assert_eq!(event.turn_id, start.turn_id);
        }
        assert!(start.cursor < a.cursor && a.cursor < b.cursor && b.cursor < closed.cursor);
        attempt.sink().delta("late");
        drop(attempt);
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn restart_rotates_attempt_identity_and_drop_closes_exactly_once() {
        let clock = Arc::new(Clock::default());
        let (subs, mut rx, _) = subscriber(12, true);
        let attempt = Attempt::start(clock, subs, "s", "r", "turn", 1, true);
        let old = update(&mut rx);
        let sink = attempt.sink();
        sink.delta("old");
        update(&mut rx);
        sink.restart();
        let closed = update(&mut rx);
        let fresh = update(&mut rx);
        assert_eq!(closed.attempt_id, old.attempt_id);
        assert_ne!(fresh.attempt_id, old.attempt_id);
        assert_eq!(fresh.turn_id, old.turn_id);
        assert_eq!(fresh.run_id, old.run_id);
        sink.delta("new");
        assert_eq!(
            update(&mut rx).change,
            TranscriptAttemptChange::Delta {
                offset: 0,
                text: "new".into()
            }
        );
        drop(attempt);
        let closed = update(&mut rx);
        assert_eq!(closed.attempt_id, fresh.attempt_id);
        assert!(matches!(
            closed.change,
            TranscriptAttemptChange::Closed {
                reason: TranscriptAttemptClose::Dropped,
                ..
            }
        ));
        sink.delta("late callback");
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn late_restart_after_return_or_drop_cannot_revive_a_retained_sink() {
        for initially_opted in [false, true] {
            for returned in [false, true] {
                let clock = Arc::new(Clock::default());
                let (subs, mut rx, _) = subscriber(32, initially_opted);
                let mut owner = Some(Attempt::start(
                    clock.clone(),
                    subs.clone(),
                    "s",
                    "r",
                    "turn",
                    1,
                    true,
                ));
                let sink = owner.as_ref().unwrap().sink();
                sink.delta("before retirement");
                if returned {
                    owner.as_mut().unwrap().returned();
                } else {
                    drop(owner.take());
                }
                while rx.try_recv().is_ok() {}
                let retired_at = clock.barrier();
                // Retirement must remain final even if a new subscriber joins.
                subs.locked()[0].filter.locked().transcript_attempts = true;
                for _ in 0..3 {
                    sink.restart();
                    sink.delta("late callback");
                }
                assert_eq!(
                    clock.barrier(),
                    retired_at,
                    "retained sink revived: initially_opted={initially_opted}, returned={returned}"
                );
                assert!(rx.try_recv().is_err());
            }
        }
    }

    #[test]
    fn callback_thread_released_after_owner_retirement_remains_inert() {
        for returned in [false, true] {
            let clock = Arc::new(Clock::default());
            let (subs, mut rx, _) = subscriber(32, true);
            let mut owner = Some(Attempt::start(
                clock.clone(),
                subs,
                "s",
                "r",
                "turn",
                1,
                true,
            ));
            let sink = owner.as_ref().unwrap().sink();
            let ready = Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|scope| {
                let worker_ready = ready.clone();
                let worker = scope.spawn(move || {
                    worker_ready.wait();
                    for _ in 0..20 {
                        sink.restart();
                        sink.delta("late background callback");
                    }
                });
                if returned {
                    owner.as_mut().unwrap().returned();
                } else {
                    drop(owner.take());
                }
                while rx.try_recv().is_ok() {}
                let retired_at = clock.barrier();
                ready.wait();
                worker.join().unwrap();
                assert_eq!(clock.barrier(), retired_at);
                assert!(rx.try_recv().is_err());
            });
        }
    }

    #[test]
    fn legacy_and_schema_hidden_candidates_do_not_publish_the_opt_in_path() {
        for (opted, visible) in [(false, true), (true, false)] {
            let clock = Arc::new(Clock::default());
            let (subs, mut rx, _) = subscriber(8, opted);
            let before = clock.barrier();
            let attempt = Attempt::start(clock.clone(), subs, "s", "r", "t", 1, visible);
            attempt.sink().delta("private candidate");
            attempt.sink().restart();
            drop(attempt);
            assert!(rx.try_recv().is_err());
            assert_eq!(clock.barrier(), before);
        }
        let clock = Arc::new(Clock::default());
        let (subs, mut rx, _) = subscriber(8, true);
        subs.locked()[0].filter.locked().sessions = Some(std::collections::HashSet::from([
            "another-session".to_string(),
        ]));
        let attempt = Attempt::start(clock.clone(), subs, "s", "r", "turn", 0, true);
        attempt.sink().delta("not subscribed");
        drop(attempt);
        assert!(rx.try_recv().is_err());
        assert_eq!(clock.barrier().cursor, 0);
        let filter = EventFilter::default();
        let frame = Event::TranscriptAttempt {
            update: TranscriptAttemptEvent {
                epoch: "e".into(),
                cursor: 1,
                session_id: "s".into(),
                run_id: "r".into(),
                turn_id: "t".into(),
                attempt_id: "a".into(),
                change: TranscriptAttemptChange::Started { base_seq: 0 },
            },
        };
        assert!(!filter.wants(&frame));
    }
    #[test]
    fn loss_is_scoped_and_the_close_includes_missing_tail_bytes() {
        let clock = Arc::new(Clock::default());
        let (subs, mut rx, kick) = subscriber(2, true);
        let mut attempt = Attempt::start(clock, subs.clone(), "s", "r", "t", 0, true);
        attempt.sink().delta("kept");
        attempt.sink().delta("lost");
        update(&mut rx);
        update(&mut rx);
        attempt.returned();
        assert!(
            matches!(rx.try_recv().unwrap(),ServerMessage::Event(Event::DeltasDropped{session_id,run_id,kinds,..}) if session_id=="s"&&run_id=="r"&&kinds==vec![DroppedEventKind::MessageDelta])
        );
        assert!(matches!(
            update(&mut rx).change,
            TranscriptAttemptChange::Closed { bytes: 8, .. }
        ));
        assert!(!kick.is_cancelled());
    }
    #[test]
    fn an_undeliverable_boundary_disconnects_instead_of_silent_loss() {
        let clock = Arc::new(Clock::default());
        let (subs, _rx, kick) = subscriber(1, true);
        let attempt = Attempt::start(clock, subs.clone(), "s", "r", "t", 0, true);
        drop(attempt);
        assert!(kick.is_cancelled());
        assert!(subs.locked().is_empty());
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_start_between_storage_read_and_barrier_sample_is_excluded() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let root = std::env::temp_dir().join(format!("fl-barrier-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let id = daemon
            .handle(Request::CreateSession {
                name: Some("Private barrier fixture".into()),
            })
            .await
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (subs, mut rx, _) = subscriber(8, true);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = StdMutex::new(release_rx);
        *daemon.session_snapshot_test.locked() = Some(Arc::new(move || {
            entered_tx.send(()).unwrap();
            release_rx
                .locked()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }));
        let loading = tokio::spawn({
            let daemon = daemon.clone();
            let id = id.clone();
            async move {
                daemon
                    .handle(Request::GetTranscriptBarrier { id })
                    .await
                    .unwrap()
            }
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // This does not need the store lock held by the snapshot hook.
        let attempt = Attempt::start(
            daemon.transcript_attempts.clone(),
            subs,
            &id,
            "r",
            "t",
            0,
            true,
        );
        let start = update(&mut rx);
        release_tx.send(()).unwrap();
        let reply = loading.await.unwrap();
        let barrier: TranscriptBarrier = serde_json::from_value(reply["barrier"].clone()).unwrap();
        assert_eq!(barrier.epoch, start.epoch);
        assert!(start.cursor <= barrier.cursor);
        assert_eq!(reply["snapshot"]["messages"], json!([]));
        *daemon.session_snapshot_test.locked() = None;
        drop(attempt);
        drop(daemon);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn joining_mid_attempt_waits_for_a_fresh_retry_start() {
        let clock = Arc::new(Clock::default());
        let (subs, mut rx, _) = subscriber(8, false);
        let attempt = Attempt::start(clock.clone(), subs.clone(), "s", "r", "turn", 1, true);
        attempt.sink().delta("unseen prefix");
        {
            let subscribers = subs.locked();
            let mut filter = subscribers[0].filter.locked();
            filter.transcript_attempts = true;
            filter.deltas = false;
        }
        let barrier = clock.barrier();
        attempt.sink().delta("unseen suffix");
        assert!(rx.try_recv().is_err());
        attempt.sink().restart();
        let start = update(&mut rx);
        assert!(start.cursor > barrier.cursor);
        assert!(matches!(
            start.change,
            TranscriptAttemptChange::Started { .. }
        ));
        attempt.sink().delta("whole new prefix");
        assert!(matches!(
            update(&mut rx).change,
            TranscriptAttemptChange::Delta { offset: 0, .. }
        ));
    }

    #[test]
    fn epoch_changes_instead_of_wrapping_a_cursor() {
        let clock = Arc::new(Clock::default());
        clock.0.locked().cursor = u64::MAX;
        let before = clock.barrier();
        let (subs, mut rx, _) = subscriber(4, true);
        let attempt = Attempt::start(clock.clone(), subs, "s", "r", "t", 0, true);
        let start = update(&mut rx);
        assert_ne!(before.epoch, start.epoch);
        assert_eq!(start.cursor, 1);
        drop(attempt);
    }
    #[test]
    fn concurrent_publishers_are_serialized_in_producer_cursor_order() {
        let clock = Arc::new(Clock::default());
        let (subs, mut rx, _) = subscriber(256, true);
        std::thread::scope(|scope| {
            for number in 0..4 {
                let clock = clock.clone();
                let subs = subs.clone();
                scope.spawn(move || {
                    let attempt =
                        Attempt::start(clock, subs, "s", &format!("r{number}"), "turn", 0, true);
                    for _ in 0..20 {
                        attempt.sink().delta("x");
                    }
                });
            }
        });
        let mut previous = 0;
        let mut count = 0;
        while let Ok(ServerMessage::Event(Event::TranscriptAttempt { update })) = rx.try_recv() {
            assert!(update.cursor > previous);
            previous = update.cursor;
            count += 1;
        }
        assert_eq!(count, 88);
        assert_eq!(clock.barrier().cursor, 88);
    }
}
