//! Explicit, bounded copies of the daemon's persisted Session export.
//! Export replies have no Session id: ownership comes from the request envelope,
//! captured Session, connection and operation, never the displayed active Session.
use std::io::{self, Write};

use serde_json::Value;

pub const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Markdown,
    Html,
}

impl Format {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Html => "html",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON bundle",
            Self::Markdown => "Markdown",
            Self::Html => "HTML source",
        }
    }
}

#[derive(Debug)]
pub struct Document {
    pub name: String,
    pub text: String,
}

struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("export exceeds 1 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn decode(format: Format, value: Value) -> Result<Document, String> {
    let invalid = "Malformed or unsupported export. Use Formless TUI to export this Session.";
    let name = value.get("name").and_then(Value::as_str).ok_or(invalid)?;
    if name.len() > 16 * 1024 {
        return Err(invalid.into());
    }
    let text = if format == Format::Json {
        if value["format"] != "formless.session/1"
            || !value["messages"].is_array()
            || !value["checkpoints"].is_array()
        {
            return Err(invalid.into());
        }
        // Do not allocate an unbounded pretty-printed second copy of a reply.
        let mut output = Bounded(Vec::new());
        serde_json::to_writer_pretty(&mut output, &value).map_err(|_| {
            "Export exceeds the native 1 MiB copy limit. Use Formless TUI.".to_string()
        })?;
        String::from_utf8(output.0).expect("JSON serialization is UTF-8")
    } else {
        if value["format"] != format.wire() {
            return Err(invalid.into());
        }
        let content = value["content"].as_str().ok_or(invalid)?;
        if content.len() > MAX_BYTES {
            return Err("Export exceeds the native 1 MiB copy limit. Use Formless TUI.".into());
        }
        content.to_string()
    };
    Ok(Document {
        name: name.into(),
        text,
    })
}

pub fn excerpt(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut excerpt: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        excerpt.push_str("\n… Preview truncated; Copy export copies the complete returned text.");
    }
    excerpt
}

#[derive(Debug)]
pub struct State {
    pub revision: uuid::Uuid,
    pub document: Option<Document>,
    pub notice: String,
    pending: Option<(uuid::Uuid, Format)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            revision: uuid::Uuid::new_v4(),
            document: None,
            notice: String::new(),
            pending: None,
        }
    }
}

impl State {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn retire(&mut self, notice: &str) {
        self.revision = uuid::Uuid::new_v4();
        self.pending = None;
        self.document = None;
        self.notice = notice.into();
    }
    pub fn start(&mut self, operation: uuid::Uuid, format: Format) {
        self.retire("Reading persisted export. No file is written and nothing is copied yet.");
        self.pending = Some((operation, format));
    }
    pub fn receive(
        &mut self,
        operation: uuid::Uuid,
        format: Format,
        result: Result<Document, String>,
    ) -> bool {
        if self.pending != Some((operation, format)) {
            return false;
        }
        self.retire("");
        match result {
            Ok(document) => {
                self.notice = format!(
                    "{} · {} bytes. Preview is plain text, up to 4,000 characters. Copy export copies the complete returned text to the system clipboard. This is a past read, not a live view.",
                    format.label(), document.text.len()
                );
                self.document = Some(document);
            }
            Err(error) => self.notice = crate::questions::preview(&error, 500),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn export_copy_is_exact_unicode_and_html_is_only_source() {
        for format in [Format::Markdown, Format::Html] {
            let text = "\n<script>ไทย 😀</script>\r\n";
            let document = decode(
                format,
                json!({"name":"Session", "format":format.wire(),"content":text}),
            )
            .unwrap();
            assert_eq!(document.text, text);
            assert!(decode(
                format,
                json!({"name":"Session", "format":"json","content":text})
            )
            .is_err());
        }
        let bundle = json!({"name":"ไทย", "format":"formless.session/1","messages":[],"checkpoints":[],"future_field":{"a":1}});
        assert_eq!(
            serde_json::from_str::<Value>(&decode(Format::Json, bundle.clone()).unwrap().text)
                .unwrap(),
            bundle
        );
    }

    #[test]
    fn invalid_and_oversized_exports_are_rejected_whole() {
        for value in [
            Value::Null,
            json!({"name":"s","format":"formless.session/1"}),
            json!({"name":"s","format":"json","messages":[],"checkpoints":[]}),
        ] {
            assert!(decode(Format::Json, value).is_err());
        }
        let text = "x".repeat(MAX_BYTES);
        assert_eq!(
            decode(
                Format::Markdown,
                json!({"name":"s","format":"markdown","content":text})
            )
            .unwrap()
            .text
            .len(),
            MAX_BYTES
        );
        assert!(decode(
            Format::Html,
            json!({"name":"s","format":"html","content":"x".repeat(MAX_BYTES+1)})
        )
        .is_err());
        assert!(decode(Format::Json,json!({"name":"s","format":"formless.session/1","messages":[{"content":"x".repeat(MAX_BYTES)}],"checkpoints":[]})).is_err());
        let mut writer = Bounded(vec![0; MAX_BYTES - 1]);
        assert!(writer.write_all(&[1, 2]).is_err());
        assert_eq!(writer.0.len(), MAX_BYTES - 1);
    }

    #[test]
    fn pending_format_operation_and_retirement_own_export_results() {
        let mut state = State::default();
        assert_ne!(
            state.revision,
            State::default().revision,
            "new panels never reuse callback identities"
        );
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        state.start(a, Format::Json);
        state.start(b, Format::Html);
        assert!(!state.receive(a, Format::Json, Err("old".into())));
        assert!(!state.receive(b, Format::Json, Err("wrong format".into())));
        assert!(state.pending());
        assert!(state.receive(
            b,
            Format::Html,
            Ok(Document {
                name: "s".into(),
                text: "exact".into()
            })
        ));
        let old = state.revision;
        state.retire("Disconnected");
        assert!(state.document.is_none());
        assert_ne!(state.revision, old);
        assert!(!state.receive(b, Format::Html, Err("late".into())));
    }
}
