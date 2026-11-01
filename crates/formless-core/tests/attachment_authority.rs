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
fn register(st: &Store, owner: &str, path: &Path, bytes: &[u8]) -> Value {
    let id = st
        .save_artifact(Some(owner), path.to_str().unwrap(), "", bytes.len() as i64)
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
fn legacy_message(st: &Store, scope: &str, attachment: &Value) -> Vec<Value> {
    let message = st
        .append_message_meta(
            scope,
            "user",
            "inspect attachment",
            Some(json!({"attachments":[attachment]}).to_string()),
        )
        .unwrap();
    vec![provider_message(&message)]
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

#[test]
fn raw_message_projection_never_tests_untrusted_path_existence() {
    let root = std::env::temp_dir().join(format!("fl-projection-stat-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let st = Store::open_in_memory().unwrap();
    let sid = st.create_session(None).unwrap().id;
    let present = root.join("private");
    std::fs::write(&present, b"private").unwrap();
    let absent = root.join("absent");
    let mut content = None;
    for path in [
        present,
        absent,
        std::path::PathBuf::from(r"\\untrusted-server\share\private"),
    ] {
        let a = json!({"id":"untrusted","path":path,"name":"same.png","mime":"image/png","bytes":7,"_source_session":"forged","_source_seq":-1});
        let message = legacy_message(&st, &sid, &a).pop().unwrap();
        assert_eq!(message["_attachments"][0]["_source_session"], json!(sid));
        assert!(message["_attachments"][0]["_source_seq"].as_i64().unwrap() > 0);
        assert!(!message["content"].as_str().unwrap().contains("retention"));
        if let Some(previous) = &content {
            assert_eq!(&message["content"], previous);
        } else {
            content = Some(message["content"].clone());
        }
    }
    drop(st);
    std::fs::remove_dir_all(root).unwrap();
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
    let legacy_scope = st.create_session(None).unwrap().id;
    let foreign = register(&st, &legacy_scope, &outside, &bytes);
    let sid = import(&daemon, &foreign).await;
    let stored_before = st.list_messages(&sid).unwrap()[0].meta.clone();
    let foreign_messages = messages(&st, &sid);
    assert!(foreign_messages[0]["_attachments"].is_null());
    assert!(foreign_messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("re-attach"));
    let mut inert_imports = vec![foreign_messages.clone()];
    // Raw legacy rows bypass NEW-import sanitization and exercise the actual
    // authority guard with matching owner, registry ID, path, size and MIME.
    let mut denied = vec![legacy_message(&st, &legacy_scope, &foreign)];
    #[cfg(unix)]
    {
        let link = artifacts.join("link.png");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let a = register(&st, &legacy_scope, &link, &bytes);
        denied.push(legacy_message(&st, &legacy_scope, &a));
        let directory = artifacts.join("indirect");
        std::os::unix::fs::symlink(&root, &directory).unwrap();
        let a = register(&st, &legacy_scope, &directory.join("private.png"), &bytes);
        denied.push(legacy_message(&st, &legacy_scope, &a));
    }
    let invalid = artifacts.join("not-image.png");
    let invalid_bytes = b"\x89PNG\r\n\x1a\nPRIVATE_SENTINEL_invalid_image";
    std::fs::write(&invalid, invalid_bytes).unwrap();
    let a = register(&st, &legacy_scope, &invalid, invalid_bytes);
    denied.push(legacy_message(&st, &legacy_scope, &a));
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
    let verified_for_legacy = register(
        &st,
        &legacy_scope,
        Path::new(held[0]["path"].as_str().unwrap()),
        &bytes,
    );
    let mut bad_hash = verified_for_legacy.clone();
    bad_hash["sha256"] = json!("00");
    denied.push(legacy_message(&st, &legacy_scope, &bad_hash));
    let mut bad_mime = verified_for_legacy.clone();
    bad_mime["mime"] = json!("image/jpeg");
    denied.push(legacy_message(&st, &legacy_scope, &bad_mime));
    let mut missing_record = verified_for_legacy.clone();
    missing_record["id"] = json!(uuid::Uuid::new_v4().to_string());
    denied.push(legacy_message(&st, &legacy_scope, &missing_record));
    // Knowing another Session's real ID/path is insufficient, including when
    // malicious legacy metadata tries to supply its own reserved authority.
    let mut cross_session = held[0].clone();
    cross_session["_source_session"] = json!(original);
    cross_session["_source_seq"] = json!(1);
    denied.push(legacy_message(&st, &legacy_scope, &cross_session));
    let imported = import(&daemon, &cross_session).await;
    let imported_meta: Value = serde_json::from_str(
        st.list_messages(&imported).unwrap()[0]
            .meta
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    for field in [
        "id",
        "_artifact_id",
        "sha256",
        "path",
        "_source_session",
        "_source_seq",
    ] {
        assert!(
            imported_meta["attachments"][0].get(field).is_none(),
            "{field}: {imported_meta}"
        );
    }
    inert_imports.push(messages(&st, &imported));
    // A copied direct-parent edge remains sufficient when that parent row is
    // no longer present. This does not delete the separately registered file.
    let detached = st.create_session(None).unwrap().id;
    let a = register(
        &st,
        &detached,
        Path::new(held[0]["path"].as_str().unwrap()),
        &bytes,
    );
    legacy_message(&st, &detached, &a);
    let detached_fork = st.fork_session(&detached, None, None).unwrap().unwrap().id;
    st.delete_session(&detached).unwrap();
    let deleted_parent_positive = messages(&st, &detached_fork);
    let aggregate = st.create_session(None).unwrap().id;
    let aggregate_artifact = register(
        &st,
        &aggregate,
        Path::new(held[0]["path"].as_str().unwrap()),
        &bytes,
    );
    for count in [8, 20, 1] {
        st.append_message_meta(
            &aggregate,
            "user",
            "repeated image references",
            Some(json!({"attachments":vec![aggregate_artifact.clone();count]}).to_string()),
        )
        .unwrap();
    }
    let repeated = messages(&st, &aggregate);
    let mut invalid_attempt = aggregate_artifact.clone();
    invalid_attempt["sha256"] = json!("00");
    let mut attempts = vec![invalid_attempt; 16];
    attempts.push(aggregate_artifact.clone());
    let invalid_attempts = vec![provider_message(
        &st.append_message_meta(
            &aggregate,
            "user",
            "failed attempts count",
            Some(json!({"attachments":attempts}).to_string()),
        )
        .unwrap(),
    )];
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
            let body = wire(&p, &repeated, stream, &seen).await;
            assert_eq!(
                image_count(&body),
                16,
                "budget is shared across messages and warm/cold cache: {style}"
            );
            assert!(body
                .to_string()
                .contains("attempted native-attachment limit"));
            assert!(body.to_string().contains("read_artifact"));
            assert!(body.to_string().len() < 24 * 1024 * 1024);
            let failed = wire(&p, &invalid_attempts, stream, &seen).await;
            assert_eq!(
                image_count(&failed),
                0,
                "failed native references consume attempted count"
            );
            assert_eq!(
                failed.to_string().matches("could not be attached").count(),
                1,
                "one bounded omission note for the entire message"
            );
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
            for input in positives
                .iter()
                .chain(std::iter::once(&deleted_parent_positive))
            {
                let body = wire(&p, input, stream, &seen).await;
                assert_eq!(image_count(&body), 1, "{style} stream={stream}: {body}");
            }
            for input in &inert_imports {
                let body = wire(&p, input, stream, &seen).await;
                assert_eq!(image_count(&body), 0);
                assert!(body.to_string().contains("re-attach"));
                assert!(!body.to_string().contains(&STANDARD.encode(&bytes)));
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
