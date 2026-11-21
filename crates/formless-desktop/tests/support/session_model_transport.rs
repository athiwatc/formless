//! Actual Client IPC: pending selection blocks work; dropped setter ACK never replays.
use super::*;
use formless_desktop::session_edit::{Change, Field, Metadata, Outcome};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_model_lost_ack_reconnect_and_explicit_unchanged_save_never_repeat_setter() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let root = std::path::PathBuf::from("/tmp").join(format!(
        "fl-model-loss-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::create_dir(&root).unwrap();
    let endpoint = Endpoint(root.join("ipc.sock"));
    let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
    let mut session = formless_desktop::fixture::sessions(1).remove(0);
    session.id = "model-loss-session".into();
    session.model = Some("old-model".into());
    session.reasoning_effort = Some("low".into());
    let original = session.clone();
    let sid = session.id.clone();
    let writes = Arc::new(AtomicUsize::new(0));
    let count = writes.clone();
    let gate = Arc::new(AtomicBool::new(false));
    let gate_peer = gate.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let entered_peer = entered.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let release_peer = release.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let peer = tokio::spawn(async move {
        'serve: loop {
            let (stream, _) =
                tokio::select! {_=&mut stopped=>break,result=listener.accept()=>result.unwrap()};
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            loop {
                let mut line = String::new();
                let n = tokio::select! {_=&mut stopped=>break 'serve,result=reader.read_line(&mut line)=>result.unwrap()};
                if n == 0 {
                    break;
                }
                let r: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let value = match r.request {
                    Request::Ping => {
                        json!({"min_protocol":1,"max_protocol":1,"features":["session_model_compare_set_v1"]})
                    }
                    Request::Subscribe { deltas: false, .. } => json!({"subscribed":true}),
                    Request::ListSessions => {
                        if gate_peer.swap(false, Ordering::SeqCst) {
                            entered_peer.notify_one();
                            release_peer.notified().await;
                        }
                        json!([session])
                    }
                    Request::GetSession { id } => {
                        assert_eq!(id, session.id);
                        json!({"session":session,"messages":[],"running":false,"step_mode":false,"pending_steps":{"prompts":[],"more":false},"pending_events":[]})
                    }
                    Request::CompareSetSessionModel {
                        id,
                        expected,
                        desired,
                    } => {
                        assert_eq!(expected.model, session.model);
                        assert_eq!(expected.reasoning_effort, session.reasoning_effort);
                        let model = desired.model;
                        let reasoning_effort = desired.reasoning_effort;
                        assert_eq!(id, session.id);
                        assert_eq!(model.as_deref(), Some("role:reviewer"));
                        assert_eq!(reasoning_effort.as_deref(), Some("low"));
                        session.model = model;
                        session.reasoning_effort = reasoning_effort;
                        count.fetch_add(1, Ordering::SeqCst);
                        write.shutdown().await.unwrap();
                        break;
                    }
                    other => panic!("unexpected model-loss request {other:?}"),
                };
                let mut bytes = serde_json::to_vec(&ServerMessage::ok(r.id, value)).unwrap();
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
    until(&client, "model-loss-baseline", |u| {
        matches!(u, Update::Snapshot { .. })
    })
    .await;
    let connection = client.connection();
    gate.store(true, Ordering::SeqCst);
    client
        .send(Command::EditSession {
            connection,
            operation: uuid::Uuid::new_v4(),
            session_id: sid.clone(),
            change: Change::from_input(
                Field::Model,
                "role:reviewer",
                &Metadata::from_session(&original).unwrap(),
            )
            .unwrap(),
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    client
        .send(Command::Review {
            intent: formless_desktop::model::Intent::SendMessage {
                session_id: sid.clone(),
                content: "draft remains unsent".into(),
                mode: formless_protocol::SendMode::Send,
            },
            expected_run_id: None,
            draft_basis: "draft remains unsent".into(),
            epoch: client.epoch(),
        })
        .unwrap();
    until(
        &client,
        "pending-model-blocks-work",
        |u| matches!(u,Update::PreparationFailed{error,..}if error.contains("save receipt")),
    )
    .await;
    release.notify_one();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut fresh = false;
    loop {
        for u in client.drain() {
            if matches!(u,Update::Sessions(s)if s.iter().any(|s|s.model.as_deref()==Some("role:reviewer")))
            {
                fresh = true;
            }
        }
        if fresh && client.is_ready() && client.connection() > connection {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "model lost-ACK physical reconnect"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let read = uuid::Uuid::new_v4();
    client
        .send(Command::ReadSessionDetails {
            connection: client.connection(),
            operation: read,
            session_id: sid.clone(),
        })
        .unwrap();
    let mut metadata = None;
    until(&client, "model-loss-explicit-read", |u| {
        if let Update::SessionDetails {
            operation,
            result: Ok(m),
            ..
        } = u
        {
            if operation == read {
                metadata = Some(m);
                true
            } else {
                false
            }
        } else {
            false
        }
    })
    .await;
    let model = metadata.unwrap();
    let op = uuid::Uuid::new_v4();
    client
        .send(Command::EditSession {
            connection: client.connection(),
            operation: op,
            session_id: sid,
            change: Change::from_input(Field::Model, "role:reviewer", &model).unwrap(),
        })
        .unwrap();
    until(&client,"model-loss-read-only-already-selected",|u|matches!(u,Update::SessionEdited{operation,outcome:Outcome::ObservedSelection(_),..}if operation==op)).await;
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    stop.send(()).unwrap();
    drop(client);
    tokio::time::timeout(Duration::from_secs(3), peer)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_file(root.join("ipc.sock")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
