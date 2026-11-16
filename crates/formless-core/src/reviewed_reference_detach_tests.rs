use super::*;
struct Fixture {
    daemon: Arc<Daemon>,
    sid: String,
    root: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-reference-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.auto_test_models = false;
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let sid = daemon
            .with_store(|st| Ok(st.create_session(None)?.id))
            .unwrap();
        Self { daemon, sid, root }
    }
    fn read(&self) -> Vec<Value> {
        self.daemon
            .with_reader(|st| st.pending_attachment_inventory(&self.sid))
            .unwrap()
    }
    fn add(&self) {
        self.daemon.with_store(|st|st.add_pending_attachment(&self.sid,&json!({"reference":true,"name":"folder","path":"/not/opened","dir":true,"bytes":0,"future":true}))).unwrap();
    }
    fn request(&self, op: &str, expected: Vec<Value>, index: u32) -> Request {
        Request::DetachReviewedReference {
            session_id: self.sid.clone(),
            expected_attachments: expected,
            index,
            operation_id: op.into(),
        }
    }
    fn no_work(&self) {
        assert!(self.daemon.runs.locked().is_empty());
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
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviewed_reference_same_key_is_claimed_once_even_for_identical_duplicate_rows() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add();
    f.add();
    let expected = f.read();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let d = f.daemon.clone();
        let b = barrier.clone();
        let request = f.request("same", expected.clone(), 1);
        tasks.push(tokio::spawn(async move {
            b.wait().await;
            d.handle(request).await.unwrap()
        }));
    }
    barrier.wait().await;
    let first = tasks.remove(0).await.unwrap();
    let second = tasks.remove(0).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first,
        json!({"removed":true,"session_id":f.sid,"index":1,"operation_id":"same"})
    );
    assert_eq!(f.read().len(), 1);
    f.no_work();
    f.finish().await;
}
#[tokio::test]
async fn reviewed_reference_failure_is_cached_and_cannot_be_recertified_with_changed_arguments() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add();
    let expected = f.read();
    f.add();
    let error = f
        .daemon
        .handle(f.request("failed", expected, 0))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("pending attachments changed"));
    let current = f.read();
    let again = f
        .daemon
        .handle(f.request("failed", current.clone(), 1))
        .await
        .unwrap_err();
    assert_eq!(again.to_string(), error);
    assert_eq!(f.read(), current);
    f.daemon.handle(f.request("new", current, 1)).await.unwrap();
    assert_eq!(f.read().len(), 1);
    f.no_work();
    f.finish().await;
}
#[tokio::test]
async fn reviewed_reference_capacity_fails_closed_and_closed_connections_release_receipts() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add();
    let expected = f.read();
    for n in 0..reviewed_reference_detach::MAX_RECEIPTS {
        let error = CLIENT
            .scope(
                (41, "private".into()),
                f.daemon
                    .handle(f.request(&format!("invalid-{n}"), expected.clone(), 9)),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("out of range"));
    }
    let error = CLIENT
        .scope(
            (42, "other".into()),
            f.daemon.handle(f.request("new", expected.clone(), 0)),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("capacity"));
    assert_eq!(f.read(), expected);
    f.daemon.reference_detaches.retire_connection(41);
    CLIENT
        .scope(
            (42, "other".into()),
            f.daemon.handle(f.request("new", expected, 0)),
        )
        .await
        .unwrap();
    assert!(f.read().is_empty());
    f.no_work();
    f.finish().await;
}
#[tokio::test]
async fn reviewed_reference_invalid_identity_or_expected_metadata_never_removes_a_row() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add();
    let before = f.read();
    for n in 0..5 {
        let mut expected = before.clone();
        let op = match n {
            0 => "",
            1 => "invalid\n",
            _ => "bounded",
        };
        match n {
            2 => expected = vec![Value::Null],
            3 => expected = vec![before[0].clone(); PENDING_ATTACHMENT_MAX_ENTRIES + 1],
            4 => expected[0]["huge"] = json!("x".repeat(PENDING_ATTACHMENT_METADATA_BYTES + 1)),
            _ => {}
        }
        assert!(f.daemon.handle(f.request(op, expected, 0)).await.is_err());
        assert_eq!(f.read(), before);
    }
    f.no_work();
    f.finish().await;
}

#[tokio::test]
async fn reviewed_reference_lost_reply_can_commit_once_and_never_implies_safe_replay() {
    use tokio::io::AsyncWriteExt;
    let _env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new();
    f.add();
    f.add();
    let expected = f.read();
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let connection = tokio::spawn(f.daemon.clone().handle_conn(server));
    let mut frame = serde_json::to_vec(&RequestEnvelope {
        id: 1,
        protocol: PROTOCOL_VERSION,
        request: f.request("lost-reply", expected.clone(), 1),
    })
    .unwrap();
    frame.push(b'\n');
    client.write_all(&frame).await.unwrap();
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(5), connection)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        f.read().len(),
        1,
        "a fully written request may commit even though its client never reads a reply"
    );
    assert!(f
        .daemon
        .handle(f.request("explicit-stale-new-operation", expected, 1))
        .await
        .is_err());
    assert_eq!(f.read().len(), 1);
    f.no_work();
    f.finish().await;
}
