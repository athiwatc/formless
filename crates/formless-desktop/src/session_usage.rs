//! Recorded usage, not a current context meter or an atomic billing snapshot.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session_id: String,
    pub connection: u64,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Counters {
    steps: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_write_tokens: Option<i64>,
    cost: Option<f64>,
    #[serde(default)]
    unpriced: Vec<String>,
}

impl Counters {
    fn decode(value: Value) -> Result<Self, String> {
        if value.get("cost").is_none() {
            return Err("Missing recorded cost availability.".into());
        }
        let counters: Self =
            serde_json::from_value(value).map_err(|_| "Malformed recorded usage.".to_string())?;
        if [
            counters.steps,
            counters.prompt_tokens,
            counters.completion_tokens,
            counters.cached_tokens,
        ]
        .into_iter()
        .any(|n| n < 0)
            || counters.cache_write_tokens.is_some_and(|n| n < 0)
            || counters.cost.is_some_and(|n| !n.is_finite() || n < 0.)
            || counters.unpriced.len() > 64
            || counters
                .unpriced
                .iter()
                .any(|model| model.len() > 256 || model.is_empty())
            || (counters.cost.is_some() && !counters.unpriced.is_empty())
        {
            return Err("Unsupported recorded usage values. Use Formless TUI.".into());
        }
        Ok(counters)
    }
    fn text(&self) -> String {
        let optional = |n: Option<i64>| n.map_or_else(|| "not reported".into(), |n| n.to_string());
        let cost = self.cost.map_or_else(
            || "unavailable (not zero)".into(),
            |cost| format!("estimated USD {cost}"),
        );
        let unpriced = if self.unpriced.is_empty() {
            String::new()
        } else {
            format!("\nUnpriced models: {}", self.unpriced.join(", "))
        };
        format!("Steps: {}\nPrompt tokens: {}\nCompletion tokens: {}\nCached read tokens: {}\nCache write tokens (part of prompt): {}\nCost: {cost}{unpriced}",self.steps,self.prompt_tokens,self.completion_tokens,self.cached_tokens,optional(self.cache_write_tokens))
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Report {
    runs: u64,
    total: Counters,
    last_run: Option<Counters>,
}
impl Report {
    pub fn decode(value: Value) -> Result<Self, String> {
        let runs = value
            .get("runs")
            .and_then(Value::as_u64)
            .ok_or("Missing recorded Run count.")?;
        let total = Counters::decode(
            value
                .get("total")
                .cloned()
                .ok_or("Missing Session usage.")?,
        )?;
        let last = value
            .get("last_run")
            .ok_or("Missing last Run availability.")?;
        let last_run = if last.is_null() {
            None
        } else {
            Some(Counters::decode(last.clone())?)
        };
        if runs > 100_000 {
            return Err("Unsupported recorded Run count. Use Formless TUI.".into());
        }
        Ok(Self {
            runs,
            total,
            last_run,
        })
    }
    pub fn text(&self) -> String {
        format!("Session totals\n{}\n\nRuns returned in recent Profile window: {}\n\nLast Run usage (Run ID not supplied)\n{}",self.total.text(),self.runs,self.last_run.as_ref().map_or_else(||"Not available.".into(),Counters::text))
    }
    /// Only this validated projection; unknown fields and context meter fields
    /// are deliberately excluded. Optional omitted counters stay omitted.
    pub fn copy(&self) -> String {
        serde_json::to_string_pretty(self).expect("bounded recorded counters serialize")
    }
}

#[derive(Debug)]
pub struct State {
    pub revision: uuid::Uuid,
    pub report: Option<Report>,
    pub notice: String,
    pub target: Option<Target>,
    pending: Option<uuid::Uuid>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            revision: uuid::Uuid::new_v4(),
            report: None,
            notice: "Read recorded usage for the open Session.".into(),
            target: None,
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
        self.report = None;
        self.pending = None;
        self.target = None;
        self.notice = notice.into();
    }
    pub fn start(&mut self, target: Target, operation: uuid::Uuid) {
        self.retire(
            "Reading recorded usage. Earlier values are unavailable until this reply arrives.",
        );
        self.target = Some(target);
        self.pending = Some(operation);
    }
    pub fn receive(
        &mut self,
        target: &Target,
        operation: uuid::Uuid,
        result: Result<Report, String>,
    ) -> bool {
        if self.target.as_ref() != Some(target) || self.pending != Some(operation) {
            return false;
        }
        self.pending = None;
        self.revision = uuid::Uuid::new_v4();
        match result {
            Ok(report) => {
                self.report = Some(report);
                self.notice="Past recorded read, not live billing or a current context meter. Totals, last Run and count are sampled separately. Run count can undercount: the daemon scans up to 100,000 recent Profile Runs. Read again to refresh. Copy includes displayed fields only.".into();
            }
            Err(error) => {
                self.report = None;
                self.notice = error.chars().take(500).collect();
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn usage() -> Value {
        json!({"steps":1,"prompt_tokens":30,"completion_tokens":5,"cached_tokens":10,"cost":null})
    }
    fn report() -> Value {
        json!({"total":usage(),"last_run":null,"runs":0})
    }
    #[test]
    fn absent_optional_counters_unknown_cost_and_independent_sample_times_are_not_zeroed_or_guessed(
    ) {
        let decoded = Report::decode(report()).unwrap();
        assert!(decoded.text().contains("unavailable (not zero)"));
        assert!(decoded.text().contains("not reported"));
        let copy: Value = serde_json::from_str(&decoded.copy()).unwrap();
        assert!(copy["total"].get("cache_write_tokens").is_none());
        let mut value = report();
        value["last_run"] = usage();
        value["total"]["cost"] = json!(0.00123456);
        value["last_run"]["cost"] = json!(0.);
        value["total"]["context_tokens"] = json!(5000);
        value["total"]["future_field"] = json!("ignored");
        let decoded = Report::decode(value).unwrap();
        assert!(decoded.text().contains("0.00123456"));
        assert!(!decoded.copy().contains("context_tokens"));
        assert!(!decoded.copy().contains("future_field"));
        let mut tiny = report();
        tiny["total"]["cost"] = json!(0.000000004);
        assert!(
            Report::decode(tiny)
                .unwrap()
                .text()
                .contains("estimated USD 0.000000004"),
            "a known nonzero estimate must not display as zero"
        );
    }
    #[test]
    fn malformed_negative_oversized_and_conflicting_usage_is_unavailable() {
        assert!(Report::decode(Value::Null).is_err());
        for key in [
            "steps",
            "prompt_tokens",
            "completion_tokens",
            "cached_tokens",
            "cache_write_tokens",
            "cost",
        ] {
            let mut value = report();
            value["total"][key] = json!(-1);
            assert!(Report::decode(value).is_err(), "{key}");
        }
        for key in ["total", "last_run", "runs"] {
            let mut value = report();
            value.as_object_mut().unwrap().remove(key);
            assert!(Report::decode(value).is_err());
        }
        for models in [
            vec!["x".repeat(257)],
            vec!["x".into(); 65],
            vec![String::new()],
        ] {
            let mut value = report();
            value["total"]["unpriced"] = json!(models);
            assert!(Report::decode(value).is_err());
        }
        let mut value = report();
        value["total"]["unpriced"] = json!(["unpriced-model"]);
        value["total"]["cost"] = json!(1.);
        assert!(Report::decode(value).is_err());
        let mut value = report();
        value["runs"] = json!(100001);
        assert!(Report::decode(value).is_err());
    }
    #[test]
    fn old_operations_selection_generations_and_connections_cannot_own_new_usage() {
        let target = Target {
            session_id: "s".into(),
            connection: 1,
            generation: 2,
        };
        let mut state = State::default();
        let old = uuid::Uuid::new_v4();
        let new = uuid::Uuid::new_v4();
        state.start(target.clone(), old);
        state.start(target.clone(), new);
        for wrong in [
            Target {
                session_id: "other".into(),
                ..target.clone()
            },
            Target {
                connection: 2,
                ..target.clone()
            },
            Target {
                generation: 3,
                ..target.clone()
            },
        ] {
            assert!(!state.receive(&wrong, new, Ok(Report::decode(report()).unwrap())));
        }
        assert!(!state.receive(&target, old, Ok(Report::decode(report()).unwrap())));
        assert!(state.receive(&target, new, Ok(Report::decode(report()).unwrap())));
        let version = state.revision;
        state.retire("Disconnected");
        assert_ne!(state.revision, version);
        assert!(state.report.is_none());
        assert!(!state.receive(&target, new, Ok(Report::decode(report()).unwrap())));
    }
}
