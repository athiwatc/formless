//! Reviewed Run and Session mutations. Transport loss never authorizes a retry.
use formless_protocol::{ForecastInput, Request};
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub const MAX_TEXT: usize = 64 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Edit,
    Retry,
    Good,
    Bad,
    ProviderRetry,
    Compact,
    Delete,
    Cwd,
}
impl Kind {
    pub const ALL: [Self; 8] = [
        Self::Edit,
        Self::Retry,
        Self::Good,
        Self::Bad,
        Self::ProviderRetry,
        Self::Compact,
        Self::Delete,
        Self::Cwd,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Edit => "Edit last user message",
            Self::Retry => "Retry last run",
            Self::Good => "Thumbs up",
            Self::Bad => "Thumbs down",
            Self::ProviderRetry => "Retry provider request",
            Self::Compact => "Compact session",
            Self::Delete => "Delete session",
            Self::Cwd => "Set working directory",
        }
    }
    pub fn help(self) -> &'static str {
        match self {
        Self::Edit => "Replaces the last user message and removes everything after it. Starts provider work after forecast consent. The composer draft is kept.",
        Self::Retry => "Re-runs the last user message. The original answer is preserved in a fork. Optional alternate model applies to this Run only. Starts provider work after forecast consent.",
        Self::Compact => "Invokes the provider to summarize this Session. No compaction cost estimate is available. Replaces history in the context with a checkpoint; persisted messages remain. Optional focus below.",
        Self::Delete => "Stops any active Run and deletes this Session. Review the exact Session identity before confirming. Nothing is replayed after a disconnect.",
        Self::Cwd => "Directory on the daemon's machine, not this desktop. Empty clears the override. Applies at the next step of an active Run. Does not start provider work.",
        Self::ProviderRetry => "Re-sends the in-flight provider request in the already-admitted Run. It may incur additional provider cost. Does not start a new Run.",
        _ => "Records your verdict on the exact latest finished Run. Optional comment below. Does not start provider work.",
    }
    }
    pub fn input_label(self) -> &'static str {
        match self {
            Self::Edit => "Replacement user message",
            Self::Retry => "Alternate model (empty uses current model)",
            Self::Compact => "Compaction focus (optional)",
            Self::Cwd => "Working directory (empty clears)",
            _ => "Comment (optional)",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub client: uuid::Uuid,
    pub connection: u64,
    pub epoch: u64,
    pub generation: u64,
    pub session: String,
}
#[derive(Clone, Debug)]
pub struct DeleteUndo {
    pub owner: Owner,
    pub operation: uuid::Uuid,
    pub deadline: std::time::Instant,
}
impl DeleteUndo {
    pub fn available(&self) -> bool {
        std::time::Instant::now() < self.deadline
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub compacting: bool,
    pub user_oversized: bool,
    pub session: String,
    pub running: bool,
    pub run: Option<String>,
    pub run_status: Option<String>,
    pub user: Option<(String, String)>,
    pub cwd: Option<String>,
    pub last_seq: Option<u64>,
}
fn identity(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
impl Target {
    pub fn decode(session: &str, value: &Value) -> Result<Self, String> {
        let bad =
            "Unsupported or oversized action target; reload this Session or use Formless TUI.";
        if !identity(session) || value["session"]["id"].as_str() != Some(session) {
            return Err(bad.into());
        }
        let messages = value["messages"].as_array().ok_or(bad)?;
        let mut user_oversized = false;
        let user = messages
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .map(|m| {
                let id = m["id"].as_str().filter(|id| identity(id)).ok_or(bad)?;
                let text = m["content"].as_str().ok_or(bad)?;
                user_oversized = text.len() > MAX_TEXT;
                if m["session_id"].as_str() != Some(session) {
                    return Err(bad);
                }
                Ok((
                    id.to_string(),
                    if user_oversized {
                        String::new()
                    } else {
                        text.to_string()
                    },
                ))
            })
            .transpose()
            .map_err(str::to_string)?;
        let run = &value["latest_run"];
        if !run.is_null()
            && (run["session_id"].as_str() != Some(session)
                || !run["run_id"].as_str().is_some_and(identity))
        {
            return Err(bad.into());
        }
        Ok(Self {
            compacting: false,
            user_oversized,
            session: session.into(),
            running: value["running"].as_bool().ok_or(bad)?,
            run: run["run_id"].as_str().map(str::to_string),
            run_status: run["status"].as_str().map(str::to_string),
            user,
            cwd: value["session"]["cwd"].as_str().map(str::to_string),
            last_seq: messages.last().and_then(|m| m["seq"].as_u64()),
        })
    }
    pub fn allowed(&self, kind: Kind) -> bool {
        match kind {
            Kind::ProviderRetry => self.running && self.run.is_some(),
            Kind::Edit => {
                !self.user_oversized && !self.compacting && !self.running && self.user.is_some()
            }
            Kind::Retry => {
                !self.compacting && !self.running && self.user.is_some() && self.run.is_some()
            }
            Kind::Good | Kind::Bad => {
                !self.running
                    && self.run.is_some()
                    && self
                        .run_status
                        .as_deref()
                        .is_some_and(|s| !matches!(s, "running" | "paused"))
            }
            Kind::Compact => !self.compacting && !self.running && self.last_seq.is_some(),
            Kind::Delete | Kind::Cwd => true,
        }
    }
    pub fn unavailable_reason(&self, kind: Kind) -> &'static str {
        if kind == Kind::Edit && self.user_oversized {
            "Edit unavailable: the last user message exceeds the 64 KiB desktop edit limit. Other actions remain available."
        } else if self.compacting && matches!(kind, Kind::Edit | Kind::Retry | Kind::Compact) {
            "Compaction is still pending. Wait for its completion before rewriting history or starting work."
        } else {
            "Action unavailable for the current Run state. Close and reload after the Run changes."
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub operation: uuid::Uuid,
    pub owner: Owner,
    pub target: Target,
    pub kind: Kind,
    pub text: String,
}
impl Action {
    pub fn valid(&self) -> bool {
        identity(&self.owner.session)
            && self.owner.session == self.target.session
            && self.target.allowed(self.kind)
            && self.text.len() <= MAX_TEXT
            && (self.kind != Kind::Edit || !self.text.trim().is_empty())
            && (self.kind != Kind::Cwd || !self.text.chars().any(char::is_control))
    }
    pub fn forecast(&self) -> Option<ForecastInput> {
        matches!(self.kind, Kind::Edit | Kind::Retry).then(|| ForecastInput {
            session_id: Some(self.owner.session.clone()),
            draft: if self.kind == Kind::Edit {
                self.text.clone()
            } else {
                String::new()
            },
            edit_message_id: if self.kind == Kind::Edit {
                self.target.user.as_ref().map(|u| u.0.clone())
            } else {
                None
            },
            retry: self.kind == Kind::Retry,
            model: if self.kind == Kind::Retry {
                optional(&self.text)
            } else {
                None
            },
            ..Default::default()
        })
    }
    pub fn request(&self) -> Request {
        let session_id = self.owner.session.clone();
        match self.kind {
            Kind::Edit => Request::EditMessage {
                session_id,
                message_id: self.target.user.as_ref().unwrap().0.clone(),
                content: self.text.clone(),
            },
            Kind::Retry => Request::RetryRun {
                session_id,
                expected_run_id: self.target.run.clone(),
                model: optional(&self.text),
                keep_original: true,
            },
            Kind::Good | Kind::Bad => Request::RateRun {
                session_id,
                run_id: self.target.run.clone(),
                rating: if self.kind == Kind::Good {
                    "good"
                } else {
                    "bad"
                }
                .into(),
                comment: optional(&self.text),
            },
            Kind::ProviderRetry => Request::RetryProviderRequest { session_id },
            Kind::Compact => Request::CompactSession {
                session_id,
                focus: optional(&self.text),
            },
            Kind::Delete => Request::DeleteSession { id: session_id },
            Kind::Cwd => Request::SetSessionCwd {
                id: session_id,
                cwd: optional(&self.text),
            },
        }
    }
}
fn optional(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.trim().to_string())
}
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Idle,
    Reviewing(uuid::Uuid),
    Sent(uuid::Uuid),
    Applied(String),
    Reconciled,
    Rejected(String),
    Unknown,
}
impl Status {
    pub fn blocked(&self) -> bool {
        matches!(self, Self::Reviewing(_) | Self::Sent(_) | Self::Unknown)
    }
    pub fn notice(&self) -> String {
        match self {
        Self::Idle => "Choose an action. Nothing is sent until explicitly confirmed.".into(),
        Self::Reviewing(_) => "Reviewing the exact action. No mutation has been written.".into(),
        Self::Sent(_) => "Submitted; awaiting acknowledgement. Never replayed.".into(),
        Self::Applied(s) => format!("Acknowledged: {s}"), Self::Rejected(s) => format!("Not applied: {s}"),
        Self::Reconciled => "Canonical Session state reloaded after an uncertain mutation. Some changes may have persisted. Nothing was replayed.".into(),
            Self::Unknown => "Outcome UNKNOWN. The action may already have applied. Further run actions are blocked until canonical state confirms the outcome. Refresh the Session to check again. Nothing was replayed.".into(),
    }
    }
}
#[derive(Default, Debug)]
pub struct Ledger(Mutex<Status>);
impl Ledger {
    pub fn status(&self) -> Status {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn reserve(self: &Arc<Self>, action: Action) -> Result<Dispatch, String> {
        let mut status = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !action.valid() || status.blocked() {
            return Err(
                "Action unavailable, oversized, pending or unknown. Reload before reviewing again."
                    .into(),
            );
        }
        *status = Status::Reviewing(action.operation);
        Ok(Dispatch {
            action,
            ledger: self.clone(),
        })
    }
    pub fn cancel(&self) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*s, Status::Reviewing(_)) {
            *s = Status::Rejected("Review cancelled or retired; nothing was sent.".into());
        }
    }
    pub fn cancel_operation(&self, operation: uuid::Uuid) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if *s == Status::Reviewing(operation) {
            *s = Status::Rejected("Review retired before writing; nothing was sent.".into());
        }
    }
}
#[derive(Clone, Debug)]
pub struct Dispatch {
    pub action: Action,
    ledger: Arc<Ledger>,
}
impl Dispatch {
    pub fn acknowledged(&self, value: &Value) -> bool {
        receipt(&self.action, value)
    }
    pub fn reserved(&self) -> bool {
        self.ledger.status() == Status::Reviewing(self.action.operation)
    }
    pub fn reject(&self, reason: &str) {
        let mut s = self.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if *s == Status::Reviewing(self.action.operation) {
            *s = Status::Rejected(reason.chars().take(500).collect());
        }
    }
    pub fn before_write(&self) -> bool {
        let mut s = self.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if *s != Status::Reviewing(self.action.operation) {
            return false;
        }
        *s = Status::Sent(self.action.operation);
        true
    }
    pub fn finish(&self, result: Result<Value, String>, definite_rejection: bool) {
        let mut s = self.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if *s != Status::Sent(self.action.operation) {
            return;
        }
        *s = match result {
            Err(e) if definite_rejection => Status::Rejected(e.chars().take(500).collect()),
            Ok(v) if receipt(&self.action, &v) => Status::Applied(
                if self.action.kind == Kind::ProviderRetry && v["retrying"] == false {
                    "No provider request was available; nothing was retried.".into()
                } else {
                    self.action.kind.label().into()
                },
            ),
            _ => Status::Unknown,
        };
    }
    pub fn unknown(&self) -> bool {
        self.ledger.status() == Status::Unknown
    }
    pub fn reconcile(&self, unchanged_error: Option<&str>) {
        let mut status = self.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if *status == Status::Unknown {
            *status = unchanged_error.map_or(Status::Reconciled, |e| {
                Status::Rejected(e.chars().take(500).collect())
            });
        }
    }
    pub fn lost(&self) {
        let mut s = self.ledger.0.lock().unwrap_or_else(|e| e.into_inner());
        if *s == Status::Sent(self.action.operation) {
            *s = Status::Unknown;
        } else if *s == Status::Reviewing(self.action.operation) {
            *s = Status::Rejected("Review retired before writing; nothing was sent.".into());
        }
    }
}
fn receipt(a: &Action, v: &Value) -> bool {
    match a.kind {
        Kind::Edit => {
            v["session_id"] == a.owner.session
                && v["id"].as_str() == a.target.user.as_ref().map(|u| u.0.as_str())
                && v["content"] == a.text
        }
        Kind::Retry => {
            v.get("model").is_some_and(|m| {
                m.as_str() == optional(&a.text).as_deref() && (m.is_null() || m.is_string())
            }) && v
                .get("original_session")
                .is_some_and(|id| id.is_null() || id.as_str().is_some_and(identity))
        }
        Kind::Good | Kind::Bad => {
            v["run_id"].as_str() == a.target.run.as_deref()
                && v["rating"] == if a.kind == Kind::Good { "good" } else { "bad" }
        }
        Kind::ProviderRetry => v["retrying"].is_boolean(),
        Kind::Compact => v["started"] == true && v["up_to"].as_u64().is_some(),
        Kind::Delete => v["deleted"] == true,
        Kind::Cwd => {
            v["id"] == a.owner.session
                && if optional(&a.text).is_none() {
                    v.get("cwd").is_some_and(Value::is_null)
                } else {
                    v["cwd"].as_str().is_some_and(|s| !s.is_empty())
                }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub used: u64,
    pub limit: u64,
    pub measured: bool,
}
impl Context {
    pub fn decode(v: &Value) -> Result<Self, String> {
        Ok(Self {
            used: v["used_tokens"].as_u64().ok_or("Missing context usage")?,
            limit: v["context_tokens"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or("Context limit unavailable")?,
            measured: v["measured"]
                .as_bool()
                .ok_or("Context source unavailable")?,
        })
    }
    pub fn label(&self) -> String {
        format!(
            "Context {}% · {} / {} tokens · {}",
            self.used
                .saturating_mul(100)
                .checked_div(self.limit)
                .unwrap_or(0),
            self.used,
            self.limit,
            if self.measured {
                "measured"
            } else {
                "estimated"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn oversized_prompt_only_disables_edit_without_retaining_the_prompt() {
        for size in [0, MAX_TEXT, MAX_TEXT + 1] {
            let mut value = snapshot();
            value["messages"][0]["content"] = json!("x".repeat(size));
            let target = Target::decode("s", &value).unwrap();
            assert_eq!(target.allowed(Kind::Edit), size <= MAX_TEXT);
            assert!(target.user.as_ref().unwrap().1.len() <= MAX_TEXT);
            for kind in [
                Kind::Retry,
                Kind::Good,
                Kind::Bad,
                Kind::Compact,
                Kind::Delete,
                Kind::Cwd,
            ] {
                assert!(target.allowed(kind));
            }
            if size > MAX_TEXT {
                assert!(target.unavailable_reason(Kind::Edit).contains("64 KiB"));
            }
        }
    }

    pub(super) fn snapshot() -> Value {
        json!({"session":{"id":"s","cwd":null},"running":false,
            "latest_run":{"session_id":"s","run_id":"r","status":"completed"},
            "messages":[{"session_id":"s","id":"u","role":"user","content":"original 😀","seq":1},
                {"session_id":"s","id":"a","role":"assistant","content":"answer","seq":2}]})
    }
    fn action(kind: Kind) -> Action {
        let mut target = Target::decode("s", &snapshot()).unwrap();
        target.running = kind == Kind::ProviderRetry;
        Action {
            operation: uuid::Uuid::new_v4(),
            owner: Owner {
                client: uuid::Uuid::new_v4(),
                connection: 1,
                epoch: 2,
                generation: 3,
                session: "s".into(),
            },
            target,
            kind,
            text: if kind == Kind::Edit {
                "replacement 😀".into()
            } else {
                String::new()
            },
        }
    }
    #[test]
    fn targets_are_exact_bounded_and_empty_sessions_still_allow_metadata_actions() {
        let t = Target::decode("s", &snapshot()).unwrap();
        assert_eq!(t.user.unwrap(), ("u".into(), "original 😀".into()));
        assert!(Target::decode("other", &snapshot()).is_err());
        for path in ["session_id", "run_id"] {
            let mut v = snapshot();
            v["latest_run"][path] = json!("");
            assert!(Target::decode("s", &v).is_err());
        }
        let mut v = snapshot();
        v["messages"][0]["content"] = json!("x".repeat(MAX_TEXT + 1));
        let target = Target::decode("s", &v).unwrap();
        assert!(!target.allowed(Kind::Edit));
        assert!(target.allowed(Kind::Delete));
        let t = Target::decode(
            "s",
            &json!({"session":{"id":"s"},"running":false,"messages":[]}),
        )
        .unwrap();
        for k in Kind::ALL {
            assert_eq!(t.allowed(k), matches!(k, Kind::Delete | Kind::Cwd));
        }
    }
    #[test]
    fn forecast_and_requests_preserve_exact_edit_and_retry_semantics() {
        let a = action(Kind::Edit);
        let input = a.forecast().unwrap();
        assert_eq!(input.draft, a.text);
        assert_eq!(input.edit_message_id.as_deref(), Some("u"));
        assert!(!input.retry);
        assert!(
            matches!(a.request(), Request::EditMessage { message_id, content, .. } if message_id == "u" && content == a.text)
        );
        let mut a = action(Kind::Retry);
        a.text = "  alternate-model  ".into();
        let input = a.forecast().unwrap();
        assert!(input.retry);
        assert_eq!(input.model.as_deref(), Some("alternate-model"));
        assert!(
            matches!(a.request(), Request::RetryRun { expected_run_id: Some(run), model: Some(model), keep_original: true, .. } if run == "r" && model == "alternate-model")
        );
        for kind in [
            Kind::Good,
            Kind::Bad,
            Kind::ProviderRetry,
            Kind::Compact,
            Kind::Delete,
            Kind::Cwd,
        ] {
            assert!(action(kind).forecast().is_none());
        }
        assert!(Kind::Compact.help().contains("Invokes the provider"));
        assert!(Kind::Compact.help().contains("No compaction cost estimate"));
    }
    #[test]
    fn input_and_running_state_fail_closed() {
        let mut a = action(Kind::Edit);
        a.text = " \n ".into();
        assert!(!a.valid());
        a.text = "x".repeat(MAX_TEXT + 1);
        assert!(!a.valid());
        a.text = "valid".into();
        a.target.running = true;
        assert!(!a.valid());
        let mut a = action(Kind::Cwd);
        a.text = "/a\nb".into();
        assert!(!a.valid());
        for kind in [Kind::Cwd, Kind::Delete] {
            let mut a = action(kind);
            a.target.running = true;
            assert!(a.valid());
            assert!(a.forecast().is_none());
        }
        let mut a = action(Kind::Good);
        a.target.run_status = Some("paused".into());
        assert!(!a.valid());
        a.target.run_status = None;
        assert!(!a.valid());
        let mut a = action(Kind::Delete);
        a.owner.session = "elsewhere".into();
        assert!(!a.valid());
    }
    #[test]
    fn every_rpc_requires_its_own_acknowledgement_and_never_replays_unknown_writes() {
        let receipts = [
            json!({"session_id":"s","id":"u","content":"replacement 😀"}),
            json!({"model":null,"original_session":null}),
            json!({"run_id":"r","rating":"good"}),
            json!({"run_id":"r","rating":"bad"}),
            json!({"retrying":false}),
            json!({"started":true,"up_to":2}),
            json!({"deleted":true}),
            json!({"id":"s","cwd":null}),
        ];
        for (kind, value) in Kind::ALL.into_iter().zip(receipts) {
            let ledger = Arc::new(Ledger::default());
            let a = action(kind);
            let d = ledger.reserve(a.clone()).unwrap();
            assert!(ledger.reserve(a.clone()).is_err());
            assert!(d.before_write());
            assert!(!d.before_write());
            assert!(d.acknowledged(&value));
            d.finish(Ok(value), true);
            assert!(matches!(ledger.status(), Status::Applied(_)));
            d.lost();
            assert!(matches!(ledger.status(), Status::Applied(_)));
            let d = ledger.reserve(a.clone()).unwrap();
            assert!(d.before_write());
            d.finish(Ok(json!({})), true);
            assert_eq!(ledger.status(), Status::Unknown);
            assert!(ledger.reserve(a).is_err());
        }
    }
    #[test]
    fn cancellation_stale_dispatch_errors_and_transport_loss_are_distinct() {
        let ledger = Arc::new(Ledger::default());
        let a = action(Kind::Compact);
        let old = ledger.reserve(a.clone()).unwrap();
        ledger.cancel();
        assert!(!old.before_write());
        let mut fresh = a;
        fresh.operation = uuid::Uuid::new_v4();
        let new = ledger.reserve(fresh).unwrap();
        old.lost();
        old.reject("stale");
        assert!(new.reserved());
        assert!(new.before_write());
        new.finish(Err("daemon refused".into()), false);
        assert_eq!(ledger.status(), Status::Unknown);
        assert!(ledger.reserve(action(Kind::Compact)).is_err());
        let ledger = Arc::new(Ledger::default());
        let d = ledger.reserve(action(Kind::Compact)).unwrap();
        assert!(d.before_write());
        ledger.cancel();
        d.lost();
        assert_eq!(ledger.status(), Status::Unknown);
    }
    #[test]
    fn context_usage_is_explicit_about_estimates_and_handles_empty_and_overflow() {
        assert_eq!(
            Context::decode(&json!({"used_tokens":0,"context_tokens":100,"measured":false}))
                .unwrap()
                .label(),
            "Context 0% · 0 / 100 tokens · estimated"
        );
        assert!(Context::decode(&json!({})).is_err());
        assert!(
            Context::decode(&json!({"used_tokens":1,"context_tokens":0,"measured":true})).is_err()
        );
        assert!(
            Context::decode(&json!({"used_tokens":-1,"context_tokens":100,"measured":true}))
                .is_err()
        );
        assert!(Context {
            used: u64::MAX,
            limit: 1,
            measured: true
        }
        .label()
        .contains("measured"));
    }
}
