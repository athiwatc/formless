//! Lost approval ACK through actual Client IPC; no GUI, provider or Tool.
use super::*;
use formless_desktop::approvals::{fixture_inventory, Action, Outcome, Prompt, Status};
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_lost_ack_retains_unknown_after_fresh_empty_inventory_and_never_replays() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let root = std::path::PathBuf::from("/tmp").join(format!(
        "fl-approval-loss-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::create_dir(&root).unwrap();
    let endpoint = Endpoint(root.join("ipc.sock"));
    let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
    let mode = Arc::new(AtomicBool::new(true));
    let writes = Arc::new(AtomicUsize::new(0));
    let state = mode.clone();
    let count = writes.clone();
    let mut session = formless_desktop::fixture::sessions(1).remove(0);
    session.id = "approval-session".into();
    let sid = session.id.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let peer = tokio::spawn(async move {
        'serve: loop {
            let (stream, _) =
                tokio::select! {_=&mut stopped=>break,opened=listener.accept()=>opened.unwrap()};
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            loop {
                let mut line = String::new();
                let n = tokio::select! {_=&mut stopped=>break 'serve,result=reader.read_line(&mut line)=>result.unwrap()};
                if n == 0 {
                    break;
                }
                let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let value = match request.request {
                    Request::Ping => json!({"min_protocol":1,"max_protocol":1}),
                    Request::Subscribe { deltas: false, .. } => json!({"subscribed":true}),
                    Request::ListSessions => json!([session]),
                    Request::GetSession { id } => {
                        assert_eq!(id, session.id);
                        json!({"session":{"id":id},"messages":[],"running":false,"step_mode":false,"pending_events":[]})
                    }
                    Request::PendingApprovals => {
                        if state.load(Ordering::SeqCst) {
                            fixture_inventory()
                        } else {
                            json!([])
                        }
                    }
                    Request::ApprovalDecision {
                        approval_id,
                        approved: true,
                        scope: formless_protocol::ApprovalScope::Once,
                        paths,
                        remember_denial: false,
                    } => {
                        assert_eq!(approval_id, "approval-fixture");
                        assert!(paths.is_empty());
                        state.store(false, Ordering::SeqCst);
                        count.fetch_add(1, Ordering::SeqCst);
                        write.shutdown().await.unwrap();
                        break;
                    }
                    other => panic!("unexpected approval-loss request {other:?}"),
                };
                let mut bytes = serde_json::to_vec(&ServerMessage::ok(request.id, value)).unwrap();
                bytes.push(b'\n');
                write.write_all(&bytes).await.unwrap();
            }
        }
    });
    let client = Client::start(endpoint);
    client
        .send(Command::Select {
            session_id: Some(sid.clone()),
            generation: 1,
        })
        .unwrap();
    until(
        &client,
        "approval-loss-baseline",
        |u| matches!(u,Update::Snapshot{snapshot,..} if snapshot.step_mode==Some(false)),
    )
    .await;
    let connection = client.connection();
    let action = || Action {
        client: client.instance(),
        connection: client.connection(),
        epoch: client.epoch(),
        operation: uuid::Uuid::new_v4(),
        prompt: Prompt::from_list(&fixture_inventory()).unwrap().unwrap(),
        allow: true,
    };
    client.decide_approval(action()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut reloaded = false;
    loop {
        for u in client.drain() {
            if matches!(u,Update::Snapshot{snapshot,..} if snapshot.step_mode==Some(false)) {
                reloaded = true;
            }
        }
        if reloaded
            && client.is_ready()
            && client.connection() > connection
            && matches!(
                client.approval_status(),
                Status::Finished {
                    outcome: Outcome::Unknown,
                    ..
                }
            )
        {
            break;
        }
        assert!(Instant::now() < deadline, "approval lost-ACK recovery");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let op = uuid::Uuid::new_v4();
    client
        .send(Command::ReadApprovals {
            connection: client.connection(),
            operation: op,
        })
        .unwrap();
    until(
        &client,
        "fresh-empty-approval-inventory",
        |u| matches!(u,Update::ApprovalInventory{operation,result:Ok(None),..}if operation==op),
    )
    .await;
    assert!(client.decide_approval(action()).is_err());
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(!mode.load(Ordering::SeqCst));
    stop.send(()).unwrap();
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(root.join("ipc.sock")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
