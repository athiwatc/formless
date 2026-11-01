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
    format!("{name}:{:x}", Sha256::digest(format!("{}\0{}", cfg.base_url.trim_end_matches('/'), cfg.api_style).as_bytes()))
}

pub fn calibrated(raw: usize, ratio: f64) -> usize {
    let ratio = if ratio.is_finite() { ratio.clamp(MIN_RATIO, MAX_RATIO) } else { 1.0 };
    (raw as f64 * ratio).ceil().min(usize::MAX as f64) as usize
}

/// Dimension/detail heuristic. Auto and unknown detail use the conservative
/// high-detail estimate. Dimensions are fitted to a bounded image tile grid.
pub fn image_tokens(dimensions: Option<(u32, u32)>, detail: &str) -> usize {
    if detail == "low" { return 85; }
    let Some((width, height)) = dimensions.filter(|(w, h)| *w > 0 && *h > 0) else { return 1105; };
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
    image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()
}

fn url_dimensions(url: &str) -> Option<(u32, u32)> {
    use base64::Engine;
    let (header, data) = url.split_once(',')?;
    if !header.starts_with("data:image/") || !header.ends_with(";base64") || data.len() > 32 * 1024 * 1024 { return None; }
    dimensions(&base64::engine::general_purpose::STANDARD.decode(data).ok()?)
}

/// Replace image payloads with a bounded cost before serializing JSON. All
/// other keys/values (not just visible content) contribute to the estimate.
fn scrub_images(value: &Value, images: &mut usize) -> Value {
    if let Some(obj) = value.as_object() {
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind == "image_url" || kind == "input_image" || kind == "image" {
            let image = value.get("image_url").unwrap_or(value);
            let detail = image.get("detail").and_then(Value::as_str).unwrap_or("auto");
            let url = image.as_str().or_else(|| image.get("url").and_then(Value::as_str))
                .or_else(|| value.get("image_url").and_then(Value::as_str));
            let dims = url.and_then(url_dimensions).or_else(|| {
                use base64::Engine;
                let data = value["source"]["data"].as_str()?;
                if data.len() > 32 * 1024 * 1024 { return None; }
                dimensions(&base64::engine::general_purpose::STANDARD.decode(data).ok()?)
            });
            *images = images.saturating_add(image_tokens(dims, detail));
            return json!({"type": "image"});
        }
        let mut out = serde_json::Map::new();
        for (key, value) in obj {
            if key == "_attachments" {
                for attachment in value.as_array().into_iter().flatten() {
                    let dims = attachment["path"].as_str().and_then(|path| {
                        image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_dimensions().ok()
                    });
                    *images = images.saturating_add(image_tokens(dims, attachment["detail"].as_str().unwrap_or("auto")));
                }
            } else if key != "cache_control" {
                out.insert(key.clone(), scrub_images(value, images));
            }
        }
        Value::Object(out)
    } else if let Some(values) = value.as_array() {
        Value::Array(values.iter().map(|v| scrub_images(v, images)).collect())
    } else { value.clone() }
}

pub fn value(value: &Value) -> usize {
    let mut images = 0;
    let scrubbed = scrub_images(value, &mut images);
    text(&scrubbed.to_string()).saturating_add(images)
}

/// Only fields that carry prompt content, after endpoint conversion, repairs
/// and optional-body changes. Output caps and transport flags are not input.
pub fn request(body: &Value) -> usize {
    ["system", "messages", "input", "tools"].into_iter()
        .filter_map(|key| body.get(key))
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
