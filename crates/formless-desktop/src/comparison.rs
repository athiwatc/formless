//! Explicit read-only comparisons of persisted Session histories. No model calls.
use std::collections::HashSet;
use std::io::{self, Write};

use formless_protocol::{SessionBranchUsage, SessionComparison};
use serde_json::Value;

use crate::pages::{Page, Row, PREVIEW_CHARS};

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_MESSAGES: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub left: String,
    pub right: String,
}
impl Pair {
    pub fn valid(&self) -> bool {
        self.left != self.right
            && [&self.left, &self.right]
                .iter()
                .all(|id| !id.is_empty() && id.len() <= 256)
    }
}

struct Bounded(Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::other("native comparison size bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn strings(values: &[String], max: usize, length: usize) -> bool {
    values.len() <= max && values.iter().all(|value| value.len() <= length)
}
fn usage_valid(usage: &SessionBranchUsage) -> bool {
    [
        usage.recorded_prompt_tokens,
        usage.recorded_completion_tokens,
        usage.recorded_cached_tokens,
        usage.recorded_cache_write_tokens,
        usage.unallocated_prompt_tokens,
        usage.unallocated_completion_tokens,
    ]
    .iter()
    .all(|n| *n >= 0)
        && [
            usage.priced_cost_subtotal,
            usage.unallocated_priced_cost_subtotal,
        ]
        .iter()
        .all(|n| n.is_finite() && *n >= 0.)
        && usage.cost.is_none_or(|n| n.is_finite() && n >= 0.)
        && usage
            .known_records_cost_range
            .is_none_or(|[a, b]| a.is_finite() && b.is_finite() && a >= 0. && b >= a)
        && usage.scope.len() <= 4096
        && strings(&usage.unpriced, 64, 256)
        && strings(&usage.warnings, 64, 4096)
}
fn usage_text(usage: &SessionBranchUsage) -> String {
    let cost = usage.cost.map_or_else(
        || "unavailable (not zero)".into(),
        |cost| format!("estimated USD {cost}"),
    );
    let range = usage
        .known_records_cost_range
        .map_or_else(|| "not supplied".into(), |[a, b]| format!("USD {a} to {b}"));
    format!("Scope: {}\nRecorded requests: {} · estimated: {}\nPrompt / completion / cached / cache-write tokens: {} / {} / {} / {}\nCost: {cost}\nPriced allocated subtotal only: USD {}\nUnallocated requests: {} · prompt/completion: {}/{} · priced subtotal only: USD {}\nUnknown Runs: {} · complete coverage reported: {}\nKnown priced observed-record range only: {range}; this does not bound missing usage or unpriced records.\nUnpriced models: {}", usage.scope, usage.requests, usage.estimated_requests, usage.recorded_prompt_tokens, usage.recorded_completion_tokens, usage.recorded_cached_tokens, usage.recorded_cache_write_tokens, usage.priced_cost_subtotal, usage.unallocated_requests, usage.unallocated_prompt_tokens, usage.unallocated_completion_tokens, usage.unallocated_priced_cost_subtotal, usage.unknown_runs, usage.usage_complete, usage.unpriced.join(", "))
}
fn row(label: String, text: String, detail: String) -> Row {
    Row {
        label,
        preview: text.chars().take(PREVIEW_CHARS).collect(),
        preview_truncated: text.chars().nth(PREVIEW_CHARS).is_some(),
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
    pub fn decode(pair: &Pair, value: Value) -> Result<Self, String> {
        let invalid = "Unsupported or oversized comparison (native limits: 1 MiB / 1,000 message rows). Use Formless TUI; no partial comparison is shown.";
        if !pair.valid() {
            return Err(invalid.into());
        }
        // Bound the received shape too, including unknown fields, before making
        // the typed native projection. The transport frame limit still precedes this.
        serde_json::to_writer(&mut Bounded(Vec::new()), &value).map_err(|_| invalid.to_string())?;
        let data: SessionComparison =
            serde_json::from_value(value).map_err(|_| invalid.to_string())?;
        if data.left.session.id != pair.left
            || data.right.session.id != pair.right
            || data.messages.len() > MAX_MESSAGES
            || data.diff_from_seq < 0
            || data.fork_point_seq.is_some_and(|seq| seq < 0)
            || data
                .common_ancestor
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 256)
            || !usage_valid(&data.left.usage)
            || !usage_valid(&data.right.usage)
            || !strings(&data.warnings, 64, 4096)
        {
            return Err(invalid.into());
        }
        let mut left = HashSet::new();
        let mut right = HashSet::new();
        for change in &data.messages {
            let shape = match change.kind.as_str() {
                "left_only" => change.left.is_some() && change.right.is_none(),
                "right_only" => change.left.is_none() && change.right.is_some(),
                "unchanged" => match (&change.left, &change.right) {
                    (Some(a), Some(b)) => {
                        a.role == b.role && a.content == b.content && a.meta == b.meta
                    }
                    _ => false,
                },
                _ => false,
            };
            if !shape {
                return Err(invalid.into());
            }
            for (message, id, seen) in [
                (&change.left, &pair.left, &mut left),
                (&change.right, &pair.right, &mut right),
            ] {
                if let Some(message) = message {
                    if &message.session_id != id
                        || message.seq < 0
                        || !seen.insert(message.seq)
                        || message.id.is_empty()
                        || message.id.len() > 256
                        || message.role.len() > 256
                    {
                        return Err(invalid.into());
                    }
                }
            }
        }
        let mut copy = Bounded(Vec::new());
        serde_json::to_writer_pretty(&mut copy, &data).map_err(|_| invalid.to_string())?;
        let copy = String::from_utf8(copy.0).expect("JSON is UTF-8");
        let scope = format!("Left: {} · {}\nRight: {} · {}\nCommon ancestor: {}\nFork point: {} · comparison starts after seq {} · prefix changed: {}", pair.left, crate::directory::visual_text(&data.left.session.name), pair.right, crate::directory::visual_text(&data.right.session.name), data.common_ancestor.as_deref().unwrap_or("not supplied"), data.fork_point_seq.map_or_else(|| "not supplied".into(), |seq|seq.to_string()), data.diff_from_seq, data.prefix_changed);
        let mut rows = vec![row(
            "Scope · Copy details copies the whole validated comparison".into(),
            scope,
            copy,
        )];
        for (label, side) in [
            ("Left recorded usage", &data.left),
            ("Right recorded usage", &data.right),
        ] {
            rows.push(row(
                label.into(),
                usage_text(&side.usage),
                serde_json::to_string_pretty(&side.usage).expect("usage serializes"),
            ));
        }
        let warnings = serde_json::json!({"comparison":data.warnings,"left_usage":data.left.usage.warnings,"right_usage":data.right.usage.warnings});
        rows.push(row(
            "Warnings · copy all warnings".into(),
            warnings.to_string(),
            serde_json::to_string_pretty(&warnings).expect("warnings serialize"),
        ));
        for change in &data.messages {
            let label = format!(
                "{} · left seq {} / right seq {}",
                change.kind,
                change
                    .left
                    .as_ref()
                    .map_or_else(|| "absent".into(), |m| m.seq.to_string()),
                change
                    .right
                    .as_ref()
                    .map_or_else(|| "absent".into(), |m| m.seq.to_string())
            );
            let text = format!(
                "Left\n{}\n\nRight\n{}",
                change
                    .left
                    .as_ref()
                    .map_or("(absent)", |m| m.content.as_str()),
                change
                    .right
                    .as_ref()
                    .map_or("(absent)", |m| m.content.as_str())
            );
            rows.push(row(
                label,
                text,
                serde_json::to_string_pretty(change).expect("message diff serializes"),
            ));
        }
        if rows.iter().map(|row| row.detail.len()).sum::<usize>() > 3 * MAX_BYTES {
            return Err(invalid.into());
        }
        Ok(Self { page: Page { revision: 0, selected: Some(0), rows, notice: format!("Left: {}\nRight: {}\nPast local read; whole-message differences, not a Git diff or paid model comparison. Shared prefix may be excluded by the daemon; sequence scope and warnings are in the rows. Select Scope and Copy details for the whole validated JSON (known protocol fields), including Session settings/message metadata and private data. Other rows copy their full record. Previews are limited; Refresh is explicit. No model call or automatic replay.",pair.left,pair.right) } })
    }
}

#[derive(Debug)]
pub struct State {
    pub revision: uuid::Uuid,
    pub left: Option<String>,
    pub pair: Option<Pair>,
    pub notice: String,
    pending: Option<(u64, uuid::Uuid, Pair)>,
}
impl Default for State {
    fn default() -> Self {
        Self { revision:uuid::Uuid::new_v4(), left:None, pair:None, notice:"Mark a left Session in Sessions, then select a different right Session and Compare.".into(), pending:None }
    }
}
impl State {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn clear_result(&mut self, notice: &str) {
        self.revision = uuid::Uuid::new_v4();
        self.pair = None;
        self.pending = None;
        self.notice = notice.into();
    }
    pub fn reset(&mut self, notice: &str) {
        self.clear_result(notice);
        self.left = None;
    }
    pub fn mark(&mut self, id: String) {
        self.clear_result(
            "Left Session marked. Select a different Session for the right side, then Compare.",
        );
        self.left = Some(id);
    }
    pub fn start(&mut self, connection: u64, operation: uuid::Uuid, pair: Pair) {
        self.clear_result("Reading persisted comparison. No model call; nothing is written.");
        self.pair = Some(pair.clone());
        self.pending = Some((connection, operation, pair));
    }
    pub fn receive(&mut self, connection: u64, operation: uuid::Uuid, pair: &Pair) -> bool {
        if self.pending.as_ref() != Some(&(connection, operation, pair.clone())) {
            return false;
        }
        self.pending = None;
        self.revision = uuid::Uuid::new_v4();
        true
    }
}

#[cfg(test)]
pub(crate) fn test_value(pair: &Pair) -> Value {
    let usage = serde_json::json!({"scope":"recorded branch only", "recorded_prompt_tokens":11,"recorded_completion_tokens":2,"recorded_cached_tokens":3,"recorded_cache_write_tokens":4,"requests":1,"unallocated_requests":2,"unallocated_prompt_tokens":7,"unallocated_completion_tokens":8,"unknown_runs":1,"usage_complete":false,"cost":null,"priced_cost_subtotal":0.12,"unallocated_priced_cost_subtotal":0.03,"known_records_cost_range":[0.12,0.15],"estimated_requests":1,"unpriced":["unknown-model"],"warnings":["coverage missing"]});
    let mut sessions = crate::fixture::sessions(2);
    sessions[0].id = pair.left.clone();
    sessions[1].id = pair.right.clone();
    let message = |id: &str| serde_json::json!({"id":format!("{id}-m"),"session_id":id,"seq":1,"role":"assistant","content":"  ไทย 😀\r\n```rust\n code\n```  ","created_at":"now","meta":"{\"private\":true}"});
    serde_json::json!({"left":{"session":sessions[0],"usage":usage},"right":{"session":sessions[1],"usage":usage},"common_ancestor":null,"fork_point_seq":null,"diff_from_seq":0,"prefix_changed":false,"messages":[{"kind":"unchanged","left":message(&pair.left),"right":message(&pair.right)}],"warnings":["local persisted read"]})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> Pair {
        Pair {
            left: "left".into(),
            right: "right".into(),
        }
    }
    #[test]
    fn comparison_preserves_exact_known_json_and_qualifies_unknown_costs() {
        let pair = pair();
        let value = test_value(&pair);
        let report = Report::decode(&pair, value.clone()).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&report.page.rows[0].detail).unwrap(),
            value
        );
        assert_eq!(
            serde_json::from_str::<Value>(&report.page.rows[4].detail).unwrap(),
            value["messages"][0]
        );
        assert!(report.page.rows[1]
            .preview
            .contains("unavailable (not zero)"));
        assert!(report.page.rows[1].preview.contains("subtotal only"));
        assert!(report.page.notice.contains("No model call"));
        assert_eq!(report.page.rows.len(), 5);
    }
    #[test]
    fn comparison_rejects_wrong_pair_shape_semantics_and_missing_usage() {
        let pair = pair();
        let value = test_value(&pair);
        for path in [
            "kind",
            "foreign",
            "seq",
            "duplicate",
            "semantic",
            "missing",
            "range",
            "negative",
            "echo",
            "count",
        ] {
            let mut bad = value.clone();
            match path {
                "kind" => bad["messages"][0]["kind"] = "future".into(),
                "foreign" => bad["messages"][0]["left"]["session_id"] = "other".into(),
                "seq" => bad["messages"][0]["right"]["seq"] = (-1).into(),
                "duplicate" => {
                    let msg = bad["messages"][0].clone();
                    bad["messages"].as_array_mut().unwrap().push(msg);
                }
                "semantic" => bad["messages"][0]["right"]["meta"] = "different".into(),
                "missing" => {
                    bad["left"]["usage"]
                        .as_object_mut()
                        .unwrap()
                        .remove("requests");
                }
                "range" => {
                    bad["left"]["usage"]["known_records_cost_range"] = serde_json::json!([2, 1])
                }
                "negative" => bad["right"]["usage"]["recorded_prompt_tokens"] = (-1).into(),
                "echo" => bad["left"]["session"]["id"] = "other".into(),
                "count" => {
                    bad["messages"] =
                        serde_json::json!(vec![value["messages"][0].clone(); MAX_MESSAGES + 1])
                }
                _ => unreachable!(),
            }
            assert!(Report::decode(&pair, bad).is_err(), "{path}");
        }
        assert!(Report::decode(
            &Pair {
                left: "same".into(),
                right: "same".into()
            },
            value
        )
        .is_err());
    }
    #[test]
    fn comparison_bounds_raw_unknown_fields_and_pretty_expansion() {
        let pair = pair();
        let mut value = test_value(&pair);
        value["unknown"] = Value::String("x".repeat(MAX_BYTES));
        assert!(Report::decode(&pair, value).is_err());
        let mut value = test_value(&pair);
        // Compact input fits, but canonical pretty JSON must also fit.
        let message = value["messages"][0].clone();
        value["messages"] =
            serde_json::json!([{"kind":"left_only","left":message["left"],"right":null}]);
        value["messages"][0]["left"]["content"] = "".into();
        let overhead = serde_json::to_vec(&value).unwrap().len();
        value["messages"][0]["left"]["content"] =
            Value::String("x".repeat(MAX_BYTES - overhead - 100));
        let compact = serde_json::to_vec(&value).unwrap().len();
        assert!(compact < MAX_BYTES);
        assert!(Report::decode(&pair, value).is_err());
    }
    #[test]
    fn comparison_state_retires_callbacks_and_preserves_only_mark_on_back() {
        let pair = pair();
        let mut state = State::default();
        state.mark(pair.left.clone());
        let operation = uuid::Uuid::new_v4();
        state.start(4, operation, pair.clone());
        let revision = state.revision;
        assert!(!state.receive(5, operation, &pair));
        assert!(!state.receive(4, uuid::Uuid::new_v4(), &pair));
        assert!(state.receive(4, operation, &pair));
        assert_ne!(state.revision, revision);
        assert!(!state.receive(4, operation, &pair));
        state.clear_result("back");
        assert_eq!(state.left.as_deref(), Some("left"));
        assert!(state.pair.is_none());
        state.start(4, operation, pair.clone());
        state.reset("attachment changed");
        assert!(state.left.is_none());
        assert!(!state.receive(4, operation, &pair));
    }
}
