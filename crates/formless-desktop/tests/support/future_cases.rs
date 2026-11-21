//! Additional validation-only schedules. No production enablement or GUI.
use super::*;

pub(super) async fn reader(fixture: &Fixture) -> Client {
    let client = Client::start_future_attempts_for_validation(fixture.endpoint.clone());
    client
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 1,
        })
        .unwrap();
    until(
        &client,
        "future-case-baseline",
        |update| matches!(update, Update::Transcript(batch) if batch.baseline.is_some()),
    )
    .await;
    client
}

pub(super) async fn live(client: &Client, text: &str) {
    until(client, "future-case-live-prefix", |update| {
        matches!(update,Update::Transcript(batch) if batch.events.iter().any(|event|
            matches!(event,Event::MessageDelta{delta,..} if delta==text)))
    })
    .await;
}

pub(super) async fn canonical(client: &Client, text: &str, forbidden: &[&str]) {
    until(client, "future-case-canonical-final", |update| {
        let check = |content: &str| {
            for private in forbidden {
                assert!(
                    !content.contains(private),
                    "hidden candidate escaped: {private}"
                );
            }
        };
        match update {
            Update::Transcript(batch) => {
                assert_eq!(batch.owner.connection, client.connection());
                assert_eq!(batch.owner.view_epoch, client.stream_epoch());
                let mut found = false;
                for event in batch.events {
                    match event {
                        Event::MessageDelta { delta, .. } => check(&delta),
                        Event::MessageComplete { message, .. }
                        | Event::MessageAppended { message, .. } => {
                            check(&message.content);
                            found |= message.role == "assistant" && message.content == text;
                        }
                        _ => {}
                    }
                }
                if let Some(snapshot) = batch.baseline {
                    for block in snapshot.blocks {
                        check(block.markdown());
                        found |= block.markdown() == text;
                    }
                }
                found
            }
            // Conservative canonical fallback is permitted; no provisional
            // prefix or mutation is reconstructed to pass this check.
            Update::Snapshot { snapshot, .. } => {
                snapshot
                    .blocks
                    .iter()
                    .for_each(|block| check(block.markdown()));
                snapshot.blocks.iter().any(|block| block.markdown() == text)
            }
            _ => false,
        }
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_schema_repair_never_exposes_unvalidated_candidates() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-future-schema-key");
    let calls: Arc<Mutex<Vec<Value>>> = Arc::default();
    let fixture = Fixture::start(
        app(axum::routing::post({
            let calls = calls.clone();
            move |axum::Json(body): axum::Json<Value>| {
                let calls = calls.clone();
                async move {
                    let number = {
                        let mut calls = calls.lock().unwrap();
                        calls.push(body);
                        calls.len()
                    };
                    match number {
                        1 => {
                            let gate = Arc::new(Notify::new());
                            gate.notify_one();
                            gated_response(
                                prefix("INVALID private schema candidate ไทย"),
                                Some(tail("")),
                                gate,
                            )
                        }
                        2 => axum::response::IntoResponse::into_response(axum::Json(json!({
                            "id":"private-schema-repair","type":"message","role":"assistant",
                            "content":[{"type":"text","text":"{\"n\":2}"}],
                            "stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":10}
                        }))),
                        _ => panic!("unexpected schema request {number}"),
                    }
                }
            }
        })),
        0,
    )
    .await;
    let client = reader(&fixture).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    observer.request(Request::StartPipelineRun {
        session_id:fixture.session.clone(),content:"Private schema repair fixture".into(),
        options:formless_protocol::RunOptions {
            schema:Some(json!({"type":"object","required":["n"],"properties":{"n":{"type":"integer"}},"additionalProperties":false})),
            ..Default::default()
        },client_msg_id:Some(uuid::Uuid::new_v4().to_string()),
    }).await;
    canonical(&client, r#"{"n":2}"#, &["INVALID private schema candidate"]).await;
    let stored = snapshot(&mut observer, &fixture.session).await;
    assert_eq!(assistant_messages(&stored).len(), 1);
    assert_eq!(assistant_messages(&stored)[0]["content"], r#"{"n":2}"#);
    {
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["stream"], true);
        assert_eq!(calls[1]["stream"], false);
    }
    drop(client);
    drop(observer);
    fixture.finish().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_unexpected_ipc_eof_retires_prefix_without_replaying_provider_work() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-future-eof-key");
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = Fixture::start(
        app(axum::routing::post({
            let complete = complete.clone();
            let calls = calls.clone();
            move || {
                let complete = complete.clone();
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    gated_response(prefix("Before EOF ไทย "), Some(tail("😀")), complete)
                }
            }
        })),
        0,
    )
    .await;
    // Establish daemon readiness before the proxy accepts its first front.
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    let endpoint = Endpoint(fixture.root.join("reader-eof-proxy.sock"));
    let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
    let cut = Arc::new(Notify::new());
    let proxy = tokio::spawn({
        let endpoint = fixture.endpoint.clone();
        let cut = cut.clone();
        async move {
            loop {
                let (mut front, _) = listener.accept().await.unwrap();
                let mut back = tokio::net::UnixStream::connect(&endpoint.0).await.unwrap();
                tokio::select! {
                    _=tokio::io::copy_bidirectional(&mut front,&mut back)=>{},
                    _=cut.notified()=>{},
                }
            }
        }
    });
    let client = Client::start_future_attempts_for_validation(endpoint);
    client
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 1,
        })
        .unwrap();
    until(
        &client,
        "eof-reader-baseline",
        |u| matches!(u,Update::Transcript(b) if b.baseline.is_some()),
    )
    .await;

    send(&mut observer, &fixture.session).await;
    live(&client, "Before EOF ไทย ").await;
    let old = client.connection();
    let epoch = client.stream_epoch();
    cut.notify_one();
    until(&client, "eof-reader-new-baseline", |u| {
        if let Update::Transcript(b) = u {
            if b.owner.connection != old && b.owner.connection == client.connection() {
                if let Some(s) = b.baseline {
                    assert!(client.stream_epoch() > epoch);
                    assert_eq!(s.blocks.len(), 1);
                    return true;
                }
            }
        }
        false
    })
    .await;
    complete.notify_one();
    // A suffix from the already active attempt is not a new eligible prefix.
    until(&client,"eof-reader-final",|u| {
        if let Update::Transcript(b)=u {
            assert_eq!(b.owner.connection,client.connection());
            assert!(!b.events.iter().any(|e|matches!(e,Event::MessageDelta{..})));
            if b.baseline.as_ref().is_some_and(|s|s.blocks.iter().any(|block|block.markdown()=="Before EOF ไทย 😀")) {return true;}
            if b.events.iter().any(|e|matches!(e,Event::MessageComplete{message,..} if message.content=="Before EOF ไทย 😀")) {return true;}
        }false
    }).await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(client);
    drop(observer);
    proxy.abort();
    let _ = proxy.await;
    fixture.finish().await;
}
