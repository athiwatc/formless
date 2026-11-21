//! Private HTTP integration of pinned routing receipts with existing accounting.
use super::*;
use crate::provider::{
    CompletionOptions, DeltaKind, Provider, RequestLease, StructuredOutputError,
};
use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn server(app: Router) -> (String, Server) {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", socket.local_addr().unwrap());
    (
        base,
        Server(tokio::spawn(async move {
            axum::serve(socket, app).await.unwrap()
        })),
    )
}
fn config(base: String) -> ProviderCfg {
    ProviderCfg {
        base_url: base,
        model: "requested".into(),
        max_retries: 0,
        ..Default::default()
    }
}
fn provider(cfg: ProviderCfg) -> Provider {
    Provider::new(cfg, format!("private-dummy-{}", uuid::Uuid::new_v4())).unwrap()
}
fn capture() -> (Arc<Call>, Arc<Mutex<Vec<Record>>>) {
    let records = Arc::new(Mutex::new(Vec::new()));
    let sink = records.clone();
    (Call::new(Arc::new(move |r| sink.locked().push(r))), records)
}
fn private_env() -> crate::credential_env::EnvGuard {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env
}
fn answer(text: &str, cost: f64) -> Value {
    json!({"choices":[{"message":{"content":text}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":cost}})
}

#[tokio::test]
async fn fallback_success_keeps_all_physical_costs_and_only_actual_serving_identity() {
    let _env = private_env();
    let (base,_server)=server(Router::new().route("/chat/completions",post(|Json(body):Json<Value>|async move{
        if body["model"]=="requested"{(StatusCode::NOT_FOUND,Json(json!({"error":{"code":"model_not_found","message":"model not found"},"usage":{"prompt_tokens":2,"completion_tokens":1,"cost":0.004}}))).into_response()}
        else{assert_eq!(body["model"],"served");Json(answer("ok",0.006)).into_response()}
    }))).await;
    let mut cfg = config(base);
    cfg.fallback_models = vec!["served".into()];
    let billed = Arc::new(Mutex::new(Vec::new()));
    let charged = billed.clone();
    let p = provider(cfg.clone()).with_usage_hook(Arc::new(move |_, model, usage| {
        charged.locked().push((model.to_string(), *usage));
        Ok(())
    }));
    let (call, records) = capture();
    let result = scope(call.clone(), p.complete_result(&[], "requested", None))
        .await
        .unwrap();
    assert_eq!(result.model_used, "served");
    call.finish(Outcome::Success(Identity::new(
        "default",
        &cfg,
        &result.model_used,
    )));
    drop(call);
    let records = records.locked();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        record.outcome,
        Outcome::Success(Identity::new("default", &cfg, "served"))
    );
    assert_eq!(record.attempts.len(), 2);
    let rejected = record
        .attempts
        .iter()
        .find(|a| a.identity.model == "requested")
        .unwrap();
    assert_eq!(rejected.neutral, Some(Neutral::Unavailable));
    assert_eq!(rejected.status, Some(404));
    assert!(record
        .attempts
        .iter()
        .all(|a| a.sent && a.response_complete));
    let total = record
        .attempts
        .iter()
        .map(|a| a.usage.unwrap().cost.unwrap())
        .sum::<f64>();
    assert!((total - 0.01).abs() < 1e-12);
    let billed = billed.locked();
    assert_eq!(
        billed.len(),
        2,
        "routing must never become a second billing hook"
    );
    assert_eq!(billed[0].0, "requested");
    assert_eq!(billed[1].0, "served");
}

#[tokio::test]
async fn auth_rate_context_availability_and_server_errors_are_neutral_attempts() {
    let _env = private_env();
    for (status, message, expected) in [
        (400, "context_length_exceeded", Neutral::Context),
        (401, "invalid_api_key", Neutral::Auth),
        (403, "permission denied", Neutral::Auth),
        (404, "model not found", Neutral::Unavailable),
        (429, "rate limit", Neutral::RateLimit),
        (503, "server unavailable", Neutral::Server),
    ] {
        let (base,_server)=server(Router::new().route("/chat/completions",post(move||async move{(StatusCode::from_u16(status).unwrap(),Json(json!({"error":{"message":message},"usage":{"prompt_tokens":1,"completion_tokens":0,"cost":0.001}})))}))).await;
        let p = provider(config(base));
        let (call, records) = capture();
        assert!(
            scope(call.clone(), p.complete_result(&[], "requested", None))
                .await
                .is_err()
        );
        call.finish(Outcome::Neutral(expected));
        drop(call);
        let records = records.locked();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, Outcome::Neutral(expected));
        assert_eq!(records[0].attempts.len(), 1);
        assert_eq!(records[0].attempts[0].neutral, Some(expected));
        assert_eq!(records[0].attempts[0].usage.unwrap().cost, Some(0.001));
    }
}

#[tokio::test]
async fn cancellation_records_headers_body_and_partial_stream_before_admission_release() {
    let _env = private_env();
    for phase in ["headers", "body", "stream"] {
        let ready = Arc::new(tokio::sync::Notify::new());
        let server_ready = ready.clone();
        let (base,_server)=server(Router::new().route("/chat/completions",post(move||{let ready=server_ready.clone();async move{
            if phase=="headers" {ready.notify_one();return std::future::pending::<axum::response::Response>().await;}
            let first=if phase=="stream"{format!("data: {}\n\n",json!({"choices":[{"delta":{"content":"partial"}}],"usage":{"prompt_tokens":3}})).into_bytes()}else{b"{\"choices\":".to_vec()};
            let chunks=futures_util::stream::once(async move{Ok::<_,std::io::Error>(first)}).chain(futures_util::stream::pending());
            ([("content-type",if phase=="stream"{"text/event-stream"}else{"application/json"})],axum::body::Body::from_stream(chunks)).into_response()
        }}))).await;
        let (call, records) = capture();
        let active = Arc::new(AtomicUsize::new(0));
        let checked = Arc::new(AtomicUsize::new(0));
        let billed = Arc::new(AtomicUsize::new(0));
        struct Lease {
            call: std::sync::Weak<Call>,
            active: Arc<AtomicUsize>,
            checked: Arc<AtomicUsize>,
            billed: Arc<AtomicUsize>,
            phase: &'static str,
        }
        impl Drop for Lease {
            fn drop(&mut self) {
                let call = self.call.upgrade().unwrap();
                let state = call.state.locked();
                assert_eq!(
                    state.attempts.len(),
                    1,
                    "snapshot must settle BEFORE admission release"
                );
                let a = &state.attempts[0];
                assert!(a.sent && !a.response_complete);
                assert_eq!(
                    a.status,
                    if self.phase == "headers" {
                        None
                    } else {
                        Some(200)
                    }
                );
                assert_eq!(
                    a.usage.map(|u| u.prompt_tokens),
                    if self.phase == "stream" {
                        Some(3)
                    } else {
                        None
                    }
                );
                assert_eq!(
                    self.billed.load(Ordering::SeqCst),
                    usize::from(self.phase == "stream")
                );
                self.active.fetch_sub(1, Ordering::SeqCst);
                self.checked.fetch_add(1, Ordering::SeqCst);
            }
        }
        let owner = Arc::downgrade(&call);
        let start = active.clone();
        let inspect = checked.clone();
        let usage_count = billed.clone();
        let mut p = provider(config(base)).with_request_hook(Arc::new(move || {
            let owner = owner.clone();
            let active = start.clone();
            let checked = inspect.clone();
            let billed = usage_count.clone();
            Box::pin(async move {
                active.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(Lease {
                    call: owner,
                    active,
                    checked,
                    billed,
                    phase,
                }) as RequestLease)
            })
        }));
        let charged = billed.clone();
        let held = active.clone();
        p = p.with_usage_hook(Arc::new(move |_, _, _| {
            assert_eq!(held.load(Ordering::SeqCst), 1);
            charged.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        if phase == "body" {
            let ready = ready.clone();
            p = p.with_input_hook(Arc::new(move |_| ready.notify_one()));
        }
        let producer = call.clone();
        let client_ready = ready.clone();
        let work = tokio::spawn(scope(producer, async move {
            if phase == "stream" {
                p.chat_stream(
                    &[],
                    &[],
                    "requested",
                    None,
                    &tokio_util::sync::CancellationToken::new(),
                    move |kind, _| {
                        if kind == DeltaKind::Content {
                            client_ready.notify_one()
                        }
                    },
                )
                .await
                .map(|_| ())
            } else {
                p.complete_result(&[], "requested", None).await.map(|_| ())
            }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), ready.notified())
            .await
            .unwrap();
        work.abort();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), work)
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(checked.load(Ordering::SeqCst), 1);
        drop(call);
        let records = records.locked();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, Outcome::Neutral(Neutral::Cancelled));
    }
}

#[tokio::test]
async fn compiled_output_failure_claims_actual_fallback_only_after_complete_response_and_billing() {
    let _env = private_env();
    for text in ["not json", "{}"] {
        let (base, _server) = server(Router::new().route(
            "/chat/completions",
            post(move |Json(body): Json<Value>| async move {
                if body["model"] == "requested" {
                    (
                        StatusCode::NOT_FOUND,
                        Json(json!({"error":{"message":"model not found"}})),
                    )
                        .into_response()
                } else {
                    Json(answer(text, 0.001)).into_response()
                }
            }),
        ))
        .await;
        let mut cfg = config(base);
        cfg.fallback_models = vec!["served".into()];
        let billed = Arc::new(AtomicUsize::new(0));
        let charged = billed.clone();
        let p = provider(cfg.clone()).with_usage_hook(Arc::new(move |_, _, _| {
            charged.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        let options=CompletionOptions::json_schema(json!({"type":"object","properties":{"ok":{"type":"boolean"}},"required":["ok"],"additionalProperties":false})).unwrap();
        let (call, records) = capture();
        let error = scope(
            call.clone(),
            p.complete_result_with_options(&[], "requested", None, &options),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<StructuredOutputError>(),
                Some(
                    StructuredOutputError::InvalidJson { .. }
                        | StructuredOutputError::SchemaViolation { .. }
                )
            ),
            "{error:#}"
        );
        let actual = call
            .contract_identity()
            .expect("typed complete response must retain its actual serving proof");
        assert_eq!(actual.model, "served");
        assert_eq!(billed.load(Ordering::SeqCst), 1);
        call.finish(Outcome::ContractFailure(actual));
        drop(call);
        let records = records.locked();
        assert!(matches!(&records[0].outcome,Outcome::ContractFailure(id)if id.model=="served"));
    }
}

#[tokio::test]
async fn direct_compact_resources_are_captured_without_relabeling_responsible_role_model() {
    use crate::{
        routing_recorder::{Receipt, Recorder},
        routing_task_observation::{self, Event, Task, TaskOutcome},
    };
    let _env = private_env();
    let (base,_server)=server(Router::new().route("/chat/completions",post(||async{Json(answer("ok",0.001))})).route("/compact",post(|Json(body):Json<Value>|async move{assert_eq!(body["model"],"compact-helper");Json(json!({"summary":"short","usage":{"prompt_tokens":4,"completion_tokens":2,"cost":0.002}}))}))).await;
    let cfg = config(base);
    let billed = Arc::new(AtomicUsize::new(0));
    let charged = billed.clone();
    let p = provider(cfg.clone()).with_usage_hook(Arc::new(move |_, _, _| {
        charged.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    let recorder = Arc::new(Recorder::default());
    let role = Task::with_recorder("role", None, recorder.clone()).unwrap();
    routing_task_observation::scope(Some(role.clone()), async {
        let step = Call::responsible_step(recorder.call_sink());
        let result = scope(step.clone(), p.complete_result(&[], "requested", None))
            .await
            .unwrap();
        step.finish(Outcome::Success(Identity::new(
            "default",
            &cfg,
            &result.model_used,
        )));
        drop(step);
        assert_eq!(
            p.compact_server("private fixture", "compact-helper")
                .await
                .unwrap(),
            "short"
        );
        role.finish(TaskOutcome::Completed);
    })
    .await;
    drop(role);
    let batch = recorder.take().await;
    assert_eq!(billed.load(Ordering::SeqCst), 2);
    assert_eq!(batch.receipts.len(), 4);
    let compact = batch
        .receipts
        .iter()
        .find_map(|r| match r {
            Receipt::Call(r) if r.resource_only => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(compact.outcome, Outcome::Neutral(Neutral::Unknown));
    assert_eq!(compact.attempts[0].identity.model, "compact-helper");
    assert_eq!(compact.attempts[0].usage.unwrap().cost, Some(0.002));
    assert!(compact.attempts[0].response_complete);
    assert!(batch.receipts.iter().any(|r|matches!(r,Receipt::Task(Event::Final(task))if task.expected_calls==2&&task.responsible.as_ref().unwrap().model=="requested"&&task.capture_complete)));
}

#[tokio::test]
async fn clean_stream_eof_without_protocol_terminal_keeps_interim_cost_incomplete() {
    let _env = private_env();
    for style in ["chat", "responses"] {
        for terminal in [false, true] {
            let endpoint = if style == "responses" {
                "/responses"
            } else {
                "/chat/completions"
            };
            let (base,_server)=server(Router::new().route(endpoint,post(move||async move{
                let event=if style=="responses"{json!({"type":"response.output_text.delta","delta":"ok","usage":{"input_tokens":3,"output_tokens":2,"cost":0.001}})}else{json!({"choices":[{"delta":{"content":"ok"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":0.001}})};
                let mut text=format!("data: {event}\n\n");
                if terminal{if style=="responses"{text+=&format!("data: {}\n\n",json!({"type":"response.completed","response":{"usage":{"input_tokens":3,"output_tokens":2,"cost":0.001}}}));}else{text+="data: [DONE]\n\n";}}
                ([("content-type","text/event-stream")],text)
            }))).await;
            let mut cfg = config(base);
            cfg.api_style = style.into();
            let p = provider(cfg.clone());
            let (call, records) = capture();
            let result = scope(
                call.clone(),
                p.chat_stream(
                    &[],
                    &[],
                    "requested",
                    None,
                    &tokio_util::sync::CancellationToken::new(),
                    |_, _| {},
                ),
            )
            .await
            .unwrap();
            assert_eq!(
                result.content, "ok",
                "legacy stream-return behavior remains compatible"
            );
            call.finish(Outcome::Success(Identity::new(
                "default",
                &cfg,
                &result.model_used,
            )));
            drop(call);
            let records = records.locked();
            let record = &records[0];
            assert_eq!(record.attempts.len(), 1);
            assert_eq!(record.attempts[0].response_complete, terminal);
            assert_eq!(record.attempts[0].usage.unwrap().cost, Some(0.001));
            assert_eq!(
                matches!(record.outcome, Outcome::Success(_)),
                terminal,
                "interim reported USD is not a complete USD-per-success basis"
            );
        }
    }
}

#[tokio::test]
async fn chat_finish_evidence_uses_final_wire_multiplicity_and_every_observed_choice() {
    let _env = private_env();
    // Final-body overrides are physically asserted, including invalid values;
    // no allocation or guessed choice count follows an untrusted n.
    for (overrides, extra_event, finish, expected) in [
        (json!({}), json!(null), "stop", true),
        (json!({"n":1}), json!(null), "tool_calls", true),
        (json!({"n":2}), json!(null), "stop", false),
        (json!({"n":null}), json!(null), "stop", false),
        (json!({"n":"1"}), json!(null), "stop", false),
        (json!({"n":-1}), json!(null), "stop", false),
        (json!({"options":{"n":2}}), json!(null), "stop", false),
        (json!({"best_of":2}), json!(null), "stop", false),
        (
            json!({}),
            json!({"choices":[{"index":1,"delta":{"content":"other"}}]}),
            "stop",
            false,
        ),
        (
            json!({}),
            json!({"choices":[{"index":"0","delta":{}}]}),
            "stop",
            false,
        ),
        (
            json!({}),
            json!({"choices":[{"index":0,"delta":{}},{"index":1,"delta":{}}]}),
            "stop",
            false,
        ),
        (json!({}), json!(null), "unknown_vendor_finish", false),
    ] {
        for done in [false, true] {
            let captured = overrides.clone();
            let extra = extra_event.clone();
            let (base,_server)=server(Router::new().route("/chat/completions",post(move|Json(body):Json<Value>|{
                let captured=captured.clone();let extra=extra.clone();async move {
                for (key,value) in captured.as_object().unwrap(){assert_eq!(&body[key],value,"override must reach actual accepted body");}
                let mut text=format!("data: {}\n\n",json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":finish}]}));
                if !extra.is_null(){text+=&format!("data: {extra}\n\n");}
                text+=&format!("data: {}\n\n",json!({"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":2,"cost":0.001}}));
                if done{text+="data: [DONE]\n\n";}
                ([("content-type","text/event-stream")],text)
            }}))).await;
            let mut cfg = config(base);
            cfg.extra_body = serde_json::from_value(overrides.clone()).unwrap();
            let p = provider(cfg.clone());
            let (call, records) = capture();
            let result = scope(
                call.clone(),
                p.chat_stream(
                    &[],
                    &[],
                    "requested",
                    None,
                    &tokio_util::sync::CancellationToken::new(),
                    |_, _| {},
                ),
            )
            .await
            .unwrap();
            assert_eq!(
                result.content,
                format!(
                    "ok{}",
                    extra_event["choices"][0]["delta"]["content"]
                        .as_str()
                        .unwrap_or_default()
                ),
                "observer preserves the legacy first-choice renderer"
            );
            call.finish(Outcome::Success(Identity::new(
                "default",
                &cfg,
                &result.model_used,
            )));
            drop(call);
            let rows = records.locked();
            let record = &rows[0];
            assert_eq!(
                record.attempts[0].response_complete,
                done || expected,
                "overrides={overrides}, extra={extra_event}, finish={finish}, done={done}"
            );
            assert_eq!(
                matches!(record.outcome, Outcome::Success(_)),
                done || expected
            );
        }
    }
}
