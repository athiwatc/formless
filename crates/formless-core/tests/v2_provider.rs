//! v2 provider tranche: Responses API, prompt-based tool-calling
//! fallback (with probe auto-detect), and model failover routing.

use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use formless_core::config::ProviderCfg;
use formless_core::provider::{
    parse_prompt_tool_calls, probe, responses_input, responses_tools, DeltaKind, Provider,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

fn body_text(body: &Value) -> String {
    body.to_string()
}

async fn mock_server() -> String {
    let app = Router::new()
        .route(
            "/models",
            get(|| async { Json(json!({"data": [{"id": "good"}, {"id": "bad"}, {"id": "resp"}]})) }),
        )
        .route(
            "/chat/completions",
            post(|Json(body): Json<Value>| async move {
                if body["model"].as_str() == Some("bad") {
                    return (
                        axum::http::StatusCode::NOT_FOUND,
                        Json(json!({"error": {"message": "model not found"}})),
                    )
                        .into_response();
                }
                let text = body_text(&body);
                let stream = body["stream"].as_bool().unwrap_or(false);
                let has_tools = body.get("tools").is_some();
                if !stream {
                    if has_tools {
                        // Native probe: this model never emits native calls.
                        return Json(json!({
                            "choices": [{"message": {"content": "I cannot call tools"}}]
                        }))
                        .into_response();
                    }
                    if text.contains("Tool call format") {
                        let name = if text.contains("probe_ping") { "probe_ping" } else { "writer" };
                        return Json(json!({
                            "choices": [{"message": {"content": format!("```json\n{{\"tool_calls\":[{{\"name\":\"{name}\",\"arguments\":{{\"x\":1}}}}]}}\n```")}}]
                        }))
                        .into_response();
                    }
                    return Json(json!({
                        "choices": [{"message": {"content": "complete-ok"}}]
                    }))
                    .into_response();
                }
                // Streaming.
                if !has_tools && text.contains("Tool call format") {
                    let sse = format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"choices": [{"delta": {"content": "{\"tool_calls\": [{\"name\": \"writer\","}}]}),
                        json!({"choices": [{"delta": {"content": " \"arguments\": {\"x\": 2}}]}"}}]})
                    );
                    return axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse))
                        .unwrap()
                        .into_response();
                }
                let sse = format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"choices": [{"delta": {"content": "hello from good"}}]})
                );
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(sse))
                    .unwrap()
                    .into_response()
            }),
        )
        .route(
            "/responses",
            post(|Json(body): Json<Value>| async move {
                let text = body_text(&body);
                let stream = body["stream"].as_bool().unwrap_or(false);
                let has_tools = body.get("tools").is_some();
                if !stream {
                    if has_tools {
                        return Json(json!({
                            "output": [{"type": "function_call", "call_id": "c1", "name": "probe_ping", "arguments": "{}"}]
                        }))
                        .into_response();
                    }
                    return Json(json!({
                        "output": [{"type": "message", "content": [{"type": "output_text", "text": "resp-complete"}]}]
                    }))
                    .into_response();
                }
                let events: Vec<Value> = if has_tools {
                    vec![
                        json!({"type": "response.reasoning_summary_text.delta", "delta": "thinking "}),
                        json!({"type": "response.output_item.added", "output_index": 0, "item": {"type": "function_call", "call_id": "c1", "name": "writer", "arguments": ""}}),
                        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "delta": "{\"x\":"}),
                        json!({"type": "response.function_call_arguments.delta", "output_index": 0, "delta": "1}"}),
                        json!({"type": "response.completed", "response": {"output": []}}),
                    ]
                } else {
                    let _ = text;
                    vec![
                        json!({"type": "response.output_text.delta", "delta": "resp hi"}),
                        json!({"type": "response.completed", "response": {"output": []}}),
                    ]
                };
                let sse: String = events
                    .iter()
                    .map(|e| format!("data: {}\n\n", e))
                    .collect::<String>()
                    + "data: [DONE]\n\n";
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(sse))
                    .unwrap()
                    .into_response()
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn cfg(base: &str) -> ProviderCfg {
    let mut c = ProviderCfg {
        base_url: base.to_string(),
        model: "good".into(),
        max_retries: 1,
        ..Default::default()
    };
    c.fallback_models = vec!["good".into()];
    c
}

fn writer_tool() -> Value {
    json!({"type": "function", "function": {"name": "writer", "description": "writes", "parameters": {"type": "object", "properties": {"x": {"type": "integer"}}}}})
}

#[test]
fn conversions_and_prompt_parsing() {
    let input = responses_input(&[
        json!({"role": "system", "content": "S"}),
        json!({"role": "assistant", "content": Value::Null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "writer", "arguments": "{}"}}]}),
        json!({"role": "tool", "tool_call_id": "c1", "content": "done"}),
    ]);
    assert_eq!(input[1]["type"], "function_call");
    assert_eq!(input[2]["type"], "function_call_output");
    let tools = responses_tools(&[writer_tool()]);
    assert_eq!(tools[0]["name"], "writer");
    assert!(tools[0].get("function").is_none());

    let (clean, calls) = parse_prompt_tool_calls(
        "note {\"tool_calls\": [{\"name\": \"writer\", \"arguments\": {\"x\": 1}}]} tail",
    );
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "writer");
    assert_eq!(calls[0].arguments, "{\"x\":1}");
    assert!(clean.contains("note") && clean.contains("tail"), "{clean}");
    assert!(parse_prompt_tool_calls("plain text").1.is_empty());
}

#[tokio::test]
async fn responses_stream_complete_and_probe() {
    let base = mock_server().await;
    let mut c = cfg(&base);
    c.api_style = "responses".into();
    c.fallback_models.clear();
    let p = Provider::new(c.clone(), "k".into()).unwrap();
    let mut reasoning = String::new();
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "tool please"})],
            &[writer_tool()],
            "resp",
            None,
            &CancellationToken::new(),
            |kind, d| {
                if kind == DeltaKind::Reasoning {
                    reasoning.push_str(d);
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(reasoning, "thinking ");
    assert_eq!(out.tool_calls.len(), 1);
    assert_eq!(out.tool_calls[0].name, "writer");
    assert_eq!(out.tool_calls[0].arguments, "{\"x\":1}");
    assert_eq!(out.model_used, "resp");
    let text = p
        .complete(&[json!({"role": "user", "content": "hi"})], "resp", None)
        .await
        .unwrap();
    assert_eq!(text, "resp-complete");
    let pr = probe(&c, "k", "resp").await.unwrap();
    assert!(pr.tool_calling && !pr.prompt_fallback);
}

#[tokio::test]
async fn prompt_fallback_buffers_json_and_probe_autodetects() {
    let base = mock_server().await;
    let mut c = cfg(&base);
    c.tool_mode = "prompt".into();
    c.fallback_models.clear();
    let p = Provider::new(c.clone(), "k".into()).unwrap();
    let mut visible = String::new();
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "write it"})],
            &[writer_tool()],
            "good",
            None,
            &CancellationToken::new(),
            |kind, d| {
                if kind == DeltaKind::Content {
                    visible.push_str(d);
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(out.tool_calls.len(), 1);
    assert_eq!(out.tool_calls[0].arguments, "{\"x\":2}");
    assert!(
        visible.is_empty(),
        "tool JSON must not stream to the user: {visible}"
    );

    // Native probe fails on this mock; auto-detect falls back to prompt mode.
    let mut native = cfg(&base);
    native.fallback_models.clear();
    let pr = probe(&native, "k", "good").await.unwrap();
    assert!(pr.tool_calling && pr.prompt_fallback, "{pr:?}");
}

#[tokio::test]
async fn model_failover_routes_to_fallback() {
    let base = mock_server().await;
    let c = cfg(&base); // fallback_models = ["good"]
    let p = Provider::new(c, "k".into()).unwrap();
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "hi"})],
            &[],
            "bad",
            None,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(out.content, "hello from good");
    assert_eq!(out.model_used, "good");
}

/// A multibyte character split across network chunks must survive intact
/// (not become two U+FFFD), in both content and tool-call arguments.
#[tokio::test]
async fn sse_keeps_multibyte_chars_split_across_chunks() {
    let line = |v: Value| format!("data: {v}\n\n");
    let body = [
        line(json!({"choices": [{"delta": {"content": "hi 😀 ok"}}]})),
        line(json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "type": "function",
            "function": {"name": "writer", "arguments": "{\"x\":\"日本\"}"}}]}}]})),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat()
    .into_bytes();
    // Cut inside the emoji (2 of its 4 bytes) and inside "日".
    let emoji = body.windows(4).position(|w| w == "😀".as_bytes()).unwrap() + 2;
    let kanji = body.windows(3).position(|w| w == "日".as_bytes()).unwrap() + 1;
    let chunks = vec![
        body[..emoji].to_vec(),
        body[emoji..kanji].to_vec(),
        body[kanji..].to_vec(),
    ];
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let chunks = chunks.clone();
            async move {
                let stream =
                    futures_util::stream::unfold(chunks.into_iter(), |mut it| async move {
                        let next = it.next()?;
                        // Separate writes so the client sees separate chunks.
                        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                        Some((Ok::<_, std::io::Error>(next), it))
                    });
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut c = cfg(&base);
    c.fallback_models.clear();
    let p = Provider::new(c, "k".into()).unwrap();
    let mut streamed = String::new();
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "go"})],
            &[writer_tool()],
            "good",
            None,
            &CancellationToken::new(),
            |_, d| streamed.push_str(d),
        )
        .await
        .unwrap();
    assert_eq!(streamed, "hi 😀 ok");
    assert_eq!(out.content, "hi 😀 ok");
    assert_eq!(out.tool_calls[0].arguments, "{\"x\":\"日本\"}");
}

/// Serves each scripted SSE body in turn (the last one repeats).
async fn scripted_sse(endpoint: &'static str, bodies: Vec<String>) -> String {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new().route(
        endpoint,
        post(move || {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = bodies[n.min(bodies.len() - 1)].clone();
            async move {
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

async fn stream_once(c: ProviderCfg) -> anyhow::Result<String> {
    let p = Provider::new(c, "k".into()).unwrap();
    p.chat_stream(
        &[json!({"role": "user", "content": "hi"})],
        &[],
        "good",
        None,
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .map(|r| r.content)
}

#[tokio::test]
async fn stream_failure_events_fail_loudly_or_retry() {
    let ev = |v: Value| format!("data: {v}\n\n");
    let ok = ev(json!({"type": "response.output_text.delta", "delta": "fine"}))
        + &ev(json!({"type": "response.completed", "response": {}}));
    let responses = |base: &str| {
        let mut c = cfg(base);
        c.api_style = "responses".into();
        c.fallback_models.clear();
        c
    };

    // A non-retryable failure is an error, not an empty answer.
    let failed = ev(json!({"type": "response.failed", "response": {"error":
        {"code": "invalid_prompt", "message": "bad input"}}}));
    let base = scripted_sse("/responses", vec![failed]).await;
    let err = stream_once(responses(&base)).await.unwrap_err().to_string();
    assert!(
        err.contains("invalid_prompt") && err.contains("bad input"),
        "{err}"
    );

    // Incomplete responses surface their reason.
    let incomplete = ev(json!({"type": "response.incomplete", "response":
        {"incomplete_details": {"reason": "max_output_tokens"}}}));
    let base = scripted_sse("/responses", vec![incomplete]).await;
    let err = stream_once(responses(&base)).await.unwrap_err().to_string();
    assert!(err.contains("max_output_tokens"), "{err}");

    // A rate-limit `error` before any output is retried.
    let limited =
        ev(json!({"type": "error", "code": "rate_limit_exceeded", "message": "slow down"}));
    let base = scripted_sse("/responses", vec![limited, ok]).await;
    assert_eq!(stream_once(responses(&base)).await.unwrap(), "fine");

    // Chat Completions: a bare error chunk fails the stream too.
    let chat_err = ev(json!({"error": {"type": "invalid_request_error", "message": "nope"}}));
    let base = scripted_sse("/chat/completions", vec![chat_err]).await;
    let mut c = cfg(&base);
    c.fallback_models.clear();
    let err = stream_once(c).await.unwrap_err().to_string();
    assert!(err.contains("nope"), "{err}");
}

/// A stream that drops mid-response is re-sent instead of failing the Run;
/// the partial output is discarded and a Restart delta tells clients so.
#[tokio::test]
async fn dropped_stream_is_retried_and_partial_output_discarded() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                let line = |t: &str| {
                    format!(
                        "data: {}\n\n",
                        json!({"choices": [{"delta": {"content": t}}]})
                    )
                };
                let chunks: Vec<Result<String, std::io::Error>> = if n == 0 {
                    vec![
                        Ok(line("half an ans")),
                        Err(std::io::Error::other("connection reset")),
                    ]
                } else {
                    vec![Ok(line("the whole answer")), Ok("data: [DONE]\n\n".into())]
                };
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from_stream(futures_util::stream::unfold(
                        chunks.into_iter(),
                        |mut it| async move {
                            let next = it.next()?;
                            // Flush each chunk before the next (or the drop).
                            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                            Some((next, it))
                        },
                    )))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut c = cfg(&base);
    c.fallback_models.clear();
    let p = Provider::new(c, "k".into()).unwrap();
    let mut shown = String::new();
    let mut restarts = 0;
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "go"})],
            &[],
            "good",
            None,
            &CancellationToken::new(),
            |kind, d| match kind {
                DeltaKind::Restart => {
                    restarts += 1;
                    shown.clear();
                }
                _ => shown.push_str(d),
            },
        )
        .await
        .unwrap();
    assert_eq!(restarts, 1);
    assert_eq!(out.content, "the whole answer");
    assert_eq!(shown, "the whole answer");
}

#[tokio::test]
async fn real_token_usage_is_read_from_both_apis() {
    use formless_core::provider::Usage;
    let ev = |v: Value| format!("data: {v}\n\n");
    // Chat Completions: usage arrives in a final chunk with no choices.
    let chat = ev(json!({"choices": [{"delta": {"content": "hi"}}]}))
        + &ev(
            json!({"choices": [], "usage": {"prompt_tokens": 120, "completion_tokens": 7,
            "prompt_tokens_details": {"cached_tokens": 100},
            "completion_tokens_details": {"reasoning_tokens": 3}}}),
        )
        + "data: [DONE]\n\n";
    let base = scripted_sse("/chat/completions", vec![chat]).await;
    let mut c = cfg(&base);
    c.fallback_models.clear();
    let p = Provider::new(c, "k".into()).unwrap();
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "x"})],
            &[],
            "good",
            None,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(
        out.usage,
        Some(Usage {
            prompt_tokens: 120,
            completion_tokens: 7,
            reasoning_tokens: 3,
            cached_tokens: 100,
            ..Default::default()
        })
    );

    // Responses API: usage on response.completed.
    let resp = ev(json!({"type": "response.output_text.delta", "delta": "hi"}))
        + &ev(
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 50,
            "output_tokens": 4, "input_tokens_details": {"cached_tokens": 32}}}}),
        );
    let base = scripted_sse("/responses", vec![resp]).await;
    let mut c = cfg(&base);
    c.api_style = "responses".into();
    c.fallback_models.clear();
    let p = Provider::new(c, "k".into()).unwrap();
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "x"})],
            &[],
            "good",
            None,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(out.usage.unwrap().cached_tokens, 32);
    assert_eq!(out.usage.unwrap().prompt_tokens, 50);
}

/// `output_index` counts reasoning items too: a call at index 1 after a
/// reasoning item at 0 must come back alone, not after an empty phantom
/// call (#540). Nameless calls never leave the provider on either API.
#[tokio::test]
async fn tool_calls_after_reasoning_have_no_phantom_slot() {
    let ev = |v: Value| format!("data: {v}\n\n");
    let resp = ev(
        json!({"type": "response.output_item.added", "output_index": 0,
            "item": {"id": "rs_1", "type": "reasoning", "summary": []}}),
    ) + &ev(
        json!({"type": "response.reasoning_text.delta", "output_index": 0, "delta": "use the tool"}),
    ) + &ev(
        json!({"type": "response.output_item.added", "output_index": 1,
            "item": {"id": "fc_1", "type": "function_call", "call_id": "call_w", "name": "get_weather", "arguments": ""}}),
    ) + &ev(
        json!({"type": "response.function_call_arguments.delta", "output_index": 1, "delta": "{\"city\":"}),
    ) + &ev(
        json!({"type": "response.function_call_arguments.delta", "output_index": 1, "delta": "\"Paris\"}"}),
    ) + &ev(
        json!({"type": "response.output_item.done", "output_index": 1,
            "item": {"id": "fc_1", "type": "function_call", "call_id": "call_w", "name": "get_weather",
            "arguments": "{\"city\":\"Paris\"}"}}),
    ) + &ev(json!({"type": "response.completed", "response": {"output": []}}));
    // Chat Completions with a gap in `index`: slot 0 never gets a name.
    let chat = ev(
        json!({"choices": [{"delta": {"tool_calls": [{"index": 1, "id": "call_c",
            "function": {"name": "get_weather", "arguments": "{}"}}]}}]}),
    ) + "data: [DONE]\n\n";
    for (style, endpoint, body, id, args) in [
        (
            "responses",
            "/responses",
            resp,
            "call_w",
            "{\"city\":\"Paris\"}",
        ),
        ("chat", "/chat/completions", chat, "call_c", "{}"),
    ] {
        let base = scripted_sse(endpoint, vec![body]).await;
        let mut c = cfg(&base);
        c.api_style = style.into();
        c.fallback_models.clear();
        let p = Provider::new(c, "k".into()).unwrap();
        let out = p
            .chat_stream(
                &[json!({"role": "user", "content": "weather?"})],
                &[],
                "good",
                None,
                &CancellationToken::new(),
                |_, _| {},
            )
            .await
            .unwrap();
        assert_eq!(out.tool_calls.len(), 1, "{style}: {:?}", out.tool_calls);
        assert_eq!(out.tool_calls[0].name, "get_weather", "{style}");
        assert_eq!(out.tool_calls[0].id, id, "{style}");
        assert_eq!(out.tool_calls[0].arguments, args, "{style}");
    }
}

/// Endpoints that reject `stream_options` still work: it is dropped.
#[tokio::test]
async fn stream_options_is_dropped_when_rejected() {
    let app = Router::new().route(
        "/chat/completions",
        post(|Json(body): Json<Value>| async move {
            if body.get("stream_options").is_some() {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(json!({"error": {"message": "Unknown parameter: 'stream_options'"}})),
                )
                    .into_response();
            }
            format!(
                "data: {}\n\ndata: [DONE]\n\n",
                json!({"choices": [{"delta": {"content": "fine"}}]})
            )
            .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut c = cfg(&base);
    c.fallback_models.clear();
    let p = Provider::new(c, "k".into()).unwrap();
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "x"})],
            &[],
            "good",
            None,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(out.content, "fine");
    assert_eq!(out.usage, None);
}

#[tokio::test]
async fn cache_probe_reports_cached_tokens_on_the_repeat() {
    let body = |cached: u64| {
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices": [{"delta": {"content": "ok"}}]}),
            json!({"choices": [], "usage": {"prompt_tokens": 1500, "completion_tokens": 1,
                "prompt_tokens_details": {"cached_tokens": cached}}})
        )
    };
    let base = scripted_sse("/chat/completions", vec![body(0), body(1408)]).await;
    let mut c = cfg(&base);
    c.fallback_models.clear();
    let u = formless_core::provider::cache_probe(&c, "k", "good")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((u.prompt_tokens, u.cached_tokens), (1500, 1408));
}

/// prompt_cache_key is sent when set, and dropped if an endpoint rejects it.
#[tokio::test]
async fn prompt_cache_key_is_sent_and_dropped_when_rejected() {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let app = Router::new().route(
        "/chat/completions",
        post({
            let seen = seen.clone();
            move |Json(body): Json<Value>| {
                let reject = seen.lock().unwrap().is_empty();
                seen.lock().unwrap().push(body["prompt_cache_key"].clone());
                async move {
                    if reject {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(json!({"error": {"message": "Unrecognized request argument: prompt_cache_key"}})),
                        )
                            .into_response();
                    }
                    format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        json!({"choices": [{"delta": {"content": "fine"}}]})
                    )
                    .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut c = cfg(&base);
    c.fallback_models.clear();
    let p = Provider::new(c.clone(), "k".into())
        .unwrap()
        .with_cache_key("formless-s1");
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "x"})],
            &[],
            "good",
            None,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(out.content, "fine");
    assert_eq!(
        *seen.lock().unwrap(),
        vec![json!("formless-s1"), Value::Null]
    );

    // cache_key = "off" never sends it.
    c.cache_key = "off".into();
    let p = Provider::new(c, "k".into())
        .unwrap()
        .with_cache_key("formless-s1");
    p.chat_stream(
        &[json!({"role": "user", "content": "x"})],
        &[],
        "good",
        None,
        &CancellationToken::new(),
        |_, _| {},
    )
    .await
    .unwrap();
    assert_eq!(seen.lock().unwrap().last().unwrap(), &Value::Null);
}

/// A 429 is retried after the server's `Retry-After`, and the retry hook
/// hears why, how long and which attempt (the activity-line countdown).
#[tokio::test]
async fn rate_limit_retries_are_reported_to_the_hook() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let app = Router::new().route(
        "/chat/completions",
        post(move || {
            let h = h.clone();
            async move {
                if h.fetch_add(1, Ordering::SeqCst) == 0 {
                    return (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "1")],
                        "slow down",
                    )
                        .into_response();
                }
                format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    json!({"choices": [{"delta": {"content": "ok"}}]})
                )
                .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut c = cfg(&base);
    c.fallback_models.clear();
    c.max_retries = 3;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    let p = Provider::new(c, "k".into())
        .unwrap()
        .with_retry_hook(Arc::new(move |n| s.lock().unwrap().push(n)));
    let out = p
        .chat_stream(
            &[json!({"role": "user", "content": "x"})],
            &[],
            "good",
            None,
            &CancellationToken::new(),
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(out.content, "ok");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].reason, "rate limited");
    assert_eq!(seen[0].wait.as_secs(), 1);
    assert_eq!((seen[0].attempt, seen[0].max_attempts), (1, 3));
}
