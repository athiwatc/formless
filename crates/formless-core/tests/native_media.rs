//! Native media contracts against owned loopback peers, never providers.
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
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
fn wav() -> Vec<u8> {
    let mut v = b"RIFF".to_vec();
    v.extend(40u32.to_le_bytes());
    v.extend(b"WAVEfmt ");
    v.extend(16u32.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(16000u32.to_le_bytes());
    v.extend(32000u32.to_le_bytes());
    v.extend(2u16.to_le_bytes());
    v.extend(16u16.to_le_bytes());
    v.extend(b"data");
    v.extend(4u32.to_le_bytes());
    v.extend([0, 0, 1, 0]);
    v
}
fn parts<'a>(v: &'a Value, kind: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some(kind) {
                out.push(v);
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
#[tokio::test]
async fn owned_audio_pdf_reach_exact_supported_api_parts_streaming_and_nonstreaming() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-fixture-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for key in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
        env.remove(key);
    }
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = seen.clone();
    let handler = move |OriginalUri(uri): OriginalUri, Json(body): Json<Value>| {
        capture.lock().unwrap().push(body.clone());
        async move {
            let a = uri.path() == "/messages";
            let r = uri.path() == "/responses";
            if body["stream"] == true {
                let events = if a {
                    vec![
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
                        json!({"type":"message_stop"}),
                    ]
                } else if r {
                    vec![
                        json!({"type":"response.output_text.delta","delta":"ok"}),
                        json!({"type":"response.completed","response":{"status":"completed"}}),
                    ]
                } else {
                    vec![json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]})]
                };
                let mut text = events
                    .into_iter()
                    .map(|e| format!("data: {e}\n\n"))
                    .collect::<String>();
                if !a && !r {
                    text.push_str("data: [DONE]\n\n");
                }
                return ([("content-type", "text/event-stream")], text).into_response();
            }
            Json(if a {json!({"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"})}else if r {json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}]})}else {json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]})}).into_response()
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
    let root = std::env::temp_dir().join(format!("fl-native-media-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut c = Config::default();
    c.naming.enabled = false;
    c.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let st = Store::open(&paths.db_file()).unwrap();
    let sid = st.create_session(None).unwrap().id;
    let audio = wav();
    let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
    for (name, bytes, mime) in [
        ("clip.wav", audio.as_slice(), "audio/wav"),
        ("document.pdf", pdf.as_slice(), "application/pdf"),
    ] {
        daemon
            .handle(Request::AttachFile {
                session_id: sid.clone(),
                name: name.into(),
                data_b64: STANDARD.encode(bytes),
                mime: Some(mime.into()),
            })
            .await
            .unwrap();
    }
    let held = st.pending_attachments(&sid).unwrap();
    assert_eq!(held.len(), 2);
    let message = st
        .append_message_meta(
            &sid,
            "user",
            "analyze files",
            Some(json!({"attachments":held}).to_string()),
        )
        .unwrap();
    let messages = [provider_message(&message)];
    for style in ["chat", "responses", "anthropic"] {
        let cfg = ProviderCfg {
            base_url: base.clone(),
            model: "fixture".into(),
            api_style: style.into(),
            audio_input: Some(true),
            pdf_input: Some(true),
            max_retries: 0,
            fallback_models: vec![],
            ..Default::default()
        };
        let p = Provider::for_profile_with(cfg, paths.clone(), reqwest::Client::new())
            .await
            .unwrap();
        for stream in [false, true] {
            let before = seen.lock().unwrap().len();
            tokio::time::timeout(Duration::from_secs(5), async {
                if stream {
                    p.chat_stream(
                        &messages,
                        &[],
                        "fixture",
                        None,
                        &CancellationToken::new(),
                        |_, _| {},
                    )
                    .await
                    .map(|_| ())
                } else {
                    p.complete(&messages, "fixture", None).await.map(|_| ())
                }
            })
            .await
            .unwrap()
            .unwrap();
            let captured = seen.lock().unwrap();
            assert_eq!(
                captured.len(),
                before + 1,
                "no file/probe/transcription requests"
            );
            let body = captured.last().unwrap();
            let text = body.to_string();
            assert!(
                !text.contains("_attachments")
                    && !text.contains("_source_session")
                    && !text.contains(paths.data_dir.to_str().unwrap())
            );
            let mut found = Vec::new();
            parts(
                body,
                if style == "responses" {
                    "input_file"
                } else if style == "anthropic" {
                    "document"
                } else {
                    "file"
                },
                &mut found,
            );
            assert_eq!(found.len(), 1, "{body}");
            let file = found[0];
            let raw = if style == "anthropic" {
                assert_eq!(file["source"]["media_type"], "application/pdf");
                file["source"]["data"].as_str().unwrap()
            } else if style == "responses" {
                assert_eq!(file["filename"], "document.pdf");
                file["file_data"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("data:application/pdf;base64,")
                    .unwrap()
            } else {
                assert_eq!(file["file"]["filename"], "document.pdf");
                file["file"]["file_data"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("data:application/pdf;base64,")
                    .unwrap()
            };
            assert_eq!(STANDARD.decode(raw).unwrap(), pdf);
            let mut found = Vec::new();
            parts(body, "input_audio", &mut found);
            if style == "chat" {
                assert_eq!(found.len(), 1);
                assert_eq!(found[0]["input_audio"]["format"], "wav");
                assert_eq!(
                    STANDARD
                        .decode(found[0]["input_audio"]["data"].as_str().unwrap())
                        .unwrap(),
                    audio
                );
            } else {
                assert!(found.is_empty());
                assert!(text.contains("artifact reference retained"));
            }
            assert!(text.contains("read_artifact"));
        }
    }
    drop(st);
    drop(daemon);
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
