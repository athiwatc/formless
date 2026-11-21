use super::*;
use formless_desktop::run_actions::{Action, Kind, Ledger, Owner, Status, Target};

async fn selected(f: &Fixture) -> Client {
    let c = Client::start(f.endpoint.clone());
    until(&c, "actions connected", |u| {
        matches!(u, Update::Connection { ready: true, .. })
    })
    .await;
    c.send(Command::Select {
        session_id: Some(f.session.clone()),
        generation: 1,
    })
    .unwrap();
    until(&c, "actions selected", |u| {
        matches!(u, Update::Snapshot { generation: 1, .. })
    })
    .await;
    c
}

async fn target(c: &Client, session: &str) -> (Owner, Target) {
    let owner = Owner {
        client: c.instance(),
        connection: c.connection(),
        epoch: c.epoch(),
        generation: 1,
        session: session.into(),
    };
    let operation = uuid::Uuid::new_v4();
    c.send(Command::ReadRunAction {
        owner: owner.clone(),
        operation,
    })
    .unwrap();
    let mut target = None;
    until(c, "canonical action target", |u| {
        if let Update::RunActionTarget {
            operation: op,
            result,
            ..
        } = u
        {
            if op == operation {
                target = Some(result.unwrap());
                return true;
            }
        }
        false
    })
    .await;
    (owner, target.unwrap())
}

async fn perform(c: &Client, session: &str, kind: Kind, text: &str) -> String {
    c.send(Command::Refresh).unwrap();
    until(c,"current run state before review",|u| matches!(u,Update::Snapshot { snapshot,.. } if snapshot.running == matches!(kind, Kind::ProviderRetry | Kind::Queue(_)))).await;
    let (owner, target) = target(c, session).await;
    let operation = uuid::Uuid::new_v4();
    let ledger = Arc::new(Ledger::default());
    let d = ledger
        .reserve(Action {
            owner,
            target,
            kind,
            text: text.into(),
            operation,
        })
        .unwrap();
    c.send(Command::PrepareRunAction(d)).unwrap();
    until(c, "exact run action review", |u| {
        if let Update::RunActionReview {
            operation: op,
            quote,
            ..
        } = u
        {
            if op == operation {
                assert_eq!(
                    quote.is_some(),
                    matches!(
                        kind,
                        Kind::Edit
                            | Kind::Retry
                            | Kind::Queue(formless_desktop::message_queue::Operation::Enqueue)
                    )
                );
                return true;
            }
        }
        false
    })
    .await;
    c.send(Command::ConfirmRunAction(operation)).unwrap();
    until(
        c,
        "run action consent",
        |u| matches!(u, Update::RunActionReady { operation:op,.. } if op == operation),
    )
    .await;
    c.send(Command::CommitRunAction(operation)).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ledger.status() {
                Status::Applied(notice) => break notice,
                Status::Rejected(e) => panic!("action {kind:?} rejected: {e}"),
                Status::Unknown => panic!("action {kind:?} acknowledgement was ambiguous"),
                _ => {
                    c.drain().for_each(drop);
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    })
    .await
    .unwrap()
}

async fn idle(observer: &mut Observer, session: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let v = snapshot(observer, session).await;
            if v["running"] == false {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

fn response(body: &Value) -> axum::response::Response {
    let (content_type, text) = if body["stream"] == true {
        (
            "text/event-stream",
            format!("{}{}", prefix("Private action answer "), tail("complete")),
        )
    } else {
        ("application/json", json!({"id":"private-summary","type":"message","role":"assistant","content":[{"type":"text","text":"Private checkpoint summary"}],"stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":10}}).to_string())
    };
    axum::response::Response::builder()
        .header("content-type", content_type)
        .body(axum::body::Body::from(text))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_queue_rpc_receipts_edit_revisions_and_order_are_canonical() {
    use formless_desktop::message_queue::Operation;
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-queue-test-key");
    let gate = Arc::new(Notify::new());
    let f =
        Fixture::start(
            app(axum::routing::post({
                let gate = gate.clone();
                move || {
                    let gate = gate.clone();
                    async move {
                        gated_response(prefix("Active queue test"), Some(tail(" complete")), gate)
                    }
                }
            })),
            0,
        )
        .await;
    let mut observer = Observer::connect(&f.endpoint, &f.session).await;
    send(&mut observer, &f.session).await;
    wait(&mut observer, |e| matches!(e, Event::MessageDelta { .. })).await;
    let c = selected(&f).await;
    for text in ["First queued", "Second queued"] {
        perform(&c, &f.session, Kind::Queue(Operation::Enqueue), text).await;
    }
    let queued = snapshot(&mut observer, &f.session).await;
    let first = queued["queued_items"][0]["id"].as_i64().unwrap();
    let second = queued["queued_items"][1]["id"].as_i64().unwrap();
    let revision = queued["queued_items"][0]["revision"].as_i64().unwrap();
    perform(
        &c,
        &f.session,
        Kind::Queue(Operation::Edit(first)),
        "Changed queued text",
    )
    .await;
    assert!(f
        .daemon
        .handle(Request::EditQueuedIfCurrent {
            session_id: f.session.clone(),
            id: first,
            revision,
            content: "stale edit".into()
        })
        .await
        .is_err());
    perform(&c, &f.session, Kind::Queue(Operation::Up(second)), "").await;
    assert_eq!(
        snapshot(&mut observer, &f.session).await["queued_items"][0]["id"],
        second
    );
    perform(&c, &f.session, Kind::Queue(Operation::Down(second)), "").await;
    let ordered = snapshot(&mut observer, &f.session).await;
    assert_eq!(ordered["queued_items"][0]["content"], "Changed queued text");
    assert_eq!(ordered["queued_items"][1]["id"], second);
    for id in [first, second] {
        perform(&c, &f.session, Kind::Queue(Operation::Remove(id)), "").await;
    }
    assert!(snapshot(&mut observer, &f.session).await["queued_items"]
        .as_array()
        .unwrap()
        .is_empty());
    gate.notify_one();
    idle(&mut observer, &f.session).await;
    drop(c);
    drop(observer);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_reviewed_actions_use_real_rpc_receipts_and_fake_provider() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-run-actions-key");
    let calls: Arc<Mutex<Vec<Value>>> = Arc::default();
    let route = axum::routing::post({
        let calls = calls.clone();
        move |axum::Json(body): axum::Json<Value>| {
            let calls = calls.clone();
            async move {
                calls.lock().unwrap().push(body.clone());
                response(&body)
            }
        }
    });
    let f = Fixture::start(app(route), 0).await;
    let mut observer = Observer::connect(&f.endpoint, &f.session).await;
    send(&mut observer, &f.session).await;
    wait(&mut observer, |e| matches!(e, Event::RunFinished { .. })).await;
    let original = idle(&mut observer, &f.session).await;
    let c = selected(&f).await;
    until(&c, "live context", |u| {
        matches!(u, Update::LiveContext { result: Ok(_), .. })
    })
    .await;
    perform(&c, &f.session, Kind::Good, "Useful").await;
    let rated = observer
        .request(Request::RunMetrics {
            limit: Some(10),
            session_id: Some(f.session.clone()),
            since: None,
        })
        .await;
    assert!(rated["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["run_id"] == original["latest_run"]["run_id"] && r["rating"] == "good"));
    perform(&c, &f.session, Kind::Bad, "Needs work").await;
    let rated = observer
        .request(Request::RunMetrics {
            limit: Some(10),
            session_id: Some(f.session.clone()),
            since: None,
        })
        .await;
    assert!(rated["runs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["run_id"] == original["latest_run"]["run_id"] && r["rating"] == "bad"));
    perform(&c, &f.session, Kind::Edit, "Edited exact user message 😀").await;
    let edited = idle(&mut observer, &f.session).await;
    assert!(edited["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "user" && m["content"] == "Edited exact user message 😀"));
    perform(&c, &f.session, Kind::Retry, "alternate-private-model").await;
    let retried = idle(&mut observer, &f.session).await;
    assert_ne!(
        edited["latest_run"]["run_id"],
        retried["latest_run"]["run_id"]
    );
    assert!(calls
        .lock()
        .unwrap()
        .iter()
        .any(|body| body["model"] == "alternate-private-model"));
    let listed = observer.request(Request::ListSessions).await;
    assert!(
        listed.as_array().unwrap().len() >= 2,
        "retry preserves original answer in a fork"
    );
    let directory = f
        .root
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    perform(&c, &f.session, Kind::Cwd, &directory).await;
    assert_eq!(
        snapshot(&mut observer, &f.session).await["session"]["cwd"],
        directory
    );
    perform(&c, &f.session, Kind::Cwd, "").await;
    assert!(snapshot(&mut observer, &f.session).await["session"]["cwd"].is_null());
    perform(&c, &f.session, Kind::Compact, "Keep decisions").await;
    wait(&mut observer, |e| {
        matches!(e, Event::CompactionNotice { .. })
    })
    .await;
    assert!(calls.lock().unwrap().len() >= 4);
    perform(&c, &f.session, Kind::Delete, "").await;
    let listed = observer.request(Request::ListSessions).await;
    assert!(!listed
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == f.session));
    drop(c);
    drop(observer);
    f.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_provider_retry_continues_one_stalled_run_without_a_forecast() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-run-actions-key");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let gate = Arc::new(Notify::new());
    let route = axum::routing::post({
        let calls = calls.clone();
        move |axum::Json(body): axum::Json<Value>| {
            let number = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let gate = gate.clone();
            async move {
                if number == 0 {
                    gated_response(prefix("Discard this provisional attempt"), None, gate)
                } else {
                    response(&body)
                }
            }
        }
    });
    let f = Fixture::start_with_stall(app(route), 0, 1).await;
    let mut observer = Observer::connect(&f.endpoint, &f.session).await;
    let c = selected(&f).await;
    send(&mut observer, &f.session).await;
    until(&c, "provider stalled activity", |u| {
        matches!(
            u,
            Update::RunActivity {
                event: Event::ProviderStalled { .. },
                ..
            }
        )
    })
    .await;
    let before = snapshot(&mut observer, &f.session).await;
    perform(&c, &f.session, Kind::ProviderRetry, "").await;
    let after = idle(&mut observer, &f.session).await;
    assert_eq!(
        before["latest_run"]["run_id"],
        after["latest_run"]["run_id"]
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(!assistant_messages(&after)
        .iter()
        .any(|m| m["content"].as_str().unwrap().contains("Discard")));
    assert_eq!(
        observer
            .request(Request::RetryProviderRequest {
                session_id: f.session.clone()
            })
            .await,
        json!({"retrying":false})
    );
    drop(c);
    drop(observer);
    f.finish().await;
}
