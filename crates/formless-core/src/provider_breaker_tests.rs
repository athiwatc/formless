use super::*;
use axum::{extract::State, http::HeaderMap, response::IntoResponse, routing::post, Json, Router};
use std::sync::atomic::{AtomicUsize, Ordering};

fn config(base: &str) -> ProviderCfg {
    ProviderCfg {
        base_url: base.into(),
        model: "bad".into(),
        fallback_models: vec!["good".into()],
        max_retries: 0,
        max_backoff_secs: 1,
        ..Default::default()
    }
}
async fn server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn stop(task: tokio::task::JoinHandle<()>) {
    task.abort();
    if let Err(e) = task.await {
        assert!(e.is_cancelled(), "{e}");
    }
}
fn good(body: &Value) -> axum::response::Response {
    if body["stream"] == true {
        format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]})
        )
        .into_response()
    } else {
        Json(json!({"choices":[{"message":{"content":"ok"}}]})).into_response()
    }
}
fn expire(p: &Provider) {
    let mut map = BREAKERS.lock().unwrap_or_else(|e| e.into_inner());
    map.as_mut()
        .unwrap()
        .get_mut(&p.breaker_key("bad", "k"))
        .unwrap()
        .open_until = Some(std::time::Instant::now());
}
fn open(p: &Provider) {
    let lease = p.breaker_permit("bad", "k").unwrap();
    assert!(!lease.failure("fixture", false));
    assert!(!lease.failure("fixture", false));
    assert!(lease.failure("fixture", false));
}
async fn stream(p: &Provider) -> Result<ChatResult> {
    tokio::time::timeout(
        Duration::from_secs(10),
        p.chat_stream(
            &[json!({"role":"user","content":"hello"})],
            &[],
            "bad",
            None,
            &CancellationToken::new(),
            |_, _| {},
        ),
    )
    .await
    .context("breaker fixture stream never completed")?
}
async fn complete(p: &Provider) -> Result<String> {
    tokio::time::timeout(Duration::from_secs(10), p.complete(&[], "bad", None))
        .await
        .context("breaker fixture completion never completed")?
}

#[test]
fn stream_errors_count_availability_but_not_user_rejections() {
    assert_eq!(
        stream_failure_counts("invalid_request_error", "oversized context"),
        (false, false)
    );
    assert_eq!(
        stream_failure_counts("unknown", "stream failed"),
        (true, false)
    );
    assert_eq!(stream_failure_counts("server_error", "down"), (true, false));
    assert_eq!(
        stream_failure_counts("invalid_request_error", "model not found"),
        (true, true)
    );
}

#[test]
fn breaker_identity_normalizes_urls_and_separates_credentials_and_names() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let a = Provider::new(config("HTTPS://Example.COM:443/v1/"), "k".into()).unwrap();
    let b = Provider::new(config("https://example.com/v1"), "k".into()).unwrap();
    assert_eq!(a.breaker_key("bad", "k"), b.breaker_key("bad", "k"));
    assert_ne!(a.breaker_key("bad", "k"), b.breaker_key("bad", "other-key"));
    let b = b.with_name("work");
    assert_ne!(a.breaker_key("bad", "k"), b.breaker_key("bad", "k"));
}

#[test]
fn cancelled_probe_releases_ownership_and_stale_success_cannot_clear_new_failures() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let notes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = notes.clone();
    let p = Provider::new(
        config(&format!("http://{}.invalid", uuid::Uuid::new_v4())),
        "k".into(),
    )
    .unwrap()
    .with_note_hook(Arc::new(move |n| received.lock().unwrap().push(n)));
    open(&p);
    expire(&p);
    let cancelled = p.breaker_permit("bad", "k").unwrap();
    assert!(p.breaker_permit("bad", "k").is_none());
    drop(cancelled);
    let recovered = p.breaker_permit("bad", "k").unwrap();
    recovered.success();
    let fresh = p.breaker_permit("bad", "k").unwrap();
    assert!(!fresh.failure("new failure", false));
    recovered.success(); // Late completion from the old probe is stale.
    let map = BREAKERS.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(map.as_ref().unwrap()[&p.breaker_key("bad", "k")].fails, 1);
    drop(map);
    assert_eq!(notes.lock().unwrap().len(), 2, "one open and one close");
}

#[test]
fn breaker_cache_is_bounded_and_never_evicts_active_requests() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let prefix = uuid::Uuid::new_v4().to_string();
    let active_key = format!("{prefix}-active");
    let active = BreakerPermit::acquire(active_key.clone(), "active".into(), None).unwrap();
    for i in 0..MAX_BREAKERS + 2 {
        drop(BreakerPermit::acquire(format!("{prefix}-{i}"), "unused".into(), None).unwrap());
    }
    let map = BREAKERS.lock().unwrap_or_else(|e| e.into_inner());
    assert!(map.as_ref().unwrap().len() <= MAX_BREAKERS);
    assert!(map.as_ref().unwrap().contains_key(&active_key));
    drop(map);
    let mut leases = vec![active];
    for i in 0..MAX_BREAKERS - 1 {
        leases.push(
            BreakerPermit::acquire(format!("{prefix}-busy-{i}"), "busy".into(), None).unwrap(),
        );
    }
    assert!(
        BreakerPermit::acquire(format!("{prefix}-overflow"), "overflow".into(), None).is_none(),
        "all-active saturation must refuse new identities"
    );
    let map = BREAKERS.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(map.as_ref().unwrap().len(), MAX_BREAKERS);
    assert!(map.as_ref().unwrap().contains_key(&active_key));
    drop(map);
    drop(leases);
    assert!(
        BreakerPermit::acquire(format!("{prefix}-overflow"), "overflow".into(), None).is_some()
    );
}

#[tokio::test]
async fn model_missing_opens_immediately_but_keys_and_bad_parameters_are_isolated() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let hits = Arc::new(std::sync::Mutex::new(HashMap::<String, usize>::new()));
    let app = Router::new()
        .route(
            "/chat/completions",
            post(
                |State(hits): State<Arc<std::sync::Mutex<HashMap<String, usize>>>>,
                 headers: HeaderMap,
                 Json(body): Json<Value>| async move {
                    let key = headers["authorization"].to_str().unwrap().to_string();
                    if body["model"] == "bad" {
                        *hits.lock().unwrap().entry(key.clone()).or_default() += 1;
                        if key.ends_with("quota") {
                            return (
                                axum::http::StatusCode::TOO_MANY_REQUESTS,
                                "account quota exhausted",
                            )
                                .into_response();
                        }
                        if key.ends_with("bad-params") {
                            return (axum::http::StatusCode::BAD_REQUEST, "oversized context")
                                .into_response();
                        }
                        if key.ends_with("healthy") {
                            return good(&body);
                        }
                        return (axum::http::StatusCode::NOT_FOUND, "model not found")
                            .into_response();
                    }
                    good(&body)
                },
            ),
        )
        .with_state(hits.clone());
    let (base, task) = server(app).await;
    let missing = Provider::new(config(&base), "missing".into()).unwrap();
    for _ in 0..2 {
        assert_eq!(complete(&missing).await.unwrap(), "ok");
    }
    assert_eq!(hits.lock().unwrap()["Bearer missing"], 1);
    let quota = Provider::new(config(&base), "quota".into()).unwrap();
    for _ in 0..4 {
        assert_eq!(complete(&quota).await.unwrap(), "ok");
    }
    assert_eq!(hits.lock().unwrap()["Bearer quota"], BREAKER_TRIP as usize);
    let healthy = Provider::new(config(&base), "healthy".into()).unwrap();
    assert_eq!(complete(&healthy).await.unwrap(), "ok");
    assert_eq!(hits.lock().unwrap()["Bearer healthy"], 1);
    let mut cfg = config(&base);
    cfg.fallback_models.clear();
    let invalid = Provider::new(cfg, "bad-params".into()).unwrap();
    for _ in 0..4 {
        assert!(complete(&invalid)
            .await
            .unwrap_err()
            .to_string()
            .contains("400"));
    }
    let map = BREAKERS.lock().unwrap_or_else(|e| e.into_inner());
    let state = &map.as_ref().unwrap()[&invalid.breaker_key("bad", "bad-params")];
    assert_eq!(state.fails, 0);
    assert!(state.open_until.is_none());
    drop(map);
    stop(task).await;
}

fn failing_body(release: Arc<tokio::sync::Notify>) -> axum::body::Body {
    axum::body::Body::from_stream(futures_util::stream::unfold(0, move |phase| {
        let release = release.clone();
        async move {
            match phase {
                0 => Some((
                    Ok::<_, std::io::Error>(format!(
                        "data: {}\n\n",
                        json!({"choices":[{"delta":{"content":"partial"}}]})
                    )),
                    1,
                )),
                1 => {
                    release.notified().await;
                    Some((Err(std::io::Error::other("controlled body failure")), 2))
                }
                _ => None,
            }
        }
    }))
}

#[tokio::test]
async fn only_one_half_open_stream_probes_and_failure_immediately_doubles_cooldown() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    let app = Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<Value>| {
            let hits = counted.clone();
            let gate = gate.clone();
            async move {
                if body["model"] == "bad" {
                    hits.fetch_add(1, Ordering::SeqCst);
                    axum::response::Response::new(failing_body(gate))
                } else {
                    good(&body)
                }
            }
        }),
    );
    let (base, task) = server(app).await;
    let mut cfg = config(&base);
    cfg.max_retries = 1;
    let p = Arc::new(Provider::new(cfg, "k".into()).unwrap());
    open(&p);
    expire(&p);
    let parsed = Arc::new(tokio::sync::Notify::new());
    let observed = parsed.clone();
    let probe = p.clone();
    let first = tokio::spawn(async move {
        probe
            .chat_stream(
                &[],
                &[],
                "bad",
                None,
                &CancellationToken::new(),
                |kind, text| {
                    if kind == DeltaKind::Content && text == "partial" {
                        observed.notify_one();
                    }
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), parsed.notified())
        .await
        .expect("probe headers/body were never parsed");
    assert_eq!(
        stream(&p).await.unwrap().content,
        "ok",
        "concurrent request skips the in-flight probe"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), first)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .content,
        "ok"
    );
    let map = BREAKERS.lock().unwrap_or_else(|e| e.into_inner());
    let b = &map.as_ref().unwrap()[&p.breaker_key("bad", "k")];
    assert_eq!(b.cooldown, Duration::from_secs(120));
    assert!(!b.probing);
    assert!(b.open_until.unwrap() > std::time::Instant::now());
    drop(map);
    assert_eq!(stream(&p).await.unwrap().content, "ok");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    stop(task).await;
}

#[tokio::test]
async fn successful_headers_never_reset_repeated_stream_body_failures() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    let app = Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<Value>| {
            let hits = counted.clone();
            let gate = gate.clone();
            async move {
                if body["model"] == "bad" {
                    hits.fetch_add(1, Ordering::SeqCst);
                    axum::response::Response::new(failing_body(gate))
                } else {
                    good(&body)
                }
            }
        }),
    );
    let (base, task) = server(app).await;
    let p = Provider::new(config(&base), "k".into()).unwrap();
    for _ in 0..BREAKER_TRIP {
        let cancel = CancellationToken::new();
        let turn = p.chat_stream(&[], &[], "bad", None, &cancel, |kind, text| {
            if kind == DeltaKind::Content && text == "partial" {
                release.notify_one();
            }
        });
        assert!(tokio::time::timeout(Duration::from_secs(10), turn)
            .await
            .unwrap()
            .is_err());
    }
    assert_eq!(stream(&p).await.unwrap().content, "ok");
    assert_eq!(hits.load(Ordering::SeqCst), BREAKER_TRIP as usize);
    stop(task).await;
}

#[tokio::test]
async fn retryable_error_events_after_successful_headers_count_in_all_stream_apis() {
    let _env = crate::credential_env::EnvGuard::isolated();
    for style in ["chat", "responses", "anthropic"] {
        let bad = match style {
            "chat" => json!({"error":{"type":"server_error","message":"down"}}),
            "responses" => json!({"type":"error","code":"server_error","message":"down"}),
            _ => json!({"type":"error","error":{"type":"api_error","message":"down"}}),
        };
        let good_events = match style {
            "chat" => vec![json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]})],
            "responses" => vec![
                json!({"type":"response.output_text.delta","delta":"ok"}),
                json!({"type":"response.completed","response":{}}),
            ],
            _ => vec![
                json!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
                json!({"type":"message_stop"}),
            ],
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let endpoint = match style {
            "responses" => "/responses",
            "anthropic" => "/messages",
            _ => "/chat/completions",
        };
        let app = Router::new().route(
            endpoint,
            post(move |Json(body): Json<Value>| {
                let counted = counted.clone();
                let bad = bad.clone();
                let good_events = good_events.clone();
                async move {
                    let events = if body["model"] == "bad" {
                        counted.fetch_add(1, Ordering::SeqCst);
                        vec![bad]
                    } else {
                        good_events
                    };
                    let text: String = events.iter().map(|v| format!("data: {v}\n\n")).collect();
                    text.into_response()
                }
            }),
        );
        let (base, task) = server(app).await;
        let mut cfg = config(&base);
        cfg.api_style = style.into();
        let p = Provider::new(cfg, "k".into()).unwrap();
        for _ in 0..BREAKER_TRIP {
            assert!(stream(&p).await.is_err(), "{style}");
        }
        assert_eq!(stream(&p).await.unwrap().content, "ok", "{style}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            BREAKER_TRIP as usize,
            "{style}"
        );
        stop(task).await;
    }
}
