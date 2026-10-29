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
    // One taken back before sending doesn't go.
    let dropped = daemon
        .handle(Request::AttachFile {
            session_id: sid.clone(),
            name: "oops.txt".into(),
            data_b64: b64(b"wrong file"),
            mime: None,
        })
        .await
        .unwrap();
    let r = daemon
        .handle(Request::DetachFile {
            session_id: sid.clone(),
            id: dropped["id"].as_str().unwrap().into(),
        })
        .await
        .unwrap();
    assert_eq!(r["removed"], true);

    daemon
        .handle(Request::SendMessage {
            session_id: sid.clone(),
            content: "what is this?".into(),
            mode: SendMode::Send,
            client_msg_id: None,
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
    assert!(!text.contains("oops.txt"), "{text}");
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

#[test]
fn artifact_refs_are_found_in_tool_results() {
    use formless_protocol::artifact_refs;
    let id = "0b9c4a6e-1f2d-4c3b-9a8e-7d6c5b4a3f21";
    let note = format!(
        "preview…\n[full output (90000 bytes) stored as artifact {id}; read it with read_artifact]"
    );
    assert_eq!(artifact_refs(&note), vec![id.to_string()]);
    let emitted = format!("{{\"result\":1,\"emitted_artifact\":\"{id}\",\"emitted_bytes\":5}}");
    assert_eq!(artifact_refs(&emitted), vec![id.to_string()]);
    assert!(artifact_refs("stored as artifact nope").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn get_artifact_reads_slices_and_never_the_path() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-art-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-art-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let att = daemon
        .handle(Request::AttachFile {
            session_id: sid.clone(),
            name: "notes.txt".into(),
            data_b64: base64::engine::general_purpose::STANDARD.encode("abcdefghij"),
            mime: None,
        })
        .await
        .unwrap();
    let id = att["id"].as_str().unwrap().to_string();
    let r = daemon
        .handle(Request::GetArtifact {
            id: id.clone(),
            offset: Some(2),
            length: Some(4),
        })
        .await
        .unwrap();
    assert_eq!(
        (
            r["text"].as_str(),
            r["end"].as_u64(),
            r["total_chars"].as_u64()
        ),
        (Some("cdef"), Some(6), Some(10))
    );
    let all = r.to_string();
    assert!(
        !all.contains(&paths.data_dir.display().to_string()),
        "no path: {all}"
    );
    assert!(daemon
        .handle(Request::GetArtifact {
            id: "missing".into(),
            offset: None,
            length: None
        })
        .await
        .is_err());
}
