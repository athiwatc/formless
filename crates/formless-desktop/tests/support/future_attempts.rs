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
