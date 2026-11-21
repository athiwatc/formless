//! Bounded display metadata only. Never resolves daemon paths or reads file bodies.
use serde_json::Value;

pub const MAX_ENTRIES: usize = 64;
pub const MAX_NAME_BYTES: usize = 512;
pub const MAX_MIME_BYTES: usize = 256;
pub const MAX_META_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    pub reference: bool,
    pub expired: bool,
    pub unavailable: bool,
}

fn text(value: &Value, max: usize) -> Option<&str> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control))
}

fn flag(value: &Value, key: &str) -> Option<bool> {
    value.get(key).map_or(Some(false), Value::as_bool)
}

fn entry(value: &Value) -> Option<Attachment> {
    let name = text(value.get("name")?, MAX_NAME_BYTES)?;
    let name = value
        .get("full_name")
        .and_then(|value| text(value, MAX_NAME_BYTES))
        .unwrap_or(name);
    let mime = value
        .get("mime")
        .map_or(Some("application/octet-stream"), |value| {
            text(value, MAX_MIME_BYTES)
        })?;
    Some(Attachment {
        name: name.into(),
        mime: mime.into(),
        bytes: value.get("bytes").map_or(Some(0), Value::as_u64)?,
        reference: flag(value, "reference")?,
        expired: flag(value, "expired")?,
        unavailable: flag(value, "unavailable")?,
    })
}

pub(crate) fn decode(meta: Option<&str>) -> Vec<Attachment> {
    let Some(meta) = meta.filter(|meta| meta.len() <= MAX_META_BYTES) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(meta) else {
        return Vec::new();
    };
    value["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_ENTRIES)
        .filter_map(entry)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projects_files_images_references_and_expired_or_unavailable_tombstones() {
        let rows = decode(Some(&json!({"attachments":[
            {"name":"image.png","full_name":"original-image.png","mime":"image/png","bytes":12,"unknown":"ignored","path":"/never/read"},
            {"name":"folder","reference":true,"bytes":4096},
            {"name":"old.pdf","expired":true},
            {"name":"missing.txt","unavailable":true}
        ]}).to_string()));
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[0],
            Attachment {
                name: "original-image.png".into(),
                mime: "image/png".into(),
                bytes: 12,
                reference: false,
                expired: false,
                unavailable: false
            }
        );
        assert!(rows[1].reference);
        assert_eq!(rows[1].mime, "application/octet-stream");
        assert!(rows[2].expired);
        assert!(rows[3].unavailable);
    }

    #[test]
    fn malformed_metadata_and_entries_are_skipped_without_losing_valid_neighbors() {
        for meta in [None, Some("{"), Some("null"), Some(r#"{"attachments":{}}"#)] {
            assert!(decode(meta).is_empty());
        }
        let rows = decode(Some(&json!({"attachments":[null,{}, {"name":2}, {"name":"bad","bytes":-1}, {"name":"bad","mime":false}, {"name":"bad","expired":"true"}, {"name":"bad\nname"}, {"name":"good","bytes":3}]}).to_string()));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "good");
    }

    #[test]
    fn bounds_metadata_entry_count_and_utf8_string_bytes() {
        let rows = vec![json!({"name":"ok","mime":"text/plain"}); MAX_ENTRIES + 5];
        assert_eq!(
            decode(Some(&json!({"attachments":rows}).to_string())).len(),
            MAX_ENTRIES
        );
        for row in [
            json!({"name":"ก".repeat(MAX_NAME_BYTES / 3 + 1)}),
            json!({"name":"ok","mime":"m".repeat(MAX_MIME_BYTES+1)}),
        ] {
            assert!(decode(Some(&json!({"attachments":[row]}).to_string())).is_empty());
        }
        assert!(decode(Some(&" ".repeat(MAX_META_BYTES + 1))).is_empty());
        assert_eq!(decode(Some(&json!({"attachments":[{"name":"n".repeat(MAX_NAME_BYTES),"mime":"m".repeat(MAX_MIME_BYTES)}]}).to_string())).len(), 1);
    }
}
