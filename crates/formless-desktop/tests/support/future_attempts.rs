//! Opt-in producer protocol through real IPC and a private fake provider.
use super::*;
use formless_desktop::attempt_admission::{Attachment, Change, Gate};
use formless_protocol::{TranscriptAttemptChange as Phase, TranscriptBarrier};

async fn barrier(observer: &mut Observer, session: &str) -> (Attachment, Gate) {
    let value = observer
        .request(Request::GetTranscriptBarrier { id: session.into() })
        .await;
    assert_eq!(value["mode"], "future_attempts_only_v1");
    let snapshot =
        formless_desktop::replies::session_snapshot(session, &value["snapshot"]).unwrap();
    let barrier: TranscriptBarrier = serde_json::from_value(value["barrier"].clone()).unwrap();
    let tag = Attachment::default();
    let gate = Gate::new(tag.clone(), &snapshot, barrier).unwrap();
    (tag, gate)
}
fn feed(gate: &mut Gate, tag: &Attachment, events: &[Event]) -> (String, usize, usize) {
    let mut text = String::new();
    let mut starts = 0;
    let mut reloads = 0;
    for event in events {
        match gate.event(tag, event) {
            Change::Append(delta) => text.push_str(delta),
            Change::Started => starts += 1,
            Change::Reload => reloads += 1,
            _ => {}
        }
    }
    (text, starts, reloads)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_attempt_send_midrun_barrier_and_legacy_reset_have_real_wire_boundaries() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-future-attempt-key");
    let middle = Arc::new(Notify::new());
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture=Fixture::start(app(axum::routing::post({
        let middle=middle.clone();let complete=complete.clone();let calls=calls.clone();
        move || {let middle=middle.clone();let complete=complete.clone();
            let number=calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst)+1;
            async move {
                if number==2 {let gate=Arc::new(Notify::new());gate.notify_one();return gated_response(prefix("Legacy answer."),Some(tail("")),gate);}
                assert_eq!(number,1);
                let chunks=VecDeque::from([
                    (None,prefix("Fresh ไทย")),
                    (Some(middle),encode(&[json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" 😀"}})])),
                    (Some(complete),tail(" done")),
                ]);
                let stream=futures_util::stream::unfold(chunks,|mut chunks|async move {
                    let(gate,text)=chunks.pop_front()?;if let Some(gate)=gate {gate.notified().await;}
                    Some((Ok::<_,std::io::Error>(text),chunks))
                });
                axum::response::Response::builder().header("content-type","text/event-stream").body(axum::body::Body::from_stream(stream)).unwrap()
            }
        }
    })),0).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    let subscribed = observer
        .request(Request::SubscribeTranscriptAttempts {
            session_id: fixture.session.clone(),
        })
        .await;
    assert_eq!(subscribed["mode"], "future_attempts_only_v1");
    let (tag, mut gate) = barrier(&mut observer, &fixture.session).await;
    observer.events.clear();
    send(&mut observer, &fixture.session).await;
    wait(&mut observer,|event|matches!(event,Event::TranscriptAttempt{update} if matches!(&update.change,Phase::Delta{text,..} if text=="Fresh ไทย"))).await;
    assert_eq!(
        feed(&mut gate, &tag, &observer.events),
        ("Fresh ไทย".into(), 1, 0)
    );
    let mut processed = observer.events.len();
    let (mid_tag, mut mid_gate) = barrier(&mut observer, &fixture.session).await;
    assert_eq!(
        feed(&mut gate, &tag, &observer.events[processed..]),
        ("".into(), 0, 0)
    );
    processed = observer.events.len();
    middle.notify_one();
    wait(&mut observer,|event|matches!(event,Event::TranscriptAttempt{update} if matches!(&update.change,Phase::Delta{text,..} if text==" 😀"))).await;
    assert_eq!(
        feed(&mut gate, &tag, &observer.events[processed..]),
        (" 😀".into(), 0, 0)
    );
    assert_eq!(
        feed(&mut mid_gate, &mid_tag, &observer.events[processed..]),
        ("".into(), 0, 0),
        "no missing prefix is invented for an already active attempt"
    );
    processed = observer.events.len();
    complete.notify_one();
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    let tail = feed(&mut gate, &tag, &observer.events[processed..]);
    assert_eq!(tail, (" done".into(), 0, 1));
    assert_eq!(
        feed(&mut mid_gate, &mid_tag, &observer.events[processed..]),
        ("".into(), 0, 0)
    );
    assert!(
        !observer.events.iter().any(|event| matches!(
            event,
            Event::MessageDelta { .. }
                | Event::ReasoningDelta { .. }
                | Event::StreamRestarted { .. }
        )),
        "opt-in does not receive competing legacy chunks/resets"
    );
    let final_snapshot = snapshot(&mut observer, &fixture.session).await;
    assert_eq!(
        assistant_messages(&final_snapshot)[0]["content"],
        "Fresh ไทย 😀 done"
    );
    observer
        .request(Request::Subscribe {
            sessions: Some(vec![fixture.session.clone()]),
            deltas: true,
            client: None,
        })
        .await;
    observer.events.clear();
    send(&mut observer, &fixture.session).await;
    wait(
        &mut observer,
        |event| matches!(event,Event::MessageDelta{delta,..} if delta=="Legacy answer."),
    )
    .await;
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    assert!(
        !observer
            .events
            .iter()
            .any(|event| matches!(event, Event::TranscriptAttempt { .. })),
        "legacy Subscribe resets opt-in mode"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    drop(observer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_attempt_provider_retry_has_one_turn_new_attempt_and_a_complete_prefix() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-attempt-retry-key");
    let dropped = Arc::new(Notify::new());
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = Fixture::start(
        app(axum::routing::post({
            let dropped = dropped.clone();
            let complete = complete.clone();
            let calls = calls.clone();
            move || {
                let dropped = dropped.clone();
                let complete = complete.clone();
                let number = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                async move {
                    match number {
                        1 => gated_response(prefix("Discard old prefix."), None, dropped),
                        2 => gated_response(
                            prefix("Whole replacement ไทย "),
                            Some(tail("😀")),
                            complete,
                        ),
                        _ => panic!("unexpected request {number}"),
                    }
                }
            }
        })),
        1,
    )
    .await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    observer
        .request(Request::SubscribeTranscriptAttempts {
            session_id: fixture.session.clone(),
        })
        .await;
    send(&mut observer, &fixture.session).await;
    wait(&mut observer,|event|matches!(event,Event::TranscriptAttempt{update} if matches!(&update.change,Phase::Delta{text,..} if text=="Discard old prefix."))).await;
    let original = observer
        .events
        .iter()
        .find_map(|event| match event {
            Event::TranscriptAttempt { update }
                if matches!(&update.change, Phase::Started { .. }) =>
            {
                Some(update.clone())
            }
            _ => None,
        })
        .unwrap();
    let (tag, mut gate) = barrier(&mut observer, &fixture.session).await;
    let processed = observer.events.len();
    dropped.notify_one();
    wait(&mut observer,|event|matches!(event,Event::TranscriptAttempt{update} if matches!(&update.change,Phase::Delta{text,..} if text=="Whole replacement ไทย "))).await;
    assert_eq!(
        feed(&mut gate, &tag, &observer.events[processed..]),
        ("Whole replacement ไทย ".into(), 1, 0)
    );
    let replacement = observer.events[processed..]
        .iter()
        .find_map(|event| match event {
            Event::TranscriptAttempt { update }
                if matches!(&update.change, Phase::Started { .. }) =>
            {
                Some(update.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(original.turn_id, replacement.turn_id);
    assert_eq!(original.run_id, replacement.run_id);
    assert_ne!(original.attempt_id, replacement.attempt_id);
    let processed = observer.events.len();
    complete.notify_one();
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    assert_eq!(
        feed(&mut gate, &tag, &observer.events[processed..]),
        ("😀".into(), 0, 1)
    );
    let final_snapshot = snapshot(&mut observer, &fixture.session).await;
    assert_eq!(assistant_messages(&final_snapshot).len(), 1);
    assert_eq!(
        assistant_messages(&final_snapshot)[0]["content"],
        "Whole replacement ไทย 😀"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    drop(observer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_reader_negotiates_and_keeps_live_prefix_during_status_reads() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-reader-key");
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = Fixture::start(
        app(axum::routing::post({
            let complete = complete.clone();
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let complete = complete.clone();
                async move {
                    gated_response(prefix("Owned live ไทย "), Some(tail("😀")), complete)
                }
            }
        })),
        0,
    )
    .await;
    let client = Client::start_future_attempts_for_validation(fixture.endpoint.clone());
    client
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 1,
        })
        .unwrap();
    super::super::until(&client,"actual-reader-baseline",|update|matches!(update,Update::Transcript(batch) if batch.owner.generation==1&&batch.baseline.is_some())).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    send(&mut observer, &fixture.session).await;
    let (mut live, mut status) = (false, false);
    super::super::until(&client, "actual-reader-live-and-status", |update| {
        if let Update::Transcript(batch) = update {
            assert_eq!(batch.owner.session_id, fixture.session);
            assert_eq!(batch.owner.generation, 1);
            assert_eq!(batch.owner.connection, client.connection());
            assert_eq!(batch.owner.view_epoch, client.stream_epoch());
            if batch.events.iter().any(
                |event| matches!(event,Event::MessageDelta{delta,..} if delta=="Owned live ไทย "),
            ) {
                assert!(batch.status.is_none());
                live = true;
            }
            if batch.status.as_ref().is_some_and(|status| status.running) {
                assert!(batch.baseline.is_none());
                status = true;
            }
        }
        live && status
    })
    .await;
    complete.notify_one();
    let mut reloaded = false;
    super::super::until(&client, "actual-reader-final-canonical", |update| {
        if let Update::Transcript(batch) = update {
            reloaded |= batch.reload;
            if let Some(snapshot) = batch.baseline {
                if snapshot
                    .blocks
                    .iter()
                    .any(|block| block.markdown() == "Owned live ไทย 😀")
                {
                    assert!(
                        reloaded,
                        "live closure retires provisional text before canonical install"
                    );
                    assert_eq!(snapshot.blocks.len(), 2);
                    return true;
                }
            }
        }
        false
    })
    .await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(client);
    drop(observer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_reader_a_b_a_and_reconnect_never_revive_the_old_prefix_or_replay_work() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-reader-context-key");
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = Fixture::start(
        app(axum::routing::post({
            let complete = complete.clone();
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let complete = complete.clone();
                async move {
                    gated_response(
                        prefix("Original prefix "),
                        Some(tail("final ไทย 😀")),
                        complete,
                    )
                }
            }
        })),
        0,
    )
    .await;
    let other = fixture
        .daemon
        .handle(Request::CreateSession {
            name: Some("Other private Session".into()),
        })
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let client = Client::start_future_attempts_for_validation(fixture.endpoint.clone());
    client
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 1,
        })
        .unwrap();
    super::super::until(&client,"reader-context-initial",|update|matches!(update,Update::Transcript(batch) if batch.baseline.is_some()&&batch.owner.generation==1)).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    send(&mut observer, &fixture.session).await;
    super::super::until(&client,"reader-context-live",|update|matches!(update,Update::Transcript(batch) if batch.events.iter().any(|event|matches!(event,Event::MessageDelta{delta,..} if delta=="Original prefix ")))).await;
    let before = client.stream_epoch();
    client
        .send(Command::Select {
            session_id: Some(other),
            generation: 2,
        })
        .unwrap();
    client
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 3,
        })
        .unwrap();
    assert!(
        client.stream_epoch() > before,
        "view invalidation precedes worker command dispatch"
    );
    super::super::until(&client, "reader-context-returned-a", |update| {
        if let Update::Transcript(batch) = update {
            if batch.owner.generation == 3 && batch.owner.view_epoch == client.stream_epoch() {
                if let Some(snapshot) = batch.baseline {
                    assert_eq!(snapshot.blocks.len(), 1);
                    return true;
                }
            }
        }
        false
    })
    .await;
    let connection = client.connection();
    client.send(Command::Reconnect).unwrap();
    super::super::until(&client, "reader-context-reconnected", |update| {
        if let Update::Transcript(batch) = update {
            if batch.owner.generation == 3
                && batch.owner.connection != connection
                && batch.owner.connection == client.connection()
                && batch.owner.view_epoch == client.stream_epoch()
            {
                if let Some(snapshot) = batch.baseline {
                    assert_eq!(snapshot.blocks.len(), 1);
                    return true;
                }
            }
        }
        false
    })
    .await;
    complete.notify_one();
    super::super::until(&client,"reader-context-final",|update| {
        if let Update::Transcript(batch)=update {
            if batch.owner.connection==client.connection()&&batch.owner.view_epoch==client.stream_epoch()&&batch.owner.generation==3 {
                assert!(!batch.events.iter().any(|event|matches!(event,Event::MessageDelta{..})),"the old mid-attempt suffix is not an eligible prefix");
                if batch.events.iter().any(|event|matches!(event,Event::MessageComplete{message,..} if message.content=="Original prefix final ไทย 😀")) {return true;}
                if batch.baseline.as_ref().is_some_and(|snapshot|snapshot.blocks.iter().any(|block|block.markdown()=="Original prefix final ไทย 😀")) {return true;}
            }
        }false
    }).await;
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "navigation and reconnect never restart paid/provider work"
    );
    drop(client);
    drop(observer);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_reader_ui_queue_overflow_retires_old_frames_and_reconnects_without_work_replay() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-reader-overflow-key");
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture=Fixture::start(app(axum::routing::post({let complete=complete.clone();let calls=calls.clone();move|| {
        let complete=complete.clone();calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        async move {
            let mut chunks=prefix("x");for _ in 0..99 {chunks.push_str(&encode(&[json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}})]));}
            gated_response(chunks,Some(tail("done")),complete)
        }
    }})),0).await;
    let client = Client::start_future_attempts_for_validation(fixture.endpoint.clone());
    client
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 1,
        })
        .unwrap();
    super::super::until(
        &client,
        "reader-overflow-baseline",
        |update| matches!(update,Update::Transcript(batch) if batch.baseline.is_some()),
    )
    .await;
    let connection = client.connection();
    let epoch = client.stream_epoch();
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    send(&mut observer, &fixture.session).await;
    // Deliberately do not drain the bounded UI channel until it fails closed.
    let deadline = Instant::now() + Duration::from_secs(10);
    while client.connection() == connection {
        assert!(
            Instant::now() < deadline,
            "full UI channel did not retire its connection"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(client.stream_epoch() > epoch);
    let old = client
        .drain()
        .filter_map(|update| {
            if let Update::Transcript(batch) = update {
                Some(batch)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert!(!old.is_empty());
    assert!(
        old.iter().all(|batch| batch.owner.connection == connection
            && batch.owner.view_epoch != client.stream_epoch()),
        "queued old frames are visibly retired before view delivery"
    );
    super::super::until(&client, "reader-overflow-canonical-recovery", |update| {
        if let Update::Transcript(batch) = update {
            if batch.owner.connection == client.connection()
                && batch.owner.connection != connection
                && batch.owner.view_epoch == client.stream_epoch()
            {
                if let Some(snapshot) = batch.baseline {
                    assert_eq!(snapshot.blocks.len(), 1);
                    return true;
                }
            }
        }
        false
    })
    .await;
    complete.notify_one();
    let expected = format!("{}done", "x".repeat(100));
    super::super::until(&client,"reader-overflow-final",|update| {
        if let Update::Transcript(batch)=update {
            if batch.owner.connection==client.connection()&&batch.owner.view_epoch==client.stream_epoch() {
                assert!(!batch.events.iter().any(|event|matches!(event,Event::MessageDelta{..})),"recovery cannot resume the missing prefix");
                if batch.events.iter().any(|event|matches!(event,Event::MessageComplete{message,..} if message.content==expected)) {return true;}
                if batch.baseline.as_ref().is_some_and(|snapshot|snapshot.blocks.iter().any(|block|block.markdown()==expected)) {return true;}
            }
        }false
    }).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(client);
    drop(observer);
    fixture.finish().await;
}
