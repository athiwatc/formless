//! Name guesses and real owned-image preparation, without contacting a provider.
use super::*;
use crate::{Paths, Store};

fn cases() -> Vec<(&'static str, bool)> {
    vec![
        ("o3-mini", false),
        ("openai/o3-mini-2025-01-31", false),
        ("gemma3:1b", false),
        ("google/gemma-3-270m-it", false),
        ("google/gemma-3-1b-it-qat", false),
        ("anthropic/claude-2.1", false),
        ("claude-instant-1.2", false),
        ("anthropic.claude-v2:1", false),
        ("o1", true),
        ("openai/o1-pro", true),
        ("o1-2024-12-17", true),
        ("o1-mini", false),
        ("o1-preview-2024-09-12", false),
        ("google/gemma-3-4b-it", true),
        ("gemma3:12b", true),
        ("gemma3:27b", true),
        ("claude-3-haiku-20240307", true),
        ("o3", true),
        ("openai/o4-mini", true),
        ("gpt-4o", true),
        ("my-gemini-embed", false),
        ("gemini-embedding-001", false),
        ("my-claude-3-haiku", false),
        // Preserve broad unknown-family fallback without confusing 20 with 2
        // or minimax with mini. These fixture IDs make no availability claim.
        ("claude-20-fixture", true),
        ("openai/o3-minimax-fixture", true),
        ("unknown-text-model", false),
    ]
}

#[test]
fn name_guesses_distinguish_text_only_variants_from_vision_families() {
    for (model, expected) in cases() {
        assert_eq!(is_vision_model(model), expected, "{model}");
        assert_eq!(
            is_vision_model(&model.to_uppercase()),
            expected,
            "uppercase {model}"
        );
    }
}

#[test]
fn explicit_vision_overrides_and_prompt_tool_policy_are_retained() {
    for vision in [false, true] {
        let cfg = ProviderCfg {
            vision: Some(vision),
            ..Default::default()
        };
        let provider = Provider::new(cfg, "fixture-key".into()).unwrap();
        for model in ["o1", "o3-mini", "unknown"] {
            assert_eq!(provider.sends_images(model), vision, "{model}");
        }
    }
    let mut cfg = ProviderCfg {
        vision: Some(true),
        tool_mode: "prompt".into(),
        ..Default::default()
    };
    assert!(!Provider::new(cfg.clone(), "fixture-key".into())
        .unwrap()
        .sends_images("o1"));
    cfg.api_style = "anthropic".into();
    assert!(Provider::new(cfg, "fixture-key".into())
        .unwrap()
        .sends_images("claude-3-haiku"));
}

#[test]
fn per_model_vision_corrections_precede_global_and_family_defaults() {
    let cfg = ProviderCfg {
        vision: Some(true),
        model_meta: Box::new(HashMap::from([
            (
                "gpt-5".into(),
                crate::config::ModelMetaCfg {
                    vision: Some(false),
                    ..Default::default()
                },
            ),
            (
                "custom".into(),
                crate::config::ModelMetaCfg {
                    vision: Some(true),
                    ..Default::default()
                },
            ),
        ])),
        ..Default::default()
    };
    let provider = Provider::new(cfg, "fixture-key".into()).unwrap();
    assert!(!provider.sends_images("gpt-5"));
    assert!(provider.sends_images("custom"));
}

fn image_count(value: &Value) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(matches!(
                object.get("type").and_then(Value::as_str),
                Some("image_url" | "input_image" | "image")
            )) + object.values().map(image_count).sum::<usize>()
        }
        Value::Array(values) => values.iter().map(image_count).sum(),
        _ => 0,
    }
}

#[test]
fn owned_image_projection_keeps_text_only_references_and_sends_o1_parts() {
    struct Owned(std::path::PathBuf);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let owned = Owned(std::env::temp_dir().join(format!("fl-vision-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(owned.0.join("data"), owned.0.join("config")).unwrap();
    let root = paths.data_dir.join("artifacts");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("fixture.png");
    let mut png = Vec::new();
    image::DynamicImage::new_rgb8(8, 8)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    std::fs::write(&file, &png).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let id = store
        .save_artifact(Some(&sid), file.to_str().unwrap(), "", png.len() as i64)
        .unwrap();
    let message = store.append_message_meta(&sid, "user", "inspect the fixture", Some(json!({"attachments":[{"id":id,"path":file,"bytes":png.len(),"mime":"image/png","name":"fixture.png"}]}).to_string())).unwrap();
    let messages = vec![crate::storage::provider_message(&message)];
    let forced = Provider::new(
        ProviderCfg {
            vision: Some(true),
            ..Default::default()
        },
        "fixture-key".into(),
    )
    .unwrap()
    .with_attachment_snapshot(&store, &messages)
    .unwrap();
    assert_eq!(
        image_count(&json!(forced.prepare_attachments(&messages, "fixture"))),
        1,
        "owned fixture must actually encode an image"
    );
    let provider = Provider::new(ProviderCfg::default(), "fixture-key".into())
        .unwrap()
        .with_attachment_snapshot(&store, &messages)
        .unwrap();
    for (model, expected) in cases() {
        let output = provider.prepare_attachments(&messages, model);
        assert_eq!(
            image_count(&json!(output)),
            usize::from(expected),
            "{model}"
        );
        assert!(output.iter().all(|m| m.get("_attachments").is_none()));
        if !expected {
            assert!(
                json!(output).to_string().contains("read_artifact"),
                "reference retained for {model}"
            );
        }
    }
}
