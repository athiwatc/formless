//! Every selected physical model gets its own native projection from frozen,
//! owned bytes. All peers/files are disposable; no provider or keyring calls.
#[path = "support/env.rs"]
mod credential_env;
use axum::{extract::OriginalUri, response::IntoResponse, routing::post, Json, Router};
use base64::{engine::general_purpose::STANDARD, Engine};
use formless_core::{
    config::ProviderCfg, daemon::Daemon, provider::Provider, storage::provider_message, Config,
    Paths, Store,
};
use formless_protocol::Request;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
fn wav() -> Vec<u8> {
    let mut b = b"RIFF".to_vec();
    b.extend(40u32.to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(16000u32.to_le_bytes());
    b.extend(32000u32.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend(4u32.to_le_bytes());
    b.extend([0, 0, 1, 0]);
    b
}
fn pdf() -> Vec<u8> {
    b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n".to_vec()
}
fn response(path: &str, body: &Value) -> axum::response::Response {
    let native = path == "/messages";
    let responses = path == "/responses";
    let usage = if native || responses {
        json!({"input_tokens":20,"output_tokens":3})
    } else {
        json!({"prompt_tokens":20,"completion_tokens":3})
    };
    if body["stream"] == true {
        let events = if native {
            vec![
                json!({"type":"message_start","message":{"usage":{"input_tokens":20}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
                json!({"type":"message_stop"}),
            ]
        } else if responses {
            vec![
                json!({"type":"response.output_text.delta","delta":"ok"}),
                json!({"type":"response.completed","response":{"status":"completed","usage":usage}}),
            ]
        } else {
            vec![
                json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}],"usage":usage}),
            ]
        };
        let mut text = events
            .into_iter()
            .map(|e| format!("data: {e}\n\n"))
            .collect::<String>();
        if !native && !responses {
            text.push_str("data: [DONE]\n\n");
        }
        return ([("content-type", "text/event-stream")], text).into_response();
    }
    Json(if native {json!({"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":usage})}else if responses {json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}],"usage":usage})}else {json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":usage})}).into_response()
}
fn parts<'a>(body: &'a Value, kind: &str, out: &mut Vec<&'a Value>) {
    match body {
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some(kind) {
                out.push(body);
            }
            for v in o.values() {
                parts(v, kind, out);
            }
        }
        Value::Array(a) => {
            for v in a {
                parts(v, kind, out)
            }
        }
        _ => {}
    }
}
fn native_bytes(body: &Value, style: &str, audio: bool) -> Option<Vec<u8>> {
    let kind = if audio {
        "input_audio"
    } else if style == "responses" {
        "input_file"
    } else if style == "anthropic" {
        "document"
    } else {
        "file"
    };
    let mut found = Vec::new();
    parts(body, kind, &mut found);
    assert!(found.len() <= 1);
    let part = found.first()?;
    let raw = if audio {
        assert_eq!(part["input_audio"]["format"], "wav");
        part["input_audio"]["data"].as_str().unwrap()
    } else if style == "anthropic" {
        part["source"]["data"].as_str().unwrap()
    } else if style == "responses" {
        part["file_data"]
            .as_str()
            .unwrap()
            .strip_prefix("data:application/pdf;base64,")
            .unwrap()
    } else {
        part["file"]["file_data"]
            .as_str()
            .unwrap()
            .strip_prefix("data:application/pdf;base64,")
            .unwrap()
    };
    Some(STANDARD.decode(raw).unwrap())
}
struct Owned {
    daemon: Arc<Daemon>,
    store: Store,
    paths: Paths,
    root: PathBuf,
    sid: String,
}
impl Owned {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-media-attempts-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        let store = Store::open(&paths.db_file()).unwrap();
        let sid = store.create_session(None).unwrap().id;
        Self {
            daemon,
            store,
            paths,
            root,
            sid,
        }
    }
    async fn upload(&self, bytes: &[u8], audio: bool) -> (Vec<Value>, PathBuf) {
        self.daemon
            .handle(Request::AttachFile {
                session_id: self.sid.clone(),
                name: if audio { "source.wav" } else { "source.pdf" }.into(),
                data_b64: STANDARD.encode(bytes),
                mime: Some(
                    if audio {
                        "audio/wav"
                    } else {
                        "application/pdf"
                    }
                    .into(),
                ),
            })
            .await
            .unwrap();
        let held = self.store.take_pending_attachments(&self.sid).unwrap();
        assert_eq!(held.len(), 1);
        let path = PathBuf::from(held[0]["path"].as_str().unwrap());
        let message = self
            .store
            .append_message_meta(
                &self.sid,
                "user",
                "analyze source",
                Some(json!({"attachments":held}).to_string()),
            )
            .unwrap();
        (vec![provider_message(&message)], path)
    }
    fn close(self) {
        let Self {
            daemon,
            store,
            root,
            ..
        } = self;
        drop(store);
        drop(daemon);
        std::fs::remove_dir_all(root).unwrap();
    }
}
struct Lease(Arc<AtomicUsize>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn fallbacks_reproject_both_capability_directions_using_pre_failure_owned_bytes() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-media-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for key in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
        env.remove(key);
    }
    let owned = Owned::new();
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let mutations = Arc::new(Mutex::new(HashMap::<String, (PathBuf, Vec<u8>)>::new()));
    let capture = seen.clone();
    let changes = mutations.clone();
    let handler = move |OriginalUri(uri): OriginalUri, Json(body): Json<Value>| {
        capture.lock().unwrap().push(body.clone());
        let change = changes
            .lock()
            .unwrap()
            .get(body["model"].as_str().unwrap())
            .cloned();
        async move {
            if let Some((file, replacement)) = change {
                std::fs::write(file, replacement).unwrap();
                return (axum::http::StatusCode::NOT_FOUND, "model not found").into_response();
            }
            response(uri.path(), &body)
        }
    };
    let app = Router::new()
        .route("/chat/completions", post(handler.clone()))
        .route("/responses", post(handler.clone()))
        .route("/messages", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap()
    });
    for (style, audio) in [
        ("chat", true),
        ("chat", false),
        ("responses", false),
        ("anthropic", false),
    ] {
        for stream in [false, true] {
            for primary_accepts in [false, true] {
                let original = if audio { wav() } else { pdf() };
                let (messages, file) = owned.upload(&original, audio).await;
                let mut changed = original.clone();
                *changed.last_mut().unwrap() ^= 1;
                let nonce = uuid::Uuid::new_v4();
                let primary = format!("primary-{nonce}");
                let fallback = format!("fallback-{nonce}");
                mutations
                    .lock()
                    .unwrap()
                    .insert(primary.clone(), (file.clone(), changed.clone()));
                let mut cfg = ProviderCfg {
                    base_url: base.clone(),
                    model: primary.clone(),
                    api_style: style.into(),
                    audio_input: Some(false),
                    pdf_input: Some(false),
                    max_retries: 0,
                    fallback_models: vec![fallback.clone()],
                    ..Default::default()
                };
                cfg.aliases.insert("shortcut".into(), primary.clone());
                let map = if audio {
                    &mut cfg.model_audio_input
                } else {
                    &mut cfg.model_pdf_input
                };
                map.insert(primary.clone(), primary_accepts);
                map.insert(fallback.clone(), !primary_accepts);
                let active = Arc::new(AtomicUsize::new(0));
                let calls = Arc::new(AtomicUsize::new(0));
                let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
                let usages = Arc::new(Mutex::new(Vec::new()));
                let p =
                    Provider::for_profile_with(cfg, owned.paths.clone(), reqwest::Client::new())
                        .await
                        .unwrap()
                        .with_request_hook({
                            let active = active.clone();
                            let calls = calls.clone();
                            Arc::new(move || {
                                let active = active.clone();
                                calls.fetch_add(1, Ordering::SeqCst);
                                Box::pin(async move {
                                    active.fetch_add(1, Ordering::SeqCst);
                                    Ok(Box::new(Lease(active))
                                        as formless_core::provider::RequestLease)
                                })
                            })
                        })
                        .with_input_hook({
                            let observed = observed.clone();
                            let active = active.clone();
                            Arc::new(move |body| {
                                assert_eq!(active.load(Ordering::SeqCst), 1);
                                observed.lock().unwrap().push(body.clone());
                            })
                        })
                        .with_usage_hook({
                            let usages = usages.clone();
                            let active = active.clone();
                            Arc::new(move |_, model, usage| {
                                assert_eq!(
                                    active.load(Ordering::SeqCst),
                                    1,
                                    "known usage committed while lease held"
                                );
                                usages.lock().unwrap().push((model.to_string(), *usage));
                                Ok(())
                            })
                        });
                let before = seen.lock().unwrap().len();
                let (model, usage) = tokio::time::timeout(Duration::from_secs(5), async {
                    if stream {
                        let r = p
                            .chat_stream(
                                &messages,
                                &[],
                                "shortcut",
                                None,
                                &CancellationToken::new(),
                                |_, _| {},
                            )
                            .await
                            .unwrap();
                        (r.model_used, r.usage)
                    } else {
                        let r = p
                            .complete_result(&messages, "shortcut", None)
                            .await
                            .unwrap();
                        (r.model_used, r.usage)
                    }
                })
                .await
                .expect("bounded fallback fixture");
                assert_eq!(model, fallback);
                assert_eq!(usage.unwrap().cost_unknown, !primary_accepts);
                assert_eq!(calls.load(Ordering::SeqCst), 2);
                assert_eq!(active.load(Ordering::SeqCst), 0);
                let captured = seen.lock().unwrap();
                let attempts = &captured[before..];
                assert_eq!(attempts.len(), 2);
                assert_eq!(attempts[0]["model"], primary);
                assert_eq!(attempts[1]["model"], fallback);
                for (body, accepts) in attempts.iter().zip([primary_accepts, !primary_accepts]) {
                    assert_eq!(
                        native_bytes(body, style, audio),
                        accepts.then(|| original.clone()),
                        "actual model projection: {style} stream={stream}"
                    );
                    if !accepts {
                        assert!(body.to_string().contains("artifact reference retained"));
                    }
                    assert!(
                        !body.to_string().contains("_attachments")
                            && !body.to_string().contains("_source_session")
                            && !body
                                .to_string()
                                .contains(owned.paths.data_dir.to_str().unwrap())
                    );
                }
                assert_eq!(
                    std::fs::read(file).unwrap(),
                    changed,
                    "fixture really changed stored bytes between models"
                );
                assert_eq!(*observed.lock().unwrap(), vec![attempts[1].clone()]);
                assert!(usages
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(model, _)| model == &fallback));
            }
        }
    }
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    owned.close();
}

#[tokio::test]
async fn native_formats_sizes_and_foreign_markers_fall_back_without_extra_calls() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-media-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for key in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
        env.remove(key);
    }
    let owned = Owned::new();
    let seen = Arc::new(Mutex::new(None::<Value>));
    let requests = Arc::new(AtomicUsize::new(0));
    let capture = seen.clone();
    let count = requests.clone();
    let handler = move |OriginalUri(uri): OriginalUri, Json(body): Json<Value>| {
        count.fetch_add(1, Ordering::SeqCst);
        *capture.lock().unwrap() = Some(body.clone());
        async move { response(uri.path(), &body) }
    };
    let app = Router::new()
        .route("/chat/completions", post(handler.clone()))
        .route("/responses", post(handler.clone()))
        .route("/messages", post(handler))
        .layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap()
    });
    // A valid WAV signature wins over a misleading PDF name/client MIME.
    let source = wav();
    let (messages, _) = owned.upload(&source, false).await;
    let chat = Provider::for_profile_with(
        ProviderCfg {
            base_url: base.clone(),
            model: "fixture".into(),
            audio_input: Some(true),
            pdf_input: Some(true),
            max_retries: 0,
            ..Default::default()
        },
        owned.paths.clone(),
        reqwest::Client::new(),
    )
    .await
    .unwrap();
    let before = requests.load(Ordering::SeqCst);
    chat.complete(&messages, "fixture", None).await.unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), before + 1);
    assert_eq!(
        native_bytes(seen.lock().unwrap().as_ref().unwrap(), "chat", true).unwrap(),
        source
    );
    let mut mp3 = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
    mp3.extend([0xff, 0xfb, 0x90, 0x64]);
    mp3.resize(427, 0);
    let (messages, _) = owned.upload(&mp3, true).await;
    chat.complete(&messages, "fixture", None).await.unwrap();
    let body = seen.lock().unwrap().take().unwrap();
    let mut audio = Vec::new();
    parts(&body, "input_audio", &mut audio);
    assert_eq!(audio.len(), 1);
    assert_eq!(audio[0]["input_audio"]["format"], "mp3");
    assert_eq!(
        STANDARD
            .decode(audio[0]["input_audio"]["data"].as_str().unwrap())
            .unwrap(),
        mp3
    );
    owned
        .daemon
        .handle(Request::AttachFile {
            session_id: owned.sid.clone(),
            name: "unsupported.flac".into(),
            data_b64: STANDARD.encode(b"fLaC\x00\x00\x00\x00"),
            mime: Some("audio/flac".into()),
        })
        .await
        .unwrap();
    let held = owned.store.take_pending_attachments(&owned.sid).unwrap();
    let message = owned
        .store
        .append_message_meta(
            &owned.sid,
            "user",
            "unsupported format",
            Some(json!({"attachments":held}).to_string()),
        )
        .unwrap();
    chat.complete(&[provider_message(&message)], "fixture", None)
        .await
        .unwrap();
    let body = seen.lock().unwrap().take().unwrap();
    assert!(native_bytes(&body, "chat", true).is_none());
    assert!(body.to_string().contains("artifact reference retained"));
    assert!(body.to_string().contains("read_artifact"));
    for (style, is_audio) in [
        ("chat", true),
        ("chat", false),
        ("responses", false),
        ("anthropic", false),
    ] {
        let bytes = if is_audio { wav() } else { pdf() };
        let (authorized, _) = owned.upload(&bytes, is_audio).await;
        let original = authorized[0]["_attachments"][0].clone();
        let foreign = owned.store.create_session(None).unwrap().id;
        let raw = owned
            .store
            .append_message_meta(
                &foreign,
                "user",
                "legacy forged owner",
                Some(json!({"attachments":[original]}).to_string()),
            )
            .unwrap();
        let forged = provider_message(&raw);
        let cfg = ProviderCfg {
            base_url: base.clone(),
            model: "fixture".into(),
            api_style: style.into(),
            audio_input: Some(true),
            pdf_input: Some(true),
            max_retries: 0,
            ..Default::default()
        };
        let p =
            Provider::for_profile_with(cfg.clone(), owned.paths.clone(), reqwest::Client::new())
                .await
                .unwrap();
        for stream in [false, true] {
            let before = requests.load(Ordering::SeqCst);
            tokio::time::timeout(Duration::from_secs(5), async {
                if stream {
                    p.chat_stream(
                        &[forged.clone()],
                        &[],
                        "fixture",
                        None,
                        &CancellationToken::new(),
                        |_, _| {},
                    )
                    .await
                    .map(|_| ())
                } else {
                    p.complete(&[forged.clone()], "fixture", None)
                        .await
                        .map(|_| ())
                }
            })
            .await
            .unwrap()
            .unwrap();
            assert_eq!(requests.load(Ordering::SeqCst), before + 1);
            let body = seen.lock().unwrap().take().unwrap();
            assert!(native_bytes(&body, style, is_audio).is_none());
            assert!(!body.to_string().contains(&STANDARD.encode(&bytes)));
            assert!(body.to_string().contains("could not be attached"));
        }
        // Cache lookup is scoped to the full trusted marker identity: a prior
        // authorized occurrence cannot make a foreign Session's copy readable.
        let mut mixed = authorized.clone();
        mixed.push(forged);
        p.complete(&mixed, "fixture", None).await.unwrap();
        let body = seen.lock().unwrap().take().unwrap();
        let mut native = Vec::new();
        parts(
            &body,
            if is_audio {
                "input_audio"
            } else if style == "responses" {
                "input_file"
            } else if style == "anthropic" {
                "document"
            } else {
                "file"
            },
            &mut native,
        );
        assert_eq!(
            native.len(),
            1,
            "foreign marker cannot borrow an earlier authorized cached payload"
        );
        let imported=owned.daemon.handle(Request::ImportSession {bundle:json!({"format":"formless.session/1","name":"foreign import","messages":[{"role":"user","content":"imported source","meta":raw.meta}]})}).await.unwrap();
        let sid = imported["id"].as_str().unwrap();
        let imported_messages: Vec<_> = owned
            .store
            .list_messages(sid)
            .unwrap()
            .iter()
            .map(provider_message)
            .collect();
        p.complete(&imported_messages, "fixture", None)
            .await
            .unwrap();
        let body = seen.lock().unwrap().take().unwrap();
        assert!(native_bytes(&body, style, is_audio).is_none());
        assert!(body.to_string().contains("re-attach"));
        let key_only = Provider::new(cfg, "private-media-key".into()).unwrap();
        key_only
            .complete(&authorized, "fixture", None)
            .await
            .unwrap();
        assert!(native_bytes(seen.lock().unwrap().as_ref().unwrap(), style, is_audio).is_none());
    }
    // Per-source 8 MiB native limit retains reference text; source intake still
    // accepts the file safely within its independent 20 MiB upload bound.
    for is_audio in [false, true] {
        let mut bytes = if is_audio { wav() } else { pdf() };
        bytes.resize(8 * 1024 * 1024 + 1, 0);
        let (messages, _) = owned.upload(&bytes, is_audio).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            chat.complete(&messages, "fixture", None),
        )
        .await
        .unwrap()
        .unwrap();
        let body = seen.lock().unwrap().take().unwrap();
        assert!(native_bytes(&body, "chat", is_audio).is_none());
        assert!(body.to_string().contains("8 MiB inline limit"));
        assert!(body.to_string().contains("read_artifact"));
    }
    // Three legal 8 MiB PDFs would exceed the request's 24 MiB encoded budget.
    // Only a deterministic bounded prefix is transmitted, with one omission.
    let mut bytes = pdf();
    bytes.resize(8 * 1024 * 1024, 0);
    let (messages, _) = owned.upload(&bytes, false).await;
    let mut marker = messages[0].clone();
    let a = marker["_attachments"][0].clone();
    marker["_attachments"] = json!([a, a, a]);
    tokio::time::timeout(
        Duration::from_secs(5),
        chat.complete(&[marker], "fixture", None),
    )
    .await
    .unwrap()
    .unwrap();
    let body = seen.lock().unwrap().take().unwrap();
    let mut files = Vec::new();
    parts(&body, "file", &mut files);
    assert_eq!(files.len(), 2);
    let encoded: usize = files
        .iter()
        .map(|p| p["file"]["file_data"].as_str().unwrap().len())
        .sum();
    assert!(encoded <= 24 * 1024 * 1024);
    assert!(body.to_string().contains("encoded-image budget"));
    assert_eq!(body.to_string().matches("could not be attached").count(), 1);
    drop(body);
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    owned.close();
}

#[tokio::test]
async fn attachment_diagnostics_suppress_full_and_prefix_echoes_after_repairs_and_in_streams() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-media-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for key in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
        env.remove(key);
    }
    let owned = Owned::new();
    for (style, audio) in [
        ("chat", false),
        ("responses", false),
        ("anthropic", false),
        ("chat", true),
    ] {
        let mut bytes = if audio { wav() } else { pdf() };
        bytes.extend((0..4096).map(|i| ((i * 71 + 193) % 256) as u8));
        let encoded = STANDARD.encode(&bytes);
        let (messages, _) = owned.upload(&bytes, audio).await;
        for mode in ["full", "prefix", "sse"] {
            for stream in [false, true] {
                if mode == "sse" && !stream {
                    continue;
                }
                let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
                let capture = seen.clone();
                let expected = bytes.clone();
                let echo = if mode == "full" {
                    encoded.clone()
                } else {
                    encoded.chars().take(80).collect()
                };
                let handler = move |Json(body): Json<Value>| {
                    let mut requests = capture.lock().unwrap();
                    let index = requests.len();
                    if index == 0 {
                        assert_eq!(native_bytes(&body, style, audio).unwrap(), expected);
                    } else {
                        assert!(
                            native_bytes(&body, style, audio).is_none(),
                            "typed repair must remove native part"
                        );
                    }
                    requests.push(body);
                    let message = format!(
                        "unsupported {} submitted {echo}",
                        if audio { "input_audio" } else { "PDF document" }
                    );
                    async move {
                        if mode == "sse" {
                            let error = json!({"type":"invalid_request_error","code":"invalid_request_error","message":message});
                            let event = if style == "responses" {
                                json!({"type":"response.failed","response":{"error":error}})
                            } else if style == "anthropic" {
                                json!({"type":"error","error":error})
                            } else {
                                json!({"error":error})
                            };
                            (
                                [("content-type", "text/event-stream")],
                                format!("data: {event}\n\n"),
                            )
                                .into_response()
                        } else {
                            (axum::http::StatusCode::BAD_REQUEST, Json(json!({"error":{"type":"invalid_request_error","message":message}}))).into_response()
                        }
                    }
                };
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let base = format!("http://{}", listener.local_addr().unwrap());
                let app = Router::new()
                    .route("/chat/completions", post(handler.clone()))
                    .route("/responses", post(handler.clone()))
                    .route("/messages", post(handler));
                let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
                let server = tokio::spawn(async move {
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async {
                            let _ = stopped.await;
                        })
                        .await
                        .unwrap();
                });
                let notes = Arc::new(Mutex::new(Vec::<String>::new()));
                let capture_notes = notes.clone();
                let cfg = ProviderCfg {
                    base_url: base,
                    model: format!("privacy-{}", uuid::Uuid::new_v4()),
                    api_style: style.into(),
                    audio_input: Some(true),
                    pdf_input: Some(true),
                    max_retries: 0,
                    fallback_models: vec![],
                    ..Default::default()
                };
                let model = cfg.model.clone();
                let provider =
                    Provider::for_profile_with(cfg, owned.paths.clone(), reqwest::Client::new())
                        .await
                        .unwrap()
                        .with_note_hook(Arc::new(move |note| {
                            capture_notes.lock().unwrap().push(note)
                        }));
                let result = tokio::time::timeout(Duration::from_secs(5), async {
                    if stream {
                        provider
                            .chat_stream(
                                &messages,
                                &[],
                                &model,
                                None,
                                &CancellationToken::new(),
                                |_, _| {},
                            )
                            .await
                            .map(|_| ())
                    } else {
                        provider.complete(&messages, &model, None).await.map(|_| ())
                    }
                })
                .await
                .unwrap();
                let diagnostic = format!("{:#}", result.unwrap_err());
                assert!(
                    diagnostic.contains("diagnostic omitted"),
                    "{style}/{mode}/{stream}: {diagnostic}"
                );
                if mode != "sse" {
                    assert!(diagnostic.contains("400"), "{diagnostic}");
                }
                let displayed = format!("{diagnostic} {:?}", notes.lock().unwrap());
                assert!(!displayed.contains(&encoded));
                assert!(!displayed.contains(&encoded[..32]));
                assert_eq!(
                    seen.lock().unwrap().len(),
                    if mode == "sse" { 1 } else { 2 },
                    "no extra API calls; native repair persists"
                );
                stop.send(()).unwrap();
                tokio::time::timeout(Duration::from_secs(5), server)
                    .await
                    .unwrap()
                    .unwrap();
            }
        }
    }
    owned.close();
}
