use super::*;

struct Fixture {
    daemon: Arc<Daemon>,
    sid: String,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-reviewed-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let mut config = Config::default();
        config.provider.auto_test_models = false;
        config.naming.enabled = false;
        config.skills.auto_create = false;
        config.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let sid = daemon
            .with_store(|st| Ok(st.create_session(None)?.id))
            .unwrap();
        let (steer_tx, _rx) = mpsc::unbounded_channel();
        daemon.runs.locked().insert(
            sid.clone(),
            RunCtl {
                run_id: "inert-test-run".into(),
                cancel: CancellationToken::new(),
                cancel_flag: Arc::new(AtomicBool::new(false)),
                steer_tx,
                steering: Arc::new(Default::default()),
                retry: Arc::new(provider_retry_daemon::ProviderRetry::default()),
            },
        );
        Self { daemon, sid, root }
    }
    fn read(&self) -> Vec<Value> {
        self.daemon
            .with_reader(|st| st.pending_attachment_inventory(&self.sid))
            .unwrap()
    }
    fn add(&self, value: &Value) {
        self.daemon
            .with_store(|st| st.add_pending_attachment(&self.sid, value))
            .unwrap();
    }
    fn request(&self, key: &str, expected: Vec<Value>) -> Request {
        Request::SendReviewedMessage {
            session_id: self.sid.clone(),
            content: "exact draft ไทย😀".into(),
            mode: SendMode::Steer,
            expected_attachments: expected,
            client_msg_id: key.into(),
        }
    }
    fn untouched(&self, expected: &[Value]) {
        assert_eq!(self.read(), expected);
        assert!(self
            .daemon
            .with_reader(|st| st.list_messages(&self.sid))
            .unwrap()
            .is_empty());
        assert_eq!(
            self.daemon
                .with_reader(|st| st.queued_count(&self.sid))
                .unwrap(),
            0
        );
    }
    async fn finish(self) {
        let weak = Arc::downgrade(&self.daemon);
        drop(self.daemon);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while weak.strong_count() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
fn record(name: &str) -> Value {
    json!({"id":name,"name":name,"path":"/not/read/or/resolved","mime":"text/plain","bytes":5,"future":{"preserved":true}})
}

#[tokio::test]
async fn reviewed_send_compares_all_records_order_duplicates_and_unknown_fields_before_consuming() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    for row in [record("a"), record("b"), record("a")] {
        f.add(&row);
    }
    let expected = f.read();
    for n in 0..6 {
        let mut stale = expected.clone();
        match n {
            0 => {
                stale.pop();
            }
            1 => stale.reverse(), // palindrome below is altered too
            2 => stale[0]["future"]["preserved"] = json!(false),
            3 => stale[0]["path"] = json!("/changed"),
            4 => stale.clear(),
            _ => stale.push(record("a")),
        }
        if n == 1 {
            stale.swap(0, 1);
        }
        let err = f
            .daemon
            .handle(f.request(&format!("reject-{n}"), stale))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("pending attachments changed"),
            "{err:#}"
        );
        f.untouched(&expected);
    }
    let result = f
        .daemon
        .handle(f.request("accepted", expected.clone()))
        .await
        .unwrap();
    assert_eq!(result["steered"], true);
    assert!(f.read().is_empty());
    let messages = f.daemon.with_reader(|st| st.list_messages(&f.sid)).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "exact draft ไทย😀");
    let metadata: Value = serde_json::from_str(messages[0].meta.as_ref().unwrap()).unwrap();
    assert_eq!(metadata["attachments"], json!(expected));
    assert_eq!(
        f.daemon
            .handle(f.request("accepted", vec![]))
            .await
            .unwrap(),
        result,
        "a retry replays the first receipt rather than sending again"
    );
    assert_eq!(
        f.daemon
            .with_reader(|st| st.list_messages(&f.sid))
            .unwrap()
            .len(),
        1
    );
    f.finish().await;
}

#[tokio::test]
async fn reviewed_send_duplicate_waiters_share_claim_but_two_new_actions_cannot_consume_same_inventory(
) {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add(&record("one"));
    let expected = f.read();
    let mut abandoned = Box::pin(f.daemon.handle(f.request("first", expected.clone())));
    assert!(futures_util::poll!(abandoned.as_mut()).is_pending());
    drop(abandoned);
    let (first, second) = tokio::join!(
        f.daemon.handle(f.request("first", expected.clone())),
        f.daemon.handle(f.request("second", expected))
    );
    assert!(first.is_ok());
    assert!(second
        .unwrap_err()
        .to_string()
        .contains("pending attachments changed"));
    assert_eq!(
        f.daemon
            .with_reader(|st| st.list_messages(&f.sid))
            .unwrap()
            .len(),
        1
    );
    f.finish().await;
}

#[tokio::test]
async fn reviewed_send_queue_closing_cancelled_deleted_and_invalid_inputs_fail_without_mutation() {
    let _env = crate::credential_env::EnvGuard::isolated();
    for mode in 0..12 {
        let f = Fixture::new();
        f.add(&record("one"));
        let expected = f.read();
        let mut request = f.request("rejected", expected.clone());
        if let Request::SendReviewedMessage {
            session_id,
            mode: send_mode,
            expected_attachments,
            client_msg_id,
            ..
        } = &mut request
        {
            match mode {
                0 => *send_mode = SendMode::Queue,
                1 => f.daemon.runs.locked().get(&f.sid).unwrap().cancel.cancel(),
                2 => f
                    .daemon
                    .runs
                    .locked()
                    .get(&f.sid)
                    .unwrap()
                    .cancel_flag
                    .store(true, Ordering::SeqCst),
                3 => {
                    f.daemon
                        .with_store(|st| st.soft_delete_session(&f.sid))
                        .unwrap();
                }
                4 => *expected_attachments = vec![Value::Null],
                5 => {
                    *expected_attachments = vec![record("over"); PENDING_ATTACHMENT_MAX_ENTRIES + 1]
                }
                6 => {
                    *expected_attachments =
                        vec![json!({"huge":"x".repeat(PENDING_ATTACHMENT_METADATA_BYTES+1)})]
                }
                7 => session_id.push('\n'),
                8 => client_msg_id.clear(),
                9 => {
                    f.daemon.runs.locked().remove(&f.sid);
                    *send_mode = SendMode::Queue;
                }
                10 => {
                    f.daemon.runs.locked().remove(&f.sid);
                    expected_attachments.clear();
                }
                _ => f.daemon.close_run_steering(&f.sid, "inert-test-run"),
            }
        }
        assert!(f.daemon.handle(request).await.is_err(), "mode {mode}");
        f.untouched(&expected);
        f.finish().await;
    }
}

#[tokio::test]
async fn reviewed_send_rejects_both_run_state_changes_before_any_write() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add(&record("confirmed"));
    let expected = f.read();

    let ping = f.daemon.handle(Request::Ping).await.unwrap();
    assert!(ping["features"]
        .as_array()
        .unwrap()
        .iter()
        .any(|feature| feature == formless_protocol::REVIEWED_SEND_MODE_FEATURE));

    // An idle Send was confirmed before another client started a Run.
    let mut send = f.request("confirmed-idle", expected.clone());
    let Request::SendReviewedMessage { mode, .. } = &mut send else {
        unreachable!();
    };
    *mode = SendMode::Send;
    let error = f.daemon.handle(send).await.unwrap_err();
    assert!(error.to_string().contains("Session became active"));
    f.untouched(&expected);

    // A Steer was confirmed before the Run ended. The hook keeps a
    // regression from appending or launching real Provider work.
    f.daemon.runs.locked().remove(&f.sid);
    *f.daemon.forecast_admission_test.locked() = Some(Arc::new(|| {
        panic!("reviewed Steer entered new Run admission");
    }));
    let error = f
        .daemon
        .handle(f.request("confirmed-active", expected.clone()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Session became idle"));
    f.untouched(&expected);
    assert!(f
        .daemon
        .with_reader(|st| st.last_run(&f.sid))
        .unwrap()
        .is_none());
    assert!(f.daemon.runs.locked().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn reviewed_send_exact_steer_refuses_replacement_and_separates_legacy_receipts() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add(&record("same-inventory"));
    let expected = f.read();
    let inspect = || Request::ReadSteerReview {
        session_id: f.sid.clone(),
    };
    let snapshot = || Request::GetSession { id: f.sid.clone() };
    let review = f.daemon.handle(inspect()).await.unwrap();
    assert_eq!(review["session_id"], f.sid);
    assert_eq!(review["attachments"], json!(expected));
    let old_run = review["run_id"].as_str().unwrap();
    assert_eq!(
        f.daemon.handle(snapshot()).await.unwrap()["steerable_run_id"],
        old_run
    );
    f.daemon.runs.locked().get_mut(&f.sid).unwrap().run_id = "replacement-run".into();
    let replacement = f.daemon.handle(inspect()).await.unwrap();
    assert_eq!(replacement["run_id"], "replacement-run");
    assert_eq!(replacement["attachments"], review["attachments"]);
    let steer = |run: &str, key: &str| Request::SendReviewedSteer {
        session_id: f.sid.clone(),
        run_id: run.into(),
        content: "bound message ไทย".into(),
        expected_attachments: expected.clone(),
        client_msg_id: key.into(),
    };
    let error = f.daemon.handle(steer(old_run, "stale")).await.unwrap_err();
    assert!(error
        .to_string()
        .contains("Reviewed Run is no longer active"));
    f.untouched(&expected);
    // A v1 failure under this same key cannot supply the bound RPC's receipt.
    assert!(f
        .daemon
        .handle(f.request("shared-key", Vec::new()))
        .await
        .is_err());
    let receipt = f
        .daemon
        .handle(steer("replacement-run", "shared-key"))
        .await
        .unwrap();
    assert_eq!(receipt, json!({"steered":true,"run_id":"replacement-run"}));
    assert_eq!(
        f.daemon
            .handle(steer("replacement-run", "shared-key"))
            .await
            .unwrap(),
        receipt
    );
    let messages = f.daemon.with_reader(|st| st.list_messages(&f.sid)).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "bound message ไทย");
    let metadata: Value = serde_json::from_str(messages[0].meta.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["attachments"], json!(expected));
    assert_eq!(metadata["steer"]["run_id"], "replacement-run");
    assert!(f.read().is_empty());
    f.daemon.close_run_steering(&f.sid, "replacement-run");
    assert!(f.daemon.handle(inspect()).await.is_err());
    assert!(f.daemon.handle(snapshot()).await.unwrap()["steerable_run_id"].is_null());
    f.finish().await;
}

#[tokio::test]
async fn reviewed_send_never_replays_a_legacy_send_ack_with_the_same_key() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add(&record("one"));
    let expected = f.read();
    f.daemon
        .handle(Request::SendMessage {
            session_id: f.sid.clone(),
            content: "legacy".into(),
            mode: SendMode::Steer,
            client_msg_id: Some("collision".into()),
        })
        .await
        .unwrap();
    assert!(f
        .daemon
        .handle(f.request("collision", expected))
        .await
        .unwrap_err()
        .to_string()
        .contains("pending attachments changed"));
    assert_eq!(
        f.daemon
            .with_reader(|st| st.list_messages(&f.sid))
            .unwrap()
            .len(),
        1
    );
    f.finish().await;
}

#[test]
fn reviewed_send_append_failure_rolls_back_message_and_attachment_consumption() {
    let st = Store::open_in_memory().unwrap();
    let sid = st.create_session(None).unwrap().id;
    let expected = vec![record("rollback")];
    st.add_pending_attachment(&sid, &expected[0]).unwrap();
    let result: Result<()> = st.transaction(|st| {
        reviewed_send::check(st, &sid, Some(&expected))?;
        Daemon::append_user_message_store(st, &sid, "draft", None)?;
        anyhow::bail!("later admission failed")
    });
    assert!(result.is_err());
    assert_eq!(st.pending_attachment_inventory(&sid).unwrap(), expected);
    assert!(st.list_messages(&sid).unwrap().is_empty());
}

#[tokio::test]
async fn reviewed_send_sqlite_writer_cannot_insert_between_comparison_and_consumption() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add(&record("reviewed"));
    let expected = f.read();
    let path = f.daemon.paths.db_file();
    let sid = f.sid.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let other = Store::open(&path).unwrap();
        ready_tx.send(()).unwrap();
        go_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        attempt_tx.send(()).unwrap();
        let result = other.add_pending_attachment(&sid, &record("late"));
        done_tx.send(result).unwrap();
    });
    ready_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    f.daemon
        .with_store(|st| {
            st.transaction(|st| {
                reviewed_send::check(st, &f.sid, Some(&expected))?;
                go_tx.send(()).unwrap();
                attempt_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                assert!(
                    matches!(
                        done_rx.recv_timeout(std::time::Duration::from_millis(50)),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    ),
                    "separate SQLite writer crossed the admission transaction"
                );
                Daemon::append_user_message_store(st, &f.sid, "reviewed", None)?;
                Ok(())
            })
        })
        .unwrap();
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .unwrap();
    writer.join().unwrap();
    assert_eq!(f.read(), vec![record("late")]);
    let messages = f.daemon.with_reader(|st| st.list_messages(&f.sid)).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(messages[0].meta.as_ref().unwrap()).unwrap()["attachments"],
        json!(expected)
    );
    f.finish().await;
}

#[tokio::test]
async fn reviewed_send_corrupt_stored_inventory_is_not_skipped_or_consumed() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add(&record("valid"));
    f.add(&Value::Null);
    assert!(f
        .daemon
        .handle(f.request("strict", vec![record("valid")]))
        .await
        .is_err());
    assert_eq!(
        f.daemon
            .with_reader(|st| st.pending_attachments(&f.sid))
            .unwrap(),
        vec![record("valid"), Value::Null]
    );
    assert!(f
        .daemon
        .with_reader(|st| st.list_messages(&f.sid))
        .unwrap()
        .is_empty());
    f.finish().await;
}

#[tokio::test]
async fn reviewed_send_real_framing_rejects_peer_change_and_replays_one_connection_receipt() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add(&record("first"));
    let (client, server) = tokio::io::duplex(256 * 1024);
    let worker = tokio::spawn(f.daemon.clone().handle_conn(server));
    let (read, mut write) = tokio::io::split(client);
    let mut lines = tokio::io::BufReader::new(read).lines();
    // The envelope parser, per-connection worker, task-local attribution and
    // response writer all participate; no direct response injection.
    let mut id = 0;
    macro_rules! rpc {
        ($request:expr) => {{
            id += 1;
            let mut frame = serde_json::to_vec(&RequestEnvelope {
                id,
                protocol: PROTOCOL_VERSION,
                request: $request,
            })
            .unwrap();
            frame.push(b'\n');
            write.write_all(&frame).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let line = lines.next_line().await.unwrap().expect("missing response");
                    if let ServerMessage::Response {
                        id: got,
                        ok,
                        result,
                        error,
                    } = serde_json::from_str::<ServerMessage>(&line).unwrap()
                    {
                        assert_eq!(got, id);
                        break (ok, result, error);
                    }
                }
            })
            .await
            .unwrap()
        }};
    }
    let (ok, value, _) = rpc!(Request::ListPendingAttachments {
        session_id: f.sid.clone()
    });
    assert!(ok);
    let expected = value.unwrap()["attachments"].as_array().unwrap().clone();
    f.add(&record("peer-added"));
    let (ok, _, error) = rpc!(f.request("stale", expected));
    assert!(!ok);
    assert!(error.unwrap().contains("pending attachments changed"));
    let current = f.read();
    f.untouched(&current);
    let accepted = rpc!(f.request("new-action", current));
    assert!(accepted.0, "{:?}", accepted.2);
    let replay = rpc!(f.request("new-action", vec![]));
    assert_eq!(accepted, replay);
    assert_eq!(
        f.daemon
            .with_reader(|st| st.list_messages(&f.sid))
            .unwrap()
            .len(),
        1
    );
    drop(lines);
    drop(write);
    tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    f.finish().await;
}
