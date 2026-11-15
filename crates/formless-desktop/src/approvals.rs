//! Bounded, explicit per-call security decisions; no persistent grants or path widening.
use formless_protocol::{ApprovalScope, Event, Request};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_OPERATIONS: usize = 256;
fn directional(c: char) -> bool {
    matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')
}
fn identity(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(|c| c.is_control() || directional(c))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    id: String,
    session: String,
    tool: String,
    version: i64,
    raw: Arc<Value>,
}
impl Prompt {
    pub fn from_list(value: &Value) -> Result<Option<Self>, &'static str> {
        let rows = value
            .as_array()
            .ok_or("Approval inventory is unavailable; use Formless TUI.")?;
        if rows.is_empty() {
            return Ok(None);
        }
        if rows.len() != 1 {
            return Err("Native review requires one pending approval. Review multiple approvals in Formless TUI.");
        }
        let raw = &rows[0];
        let object = raw
            .as_object()
            .ok_or("Invalid approval; use Formless TUI.")?;
        const KEYS: &[&str] = &[
            "type",
            "context",
            "approval_id",
            "session_id",
            "tool_name",
            "version",
            "capabilities",
            "memory_mb",
            "detail",
            "already_allowed",
            "code_preview",
            "tool_id",
            "batch",
            "path_options",
        ];
        if object.keys().any(|k| !KEYS.contains(&k.as_str()))
            || serde_json::to_vec(raw)
                .map_err(|_| "Invalid approval")?
                .len()
                > MAX_PROMPT_BYTES
        {
            return Err("Approval contains unsupported or oversized data; use Formless TUI.");
        }
        let event: Event = serde_json::from_value(raw.clone())
            .map_err(|_| "Approval shape is unsupported; use Formless TUI.")?;
        let Event::ApprovalRequested {
            context: Some(context),
            approval_id,
            session_id,
            tool_name,
            version,
            tool_id: Some(tool_id),
            batch: None,
            capabilities,
            already_allowed,
            path_options,
            ..
        } = event
        else {
            return Err("This approval needs the existing Formless TUI review.");
        };
        if !identity(&approval_id)
            || !identity(&session_id)
            || !identity(&tool_id)
            || !identity(&tool_name)
            || version <= 0
            || !context.run_id.as_deref().is_some_and(identity)
            || !context.call.as_ref().is_some_and(|c| !c.truncated)
            || capabilities.len() > 64
            || already_allowed.len() > 64
            || path_options.len() > 64
            || capabilities
                .iter()
                .chain(&already_allowed)
                .any(|c| c.len() > 1024 || c.chars().any(char::is_control))
        {
            return Err("Complete bounded Tool call evidence is required; use Formless TUI.");
        }
        let ctx = raw["context"]
            .as_object()
            .ok_or("Missing approval context")?;
        if ctx
            .keys()
            .any(|k| !["run_id", "call", "sources", "scope_warnings"].contains(&k.as_str()))
        {
            return Err("Approval context has unsupported fields; use Formless TUI.");
        }
        let call = raw["context"]["call"]
            .as_object()
            .ok_or("Missing call evidence")?;
        if call
            .keys()
            .any(|k| !["arguments", "urls", "paths", "truncated", "note"].contains(&k.as_str()))
        {
            return Err("Approval call has unsupported fields; use Formless TUI.");
        }
        Ok(Some(Self {
            id: approval_id,
            session: session_id,
            tool: tool_name,
            version,
            raw: Arc::new(raw.clone()),
        }))
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn session(&self) -> &str {
        &self.session
    }
    pub fn tool(&self) -> &str {
        &self.tool
    }
    pub fn version(&self) -> i64 {
        self.version
    }
    pub fn display(&self) -> String {
        let json = serde_json::to_string_pretty(self.raw.as_ref()).expect("JSON value");
        json.chars().map(|c|if matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'){format!("\\u{:04x}",c as u32)}else{c.to_string()}).collect()
    }
    // Prompt construction is sealed: all decision evidence was validated by
    // from_list and remains immutable behind Arc; callers cannot alter fields.
    fn valid(&self) -> bool {
        identity(&self.id) && identity(&self.session) && self.version > 0
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub client: uuid::Uuid,
    pub connection: u64,
    pub epoch: u64,
    pub operation: uuid::Uuid,
    pub prompt: Prompt,
    pub allow: bool,
}
impl Action {
    pub fn valid(&self) -> bool {
        !self.client.is_nil() && !self.operation.is_nil() && self.prompt.valid()
    }
    pub fn request(&self) -> Request {
        Request::ApprovalDecision {
            approval_id: self.prompt.id.clone(),
            approved: self.allow,
            scope: ApprovalScope::Once,
            paths: BTreeMap::new(),
            remember_denial: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Accepted { audit_recorded: bool },
    Rejected(String),
    Unknown,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Reserved(Action),
    Sent(Action),
    Finished {
        action: Action,
        outcome: Outcome,
    },
}
impl Status {
    pub fn blocked(&self) -> bool {
        matches!(
            self,
            Self::Reserved(_)
                | Self::Sent(_)
                | Self::Finished {
                    outcome: Outcome::Unknown,
                    ..
                }
        )
    }
    pub fn action(&self) -> Option<&Action> {
        match self {
            Self::Idle => None,
            Self::Reserved(a) | Self::Sent(a) | Self::Finished { action: a, .. } => Some(a),
        }
    }
    pub fn notice(&self) -> Option<String> {
        match self{
  Self::Idle=>None,Self::Reserved(_)=>Some("Revalidating the exact approval; no decision has been written.".into()),Self::Sent(_)=>Some("A decision may have been written; waiting for its receipt. No automatic retry.".into()),Self::Finished{outcome:Outcome::Accepted{audit_recorded},..}=>Some(if *audit_recorded{"Decision accepted and audit recorded. Tool execution or success is not established."}else{"Decision accepted, but audit recording failed. Tool execution or success is not established."}.into()),Self::Finished{outcome:Outcome::Rejected(s),..}=>Some(s.clone()),Self::Finished{outcome:Outcome::Unknown,..}=>Some("Decision outcome unknown. Native decisions are blocked for this Client; inspect Formless TUI. Nothing is replayed.".into())}
    }
}
#[derive(Debug, Default)]
struct State {
    status: Status,
    operations: HashSet<uuid::Uuid>,
    spent: HashSet<String>,
}
#[derive(Debug, Default)]
pub struct Ledger(Mutex<State>);
impl Ledger {
    pub fn status(&self) -> Status {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn cancel_unsent(&self, reason: &str) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Status::Reserved(a) = &s.status {
            s.status = Status::Finished {
                action: a.clone(),
                outcome: Outcome::Rejected(reason.chars().take(500).collect()),
            }
        }
    }
    pub(crate) fn reserve(self: &Arc<Self>, action: Action) -> Result<Dispatch, &'static str> {
        if !action.valid() {
            return Err("A supported complete approval and fresh operation are required.");
        }
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if s.status.blocked() {
            return Err("A decision is pending or unknown. Do not retry; inspect Formless TUI.");
        }
        if s.spent.contains(&action.prompt.id) || s.operations.contains(&action.operation) {
            return Err("This approval or operation was already spent; no retry was queued.");
        }
        if s.operations.len() >= MAX_OPERATIONS {
            return Err("Native approval receipt capacity reached; use Formless TUI.");
        }
        s.operations.insert(action.operation);
        s.status = Status::Reserved(action.clone());
        Ok(Dispatch(Arc::new(Owner {
            ledger: self.clone(),
            action,
        })))
    }
}
#[derive(Debug)]
struct Owner {
    ledger: Arc<Ledger>,
    action: Action,
}
impl Drop for Owner {
    fn drop(&mut self) {
        let mut s = self.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        let outcome = match &s.status {
            Status::Reserved(a) if a == &self.action => Some(Outcome::Rejected(
                "Decision was not written; review again explicitly.".into(),
            )),
            Status::Sent(a) if a == &self.action => Some(Outcome::Unknown),
            _ => None,
        };
        if let Some(outcome) = outcome {
            s.status = Status::Finished {
                action: self.action.clone(),
                outcome,
            }
        }
    }
}
#[derive(Clone, Debug)]
pub struct Dispatch(Arc<Owner>);
impl Dispatch {
    pub fn action(&self) -> &Action {
        &self.0.action
    }
    pub(crate) fn owned_by(&self, l: &Arc<Ledger>) -> bool {
        Arc::ptr_eq(&self.0.ledger, l)
    }
    pub(crate) fn reserved(&self) -> bool {
        self.0.ledger.status() == Status::Reserved(self.action().clone())
    }
    pub(crate) fn sent(&self) -> bool {
        self.0.ledger.status() == Status::Sent(self.action().clone())
    }
    pub(crate) fn reject(&self, reason: &str) {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Reserved(a)if a==self.action()) {
            s.status = Status::Finished {
                action: self.action().clone(),
                outcome: Outcome::Rejected(reason.chars().take(500).collect()),
            }
        }
    }
    pub(crate) fn before_write(&self) -> bool {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Sent(a)if a==self.action()) {
            return true;
        }
        if matches!(&s.status,Status::Reserved(a)if a==self.action())
            && !s.spent.contains(&self.action().prompt.id)
        {
            s.spent.insert(self.action().prompt.id.clone());
            s.status = Status::Sent(self.action().clone());
            true
        } else {
            false
        }
    }
    pub(crate) fn finish(&self, outcome: Outcome) {
        let mut s = self.0.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(&s.status,Status::Sent(a)if a==self.action()) {
            s.status = Status::Finished {
                action: self.action().clone(),
                outcome,
            }
        }
    }
}
pub fn decode(result: Result<Value, String>, ok: bool) -> Outcome {
    match result {
        Ok(v) if ok && v["resolved"] == true => v["audit_recorded"]
            .as_bool()
            .map_or(Outcome::Unknown, |audit_recorded| Outcome::Accepted {
                audit_recorded,
            }),
        Ok(v) if ok && v["resolved"] == false => {
            Outcome::Rejected("This request did not resolve the approval. It is no longer waiting; another client's decision may already apply.".into())
        }
        Err(e) if !ok => Outcome::Rejected(e.chars().take(500).collect()),
        _ => Outcome::Unknown,
    }
}

#[cfg(any(test, feature = "test-support", feature = "daemon-tests"))]
#[doc(hidden)]
pub fn fixture_inventory() -> Value {
    serde_json::json!([{
     "type":"approval_requested","approval_id":"approval-fixture","session_id":"approval-session","tool_name":"fixture_tool","tool_id":"tool-fixture","version":1,"capabilities":["fs:read:/private-fixture/only"],"already_allowed":[],"memory_mb":8,"detail":null,"code_preview":"export function main(){ return 1; }","batch":null,"path_options":[],
     "context":{"run_id":"run-fixture","call":{"arguments":{"message":"ไทย 漢字 😀"},"urls":[],"paths":["/private-fixture/only"],"truncated":false,"note":"Redacted evidence"},"sources":[],"scope_warnings":[]}
    }])
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn action() -> Action {
        Action {
            client: uuid::Uuid::new_v4(),
            connection: 2,
            epoch: 3,
            operation: uuid::Uuid::new_v4(),
            prompt: Prompt::from_list(&fixture_inventory()).unwrap().unwrap(),
            allow: true,
        }
    }
    #[test]
    fn native_decisions_are_explicit_once_and_never_widen_or_remember() {
        for allow in [false, true] {
            let mut a = action();
            a.allow = allow;
            let wire = serde_json::to_value(a.request()).unwrap();
            assert_eq!(wire["scope"], "once");
            assert_eq!(wire["approved"], allow);
            assert!(
                matches!(a.request(),Request::ApprovalDecision{approval_id,approved,scope:ApprovalScope::Once,paths,remember_denial:false}if approval_id==a.prompt.id&&approved==allow&&paths.is_empty())
            );
        }
    }
    #[test]
    fn evidence_is_complete_bounded_and_unknown_shapes_stay_on_tui() {
        assert!(Prompt::from_list(&json!([])).unwrap().is_none());
        for mode in 0..9 {
            let mut v = fixture_inventory();
            match mode {
                0 => v[0]["context"] = Value::Null,
                1 => v[0]["context"]["call"]["truncated"] = json!(true),
                2 => v[0]["batch"] = json!({}),
                3 => v[0]["new_authority"] = json!(true),
                4 => v[0]["approval_id"] = json!(""),
                5 => v[0]["tool_id"] = Value::Null,
                6 => v[0]["version"] = json!(0),
                7 => v[0]["detail"] = json!("x".repeat(MAX_PROMPT_BYTES)),
                _ => {
                    let row = v[0].clone();
                    v.as_array_mut().unwrap().push(row);
                }
            }
            assert!(Prompt::from_list(&v).is_err(), "{mode}");
        }
    }
    #[test]
    fn display_keeps_json_meaning_and_escapes_direction_controls() {
        let mut v = fixture_inventory();
        v[0]["context"]["call"]["arguments"]["message"] = json!("left\u{202e}right");
        let p = Prompt::from_list(&v).unwrap().unwrap();
        assert!(!p.display().contains('\u{202e}'));
        assert_eq!(serde_json::from_str::<Value>(&p.display()).unwrap(), v[0]);
        v[0]["approval_id"] = json!("id\u{202e}spoof");
        assert!(Prompt::from_list(&v).is_err());
    }
    #[test]
    fn receipt_requires_resolved_and_audit_boolean() {
        for v in [
            json!({}),
            json!({"resolved":true}),
            json!({"resolved":"true","audit_recorded":true}),
            json!({"resolved":true,"audit_recorded":null}),
        ] {
            assert_eq!(decode(Ok(v), true), Outcome::Unknown)
        }
        for audit_recorded in [false, true] {
            assert_eq!(
                decode(
                    Ok(json!({"resolved":true,"audit_recorded":audit_recorded})),
                    true
                ),
                Outcome::Accepted { audit_recorded }
            )
        }
        assert!(matches!(
            decode(Ok(json!({"resolved":false})), true),
            Outcome::Rejected(_)
        ));
        assert_eq!(
            decode(Err("malformed success".into()), true),
            Outcome::Unknown
        );
    }
    #[test]
    fn reservation_drop_cancellation_and_sent_drop_preserve_uncertainty() {
        for sent in [false, true] {
            let l = Arc::new(Ledger::default());
            let d = l.reserve(action()).unwrap();
            if sent {
                assert!(d.before_write());
                l.cancel_unsent("late close");
            }
            drop(d);
            assert_eq!(l.status().blocked(), sent);
            assert!(
                matches!(
                    l.status(),
                    Status::Finished {
                        outcome: Outcome::Unknown,
                        ..
                    }
                ) == sent
            );
            assert_eq!(Arc::strong_count(&l), 1);
        }
        let l = Arc::new(Ledger::default());
        let old = l.reserve(action()).unwrap();
        old.reject("cancel");
        let new = l.reserve(action()).unwrap();
        drop(old);
        assert!(new.reserved());
    }
    #[test]
    fn spent_approval_and_operation_ids_never_replay_and_capacity_fails_closed() {
        let l = Arc::new(Ledger::default());
        let a = action();
        let d = l.reserve(a.clone()).unwrap();
        assert!(d.before_write());
        assert!(d.before_write());
        d.finish(Outcome::Accepted {
            audit_recorded: true,
        });
        let mut repeat = a.clone();
        repeat.operation = uuid::Uuid::new_v4();
        assert!(l.reserve(repeat).is_err());
        assert!(l.reserve(a).is_err());
        for n in 1..MAX_OPERATIONS {
            let mut a = action();
            a.prompt.id = format!("id-{n}");
            let mut v = fixture_inventory();
            v[0]["approval_id"] = json!(a.prompt.id);
            a.prompt = Prompt::from_list(&v).unwrap().unwrap();
            let d = l.reserve(a).unwrap();
            d.reject("not written");
        }
        let mut fresh = action();
        let mut v = fixture_inventory();
        v[0]["approval_id"] = json!("capacity-new");
        fresh.prompt = Prompt::from_list(&v).unwrap().unwrap();
        assert!(l.reserve(fresh).is_err());
    }
}
