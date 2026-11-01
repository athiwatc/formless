//! Local prompt estimates. These are calibrated heuristics, never a tokenizer.
use crate::config::ProviderCfg;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const MIN_RATIO: f64 = 0.25;
pub const MAX_RATIO: f64 = 8.0;
pub const ALPHA: f64 = 0.2;

pub fn text(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

/// Provider names alone are insufficient when an endpoint is replaced. Hash
/// the endpoint/API identity so credentials in a URL never enter the table.
pub fn provider_key(name: &str, cfg: &ProviderCfg) -> String {
    format!(
        "{name}:{:x}",
        Sha256::digest(
            format!("{}\0{}", cfg.base_url.trim_end_matches('/'), cfg.api_style).as_bytes()
        )
    )
}

pub fn calibrated(raw: usize, ratio: f64) -> usize {
    let ratio = if ratio.is_finite() {
        ratio.clamp(MIN_RATIO, MAX_RATIO)
    } else {
        1.0
    };
    (raw as f64 * ratio).ceil().min(usize::MAX as f64) as usize
}

/// Dimension/detail heuristic. Auto and unknown detail use the conservative
/// high-detail estimate. Dimensions are fitted to a bounded image tile grid.
pub fn image_tokens(dimensions: Option<(u32, u32)>, detail: &str) -> usize {
    if detail == "low" {
        return 85;
    }
    let Some((width, height)) = dimensions.filter(|(w, h)| *w > 0 && *h > 0) else {
        return 1105;
    };
    let mut w = width as f64;
    let mut h = height as f64;
    let scale = (2048.0 / w.max(h)).min(1.0);
    w *= scale;
    h *= scale;
    let scale = (768.0 / w.min(h)).min(1.0);
    w *= scale;
    h *= scale;
    85 + 170 * ((w / 512.0).ceil() as usize) * ((h / 512.0).ceil() as usize)
}

fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn url_dimensions(url: &str) -> Option<(u32, u32)> {
    use base64::Engine;
    let (header, data) = url.split_once(',')?;
    if !header.starts_with("data:image/")
        || !header.ends_with(";base64")
        || data.len() > 32 * 1024 * 1024
    {
        return None;
    }
    dimensions(
        &base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?,
    )
}

/// PDF page expansion and audio tokenization cannot be inferred from base64.
/// Keep their cost explicitly unknown and do not train text-only calibration.
pub fn has_native_media(value: &Value) -> bool {
    match value {
        Value::Object(o) => {
            matches!(
                o.get("type").and_then(Value::as_str),
                Some("input_audio" | "file" | "input_file" | "document")
            ) || o.values().any(has_native_media)
        }
        Value::Array(a) => a.iter().any(has_native_media),
        _ => false,
    }
}

/// Replace image payloads with a bounded cost before serializing JSON. All
/// other keys/values (not just visible content) contribute to the estimate.
fn scrub_images(value: &Value, images: &mut usize) -> Value {
    if let Some(obj) = value.as_object() {
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or_default();
        if matches!(kind, "input_audio" | "file" | "input_file" | "document") {
            // A bounded structural marker is a text-estimate lower bound,
            // never a claim that the native modality has zero provider tokens.
            return json!({"type":kind,"native_tokens":"unknown"});
        }
        if kind == "image_url" || kind == "input_image" || kind == "image" {
            let image = value.get("image_url").unwrap_or(value);
            let detail = image
                .get("detail")
                .and_then(Value::as_str)
                .or_else(|| value.get("detail").and_then(Value::as_str))
                .unwrap_or("auto");
            let url = image
                .as_str()
                .or_else(|| image.get("url").and_then(Value::as_str))
                .or_else(|| value.get("image_url").and_then(Value::as_str));
            let dims = url.and_then(url_dimensions).or_else(|| {
                use base64::Engine;
                let data = value["source"]["data"].as_str()?;
                if data.len() > 32 * 1024 * 1024 {
                    return None;
                }
                dimensions(
                    &base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .ok()?,
                )
            });
            *images = images.saturating_add(image_tokens(dims, detail));
            return json!({"type": "image"});
        }
        let mut out = serde_json::Map::new();
        for (key, value) in obj {
            if key == "_attachments" {
                for attachment in value.as_array().into_iter().flatten() {
                    if !attachment["mime"]
                        .as_str()
                        .is_some_and(crate::daemon::is_raster_image)
                    {
                        continue;
                    }
                    // Raw metadata (including imported Session metadata) is
                    // never filesystem authority. Final prepared image parts
                    // carry verified dimensions; otherwise use an honest
                    // nonzero unknown-dimension estimate, without opening paths.
                    let dims = attachment["width"]
                        .as_u64()
                        .zip(attachment["height"].as_u64())
                        .and_then(|(w, h)| Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?)));
                    *images = images.saturating_add(image_tokens(
                        dims,
                        attachment["detail"].as_str().unwrap_or("auto"),
                    ));
                }
            } else if key != "cache_control" {
                out.insert(key.clone(), scrub_images(value, images));
            }
        }
        Value::Object(out)
    } else if let Some(values) = value.as_array() {
        Value::Array(values.iter().map(|v| scrub_images(v, images)).collect())
    } else {
        value.clone()
    }
}

pub fn value(value: &Value) -> usize {
    let mut images = 0;
    let scrubbed = scrub_images(value, &mut images);
    text(&scrubbed.to_string()).saturating_add(images)
}

/// Only fields that carry prompt content, after endpoint conversion, repairs
/// and optional-body changes. Output caps and transport flags are not input.
pub fn request(body: &Value) -> usize {
    ["system", "messages", "input", "tools"]
        .into_iter()
        .filter_map(|key| body.get(key))
        .filter(|v| !v.is_null() && !v.as_array().is_some_and(Vec::is_empty))
        .map(value)
        .fold(0usize, usize::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_json_metadata_and_images_share_one_counter() {
        assert_eq!(text("你好โลก"), "你好โลก".chars().count().div_ceil(4));
        let content = json!({"role":"assistant", "content":"你好", "tool_calls":[{"function":{"name":"測定", "arguments":"{\"x\":2}"}}]});
        assert_eq!(value(&content), text(&content.to_string()));
        assert!(value(&content) > value(&json!({"role":"assistant","content":"你好"})));
        let small = image_tokens(Some((256, 256)), "high");
        assert!(image_tokens(Some((2048, 2048)), "high") > small);
        assert!(small > image_tokens(Some((256, 256)), "low"));
        assert!(image_tokens(None, "auto") > 0);
        let part = |payload: &str| json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{payload}")}});
        assert_eq!(value(&part("broken")), value(&part(&"A".repeat(100_000))));
    }
    #[test]
    fn ratios_are_bounded_and_provider_identity_has_no_url_secrets() {
        assert_eq!(calibrated(100, 1000.0), 800);
        assert_eq!(calibrated(100, 0.0), 25);
        assert_eq!(calibrated(100, f64::NAN), 100);
        let mut cfg = ProviderCfg::default();
        cfg.base_url = "https://user:secret@example.test/v1".into();
        let a = provider_key("default", &cfg);
        assert!(!a.contains("secret"));
        cfg.base_url.push_str("/other");
        assert_ne!(a, provider_key("default", &cfg));
    }
}

#[cfg(test)]
mod calibration_tests {
    use super::*;
    use crate::storage::{ContextCache, Store};

    #[test]
    fn observations_reduce_error_persist_and_remain_provider_model_specific() {
        let root =
            std::env::temp_dir().join(format!("fl-token-calibration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("store.db");
        let raw = text("你好 code {\"arguments\":123}".repeat(20).as_str());
        let actual = raw as u64 * 3;
        {
            let store = Store::open(&path).unwrap();
            let before = calibrated(raw, store.token_ratio("one", "model").unwrap())
                .abs_diff(actual as usize);
            for _ in 0..12 {
                store
                    .observe_prompt_tokens("one", "model", raw, actual)
                    .unwrap();
            }
            let after = calibrated(raw, store.token_ratio("one", "model").unwrap())
                .abs_diff(actual as usize);
            assert!(after < before / 8, "{before} -> {after}");
            assert_eq!(store.token_ratio("two", "model").unwrap(), 1.0);
            assert_eq!(store.token_ratio("one", "other").unwrap(), 1.0);
            let ratio = store.token_ratio("one", "model").unwrap();
            store.observe_prompt_tokens("one", "model", 0, 200).unwrap();
            store.observe_prompt_tokens("one", "model", raw, 0).unwrap();
            assert_eq!(store.token_ratio("one", "model").unwrap(), ratio);
            for _ in 0..50 {
                store
                    .observe_prompt_tokens("clamped", "model", 1, u64::MAX)
                    .unwrap();
            }
            assert!(store.token_ratio("clamped", "model").unwrap() <= MAX_RATIO);
        }
        let store = Store::open(&path).unwrap();
        assert!(store.token_ratio("one", "model").unwrap() > 2.8);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compaction_and_routing_apply_the_calibrated_prompt_not_raw_chars() {
        let store = Store::open_in_memory().unwrap();
        let sid = store.create_session(None).unwrap().id;
        for i in 0..4 {
            store
                .append_message(&sid, "user", &format!("turn{i} {}", "word ".repeat(30)))
                .unwrap();
            store.append_message(&sid, "assistant", "okay").unwrap();
        }
        let mut cache = ContextCache::default();
        let built = cache.build(&store, &sid, "system", 1000, 0.5).unwrap();
        assert!(!cache.with_prompt_tokens(built.clone(), 200, 1000, 0.5).1);
        assert!(
            cache
                .with_prompt_tokens(built.clone(), calibrated(200, 3.0), 1000, 0.5)
                .1
        );
        let raw_over = cache.build(&store, &sid, "system", 100, 0.5).unwrap();
        assert!(raw_over.1);
        assert!(!cache.with_prompt_tokens(raw_over, 25, 100, 0.5).1);
        assert!(!crate::routing::is_complex_tokens(200, 2, 2000));
        assert!(crate::routing::is_complex_tokens(
            calibrated(200, 3.0),
            2,
            2000
        ));
    }
}

#[cfg(test)]
mod image_request_tests {
    use super::*;
    use base64::Engine;
    #[test]
    fn actual_encoded_dimensions_and_detail_contribute_without_base64_text() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1024, 1024)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
        let part = |detail| json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{encoded}"),"detail":detail}});
        let overhead = text(&json!({"type":"image"}).to_string());
        assert_eq!(
            value(&part("low")),
            overhead + image_tokens(Some((1024, 1024)), "low")
        );
        assert_eq!(
            value(&part("high")),
            overhead + image_tokens(Some((1024, 1024)), "high")
        );
        let mut request_body = json!({"messages":[{"role":"user","content":[part("high")]}], "tools":[{"function":{"name":"測定","parameters":{"type":"object"}}}]});
        let expected = request(&request_body);
        request_body["max_tokens"] = json!(100000);
        request_body["temperature"] = json!(0.5);
        request_body["authorization"] = json!("do not estimate credentials");
        assert_eq!(request(&request_body), expected);
    }
}
