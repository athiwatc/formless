//! Actual RPCs, an owned private filesystem and a zero-connection HTTP listener.
use super::*;
struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Setup {
    daemon: Arc<Daemon>,
    _root: Fixture,
    session: String,
    run: String,
    path: std::path::PathBuf,
    old: String,
    new: String,
    listener: std::net::TcpListener,
}
fn setup(env: &crate::credential_env::EnvGuard) -> Setup {
    env.set("FORMLESS_API_KEY", "private-hunk-fixture-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    let root =
        Fixture(std::env::temp_dir().join(format!("fl-review-rpc-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&root.0).unwrap();
    let paths = Paths::from_dirs(root.0.join("data"), root.0.join("config")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut cfg = Config::default();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    cfg.provider.base_url = endpoint.clone();
    cfg.provider.model = "private-review-model".into();
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    assert!(
        Config::load(&paths).unwrap().provider.base_url == endpoint,
        "effective fixture endpoint must equal the owned loopback listener"
    );
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let path = root.0.canonicalize().unwrap().join("owned.txt");
    let old = "first old\r\n2\n3\r\n4\n5\n6\n7\n8\n9\nlast old".to_string();
    let new = old
        .replace("first old", "first NEW")
        .replace("last old", "last NEW");
    std::fs::write(&path, &new).unwrap();
    let (session, run) = daemon
        .with_store(|st| {
            let s = st.create_session(None)?;
            let r = st.create_run(&s.id)?;
            st.snapshot_run_file(
                &s.id,
                &r,
                path.to_str().unwrap(),
                true,
                Some(old.as_bytes()),
            )?;
            st.set_run_status(&r, "completed")?;
            Ok((s.id, r))
        })
        .unwrap();
    Setup {
        daemon,
        _root: root,
        session,
        run,
        path,
        old,
        new,
        listener,
    }
}
async fn inspect(s: &Setup) -> HunkReview {
    serde_json::from_value(
        s.daemon
            .handle(Request::InspectRunReview {
                session_id: s.session.clone(),
                run_id: Some(s.run.clone()),
            })
            .await
            .unwrap(),
    )
    .unwrap()
}
fn decide(view: &HunkReview, index: usize, decision: &str) -> Request {
    let f = &view.files[0];
    Request::DecideReviewHunk {
        review_id: view.review_id.clone(),
        file_id: f.id.clone(),
        hunk_id: f
            .hunks
            .get(index)
            .map(|h| h.id.clone())
            .unwrap_or_else(|| "readonly-fixture-hunk".into()),
        expected_hash: f.after_hash.clone(),
        decision: decision.into(),
    }
}
fn undo(view: &HunkReview, id: &str) -> Request {
    let c = view
        .undo_changes
        .iter()
        .find(|c| c.change_id == id)
        .unwrap();
    let f = view.files.iter().find(|f| f.id == c.file_id).unwrap();
    Request::UndoReviewChange {
        review_id: view.review_id.clone(),
        change_id: id.into(),
        expected_hash: f.after_hash.clone(),
    }
}
async fn writable_fixture(s: &Setup, view: &HunkReview) -> bool {
    assert!(
        view.files[0].exclusion_reason.is_none(),
        "plain private fixture must be reviewable on a supported metadata path"
    );
    assert_eq!(view.files[0].hunks.len(), 2);
    let _ = s;
    true
}
fn no_network(s: &Setup) {
    assert!(
        matches!(s.listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock),
        "review never contacts a model"
    );
}

#[tokio::test]
async fn run_hunk_reject_undo_accept_and_server_comment_are_exact_without_provider_or_run() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let view = inspect(&s).await;
    if !writable_fixture(&s, &view).await {
        return;
    }
    assert_eq!(view.files[0].hunks.len(), 2);
    assert!(
        !s.daemon.paths.data_dir.join("review-receipts").exists(),
        "inspection is journal-read-only"
    );
    let accepted: ReviewDecisionResult =
        serde_json::from_value(s.daemon.handle(decide(&view, 1, "accept")).await.unwrap()).unwrap();
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    assert_eq!(accepted.review.files[0].hunks[1].decision, "accepted");
    let rejected: ReviewDecisionResult = serde_json::from_value(
        s.daemon
            .handle(decide(&accepted.review, 0, "reject"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&s.path).unwrap(),
        s.new.replace("first NEW", "first old").as_bytes()
    );
    assert!(s
        .daemon
        .handle(decide(&rejected.review, 0, "accept"))
        .await
        .is_err());
    let change = rejected.change_id.clone().unwrap();
    let restored: ReviewDecisionResult = serde_json::from_value(
        s.daemon
            .handle(undo(&rejected.review, &change))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    assert_eq!(restored.review.files[0].hunks[0].decision, "pending");
    let file = &restored.review.files[0];
    let comment: ReviewCommentResult = serde_json::from_value(
        s.daemon
            .handle(Request::CommentReviewHunk {
                review_id: restored.review.review_id.clone(),
                file_id: file.id.clone(),
                hunk_id: file.hunks[1].id.clone(),
                expected_hash: file.after_hash.clone(),
                comment: "Please keep the measured behavior".into(),
            })
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(!comment.steered);
    assert_eq!(comment.message.role, "user");
    assert!(comment
        .message
        .content
        .contains("Please keep the measured behavior"));
    let meta: Value = serde_json::from_str(comment.message.meta.as_deref().unwrap()).unwrap();
    assert_eq!(meta["review_context"]["path"], s.path.to_str().unwrap());
    assert_eq!(meta["review_context"]["line"], 10);
    assert!(s.daemon.runs.locked().is_empty());
    assert_eq!(
        s.daemon
            .with_reader(|st| st.last_run(&s.session))
            .unwrap()
            .unwrap()
            .0,
        s.run
    );
    let snapshots = s.daemon.with_reader(|st| st.run_files(&s.run)).unwrap();
    assert_eq!(
        snapshots[0].3.as_deref(),
        Some(s.old.as_bytes()),
        "original whole-Run rewind snapshot remains unchanged"
    );
    no_network(&s);
}
#[cfg(unix)]
#[tokio::test]
async fn dismissed_run_action_reopens_with_durable_undo_and_refreshed_ranges_after_undo() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let view = inspect(&s).await;
    if !writable_fixture(&s, &view).await {
        return;
    }
    let rejected: ReviewDecisionResult =
        serde_json::from_value(s.daemon.handle(decide(&view, 0, "reject")).await.unwrap()).unwrap();
    let change = rejected.change_id.unwrap();
    s.daemon.hunk_reviews.locked().entries.clear();
    let reopened = inspect(&s).await;
    assert_eq!(reopened.undo_changes.last().unwrap().change_id, change);
    assert!(!reopened.undo_changes.last().unwrap().undone);
    let undone: ReviewDecisionResult =
        serde_json::from_value(s.daemon.handle(undo(&reopened, &change)).await.unwrap()).unwrap();
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    assert_eq!(undone.review.files[0].hunks.len(), 2);
    let other: ReviewDecisionResult = serde_json::from_value(
        s.daemon
            .handle(decide(&undone.review, 1, "reject"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&s.path).unwrap(),
        s.new.replace("last NEW", "last old").as_bytes(),
        "rejecting later hunk must preserve restored earlier data"
    );
    assert!(other.change_id.is_some());
    no_network(&s);
}
#[tokio::test]
async fn completed_run_with_live_background_invocation_refuses_inspection_and_decision() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let view = inspect(&s).await;
    let (tool, version) = s
        .daemon
        .with_store(|st| {
            st.create_tool_version(
                "private_pending_fixture",
                "Ordinary fixture",
                "{}",
                "{}",
                "function run(){return 1}",
                &Capabilities::default(),
                64,
                "user",
            )
        })
        .unwrap();
    let invocation = s
        .daemon
        .with_store(|st| {
            st.start_invocation(
                &tool.id,
                version,
                Some(&s.session),
                Some(&s.run),
                None,
                "{}",
            )
        })
        .unwrap();
    assert!(s.daemon.runs.locked().is_empty());
    assert_eq!(
        s.daemon
            .with_reader(|st| st.run_status(&s.run))
            .unwrap()
            .as_deref(),
        Some("completed")
    );
    for request in [
        Request::InspectRunReview {
            session_id: s.session.clone(),
            run_id: Some(s.run.clone()),
        },
        decide(&view, 0, "reject"),
    ] {
        let error = s.daemon.handle(request).await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<RequestFailure>().unwrap().code,
            codes::BUSY
        );
    }
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    assert!(!s.daemon.paths.data_dir.join("review-receipts").exists());
    s.daemon
        .with_store(|st| {
            st.finish_invocation(&invocation, &tool.id, "success", "", "{}", "", 1, 100)
        })
        .unwrap();
    assert_eq!(inspect(&s).await.run_id.as_deref(), Some(s.run.as_str()));
    no_network(&s);
}
fn review_receipt_bytes(s: &Setup) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    let directory = s.daemon.paths.data_dir.join("review-receipts");
    if !directory.exists() {
        return Default::default();
    }
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into(),
                std::fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}
async fn assert_other_work_blocks_file_decision(s: &Setup, view: &HunkReview) {
    let bytes = std::fs::read(&s.path).unwrap();
    let receipts = review_receipt_bytes(s);
    for decision in ["accept", "reject"] {
        let error = s
            .daemon
            .handle(decide(view, 0, decision))
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<RequestFailure>().unwrap().code,
            codes::BUSY
        );
        assert_eq!(std::fs::read(&s.path).unwrap(), bytes);
        assert_eq!(review_receipt_bytes(s), receipts);
    }
    // Another Session's work does not prohibit this read-only inspection.
    assert_eq!(inspect(s).await.run_id, view.run_id);
}

#[tokio::test]
async fn other_session_run_and_invocation_block_file_decisions_without_receipts() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let view = inspect(&s).await;
    let (other, run) = s
        .daemon
        .with_store(|st| {
            let other = st.create_session(None)?.id;
            let run = st.create_run(&other)?;
            Ok((other, run))
        })
        .unwrap();
    assert!(
        s.daemon.runs.locked().is_empty(),
        "persisted admission alone must block"
    );
    assert_other_work_blocks_file_decision(&s, &view).await;
    s.daemon
        .with_store(|st| st.set_run_status(&run, "completed"))
        .unwrap();
    let (tool, version) = s
        .daemon
        .with_store(|st| {
            st.create_tool_version(
                "other_session_fixture",
                "Ordinary fixture",
                "{}",
                "{}",
                "function run(){return 1}",
                &Capabilities::default(),
                64,
                "user",
            )
        })
        .unwrap();
    let invocation = s
        .daemon
        .with_store(|st| {
            st.start_invocation(&tool.id, version, Some(&other), Some(&run), None, "{}")
        })
        .unwrap();
    assert_other_work_blocks_file_decision(&s, &view).await;
    s.daemon
        .with_store(|st| {
            st.finish_invocation(&invocation, &tool.id, "success", "", "{}", "", 1, 100)
        })
        .unwrap();
    assert!(review_receipt_bytes(&s).is_empty());
    no_network(&s);
}

#[tokio::test]
async fn other_session_work_blocks_undo_without_changing_file_or_receipt() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let view = inspect(&s).await;
    if !writable_fixture(&s, &view).await {
        return;
    }
    let rejected: ReviewDecisionResult =
        serde_json::from_value(s.daemon.handle(decide(&view, 0, "reject")).await.unwrap()).unwrap();
    let change = rejected.change_id.as_deref().unwrap();
    let bytes = std::fs::read(&s.path).unwrap();
    let receipts = review_receipt_bytes(&s);
    let (other, run) = s
        .daemon
        .with_store(|st| {
            let other = st.create_session(None)?.id;
            let run = st.create_run(&other)?;
            Ok((other, run))
        })
        .unwrap();
    let error = s
        .daemon
        .handle(undo(&rejected.review, change))
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::BUSY
    );
    assert_eq!(std::fs::read(&s.path).unwrap(), bytes);
    assert_eq!(review_receipt_bytes(&s), receipts);
    s.daemon
        .with_store(|st| st.set_run_status(&run, "completed"))
        .unwrap();
    let (tool, version) = s
        .daemon
        .with_store(|st| {
            st.create_tool_version(
                "other_undo_fixture",
                "Ordinary fixture",
                "{}",
                "{}",
                "function run(){return 1}",
                &Capabilities::default(),
                64,
                "user",
            )
        })
        .unwrap();
    let invocation = s
        .daemon
        .with_store(|st| {
            st.start_invocation(&tool.id, version, Some(&other), Some(&run), None, "{}")
        })
        .unwrap();
    let error = s
        .daemon
        .handle(undo(&rejected.review, change))
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::BUSY
    );
    assert_eq!(std::fs::read(&s.path).unwrap(), bytes);
    assert_eq!(review_receipt_bytes(&s), receipts);
    s.daemon
        .with_store(|st| {
            st.finish_invocation(&invocation, &tool.id, "success", "", "{}", "", 1, 100)
        })
        .unwrap();
    s.daemon
        .handle(undo(&rejected.review, change))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    no_network(&s);
}

#[tokio::test]
async fn other_session_completed_awaiting_delivery_allows_file_changes_without_consuming_result() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let view = inspect(&s).await;
    if !writable_fixture(&s, &view).await {
        return;
    }
    let (other, run) = s
        .daemon
        .with_store(|st| {
            let other = st.create_session(None)?.id;
            let run = st.create_run(&other)?;
            st.set_run_status(&run, "completed")?;
            Ok((other, run))
        })
        .unwrap();
    let handle = uuid::Uuid::new_v4().to_string();
    let pending = Arc::new(PendingEntry {
        _pipeline: StdMutex::new(None),
        routing_task: None,
        taint_scope: s.daemon.taint_scope(&other, &run),
        tool_name: "other-background-fixture".into(),
        run_id: run,
        started_ms: epoch_ms(),
        session_id: other.clone(),
        result: StdMutex::new(None),
        delivered: AtomicBool::new(false),
        abandoned: AtomicBool::new(false),
        stalled_noted: AtomicBool::new(false),
        activity: Arc::new(AtomicU64::new(epoch_ms())),
        notify: tokio::sync::Notify::new(),
        cancel: CancellationToken::new(),
        order: 1,
    });
    s.daemon
        .pending
        .locked()
        .insert(handle.clone(), pending.clone());
    s.daemon
        .with_store(|st| st.add_pending_handle(&handle, &other, "other-background-fixture"))
        .unwrap();
    assert_other_work_blocks_file_decision(&s, &view).await;
    *pending.result.locked() = Some((true, "private completed result".into()));
    let rejected: ReviewDecisionResult =
        serde_json::from_value(s.daemon.handle(decide(&view, 0, "reject")).await.unwrap()).unwrap();
    s.daemon
        .handle(undo(
            &rejected.review,
            rejected.change_id.as_deref().unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    assert!(s.daemon.pending.locked().contains_key(&handle));
    assert_eq!(
        pending.result.locked().as_ref().unwrap().1,
        "private completed result"
    );
    assert!(!pending.delivered.load(Ordering::SeqCst));
    assert!(s
        .daemon
        .with_reader(|st| st.session_pending_handle(&other, &handle))
        .unwrap()
        .is_some());
    no_network(&s);
}

#[tokio::test]
async fn tool_review_is_a_version_and_hash_pinned_draft_without_registry_save_or_smoke_execution() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    // File mutation exclusion must not block this unrelated in-memory draft.
    let other_run = s
        .daemon
        .with_store(|st| {
            let other = st.create_session(None)?;
            st.create_run(&other.id)
        })
        .unwrap();
    let code = "function run(){return 1;}\r\n";
    let candidate = "function run(){return 2;}\r\n";
    let (tool, version) = s
        .daemon
        .with_store(|st| {
            st.create_tool_version(
                "private_edit_fixture",
                "Ordinary fixture",
                "{}",
                "{}",
                code,
                &Capabilities::default(),
                64,
                "user",
            )
        })
        .unwrap();
    let view: HunkReview = serde_json::from_value(
        s.daemon
            .handle(Request::PrepareToolReview {
                session_id: s.session.clone(),
                tool_id: tool.id.clone(),
                base_version: version,
                candidate_code: candidate.into(),
            })
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        view.original_candidate_hash.as_deref(),
        Some(image_hash(Some(candidate.as_bytes())).as_str())
    );
    let rejected: ReviewDecisionResult =
        serde_json::from_value(s.daemon.handle(decide(&view, 0, "reject")).await.unwrap()).unwrap();
    assert_eq!(rejected.review.draft_code.as_deref(), Some(code));
    assert_eq!(
        rejected.review.candidate_hash.as_deref(),
        Some(rejected.review.files[0].after_hash.as_str())
    );
    let restored: ReviewDecisionResult = serde_json::from_value(
        s.daemon
            .handle(undo(
                &rejected.review,
                rejected.change_id.as_deref().unwrap(),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(restored.review.draft_code.as_deref(), Some(candidate));
    let (_, current) = s
        .daemon
        .with_reader(|st| st.get_tool_current(&tool.id))
        .unwrap()
        .unwrap();
    assert_eq!(current.version, version);
    assert_eq!(current.code, code);
    s.daemon
        .with_store(|st| {
            st.create_tool_version(
                "private_edit_fixture",
                "Ordinary fixture",
                "{}",
                "{}",
                "function run(){return 3;}",
                &Capabilities::default(),
                64,
                "user",
            )
        })
        .unwrap();
    assert!(s
        .daemon
        .handle(decide(&restored.review, 0, "reject"))
        .await
        .is_err());
    assert!(!s.daemon.paths.data_dir.join("review-receipts").exists());
    s.daemon
        .with_store(|st| st.set_run_status(&other_run, "completed"))
        .unwrap();
    no_network(&s);
}

#[tokio::test]
async fn unresolved_pending_memory_blocks_but_completed_awaiting_delivery_is_preserved() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let initial = inspect(&s).await;
    let handle = uuid::Uuid::new_v4().to_string();
    let pending = Arc::new(PendingEntry {
        _pipeline: StdMutex::new(None),
        routing_task: None,
        taint_scope: s.daemon.taint_scope(&s.session, &s.run),
        tool_name: "private-background-fixture".into(),
        run_id: s.run.clone(),
        started_ms: epoch_ms(),
        session_id: s.session.clone(),
        result: StdMutex::new(None),
        delivered: AtomicBool::new(false),
        abandoned: AtomicBool::new(false),
        stalled_noted: AtomicBool::new(false),
        activity: Arc::new(AtomicU64::new(epoch_ms())),
        notify: tokio::sync::Notify::new(),
        cancel: CancellationToken::new(),
        order: 1,
    });
    s.daemon
        .pending
        .locked()
        .insert(handle.clone(), pending.clone());
    s.daemon
        .with_store(|st| st.add_pending_handle(&handle, &s.session, "private-background-fixture"))
        .unwrap();
    let error = s
        .daemon
        .handle(decide(&initial, 0, "reject"))
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::BUSY
    );
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    assert!(!s.daemon.paths.data_dir.join("review-receipts").exists());
    *pending.result.locked() = Some((true, "completed private result".into()));
    let review = inspect(&s).await;
    assert_eq!(review.run_id.as_deref(), Some(s.run.as_str()));
    assert!(s.daemon.pending.locked().contains_key(&handle));
    assert!(
        s.daemon
            .with_reader(|st| st.session_pending_handle(&s.session, &handle))
            .unwrap()
            .is_some(),
        "review never consumes await/push results"
    );
    assert!(!pending.delivered.load(Ordering::SeqCst));
    no_network(&s);
}

#[tokio::test]
async fn valid_base64_preimage_corruption_cannot_reach_actual_rpc_undo() {
    use base64::Engine;
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let initial = inspect(&s).await;
    if !writable_fixture(&s, &initial).await {
        return;
    }
    let rejected: ReviewDecisionResult = serde_json::from_value(
        s.daemon
            .handle(decide(&initial, 0, "reject"))
            .await
            .unwrap(),
    )
    .unwrap();
    let change = rejected.change_id.as_deref().unwrap();
    let current = std::fs::read(&s.path).unwrap();
    let receipt = s
        .daemon
        .paths
        .data_dir
        .join("review-receipts")
        .join(format!("{change}.json"));
    let mut data: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    data["before"]["base64"] =
        json!(base64::engine::general_purpose::STANDARD.encode(b"corrupted valid base64 source"));
    std::fs::write(&receipt, serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(s
        .daemon
        .handle(undo(&rejected.review, change))
        .await
        .is_err());
    assert_eq!(std::fs::read(&s.path).unwrap(), current);
    assert!(s.daemon.runs.locked().is_empty());
    no_network(&s);
}

#[cfg(unix)]
#[tokio::test]
async fn new_run_deleted_file_reject_and_undo_preserve_captured_bytes_owner_and_mode() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    std::fs::write(&s.path, b"original\r\nlast line").unwrap();
    std::fs::set_permissions(&s.path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let original = std::fs::metadata(&s.path).unwrap();
    // Exercise the production synchronous pre-mutation hook, not a manually
    // manufactured metadata row. Later hooks must preserve this FIRST image.
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    std::fs::set_permissions(&s.path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&s.path, b"later version").unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    std::fs::remove_file(&s.path).unwrap();
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    let view = inspect(&s).await;
    assert!(
        view.files[0].exclusion_reason.is_none(),
        "captured ordinary deleted file must be editable"
    );
    let result: ReviewDecisionResult =
        serde_json::from_value(s.daemon.handle(decide(&view, 0, "reject")).await.unwrap()).unwrap();
    assert_eq!(std::fs::read(&s.path).unwrap(), b"original\r\nlast line");
    let restored = std::fs::metadata(&s.path).unwrap();
    assert_eq!(
        (restored.mode(), restored.uid(), restored.gid()),
        (original.mode(), original.uid(), original.gid())
    );
    let id = result.change_id.as_deref().unwrap();
    s.daemon.handle(undo(&result.review, id)).await.unwrap();
    assert!(
        !s.path.exists(),
        "Undo must return to the exact absent post-Run image"
    );
    no_network(&s);
}

#[cfg(unix)]
#[tokio::test]
async fn new_run_created_file_reject_and_undo_and_legacy_deleted_metadata_refusal() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    std::fs::remove_file(&s.path).unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    std::fs::write(&s.path, b"created\r\n").unwrap();
    let policy = s.daemon.review_policy().unwrap();
    let before = ReviewPath::open(&s.path, &policy)
        .unwrap()
        .read(&policy)
        .unwrap();
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    let view = inspect(&s).await;
    let result: ReviewDecisionResult =
        serde_json::from_value(s.daemon.handle(decide(&view, 0, "reject")).await.unwrap()).unwrap();
    assert!(!s.path.exists());
    s.daemon
        .handle(undo(&result.review, result.change_id.as_deref().unwrap()))
        .await
        .unwrap();
    assert_eq!(
        ReviewPath::open(&s.path, &policy)
            .unwrap()
            .read(&policy)
            .unwrap(),
        before
    );
    s.run = s
        .daemon
        .with_store(|st| {
            let run = st.create_run(&s.session)?;
            st.snapshot_run_file(
                &s.session,
                &run,
                s.path.to_str().unwrap(),
                true,
                Some(b"legacy"),
            )?;
            st.set_run_status(&run, "completed")?;
            Ok(run)
        })
        .unwrap();
    std::fs::remove_file(&s.path).unwrap();
    let legacy = inspect(&s).await;
    assert!(legacy.files[0]
        .exclusion_reason
        .as_deref()
        .unwrap()
        .contains("no recorded original metadata"));
    assert!(s.daemon.handle(decide(&legacy, 0, "reject")).await.is_err());
    assert!(!s.path.exists());
    no_network(&s);
}

#[tokio::test]
async fn snapshot_capture_never_copies_private_profile_bytes_or_invents_absence_for_unsupported_files(
) {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let private = s.daemon.paths.data_dir.join("private-before-fixture");
    std::fs::write(
        &private,
        b"private snapshot fixture; never a live credential",
    )
    .unwrap();
    s.daemon
        .snapshot_before_change(&s.session, &s.run, &private);
    // A directory exists but has no supported regular-file image. It must not
    // become a recorded creation that an inverse might later delete.
    let directory = s._root.0.join("existing-directory");
    std::fs::create_dir(&directory).unwrap();
    s.daemon
        .snapshot_before_change(&s.session, &s.run, &directory);
    let rows = s.daemon.with_reader(|st| st.run_files(&s.run)).unwrap();
    for path in [&private, &directory] {
        let row = rows
            .iter()
            .find(|row| row.0 == path.to_str().unwrap())
            .unwrap();
        assert!(row.1, "unsupported state must never be marked known absent");
        assert!(!row.2);
        assert!(row.3.is_none());
    }
    assert!(private.exists());
    assert!(directory.is_dir());
    no_network(&s);
}

#[cfg(unix)]
#[tokio::test]
async fn corrupt_recorded_existence_cannot_turn_original_file_into_a_rejectable_creation() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    // The bytes and metadata still match their valid checksum. Corrupt only
    // the separate old table's existence flag; it cannot confer delete authority.
    let db = rusqlite::Connection::open(s.daemon.paths.db_file()).unwrap();
    db.execute("UPDATE run_files SET existed=0 WHERE run_id=?1", [&s.run])
        .unwrap();
    drop(db);
    let current = std::fs::read(&s.path).unwrap();
    let view = inspect(&s).await;
    assert!(view.files[0]
        .exclusion_reason
        .as_deref()
        .unwrap()
        .contains("Recorded existence"));
    assert!(s.daemon.handle(decide(&view, 0, "reject")).await.is_err());
    assert_eq!(std::fs::read(&s.path).unwrap(), current);
    assert!(!s.daemon.paths.data_dir.join("review-receipts").exists());
    no_network(&s);
}

#[cfg(unix)]
#[tokio::test]
async fn byte_only_legacy_snapshot_cannot_be_reinterpreted_as_creation_by_corrupt_existence() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env); // Legacy snapshot API deliberately records NULL metadata.
    let db = rusqlite::Connection::open(s.daemon.paths.db_file()).unwrap();
    db.execute("UPDATE run_files SET existed=0 WHERE run_id=?1", [&s.run])
        .unwrap();
    drop(db);
    let current = std::fs::read(&s.path).unwrap();
    let view = inspect(&s).await;
    assert!(view.files[0]
        .exclusion_reason
        .as_deref()
        .unwrap()
        .contains("Recorded existence"));
    assert!(s.daemon.handle(decide(&view, 0, "reject")).await.is_err());
    assert_eq!(std::fs::read(&s.path).unwrap(), current);
    assert!(!s.daemon.paths.data_dir.join("review-receipts").exists());
    no_network(&s);
}

#[cfg(unix)]
#[tokio::test]
async fn production_hook_keeps_authorized_hardlink_and_parent_alias_bytes_without_hunk_authority() {
    use std::os::unix::fs::symlink;
    let env = crate::credential_env::EnvGuard::isolated();
    for alias_parent in [false, true] {
        let mut s = setup(&env);
        s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
        if alias_parent {
            let actual = s._root.0.join("ordinary-target");
            std::fs::create_dir(&actual).unwrap();
            std::fs::write(actual.join("owned.txt"), b"alias original\r\nEOF").unwrap();
            let alias = s._root.0.join("parent-alias");
            symlink(&actual, &alias).unwrap();
            s.path = alias.join("owned.txt");
        } else {
            std::fs::hard_link(&s.path, s._root.0.join("hardlink-peer")).unwrap();
        }
        // Precisely the old callback's regular-file/readable criteria, owned
        // private fixture only. The production callback uses anchored I/O.
        assert!(std::fs::symlink_metadata(&s.path).unwrap().is_file());
        let original = std::fs::read(&s.path).unwrap();
        s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
        let rows = s.daemon.with_reader(|st| st.run_files(&s.run)).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].1 && rows[0].2);
        assert_eq!(rows[0].3.as_deref(), Some(original.as_slice()));
        std::fs::write(&s.path, b"tool changed this file").unwrap();
        s.daemon
            .with_store(|st| st.set_run_status(&s.run, "completed"))
            .unwrap();
        let view = inspect(&s).await;
        assert!(
            view.files[0].exclusion_reason.is_some(),
            "byte recovery is not exact metadata/inode mutation authority"
        );
        assert!(s.daemon.handle(decide(&view, 0, "reject")).await.is_err());
        assert_eq!(std::fs::read(&s.path).unwrap(), b"tool changed this file");
        no_network(&s);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn production_hook_refuses_retargeted_parent_alias_to_private_profile_without_copying_body() {
    use std::os::unix::fs::symlink;
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let ordinary = s._root.0.join("ordinary-alias-target");
    std::fs::create_dir(&ordinary).unwrap();
    std::fs::write(ordinary.join("owned.txt"), b"ordinary route").unwrap();
    let alias = s._root.0.join("changing-parent");
    symlink(&ordinary, &alias).unwrap();
    let target = alias.join("owned.txt");
    let former_target = target.canonicalize().unwrap();
    let private = s.daemon.paths.data_dir.join("owned.txt");
    std::fs::write(&private, b"private fixture bytes; not a credential").unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&s.daemon.paths.data_dir, &alias).unwrap();
    assert_ne!(target.canonicalize().unwrap(), former_target);
    s.daemon.snapshot_before_change(&s.session, &s.run, &target);
    let rows = s.daemon.with_reader(|st| st.run_files(&s.run)).unwrap();
    let row = rows
        .iter()
        .find(|row| row.0 == target.to_str().unwrap())
        .unwrap();
    assert!(row.1 && !row.2);
    assert!(
        row.3.is_none(),
        "protected target must never be copied by snapshot capture"
    );
    assert_eq!(
        std::fs::read(&private).unwrap(),
        b"private fixture bytes; not a credential"
    );
    no_network(&s);
}

#[cfg(windows)]
#[tokio::test]
async fn native_windows_hook_keeps_hardlink_bytes_with_readonly_hunk_metadata() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    std::fs::hard_link(&s.path, s._root.0.join("hardlink-peer")).unwrap();
    let original = std::fs::read(&s.path).unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    let rows = s.daemon.with_reader(|st| st.run_files(&s.run)).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].1 && rows[0].2);
    assert_eq!(rows[0].3.as_deref(), Some(original.as_slice()));
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    let view = inspect(&s).await;
    assert!(view.files[0].exclusion_reason.is_some());
    assert!(s.daemon.handle(decide(&view, 0, "reject")).await.is_err());
    assert_eq!(std::fs::read(&s.path).unwrap(), original);
    no_network(&s);
}

#[cfg(windows)]
#[tokio::test]
async fn ordinary_windows_active_review_undo_is_real_but_reopened_receipts_cannot_mint_witnesses() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let initial = inspect(&s).await;
    assert!(writable_fixture(&s, &initial).await);
    let rejected: ReviewDecisionResult = serde_json::from_value(
        s.daemon
            .handle(decide(&initial, 0, "reject"))
            .await
            .unwrap(),
    )
    .unwrap();
    let change = rejected.change_id.clone().unwrap();
    let rejected_bytes = std::fs::read(&s.path).unwrap();
    assert_ne!(rejected_bytes, s.new.as_bytes());
    let undone: ReviewDecisionResult = serde_json::from_value(
        s.daemon
            .handle(undo(&rejected.review, &change))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    let rejected: ReviewDecisionResult = serde_json::from_value(
        s.daemon
            .handle(decide(&undone.review, 0, "reject"))
            .await
            .unwrap(),
    )
    .unwrap();
    let change = rejected.change_id.clone().unwrap();
    s.daemon.hunk_reviews.locked().entries.clear();
    let reopened = inspect(&s).await;
    assert!(reopened.undo_changes.is_empty());
    assert!(reopened
        .warnings
        .iter()
        .any(|w| w.contains("original live file object")));
    let error = s
        .daemon
        .handle(Request::UndoReviewChange {
            review_id: reopened.review_id.clone(),
            change_id: change.clone(),
            expected_hash: reopened.files[0].after_hash.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<RequestFailure>().unwrap().code,
        codes::INVALID
    );
    assert_eq!(std::fs::read(&s.path).unwrap(), rejected_bytes);
    assert!(s
        .daemon
        .paths
        .data_dir
        .join("review-receipts")
        .join(format!("{change}.json"))
        .exists());
    no_network(&s);
}

#[cfg(windows)]
fn decide_path(view: &HunkReview, path: &std::path::Path) -> Request {
    let file = view
        .files
        .iter()
        .find(|file| file.path == path.to_str().unwrap())
        .unwrap();
    assert!(
        file.exclusion_reason.is_none(),
        "{:?}",
        file.exclusion_reason
    );
    assert_eq!(file.hunks.len(), 1);
    Request::DecideReviewHunk {
        review_id: view.review_id.clone(),
        file_id: file.id.clone(),
        hunk_id: file.hunks[0].id.clone(),
        expected_hash: file.after_hash.clone(),
        decision: "reject".into(),
    }
}
#[cfg(windows)]
async fn decision(s: &Setup, request: Request) -> ReviewDecisionResult {
    serde_json::from_value(s.daemon.handle(request).await.unwrap()).unwrap()
}
#[cfg(windows)]
#[tokio::test]
async fn windows_created_file_reject_undo_and_redo_reuse_the_exact_retained_object() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    s.path = s.path.parent().unwrap().join("created-日.txt");
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    let bytes = "created\r\n🦀\r\n".as_bytes();
    std::fs::write(&s.path, bytes).unwrap();
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    let policy = s.daemon.review_policy().unwrap();
    let original = s
        .daemon
        .read_review_image(s.path.to_str().unwrap(), &policy)
        .unwrap();
    let initial = inspect(&s).await;
    let rejected = decision(&s, decide_path(&initial, &s.path)).await;
    assert!(!s.path.exists());
    let change = rejected.change_id.clone().unwrap();
    let restored = decision(&s, undo(&rejected.review, &change)).await;
    assert_eq!(std::fs::read(&s.path).unwrap(), bytes);
    assert_eq!(
        s.daemon
            .read_review_image(s.path.to_str().unwrap(), &policy)
            .unwrap(),
        original
    );
    let receipts_before = std::fs::read_dir(s.daemon.paths.data_dir.join("review-objects"))
        .unwrap()
        .count();
    let rejected_again = decision(&s, decide_path(&restored.review, &s.path)).await;
    assert!(!s.path.exists());
    assert_eq!(
        std::fs::read_dir(s.daemon.paths.data_dir.join("review-objects"))
            .unwrap()
            .count(),
        receipts_before,
        "redo reuses its durable original object; no extra retained slot is consumed"
    );
    s.daemon.hunk_reviews.locked().entries.clear();
    let reopened = inspect(&s).await;
    let change = rejected_again.change_id.unwrap();
    let _restored = decision(&s, undo(&reopened, &change)).await;
    assert_eq!(
        s.daemon
            .read_review_image(s.path.to_str().unwrap(), &policy)
            .unwrap(),
        original
    );
    assert!(s.daemon.runs.locked().is_empty());
    no_network(&s);
}
#[cfg(windows)]
#[tokio::test]
async fn windows_tracked_delete_restores_immutable_bytes_on_original_object_then_undoes_without_new_slot(
) {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    let bytes = "original\r\n日本語\n".as_bytes();
    std::fs::write(&s.path, bytes).unwrap();
    let policy = s.daemon.review_policy().unwrap();
    let original = s
        .daemon
        .read_review_image(s.path.to_str().unwrap(), &policy)
        .unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    // The hardlink is not an immutable byte snapshot: the Tool can change it
    // before deleting the public name. Recovery must use private saved bytes.
    std::fs::write(&s.path, b"later Tool body\r\n").unwrap();
    std::fs::remove_file(&s.path).unwrap();
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    let view = inspect(&s).await;
    let restored = decision(&s, decide_path(&view, &s.path)).await;
    assert_eq!(
        s.daemon
            .read_review_image(s.path.to_str().unwrap(), &policy)
            .unwrap(),
        original
    );
    let store_count = std::fs::read_dir(s.daemon.paths.data_dir.join("review-objects"))
        .unwrap()
        .count();
    let change = restored.change_id.clone().unwrap();
    let removed = decision(&s, undo(&restored.review, &change)).await;
    assert!(!s.path.exists());
    assert_eq!(
        std::fs::read_dir(s.daemon.paths.data_dir.join("review-objects"))
            .unwrap()
            .count(),
        store_count,
        "Undo removes only the public link and never needs another retention slot"
    );
    s.daemon.hunk_reviews.locked().entries.clear();
    let reopened = inspect(&s).await;
    let _restored_again = decision(&s, decide_path(&reopened, &s.path)).await;
    assert_eq!(
        s.daemon
            .read_review_image(s.path.to_str().unwrap(), &policy)
            .unwrap(),
        original
    );
    assert_eq!(removed.review.files[0].after_hash, image_hash(None));
    no_network(&s);
}
#[cfg(windows)]
#[tokio::test]
async fn windows_rename_delete_review_obeys_public_alias_and_lifo_undo_guards() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    let a = s.path.clone();
    let b = a.parent().unwrap().join("renamed.txt");
    std::fs::write(&a, b"original A\r\n").unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &a);
    s.daemon.snapshot_before_change(&s.session, &s.run, &b);
    std::fs::rename(&a, &b).unwrap();
    std::fs::write(&b, b"modified B\r\n").unwrap();
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    let initial = inspect(&s).await;
    assert!(s.daemon.handle(decide_path(&initial, &a)).await.is_err());
    assert!(!a.exists());
    assert_eq!(std::fs::read(&b).unwrap(), b"modified B\r\n");
    let rejected_b = decision(&s, decide_path(&initial, &b)).await;
    assert!(!b.exists());
    let rejected_a = decision(&s, decide_path(&rejected_b.review, &a)).await;
    assert_eq!(std::fs::read(&a).unwrap(), b"original A\r\n");
    let change_b = rejected_b.change_id.unwrap();
    assert!(
        s.daemon
            .handle(undo(&rejected_a.review, &change_b))
            .await
            .is_err(),
        "out-of-order Undo cannot rewrite the shared object behind live A"
    );
    assert_eq!(std::fs::read(&a).unwrap(), b"original A\r\n");
    assert!(!b.exists());
    let undone_a = decision(
        &s,
        undo(&rejected_a.review, rejected_a.change_id.as_ref().unwrap()),
    )
    .await;
    assert!(!a.exists());
    let _undone_b = decision(&s, undo(&undone_a.review, &change_b)).await;
    assert!(!a.exists());
    assert_eq!(std::fs::read(&b).unwrap(), b"modified B\r\n");
    assert!(s.daemon.runs.locked().is_empty());
    no_network(&s);
}

#[cfg(windows)]
#[tokio::test]
async fn windows_forged_database_missing_image_without_private_marker_cannot_remove_a_file() {
    let env = crate::credential_env::EnvGuard::isolated();
    let s = setup(&env);
    let current = std::fs::read(&s.path).unwrap();
    let missing = crate::review_file_io::encode_before_metadata(&ReviewImage::missing()).unwrap();
    let db = rusqlite::Connection::open(s.daemon.paths.db_file()).unwrap();
    db.execute(
        "UPDATE run_files SET existed=0,kept=1,before=NULL,before_metadata=?1 WHERE run_id=?2",
        rusqlite::params![missing, s.run],
    )
    .unwrap();
    drop(db);
    let view = inspect(&s).await;
    assert!(view.files[0]
        .exclusion_reason
        .as_deref()
        .unwrap()
        .contains("private pre-Tool missing-state marker"));
    assert!(s.daemon.handle(decide(&view, 0, "reject")).await.is_err());
    assert_eq!(std::fs::read(&s.path).unwrap(), current);
    assert!(!s.daemon.paths.data_dir.join("review-receipts").exists());
    no_network(&s);
}
#[cfg(windows)]
#[tokio::test]
async fn windows_retention_capacity_failure_preserves_whole_run_before_bytes_and_refuses_unretained_delete(
) {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    let retained = crate::review_retained::RetainedStore::open(&s.daemon.paths, true)
        .unwrap()
        .unwrap();
    let quota_path = s.path.parent().unwrap().join("quota-fixture.txt");
    std::fs::write(&quota_path, b"small quota object").unwrap();
    let policy = s.daemon.review_policy().unwrap();
    let quota_image = s
        .daemon
        .read_review_image(quota_path.to_str().unwrap(), &policy)
        .unwrap();
    let access = ReviewPath::open(&quota_path, &policy).unwrap();
    let source = access
        .namespace_source(&quota_image, &policy, &retained)
        .unwrap();
    for i in 0..crate::review_retained::MAX_RECORDS {
        let owner = crate::review_retained::RetainedOwner {
            session_id: "private-capacity".into(),
            run_id: format!("run-{i}"),
            path_digest: format!("{i:064x}"),
        };
        retained.capture(&owner, &source, &quota_image).unwrap();
    }
    let extra = crate::review_retained::RetainedOwner {
        session_id: "private-capacity".into(),
        run_id: "one-too-many".into(),
        path_digest: "f".repeat(64),
    };
    let error = retained.capture(&extra, &source, &quota_image).unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("capacity"),
        "wrong full-store failure: {error:#}"
    );
    drop(source);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    let before = "exact before despite retention failure\r\n日\n".as_bytes();
    std::fs::write(&s.path, before).unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    let recorded = s.daemon.with_reader(|st| st.run_files(&s.run)).unwrap();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].1 && recorded[0].2);
    assert_eq!(recorded[0].3.as_deref(), Some(before));
    std::fs::remove_file(&s.path).unwrap();
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    let view = inspect(&s).await;
    assert!(view.files[0]
        .exclusion_reason
        .as_deref()
        .unwrap()
        .contains("no verified retained original object"));
    assert!(s.daemon.handle(decide(&view, 0, "reject")).await.is_err());
    assert!(!s.path.exists());
    assert_eq!(
        s.daemon.with_reader(|st| st.run_files(&s.run)).unwrap()[0]
            .3
            .as_deref(),
        Some(before)
    );
    no_network(&s);
}
#[cfg(windows)]
#[tokio::test]
async fn windows_completed_run_releases_capture_pin_but_active_review_suspends_expiry() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    let retained = crate::review_retained::RetainedStore::open(&s.daemon.paths, false)
        .unwrap()
        .unwrap();
    assert_eq!(s.daemon.retained_capture_leases.locked().len(), 1);
    assert_eq!(
        retained.expire(u64::MAX).unwrap(),
        0,
        "an active Run pins its original object"
    );
    s.daemon.record_run_status(&s.session, &s.run, "completed");
    assert!(
        s.daemon.retained_capture_leases.locked().is_empty(),
        "the last completed Run releases without another Tool callback"
    );
    assert_eq!(retained.expire(u64::MAX).unwrap(), 1);
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    s.daemon.record_run_status(&s.session, &s.run, "completed");
    let _view = inspect(&s).await;
    assert_eq!(
        retained.expire(u64::MAX).unwrap(),
        0,
        "a live review token independently retains the full advertised Undo lifetime"
    );
    s.daemon.hunk_reviews.locked().entries.clear();
    assert_eq!(retained.expire(u64::MAX).unwrap(), 1);
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    no_network(&s);
}

#[cfg(windows)]
#[tokio::test]
async fn windows_private_before_binding_rejects_rechecksummed_database_tampering() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    std::fs::write(&s.path, b"true immutable before\r\n").unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    let recorded = s
        .daemon
        .with_reader(|st| st.recorded_run_review(&s.session, Some(&s.run)))
        .unwrap();
    let row = &recorded.files[0];
    let mut forged = crate::review_file_io::decode_before_metadata(
        row.before_metadata.as_deref().unwrap(),
        row.before.as_deref(),
    )
    .unwrap();
    forged.bytes = Some(b"forged inverse bytes\r\n".to_vec());
    let metadata = crate::review_file_io::encode_before_metadata(&forged).unwrap();
    std::fs::write(&s.path, b"actual Tool result\r\n").unwrap();
    s.daemon
        .with_store(|st| st.set_run_status(&s.run, "completed"))
        .unwrap();
    let db = rusqlite::Connection::open(s.daemon.paths.db_file()).unwrap();
    db.execute(
        "UPDATE run_files SET before=?1,before_metadata=?2 WHERE run_id=?3",
        rusqlite::params![forged.bytes.as_deref(), metadata, s.run],
    )
    .unwrap();
    drop(db);
    let view = inspect(&s).await;
    assert!(view.files[0]
        .exclusion_reason
        .as_deref()
        .unwrap()
        .contains("Private immutable first-capture binding"));
    assert!(s.daemon.handle(decide(&view, 0, "reject")).await.is_err());
    assert_eq!(std::fs::read(&s.path).unwrap(), b"actual Tool result\r\n");
    assert!(!s.daemon.paths.data_dir.join("review-receipts").exists());
    no_network(&s);
}
#[cfg(windows)]
#[tokio::test]
async fn windows_last_background_completion_releases_completed_run_capture_without_another_callback(
) {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut s = setup(&env);
    s.run = s.daemon.with_store(|st| st.create_run(&s.session)).unwrap();
    s.daemon.snapshot_before_change(&s.session, &s.run, &s.path);
    let handle = uuid::Uuid::new_v4().to_string();
    let pending = Arc::new(PendingEntry {
        _pipeline: StdMutex::new(None),
        routing_task: None,
        taint_scope: s.daemon.taint_scope(&s.session, &s.run),
        tool_name: "private-retention-completion".into(),
        run_id: s.run.clone(),
        started_ms: epoch_ms(),
        session_id: s.session.clone(),
        result: StdMutex::new(None),
        delivered: AtomicBool::new(false),
        abandoned: AtomicBool::new(false),
        stalled_noted: AtomicBool::new(false),
        activity: Arc::new(AtomicU64::new(epoch_ms())),
        notify: tokio::sync::Notify::new(),
        cancel: CancellationToken::new(),
        order: 1,
    });
    s.daemon
        .pending
        .locked()
        .insert(handle.clone(), pending.clone());
    s.daemon.record_run_status(&s.session, &s.run, "completed");
    assert_eq!(
        s.daemon.retained_capture_leases.locked().len(),
        1,
        "unfinished background work still owns its source object"
    );
    s.daemon
        .complete_pending(&handle, &pending, true, "done".into());
    assert!(s.daemon.retained_capture_leases.locked().is_empty());
    assert!(
        s.daemon.pending.locked().contains_key(&handle),
        "cleanup never consumes an awaiting-delivery result"
    );
    let store = crate::review_retained::RetainedStore::open(&s.daemon.paths, false)
        .unwrap()
        .unwrap();
    assert_eq!(store.expire(u64::MAX).unwrap(), 1);
    assert_eq!(std::fs::read(&s.path).unwrap(), s.new.as_bytes());
    no_network(&s);
}
