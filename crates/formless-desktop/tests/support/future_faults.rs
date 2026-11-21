//! Unix-only IPC fault schedules, using a real private daemon and fake provider.
#![cfg(unix)]
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy)]
enum Fault {
    StatusReply,
    ServerRead,
}
struct Proxy {
    endpoint: Endpoint,
    arm: Arc<AtomicBool>,
    reached: Arc<Notify>,
    release: Arc<Notify>,
    value: Arc<Mutex<Option<Value>>>,
    upstream_eof: Arc<AtomicBool>,
    closed_by: Arc<Mutex<Option<&'static str>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Proxy {
    async fn start(fixture: &Fixture, fault: Fault) -> Self {
        let endpoint = Endpoint(fixture.root.join("reader-fault-proxy.sock"));
        let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
        let arm = Arc::new(AtomicBool::new(false));
        let reached = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let value = Arc::new(Mutex::new(None));
        let upstream_eof = Arc::new(AtomicBool::new(false));
        let closed_by = Arc::new(Mutex::new(None));
        let task = tokio::spawn({
            let upstream = fixture.endpoint.clone();
            let arm = arm.clone();
            let reached = reached.clone();
            let release = release.clone();
            let value = value.clone();
            let upstream_eof = upstream_eof.clone();
            let closed_by = closed_by.clone();
            async move {
                loop {
                    let (front, _) = listener.accept().await.unwrap();
                    let back = tokio::net::UnixStream::connect(&upstream.0).await.unwrap();
                    let (front_read, mut front_write) = front.into_split();
                    let (back_read, mut back_write) = back.into_split();
                    let selected = Arc::new(Mutex::new(None));
                    let requests = async {
                        let mut lines = BufReader::new(front_read).lines();
                        while let Some(mut line) = lines.next_line().await? {
                            let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                            if matches!(fault, Fault::StatusReply)
                                && matches!(request.request, Request::GetSession { .. })
                                && arm.swap(false, Ordering::SeqCst)
                            {
                                *selected.lock().unwrap() = Some(request.id);
                            }
                            line.push('\n');
                            if let Err(error) = back_write.write_all(line.as_bytes()).await {
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::BrokenPipe
                                        | std::io::ErrorKind::ConnectionReset
                                        | std::io::ErrorKind::NotConnected
                                ) {
                                    upstream_eof.store(true, Ordering::SeqCst);
                                    *closed_by.lock().unwrap() =
                                        Some("upstream request write closed");
                                }
                                return Err(error);
                            }
                        }
                        Ok::<_, std::io::Error>(())
                    };
                    let responses = async {
                        let mut lines = BufReader::new(back_read).lines();
                        let mut held: Option<String> = None;
                        let mut draining_finished_run = false;
                        loop {
                            tokio::select! {
                                biased;
                                _=release.notified(), if held.is_some()=> {
                                    front_write.write_all(held.take().unwrap().as_bytes()).await?;
                                }
                                line=lines.next_line()=> {
                                    let mut line = match line {
                                        Ok(Some(line)) => line,
                                        Ok(None) => {upstream_eof.store(true,Ordering::SeqCst);*closed_by.lock().unwrap()=Some("upstream response EOF");break;},
                                        Err(error) => {
                                            if matches!(error.kind(),std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::NotConnected) {
                                                upstream_eof.store(true,Ordering::SeqCst);
                                                *closed_by.lock().unwrap()=Some("upstream response reset");
                                            }
                                            return Err(error);
                                        }
                                    };
                                    let frame:ServerMessage=serde_json::from_str(&line).unwrap();
                                    let provisional = matches!(&frame, ServerMessage::Event(Event::MessageDelta { .. } | Event::RunUsage { .. }));
                                    let intercept=match (&frame,fault) {
                                        (ServerMessage::Response{id,..},Fault::StatusReply)=>*selected.lock().unwrap()==Some(*id),
                                        (_,Fault::ServerRead)=>arm.swap(false,Ordering::SeqCst),
                                        _=>false,
                                    };
                                    line.push('\n');
                                    if intercept {
                                        if let ServerMessage::Response{result,..}=frame {*value.lock().unwrap()=result;}
                                        reached.notify_one();
                                        if matches!(fault,Fault::StatusReply) {held=Some(line);continue;}
                                        release.notified().await;
                                        // ServerRead is released only after the observer saw
                                        // RunFinished. Discard the obsolete provisional frames
                                        // while draining this failed connection, rather than
                                        // saturating the UI consumer or sleeping per frame.
                                        // The assertion requires the real upstream EOF and a
                                        // complete canonical baseline on a new connection.
                                        draining_finished_run = true;
                                    }
                                    if draining_finished_run && provisional {continue;}
                                    front_write.write_all(line.as_bytes()).await?;
                                }
                            }
                        }
                        Ok::<_, std::io::Error>(())
                    };
                    tokio::select! {_=requests=>{},_=responses=>{}}
                }
            }
        });
        Self {
            endpoint,
            arm,
            reached,
            release,
            value,
            upstream_eof,
            closed_by,
            task,
        }
    }
    async fn reached(&self, client: &Client) {
        let ready = self.reached.notified();
        tokio::pin!(ready);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(Instant::now() < deadline, "proxy fault was not reached");
            tokio::select! {biased;_= &mut ready=>return,_=tokio::time::sleep(Duration::from_millis(2))=>{client.drain().for_each(drop);}}
        }
    }
    async fn finish(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}
async fn through(fixture: &Fixture, proxy: &Proxy) -> Client {
    let client = Client::start_future_attempts_for_validation(proxy.endpoint.clone());
    client
        .send(Command::Select {
            session_id: Some(fixture.session.clone()),
            generation: 1,
        })
        .unwrap();
    until(
        &client,
        "fault-reader-baseline",
        |u| matches!(u,Update::Transcript(b) if b.baseline.is_some()),
    )
    .await;
    client
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_edit_rejects_delayed_status_from_the_truncated_history() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-edit-status-key");
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture = Fixture::start(
        app(axum::routing::post({
            let complete = complete.clone();
            let calls = calls.clone();
            move || {
                let complete = complete.clone();
                let number = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    match number {
                        1 => {
                            let gate = Arc::new(Notify::new());
                            gate.notify_one();
                            gated_response(prefix("Previous answer."), Some(tail("")), gate)
                        }
                        2 => {
                            gated_response(prefix("Edited answer "), Some(tail("ไทย 😀")), complete)
                        }
                        _ => panic!("unexpected edit replay {number}"),
                    }
                }
            }
        })),
        0,
    )
    .await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    let proxy = Proxy::start(&fixture, Fault::StatusReply).await;
    let client = through(&fixture, &proxy).await;
    send(&mut observer, &fixture.session).await;
    wait(&mut observer, |e| matches!(e, Event::RunFinished { .. })).await;
    future_cases::canonical(&client, "Previous answer.", &[]).await;
    let stored = snapshot(&mut observer, &fixture.session).await;
    let message_id = stored["messages"][0]["id"].as_str().unwrap().to_owned();
    proxy.arm.store(true, Ordering::SeqCst);
    proxy.reached(&client).await;
    assert!(proxy.value.lock().unwrap().as_ref().unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["content"] == "Previous answer."));
    observer.events.clear();
    observer
        .request(Request::EditMessage {
            session_id: fixture.session.clone(),
            message_id,
            content: "Edited user message".into(),
        })
        .await;
    wait(
        &mut observer,
        |e| matches!(e,Event::MessageDelta{delta,..} if delta=="Edited answer "),
    )
    .await;
    proxy.release.notify_one();
    until(&client, "edit-reader-new-baseline", |u| {
        if let Update::Transcript(b) = u {
            if let Some(s) = b.baseline {
                assert!(
                    !s.blocks
                        .iter()
                        .any(|block| block.markdown() == "Previous answer."),
                    "old metadata reinstated truncated history"
                );
                if s.blocks
                    .iter()
                    .any(|block| block.markdown() == "Edited user message")
                {
                    assert_eq!(s.blocks.len(), 1);
                    return true;
                }
            }
        }
        false
    })
    .await;
    complete.notify_one();
    future_cases::canonical(&client, "Edited answer ไทย 😀", &["Previous answer."]).await;
    let stored = snapshot(&mut observer, &fixture.session).await;
    assert_eq!(stored["messages"][0]["content"], "Edited user message");
    assert_eq!(assistant_messages(&stored).len(), 1);
    assert_eq!(
        assistant_messages(&stored)[0]["content"],
        "Edited answer ไทย 😀"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(client);
    drop(observer);
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_server_queue_saturation_disconnects_and_recovers_canonically() {
    // Exceed both the daemon's 4096-frame queue and large OS socket
    // buffers. A tiny delta keeps the canonical snapshot small; frame
    // count, not answer size, is what must saturate the server here.
    const PRESSURE_FRAMES: usize = 65_536;
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-server-queue-key");
    let burst = Arc::new(Notify::new());
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture=Fixture::start(app(axum::routing::post({let burst=burst.clone();let complete=complete.clone();let calls=calls.clone();move|| {
        let burst=burst.clone();let complete=complete.clone();calls.fetch_add(1,Ordering::SeqCst);
        async move {
            let chunk=encode(&[json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}})]);
            let chunks=VecDeque::from([(None,prefix("Before pressure ")),(Some(burst),chunk.repeat(PRESSURE_FRAMES)),(Some(complete),tail("END"))]);
            let stream=futures_util::stream::unfold(chunks,|mut chunks|async move {
                let (gate,text)=chunks.pop_front()?;if let Some(gate)=gate {gate.notified().await;}
                Some((Ok::<_,std::io::Error>(text),chunks))
            });
            axum::response::Response::builder().header("content-type","text/event-stream").body(axum::body::Body::from_stream(stream)).unwrap()
        }
    }})),0).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    observer
        .request(Request::Subscribe {
            sessions: Some(vec![fixture.session.clone()]),
            deltas: false,
            client: None,
        })
        .await;
    let proxy = Proxy::start(&fixture, Fault::ServerRead).await;
    let client = through(&fixture, &proxy).await;
    send(&mut observer, &fixture.session).await;
    future_cases::live(&client, "Before pressure ").await;
    let connection = client.connection();
    proxy.arm.store(true, Ordering::SeqCst);
    burst.notify_one();
    proxy.reached(&client).await;
    complete.notify_one();
    wait(&mut observer, |event| {
        matches!(event, Event::RunFinished { .. })
    })
    .await;
    proxy.release.notify_one();
    let expected = format!("Before pressure {}END", "x".repeat(PRESSURE_FRAMES));
    until(&client, "server-pressure-canonical-recovery", |u| {
        if let Update::Transcript(b) = u {
            if b.owner.connection != connection && b.owner.connection == client.connection() {
                if let Some(s) = b.baseline {
                    return s.blocks.iter().any(|block| block.markdown() == expected);
                }
            }
        }
        false
    })
    .await;
    assert!(
        proxy.upstream_eof.load(Ordering::SeqCst),
        "the upstream server, rather than the UI consumer, must end its saturated connection"
    );
    eprintln!(
        "reader_server_pressure_metrics {}",
        json!({"provider_delta_frames":PRESSURE_FRAMES,"canonical_payload_bytes":expected.len(),"server_closure_observed":proxy.closed_by.lock().unwrap().as_ref(),"provider_calls":calls.load(Ordering::SeqCst),"ui_consumer_deliberately_stalled":false})
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(client);
    drop(observer);
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_delayed_status_question_snapshot_cannot_reopen_an_answered_question() {
    use formless_desktop::questions::Outcome;
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-delayed-question-key");
    let complete = Arc::new(Notify::new());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let fixture=Fixture::start(app(axum::routing::post({let complete=complete.clone();let calls=calls.clone();move|| {
        let complete=complete.clone();let number=calls.fetch_add(1,Ordering::SeqCst)+1;
        async move {match number {
            1=> {
                let args=json!({"questions":[{"question":"Private delayed question?"}]});
                let gate=Arc::new(Notify::new());gate.notify_one();
                gated_response(prefix("Question pending. "),Some(encode(&[
                    json!({"type":"content_block_stop","index":0}),
                    json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"delayed-question-call","name":"ask_user"}}),
                    json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":args.to_string()}}),
                    json!({"type":"content_block_stop","index":1}),
                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":10}}),json!({"type":"message_stop"})
                ])),gate)
            }
            2=>gated_response(prefix("Answer confirmed "),Some(tail("done")),complete),
            _=>panic!("unexpected question replay {number}"),
        }}
    }})),0).await;
    let mut observer = Observer::connect(&fixture.endpoint, &fixture.session).await;
    let proxy = Proxy::start(&fixture, Fault::StatusReply).await;
    let client = through(&fixture, &proxy).await;
    send(&mut observer, &fixture.session).await;
    let mut open = None;
    until(&client, "delayed-question-open", |u| {
        if let Update::Questions(state) = u {
            open = state
                .open
                .into_iter()
                .find(|q| q.session_id == fixture.session);
        }
        open.is_some()
    })
    .await;
    let open = open.unwrap();
    proxy.arm.store(true, Ordering::SeqCst);
    proxy.reached(&client).await;
    assert!(
        proxy.value.lock().unwrap().as_ref().unwrap()["pending_events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["question_id"] == open.id)
    );
    let operation = uuid::Uuid::new_v4();
    client
        .send(Command::AnswerQuestion {
            connection: client.connection(),
            operation,
            question: Box::new(open.clone()),
            answers: vec![formless_protocol::QuestionAnswer {
                selected: vec![],
                text: Some("Private answer".into()),
            }],
        })
        .unwrap();
    until(&client,"delayed-question-answer-ack",|u|matches!(u,Update::QuestionAnswer{operation:got,outcome:Outcome::Accepted,..} if got==operation)).await;
    complete.notify_one();
    wait(&mut observer, |e| matches!(e, Event::RunFinished { .. })).await;
    proxy.release.notify_one();
    until(&client, "delayed-question-final-baseline", |u| {
        match u {
            Update::Questions(state) => assert!(
                !state.open.iter().any(|q| q.id == open.id),
                "stale question snapshot revived an acknowledged answer"
            ),
            Update::Transcript(b) => {
                assert!(
                    b.status.as_ref().is_none_or(|s| !s.running),
                    "stale running metadata crossed a retired basis"
                );
                if let Some(s) = b.baseline {
                    if s.blocks
                        .iter()
                        .any(|block| block.markdown() == "Answer confirmed done")
                    {
                        assert!(!s.running);
                        return true;
                    }
                }
            }
            _ => {}
        }
        false
    })
    .await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    drop(client);
    drop(observer);
    proxy.finish().await;
    fixture.finish().await;
}
