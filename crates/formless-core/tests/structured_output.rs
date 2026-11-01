//! Private peers exercise actual wire requests, not just body builders.
#[path = "support/env.rs"]
mod credential_env;
use axum::{response::IntoResponse, routing::post, Json, Router};
use formless_core::{
    config::ProviderCfg,
    provider::{CompletionOptions, Provider, StructuredOutputError},
};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct Peer {
    base: String,
    seen: Arc<Mutex<Vec<Value>>>,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
impl Peer {
    async fn new(
        handler: impl Fn(usize, &Value) -> axum::response::Response + Send + Sync + 'static,
    ) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capture = seen.clone();
        let handler = Arc::new(handler);
        let app = Router::new().route(
            "/{*path}",
            post(move |Json(body): Json<Value>| {
                let mut seen = capture.lock().unwrap();
                let index = seen.len();
                seen.push(body.clone());
                std::future::ready(handler(index, &body))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        Self {
            base,
            seen,
            stop,
            task,
        }
    }
    fn provider(&self, style: &str) -> Provider {
        Provider::new(
            ProviderCfg {
                base_url: self.base.clone(),
                api_style: style.into(),
                model: format!("structured-{}", uuid::Uuid::new_v4()),
                max_retries: 0,
                fallback_models: vec![],
                ..Default::default()
            },
            "private-structured-key".into(),
        )
        .unwrap()
    }
    async fn close(self) {
        self.stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}
fn schema() -> Value {
    json!({"type":"object","properties":{"answer":{"type":"integer"}},"required":["answer"],"additionalProperties":false,
        "default":{"$ref":"file:///inert-schema-data","type":"input_audio"},"examples":[{"answer":7,"opaque":"unchanged"}]})
}
fn format<'a>(style: &str, body: &'a Value) -> &'a Value {
    if style == "responses" {
        &body["text"]["format"]
    } else {
        &body["response_format"]
    }
}
fn response(style: &str, stream: bool, text: &str, terminal: &str) -> axum::response::Response {
    let usage = if style == "chat" {
        json!({"prompt_tokens":11,"completion_tokens":3,"cost":0.25})
    } else {
        json!({"input_tokens":11,"output_tokens":3,"cost":0.25})
    };
    if stream {
        let events = match style {
            "responses" if terminal == "refusal" => vec![
                json!({"type":"response.refusal.delta","delta":"refused"}),
                json!({"type":"response.completed","response":{"status":"completed","usage":usage}}),
            ],
            "responses" => vec![
                json!({"type":"response.output_text.delta","delta":text}),
                json!({"type":if terminal=="length" {"response.incomplete"} else {"response.completed"},"response":{"status":if terminal=="length" {"incomplete"} else {"completed"},"incomplete_details":{"reason":"max_output_tokens"},"usage":usage}}),
            ],
            "anthropic" => vec![
                json!({"type":"message_start","message":{"usage":{"input_tokens":11}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":match terminal {"length"=>"max_tokens","refusal"=>"refusal",_=>"end_turn"}},"usage":{"output_tokens":3,"cost":0.25}}),
                json!({"type":"message_stop"}),
            ],
            _ => vec![
                json!({"choices":[{"delta":{"content":text},"finish_reason":match terminal {"refusal"=>"content_filter","length"=>"length",_=>"stop"}}],"usage":usage}),
            ],
        };
        let mut body = events
            .into_iter()
            .map(|value| format!("data: {value}\n\n"))
            .collect::<String>();
        if style == "chat" {
            body.push_str("data: [DONE]\n\n");
        }
        return ([("content-type", "text/event-stream")], body).into_response();
    }
    Json(match style {
        "responses" => json!({"status":if terminal=="length" {"incomplete"} else {"completed"},"output":[{"type":"message","content":[if terminal=="refusal" {json!({"type":"refusal","refusal":"refused"})} else {json!({"type":"output_text","text":text})}]}],"usage":usage}),
        "anthropic" => json!({"content":[{"type":"text","text":text}],"stop_reason":match terminal {"refusal"=>"refusal","length"=>"max_tokens",_=>"end_turn"},"usage":usage}),
        _ => json!({"choices":[{"message":{"content":text,"refusal":if terminal=="refusal" {json!("refused")}else{Value::Null}},"finish_reason":if terminal=="length" {"length"}else{"stop"}}],"usage":usage}),
    }).into_response()
}
async fn call(
    provider: &Provider,
    options: &CompletionOptions,
    stream: bool,
) -> anyhow::Result<String> {
    let model = provider.settings().model.clone();
    let messages = [json!({"role":"user","content":"private schema fixture"})];
    tokio::time::timeout(Duration::from_secs(5), async {
        if stream {
            provider
                .chat_stream_with_options(
                    &messages,
                    &[],
                    &model,
                    None,
                    options,
                    &CancellationToken::new(),
                    |_, _| {},
                )
                .await
                .map(|result| result.content)
        } else {
            provider
                .complete_json(&messages, &model, None, options)
                .await
                .map(|result| {
                    assert_eq!(result.value, json!({"answer":7}));
                    result.completion.text
                })
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn all_styles_project_exact_schema_and_preserve_legacy_text_calls() {
    let env = credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for style in ["chat", "responses", "anthropic"] {
        let peer = Peer::new(move |_, body| {
            response(style, body["stream"] == true, r#"{"answer":7}"#, "stop")
        })
        .await;
        let provider = peer.provider(style);
        let options = CompletionOptions::json_schema(schema()).unwrap();
        for stream in [false, true] {
            assert_eq!(
                call(&provider, &options, stream).await.unwrap(),
                r#"{"answer":7}"#
            );
        }
        assert_eq!(
            provider
                .complete(
                    &[json!({"role":"user","content":"legacy"})],
                    &provider.settings().model,
                    None
                )
                .await
                .unwrap(),
            r#"{"answer":7}"#
        );
        let seen = peer.seen.lock().unwrap();
        for body in &seen[..2] {
            if style == "anthropic" {
                assert!(format(style, body).is_null());
            } else {
                let mode = format(style, body);
                assert_eq!(mode["type"], "json_schema");
                let inner = if style == "responses" {
                    mode
                } else {
                    &mode["json_schema"]
                };
                assert_eq!(inner["schema"], schema());
                assert_eq!(inner["name"], "formless_output");
                assert_eq!(inner["strict"], false);
            }
        }
        assert!(
            format(style, &seen[2]).is_null(),
            "schema policy must not leak into next call"
        );
        drop(seen);
        peer.close().await;
    }
}

#[tokio::test]
async fn unsupported_mode_falls_back_once_and_never_mutates_the_next_logical_call() {
    let env = credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for style in ["chat", "responses"] {
        for stream in [false, true] {
            let field = if style == "chat" {
                "response_format"
            } else {
                "text.format"
            };
            let peer = Peer::new(move |index,body| {
            if index%2==0 {
                assert_eq!(format(style,body)["type"],"json_schema");
                (axum::http::StatusCode::BAD_REQUEST,Json(json!({"error":{"code":"unsupported_parameter","param":field,"message":format!("{field} json_schema is not supported for this model")}}))).into_response()
            } else { assert!(format(style,body).is_null()); response(style,body["stream"]==true,r#"{"answer":7}"#,"stop") }
        }).await;
            let mut cfg = peer.provider(style).settings().clone();
            cfg.extra_body.insert(
                "response_format".into(),
                json!({"type":"json_schema","json_schema":{"schema":{"const":"vendor-conflict"}}}),
            );
            cfg.extra_body.insert("text".into(),json!({"format":{"type":"json_schema","schema":{"const":"vendor-conflict"}},"verbosity":"low"}));
            let provider = Provider::new(cfg, "private-key".into()).unwrap();
            let charged = Arc::new(Mutex::new(Vec::new()));
            let capture = charged.clone();
            let provider = provider.with_usage_hook(Arc::new(move |_, model, usage| {
                capture.lock().unwrap().push((model.to_owned(), *usage));
                Ok(())
            }));
            let options = CompletionOptions::json_schema(schema()).unwrap();
            for _ in 0..2 {
                call(&provider, &options, stream).await.unwrap();
            }
            assert_eq!(peer.seen.lock().unwrap().len(), 4);
            assert_eq!(charged.lock().unwrap().len(), 2);
            peer.close().await;
        }
    }
}

#[tokio::test]
async fn invalid_schema_and_unrelated_errors_never_manufacture_mode_fallback() {
    let env = credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for style in ["chat", "responses"] {
        for stream in [false, true] {
            for invalid in [false, true] {
                let field = if style == "chat" {
                    "response_format"
                } else {
                    "text.format"
                };
                let peer=Peer::new(move |_,body| {
            let error=if invalid {json!({"param":format!("{field}.json_schema.schema"),"code":"invalid_json_schema","message":format!("Unsupported schema keyword prefixItems in {field} json_schema")})}
                else {json!({"param":"temperature","code":"unknown_parameter","message":"Unknown parameter temperature"})};
            (axum::http::StatusCode::BAD_REQUEST,Json(json!({"error":error,"request":body}))).into_response()
        }).await;
                let error = call(
                    &peer.provider(style),
                    &CompletionOptions::json_schema(schema()).unwrap(),
                    stream,
                )
                .await
                .unwrap_err();
                if invalid {
                    assert_eq!(
                        error.downcast_ref::<StructuredOutputError>(),
                        Some(&StructuredOutputError::InvalidSchema)
                    );
                }
                assert_eq!(
                    peer.seen.lock().unwrap().len(),
                    1,
                    "{style}/{stream}/{invalid}: {error}"
                );
                peer.close().await;
            }
        }
    }
}

#[tokio::test]
async fn mode_fallback_is_single_and_persists_through_a_real_stream_restart() {
    use formless_core::provider::DeltaKind;
    use futures_util::StreamExt;
    let env = credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for style in ["chat", "responses"] {
        let accepted = Arc::new(tokio::sync::Notify::new());
        let release = accepted.clone();
        let field = if style == "chat" {
            "response_format"
        } else {
            "text.format"
        };
        let peer=Peer::new(move |index,body| {
            if index==0 { assert_eq!(format(style,body)["type"],"json_schema"); return (axum::http::StatusCode::BAD_REQUEST,Json(json!({"error":{"param":field,"message":format!("{field} is not supported")}}))).into_response(); }
            assert!(format(style,body).is_null(),"repair must survive outer stream restart");
            if index==1 {
                let event=if style=="chat" {json!({"choices":[{"delta":{"content":"{\"answer\":"}}]})}else{json!({"type":"response.output_text.delta","delta":"{\"answer\":"})};
                let release=release.clone();
                let stream=futures_util::stream::once(async move {Ok::<_,std::io::Error>(axum::body::Bytes::from(format!("data: {event}\n\n")))})
                    .chain(futures_util::stream::once(async move {release.notified().await;Err(std::io::Error::other("private acknowledged transport drop"))}));
                return ([("content-type","text/event-stream")],axum::body::Body::from_stream(stream)).into_response();
            }
            response(style,true,r#"{"answer":7}"#,"stop")
        }).await;
        let mut cfg = peer.provider(style).settings().clone();
        cfg.max_retries = 1;
        cfg.max_backoff_secs = 1;
        let provider = Provider::new(cfg, "private-key".into()).unwrap();
        let mut restarts = 0;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            provider.chat_stream_with_options(
                &[json!({"role":"user","content":"restart fixture"})],
                &[],
                &provider.settings().model,
                None,
                &CompletionOptions::json_schema(schema()).unwrap(),
                &CancellationToken::new(),
                |kind, text| {
                    if kind == DeltaKind::Content && text == "{\"answer\":" {
                        accepted.notify_one();
                    }
                    if kind == DeltaKind::Restart {
                        restarts += 1;
                    }
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.content, r#"{"answer":7}"#);
        assert_eq!(restarts, 1);
        assert_eq!(peer.seen.lock().unwrap().len(), 3);
        peer.close().await;
        let peer = Peer::new(move |index, body| {
            assert_eq!(format(style, body).is_null(), index == 1);
            (
                axum::http::StatusCode::BAD_REQUEST,
                Json(
                    json!({"error":{"param":field,"message":format!("{field} is not supported")}}),
                ),
            )
                .into_response()
        })
        .await;
        call(
            &peer.provider(style),
            &CompletionOptions::json_schema(schema()).unwrap(),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(
            peer.seen.lock().unwrap().len(),
            2,
            "only one unsupported-mode resend"
        );
        peer.close().await;
    }
}

#[tokio::test]
async fn paid_invalid_json_schema_refusal_and_cutoff_keep_one_physical_charge() {
    let env = credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for style in ["chat", "responses", "anthropic"] {
        for stream in [false, true] {
            for mode in ["json", "schema", "refusal", "length"] {
                let text = match mode {
                    "json" => "prose {\"answer\":7}",
                    "schema" => r#"{"answer":"wrong"}"#,
                    _ => r#"{"answer":7}"#,
                };
                let peer =
                    Peer::new(move |_, body| response(style, body["stream"] == true, text, mode))
                        .await;
                let charged = Arc::new(Mutex::new(Vec::new()));
                let capture = charged.clone();
                let provider =
                    peer.provider(style)
                        .with_usage_hook(Arc::new(move |_, model, usage| {
                            capture.lock().unwrap().push((model.to_owned(), *usage));
                            Ok(())
                        }));
                let error = call(
                    &provider,
                    &CompletionOptions::json_schema(schema()).unwrap(),
                    stream,
                )
                .await
                .unwrap_err();
                let expected = match mode {
                    "json" => StructuredOutputError::InvalidJson,
                    "schema" => StructuredOutputError::SchemaViolation,
                    "refusal" => StructuredOutputError::RefusedResponse,
                    _ => StructuredOutputError::IncompleteResponse,
                };
                assert_eq!(
                    error.downcast_ref::<StructuredOutputError>(),
                    Some(&expected),
                    "{style}/{stream}/{mode}: {error}"
                );
                assert_eq!(peer.seen.lock().unwrap().len(), 1);
                let charged = charged.lock().unwrap();
                assert_eq!(charged.len(), 1, "one charge for {style}/{stream}/{mode}");
                assert_eq!(charged[0].1.prompt_tokens, 11);
                assert_eq!(charged[0].1.completion_tokens, 3);
                drop(charged);
                peer.close().await;
            }
        }
    }
}

#[tokio::test]
async fn structured_fallback_survives_model_failover_and_keeps_served_usage_identity() {
    let env = credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for style in ["chat", "responses"] {
        for stream in [false, true] {
            let field = if style == "chat" {
                "response_format"
            } else {
                "text.format"
            };
            let peer=Peer::new(move |index,body| {
            if index==0 {assert_eq!(format(style,body)["type"],"json_schema");return (axum::http::StatusCode::BAD_REQUEST,Json(json!({"error":{"param":field,"message":format!("{field} is not supported")}}))).into_response();}
            assert!(format(style,body).is_null());
            if index==1 {return (axum::http::StatusCode::NOT_FOUND,Json(json!({"error":{"message":"model not found"}}))).into_response();}
            response(style,stream,r#"{"answer":7}"#,"stop")
        }).await;
            let mut cfg = peer.provider(style).settings().clone();
            let fallback = format!("fallback-{}", uuid::Uuid::new_v4());
            cfg.fallback_models = vec![fallback.clone()];
            let charged = Arc::new(Mutex::new(Vec::new()));
            let capture = charged.clone();
            let provider = Provider::new(cfg, "private-key".into())
                .unwrap()
                .with_usage_hook(Arc::new(move |_, model, usage| {
                    capture.lock().unwrap().push((model.to_owned(), *usage));
                    Ok(())
                }));
            let options = CompletionOptions::json_schema(schema()).unwrap();
            let messages = [json!({"role":"user","content":"fallback fixture"})];
            let served = if stream {
                provider
                    .chat_stream_with_options(
                        &messages,
                        &[],
                        &provider.settings().model,
                        None,
                        &options,
                        &CancellationToken::new(),
                        |_, _| {},
                    )
                    .await
                    .unwrap()
                    .model_used
            } else {
                provider
                    .complete_json(&messages, &provider.settings().model, None, &options)
                    .await
                    .unwrap()
                    .completion
                    .model_used
            };
            assert_eq!(served, fallback);
            assert_eq!(peer.seen.lock().unwrap().len(), 3);
            assert_eq!(charged.lock().unwrap()[0].0, fallback);
            assert_eq!(charged.lock().unwrap().len(), 1);
            peer.close().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn actual_tool_structured_failures_preserve_parent_invocation_and_run_usage() {
    use formless_core::{daemon::Daemon, Config, Paths, Store};
    use formless_runtime::Capabilities;
    let env = credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    env.set("FORMLESS_API_KEY", "private-structured-key");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env.remove("FORMLESS_SECURE");
    for mode in [
        "json",
        "schema",
        "refusal",
        "length",
        "valid",
        "legacy",
        "invalid-local",
    ] {
        let peer = Peer::new(move |_, body| {
            if mode == "legacy" {
                assert!(body.get("response_format").is_none());
            } else {
                assert_eq!(body["response_format"]["type"], "json_schema");
            }
            response(
                "chat",
                false,
                match mode {
                    "json" => "not json",
                    "schema" => r#"{"answer":"wrong"}"#,
                    "legacy" => "ordinary legacy text",
                    _ => r#"{"answer":7}"#,
                },
                mode,
            )
        })
        .await;
        let root = std::env::temp_dir().join(format!("fl-tool-schema-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let mut cfg = Config::default();
        cfg.provider = peer.provider("chat").settings().clone();
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.security.secure = false;
        cfg.save(&paths).unwrap();
        let store = Store::open(&paths.db_file()).unwrap();
        let sid = store.create_session(None).unwrap().id;
        let opts = if mode == "legacy" {
            "{}".into()
        } else if mode == "invalid-local" {
            r#"{schema:{$ref:'file:///private-not-read'}}"#.into()
        } else {
            format!("{{schema:{}}}", schema())
        };
        let code = format!("function run(){{return llm.complete('fixture', {opts});}}");
        store
            .create_tool_version(
                "schema_fixture",
                "private schema fixture",
                r#"{"type":"object"}"#,
                "{}",
                &code,
                &Capabilities::parse(&["llm".into()]).unwrap(),
                128,
                "agent",
            )
            .unwrap();
        drop(store);
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            daemon.clone().execute_tool_value(
                sid.clone(),
                "schema-parent-run".into(),
                "schema_fixture".into(),
                json!({}),
                0,
                None,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                None,
                CancellationToken::new(),
                cfg.clone(),
                true,
            ),
        )
        .await
        .unwrap();
        if matches!(mode, "valid" | "legacy") {
            let (value, _) = result.unwrap();
            assert_eq!(
                value,
                if mode == "legacy" {
                    json!("ordinary legacy text")
                } else {
                    json!({"answer":7})
                }
            );
        } else {
            assert!(result.is_err(), "{mode}: {result:?}");
        }
        let db = rusqlite::Connection::open(paths.db_file()).unwrap();
        let (tokens,model_tokens,cost,unpriced):(i64,i64,f64,i64)=db.query_row("SELECT tokens,model_tokens,model_cost,model_unpriced FROM invocations WHERE tool_id=(SELECT id FROM tools WHERE name='schema_fixture') ORDER BY created_at DESC LIMIT 1",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM run_usage WHERE source='llm' AND run_id='schema-parent-run'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if mode == "invalid-local" {
            assert_eq!(
                (tokens, model_tokens, cost, unpriced, count),
                (0, 0, 0.0, 0, 0)
            );
            assert_eq!(peer.seen.lock().unwrap().len(), 0);
        } else {
            assert_eq!(
                (tokens, model_tokens, cost, unpriced, count),
                (14, 14, 0.25, 0, 1),
                "{mode}"
            );
            assert_eq!(peer.seen.lock().unwrap().len(), 1);
        }
        drop(db);
        daemon
            .handle(formless_protocol::Request::Shutdown)
            .await
            .unwrap();
        let weak = Arc::downgrade(&daemon);
        drop(daemon);
        tokio::time::timeout(Duration::from_secs(5), async {
            while weak.strong_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        peer.close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
