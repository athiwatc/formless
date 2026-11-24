use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, HeaderValue, StatusCode},
    response::Response,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    env,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[derive(Clone, Default)]
struct Summary {
    steps: u64,
    tool_calls: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
}

pub struct ScriptedProvider {
    base_url: String,
    calls: mpsc::UnboundedReceiver<ProviderCall>,
    summary: Arc<Mutex<Summary>>,
    server: tokio::task::JoinHandle<()>,
}

pub struct ProviderCall {
    pub body: Value,
    request_id: usize,
    reply: oneshot::Sender<ScriptedReply>,
}

#[derive(Debug)]
enum ScriptedReply {
    Completion(Value),
    Truncated(Value),
    Dropped {
        delta: Value,
        release: oneshot::Receiver<()>,
    },
    RateLimited,
}

impl ScriptedProvider {
    pub async fn start() -> Self {
        let (tx, calls) = mpsc::unbounded_channel();
        let summary = Arc::new(Mutex::new(Summary::default()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/chat/completions",
            post({
                let summary = summary.clone();
                let request_count = request_count.clone();
                move |Json(body): Json<Value>| {
                    let tx = tx.clone();
                    let summary = summary.clone();
                    let request_id = request_count.fetch_add(1, Ordering::SeqCst) + 1;
                    async move {
                        let (reply, response) = oneshot::channel();
                        tx.send(ProviderCall {
                            body: body.clone(),
                            request_id,
                            reply,
                        })
                        .expect("scripted provider receiver stopped");
                        let reply = response.await.expect("scenario did not script a reply");
                        match reply {
                            ScriptedReply::Completion(delta) => {
                                let prompt_tokens = estimate_tokens(&body);
                                let completion_tokens = estimate_tokens(&delta);
                                let tool_calls =
                                    delta["tool_calls"].as_array().map_or(0, Vec::len) as u64;
                                let mut stats = summary.lock().unwrap();
                                stats.steps += 1;
                                stats.tool_calls += tool_calls;
                                stats.prompt_tokens += prompt_tokens;
                                stats.completion_tokens += completion_tokens;
                                drop(stats);
                                let usage = json!({
                                    "prompt_tokens": prompt_tokens,
                                    "completion_tokens": completion_tokens
                                });
                                let finish_reason =
                                    if tool_calls > 0 { "tool_calls" } else { "stop" };
                                if body["stream"].as_bool() == Some(true) {
                                    stream_response(delta, finish_reason, usage)
                                } else {
                                    completion_response(delta, finish_reason, usage)
                                }
                            }
                            ScriptedReply::Truncated(delta) => {
                                let prompt_tokens = estimate_tokens(&body);
                                let completion_tokens = estimate_tokens(&delta);
                                let mut stats = summary.lock().unwrap();
                                stats.steps += 1;
                                stats.tool_calls +=
                                    delta["tool_calls"].as_array().map_or(0, Vec::len) as u64;
                                stats.prompt_tokens += prompt_tokens;
                                stats.completion_tokens += completion_tokens;
                                drop(stats);
                                let usage = json!({
                                    "prompt_tokens": prompt_tokens,
                                    "completion_tokens": completion_tokens
                                });
                                if body["stream"].as_bool() == Some(true) {
                                    stream_response(delta, "length", usage)
                                } else {
                                    completion_response(delta, "length", usage)
                                }
                            }
                            ScriptedReply::Dropped { delta, release } => {
                                dropped_response(delta, release)
                            }
                            ScriptedReply::RateLimited => Response::builder()
                                .status(StatusCode::TOO_MANY_REQUESTS)
                                .header(CONTENT_TYPE, "application/json")
                                .header("retry-after", HeaderValue::from_static("0"))
                                .body(Body::from(
                                    json!({"error":{"message":"scripted rate limit"}}).to_string(),
                                ))
                                .unwrap(),
                        }
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base_url,
            calls,
            summary,
            server,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn next_call(&mut self) -> ProviderCall {
        tokio::time::timeout(Duration::from_secs(10), self.calls.recv())
            .await
            .expect("provider request did not reach the script")
            .expect("scripted provider stopped")
    }

    pub fn summary(&self) -> Value {
        let stats = self.summary.lock().unwrap().clone();
        json!({
            "steps": stats.steps,
            "tool_calls": stats.tool_calls,
            "tokens": {
                "prompt": stats.prompt_tokens,
                "completion": stats.completion_tokens
            }
        })
    }
}

impl ProviderCall {
    pub fn text(self, content: &str) {
        self.reply
            .send(ScriptedReply::Completion(json!({"content":content})))
            .expect("provider request cancelled before its scripted response");
    }

    pub fn tool_call(self, name: &str, arguments: Value) {
        let delta = json!({"tool_calls":[{
            "index":0,
            "id":format!("call-{}", self.request_id),
            "type":"function",
            "function":{"name":name,"arguments":arguments.to_string()}
        }]});
        self.reply
            .send(ScriptedReply::Completion(delta))
            .expect("provider request cancelled before its scripted response");
    }

    pub fn malformed_tool_call(self, name: &str, arguments: &str) {
        let delta = json!({"tool_calls":[{
            "index":0,
            "id":format!("call-{}", self.request_id),
            "type":"function",
            "function":{"name":name,"arguments":arguments}
        }]});
        self.reply
            .send(ScriptedReply::Completion(delta))
            .expect("provider request cancelled before its scripted response");
    }

    pub fn truncated_tool_call(self, name: &str, arguments: &str) {
        let delta = json!({"tool_calls":[{
            "index":0,
            "id":format!("call-{}", self.request_id),
            "type":"function",
            "function":{"name":name,"arguments":arguments}
        }]});
        self.reply
            .send(ScriptedReply::Truncated(delta))
            .expect("provider request cancelled before its scripted response");
    }

    pub fn drop_after(self, delta: Value) -> oneshot::Sender<()> {
        let (release, wait_for_release) = oneshot::channel();
        self.reply
            .send(ScriptedReply::Dropped {
                delta,
                release: wait_for_release,
            })
            .expect("provider request cancelled before its scripted response");
        release
    }

    pub fn too_many_requests(self) {
        self.reply
            .send(ScriptedReply::RateLimited)
            .expect("provider request cancelled before its scripted response");
    }
}

impl Drop for ScriptedProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn stream_response(delta: Value, finish_reason: &str, usage: Value) -> Response<Body> {
    let event = json!({
        "choices":[{"delta":delta,"finish_reason":finish_reason}],
        "usage":usage
    });
    Response::builder()
        .header(CONTENT_TYPE, "text/event-stream")
        .body(Body::from(format!("data: {event}\n\ndata: [DONE]\n\n")))
        .unwrap()
}

fn completion_response(delta: Value, finish_reason: &str, usage: Value) -> Response<Body> {
    Response::builder()
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"choices":[{"message":delta,"finish_reason":finish_reason}],"usage":usage})
                .to_string(),
        ))
        .unwrap()
}

fn dropped_response(delta: Value, release: oneshot::Receiver<()>) -> Response<Body> {
    let event = json!({"choices":[{"delta":delta}]});
    let line = format!("data: {event}\n\n");
    Response::builder()
        .header(CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(futures_util::stream::unfold(
            (0, Some(line), Some(release)),
            |(step, line, release)| async move {
                match step {
                    0 => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        Some((
                            Ok::<_, std::io::Error>(line.unwrap()),
                            (1, None, release),
                        ))
                    }
                    1 => {
                        let _ = release.unwrap().await;
                        Some((
                            Err(std::io::Error::other("scripted stream dropped")),
                            (2, None, None),
                        ))
                    }
                    _ => None,
                }
            },
        )))
        .unwrap()
}

fn estimate_tokens(value: &Value) -> u64 {
    let mut normalized = value.clone();
    let machine_paths = machine_path_roots();
    normalize_dynamic_value(&mut normalized, &machine_paths);
    let bytes = normalized.to_string().len() as u64;
    bytes.div_ceil(4)
}

fn machine_path_roots() -> Vec<String> {
    let mut paths = vec![env::temp_dir().to_string_lossy().into_owned()];
    if let Ok(path) = env::current_dir() {
        paths.push(path.to_string_lossy().into_owned());
    }
    for key in ["HOME", "USERPROFILE", "TMPDIR", "TEMP"] {
        if let Some(path) = env::var_os(key) {
            paths.push(path.to_string_lossy().into_owned());
        }
    }
    paths.retain(|path| !path.is_empty());
    paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
    paths.dedup();
    paths
}

fn normalize_dynamic_value(value: &mut Value, machine_paths: &[String]) {
    match value {
        Value::Object(fields) => {
            fields.retain(|key, _| {
                !(key == "id"
                    || key.ends_with("_id")
                    || key == "time"
                    || key.ends_with("_time")
                    || key.ends_with("_at")
                    || key == "timestamp"
                    || key.ends_with("_timestamp"))
            });
            for field in fields.values_mut() {
                normalize_dynamic_value(field, machine_paths);
            }
        }
        Value::Array(items) => {
            for item in items {
                normalize_dynamic_value(item, machine_paths);
            }
        }
        Value::String(text) => {
            for path in machine_paths {
                *text = text.replace(path, "<machine-path>");
            }
            *text = normalize_uuids(text);
            *text = normalize_dates_and_times(text);
        }
        _ => {}
    }
}

fn normalize_uuids(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut normalized = String::new();
    let mut segment_start = 0;
    let mut cursor = 0;
    while cursor + 36 <= bytes.len() {
        if let Ok(candidate) = std::str::from_utf8(&bytes[cursor..cursor + 36]) {
            if Uuid::parse_str(candidate).is_ok() {
                normalized.push_str(&text[segment_start..cursor]);
                normalized.push_str("<uuid>");
                cursor += 36;
                segment_start = cursor;
                continue;
            }
        }
        cursor += 1;
    }
    normalized.push_str(&text[segment_start..]);
    normalized
}

fn normalize_dates_and_times(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut normalized = String::new();
    let mut segment_start = 0;
    let mut cursor = 0;
    while cursor + 10 <= bytes.len() {
        let Some(end) = date_or_timestamp_end(bytes, cursor) else {
            cursor += 1;
            continue;
        };
        normalized.push_str(&text[segment_start..cursor]);
        normalized.push_str(if end - cursor == 10 {
            "<date>"
        } else {
            "<timestamp>"
        });
        cursor = end;
        segment_start = end;
    }
    normalized.push_str(&text[segment_start..]);
    normalized
}

fn date_or_timestamp_end(bytes: &[u8], start: usize) -> Option<usize> {
    let date = bytes.get(start..start + 10)?;
    if !date[0..4].iter().all(u8::is_ascii_digit)
        || date[4] != b'-'
        || !date[5..7].iter().all(u8::is_ascii_digit)
        || date[7] != b'-'
        || !date[8..10].iter().all(u8::is_ascii_digit)
    {
        return None;
    }

    let Some(time) = bytes.get(start + 10..start + 19) else {
        return Some(start + 10);
    };
    if !matches!(time[0], b'T' | b't' | b' ')
        || !time[1..3].iter().all(u8::is_ascii_digit)
        || time[3] != b':'
        || !time[4..6].iter().all(u8::is_ascii_digit)
        || time[6] != b':'
        || !time[7..9].iter().all(u8::is_ascii_digit)
    {
        return Some(start + 10);
    }

    let mut end = start + 19;
    if bytes.get(end) == Some(&b'.') {
        let fraction_start = end + 1;
        end = fraction_start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == fraction_start {
            end = start + 19;
        }
    }
    if matches!(bytes.get(end), Some(b'Z' | b'z')) {
        return Some(end + 1);
    }
    if matches!(bytes.get(end), Some(b'+' | b'-')) {
        let offset = bytes.get(end + 1..end + 6)?;
        if offset[0..2].iter().all(u8::is_ascii_digit)
            && offset[2] == b':'
            && offset[3..5].iter().all(u8::is_ascii_digit)
        {
            end += 6;
        }
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimates_ignore_machine_paths_times_and_random_identifiers() {
        let root = env::current_dir().unwrap();
        let temp = env::temp_dir();
        let first = json!({
            "session_id":"short-id",
            "timestamp":"2026-02-03T04:05:06Z",
            "messages":[{"role":"user","content":format!(
                "{} and 2026-02-03T04:05:06Z with 550e8400-e29b-41d4-a716-446655440000",
                root.join("same/fixture").display()
            )}]
        });
        let second = json!({
            "session_id":"a-random-identifier-of-a-different-length",
            "timestamp":"2031-11-21T19:43:17.123+07:30",
            "messages":[{"role":"user","content":format!(
                "{} and 2031-11-21T19:43:17.123+07:30 with 7e6f7c74-5a8a-4c4c-9f70-17ccf4e651d8",
                temp.join("same/fixture").display()
            )}]
        });
        assert_eq!(estimate_tokens(&first), estimate_tokens(&second));
    }
}
