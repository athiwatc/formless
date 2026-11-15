//! Explicit snapshots of unfinished Tool Invocations, never control authority.
use crate::pages::{Page, Row, PREVIEW_CHARS};
use formless_protocol::InvocationInfo;
use serde_json::Value;
use std::collections::HashSet;
use std::io::{self, Write};
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_ROWS: usize = 1000;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub invocation_id: Option<String>,
    pub handle: String,
    pub session_id: String,
    pub tool_name: String,
}
fn id(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("native Invocation snapshot size bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn pretty(value: &Value) -> Result<String, String> {
    let mut output = Bounded(Vec::new());
    serde_json::to_writer_pretty(&mut output, value).map_err(|_| {
        "Invocation snapshot exceeds the native 1 MiB copy limit. Use Formless TUI.".to_string()
    })?;
    Ok(String::from_utf8(output.0).expect("JSON is UTF-8"))
}
fn row(label: String, preview: String, detail: String) -> Row {
    Row {
        label,
        preview: preview.chars().take(PREVIEW_CHARS).collect(),
        preview_truncated: preview.chars().nth(PREVIEW_CHARS).is_some(),
        detail,
    }
}
#[derive(Debug)]
pub struct Report {
    pub page: Page,
    pub keys: Vec<Key>,
}
impl Report {
    pub fn decode(value: Value) -> Result<Self, String> {
        let invalid="Unsupported, duplicate or oversized Invocation snapshot (native limits: 1 MiB / 1,000 entries). Use Formless TUI; no partial snapshot is shown.";
        serde_json::to_writer(&mut Bounded(Vec::new()), &value).map_err(|_| invalid.to_string())?;
        let values = value.as_array().ok_or(invalid)?;
        if values.len() > MAX_ROWS {
            return Err(invalid.into());
        }
        let all = pretty(&value)?;
        let mut keys = Vec::with_capacity(values.len());
        let mut rows=vec![row("Scope · Copy details copies the complete returned snapshot JSON".into(),format!("{} unfinished Tool Invocations returned across this Profile. Past explicit read, oldest first. Select a record for its Session, progress and status; Refresh explicitly for a later snapshot.",values.len()),all)];
        let mut handles = HashSet::new();
        let mut identities = HashSet::new();
        let mut legacy = false;
        for value in values {
            let info: InvocationInfo =
                serde_json::from_value(value.clone()).map_err(|_| invalid.to_string())?;
            if !id(&info.handle, 512)
                || !id(&info.session_id, 256)
                || !id(&info.tool_name, 512)
                || info.invocation_id.as_ref().is_some_and(|identity| {
                    !id(identity, 256) || !identities.insert(identity.clone())
                })
                || !handles.insert(info.handle.clone())
                || info
                    .last_label
                    .as_ref()
                    .is_some_and(|label| label.len() > 4096)
                || info.last_fraction.is_some_and(|fraction| {
                    !fraction.is_finite() || !(0.0..=1.0).contains(&fraction)
                })
            {
                return Err(invalid.into());
            }
            legacy |= info.invocation_id.is_none();
            let progress = info.last_fraction.map_or_else(
                || "not reported".into(),
                |fraction| format!("{:.1}% reported", fraction * 100.),
            );
            let text=format!("Session: {}\nInvocation ID: {}\nHandle: {}\nTool: {}\nRecorded elapsed: {} ms · silence: {} ms\nStalled: {} · agent no longer waiting: {}\nProgress: {progress}\nLast label: {}",info.session_id,info.invocation_id.as_deref().unwrap_or("not returned (legacy handle only)"),info.handle,info.tool_name,info.elapsed_ms,info.silent_ms,info.stalled,info.abandoned,info.last_label.as_deref().unwrap_or("not reported"));
            rows.push(row(
                format!(
                    "{} · {} · stalled: {} · abandoned: {}",
                    crate::directory::visual_text(&info.tool_name),
                    crate::directory::visual_text(&info.handle),
                    info.stalled,
                    info.abandoned
                ),
                text,
                pretty(value)?,
            ));
            keys.push(Key {
                invocation_id: info.invocation_id,
                handle: info.handle,
                session_id: info.session_id,
                tool_name: info.tool_name,
            });
        }
        if rows.iter().map(|row| row.detail.len()).sum::<usize>() > 3 * MAX_BYTES {
            return Err(invalid.into());
        }
        Ok(Self{keys,page:Page{revision:0,selected:Some(0),rows,notice:format!("{} unfinished Tool Invocations in the daemon's pending list · read only. Ages/status/progress are values at read time, not a running clock; work may finish afterward. Stalled and abandoned are daemon flags; abandoned means the agent stopped waiting, not necessarily cancelled. Progress 100% is not proof of completion. {}Scope Copy details preserves the whole returned JSON, including future fields and private progress labels; other rows copy their complete record. Previews are limited to 500 characters. Open Session navigates only to an available unarchived Session; no Invocation is controlled. Use Formless TUI for Cancel/Abandon. Refresh is explicit; reconnect never replays this read.",values.len(),if legacy{"Some records lack a durable Invocation ID; handles are not durable identity. "}else{""})}})
    }
}
#[derive(Debug)]
pub struct State {
    pub revision: uuid::Uuid,
    pub connection: Option<u64>,
    pub keys: Vec<Key>,
    pub notice: String,
    pending: Option<(u64, uuid::Uuid)>,
}
impl Default for State {
    fn default() -> Self {
        Self{revision:uuid::Uuid::new_v4(),connection:None,keys:Vec::new(),notice:"Choose View Invocations in Commands or Tools to read unfinished Tool work across this Profile.".into(),pending:None}
    }
}
impl State {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn retire(&mut self, notice: &str) {
        *self = Self::default();
        self.notice = notice.into();
    }
    pub fn start(&mut self, connection: u64, operation: uuid::Uuid) {
        self.retire("Reading unfinished Tool Invocations. No Tool/Run control or model call.");
        self.connection = Some(connection);
        self.pending = Some((connection, operation));
    }
    pub fn receive(&mut self, connection: u64, operation: uuid::Uuid) -> bool {
        if self.pending != Some((connection, operation)) {
            return false;
        }
        self.pending = None;
        self.revision = uuid::Uuid::new_v4();
        true
    }
}
#[cfg(test)]
pub(crate) fn test_value() -> Value {
    serde_json::json!([{"invocation_id":"invocation-1","handle":"tool/handle","tool_name":"Tool ไทย 😀","session_id":"session-a","elapsed_ms":3210,"silent_ms":1000,"stalled":true,"abandoned":true,"last_fraction":0.5,"last_label":"  private ไทย 😀\r\n label  ","future":{"exact":"preserved"}}])
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn invocation_json_preserves_private_future_fields_and_legacy_absence() {
        let value = test_value();
        let report = Report::decode(value.clone()).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&report.page.rows[0].detail).unwrap(),
            value
        );
        assert_eq!(
            serde_json::from_str::<Value>(&report.page.rows[1].detail).unwrap(),
            value[0]
        );
        assert!(report.page.rows[1].preview.contains("50.0% reported"));
        assert!(report.page.notice.contains("not necessarily cancelled"));
        let mut legacy = value;
        for key in ["invocation_id", "last_fraction", "last_label"] {
            legacy[0].as_object_mut().unwrap().remove(key);
        }
        let report = Report::decode(legacy.clone()).unwrap();
        assert!(report.keys[0].invocation_id.is_none());
        assert!(report.page.notice.contains("lack a durable"));
        assert!(report.page.rows[1].preview.contains("not reported"));
        assert_eq!(
            serde_json::from_str::<Value>(&report.page.rows[1].detail).unwrap(),
            legacy[0]
        );
    }
    #[test]
    fn invocation_snapshot_refuses_bad_shapes_bounds_and_ambiguous_identity() {
        let value = test_value();
        for key in [
            "handle",
            "tool_name",
            "session_id",
            "elapsed_ms",
            "silent_ms",
            "stalled",
            "abandoned",
        ] {
            let mut bad = value.clone();
            bad[0].as_object_mut().unwrap().remove(key);
            assert!(Report::decode(bad).is_err(), "missing {key}");
        }
        for (key, bad_value) in [
            ("handle", json!("")),
            ("session_id", json!("s\nother")),
            ("elapsed_ms", json!(-1)),
            ("last_fraction", json!(1.1)),
            ("last_fraction", json!(-0.1)),
            ("invocation_id", json!("")),
            ("last_label", json!("x".repeat(4097))),
            ("future", json!("x".repeat(MAX_BYTES))),
        ] {
            let mut bad = value.clone();
            bad[0][key] = bad_value;
            assert!(Report::decode(bad).is_err(), "{key}");
        }
        assert!(Report::decode(json!([value[0], value[0]])).is_err());
        let mut repeated = value[0].clone();
        repeated["handle"] = "other".into();
        assert!(Report::decode(json!([value[0], repeated])).is_err());
        assert!(Report::decode(json!(vec![value[0].clone(); MAX_ROWS + 1])).is_err());
        let mut expanded = value.clone();
        expanded[0]["future"] = "".into();
        let overhead = serde_json::to_vec(&expanded).unwrap().len();
        expanded[0]["future"] = json!("x".repeat(MAX_BYTES - overhead - 1));
        assert!(serde_json::to_vec(&expanded).unwrap().len() < MAX_BYTES);
        assert!(matches!(Report::decode(expanded), Err(error) if error.contains("copy limit")));
        assert!(Report::decode(Value::Null).is_err());
        assert_eq!(Report::decode(json!([])).unwrap().page.rows.len(), 1);
    }
    #[test]
    fn invocation_lifetime_rejects_old_connections_operations_and_retirement() {
        let mut state = State::default();
        let operation = uuid::Uuid::new_v4();
        state.start(3, operation);
        assert!(!state.receive(4, operation));
        assert!(!state.receive(3, uuid::Uuid::new_v4()));
        let revision = state.revision;
        assert!(state.receive(3, operation));
        assert_ne!(state.revision, revision);
        assert!(!state.receive(3, operation));
        state.start(3, operation);
        state.retire("closed");
        assert!(state.connection.is_none());
        assert!(state.keys.is_empty());
        assert!(!state.receive(3, operation));
    }
}
