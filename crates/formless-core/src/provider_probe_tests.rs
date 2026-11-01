//! Real loopback transport tests for the private operational-probe scope.
use super::*;
use crate::probe_policy::ProbePolicy;
use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn server(app: Router) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        Server(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap()
        })),
    )
}
fn cfg(base: String, style: &str) -> ProviderCfg {
    ProviderCfg {
        base_url: base,
        api_style: style.into(),
        model: "probe-model".into(),
        max_retries: 0,
        fallback_models: vec!["unrelated-paid-model".into()],
        ..Default::default()
    }
}
fn ok(body: &Value, style: &str) -> axum::response::Response {
    let usage = json!({"prompt_tokens":3,"completion_tokens":2,"input_tokens":3,"output_tokens":2});
    if body["stream"] == true {
        let events = match style {
            "anthropic" => vec![
                json!({"type":"message_start","message":{"usage":{"input_tokens":3}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                json!({"type":"message_delta","usage":{"output_tokens":2},"delta":{"stop_reason":"end_turn"}}),
                json!({"type":"message_stop"}),
            ],
            "responses" => vec![
                json!({"type":"response.output_text.delta","delta":"ok"}),
                json!({"type":"response.completed","response":{"usage":usage}}),
            ],
            _ => vec![
                json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":usage}),
            ],
        };
        let text = events
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>()
            + "data: [DONE]\n\n";
        return ([("content-type", "text/event-stream")], text).into_response();
    }
    let reply = match style {
        "anthropic" => json!({"content":[{"type":"text","text":"ok"}],"usage":usage}),
        "responses" => {
            json!({"output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}],"usage":usage})
        }
        _ => json!({"choices":[{"message":{"content":"ok"}}],"usage":usage}),
    };
    Json(reply).into_response()
}
fn assert_cap(body: &Value, style: &str, cap: u64) {
    let key = match style {
        "anthropic" => "max_tokens",
        "responses" => "max_output_tokens",
        _ => "max_tokens",
    };
    assert_eq!(body[key], cap, "{body}");
    for other in ["max_tokens", "max_output_tokens", "max_completion_tokens"] {
        if other != key {
            assert!(body.get(other).is_none(), "{body}");
        }
    }
    for key in ["thinking", "reasoning", "reasoning_effort"] {
        assert!(body.get(key).is_none(), "{body}");
    }
    assert_eq!(body["model"], "probe-model");
}
#[tokio::test]
async fn probe_http_dialects_stream_and_complete_cap_after_hostile_overrides_and_meter_once() {
    let _env = crate::credential_env::EnvGuard::isolated();
    _env.set("NO_PROXY", "127.0.0.1,localhost");
    for style in ["chat", "responses", "anthropic"] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let endpoint = match style {
            "responses" => "/responses",
            "anthropic" => "/messages",
            _ => "/chat/completions",
        };
        let (base, _server) = server(Router::new().route(
            endpoint,
            post(move |Json(body): Json<Value>| {
                captured.locked().push(body.clone());
                async move { ok(&body, style) }
            }),
        ))
        .await;
        let mut config = cfg(base, style);
        config.extra_body = serde_json::from_value(json!({"max_tokens":999999,"max_output_tokens":-1,"max_completion_tokens":999999,
            "model":"unrelated-paid-model","thinking":{"budget_tokens":999999},"reasoning":{"effort":"high"},"options":{"num_predict":-1}})).unwrap();
        let billed = Arc::new(AtomicUsize::new(0));
        let hook = billed.clone();
        let ordinary = Provider::new(config, "private-dummy".into())
            .unwrap()
            .with_usage_hook(Arc::new(move |_, _, u| {
                assert_eq!(u.prompt_tokens, 3);
                assert_eq!(u.completion_tokens, 2);
                hook.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }));
        let policy = ProbePolicy::default();
        let probe = ordinary.scoped_probe(policy.clone()).unwrap();
        assert_eq!(
            policy
                .run(probe.complete(&[], "probe-model", None))
                .await
                .unwrap()
                .unwrap(),
            "ok"
        );
        let cancel = CancellationToken::new();
        policy
            .run(probe.chat_stream(&[], &[], "probe-model", None, &cancel, |_, _| {}))
            .await
            .unwrap()
            .unwrap();
        let tool = json!({"type":"function","function":{"name":"probe_ping","description":"private fixture","parameters":{"type":"object","properties":{}}}});
        assert!(!policy
            .run(probe_tool_call(&probe, "probe-model", &tool, true))
            .await
            .unwrap()
            .unwrap());
        assert_eq!(
            billed.load(Ordering::SeqCst),
            3,
            "observer must not bill twice"
        );
        assert_eq!(policy.report()["requests_sent"], 3);
        assert_eq!(policy.report()["reported_usage"]["prompt_tokens"], 9);
        for body in seen.locked().iter() {
            assert_cap(body, style, 128);
            assert_eq!(body["options"]["num_predict"], 128);
        }
        assert!(ordinary.probe_policy.is_none());
    }
}
#[tokio::test]
async fn probe_http_anthropic_repair_to_64_survives_reapplied_extra_body() {
    let _env = crate::credential_env::EnvGuard::isolated();
    _env.set("NO_PROXY", "127.0.0.1,localhost");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let (base, _server) = server(Router::new().route(
        "/messages",
        post(move |Json(body): Json<Value>| {
            let index = {
                let mut rows = captured.locked();
                rows.push(body.clone());
                rows.len()
            };
            async move {
                if index == 1 {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error":{"message":"max_tokens 128 > 64"}})),
                    )
                        .into_response()
                } else {
                    ok(&body, "anthropic")
                }
            }
        }),
    ))
    .await;
    let mut config = cfg(base, "anthropic");
    config.extra_body.insert("max_tokens".into(), json!(999999));
    let policy = ProbePolicy::default();
    let p = Provider::new(config, "private-dummy".into())
        .unwrap()
        .scoped_probe(policy.clone())
        .unwrap();
    policy
        .run(p.complete(&[], "probe-model", None))
        .await
        .unwrap()
        .unwrap();
    let rows = seen.locked();
    assert_eq!(rows.len(), 2);
    assert_cap(&rows[0], "anthropic", 128);
    assert_cap(&rows[1], "anthropic", 64);
}
#[tokio::test]
async fn probe_http_shared_eight_send_budget_blocks_ninth_subcall_before_http() {
    let _env = crate::credential_env::EnvGuard::isolated();
    _env.set("NO_PROXY", "127.0.0.1,localhost");
    let sent = Arc::new(AtomicUsize::new(0));
    let captured = sent.clone();
    let (base, _server) = server(Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<Value>| {
            captured.fetch_add(1, Ordering::SeqCst);
            async move { ok(&body, "chat") }
        }),
    ))
    .await;
    let policy = ProbePolicy::default();
    let p = Provider::new(cfg(base, "chat"), "private-dummy".into())
        .unwrap()
        .scoped_probe(policy.clone())
        .unwrap();
    for _ in 0..8 {
        policy
            .run(p.complete(&[], "probe-model", None))
            .await
            .unwrap()
            .unwrap();
    }
    let err = policy
        .run(p.complete(&[], "probe-model", None))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("physical POST budget"), "{err:#}");
    assert_eq!(sent.load(Ordering::SeqCst), 8);
}
#[tokio::test]
async fn probe_http_exhausted_admission_does_not_send_or_reserve_a_physical_post() {
    let _env = crate::credential_env::EnvGuard::isolated();
    _env.set("NO_PROXY", "127.0.0.1,localhost");
    let sent = Arc::new(AtomicUsize::new(0));
    let captured = sent.clone();
    let (base, _server) = server(Router::new().route(
        "/chat/completions",
        post(move || {
            captured.fetch_add(1, Ordering::SeqCst);
            async { "unexpected" }
        }),
    ))
    .await;
    let policy = ProbePolicy::default();
    let p = Provider::new(cfg(base, "chat"), "private-dummy".into())
        .unwrap()
        .with_request_hook(Arc::new(|| {
            Box::pin(async { anyhow::bail!("private exhausted budget") })
        }))
        .scoped_probe(policy.clone())
        .unwrap();
    let err = policy
        .run(p.complete(&[], "probe-model", None))
        .await
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("exhausted budget"), "{err:#}");
    assert_eq!(sent.load(Ordering::SeqCst), 0);
    assert_eq!(policy.report()["requests_sent"], 0);
}
#[tokio::test]
async fn probe_http_deadline_cancels_admission_headers_body_and_retry_waits() {
    let _env = crate::credential_env::EnvGuard::isolated();
    _env.set("NO_PROXY", "127.0.0.1,localhost");
    for phase in ["admission", "headers", "body", "retry"] {
        let sent = Arc::new(AtomicUsize::new(0));
        let captured = sent.clone();
        let (base, _server) = server(Router::new().route(
            "/chat/completions",
            post(move || {
                captured.fetch_add(1, Ordering::SeqCst);
                async move {
                    match phase {
                        "headers" => std::future::pending::<axum::response::Response>().await,
                        "body" => axum::response::Response::new(axum::body::Body::from_stream(
                            futures_util::stream::pending::<Result<Vec<u8>, std::io::Error>>(),
                        )),
                        "retry" => (
                            StatusCode::TOO_MANY_REQUESTS,
                            [("retry-after", "120")],
                            "rate limited",
                        )
                            .into_response(),
                        _ => unreachable!(),
                    }
                }
            }),
        ))
        .await;
        let mut config = cfg(base, "chat");
        config.max_retries = 20;
        let mut p = Provider::new(config, "private-dummy".into()).unwrap();
        if phase == "admission" {
            p = p.with_request_hook(Arc::new(|| Box::pin(std::future::pending())));
        }
        let policy = ProbePolicy::limited(Duration::from_secs(1), 8);
        let p = p.scoped_probe(policy.clone()).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            policy.run(p.complete(&[], "probe-model", None)),
        )
        .await
        .unwrap();
        assert!(
            result.unwrap_err().to_string().contains("logical deadline"),
            "{phase}"
        );
        assert_eq!(
            sent.load(Ordering::SeqCst),
            usize::from(phase != "admission"),
            "{phase}"
        );
    }
}
#[tokio::test]
async fn probe_http_redirect_is_not_followed_and_ordinary_cap_is_unchanged() {
    let _env = crate::credential_env::EnvGuard::isolated();
    _env.set("NO_PROXY", "127.0.0.1,localhost");
    let target = Arc::new(AtomicUsize::new(0));
    let captured = target.clone();
    let (target_url, _target) = server(Router::new().route(
        "/redirected",
        post(move || {
            captured.fetch_add(1, Ordering::SeqCst);
            async { "unexpected" }
        }),
    ))
    .await;
    let destination = format!("{target_url}/redirected");
    let (base, _server) = server(Router::new().route(
        "/chat/completions",
        post(move || {
            let location = destination.clone();
            async move { (StatusCode::TEMPORARY_REDIRECT, [("location", location)], "") }
        }),
    ))
    .await;
    let policy = ProbePolicy::default();
    let p = Provider::new(cfg(base, "chat"), "private-dummy".into())
        .unwrap()
        .scoped_probe(policy.clone())
        .unwrap();
    assert!(policy
        .run(p.complete(&[], "probe-model", None))
        .await
        .unwrap()
        .is_err());
    assert_eq!(target.load(Ordering::SeqCst), 0);
    assert_eq!(policy.report()["requests_sent"], 1);
    let (base, _ordinary) = server(Router::new().route(
        "/chat/completions",
        post(|Json(body): Json<Value>| async move {
            assert_eq!(body["max_tokens"], 777);
            ok(&body, "chat")
        }),
    ))
    .await;
    let mut config = cfg(base, "chat");
    config.extra_body.insert("max_tokens".into(), json!(777));
    Provider::new(config, "private-dummy".into())
        .unwrap()
        .complete(&[], "probe-model", None)
        .await
        .unwrap();
}
#[tokio::test]
async fn probe_http_forced_unforced_and_prompt_fallback_share_policy_and_metering() {
    let _env = crate::credential_env::EnvGuard::isolated();
    _env.set("NO_PROXY", "127.0.0.1,localhost");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let (base,_server)=server(Router::new().route("/chat/completions",post(move |Json(body):Json<Value>| {
        captured.locked().push(body.clone());
        async move {
            if body.get("tool_choice").is_some() {return (StatusCode::BAD_REQUEST,Json(json!({"error":{"message":"tool_choice is not supported"}}))).into_response();}
            if body.get("tools").is_some() {return ok(&body,"chat");}
            if body["stream"]!=true {return Json(json!({"choices":[{"message":{"content":"{\"tool_calls\":[{\"name\":\"probe_ping\",\"arguments\":{}}]}"}}],"usage":{"prompt_tokens":3,"completion_tokens":2}})).into_response();}
            ok(&body,"chat")
        }
    }))).await;
    let billed = Arc::new(AtomicUsize::new(0));
    let captured = billed.clone();
    let p = Provider::new(cfg(base, "chat"), "private-dummy".into())
        .unwrap()
        .with_usage_hook(Arc::new(move |_, _, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
    let result = p
        .test_model_scoped("probe-model", ProbePolicy::default())
        .await;
    assert!(result.ok && result.prompt_fallback, "{result:?}");
    let rows = seen.locked();
    assert_eq!(rows.len(), 4);
    assert!(rows[1].get("tool_choice").is_some());
    assert!(rows[2].get("tool_choice").is_none());
    assert!(rows[2].get("tools").is_some());
    assert!(rows[3].get("tools").is_none());
    for row in rows.iter() {
        assert_cap(row, "chat", 128);
    }
    assert_eq!(
        billed.load(Ordering::SeqCst),
        3,
        "only three responses reported usage; no double billing"
    );
    let usage = result.probe_usage.unwrap();
    assert_eq!(usage["requests_sent"], 4);
    assert_eq!(usage["reported_usage"]["prompt_tokens"], 9);
    assert_eq!(usage["usage_partial"], true);
}

#[tokio::test]
async fn probe_http_partial_stream_timeout_accounts_once_before_releasing_admission() {
    use futures_util::StreamExt;
    let _env = crate::credential_env::EnvGuard::isolated();
    _env.set("NO_PROXY", "127.0.0.1,localhost");
    let arrived = Arc::new(tokio::sync::Notify::new());
    let captured = arrived.clone();
    let (base, _server) = server(Router::new().route(
        "/chat/completions",
        post(move || {
            let arrived = captured.clone();
            async move {
                let first = format!(
                    "data: {}\n\n",
                    json!({"choices":[{"delta":{"content":"partial"}}],"usage":{"prompt_tokens":3}})
                );
                let chunks = futures_util::stream::once(async move {
                    arrived.notify_one();
                    Ok::<_, std::io::Error>(first.into_bytes())
                })
                .chain(futures_util::stream::pending());
                (
                    [("content-type", "text/event-stream")],
                    axum::body::Body::from_stream(chunks),
                )
                    .into_response()
            }
        }),
    ))
    .await;
    struct Lease(Arc<AtomicUsize>);
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let active = Arc::new(AtomicUsize::new(0));
    let started = active.clone();
    let billed = Arc::new(Mutex::new(Vec::new()));
    let recorded = billed.clone();
    let held = active.clone();
    let p = Provider::new(cfg(base, "chat"), "private-dummy".into())
        .unwrap()
        .with_request_hook(Arc::new(move || {
            let active = started.clone();
            Box::pin(async move {
                active.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Lease(active)) as RequestLease)
            })
        }))
        .with_usage_hook(Arc::new(move |_, _, u| {
            assert_eq!(
                held.load(Ordering::SeqCst),
                1,
                "billing must precede admission release"
            );
            recorded.locked().push(*u);
            Ok(())
        }));
    let policy = ProbePolicy::limited(Duration::from_secs(1), 8);
    let p = p.scoped_probe(policy.clone()).unwrap();
    let cancel = CancellationToken::new();
    let err = policy
        .run(p.chat_stream(&[], &[], "probe-model", None, &cancel, |_, _| {}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("logical deadline"));
    assert_eq!(
        active.load(Ordering::SeqCst),
        0,
        "no abandoned request lease"
    );
    let rows = billed.locked();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].prompt_tokens, 3);
    assert_eq!(rows[0].reported, 1);
    let report = policy.report();
    assert_eq!(report["requests_sent"], 1);
    assert_eq!(report["usage_partial"], true);
    assert!(report["reported_usage"].get("completion_tokens").is_none());
}

#[tokio::test]
async fn probe_http_rejected_chat_cap_switches_spelling_but_remains_bounded() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let (base,_server)=server(Router::new().route("/chat/completions",post(move |Json(body):Json<Value>| {
        let index={let mut rows=captured.locked();rows.push(body.clone());rows.len()};
        async move {if index==1 {(StatusCode::BAD_REQUEST,Json(json!({"error":{"param":"max_tokens","message":"unsupported max_tokens; use max_completion_tokens"}}))).into_response()} else {ok(&body,"chat")}}
    }))).await;
    let policy = ProbePolicy::default();
    let p = Provider::new(cfg(base, "chat"), "private-dummy".into())
        .unwrap()
        .scoped_probe(policy.clone())
        .unwrap();
    policy
        .run(p.complete(&[], "probe-model", None))
        .await
        .unwrap()
        .unwrap();
    let rows = seen.locked();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["max_tokens"], 128);
    assert!(rows[1].get("max_tokens").is_none());
    assert_eq!(rows[1]["max_completion_tokens"], 128);
}
#[tokio::test]
async fn probe_http_two_request_cache_check_uses_one_shared_bounded_scope() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let (base, _server) = server(Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<Value>| {
            captured.locked().push(body.clone());
            async move { ok(&body, "chat") }
        }),
    ))
    .await;
    let policy = ProbePolicy::default();
    let p = Provider::new(cfg(base, "chat"), "private-dummy".into())
        .unwrap()
        .scoped_probe(policy.clone())
        .unwrap();
    assert!(policy
        .run(cache_probe_provider_inner(&p, "probe-model"))
        .await
        .unwrap()
        .unwrap()
        .is_some());
    let rows = seen.locked();
    assert_eq!(rows.len(), 2);
    for row in rows.iter() {
        assert_cap(row, "chat", 128);
        assert_eq!(row["stream"], true);
    }
    assert_eq!(policy.report()["requests_sent"], 2);
}

#[tokio::test]
async fn probe_http_continual_trickle_cannot_extend_deadline_or_make_interim_usage_final() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let (base,_server)=server(Router::new().route("/chat/completions",post(|| async {
        let chunks=futures_util::stream::unfold(0u64,|count|async move{
            tokio::time::sleep(Duration::from_millis(10)).await;
            let event=json!({"choices":[{"delta":{"content":"x"}}],"usage":{"prompt_tokens":3,"completion_tokens":count+1,"cost":0.001}});
            Some((Ok::<_,std::io::Error>(format!("data: {event}\n\n").into_bytes()),count+1))
        });
        ([("content-type","text/event-stream")],axum::body::Body::from_stream(chunks)).into_response()
    }))).await;
    let billed = Arc::new(AtomicUsize::new(0));
    let captured = billed.clone();
    let p = Provider::new(cfg(base, "chat"), "private-dummy".into())
        .unwrap()
        .with_usage_hook(Arc::new(move |_, _, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
    let policy = ProbePolicy::limited(Duration::from_secs(1), 8);
    let p = p.scoped_probe(policy.clone()).unwrap();
    let cancel = CancellationToken::new();
    let mut deltas = 0;
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        policy.run(
            p.chat_stream(&[], &[], "probe-model", None, &cancel, |kind, _| {
                if kind == DeltaKind::Content {
                    deltas += 1
                }
            }),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(err.to_string().contains("logical deadline"));
    assert!(
        deltas > 2,
        "fixture must actually progress before logical timeout"
    );
    assert_eq!(billed.load(Ordering::SeqCst), 1);
    let report = policy.report();
    assert_eq!(report["deadline_exceeded"], true);
    assert_eq!(report["requests_sent"], 1);
    assert_eq!(report["reported_usage"]["prompt_tokens"], 3);
    assert!(
        report["reported_usage"]["completion_tokens"]
            .as_u64()
            .unwrap()
            > 2
    );
    assert_eq!(report["usage_partial"], true);
    assert_eq!(report["cost_partial"], true);
}

#[tokio::test]
async fn probe_http_managed_oauth_401_cannot_refresh_env_token_or_fail_over() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    env.set(
        "FORMLESS_OAUTH_ACCESS_TOKEN",
        "private-unrefreshable-fixture-token",
    );
    let sent = Arc::new(AtomicUsize::new(0));
    let captured = sent.clone();
    let refreshes = Arc::new(AtomicUsize::new(0));
    let token_posts = refreshes.clone();
    let (base,_server)=server(Router::new()
        .route("/chat/completions",post(move |headers:axum::http::HeaderMap,Json(body):Json<Value>| {
            assert_eq!(headers["authorization"],"Bearer private-unrefreshable-fixture-token");assert_cap(&body,"chat",128);
            captured.fetch_add(1,Ordering::SeqCst);
            async{(StatusCode::UNAUTHORIZED,Json(json!({"error":{"message":"invalid_token: private fixture rejected"}})))}
        }))
        .route("/token",post(move||{token_posts.fetch_add(1,Ordering::SeqCst);async{"unexpected"}}))).await;
    struct Profile(std::path::PathBuf);
    impl Drop for Profile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = Profile(
        std::env::temp_dir().join(format!("fl-private-probe-oauth-{}", uuid::Uuid::new_v4())),
    );
    let paths = crate::Paths::from_dirs(root.0.join("data"), root.0.join("config")).unwrap();
    let mut config = cfg(base.clone(), "chat");
    config.auth.mode = "oauth".into();
    config.auth.token_url = format!("{base}/token");
    config.auth.client_id = "private-fixture-client".into();
    // The explicit dummy env token takes the managed credential path without
    // reading/writing token files or touching a native keychain.
    let provider = Provider::for_profile(config, paths).await.unwrap();
    let result = provider
        .test_model_scoped("probe-model", ProbePolicy::default())
        .await;
    assert!(!result.ok);
    assert!(
        result.error.as_deref().unwrap().contains("401"),
        "{result:?}"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 1);
    assert_eq!(refreshes.load(Ordering::SeqCst), 0);
    assert_eq!(result.probe_usage.unwrap()["requests_sent"], 1);
}

#[tokio::test]
async fn probe_http_schema_mode_and_caps_compose_without_echo_driven_repairs() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let schema = json!({"type":"object","properties":{"answer":{"const":7}},
        "required":["answer"],"additionalProperties":false,"default":{"max_tokens":99999,"n":77}});
    for style in ["chat", "responses", "anthropic"] {
        for unrelated in [false, true] {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let captured = seen.clone();
            let endpoint = match style {
                "anthropic" => "/messages",
                "responses" => "/responses",
                _ => "/chat/completions",
            };
            let field = if style == "responses" {
                "text.format"
            } else {
                "response_format"
            };
            let (base, _server) = server(Router::new().route(endpoint, post(move |Json(body): Json<Value>| {
                let index = { let mut rows = captured.locked(); let index = rows.len(); rows.push(body.clone()); index };
                async move {
                    if unrelated {
                        return (StatusCode::BAD_REQUEST, Json(json!({"error":{"param":"temperature","code":"unknown_parameter","message":"Unknown parameter temperature"},"request":body}))).into_response();
                    }
                    if style != "anthropic" && index == 0 {
                        return (StatusCode::BAD_REQUEST, Json(json!({"error":{"param":field,"message":format!("{field} json_schema is not supported")},"request":body}))).into_response();
                    }
                    let usage = json!({"input_tokens":3,"output_tokens":2,"prompt_tokens":3,"completion_tokens":2});
                    let result = match style {
                        "anthropic" => json!({"model":"probe-model","content":[{"type":"text","text":"{\"answer\":7}"}],"stop_reason":"end_turn","usage":usage}),
                        "responses" => json!({"model":"probe-model","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"{\"answer\":7}"}]}],"usage":usage}),
                        _ => json!({"model":"probe-model","choices":[{"message":{"content":"{\"answer\":7}"},"finish_reason":"stop"}],"usage":usage}),
                    };
                    Json(result).into_response()
                }
            }))).await;
            let mut config = cfg(base, style);
            config.extra_body.insert("max_tokens".into(), json!(99999));
            config.extra_body.insert("n".into(), json!(7));
            config.extra_body.insert("best_of".into(), json!(7));
            config
                .extra_body
                .insert("options".into(), json!({"num_return_sequences":7}));
            if style == "chat" {
                config
                    .extra_body
                    .insert("response_format".into(), json!({"type":"json_object"}));
            } else if style == "responses" {
                config
                    .extra_body
                    .insert("text".into(), json!({"format":{"type":"json_object"}}));
            }
            let billed = Arc::new(AtomicUsize::new(0));
            let hook = billed.clone();
            let policy = ProbePolicy::default();
            let provider = Provider::new(config, "private-dummy".into())
                .unwrap()
                .with_usage_hook(Arc::new(move |_, _, _| {
                    hook.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }))
                .scoped_probe(policy.clone())
                .unwrap();
            let options = CompletionOptions::json_schema(schema.clone()).unwrap();
            let result = policy
                .run(provider.complete_json(
                    &[json!({"role":"user","content":"private joint policy fixture"})],
                    "probe-model",
                    None,
                    &options,
                ))
                .await
                .unwrap();
            let rows = seen.locked();
            if unrelated {
                assert!(result.is_err());
                assert_eq!(
                    rows.len(),
                    1,
                    "echoed schema/cap fields cannot trigger any retry"
                );
                assert_eq!(billed.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(result.unwrap().value, json!({"answer":7}));
                assert_eq!(rows.len(), if style == "anthropic" { 1 } else { 2 });
                assert_eq!(billed.load(Ordering::SeqCst), 1);
            }
            assert_eq!(policy.report()["requests_sent"], rows.len());
            for row in rows.iter() {
                assert_cap(row, style, 128);
                if style == "chat" {
                    assert_eq!(row["n"], 1);
                } else {
                    assert!(row.get("n").is_none());
                }
                assert!(row.get("best_of").is_none());
                assert!(row["options"].get("num_return_sequences").is_none());
            }
            if style == "chat" {
                assert_eq!(rows[0]["response_format"]["json_schema"]["schema"], schema);
                if !unrelated {
                    assert!(rows[1].get("response_format").is_none());
                }
            } else if style == "responses" {
                assert_eq!(rows[0]["text"]["format"]["schema"], schema);
                if !unrelated {
                    assert!(rows[1]["text"].get("format").is_none());
                }
            }
        }
    }
}

#[tokio::test]
async fn probe_http_choice_multipliers_cannot_expand_the_output_budget() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = seen.clone();
    let (base, _server) = server(Router::new().route("/chat/completions", post(move |Json(body): Json<Value>| {
        let count = body["n"].as_u64().unwrap_or(1);
        assert!((1..=7).contains(&count));
        captured.locked().push(body);
        async move {
            let choices: Vec<Value> = (0..count).map(|index| json!({"index":index,"message":{"content":"ok"},"finish_reason":"stop"})).collect();
            Json(json!({"model":"probe-model","choices":choices,"usage":{"prompt_tokens":3,"completion_tokens":2*count}}))
        }
    }))).await;
    let mut config = cfg(base, "chat");
    config.extra_body.insert("n".into(), json!(7));
    config.extra_body.insert("best_of".into(), json!(7));
    config
        .extra_body
        .insert("num_return_sequences".into(), json!(7));
    config
        .extra_body
        .insert("options".into(), json!({"num_outputs":7}));
    let billed = Arc::new(AtomicUsize::new(0));
    let hook = billed.clone();
    let ordinary = Provider::new(config, "private-dummy".into())
        .unwrap()
        .with_usage_hook(Arc::new(move |_, _, usage| {
            hook.fetch_add(usage.completion_tokens as usize, Ordering::SeqCst);
            Ok(())
        }));
    assert_eq!(
        ordinary.complete(&[], "probe-model", None).await.unwrap(),
        "ok"
    );
    assert_eq!(
        billed.load(Ordering::SeqCst),
        14,
        "ordinary owner-configured multiplicity is unchanged"
    );
    let policy = ProbePolicy::default();
    let probe = ordinary.scoped_probe(policy.clone()).unwrap();
    assert_eq!(
        policy
            .run(probe.complete(&[], "probe-model", None))
            .await
            .unwrap()
            .unwrap(),
        "ok"
    );
    let rows = seen.locked();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["n"], 7);
    assert_eq!(
        rows[1]["n"], 1,
        "probe must request only one billable output choice"
    );
    for key in ["best_of", "num_return_sequences"] {
        assert!(rows[1].get(key).is_none(), "{rows:?}");
    }
    assert!(rows[1]["options"].get("num_outputs").is_none());
    assert_eq!(
        billed.load(Ordering::SeqCst),
        16,
        "only two probe output tokens are charged"
    );
    assert_eq!(policy.report()["reported_usage"]["completion_tokens"], 2);
}
