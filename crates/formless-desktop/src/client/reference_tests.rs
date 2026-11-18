use super::*;
use crate::reference_detach::{Action as Removal, Ledger, Outcome, Status};
use serde_json::json;
fn action() -> Removal {
    Removal{client:uuid::Uuid::new_v4(),connection:0,epoch:0,form:uuid::Uuid::new_v4(),read:uuid::Uuid::new_v4(),operation:uuid::Uuid::new_v4(),session_id:"chosen".into(),index:0,report:Arc::new(crate::pending_files::Report::decode("chosen",json!({"session_id":"chosen","attachments":[{"reference":true,"name":"folder","path":"/not/read","dir":true,"bytes":0,"future":true}]})).unwrap())}
}
fn engine(a: &Removal) -> Engine {
    Engine {
        ready: true,
        reference_detach_available: true,
        instance: a.client,
        connection: a.connection,
        epoch: a.epoch,
        active: Some("different-active-conversation".into()),
        ..Default::default()
    }
}
#[test]
fn reference_actor_binds_connection_and_epoch_but_targets_the_explicit_chosen_session() {
    let a = action();
    let l = Arc::new(Ledger::default());
    let d = l.reserve(a.clone()).unwrap();
    let mut e = engine(&a);
    e.command(Command::DetachReference(d.clone()));
    e.command(Command::DetachReference(d.clone()));
    assert_eq!(e.outgoing.len(), 1);
    assert!(
        matches!(&e.outgoing[0].request,Request::DetachReviewedReference{session_id,index:0,..}if session_id=="chosen")
    );
    e.epoch += 1;
    e.retire_reference_writes();
    assert!(e.outgoing.is_empty());
    assert!(matches!(
        l.status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
    for reason in 0..4 {
        let a = action();
        let l = Arc::new(Ledger::default());
        let d = l.reserve(a.clone()).unwrap();
        let mut e = engine(&a);
        match reason {
            0 => e.reference_detach_available = false,
            1 => e.instance = uuid::Uuid::new_v4(),
            2 => e.connection += 1,
            _ => e.ready = false,
        };
        e.command(Command::DetachReference(d));
        assert!(e.outgoing.is_empty());
        assert!(matches!(
            l.status(),
            Status::Finished {
                outcome: Outcome::Rejected(_),
                ..
            }
        ));
    }
}
#[test]
fn reference_actor_success_is_exact_and_an_error_cannot_be_treated_as_unwritten() {
    for success in [false, true] {
        let a = action();
        let l = Arc::new(Ledger::default());
        let d = l.reserve(a.clone()).unwrap();
        let mut e = engine(&a);
        e.command(Command::DetachReference(d.clone()));
        let req = e.outgoing.pop_front().unwrap();
        assert!(d.before_write());
        e.receive(if success{ServerMessage::ok(req.id,json!({"removed":true,"session_id":a.session_id,"index":a.index,"operation_id":a.operation.to_string()}))}else{ServerMessage::err(req.id,"pending attachments changed".into())}).unwrap();
        assert!(
            matches!(
                l.status(),
                Status::Finished {
                    outcome: Outcome::Removed,
                    ..
                }
            ) == success
        );
        assert_eq!(l.reserve(action()).is_err(), !success);
    }
}
#[tokio::test]
async fn reference_framed_lost_failed_and_malformed_receipts_keep_unknown_and_exact_success_clears_it(
) {
    for reply in [
        None,
        Some(Err("database failed".to_string())),
        Some(Ok(json!({"removed":true}))),
        Some(Ok(json!({"good":true}))),
    ] {
        let a = action();
        let l = Arc::new(Ledger::default());
        let d = l.reserve(a.clone()).unwrap();
        let (stream, server) = tokio::io::duplex(64 * 1024);
        let (tx, mut commands) = queue::channel(QUEUE);
        let (updates, _rx) = mpsc::sync_channel(QUEUE);
        let (_stop, mut cancelled) = watch::channel(false);
        let ready = Arc::new(AtomicBool::new(false));
        let epoch = AtomicU64::new(0);
        let connection = AtomicU64::new(0);
        let mut retained = Retained {
            instance: a.client,
            ..Default::default()
        };
        let sender = tokio::spawn({
            let ready = ready.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !ready.load(Ordering::Acquire) {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
                tx.send(Command::DetachReference(d)).await.unwrap();
                tx
            }
        });
        let expected = a.clone();
        let accepted = reply
            .as_ref()
            .is_some_and(|r| r.as_ref().is_ok_and(|v| v["good"] == true));
        let peer = tokio::spawn(async move {
            let mut reader = BufReader::new(server);
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
                let req: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let value = match req.request {
                    Request::Ping => {
                        json!({"min_protocol":1,"max_protocol":1,"features":["reviewed_reference_detach_v1"]})
                    }
                    Request::Subscribe { deltas, .. } => {
                        assert!(!deltas);
                        json!({"subscribed":true})
                    }
                    Request::ListSessions => json!([]),
                    Request::DetachReviewedReference {
                        session_id,
                        expected_attachments,
                        index,
                        operation_id,
                    } => {
                        assert_eq!(session_id, expected.session_id);
                        assert_eq!(expected_attachments, expected.report.expected());
                        assert_eq!(index, expected.index);
                        assert_eq!(operation_id, expected.operation.to_string());
                        if let Some(reply) = reply {
                            let response = match reply {
                                Ok(v) if v["good"] == true => ServerMessage::ok(
                                    req.id,
                                    json!({"removed":true,"session_id":session_id,"index":index,"operation_id":operation_id}),
                                ),
                                Ok(v) => ServerMessage::ok(req.id, v),
                                Err(e) => ServerMessage::err(req.id, e),
                            };
                            let mut bytes = serde_json::to_vec(&response).unwrap();
                            bytes.push(b'\n');
                            reader.get_mut().write_all(&bytes).await.unwrap();
                        }
                        break;
                    }
                    other => panic!("unexpected reference request {other:?}"),
                };
                let mut bytes = serde_json::to_vec(&ServerMessage::ok(req.id, value)).unwrap();
                bytes.push(b'\n');
                reader.get_mut().write_all(&bytes).await.unwrap();
            }
        });
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            connected(
                stream,
                &mut commands,
                &updates,
                &mut cancelled,
                &mut retained,
                ConnectionState {
                    ready: &ready,
                    epoch: &epoch,
                    connection: &connection,
                },
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        peer.await.unwrap();
        let _sender = sender.await.unwrap();
        assert_eq!(
            matches!(
                l.status(),
                Status::Finished {
                    outcome: Outcome::Unknown,
                    ..
                }
            ),
            !accepted
        );
        assert_eq!(retained.uncertain.len(), usize::from(!accepted));
        if !accepted {
            reconnect_keeps_reference_unknown(&mut retained, &a).await;
        }
        let e = engine(&a);
        assert_eq!(
            e.blocked_by_unknown(
                &Command::Review {
                    intent: Intent::SendMessage {
                        session_id: a.session_id.clone(),
                        content: "kept".into(),
                        mode: formless_protocol::SendMode::Send
                    },
                    expected_run_id: None,
                    draft_basis: "kept".into(),
                    epoch: 0
                },
                &retained.uncertain
            ),
            !accepted
        );
    }
}

#[test]
fn reference_pending_blocks_a_previously_prepared_commit_and_its_confirmation() {
    for commit in [false, true] {
        let a = action();
        let l = Arc::new(Ledger::default());
        let d = l.reserve(a.clone()).unwrap();
        let mut e = engine(&a);
        let id = uuid::Uuid::new_v4();
        e.prepared = Some(Box::new(Preparation {
            client: a.client,
            connection: 0,
            attachments: Some((*a.report).clone()),
            id,
            epoch: 0,
            generation: 0,
            active: e.active.clone(),
            draft_basis: "kept".into(),
            status_revision: 0,
            intent: Intent::SendMessage {
                session_id: a.session_id.clone(),
                content: "kept".into(),
                mode: formless_protocol::SendMode::Send,
            },
            stage: PreparationStage::Ready,
        }));
        e.command(Command::DetachReference(d));
        e.command(if commit {
            Command::CommitPrepared(id)
        } else {
            Command::ConfirmPrepared(id)
        });
        assert_eq!(e.outgoing.len(), 1);
        assert!(matches!(
            e.outgoing[0].request,
            Request::DetachReviewedReference { .. }
        ));
        assert!(e
            .updates
            .iter()
            .any(|u| matches!(u,Update::Notice(n)if n.contains("reference-removal receipt"))));
    }
}

#[test]
fn reference_client_queue_failure_foreign_ownership_and_unknown_work_guard() {
    let (commands, mut received) = queue::channel(1);
    let (_, updates) = mpsc::sync_channel(1);
    let (stop, _) = watch::channel(false);
    let client = Client {
        reference_ledger: Arc::new(Ledger::default()),
        step_ledger: Arc::new(crate::step_decision::Ledger::default()),
        mode_ledger: Arc::new(crate::step_mode::Ledger::default()),
        approval_ledger: Arc::new(crate::approvals::Ledger::default()),
        commands,
        updates,
        stop,
        ready: Arc::new(AtomicBool::new(true)),
        epoch: Arc::new(AtomicU64::new(0)),
        connection: Arc::new(AtomicU64::new(0)),
        forks: Arc::new(session_fork::Ledger::default()),
        stream_epoch: Arc::new(AtomicU64::new(0)),
        instance: uuid::Uuid::new_v4(),
        future_attempts: false,
        #[cfg(feature = "test-support")]
        recorded: None,
    };
    let mut a = action();
    a.client = client.instance();
    client.send(Command::Refresh).unwrap();
    assert!(client.detach_reference(a.clone()).is_err());
    assert!(matches!(
        client.reference_status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
    received.try_recv().unwrap();
    let foreign = Arc::new(Ledger::default());
    let d = foreign.reserve(a.clone()).unwrap();
    assert!(client.send(Command::DetachReference(d)).is_err());
    a.operation = uuid::Uuid::new_v4();
    client.detach_reference(a.clone()).unwrap();
    let Command::DetachReference(d) = received.try_recv().unwrap() else {
        panic!("removal")
    };
    assert!(d.before_write());
    drop(d);
    assert!(matches!(
        client.reference_status(),
        Status::Finished {
            outcome: Outcome::Unknown,
            ..
        }
    ));
    let intent = |id: &str| Intent::ResumeRun {
        session_id: id.into(),
    };
    assert!(client.send(Command::Intent(intent("chosen"))).is_err());
    client.send(Command::Intent(intent("other"))).unwrap();
    received.try_recv().unwrap();
    client
        .send(Command::Intent(Intent::InterruptRun {
            session_id: "chosen".into(),
        }))
        .unwrap();
    received.try_recv().unwrap();
    assert!(client
        .detach_reference(Removal {
            operation: uuid::Uuid::new_v4(),
            ..a
        })
        .is_err());
    assert!(received.try_recv().is_err());
}

async fn reconnect_keeps_reference_unknown(retained: &mut Retained, original: &Removal) {
    async fn wait(rx: &mut Receiver<Update>, predicate: impl Fn(&Update) -> bool + Send) {
        tokio::time::timeout(Duration::from_secs(5), async move {
            loop {
                if rx.try_iter().any(|u| predicate(&u)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
    }
    let unknown = retained.uncertain.clone();
    let (stream, server) = tokio::io::duplex(64 * 1024);
    let (tx, mut commands) = queue::channel(QUEUE);
    let (updates, mut rx) = mpsc::sync_channel(QUEUE);
    let (stop, mut cancelled) = watch::channel(false);
    let ready = Arc::new(AtomicBool::new(false));
    let epoch = AtomicU64::new(0);
    let connection = AtomicU64::new(1);
    let a = Removal {
        connection: 1,
        operation: uuid::Uuid::new_v4(),
        ..original.clone()
    };
    let ledger = Arc::new(Ledger::default());
    let d = ledger.reserve(a.clone()).unwrap();
    let driver = tokio::spawn({
        let ready = ready.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !ready.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            tx.send(Command::ReadPendingAttachments {
                connection: 1,
                operation: uuid::Uuid::new_v4(),
                session_id: a.session_id.clone(),
            })
            .await
            .unwrap();
            wait(
                &mut rx,
                |u| matches!(u,Update::PendingAttachments{result:Ok(r),..}if r.rows.is_empty()),
            )
            .await;
            tx.send(Command::Intent(Intent::InterruptRun {
                session_id: a.session_id.clone(),
            }))
            .await
            .unwrap();
            wait(&mut rx, |u| {
                matches!(u, Update::Action { result: Ok(_), .. })
            })
            .await;
            tx.send(Command::Review {
                intent: Intent::ResumeRun {
                    session_id: a.session_id,
                },
                expected_run_id: None,
                draft_basis: "kept".into(),
                epoch: 0,
            })
            .await
            .unwrap();
            wait(
                &mut rx,
                |u| matches!(u,Update::Notice(n)if n.contains("outcome is unknown")),
            )
            .await;
            tx.send(Command::DetachReference(d)).await.unwrap();
            wait(
                &mut rx,
                |u| matches!(u,Update::Notice(n)if n.contains("outcome is unknown")),
            )
            .await;
            stop.send(true).unwrap();
            tx
        }
    });
    let peer = tokio::spawn(async move {
        let mut reader = BufReader::new(server);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            let req: RequestEnvelope = serde_json::from_str(&line).unwrap();
            let v = match req.request {
                Request::Ping => {
                    json!({"min_protocol":1,"max_protocol":1,"features":["reviewed_reference_detach_v1"]})
                }
                Request::Subscribe { deltas, .. } => {
                    assert!(!deltas);
                    json!({"subscribed":true})
                }
                Request::ListSessions => json!([]),
                Request::ListPendingAttachments { session_id } => {
                    json!({"session_id":session_id,"attachments":[]})
                }
                Request::InterruptRun { .. } => json!({"interrupted":true}),
                other => panic!("reconnect must not replay removal or start work: {other:?}"),
            };
            let mut frame = serde_json::to_vec(&ServerMessage::ok(req.id, v)).unwrap();
            frame.push(b'\n');
            reader.get_mut().write_all(&frame).await.unwrap();
        }
    });
    tokio::time::timeout(
        Duration::from_secs(10),
        connected(
            stream,
            &mut commands,
            &updates,
            &mut cancelled,
            retained,
            ConnectionState {
                ready: &ready,
                epoch: &epoch,
                connection: &connection,
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let _sender = driver.await.unwrap();
    peer.await.unwrap();
    assert_eq!(retained.uncertain, unknown);
    assert!(matches!(
        ledger.status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
}
