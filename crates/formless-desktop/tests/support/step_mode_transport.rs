//! Lost Step-mode ACK through actual Client IPC; no GUI, provider or Tool.
use super::*;
use formless_desktop::step_mode::{Action, Outcome, Status};
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mode_change_lost_ack_stays_unknown_across_connection_and_is_never_replayed() {
    for on in [true, false] {
        mode_lost_ack_case(on).await;
    }
}
#[cfg(unix)]
async fn mode_lost_ack_case(on: bool) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let root = std::path::PathBuf::from("/tmp").join(format!(
        "fl-mode-loss-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::create_dir(&root).unwrap();
    let endpoint = Endpoint(root.join("ipc.sock"));
    let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
    let mode = Arc::new(AtomicBool::new(!on));
    let writes = Arc::new(AtomicUsize::new(0));
    let state = mode.clone();
    let count = writes.clone();
    let mut session = formless_desktop::fixture::sessions(1).remove(0);
    session.id = "mode-loss-session".into();
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
                        json!({"session":{"id":id},"messages":[],"running":false,"step_mode":state.load(Ordering::SeqCst),"pending_events":[],"pending_steps":{"prompts":[],"more":false}})
                    }
                    Request::SetStepMode {
                        session_id,
                        on: target,
                    } if target == on => {
                        assert_eq!(session_id, session.id);
                        state.store(on, Ordering::SeqCst);
                        count.fetch_add(1, Ordering::SeqCst);
                        write.shutdown().await.unwrap();
                        break;
                    }
                    other => panic!("unexpected mode-loss request {other:?}"),
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
        "mode-loss-baseline",
        |u| matches!(u,Update::Snapshot{snapshot,..} if snapshot.step_mode==Some(!on)),
    )
    .await;
    let connection = client.connection();
    let action = || Action {
        client: client.instance(),
        connection: client.connection(),
        generation: 1,
        view_epoch: client.stream_epoch(),
        operation: uuid::Uuid::new_v4(),
        session_id: sid.clone(),
        on,
    };
    if on {
        client.enable_step_mode(action())
    } else {
        client.disable_step_mode(action())
    }
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut sampled_on = false;
    loop {
        for u in client.drain() {
            if matches!(u,Update::Snapshot{snapshot,..} if snapshot.step_mode==Some(on)) {
                sampled_on = true;
            }
        }
        if sampled_on
            && client.is_ready()
            && client.connection() > connection
            && matches!(
                client.step_mode_status(),
                Status::Finished {
                    outcome: Outcome::Unknown,
                    ..
                }
            )
        {
            break;
        }
        assert!(Instant::now() < deadline, "mode lost-ACK recovery");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!((if on {
        client.enable_step_mode(action())
    } else {
        client.disable_step_mode(action())
    })
    .is_err());
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(mode.load(Ordering::SeqCst), on);
    stop.send(()).unwrap();
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(root.join("ipc.sock")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
