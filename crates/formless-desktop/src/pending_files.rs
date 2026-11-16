//! Complete read-only metadata for the Session's next user message, not file bodies.
use serde_json::Value;
pub const MAX_BYTES: usize = 2 * formless_protocol::PENDING_ATTACHMENT_METADATA_BYTES;
pub const PAGE_SIZE: usize = 8;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: String,
    pub kind: &'static str,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub rows: Vec<Row>,
    raw: Value,
}
fn text(value: &Value, field: &str, max: usize) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= max)
        .map(str::to_string)
        .ok_or_else(|| "Unsupported pending-file metadata; use Formless TUI.".into())
}
fn bounded(value: &Value) -> bool {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_BYTES {
                return Err(std::io::Error::other("pending metadata limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Count(0), value).is_ok()
}
impl Report {
    pub fn decode(session_id: &str, value: Value) -> Result<Self, String> {
        let fail = || {
            "Pending-file inventory is unsupported or exceeds native bounds; use Formless TUI."
                .to_string()
        };
        if session_id.is_empty()
            || session_id.len() > 256
            || session_id.chars().any(char::is_control)
            || value["session_id"].as_str() != Some(session_id)
            || !bounded(&value)
        {
            return Err(fail());
        }
        let attachments = value["attachments"].as_array().ok_or_else(fail)?;
        if attachments.len() > formless_protocol::PENDING_ATTACHMENT_MAX_ENTRIES {
            return Err(fail());
        }
        let mut rows = Vec::with_capacity(attachments.len());
        for raw in attachments {
            let reference = match raw.get("reference") {
                None | Some(Value::Bool(false)) => false,
                Some(Value::Bool(true)) => true,
                _ => return Err(fail()),
            };
            let name = text(raw, "name", 512)?;
            let path = text(raw, "path", 4096)?;
            let bytes = raw["bytes"].as_u64().ok_or_else(fail)?;
            let kind = if reference {
                match raw.get("dir") {
                    Some(Value::Bool(true)) => "directory reference",
                    Some(Value::Bool(false)) => "file reference",
                    _ => return Err(fail()),
                }
            } else {
                text(raw, "id", 256)?;
                text(raw, "mime", 256)?;
                "stored file"
            };
            rows.push(Row {
                name,
                path,
                bytes,
                kind,
            });
        }
        Ok(Self { rows, raw: value })
    }
    pub fn session_id(&self) -> &str {
        self.raw["session_id"]
            .as_str()
            .expect("validated Session identity")
    }
    pub fn copy(&self) -> String {
        serde_json::to_string(&self.raw).expect("bounded metadata JSON").chars().map(|c|if matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'){format!("\\u{:04x}",c as u32)}else{c.to_string()}).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    pub fn fixture() -> Value {
        json!({"session_id":"s","attachments":[{"id":"a","name":"ไทย😀","mime":"text/plain","path":"/private/stored","bytes":5,"future":{"kept":true}},{"reference":true,"name":"dir","path":"/private/dir","dir":true,"bytes":0}]})
    }
    #[test]
    fn pending_files_preserves_complete_metadata_and_duplicates_without_reading_paths() {
        let mut v = fixture();
        let row = v["attachments"][1].clone();
        v["attachments"].as_array_mut().unwrap().push(row);
        let r = Report::decode("s", v.clone()).unwrap();
        assert_eq!(r.rows.len(), 3);
        assert_eq!(r.rows[1].kind, "directory reference");
        assert_eq!(serde_json::from_str::<Value>(&r.copy()).unwrap(), v);
        assert!(Report::decode("other", fixture()).is_err());
    }
    #[test]
    fn pending_files_rejects_malformed_missing_and_oversized_records_whole() {
        for mode in 0..6 {
            let mut v = fixture();
            match mode {
                0 => v["attachments"] = Value::Null,
                1 => v["attachments"][0]["bytes"] = json!(-1),
                2 => v["attachments"][0]["path"] = Value::Null,
                3 => v["attachments"][1]["dir"] = Value::Null,
                4 => v["attachments"][0]["future"] = json!("x".repeat(MAX_BYTES)),
                _ => {
                    let row = v["attachments"][0].clone();
                    v["attachments"] = json!(vec![
                        row;
                        formless_protocol::PENDING_ATTACHMENT_MAX_ENTRIES
                            + 1
                    ]);
                }
            }
            assert!(Report::decode("s", v).is_err(), "{mode}");
        }
    }
}
