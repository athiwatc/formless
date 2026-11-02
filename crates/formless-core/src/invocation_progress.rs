//! Invocation-local progress validation, advancement and emission policy.
//! Display changes are not evidence of work: only a new fractional high-water
//! mark can refresh the daemon's work heartbeat. State dies with its execution.
use formless_runtime::LockExt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const EMISSION_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProgressValue {
    pub fraction: Option<f64>,
    pub label: String,
}

#[derive(Default)]
struct State {
    latest: Option<ProgressValue>,
    high_water: Option<f64>,
    emitted: Option<(Instant, ProgressValue)>,
}

pub(crate) struct InvocationProgress {
    pub id: String,
    state: Mutex<State>,
}

pub(crate) struct Update {
    pub advanced: bool,
    pub emit: Option<ProgressValue>,
}

impl Default for InvocationProgress {
    fn default() -> Self {
        Self::with_id(uuid::Uuid::new_v4().to_string())
    }
}

impl InvocationProgress {
    pub fn with_id(id: String) -> Self {
        Self {
            id,
            state: Mutex::new(State::default()),
        }
    }

    pub fn snapshot(&self) -> Option<ProgressValue> {
        self.state.locked().latest.clone()
    }

    pub fn report(&self, fraction: Option<f64>, label: &str, now: Instant) -> Update {
        // Validate again after the JS boundary. Direct __call/HostOps callers
        // cannot bypass finite range checks, scalar length or control stripping.
        let value = ProgressValue {
            fraction: fraction
                .filter(|value| value.is_finite())
                .map(|value| value.clamp(0.0, 1.0)),
            label: label
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect(),
        };
        let mut state = self.state.locked();
        let advanced = value
            .fraction
            .is_some_and(|fraction| state.high_water.is_none_or(|previous| fraction > previous));
        if advanced {
            state.high_water = value.fraction;
        }
        state.latest = Some(value.clone());
        let emit = match &state.emitted {
            Some((at, previous))
                if previous == &value || now.saturating_duration_since(*at) < EMISSION_INTERVAL =>
            {
                None
            }
            _ => Some(value),
        };
        if let Some(value) = &emit {
            state.emitted = Some((now, value.clone()));
        }
        Update { advanced, emit }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_progress_and_label_changes_do_not_advance_work() {
        let progress = InvocationProgress::default();
        let now = Instant::now();
        assert!(progress.report(Some(0.5), "working", now).advanced);
        for i in 1..1000 {
            let update = progress.report(
                Some(0.5),
                if i % 2 == 0 { "A" } else { "B" },
                now + Duration::from_millis(i),
            );
            assert!(!update.advanced);
        }
        assert!(
            !progress
                .report(Some(0.4), "regressed", now + Duration::from_secs(2))
                .advanced
        );
        assert!(
            progress
                .report(Some(0.6), "advanced", now + Duration::from_secs(3))
                .advanced
        );
        assert!(
            !progress
                .report(None, "unknown", now + Duration::from_secs(4))
                .advanced
        );
    }

    #[test]
    fn hard_rate_cap_has_no_label_or_completion_exception_and_keeps_latest() {
        let progress = InvocationProgress::default();
        let now = Instant::now();
        assert!(progress.report(Some(0.0), "start", now).emit.is_some());
        for i in 1..200 {
            assert!(progress
                .report(
                    Some(1.0),
                    &format!("label {i}"),
                    now + Duration::from_millis(i)
                )
                .emit
                .is_none());
        }
        assert_eq!(progress.snapshot().unwrap().label, "label 199");
        assert!(progress
            .report(Some(1.0), "done", now + Duration::from_millis(200))
            .emit
            .is_some());
        assert!(progress
            .report(Some(1.0), "done", now + Duration::from_secs(5))
            .emit
            .is_none());
        assert!(
            !progress
                .report(Some(0.0), "reset", now + Duration::from_secs(6))
                .advanced
        );
        assert!(
            !progress
                .report(Some(1.0), "toggle", now + Duration::from_secs(7))
                .advanced
        );
    }

    #[test]
    fn direct_inputs_are_validated_and_labels_are_bounded_after_sanitization() {
        let progress = InvocationProgress::default();
        let now = Instant::now();
        for fraction in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                !progress
                    .report(Some(fraction), "\x1b[31m\n\r\tlabel\u{85}", now)
                    .advanced
            );
            let value = progress.snapshot().unwrap();
            assert_eq!(value.fraction, None);
            assert_eq!(value.label, "[31mlabel");
        }
        for (fraction, expected) in [(-0.1, 0.0), (1.1, 1.0)] {
            progress.report(Some(fraction), "bounded", now);
            assert_eq!(progress.snapshot().unwrap().fraction, Some(expected));
        }
        progress.report(None, &"日\n".repeat(200), now);
        let value = progress.snapshot().unwrap();
        assert_eq!(value.label.chars().count(), 120);
        assert!(!value.label.chars().any(char::is_control));
    }

    #[test]
    fn parallel_calls_have_independent_identity_high_water_and_throttle() {
        let a = InvocationProgress::default();
        let b = InvocationProgress::default();
        assert_ne!(a.id, b.id);
        assert!(!a.id.is_empty() && !b.id.is_empty());
        let now = Instant::now();
        assert!(a.report(Some(0.7), "A", now).emit.is_some());
        assert!(b.report(Some(0.2), "B", now).emit.is_some());
        assert!(
            !a.report(Some(0.2), "A regressed", now + EMISSION_INTERVAL)
                .advanced
        );
        assert!(
            b.report(Some(0.3), "B advanced", now + EMISSION_INTERVAL)
                .advanced
        );
        assert_eq!(a.snapshot().unwrap().label, "A regressed");
        assert_eq!(b.snapshot().unwrap().label, "B advanced");
    }

    #[test]
    fn preallocated_execution_identity_is_the_stored_running_invocation() {
        let store = crate::storage::Store::open_in_memory().unwrap();
        let (tool, _) = store
            .create_tool_version(
                "identity",
                "fixture",
                "{}",
                "{}",
                "function run(){return {};}",
                &formless_runtime::Capabilities::default(),
                256,
                "agent",
            )
            .unwrap();
        let progress = InvocationProgress::default();
        let id = store
            .start_invocation_with_id(&progress.id, &tool.id, 1, None, None, None, "{}")
            .unwrap();
        assert_eq!(id, progress.id);
        assert_eq!(
            store.invocation_status(&id).unwrap().as_deref(),
            Some("running")
        );
        assert!(
            store
                .start_invocation_with_id(&progress.id, &tool.id, 1, None, None, None, "{}")
                .is_err(),
            "a duplicate cannot replace an execution's identity"
        );
        let other = store
            .start_invocation(&tool.id, 1, None, None, None, "{}")
            .unwrap();
        assert_ne!(other, id, "the established allocator remains independent");
    }
}
