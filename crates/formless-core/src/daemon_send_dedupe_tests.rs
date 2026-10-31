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
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: "send-test".into(),
    };
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
            retry: Arc::new(tokio::sync::Notify::new()),
        },
    );
    (daemon, sid, base)
}
fn send(sid: &str, content: &str) -> Request {
    Request::SendMessage {
        session_id: sid.into(),
        content: content.into(),
        mode: SendMode::Send,
        client_msg_id: Some("same-key".into()),
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
    assert_eq!(result, json!({"steered": true}));
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
async fn two_framed_connections_with_same_key_each_append_once() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (daemon, sid, base) = fixture(&env);
    let (left, right) = tokio::join!(
        framed_client(&env, daemon.clone(), sid.clone()),
        framed_client(&env, daemon.clone(), sid.clone())
    );
    assert_eq!(left.0, left.1);
    assert_eq!(right.0, right.1);
    let messages = daemon.with_reader(|st| st.list_messages(&sid)).unwrap();
    assert_eq!(
        messages.len(),
        2,
        "the second connection's message was suppressed"
    );
    let clients: std::collections::HashSet<_> = messages
        .iter()
        .map(|m| {
            serde_json::from_str::<Value>(m.meta.as_ref().unwrap()).unwrap()["client"]["conn"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(clients.len(), 2, "each client should append exactly once");
    assert!(
        daemon.sent_keys.locked().is_empty(),
        "closed clients' dedupe state was retained"
    );
    release_fixture(&env, daemon).await;
    let _ = std::fs::remove_dir_all(base);
}
