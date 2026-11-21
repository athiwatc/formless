//! A controlled private IPC peer loses a decision ACK after reading the write.
//! No real Profile, GUI, Tool, provider or credentials are used.
use super::*;
use formless_desktop::step_decision::{Action, Choice, Outcome, Status};
use formless_protocol::{PendingStep, PendingSteps};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(unix)]
async fn a_lost_step_receipt_blocks_native_retries_across_physical_reconnect() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let root = std::path::PathBuf::from("/tmp").join(format!(
        "fl-nstep-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::create_dir(&root).unwrap();
    let endpoint = Endpoint(root.join("ipc.sock"));
    let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
    let decided = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicUsize::new(0));
    let seen_writes = writes.clone();
    let closed = decided.clone();
    let mut session = formless_desktop::fixture::sessions(1).remove(0);
    session.id = "native-step-loss-session".into();
    let sid = session.id.clone();
    let prompt = PendingStep {
        session_id: sid.clone(),
        run_id: "native-step-loss-run".into(),
        step_id: "native-step-loss-prompt".into(),
        tool_name: "never-executed-fixture".into(),
        arguments_preview: "{}".into(),
        arguments_bytes: 2,
        index: 0,
        count: 1,
    };
    let peer_prompt = prompt.clone();
    let peer_sid = sid.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let peer = tokio::spawn(async move {
        let mut connection = 0;
        loop {
            let (stream, _) =
                tokio::select! {_=&mut stopped=>break,opened=listener.accept()=>opened.unwrap()};
            connection += 1;
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let envelope: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let value = match envelope.request {
                    Request::Ping => json!({"min_protocol":1,"max_protocol":1}),
                    Request::Subscribe { deltas: false, .. } => {
                        json!({"subscribed":true,"conn":connection})
                    }
                    Request::ListSessions => json!([session]),
                    Request::ContextUsage { .. } => {
                        json!({"used_tokens":0,"context_tokens":1000,"measured":false})
                    }
                    Request::GetSession { id } => {
                        assert_eq!(id, peer_sid);
                        json!({"session":{"id":peer_sid},"running":!closed.load(Ordering::SeqCst),"messages":[],"pending_events":[],"pending_steps":PendingSteps{prompts:if closed.load(Ordering::SeqCst){vec![]}else{vec![peer_prompt.clone()]},more:false}})
                    }
                    Request::StepDecision {
                        step_id,
                        action,
                        arguments,
                    } => {
                        assert_eq!(step_id, peer_prompt.step_id);
                        assert_eq!(action, "continue");
                        assert!(arguments.is_none());
                        seen_writes.fetch_add(1, Ordering::SeqCst);
                        closed.store(true, Ordering::SeqCst);
                        write.shutdown().await.unwrap();
                        break;
                    }
                    other => panic!("unexpected native Step loss fixture request {other:?}"),
                };
                let mut bytes = serde_json::to_vec(&ServerMessage::ok(envelope.id, value)).unwrap();
                bytes.push(b'\n');
                if write.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        }
    });
    let client = Client::start(endpoint);
    client
        .send(Command::Select {
            session_id: Some(sid),
            generation: 1,
        })
        .unwrap();
    until(&client,"native-step-loss-baseline",|u|matches!(u,Update::Snapshot{snapshot,..} if matches!(&snapshot.steps,formless_desktop::steps::Inventory::Snapshot(s) if s.prompts.len()==1))).await;
    let connection = client.connection();
    let action = Action {
        client: client.instance(),
        connection,
        generation: 1,
        epoch: client.epoch(),
        operation: uuid::Uuid::new_v4(),
        prompt,
        choice: Choice::Continue,
    };
    client.decide_step(action.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        client.drain().for_each(drop);
        if matches!(
            client.step_status(),
            Status::Finished {
                outcome: Outcome::Unknown,
                ..
            }
        ) && client.is_ready()
            && client.connection() > connection
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Step loss recovery deadline: {:?}",
            client.step_status()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(decided.load(Ordering::SeqCst));
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert!(client
        .decide_step(Action {
            connection: client.connection(),
            epoch: client.epoch(),
            operation: uuid::Uuid::new_v4(),
            choice: Choice::Skip,
            ..action
        })
        .is_err());
    client.send(Command::Refresh).unwrap();
    until(&client,"native-step-loss-reconnected-baseline",|u|matches!(u,Update::Snapshot{snapshot,..} if matches!(&snapshot.steps,formless_desktop::steps::Inventory::Snapshot(s) if s.prompts.is_empty()))).await;
    assert_eq!(
        writes.load(Ordering::SeqCst),
        1,
        "no mutation was replayed on reconnect"
    );
    assert!(matches!(
        client.step_status(),
        Status::Finished {
            outcome: Outcome::Unknown,
            ..
        }
    ));
    drop(client);
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(5), peer)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
