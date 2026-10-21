//! Attachments (SPEC §5.6): files stored as Session artifacts, referenced
//! in the next user message, images inlined only for vision models.

use base64::Engine;
use formless_core::config::ProviderCfg;
use formless_core::provider::{is_vision_model, Provider};
use formless_core::{Paths, Store};
use formless_protocol::{Request, SendMode};
use std::sync::Arc;

#[test]
fn vision_models_are_guessed_from_names() {
    for m in [
        "gpt-4o-mini",
        "claude-sonnet-4",
        "gemini-2.5-pro",
        "qwen2.5-vl-7b",
        "llava:13b",
    ] {
        assert!(is_vision_model(m), "{m}");
    }
    for m in ["gpt-3.5-turbo", "qwen2.5-coder:7b", "deepseek-r1"] {
        assert!(!is_vision_model(m), "{m}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_ride_on_the_next_message() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-att-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-att-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let png = daemon
        .handle(Request::AttachFile {
            session_id: sid.clone(),
            name: "/some/dir/shot.png".into(),
            data_b64: b64(b"\x89PNGfake"),
            mime: None,
        })
        .await
        .unwrap();
    assert_eq!(
        (png["name"].as_str(), png["mime"].as_str()),
        (Some("shot.png"), Some("image/png"))
    );
    daemon
        .handle(Request::AttachFile {
            session_id: sid.clone(),
            name: "notes.txt".into(),
            data_b64: b64(b"hello notes"),
            mime: None,
        })
        .await
        .unwrap();
    assert!(daemon
        .handle(Request::AttachFile {
            session_id: sid.clone(),
            name: "x".into(),
            data_b64: "not base64!".into(),
            mime: None,
        })
        .await
        .is_err());

    daemon
        .handle(Request::SendMessage {
            session_id: sid.clone(),
            content: "what is this?".into(),
            mode: SendMode::Send,
        })
        .await
        .unwrap();
    let msg = store
        .list_messages(&sid)
        .unwrap()
        .into_iter()
        .find(|m| m.role == "user")
        .unwrap();
    let pm = formless_core::storage::provider_message(&msg);
    let text = pm["content"].as_str().unwrap();
    assert!(text.starts_with("what is this?"));
    assert!(
        text.contains("[attached: shot.png (image/png, 8 bytes)"),
        "{text}"
    );
    assert!(
        text.contains("[attached: notes.txt (text/plain, 11 bytes)"),
        "{text}"
    );
    assert_eq!(
        pm["_attachments"].as_array().unwrap().len(),
        1,
        "only images get a marker"
    );

    // The text attachment is readable by the agent as a Session artifact.
    let id = png["id"].as_str().unwrap();
    let read = daemon
        .call_tool_direct(&sid, "read_artifact", serde_json::json!({"id": id}))
        .await;
    assert!(read.contains("PNGfake"), "{read}");

    // Vision model: an image part; other models: the marker is dropped.
    let provider = |vision: Option<bool>| {
        Provider::new(
            ProviderCfg {
                base_url: "http://x".into(),
                vision,
                ..Default::default()
            },
            String::new(),
        )
        .unwrap()
    };
    let out = provider(None).prepare_attachments(std::slice::from_ref(&pm), "gpt-4o");
    let parts = out[0]["content"].as_array().unwrap();
    assert_eq!(parts[0]["type"], "text");
    assert!(parts[1]["image_url"]["url"]
        .as_str()
        .unwrap()
        .starts_with("data:image/png;base64,"));
    assert!(out[0].get("_attachments").is_none());
    let plain = provider(None).prepare_attachments(std::slice::from_ref(&pm), "gpt-3.5-turbo");
    assert!(plain[0]["content"].is_string() && plain[0].get("_attachments").is_none());
    let forced = provider(Some(false)).prepare_attachments(std::slice::from_ref(&pm), "gpt-4o");
    assert!(forced[0]["content"].is_string());
}
