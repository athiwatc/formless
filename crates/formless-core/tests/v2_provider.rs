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

    // Cut at max_output_tokens: the partial answer stands, marked
    // truncated (#615), rather than an error.
    let incomplete = ev(json!({"type": "response.output_text.delta", "delta": "par"}))
        + &ev(json!({"type": "response.incomplete", "response":
        {"incomplete_details": {"reason": "max_output_tokens"}}}));
    let base = scripted_sse("/responses", vec![incomplete]).await;
    assert_eq!(stream_once(responses(&base)).await.unwrap(), "par");
    // Any other incomplete reason still fails loudly.
    let other = ev(json!({"type": "response.incomplete", "response":
        {"incomplete_details": {"reason": "something_else"}}}));
    let base = scripted_sse("/responses", vec![other]).await;
    let err = stream_once(responses(&base)).await.unwrap_err().to_string();
    assert!(err.contains("something_else"), "{err}");

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

/// Aliases, Azure-style auth (`api-key` header + `api-version` query),
/// rate-limit headers and the model test (SPEC §4.2–§4.4).
#[tokio::test]
async fn aliases_key_header_query_rate_limits_and_model_test() {
    use axum::extract::Query;
    use axum::http::HeaderMap;
    let app = Router::new().route(
        "/chat/completions",
        post(
            |headers: HeaderMap, Query(q): Query<std::collections::HashMap<String, String>>, Json(body): Json<Value>| async move {
                assert_eq!(headers.get("api-key").and_then(|v| v.to_str().ok()), Some("k"), "key in api-key");
                assert!(headers.get("authorization").is_none(), "no Bearer");
                assert_eq!(q.get("api-version").map(String::as_str), Some("2024-10-21"));
                assert_eq!(body["model"], "real-model", "alias resolved");
                let sse = if body.get("tools").is_some() {
                    // The tool probe is a plain (non-streaming) request.
                    json!({"choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "probe_ping", "arguments": "{}"}}]}, "finish_reason": "tool_calls"}]}).to_string()
                } else {
                    format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"choices": [{"delta": {"content": "o"}}]}),
                        json!({"choices": [{"delta": {"content": "k"}}]})
                    )
                };
                (
                    [("x-ratelimit-remaining-requests", "41"), ("x-ratelimit-limit-requests", "50")],
                    sse,
                )
                    .into_response()
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut c = cfg(&base);
    c.fallback_models.clear();
    c.api_key_header = Some("api-key".into());
    c.query_params
        .insert("api-version".into(), "2024-10-21".into());
    c.aliases.insert("smart".into(), "real-model".into());
    let p = Provider::new(c, "k".into()).unwrap();
    let t = p.test_model("smart").await;
    assert_eq!(t.error, None);
    assert!(
        t.ok && t.streaming && t.tool_calling && !t.prompt_fallback,
        "{t:?}"
    );
    assert!(t.first_token_ms.is_some());
    let limits = formless_core::provider::rate_limits();
    assert_eq!(limits[&base]["remaining-requests"], "41");
}

#[test]
fn model_metadata_comes_from_the_listing_or_the_table() {
    use formless_core::provider::{known_model_meta, parse_model_meta};
    let body = json!({"data": [
        {"id": "vendor/visionary", "context_length": 200000,
         "pricing": {"prompt": "0.000003", "completion": "0.000015"},
         "architecture": {"input_modalities": ["text", "image"]},
         "supported_parameters": ["tools", "reasoning"]},
        {"id": "plain", "supported_parameters": ["temperature"]},
        {"id": "gpt-5-mini"}
    ]});
    let meta = parse_model_meta(&body);
    let v = &meta["vendor/visionary"];
    assert_eq!(
        (
            v["context"].as_u64(),
            v["vision"].as_bool(),
            v["tools"].as_bool()
        ),
        (Some(200000), Some(true), Some(true))
    );
    assert_eq!(
        (v["input_price"].as_f64(), v["output_price"].as_f64()),
        (Some(3.0), Some(15.0))
    );
    assert_eq!(v["efforts"], json!(["low", "medium", "high"]));
    assert_eq!(
        meta["plain"]["efforts"],
        json!([]),
        "no reasoning parameter: effort has no effect"
    );
    assert_eq!(meta["plain"]["tools"], false);
    assert_eq!(
        meta["gpt-5-mini"]["efforts"],
        json!(["minimal", "low", "medium", "high"])
    );
    assert!(
        known_model_meta("some-unknown").get("efforts").is_none(),
        "unknown: all offered"
    );
}

/// `api_style = "anthropic"` (SPEC §4.1): Messages API with x-api-key,
/// streamed thinking + tool_use folded into the usual result, usage with
/// cache reads, and non-streamed completions.
#[tokio::test]
async fn native_anthropic_messages_stream_and_complete() {
    use axum::http::HeaderMap;
    let app = Router::new().route(
        "/messages",
        post(|headers: HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(headers.get("x-api-key").and_then(|v| v.to_str().ok()), Some("k"));
            assert_eq!(headers.get("anthropic-version").and_then(|v| v.to_str().ok()), Some("2023-06-01"));
            assert!(headers.get("authorization").is_none());
            assert_eq!(body["system"][0]["text"], "sys");
            if body["stream"] == false {
                return Json(json!({
                    "content": [{"type": "text", "text": "summary"}],
                    "usage": {"input_tokens": 5, "output_tokens": 2}
                }))
                .into_response();
            }
            assert_eq!(body["thinking"]["type"], "enabled", "effort became a thinking budget");
            assert_eq!(body["tools"][0]["name"], "read");
            let ev = |v: Value| format!("event: {}\ndata: {}\n\n", v["type"].as_str().unwrap(), v);
            let sse = [
                json!({"type": "message_start", "message": {"usage": {"input_tokens": 3, "cache_read_input_tokens": 100}}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "need the file"}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "s1"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "tu_1", "name": "read"}}),
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"a\"}"}}),
                json!({"type": "content_block_stop", "index": 1}),
                json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 12}}),
                json!({"type": "message_stop"}),
            ]
            .into_iter()
            .map(ev)
            .collect::<String>();
            sse.into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut c = cfg(&base);
    c.fallback_models.clear();
    c.api_style = "anthropic".into();
    let p = Provider::new(c, "k".into()).unwrap();
    let msgs = [
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": "read a"}),
    ];
    let tools = [
        json!({"type": "function", "function": {"name": "read", "description": "Read", "parameters": {"type": "object"}}}),
    ];
    let mut thinking = String::new();
    let r = p
        .chat_stream(
            &msgs,
            &tools,
            "claude-x",
            Some("medium"),
            &CancellationToken::new(),
            |k, d| {
                if k == DeltaKind::Reasoning {
                    thinking.push_str(d);
                }
            },
        )
        .await
        .unwrap();
    assert_eq!(thinking, "need the file");
    assert_eq!(r.tool_calls.len(), 1);
    assert_eq!(
        (r.tool_calls[0].id.as_str(), r.tool_calls[0].name.as_str()),
        ("tu_1", "read")
    );
    assert_eq!(r.tool_calls[0].arguments, r#"{"path":"a"}"#);
    assert_eq!(r.thinking_blocks[0]["signature"], "s1");
    let u = r.usage.unwrap();
    assert_eq!(
        (u.cached_tokens, u.completion_tokens, u.prompt_tokens),
        (100, 12, 103)
    );
    assert_eq!(
        p.complete(&msgs, "claude-x", None).await.unwrap(),
        "summary"
    );
}

/// Serves each scripted (status, body) on POST /messages in turn (the
/// last repeats) and records every request body.
async fn scripted_messages(
    replies: Vec<(u16, String)>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let seen2 = seen.clone();
    let app = Router::new().route(
        "/messages",
        post(move |Json(body): Json<Value>| {
            let n = {
                let mut s = seen2.lock().unwrap();
                s.push(body);
                s.len() - 1
            };
            let (status, text) = replies[n.min(replies.len() - 1)].clone();
            async move {
                axum::response::Response::builder()
                    .status(status)
                    .header(
                        "content-type",
                        if status == 200 {
                            "text/event-stream"
                        } else {
                            "application/json"
                        },
                    )
                    .body(axum::body::Body::from(text))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

/// A complete Messages stream answering `text` (and stopping for `stop`).
fn messages_sse(text: &str, stop: &str) -> String {
    [
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ]
    .iter()
    .map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()))
    .collect()
}

fn anthropic_cfg(base: &str) -> ProviderCfg {
    let mut c = cfg(base);
    c.fallback_models.clear();
    c.api_style = "anthropic".into();
    c.max_retries = 2;
    c.max_backoff_secs = 1;
    c
}

async fn anthropic_turn(
    c: ProviderCfg,
    msgs: &[Value],
    tools: &[Value],
    effort: Option<&str>,
) -> (
    anyhow::Result<formless_core::provider::ChatResult>,
    Vec<DeltaKind>,
) {
    let p = Provider::new(c, "k".into()).unwrap();
    let mut kinds = Vec::new();
    let r = p
        .chat_stream(
            msgs,
            tools,
            "claude-x",
            effort,
            &CancellationToken::new(),
            |k, _| {
                if k != DeltaKind::Bytes {
                    kinds.push(k)
                }
            },
        )
        .await;
    (r, kinds)
}

/// #1055, #1064, #1058: a steer stored between a call and its result
/// still sends `tool_result` first; failed results carry `is_error`;
/// unsupported images never reach the API.
#[tokio::test]
async fn anthropic_turn_shape_results_first_errors_flagged_images_checked() {
    let (base, seen) = scripted_messages(vec![(200, messages_sse("ok", "end_turn"))]).await;
    let msgs = [
        json!({"role": "user", "content": [{"type": "text", "text": "look"},
            {"type": "image_url", "image_url": {"url": "data:image/heic;base64,AAAA"}}]}),
        json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "a", "type": "function", "function": {"name": "read", "arguments": "{}"}},
            {"id": "b", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}),
        json!({"role": "user", "content": "actually, stop"}),
        json!({"role": "tool", "tool_call_id": "a", "content": "error: nope", "_is_error": true}),
        json!({"role": "tool", "tool_call_id": "b", "content": "error-free output"}),
    ];
    let (r, _) = anthropic_turn(anthropic_cfg(&base), &msgs, &[], None).await;
    let r = r.unwrap();
    assert!(
        r.notes.iter().any(|n| n.contains("image/heic")),
        "{:?}",
        r.notes
    );
    let body = seen.lock().unwrap()[0].clone();
    let first = body["messages"][0]["content"].as_array().unwrap();
    assert!(
        first.iter().all(|b| b["type"] != "image"),
        "no image/heic block: {first:?}"
    );
    let turn = &body["messages"][2]["content"];
    assert_eq!(turn[0]["tool_use_id"], "a");
    assert_eq!(turn[0]["is_error"], true);
    assert_eq!(turn[1]["tool_use_id"], "b");
    assert!(turn[1].get("is_error").is_none(), "unflagged result");
    assert_eq!(turn[2]["text"], "actually, stop");
}

/// #1057, #1059: a 400 naming thinking, a replayed signature or
/// cache_control is answered by exactly one resend without it.
#[tokio::test]
async fn anthropic_400s_are_stripped_and_resent_once() {
    let err = |m: &str| {
        (
            400,
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": m}})
                .to_string(),
        )
    };
    let ok = (200, messages_sse("fine", "end_turn"));
    let history = [
        json!({"role": "system", "content": "sys"}),
        json!({"role": "user", "content": "go"}),
        json!({"role": "assistant", "content": null,
               "thinking_blocks": [{"type": "thinking", "thinking": "x", "signature": "bad"}],
               "tool_calls": [{"id": "t", "type": "function", "function": {"name": "read", "arguments": "{}"}}]}),
        json!({"role": "tool", "tool_call_id": "t", "content": "1"}),
    ];

    let (base, seen) = scripted_messages(vec![
        err("thinking is not supported by this model"),
        ok.clone(),
    ])
    .await;
    let (r, _) = anthropic_turn(anthropic_cfg(&base), &history[..2], &[], Some("high")).await;
    let r = r.unwrap();
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].get("thinking").is_some() && bodies[1].get("thinking").is_none());
    assert!(r.notes.iter().any(|n| n.contains("extended thinking")));

    let (base, seen) = scripted_messages(vec![
        err("messages.1.content.0: Invalid `signature` in `thinking` block"),
        ok.clone(),
    ])
    .await;
    let (r, _) = anthropic_turn(anthropic_cfg(&base), &history, &[], None).await;
    let r = r.unwrap();
    assert!(r.dropped_replayed_thinking);
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].to_string().contains("\"signature\""));
    assert!(!bodies[1].to_string().contains("\"signature\""));

    let (base, seen) = scripted_messages(vec![
        err("cache_control: unsupported here"),
        err("cache_control: unsupported here"),
    ])
    .await;
    let (r, _) = anthropic_turn(anthropic_cfg(&base), &history[..2], &[], None).await;
    assert!(r.is_err(), "the second rejection is final");
    let bodies = seen.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2, "exactly one resend");
    assert!(bodies[0].to_string().contains("cache_control"));
    assert!(!bodies[1].to_string().contains("cache_control"));
}

/// #1060, #1061: a stream without `message_stop` is a dropped stream
/// (re-sent, clients told to clear), and stop reasons come back.
#[tokio::test]
async fn anthropic_incomplete_streams_retry_and_stop_reasons_surface() {
    let cut: String = messages_sse("partial", "end_turn")
        .split("event: message_delta")
        .next()
        .unwrap()
        .to_string();
    let (base, seen) =
        scripted_messages(vec![(200, cut), (200, messages_sse("whole", "end_turn"))]).await;
    let (r, kinds) = anthropic_turn(
        anthropic_cfg(&base),
        &[json!({"role": "user", "content": "hi"})],
        &[],
        None,
    )
    .await;
    assert_eq!(r.unwrap().content, "whole");
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert!(kinds.contains(&DeltaKind::Restart), "{kinds:?}");

    // A last event without its trailing newline still counts.
    let sse = messages_sse("done", "end_turn");
    let (base, _) = scripted_messages(vec![(200, sse.trim_end().to_string())]).await;
    let (r, _) = anthropic_turn(
        anthropic_cfg(&base),
        &[json!({"role": "user", "content": "hi"})],
        &[],
        None,
    )
    .await;
    assert_eq!(r.unwrap().content, "done");

    for stop in ["refusal", "model_context_window_exceeded", "pause_turn"] {
        let (base, _) = scripted_messages(vec![(200, messages_sse("", stop))]).await;
        let (r, _) = anthropic_turn(
            anthropic_cfg(&base),
            &[json!({"role": "user", "content": "hi"})],
            &[],
            None,
        )
        .await;
        assert_eq!(r.unwrap().stop_reason.as_deref(), Some(stop));
    }
    let (base, _) = scripted_messages(vec![(200, messages_sse("cut", "max_tokens"))]).await;
    let (r, _) = anthropic_turn(
        anthropic_cfg(&base),
        &[json!({"role": "user", "content": "hi"})],
        &[],
        None,
    )
    .await;
    assert!(r.unwrap().truncated);
}

/// #1056: prompt-based tools on the Messages API send a valid Messages
/// body (no native tools, instruction in `system`) and parse the call.
#[tokio::test]
async fn anthropic_prompt_tool_mode_sends_messages_bodies() {
    let call =
        "```json\n{\"tool_calls\": [{\"name\": \"writer\", \"arguments\": {\"x\": 1}}]}\n```";
    let (base, seen) = scripted_messages(vec![(200, messages_sse(call, "end_turn"))]).await;
    let mut c = anthropic_cfg(&base);
    c.tool_mode = "prompt".into();
    let (r, _) = anthropic_turn(
        c,
        &[json!({"role": "user", "content": "write"})],
        &[writer_tool()],
        None,
    )
    .await;
    let r = r.unwrap();
    assert_eq!(r.tool_calls.len(), 1, "{r:?}");
    assert_eq!(r.tool_calls[0].name, "writer");
    let body = seen.lock().unwrap()[0].clone();
    assert!(body.get("tools").is_none());
    assert!(body.get("stream_options").is_none() && body.get("reasoning_effort").is_none());
    assert!(body["system"][0]["text"]
        .as_str()
        .unwrap()
        .contains("writer"));
    assert!(body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["role"] != "system"));
}

/// #1062: the native listing's `max_input_tokens` is the context window,
/// and `has_more` pages are followed.
#[tokio::test]
async fn anthropic_model_listing_pages_and_windows() {
    let app = Router::new().route(
        "/models",
        get(|q: axum::extract::Query<std::collections::HashMap<String, String>>| async move {
            Json(match q.get("after_id").map(String::as_str) {
                None => json!({"data": [{"id": "claude-a", "max_input_tokens": 200000, "max_tokens": 64000}],
                               "has_more": true, "last_id": "claude-a"}),
                Some("claude-a") => json!({"data": [{"id": "claude-b", "max_input_tokens": 1000000}],
                               "has_more": false, "last_id": "claude-b"}),
                Some(other) => panic!("unexpected after_id {other}"),
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let p = Provider::new(anthropic_cfg(&base), "k".into()).unwrap();
    let (models, windows, meta) = p.list_models_meta().await.unwrap();
    assert_eq!(models, ["claude-a", "claude-b"]);
    assert_eq!(
        (windows["claude-a"], windows["claude-b"]),
        (200_000, 1_000_000)
    );
    assert_eq!(meta["claude-a"]["max_output"], 64000);
}

async fn stream_full(base: String, responses: bool) -> formless_core::provider::ChatResult {
    let mut c = cfg(&base);
    c.fallback_models.clear();
    c.max_retries = 0;
    if responses {
        c.api_style = "responses".into();
    }
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
    .unwrap()
}

/// The stream's last event without a trailing newline, a JSON field split
/// over several `data:` lines, and tool calls sent without `index`, as
/// cumulative resends, or in both delta and message form (#614).
#[tokio::test]
async fn stream_edges_lose_and_duplicate_nothing() {
    let last = format!(
        "data: {}\n\ndata: {}",
        json!({"choices": [{"delta": {"content": "hi"}}]}),
        json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 7, "completion_tokens": 2}})
    );
    let r = stream_full(scripted_sse("/chat/completions", vec![last]).await, false).await;
    assert_eq!(r.content, "hi");
    assert_eq!(
        r.usage.map(|u| u.prompt_tokens),
        Some(7),
        "the unterminated last event counts"
    );

    let split =
        "data: {\"choices\": [{\"delta\":\ndata: {\"content\": \"joined\"}}]}\n\ndata: [DONE]\n\n"
            .to_string();
    let r = stream_full(scripted_sse("/chat/completions", vec![split]).await, false).await;
    assert_eq!(r.content, "joined");

    let ev = |v: Value| format!("data: {v}\n\n");
    let calls = [
        // No index: matched by id; then cumulative name/arguments resends.
        ev(json!({"choices": [{"delta": {"tool_calls": [{"id": "a", "function": {"name": "re", "arguments": "{\"x\""}}]}}]})),
        ev(json!({"choices": [{"delta": {"tool_calls": [{"id": "a", "function": {"name": "read", "arguments": "{\"x\":1}"}}]}}]})),
        // The same call again as a whole message: not added twice.
        ev(json!({"choices": [{"message": {"tool_calls": [{"id": "a", "type": "function", "function": {"name": "read", "arguments": "{\"x\":1}"}}]}, "finish_reason": "tool_calls"}]})),
    ]
    .concat();
    let r = stream_full(scripted_sse("/chat/completions", vec![calls]).await, false).await;
    assert_eq!(r.tool_calls.len(), 1, "{:?}", r.tool_calls);
    assert_eq!(
        (
            r.tool_calls[0].name.as_str(),
            r.tool_calls[0].arguments.as_str()
        ),
        ("read", "{\"x\":1}")
    );
}

/// Refusals and filter stops are never silent empty answers, and a
/// Responses answer cut at max_output_tokens is kept as truncated (#615).
#[tokio::test]
async fn refusals_filters_and_responses_truncation_are_told() {
    let ev = |v: Value| format!("data: {v}\n\n");
    let refusal = [
        ev(json!({"choices": [{"delta": {"refusal": "I can't help with that."}}]})),
        ev(json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
    ]
    .concat();
    let r = stream_full(
        scripted_sse("/chat/completions", vec![refusal]).await,
        false,
    )
    .await;
    assert_eq!(r.content, "I can't help with that.");
    assert_eq!(r.stop_reason.as_deref(), Some("refusal"));

    let filtered = ev(json!({"choices": [{"delta": {}, "finish_reason": "content_filter"}]}));
    let r = stream_full(
        scripted_sse("/chat/completions", vec![filtered]).await,
        false,
    )
    .await;
    assert_eq!(r.stop_reason.as_deref(), Some("content_filter"));

    let partial = [
        ev(json!({"type": "response.output_text.delta", "delta": "part"})),
        ev(json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}}})),
    ]
    .concat();
    let r = stream_full(scripted_sse("/responses", vec![partial]).await, true).await;
    assert_eq!(r.content, "part");
    assert!(r.truncated, "kept and marked, not an error");
}

/// A model in an outage is skipped by later requests instead of re-paying
/// its retries every time (#621), and the user is told once.
#[tokio::test]
async fn a_failing_model_trips_its_breaker_and_is_skipped() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let bad_hits = Arc::new(AtomicUsize::new(0));
    let hits = bad_hits.clone();
    let app = Router::new().route(
        "/chat/completions",
        post(move |Json(body): Json<Value>| {
            let hits = hits.clone();
            async move {
                if body["model"] == "bad" {
                    hits.fetch_add(1, Ordering::SeqCst);
                    return (axum::http::StatusCode::SERVICE_UNAVAILABLE, "down").into_response();
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
    c.fallback_models = vec!["good".into()];
    c.max_retries = 3;
    c.max_backoff_secs = 1;
    let notes = Arc::new(Mutex::new(Vec::new()));
    let n2 = notes.clone();
    let p = Provider::new(c, "k".into())
        .unwrap()
        .with_note_hook(Arc::new(move |t| n2.lock().unwrap().push(t)));
    let msgs = [json!({"role": "user", "content": "x"})];
    let cancel = CancellationToken::new();
    let ask = || p.chat_stream(&msgs, &[], "bad", None, &cancel, |_, _| {});
    assert_eq!(ask().await.unwrap().content, "ok");
    assert_eq!(
        bad_hits.load(Ordering::SeqCst),
        3,
        "tripped after 3, not 4 attempts"
    );
    assert_eq!(ask().await.unwrap().content, "ok");
    assert_eq!(bad_hits.load(Ordering::SeqCst), 3, "skipped while open");
    let notes = notes.lock().unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("bad failed 3 times"), "{notes:?}");
}
