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
