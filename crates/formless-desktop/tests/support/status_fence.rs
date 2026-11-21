//! Same-sequence metadata overlap over a private IPC socket; no GUI or provider.
use super::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_pre_run_status_cannot_restore_paused_controls_over_live_prefix() {
    use formless_protocol::{TranscriptAttemptChange, TranscriptAttemptEvent};
    let root = std::path::PathBuf::from("/tmp").join(format!(
        "fl-status-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::create_dir(&root).unwrap();
    let endpoint = Endpoint(root.join("ipc.sock"));
    let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let epoch = uuid::Uuid::new_v4().to_string();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut released = Some(released);
        let mut statuses = 0;
        let mut barriers = 0;
        let snapshot = |running| {
            json!({"session":{"id":"status-session"},"running":running,
            "messages":[{"id":"m","session_id":"status-session","seq":1,"role":"user","content":"Question","created_at":"now"}],
            "pending_events":[]})
        };
        let event = |cursor, change| {
            ServerMessage::Event(Event::TranscriptAttempt {
                update: TranscriptAttemptEvent {
                    epoch: epoch.clone(),
                    cursor,
                    session_id: "status-session".into(),
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
                    assert_eq!(barriers, 1, "status overlap retired a valid live basis");
                    json!({"snapshot":snapshot(false),"barrier":{"epoch":epoch,"cursor":10},"mode":"future_attempts_only_v1"})
                }
                Request::GetSession { .. } => {
                    statuses += 1;
                    if statuses == 1 {
                        send(
                            &mut write,
                            ServerMessage::Event(Event::RunStarted {
                                session_id: "status-session".into(),
                                run_id: "run".into(),
                            }),
                        )
                        .await;
                        send(
                            &mut write,
                            event(11, TranscriptAttemptChange::Started { base_seq: 1 }),
                        )
                        .await;
                        send(
                            &mut write,
                            event(
                                12,
                                TranscriptAttemptChange::Delta {
                                    offset: 0,
                                    text: "live prefix".into(),
                                },
                            ),
                        )
                        .await;
                        tokio::select! { _=&mut stopped=>break, result=released.take().unwrap()=>result.unwrap() };
                        snapshot(false)
                    } else {
                        send(
                            &mut write,
                            event(
                                13,
                                TranscriptAttemptChange::Delta {
                                    offset: 11,
                                    text: " tail".into(),
                                },
                            ),
                        )
                        .await;
                        snapshot(true)
                    }
                }
                other => panic!("unexpected private status fixture request {other:?}"),
            };
            send(&mut write, ServerMessage::ok(request.id, value)).await;
        }
        (statuses, barriers)
    });
    let client = Client::start_future_attempts_for_validation(endpoint);
    client
        .send(Command::Select {
            session_id: Some("status-session".into()),
            generation: 1,
        })
        .unwrap();
    let mut basis = None;
    until(&client, "status-fence-baseline", |u| {
        if let Update::Transcript(b) = u {
            if b.baseline.is_some() {
                basis = Some(b.owner.clone());
                return true;
            }
        }
        false
    })
    .await;
    let connection = client.connection();
    let mut invalidated = false;
    until(&client, "status-fence-prefix", |u| {
        if matches!(u, Update::SessionStatusInvalidated { .. }) { invalidated = true; }
        matches!(u, Update::Transcript(b) if b.events.iter().any(|e| matches!(e, Event::MessageDelta { delta, .. } if delta == "live prefix")))
    }).await;
    assert!(invalidated);
    release.send(()).unwrap();
    let mut tail = false;
    let mut fresh = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while !tail || !fresh {
        assert!(Instant::now() < deadline, "fresh status/tail deadline");
        for update in client.drain() {
            if let Update::Transcript(b) = update {
                assert_eq!(Some(&b.owner), basis.as_ref());
                assert!(!b.reload);
                assert!(b.baseline.is_none());
                if let Some(status) = b.status {
                    assert!(status.running, "stale pre-Run status escaped fence");
                    fresh = true;
                }
                tail |= b
                    .events
                    .iter()
                    .any(|e| matches!(e, Event::MessageDelta { delta, .. } if delta == " tail"));
            }
        }
        assert_eq!(client.connection(), connection);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    stop.send(()).unwrap();
    drop(client);
    let (statuses, barriers) = tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(statuses, 2);
    assert_eq!(barriers, 1);
    std::fs::remove_file(root.join("ipc.sock")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
