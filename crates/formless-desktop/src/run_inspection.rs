//! Bounded read-only inspection of one exact Run reported by Metrics.
use crate::pages::{Page, Row, PREVIEW_CHARS};
use serde_json::{json, Value};
use std::io::{self, Write};

pub const MAX_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub run_id: String,
    pub session_id: String,
}
impl Target {
    pub fn valid(&self) -> bool {
        [&self.run_id, &self.session_id].iter().all(|id| {
            !id.is_empty()
                && id.len() <= 256
                && id.trim() == id.as_str()
                && !id.chars().any(char::is_control)
        })
    }
    pub fn from_metric(value: &Value) -> Option<Self> {
        let target = Self {
            run_id: value.get("run_id")?.as_str()?.into(),
            session_id: value.get("session_id")?.as_str()?.into(),
        };
        target.valid().then_some(target)
    }
}
struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("native Run inspection size bound"));
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
        "Run inspection exceeds the native 1 MiB copy limit; use Formless TUI.".to_string()
    })?;
    Ok(String::from_utf8(output.0).expect("JSON is UTF-8"))
}
fn row(label: &str, preview: String, detail: String) -> Row {
    Row {
        label: label.into(),
        preview: preview.chars().take(PREVIEW_CHARS).collect(),
        preview_truncated: preview.chars().nth(PREVIEW_CHARS).is_some(),
        detail,
        status: None,
        actions: Vec::new(),
    }
}
#[derive(Debug)]
pub struct Report {
    pub page: Page,
}
impl Report {
    pub fn decode(target: &Target, value: Value) -> Result<Self, String> {
        let invalid="Unsupported, mismatched or oversized Run inspection. Use Formless TUI; no partial record is shown.";
        if !target.valid() {
            return Err(invalid.into());
        }
        serde_json::to_writer(&mut Bounded(Vec::new()), &value).map_err(|_| invalid.to_string())?;
        let object = value.as_object().ok_or(invalid)?;
        if value["run_id"].as_str() != Some(&target.run_id)
            || value["session_id"].as_str() != Some(&target.session_id)
            || !["status", "created_at", "updated_at"].iter().all(|key| {
                value[*key]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && s.len() <= 256)
            })
            || !["steps", "tool_calls", "prompt_tokens", "completion_tokens"]
                .iter()
                .all(|key| value[*key].as_u64().is_some())
            || !["start_message_seq", "end_message_seq"].iter().all(|key| {
                object
                    .get(*key)
                    .is_some_and(|v| v.is_null() || v.as_u64().is_some())
            })
            || !object
                .get("error")
                .is_some_and(|v| v.is_null() || v.is_string())
            || object
                .get("spend")
                .is_some_and(|v| !v.is_null() && !v.is_object())
        {
            return Err(invalid.into());
        }
        if let (Some(a), Some(b)) = (
            value["start_message_seq"].as_u64(),
            value["end_message_seq"].as_u64(),
        ) {
            if b < a {
                return Err(invalid.into());
            }
        }
        let all = pretty(&value)?;
        let status = json!({"run_id":value["run_id"],"session_id":value["session_id"],"status":value["status"],"error":value["error"],"created_at":value["created_at"],"updated_at":value["updated_at"],"start_message_seq":value["start_message_seq"],"end_message_seq":value["end_message_seq"]});
        let counters = json!({"steps":value["steps"],"tool_calls":value["tool_calls"],"prompt_tokens":value["prompt_tokens"],"completion_tokens":value["completion_tokens"]});
        let mut rows=vec![row("Scope · Copy details copies the complete returned Run JSON",format!("Run: {}\nSession: {}\nStatus: {}\nPast recorded read. Sequence bounds, errors and counters are in the rows; a running Run can change after this read.",target.run_id,target.session_id,value["status"].as_str().unwrap()),all),row("Status, message bounds and error",pretty(&status)?,pretty(&status)?),row("Recorded counters",pretty(&counters)?,pretty(&counters)?)];
        if let Some(spend) = object.get("spend") {
            let detail = pretty(spend)?;
            rows.push(row(
                "Reported spend · availability and completeness are not inferred",
                detail.clone(),
                detail,
            ));
        }
        if rows.iter().map(|row| row.detail.len()).sum::<usize>() > 3 * MAX_BYTES {
            return Err(invalid.into());
        }
        Ok(Self{page:Page{revision:0,selected:Some(0),rows,notice:format!("Run: {}\nSession: {}\nRead-only persisted inspection. No Run control, model call, fork or automatic replay. The record can include private errors, provider/model metadata and future fields; Scope Copy details copies the complete returned JSON. Previews are limited to 500 characters. Spend, when returned, is the daemon's reported record, not an inferred complete bill; missing/null cost is unavailable, never zero. Refresh explicitly for a later read.",target.run_id,target.session_id)}})
    }
}
#[derive(Debug)]
pub struct State {
    pub connection: Option<u64>,
    pub revision: uuid::Uuid,
    pub target: Option<Target>,
    pub notice: String,
    pending: Option<(u64, uuid::Uuid, Target)>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            connection: None,
            revision: uuid::Uuid::new_v4(),
            target: None,
            notice: "Select a Run entry in Metrics, then Inspect Run.".into(),
            pending: None,
        }
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
    pub fn start(&mut self, connection: u64, operation: uuid::Uuid, target: Target) {
        self.retire("Reading the selected persisted Run. No model call or Run control.");
        self.connection = Some(connection);
        self.target = Some(target.clone());
        self.pending = Some((connection, operation, target));
    }
    pub fn receive(&mut self, connection: u64, operation: uuid::Uuid, target: &Target) -> bool {
        if self.pending.as_ref() != Some(&(connection, operation, target.clone())) {
            return false;
        }
        self.pending = None;
        self.revision = uuid::Uuid::new_v4();
        true
    }
}

#[cfg(test)]
pub(crate) fn test_value(target: &Target) -> Value {
    json!({"run_id":target.run_id,"session_id":target.session_id,"status":"running","error":"  exact ไทย 😀\r\n private  ","steps":2,"tool_calls":3,"prompt_tokens":4,"completion_tokens":5,"created_at":"now","updated_at":"later","start_message_seq":1,"end_message_seq":null,"spend":{"cost":null,"priced_subtotal":0.12,"unpriced":["model"]},"future":{"private":"preserved"}})
}
#[cfg(test)]
mod tests {
    use super::*;
    fn target() -> Target {
        Target {
            run_id: "run-a".into(),
            session_id: "session-a".into(),
        }
    }
    #[test]
    fn run_json_preserves_future_and_private_fields_and_qualifies_spend() {
        let target = target();
        let value = test_value(&target);
        let report = Report::decode(&target, value.clone()).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&report.page.rows[0].detail).unwrap(),
            value
        );
        assert_eq!(
            serde_json::from_str::<Value>(&report.page.rows[3].detail).unwrap(),
            value["spend"]
        );
        assert!(report.page.notice.contains("never zero"));
        assert_eq!(report.page.rows.len(), 4);
        let mut no_spend = value;
        no_spend.as_object_mut().unwrap().remove("spend");
        assert_eq!(
            Report::decode(&target, no_spend).unwrap().page.rows.len(),
            3
        );
    }
    #[test]
    fn run_inspection_refuses_mismatch_missing_fields_negative_counts_and_oversize() {
        let target = target();
        let value = test_value(&target);
        for key in [
            "run_id",
            "session_id",
            "status",
            "error",
            "steps",
            "tool_calls",
            "prompt_tokens",
            "completion_tokens",
            "created_at",
            "updated_at",
            "start_message_seq",
            "end_message_seq",
        ] {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(Report::decode(&target, bad).is_err(), "missing {key}");
        }
        for (key, bad_value) in [
            ("run_id", json!("run-other")),
            ("session_id", json!("other")),
            ("steps", json!(-1)),
            ("tool_calls", json!(1.2)),
            ("start_message_seq", json!(-1)),
            ("end_message_seq", json!(0)),
            ("spend", json!([])),
            ("future", json!("x".repeat(MAX_BYTES))),
        ] {
            let mut bad = value.clone();
            bad[key] = bad_value;
            assert!(Report::decode(&target, bad).is_err(), "{key}");
        }
        let mut pretty_bound = value;
        pretty_bound["future"] = "".into();
        let overhead = serde_json::to_vec(&pretty_bound).unwrap().len();
        pretty_bound["future"] = json!("x".repeat(MAX_BYTES - overhead - 50));
        assert!(serde_json::to_vec(&pretty_bound).unwrap().len() < MAX_BYTES);
        assert!(Report::decode(&target, pretty_bound).is_err());
        assert!(Target::from_metric(&json!({"run_id":"","session_id":"s"})).is_none());
        assert!(Target::from_metric(&json!({"run_id":"a\nb","session_id":"s"})).is_none());
    }
    #[test]
    fn run_read_state_rejects_stale_connections_operations_targets_and_retirement() {
        let target = target();
        let mut state = State::default();
        let operation = uuid::Uuid::new_v4();
        state.start(2, operation, target.clone());
        let revision = state.revision;
        assert!(!state.receive(3, operation, &target));
        assert!(!state.receive(2, uuid::Uuid::new_v4(), &target));
        assert!(!state.receive(
            2,
            operation,
            &Target {
                session_id: "other".into(),
                ..target.clone()
            }
        ));
        assert!(state.receive(2, operation, &target));
        assert_ne!(state.revision, revision);
        assert!(!state.receive(2, operation, &target));
        state.start(2, operation, target.clone());
        state.retire("attachment replaced");
        assert!(state.target.is_none());
        assert!(!state.receive(2, operation, &target));
    }
}
