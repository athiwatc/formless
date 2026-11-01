//! Imported metadata is not authority to read a host file. Exercise the real
//! import/storage projection and the three provider transports without providers.
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
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::DynamicImage::new_rgb8(8, 8)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    // A valid image with a private trailer would be sent verbatim by the old
    // non-resizing encoder. A positive oracle against accidental file reads.
    bytes.extend_from_slice(b"PRIVATE_SENTINEL_not_for_the_provider_617");
    bytes
}
fn register(st: &Store, path: &Path, bytes: &[u8]) -> Value {
    let id = st
        .save_artifact(None, path.to_str().unwrap(), "", bytes.len() as i64)
        .unwrap();
    json!({"id":id,"path":path,"bytes":bytes.len(),"mime":"image/png","name":"picture.png"})
}
async fn import(daemon: &Arc<Daemon>, attachment: &Value) -> String {
    let meta = json!({"attachments":[attachment]}).to_string();
    let result=daemon.handle(Request::ImportSession {bundle:json!({"format":"formless.session/1","name":"untrusted import","messages":[{"role":"user","content":"inspect attachment","meta":meta}]})}).await.unwrap();
    result["id"].as_str().unwrap().into()
}
fn messages(st: &Store, sid: &str) -> Vec<Value> {
    st.list_messages(sid)
        .unwrap()
        .iter()
        .map(provider_message)
        .collect()
}
fn image_count(value: &Value) -> usize {
    match value {
        Value::Object(o) => {
            usize::from(matches!(
                o.get("type").and_then(Value::as_str),
                Some("image_url" | "input_image" | "image")
            )) + o.values().map(image_count).sum::<usize>()
        }
        Value::Array(a) => a.iter().map(image_count).sum(),
        _ => 0,
    }
}
async fn wire(
    provider: &Provider,
    input: &[Value],
    stream: bool,
    seen: &Arc<Mutex<Vec<Value>>>,
) -> Value {
    let before = seen.lock().unwrap().len();
    tokio::time::timeout(Duration::from_secs(5), async {
        if stream {
            provider
                .chat_stream(
                    input,
                    &[],
                    "fixture",
                    None,
                    &CancellationToken::new(),
                    |_, _| {},
                )
                .await
                .map(|_| ())
        } else {
            provider.complete(input, "fixture", None).await.map(|_| ())
        }
    })
    .await
    .expect("private wire request deadline")
    .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), before + 1, "no extra probe or file API calls");
    seen.last().unwrap().clone()
}

#[tokio::test]
async fn imported_paths_cannot_cross_artifact_authority_on_any_api() {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "fixture-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for key in ["FORMLESS_MODEL", "FORMLESS_BASE_URL", "FORMLESS_SECURE"] {
        env.remove(key);
    }
    let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
    let capture = seen.clone();
    let handler = move |OriginalUri(uri): OriginalUri, Json(body): Json<Value>| {
        capture.lock().unwrap().push(body.clone());
        async move {
            let native = uri.path() == "/messages";
            let responses = uri.path() == "/responses";
            if body["stream"] == true {
                let events = if native {
                    vec![
                        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}),
                        json!({"type":"content_block_stop","index":0}),
                        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
                        json!({"type":"message_stop"}),
                    ]
                } else if responses {
                    vec![
                        json!({"type":"response.output_text.delta","delta":"ok"}),
                        json!({"type":"response.completed","response":{"status":"completed"}}),
                    ]
                } else {
                    vec![json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]})]
                };
                let mut text = events
                    .into_iter()
                    .map(|v| format!("data: {v}\n\n"))
                    .collect::<String>();
                if !native && !responses {
                    text.push_str("data: [DONE]\n\n");
                }
                return ([("content-type", "text/event-stream")], text).into_response();
            }
            Json(if native {json!({"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn"})}
            else if responses {json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}]})}
            else {json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]})}).into_response()
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
    let root =
        std::env::temp_dir().join(format!("fl-attachment-authority-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut config = Config::default();
    config.naming.enabled = false;
    config.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let st = Store::open(&paths.db_file()).unwrap();
    let artifacts = paths.data_dir.join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let bytes = png();
    let outside = root.join("private.png");
    std::fs::write(&outside, &bytes).unwrap();
    // A record alone is insufficient: this models imported/foreign restored DBs.
    let foreign = register(&st, &outside, &bytes);
    let sid = import(&daemon, &foreign).await;
    let stored_before = st.list_messages(&sid).unwrap()[0].meta.clone();
    let foreign_messages = messages(&st, &sid);
    let mut denied = vec![foreign_messages.clone()];
    #[cfg(unix)]
    {
        let link = artifacts.join("link.png");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let a = register(&st, &link, &bytes);
        let sid = import(&daemon, &a).await;
        denied.push(messages(&st, &sid));
        let directory = artifacts.join("indirect");
        std::os::unix::fs::symlink(&root, &directory).unwrap();
        let a = register(&st, &directory.join("private.png"), &bytes);
        let sid = import(&daemon, &a).await;
        denied.push(messages(&st, &sid));
    }
    let invalid = artifacts.join("not-image.png");
    let invalid_bytes = b"\x89PNG\r\n\x1a\nPRIVATE_SENTINEL_invalid_image";
    std::fs::write(&invalid, invalid_bytes).unwrap();
    let a = register(&st, &invalid, invalid_bytes);
    let invalid_sid = import(&daemon, &a).await;
    denied.push(messages(&st, &invalid_sid));
    // Genuine intake stores owned bytes. Persist a sent attachment as the
    // daemon does, then fork: the artifact's original Session is not the fork.
    let original = st.create_session(None).unwrap().id;
    let attached = daemon
        .handle(Request::AttachFile {
            session_id: original.clone(),
            name: "owned.png".into(),
            data_b64: STANDARD.encode(&bytes),
            mime: Some("image/png".into()),
        })
        .await
        .unwrap();
    let held = st.pending_attachments(&original).unwrap();
    assert_eq!(held.len(), 1, "{attached}");
    st.append_message_meta(
        &original,
        "user",
        "owned image",
        Some(json!({"attachments":held}).to_string()),
    )
    .unwrap();
    let fork = daemon
        .handle(Request::ForkSession {
            id: original.clone(),
            name: None,
            up_to_seq: None,
        })
        .await
        .unwrap();
    let fork = fork["id"].as_str().unwrap();
    let positives = [messages(&st, &original), messages(&st, fork)];
    let mut bad_hash = held[0].clone();
    bad_hash["sha256"] = json!("00");
    let bad_sid = import(&daemon, &bad_hash).await;
    denied.push(messages(&st, &bad_sid));
    let mut bad_mime = held[0].clone();
    bad_mime["mime"] = json!("image/jpeg");
    let bad_sid = import(&daemon, &bad_mime).await;
    denied.push(messages(&st, &bad_sid));
    let mut missing_record = held[0].clone();
    missing_record["id"] = json!(uuid::Uuid::new_v4().to_string());
    let bad_sid = import(&daemon, &missing_record).await;
    denied.push(messages(&st, &bad_sid));
    // Database restore retains registry identity. Same-profile owned files
    // survive; foreign recorded paths remain references and gain no authority.
    let backup = root.join("restore-source.db");
    st.backup_into(&backup).unwrap();
    drop(st);
    drop(daemon);
    formless_core::housekeeping::restore(&paths, &backup).unwrap();
    let st = Store::open(&paths.db_file()).unwrap();
    assert_eq!(messages(&st, &original), positives[0]);
    assert_eq!(messages(&st, fork), positives[1]);
    for style in ["chat", "responses", "anthropic"] {
        let cfg = ProviderCfg {
            base_url: base.clone(),
            model: "fixture".into(),
            api_style: style.into(),
            vision: Some(true),
            max_retries: 0,
            fallback_models: vec![],
            ..Default::default()
        };
        let p = Provider::for_profile_with(cfg.clone(), paths.clone(), reqwest::Client::new())
            .await
            .unwrap();
        for stream in [false, true] {
            for input in &denied {
                let body = wire(&p, input, stream, &seen).await;
                assert_eq!(image_count(&body), 0, "{style} stream={stream}: {body}");
                let text = body.to_string();
                assert!(text.contains("could not be attached"), "{text}");
                assert!(
                    !text.contains("PRIVATE_SENTINEL")
                        && !text.contains(&STANDARD.encode(&bytes))
                        && !text.contains(&STANDARD.encode(invalid_bytes)),
                    "private bytes leaked"
                );
                assert!(
                    text.contains("read_artifact"),
                    "artifact reference must remain"
                );
            }
            for input in &positives {
                let body = wire(&p, input, stream, &seen).await;
                assert_eq!(image_count(&body), 1, "{style} stream={stream}: {body}");
            }
            let key_only = Provider::new(cfg.clone(), "fixture-key".into()).unwrap();
            let body = wire(&key_only, &positives[0], stream, &seen).await;
            assert_eq!(
                image_count(&body),
                0,
                "public constructor has no file authority"
            );
        }
        let text = json!({"role":"user","content":json!({"_attachments":[foreign]}).to_string()});
        assert_eq!(
            image_count(&wire(&p, &[text], false, &seen).await),
            0,
            "JSON in a prompt remains text"
        );
    }
    assert_eq!(
        st.list_messages(&sid).unwrap()[0].meta,
        stored_before,
        "projection must not rewrite imported source"
    );
    drop(st);
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
