//! Real loopback transport tests for the private operational-probe scope.
use super::*;
use crate::probe_policy::ProbePolicy;
use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use std::sync::atomic::{AtomicUsize, Ordering};

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
        config.extra_body.insert("*".into(),json!({"max_tokens":999999,"max_output_tokens":-1,"max_completion_tokens":999999,
            "thinking":{"budget_tokens":999999},"reasoning":{"effort":"high"},"options":{"num_predict":-1}}));
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
        assert_eq!(
            billed.load(Ordering::SeqCst),
            2,
            "observer must not bill twice"
        );
        assert_eq!(policy.report()["requests_sent"], 2);
        assert_eq!(policy.report()["reported_usage"]["prompt_tokens"], 6);
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
    config
        .extra_body
        .insert("*".into(), json!({"max_tokens":999999}));
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
        let policy = ProbePolicy::limited(Duration::from_millis(200), 8);
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
    config
        .extra_body
        .insert("*".into(), json!({"max_tokens":777}));
    Provider::new(config, "private-dummy".into())
        .unwrap()
        .complete(&[], "probe-model", None)
        .await
        .unwrap();
}
