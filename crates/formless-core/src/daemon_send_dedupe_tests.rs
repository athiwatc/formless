use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

async fn release_fixture(_env: &crate::credential_env::EnvGuard, daemon: Arc<Daemon>) {
    let pending = Arc::downgrade(&daemon);
    drop(daemon);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while pending.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("send fixture left daemon tasks running");
}

fn fixture(_env: &crate::credential_env::EnvGuard) -> (Arc<Daemon>, String, std::path::PathBuf) {
    let base = std::path::PathBuf::from(format!("/tmp/fl-send-{}", uuid::Uuid::new_v4()));
    let paths = fixture_paths(&base);
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let sid = daemon
        .with_store(|st| Ok(st.create_session(None)?.id))
        .unwrap();
    // An inert active Run exercises steering without any provider/network call.
    let (steer_tx, _rx) = mpsc::unbounded_channel();
    daemon.runs.locked().insert(
        sid.clone(),
        RunCtl {
            run_id: "test-run".into(),
            cancel: CancellationToken::new(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            steer_tx,
            steering: Arc::new(Default::default()),
            retry: Arc::new(provider_retry_daemon::ProviderRetry::default()),
        },
    );
    (daemon, sid, base)
}

fn fixture_paths(base: &std::path::Path) -> Paths {
    Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: "send-test".into(),
    }
}
fn send(sid: &str, content: &str) -> Request {
    Request::SendMessage {
        session_id: sid.into(),
        content: content.into(),
        mode: SendMode::Send,
        client_msg_id: Some("same-key".into()),
    }
}
fn outbox_send(
    sid: &str,
    content: &str,
    client_msg_id: &str,
    expected_attachments: Vec<Value>,
) -> Request {
    Request::SendOutboxMessage {
        session_id: sid.into(),
        content: content.into(),
        mode: SendMode::Steer,
        expected_attachments,
        expected_run_id: Some("test-run".into()),
        client_msg_id: client_msg_id.into(),
    }
}

#[tokio::test]
async fn concurrent_same_identity_waits_for_claim_and_survives_lost_first_waiter() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (daemon, sid, base) = fixture(&env);
    let mut first = Box::pin(CLIENT.scope((42, "test".into()), daemon.handle(send(&sid, "first"))));
    // The claim is installed before execution; both calls must wait for the
    // same detached send rather than checking an empty cache independently.
    assert!(futures_util::poll!(first.as_mut()).is_pending());
    // Long-running in-flight sends cannot expire before producing an outcome.
    daemon.sent_keys.locked().values_mut().next().unwrap().at =
        std::time::Instant::now() - std::time::Duration::from_secs(601);
    let mut duplicate =
        Box::pin(CLIENT.scope((42, "test".into()), daemon.handle(send(&sid, "duplicate"))));
    assert!(futures_util::poll!(duplicate.as_mut()).is_pending());
    assert_eq!(daemon.sent_keys.locked().len(), 1);
    assert!(daemon
        .sent_keys
        .locked()
        .values()
        .next()
        .unwrap()
        .outcome
        .borrow()
        .is_none());
    drop(first); // Lost response/cancelled waiter must not lose the claimed send.
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), duplicate)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, json!({"steered": true, "run_id": "test-run"}));
    let replay = CLIENT
        .scope(
            (42, "test".into()),
            daemon.handle(send(&sid, "another retry")),
        )
        .await
        .unwrap();
    assert_eq!(result, replay);
    let messages = daemon.with_reader(|st| st.list_messages(&sid)).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "first");
    assert_eq!(
        serde_json::from_str::<Value>(messages[0].meta.as_ref().unwrap()).unwrap()["client"]
            ["conn"],
        42
    );
    release_fixture(&env, daemon).await;
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn concurrent_failure_is_cached_and_replayed() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (daemon, _sid, base) = fixture(&env);
    let (first, second) = CLIENT
        .scope((42, "test".into()), async {
            tokio::join!(
                daemon.handle(send("missing", "first")),
                daemon.handle(send("missing", "second"))
            )
        })
        .await;
    assert_eq!(first.unwrap_err().to_string(), "session not found");
    assert_eq!(second.unwrap_err().to_string(), "session not found");
    assert!(matches!(
        &*daemon
            .sent_keys
            .locked()
            .values()
            .next()
            .unwrap()
            .outcome
            .borrow(),
        Some(Err(_))
    ));
    release_fixture(&env, daemon).await;
    let _ = std::fs::remove_dir_all(base);
}

async fn framed_client(
    _env: &crate::credential_env::EnvGuard,
    daemon: Arc<Daemon>,
    sid: String,
) -> (Value, Value) {
    let (client, server) = tokio::io::duplex(64 * 1024);
    let connection = tokio::spawn(daemon.clone().handle_conn(server));
    let (read, mut write) = tokio::io::split(client);
    let mut lines = tokio::io::BufReader::new(read).lines();
    let mut frames = String::new();
    for id in [1, 2] {
        frames.push_str(
            &serde_json::to_string(&RequestEnvelope {
                id,
                protocol: PROTOCOL_VERSION,
                request: send(&sid, "one per client"),
            })
            .unwrap(),
        );
        frames.push('\n');
    }
    // Pipeline retries before reading either response: the actual connection
    // parser/worker, task-local identity and writer all participate.
    write.write_all(frames.as_bytes()).await.unwrap();
    let responses = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut replies = Vec::new();
        while replies.len() < 2 {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("connection closed before reply");
            if let ServerMessage::Response {
                ok, result, error, ..
            } = serde_json::from_str(&line).unwrap()
            {
                assert!(ok, "{error:?}");
                replies.push(result.unwrap());
            }
        }
        replies
    })
    .await
    .expect("send replies never arrived");
    drop(lines);
    drop(write);
    tokio::time::timeout(std::time::Duration::from_secs(5), connection)
        .await
        .expect("connection worker never stopped")
        .unwrap();
    (responses[0].clone(), responses[1].clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_reuses_the_same_framed_send_receipt() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (daemon, sid, base) = fixture(&env);
    let left = framed_client(&env, daemon.clone(), sid.clone()).await;
    let right = framed_client(&env, daemon.clone(), sid.clone()).await;
    assert_eq!(left.0, left.1);
    assert_eq!(right.0, right.1);
    assert_eq!(left.0, right.0);
    let messages = daemon.with_reader(|st| st.list_messages(&sid)).unwrap();
    assert_eq!(
        messages.len(),
        1,
        "the reconnect appended the same action twice"
    );
    assert_eq!(
        daemon.sent_keys.locked().len(),
        1,
        "the retry receipt must survive connection close"
    );
    release_fixture(&env, daemon).await;
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn durable_outbox_receipt_survives_expiry_and_daemon_restart() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (daemon, sid, base) = fixture(&env);
    let first = daemon
        .handle(outbox_send(&sid, "first text", "durable-action", vec![]))
        .await
        .unwrap();
    let other_sid = daemon
        .with_store(|st| {
            st.create_session(Some("other".into()))
                .map(|session| session.id)
        })
        .unwrap();
    assert_eq!(
        daemon
            .with_reader(|st| st.durable_send_receipt(&other_sid, "durable-action"))
            .unwrap(),
        None
    );
    daemon.sent_keys.locked().values_mut().next().unwrap().at =
        std::time::Instant::now() - std::time::Duration::from_secs(601);
    let after_expiry = daemon
        .handle(outbox_send(&sid, "retry text", "durable-action", vec![]))
        .await
        .unwrap();
    assert_eq!(after_expiry, first);
    assert_eq!(
        daemon
            .with_reader(|st| st.list_messages(&sid))
            .unwrap()
            .len(),
        1
    );

    release_fixture(&env, daemon).await;
    let restarted = Arc::new(Daemon::new(fixture_paths(&base)).unwrap());
    let after_restart = restarted
        .handle(outbox_send(
            &sid,
            "retry after restart",
            "durable-action",
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(after_restart, first);
    let messages = restarted.with_reader(|st| st.list_messages(&sid)).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "first text");
    release_fixture(&env, restarted).await;
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn deferred_steer_never_starts_a_run_when_its_target_has_ended() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (daemon, sid, base) = fixture(&env);
    daemon.runs.locked().remove(&sid);
    let error = daemon
        .handle(outbox_send(&sid, "stale steer", "stale-steer", vec![]))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Steer target Run is no longer active"),
        "{error}"
    );
    assert!(daemon
        .with_reader(|st| st.list_messages(&sid))
        .unwrap()
        .is_empty());
    assert!(!daemon.runs.locked().contains_key(&sid));
    release_fixture(&env, daemon).await;
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn outbox_attachment_inventory_is_exact_and_queue_does_not_consume_attachments() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (daemon, sid, base) = fixture(&env);
    let attachment = json!({"id":"file-1","name":"file.txt","mime":"text/plain","bytes":8});
    daemon
        .with_store(|st| st.add_pending_attachment(&sid, &attachment))
        .unwrap();

    let stale_inventory = daemon
        .handle(outbox_send(&sid, "old draft", "old-inventory", vec![]))
        .await
        .unwrap_err();
    assert!(stale_inventory
        .to_string()
        .contains("pending attachments changed"));
    let queue_with_attachment = Request::SendOutboxMessage {
        session_id: sid.clone(),
        content: "queued file".into(),
        mode: SendMode::Queue,
        expected_attachments: vec![attachment.clone()],
        expected_run_id: None,
        client_msg_id: "queue-file".into(),
    };
    assert!(daemon.handle(queue_with_attachment).await.is_err());
    assert_eq!(
        daemon
            .with_reader(|st| st.pending_attachment_inventory(&sid))
            .unwrap(),
        vec![attachment.clone()]
    );
    assert!(daemon
        .with_reader(|st| st.list_messages(&sid))
        .unwrap()
        .is_empty());
    daemon
        .runs
        .locked()
        .get(&sid)
        .unwrap()
        .cancel_flag
        .store(true, Ordering::SeqCst);
    let queued_steer = daemon
        .handle(outbox_send(
            &sid,
            "cancelled-run steer",
            "cancelled-steer",
            vec![attachment.clone()],
        ))
        .await
        .unwrap_err();
    assert!(queued_steer
        .to_string()
        .contains("attachment-bound outbox message"));
    daemon
        .runs
        .locked()
        .get(&sid)
        .unwrap()
        .cancel_flag
        .store(false, Ordering::SeqCst);

    daemon
        .handle(outbox_send(
            &sid,
            "bound draft",
            "matching-inventory",
            vec![attachment.clone()],
        ))
        .await
        .unwrap();
    let messages = daemon.with_reader(|st| st.list_messages(&sid)).unwrap();
    assert_eq!(messages.len(), 1);
    let metadata: Value = serde_json::from_str(messages[0].meta.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["attachments"], json!([attachment]));
    assert!(daemon
        .with_reader(|st| st.pending_attachment_inventory(&sid))
        .unwrap()
        .is_empty());
    release_fixture(&env, daemon).await;
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test]
async fn queued_outbox_message_does_not_consume_attachments_added_later() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (daemon, sid, base) = fixture(&env);
    daemon
        .handle(Request::SendOutboxMessage {
            session_id: sid.clone(),
            content: "queued without files".into(),
            mode: SendMode::Queue,
            expected_attachments: vec![],
            expected_run_id: None,
            client_msg_id: "queued-no-files".into(),
        })
        .await
        .unwrap();
    let later_attachment =
        json!({"id":"later-file","name":"later.txt","mime":"text/plain","bytes":5});
    daemon
        .with_store(|st| st.add_pending_attachment(&sid, &later_attachment))
        .unwrap();

    let queued_message = daemon
        .with_store(|st| {
            st.transaction(|st| {
                let (content, no_pending_attachments) =
                    st.pop_queued_message_with_policy(&sid)?.unwrap();
                Daemon::append_user_message_store_with_policy(
                    st,
                    &sid,
                    &content,
                    None,
                    !no_pending_attachments,
                )
            })
        })
        .unwrap();
    let metadata: Value =
        serde_json::from_str(queued_message.meta.as_deref().unwrap_or("{}")).unwrap();
    assert!(metadata.get("attachments").is_none());
    assert_eq!(
        daemon
            .with_reader(|st| st.pending_attachment_inventory(&sid))
            .unwrap(),
        vec![later_attachment]
    );
    release_fixture(&env, daemon).await;
    let _ = std::fs::remove_dir_all(base);
}
