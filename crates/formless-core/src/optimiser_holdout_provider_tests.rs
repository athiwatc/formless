//! Physical provider projection and pre-request durability, using only an owned
//! loopback service and a disposable profile. No public provider is contacted.
use super::*;
use std::path::PathBuf;
use std::sync::Mutex;

struct PrivateProfile(PathBuf);
impl Drop for PrivateProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct CapturedRequest {
    body: String,
    snapshot: ReplaySnapshot,
    digest: String,
    attempts_before_response: i64,
}
struct PrivateServer {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl PrivateServer {
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        let mut task = self.task.take().unwrap();
        let joined = tokio::time::timeout(std::time::Duration::from_secs(5), &mut task).await;
        if joined.is_err() {
            task.abort();
            let _ = task.await;
            panic!("private provider did not stop after owned shutdown");
        }
        joined.unwrap().unwrap();
    }
}
impl Drop for PrivateServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_generation_request_excludes_committed_holdout_and_small_sample_needs_approval() {
    let env = crate::credential_env::EnvGuard::isolated();
    for name in ["FORMLESS_BASE_URL", "FORMLESS_MODEL", "FORMLESS_SECURE"] {
        env.remove(name);
    }
    env.set("FORMLESS_API_KEY", "private-loopback-holdout-fixture");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let profile = PrivateProfile(
        std::env::temp_dir().join(format!("fl-holdout-http-{}", uuid::Uuid::new_v4())),
    );
    let paths = crate::Paths::from_dirs(profile.0.join("data"), profile.0.join("config")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mut cfg = Config::default();
    cfg.provider.base_url = endpoint.clone();
    cfg.provider.model = "private-holdout-model".into();
    cfg.provider.max_retries = 1;
    cfg.provider.auto_test_models = false;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.tools.approval_summaries = false;
    cfg.optimiser.replay_pool_limit = 100;
    cfg.optimiser.replay_holdout_percent = 50;
    cfg.optimiser.replay_min_holdout = 20;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let cfg = daemon.config().unwrap();
    assert_eq!(
        cfg.provider.base_url, endpoint,
        "effective provider must be this test's owned listener"
    );
    let (tool, _) = daemon
        .with_store(|st| {
            st.create_tool_version(
                "holdout_private_target",
                "private holdout fixture",
                "{}",
                "{}",
                "function run(){throw new Error('old implementation');}",
                &Capabilities::default(),
                64,
                "agent",
            )
        })
        .unwrap();
    for index in 0..12 {
        daemon
            .with_store(|st| {
                st.record_invocation(
                    &tool.id,
                    1,
                    None,
                    None,
                    None,
                    "failure",
                    "js",
                    &json!({"case":format!("fixture_case_{index:03}"),"value":1}).to_string(),
                    "",
                    &format!("fixture_error_{index:03}"),
                    1,
                    100,
                )
            })
            .unwrap();
    }
    // Reordered keys and a numerically equivalent spelling must not become a
    // second statistical trial or leak across the partition boundary.
    daemon
        .with_store(|st| {
            st.record_invocation(
                &tool.id,
                1,
                None,
                None,
                None,
                "failure",
                "js",
                r#"{ "value":1.0, "case":"fixture_case_000" }"#,
                "",
                "fixture_error_000",
                1,
                100,
            )
        })
        .unwrap();
    let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let db = paths.db_file();
    let tool_id = tool.id.clone();
    let app = axum::Router::new().route("/chat/completions", axum::routing::post({
        let captured = captured.clone();
        move |body: String| {
            let captured = captured.clone(); let db = db.clone(); let tool_id = tool_id.clone();
            async move {
                // A separate connection cannot observe the writer's uncommitted
                // transaction. Perform this check BEFORE accepting the response.
                let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                let (manifest, digest): (String,String) = conn.query_row("SELECT manifest,digest FROM optimiser_replay_snapshots WHERE tool_id=?1",[&tool_id],|row|Ok((row.get(0)?,row.get(1)?))).expect("replay manifest must be committed before the physical provider request");
                let snapshot: ReplaySnapshot = serde_json::from_str(&manifest).unwrap();
                let attempts_before_response = conn.query_row("SELECT count(*) FROM optimiser_attempts WHERE tool_id=?1",[&tool_id],|row|row.get(0)).unwrap();
                captured.lock().unwrap().push(CapturedRequest { body, snapshot, digest, attempts_before_response });
                let candidate = json!({"description":"private holdout fixture","input_schema":{},"output_schema":{},"implementation_js":"function run(i){return i;}","capabilities":[],"memory_mb":64});
                axum::Json(json!({"model":"private-holdout-model","choices":[{"message":{"content":candidate.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}))
            }
        }
    }));
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = PrivateServer {
        stop: Some(stop),
        task: Some(tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        })),
    };
    let generated = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        daemon.optimise_tool(&tool.id, &cfg),
    )
    .await;
    server.stop().await;
    generated.expect("private generation and bounded held-out replay complete");
    let requests = captured.lock().unwrap();
    assert_eq!(
        requests.len(),
        1,
        "only one owned generation request is expected"
    );
    let request = &requests[0];
    assert_eq!(
        request.attempts_before_response, 0,
        "snapshot predates candidate creation"
    );
    assert_eq!(request.snapshot.source_count, 13);
    assert_eq!(request.snapshot.generation.len(), 6);
    assert_eq!(
        request.snapshot.holdout.len(),
        6,
        "duplicate canonical inputs count once"
    );
    for item in &request.snapshot.holdout {
        let input: Value = serde_json::from_str(&item.input).unwrap();
        let marker = input["case"].as_str().unwrap();
        assert!(
            !request.body.contains(marker),
            "held-out input leaked into physical provider body: {marker}"
        );
        assert!(
            !request.body.contains(&item.error),
            "held-out error leaked into physical provider body"
        );
    }
    for item in &request.snapshot.generation {
        let input: Value = serde_json::from_str(&item.input).unwrap();
        assert!(
            request.body.contains(input["case"].as_str().unwrap()),
            "positive generation marker must reach the real request"
        );
    }
    let attempts = daemon
        .with_store(|st| st.list_attempts(&tool.id, 10))
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0].status, "awaiting_approval",
        "a perfect six-pair win cannot auto-promote"
    );
    let bound = daemon
        .with_store(|st| st.attempt_replay_snapshot(&attempts[0].id))
        .unwrap()
        .unwrap();
    assert_eq!(bound.id, request.snapshot.id);
    assert_eq!(bound.digest, request.digest);
    assert_eq!(bound.holdout, request.snapshot.holdout);
    for item in &bound.holdout {
        let input: Value = serde_json::from_str(&item.input).unwrap();
        assert!(
            !attempts[0].detail.contains(input["case"].as_str().unwrap()),
            "presentation must not disclose held-out inputs"
        );
    }
    assert_eq!(
        daemon
            .with_store(|st| st.get_tool_current(&tool.id))
            .unwrap()
            .unwrap()
            .1
            .version,
        1
    );
}
