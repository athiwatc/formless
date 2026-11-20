//! Adversarial scoped producer ordering on private IPC, without GUI or provider.
use super::*;
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ignored_future_frame_cannot_revive_a_reordered_prefix_and_recovers_fresh() {
    use formless_protocol::{
        TranscriptAttemptChange as Phase, TranscriptAttemptClose, TranscriptAttemptEvent,
    };
    for closed in [false, true] {
        let root = std::path::PathBuf::from("/tmp").join(format!(
            "fl-cursor-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        ));
        std::fs::create_dir(&root).unwrap();
        let endpoint = Endpoint(root.join("ipc.sock"));
        let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let peer = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let epoch = uuid::Uuid::new_v4().to_string();
            let mut barriers = 0;
            let mut injected = false;
            let snapshot = || json!({"session":{"id":"cursor-session"},"running":true,"messages":[{"id":"m","session_id":"cursor-session","seq":1,"role":"user","content":"Question","created_at":"now"}],"pending_events":[]});
            let event = |cursor, change| {
                ServerMessage::Event(Event::TranscriptAttempt {
                    update: TranscriptAttemptEvent {
                        epoch: epoch.clone(),
                        cursor,
                        session_id: "cursor-session".into(),
                        run_id: "run".into(),
                        turn_id: "turn".into(),
                        attempt_id: "attempt".into(),
                        change,
                    },
                })
            };
            async fn send(write: &mut tokio::net::unix::OwnedWriteHalf, message: ServerMessage) {
                let mut bytes = serde_json::to_vec(&message).unwrap();
                bytes.push(b'\n');
                write.write_all(&bytes).await.unwrap();
            }
            loop {
                let mut line = String::new();
                let count = tokio::select! { _=&mut stopped=>break, result=reader.read_line(&mut line)=>result.unwrap() };
                if count == 0 {
                    break;
                }
                let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let value = match request.request {
                    Request::Ping => {
                        json!({"min_protocol":1,"max_protocol":1,"features":["future_transcript_attempts_v1"]})
                    }
                    Request::SubscribeTranscriptAttempts { .. } => {
                        json!({"subscribed":true,"mode":"future_attempts_only_v1"})
                    }
                    Request::ListSessions => json!([]),
                    Request::GetTranscriptBarrier { .. } => {
                        barriers += 1;
                        assert!(barriers <= 2, "recovery repeatedly reloaded");
                        let value = json!({"snapshot":snapshot(),"barrier":{"epoch":epoch,"cursor":if barriers==1 {10}else{20}},"mode":"future_attempts_only_v1"});
                        if barriers == 2 {
                            send(&mut write, ServerMessage::ok(request.id, value)).await;
                            send(&mut write, event(21, Phase::Started { base_seq: 1 })).await;
                            send(
                                &mut write,
                                event(
                                    22,
                                    Phase::Delta {
                                        offset: 0,
                                        text: "fresh eligible prefix".into(),
                                    },
                                ),
                            )
                            .await;
                            continue;
                        }
                        value
                    }
                    Request::GetSession { .. } => {
                        if !injected {
                            injected = true;
                            send(
                                &mut write,
                                event(
                                    12,
                                    if closed {
                                        Phase::Closed {
                                            bytes: 0,
                                            reason: TranscriptAttemptClose::Returned,
                                        }
                                    } else {
                                        Phase::Delta {
                                            offset: 0,
                                            text: "missing prefix".into(),
                                        }
                                    },
                                ),
                            )
                            .await;
                            send(&mut write, event(11, Phase::Started { base_seq: 1 })).await;
                            send(
                                &mut write,
                                event(
                                    13,
                                    Phase::Delta {
                                        offset: 0,
                                        text: "must not appear".into(),
                                    },
                                ),
                            )
                            .await;
                        }
                        snapshot()
                    }
                    other => panic!("unexpected cursor fixture request {other:?}"),
                };
                send(&mut write, ServerMessage::ok(request.id, value)).await;
            }
            barriers
        });
        let client = Client::start_future_attempts_for_validation(endpoint);
        client
            .send(Command::Select {
                session_id: Some("cursor-session".into()),
                generation: 1,
            })
            .unwrap();
        let mut baseline = None;
        until(&client, "cursor-order-baseline", |u| {
            if let Update::Transcript(b) = u {
                if b.baseline.is_some() {
                    baseline = Some(b.owner.clone());
                    return true;
                }
            }
            false
        })
        .await;
        let connection = client.connection();
        let mut reloaded = false;
        let mut fresh = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fresh {
            assert!(Instant::now() < deadline, "cursor recovery deadline");
            for update in client.drain() {
                if let Update::Transcript(b) = update {
                    reloaded |= b.reload;
                    for event in b.events {
                        if let Event::MessageDelta { delta, .. } = event {
                            assert_eq!(
                                delta, "fresh eligible prefix",
                                "reordered candidate escaped admission"
                            );
                            assert_ne!(Some(&b.owner), baseline.as_ref());
                            fresh = true;
                        }
                    }
                }
            }
            assert_eq!(
                client.connection(),
                connection,
                "cursor violation reconnected instead of owned barrier recovery"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(reloaded);
        stop.send(()).unwrap();
        drop(client);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), peer)
                .await
                .unwrap()
                .unwrap(),
            2
        );
        std::fs::remove_file(root.join("ipc.sock")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
