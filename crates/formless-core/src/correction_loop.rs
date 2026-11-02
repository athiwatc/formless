//! Bound retries of identical daemon feedback, without limiting real Tool work.
use crate::provider::ToolCall;
use sha2::{Digest, Sha256};

pub(super) const MAX_IDENTICAL_CORRECTIONS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) enum CorrectionClass {
    InvalidJson,
    TruncatedArguments,
    UnknownTool,
}

impl CorrectionClass {
    fn label(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid JSON arguments",
            Self::TruncatedArguments => "truncated arguments",
            Self::UnknownTool => "unknown Tool name",
        }
    }
}

#[derive(Default)]
pub(super) struct CorrectionStreak {
    signature: Option<String>,
    count: u8,
    batch_signature: Option<String>,
    batch_count: u8,
}

/// Ordered signature of one returned batch, streamed without copying arguments.
#[derive(Default)]
pub(super) struct CorrectionBatch {
    digest: Sha256,
    calls: usize,
    ordinary: bool,
}
impl CorrectionBatch {
    pub(super) fn observe(&mut self, call: &ToolCall, class: Option<CorrectionClass>) {
        self.calls += 1;
        let Some(class) = class else {
            self.ordinary = true;
            return;
        };
        for bytes in [call.name.as_bytes(), call.arguments.as_bytes()] {
            self.digest.update((bytes.len() as u64).to_le_bytes());
            self.digest.update(bytes);
        }
        self.digest.update([class as u8]);
    }
}

impl CorrectionStreak {
    pub(super) fn observe_batch(&mut self, batch: CorrectionBatch) -> Option<anyhow::Error> {
        if batch.ordinary || batch.calls == 0 {
            self.batch_signature = None;
            self.batch_count = 0;
            return None;
        }
        let signature = format!("{:x}", batch.digest.finalize());
        self.batch_count = if self.batch_signature.as_ref() == Some(&signature) {
            self.batch_count.saturating_add(1)
        } else {
            1
        };
        self.batch_signature = Some(signature);
        (self.batch_count>=MAX_IDENTICAL_CORRECTIONS).then(||anyhow::anyhow!(
            "stuck: repeated daemon correction: the model sent the same ordered batch of {} invalid Tool calls {} consecutive times without fixing it. Revise the request or switch model, then Resume to try again.",batch.calls,MAX_IDENTICAL_CORRECTIONS
        ))
    }
    pub(super) fn observe(
        &mut self,
        call: &ToolCall,
        class: Option<CorrectionClass>,
    ) -> Option<anyhow::Error> {
        let Some(class) = class else {
            *self = Self::default();
            return None;
        };
        let signature = crate::forecast::hash(&(&call.name, &call.arguments, class));
        self.count = if self.signature.as_ref() == Some(&signature) {
            self.count.saturating_add(1)
        } else {
            1
        };
        self.signature = Some(signature);
        (self.count >= MAX_IDENTICAL_CORRECTIONS).then(|| anyhow::anyhow!(
            "stuck: repeated daemon correction: the model sent the same call to '{}' with {} {} consecutive times without fixing it. Revise the request or switch model, then Resume to try again.",
            call.name.chars().filter(|ch|!ch.is_control()).take(120).collect::<String>(), class.label(), MAX_IDENTICAL_CORRECTIONS
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
    #[test]
    fn ordered_all_correction_batches_repeat_but_changed_or_real_work_resets() {
        let mut streak = CorrectionStreak::default();
        let batch = |items: &[(&str, &str, Option<CorrectionClass>)]| {
            let mut batch = CorrectionBatch::default();
            for (name, args, class) in items {
                batch.observe(&call(name, args), *class);
            }
            batch
        };
        let a = ("a", "{", Some(CorrectionClass::InvalidJson));
        let b = ("b", "{", Some(CorrectionClass::InvalidJson));
        assert!(streak.observe_batch(batch(&[a, b])).is_none());
        assert!(streak.observe_batch(batch(&[a, b])).is_none());
        assert!(streak.observe_batch(batch(&[b, a])).is_none());
        assert_eq!(streak.batch_count, 1, "changed order resets");
        assert!(streak
            .observe_batch(batch(&[a, ("b", "{ ", b.2)]))
            .is_none());
        assert_eq!(streak.batch_count, 1, "changed arguments reset");
        assert!(streak
            .observe_batch(batch(&[a, ("b", "{", Some(CorrectionClass::UnknownTool))]))
            .is_none());
        assert_eq!(streak.batch_count, 1, "changed class resets");
        assert!(streak
            .observe_batch(batch(&[a, ("b", "{", None)]))
            .is_none());
        assert_eq!(streak.batch_count, 0, "any real outcome resets");
        assert!(streak.observe_batch(batch(&[a, b])).is_none());
        assert!(streak.observe_batch(batch(&[a, b])).is_none());
        assert!(streak.observe_batch(batch(&[a, b])).is_some());
        assert_eq!(streak.batch_signature.as_ref().unwrap().len(), 64);
    }
    #[test]
    fn identical_raw_call_ignores_changing_call_ids_and_stops_on_three() {
        let mut streak = CorrectionStreak::default();
        assert!(streak
            .observe(&call("lookup", "{"), Some(CorrectionClass::InvalidJson))
            .is_none());
        assert!(streak
            .observe(&call("lookup", "{"), Some(CorrectionClass::InvalidJson))
            .is_none());
        let error = streak
            .observe(&call("lookup", "{"), Some(CorrectionClass::InvalidJson))
            .unwrap();
        assert!(error
            .to_string()
            .starts_with("stuck: repeated daemon correction:"));
        assert_eq!(streak.count, 3);
        assert_eq!(streak.signature.as_ref().unwrap().len(), 64);
    }
    #[test]
    fn changed_name_raw_arguments_class_and_real_outcomes_reset_the_streak() {
        let mut streak = CorrectionStreak::default();
        for (name, args, class) in [
            ("a", "{", Some(CorrectionClass::InvalidJson)),
            ("a", "{", Some(CorrectionClass::InvalidJson)),
            ("a", "{ ", Some(CorrectionClass::InvalidJson)),
            ("b", "{ ", Some(CorrectionClass::InvalidJson)),
            ("b", "{ ", Some(CorrectionClass::TruncatedArguments)),
            ("b", "{ ", Some(CorrectionClass::TruncatedArguments)),
            ("b", "{ ", None),
            ("b", "{ ", Some(CorrectionClass::TruncatedArguments)),
        ] {
            assert!(streak.observe(&call(name, args), class).is_none());
        }
        assert_eq!(streak.count, 1);
        // Real outcomes, including an ordinary Tool error or successful
        // pending poll, have no daemon correction class and no retry ceiling.
        for _ in 0..100 {
            assert!(streak
                .observe(&call("check_invocation", "{}"), None)
                .is_none());
        }
        assert_eq!(streak.count, 0);
        assert!(streak.signature.is_none());
    }
    #[test]
    fn explanation_offers_resumption_without_retaining_raw_arguments() {
        let mut streak = CorrectionStreak::default();
        let call = call("missing", &"private argument".repeat(1000));
        let mut error = None;
        for _ in 0..MAX_IDENTICAL_CORRECTIONS {
            error = streak.observe(&call, Some(CorrectionClass::UnknownTool));
        }
        let raw = error.unwrap().to_string();
        assert!(!raw.contains("private argument"));
        let cfg = crate::Config::default();
        let explained = crate::errors::explain(
            &raw,
            &crate::errors::ErrorContext::from_config(&cfg, "fixture"),
        );
        assert_eq!(explained.category, "stuck");
        assert!(explained.action.as_deref().unwrap().contains("Resume"));
    }
}
