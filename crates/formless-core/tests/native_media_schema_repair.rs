//! Typed provider recovery must operate on content parts, never schema data.
//! Private owned uploads and loopback HTTP only; no providers or user files.
#[path = "support/env.rs"]
mod credential_env;
use axum::{extract::OriginalUri, response::IntoResponse, routing::post, Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use formless_core::{
    config::ProviderCfg,
    daemon::Daemon,
    provider::{DeltaKind, Provider},
    storage::provider_message,
    Config, Paths, Store,
};
use formless_protocol::Request;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn schema_data() -> Value {
    json!({"type":"object","properties":{"payload":{"type":"object",
        "const":{"type":"file","file":{"file_data":"KEEP_CONST_FILE"},"inner":[{"type":"thinking","thinking":"KEEP_THINKING","signature":"KEEP_SIGNATURE"}]},
        "default":{"type":"input_audio","input_audio":{"data":"KEEP_AUDIO","format":"wav"}},
        "examples":[{"type":"image","source":{"type":"base64","data":"KEEP_IMAGE"}}, {"type":"document","source":{"data":"KEEP_PDF"}}, {"type":"input_file","file_data":"KEEP_INPUT_FILE"}, {"type":"redacted_thinking","data":"KEEP_REDACTED"}]
    }}})
}
fn tools() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{"name":"private_schema_fixture","description":"Private schema data fixture","parameters":schema_data()}}),
    ]
}
fn wav() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(40u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(16000u32.to_le_bytes());
    bytes.extend(32000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(4u32.to_le_bytes());
    bytes.extend([0, 0, 1, 0]);
    bytes
}
fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(8, 8)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    bytes
}
struct Script {
    error: String,
    drop_after_repair: bool,
    bodies: Vec<Value>,
}
struct Fixture {
    root: std::path::PathBuf,
    paths: Paths,
    daemon: Option<Arc<Daemon>>,
    store: Option<Store>,
    session: String,
    base: String,
    script: Arc<Mutex<Script>>,
    partial_seen: Arc<tokio::sync::Notify>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}
impl Fixture {
    async fn new(env: &credential_env::EnvGuard) -> Self {
        env.set("FORMLESS_API_KEY", "private-schema-repair-key");
        env.set("NO_PROXY", "127.0.0.1,localhost");
        for key in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
            env.remove(key);
        }
        let script = Arc::new(Mutex::new(Script {
            error: String::new(),
            drop_after_repair: false,
            bodies: Vec::new(),
        }));
        let partial_seen = Arc::new(tokio::sync::Notify::new());
        let handler = {
            let script = script.clone();
            let partial_seen = partial_seen.clone();
            move |OriginalUri(uri): OriginalUri, Json(body): Json<Value>| {
                let partial_seen = partial_seen.clone();
                let (attempt, error, drop_after_repair) = {
                    let mut script = script.lock().unwrap();
                    script.bodies.push(body.clone());
                    (
                        script.bodies.len(),
                        script.error.clone(),
                        script.drop_after_repair,
                    )
                };
                async move {
                    if attempt == 1 {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            Json(json!({"error":{"message":error}})),
                        )
                            .into_response();
                    }
                    let anthropic = uri.path() == "/messages";
                    let responses = uri.path() == "/responses";
                    if body["stream"] == true {
                        let partial = drop_after_repair && attempt == 2;
                        let events = if anthropic {
                            if partial {
                                vec![
                                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"lost"}}),
                                ]
                            } else {
                                vec![
                                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                                    json!({"type":"content_block_stop","index":0}),
                                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
                                    json!({"type":"message_stop"}),
                                ]
                            }
                        } else if responses {
                            if partial {
                                vec![json!({"type":"response.output_text.delta","delta":"lost"})]
                            } else {
                                vec![
                                    json!({"type":"response.output_text.delta","delta":"ok"}),
                                    json!({"type":"response.completed","response":{"status":"completed"}}),
                                ]
                            }
                        } else if partial {
                            vec![json!({"choices":[{"delta":{"content":"lost"}}]})]
                        } else {
                            vec![
                                json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}),
                            ]
                        };
                        let mut text = events
                            .into_iter()
                            .map(|event| format!("data: {event}\n\n"))
                            .collect::<String>();
                        if !anthropic && !responses && !partial {
                            text.push_str("data: [DONE]\n\n");
                        }
                        if partial {
                            let first = futures_util::stream::once(async move {
                                Ok::<_, std::io::Error>(tokio_util::bytes::Bytes::from(text))
                            });
                            let failure = futures_util::stream::once(async move {
                                partial_seen.notified().await;
                                Err::<tokio_util::bytes::Bytes, _>(std::io::Error::other(
                                    "private controlled response-body drop",
                                ))
                            });
                            return (
                                [("content-type", "text/event-stream")],
                                axum::body::Body::from_stream(first.chain(failure)),
                            )
                                .into_response();
                        }
                        return ([("content-type", "text/event-stream")], text).into_response();
                    }
                    Json(if anthropic {json!({"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"})}else if responses {json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}]})}else{json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]})}).into_response()
                }
            }
        };
        let app = Router::new()
            .route("/chat/completions", post(handler.clone()))
            .route("/responses", post(handler.clone()))
            .route("/messages", post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        let root =
            std::env::temp_dir().join(format!("fl-native-schema-repair-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        let store = Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap().id;
        Self {
            root,
            paths,
            daemon: Some(daemon),
            store: Some(store),
            session,
            base,
            script,
            partial_seen,
            stop: Some(stop),
            server: Some(server),
        }
    }
    fn reset(&self, error: &str, drop_after_repair: bool) {
        let mut script = self.script.lock().unwrap();
        script.error = error.into();
        script.drop_after_repair = drop_after_repair;
        script.bodies.clear();
    }
    async fn provider(&self, style: &str, retries: usize) -> Provider {
        let mut cfg = ProviderCfg {
            base_url: self.base.clone(),
            model: "fixture".into(),
            api_style: style.into(),
            max_retries: retries,
            max_backoff_secs: 1,
            fallback_models: Vec::new(),
            audio_input: Some(true),
            pdf_input: Some(true),
            vision: Some(true),
            cache_control: "off".into(),
            ..Default::default()
        };
        cfg.extra_body
            .insert("vendor_schema_guard".into(), schema_data());
        Provider::for_profile_with(cfg, self.paths.clone(), reqwest::Client::new())
            .await
            .unwrap()
    }
    async fn messages(&self, kind: &str) -> Vec<Value> {
        let store = self.store.as_ref().unwrap();
        if kind == "thinking" {
            let user = store
                .append_message(&self.session, "user", "private previous prompt")
                .unwrap();
            let assistant=store.append_message_meta(&self.session,"assistant","private previous answer",Some(json!({"thinking_blocks":[{"type":"thinking","thinking":"private replayed reasoning","signature":"private-rejected-signature"}]}).to_string())).unwrap();
            let next = store
                .append_message(&self.session, "user", "continue private fixture")
                .unwrap();
            return [&user, &assistant, &next]
                .into_iter()
                .map(provider_message)
                .collect();
        }
        let (name, mime, bytes) = match kind {
            "audio" => ("private.wav", "audio/wav", wav()),
            "pdf" => (
                "private.pdf",
                "application/pdf",
                b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n".to_vec(),
            ),
            "image" => ("private.png", "image/png", png()),
            _ => panic!("unknown private modality"),
        };
        self.daemon
            .as_ref()
            .unwrap()
            .handle(Request::AttachFile {
                session_id: self.session.clone(),
                name: name.into(),
                data_b64: STANDARD.encode(bytes),
                mime: Some(mime.into()),
            })
            .await
            .unwrap();
        let held = store.take_pending_attachments(&self.session).unwrap();
        assert_eq!(held.len(), 1);
        let message = store
            .append_message_meta(
                &self.session,
                "user",
                "inspect private attachment",
                Some(json!({"attachments":held}).to_string()),
            )
            .unwrap();
        vec![provider_message(&message)]
    }
    async fn close(mut self) {
        drop(self.store.take());
        drop(self.daemon.take());
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.server.take().unwrap())
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(server) = &self.server {
            server.abort();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn actual_parts(body: &Value, kind: &str) -> usize {
    fn count(parts: &Value, kind: &str) -> usize {
        parts
            .as_array()
            .into_iter()
            .flatten()
            .map(|part| {
                usize::from(part["type"] == kind)
                    + if part["type"] == "tool_result" {
                        count(&part["content"], kind)
                    } else {
                        0
                    }
            })
            .sum()
    }
    ["messages", "input"]
        .into_iter()
        .map(|field| {
            body[field]
                .as_array()
                .into_iter()
                .flatten()
                .map(|message| count(&message["content"], kind))
                .sum::<usize>()
        })
        .sum()
}
fn wire_kind(style: &str, modality: &str) -> &'static str {
    match modality {
        "audio" => "input_audio",
        "pdf" => match style {
            "responses" => "input_file",
            "anthropic" => "document",
            _ => "file",
        },
        "image" => "image",
        "thinking" => "thinking",
        _ => unreachable!(),
    }
}
fn error_for(modality: &str) -> &'static str {
    match modality {
        "audio" => "audio input is unsupported",
        "pdf" => "input_file and file_data PDF input is unsupported",
        "image" => "image input is unsupported",
        "thinking" => "signature invalid in thinking block",
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn actual_content_repairs_and_stream_restarts_preserve_tool_schema_data() {
    let env = credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env).await;
    for (style, modality) in [
        ("chat", "audio"),
        ("chat", "pdf"),
        ("responses", "pdf"),
        ("anthropic", "pdf"),
        ("anthropic", "image"),
        ("anthropic", "thinking"),
    ] {
        fixture.reset(error_for(modality), true);
        let provider = fixture.provider(style, 1).await;
        let messages = fixture.messages(modality).await;
        let mut restarts = 0;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            provider.chat_stream(
                &messages,
                &tools(),
                "fixture",
                Some("high"),
                &CancellationToken::new(),
                |kind, text| {
                    if matches!(kind, DeltaKind::Content) && text == "lost" {
                        fixture.partial_seen.notify_one();
                    }
                    if matches!(kind, DeltaKind::Restart) {
                        restarts += 1;
                    }
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.content, "ok", "{style}/{modality}");
        assert_eq!(restarts, 1, "real incomplete-stream restart");
        let bodies = fixture.script.lock().unwrap().bodies.clone();
        assert_eq!(
            bodies.len(),
            3,
            "one repair and one stream restart: {style}/{modality}"
        );
        let kind = wire_kind(style, modality);
        assert!(
            actual_parts(&bodies[0], kind) > 0,
            "actual {kind} positive control"
        );
        for body in &bodies {
            assert_eq!(
                body["tools"], bodies[0]["tools"],
                "schema changed: {style}/{modality}"
            );
            assert_eq!(
                body["vendor_schema_guard"],
                schema_data(),
                "vendor schema data changed: {style}/{modality}"
            );
        }
        assert_eq!(
            actual_parts(&bodies[1], kind),
            0,
            "repair must remove actual rejected part"
        );
        assert_eq!(
            actual_parts(&bodies[2], kind),
            0,
            "repair must survive stream restart"
        );
    }
    fixture.close().await;
}
#[tokio::test]
async fn nonstreaming_content_repairs_leave_vendor_schema_data_unchanged() {
    let env = credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env).await;
    for (style, modality) in [
        ("chat", "audio"),
        ("chat", "pdf"),
        ("responses", "pdf"),
        ("anthropic", "pdf"),
        ("anthropic", "image"),
        ("anthropic", "thinking"),
    ] {
        fixture.reset(error_for(modality), false);
        let provider = fixture.provider(style, 0).await;
        let messages = fixture.messages(modality).await;
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            provider.complete(&messages, "fixture", Some("high")),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, "ok");
        let bodies = fixture.script.lock().unwrap().bodies.clone();
        assert_eq!(
            bodies.len(),
            2,
            "exactly one typed repair: {style}/{modality}"
        );
        assert!(actual_parts(&bodies[0], wire_kind(style, modality)) > 0);
        assert_eq!(actual_parts(&bodies[1], wire_kind(style, modality)), 0);
        for body in bodies {
            assert_eq!(
                body["vendor_schema_guard"],
                schema_data(),
                "nonstream schema changed: {style}/{modality}"
            );
        }
    }
    fixture.close().await;
}
#[tokio::test]
async fn schema_only_modality_objects_never_trigger_native_repair_or_extra_send() {
    let env = credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env).await;
    for style in ["chat", "responses", "anthropic"] {
        for stream in [false, true] {
            fixture.reset("audio input is unsupported", false);
            let provider = fixture.provider(style, 0).await;
            let messages = [json!({"role":"user","content":"plain private text, no media"})];
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                if stream {
                    provider
                        .chat_stream(
                            &messages,
                            &tools(),
                            "fixture",
                            None,
                            &CancellationToken::new(),
                            |_, _| {},
                        )
                        .await
                        .map(|_| ())
                } else {
                    provider
                        .complete(&messages, "fixture", None)
                        .await
                        .map(|_| ())
                }
            })
            .await
            .unwrap();
            assert!(
                result.is_err(),
                "schema data must not manufacture a repair: {style} stream={stream}"
            );
            let bodies = fixture.script.lock().unwrap().bodies.clone();
            assert_eq!(bodies.len(), 1, "no actual audio, so no repair resend");
            assert_eq!(bodies[0]["vendor_schema_guard"], schema_data());
            assert_eq!(actual_parts(&bodies[0], "input_audio"), 0);
        }
    }
    fixture.close().await;
}
