//! Steering is accepted against a live Run and consumed only by a context that
//! actually reached its provider-request boundary. Wake-ups carry no authority.
use super::*;
use std::sync::atomic::AtomicI64;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SteeringOutcome {
    Extend,
    Completed,
    Interrupted,
}

pub(super) struct RunSteering {
    accepted: AtomicI64,
    consumed: AtomicI64,
    accepting: AtomicBool,
}

impl Default for RunSteering {
    fn default() -> Self {
        Self {
            accepted: AtomicI64::new(0),
            consumed: AtomicI64::new(0),
            accepting: AtomicBool::new(true),
        }
    }
}

impl RunSteering {
    pub(super) fn accepts(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }
    pub(super) fn accepted(&self, seq: i64) {
        self.accepted.fetch_max(seq, Ordering::SeqCst);
    }
}

impl Daemon {
    pub(super) fn mark_steering_context_included(
        &self,
        session: &str,
        run: &str,
        seq: i64,
        included_user_message_ids: &[String],
        step: i64,
    ) -> Result<()> {
        if self.with_store(|st| {
            st.mark_steering_context_included(session, run, seq, included_user_message_ids, step)
        })? {
            self.broadcast(Event::MessagesEdited {
                session_id: session.to_string(),
            });
        }
        Ok(())
    }

    pub(super) fn mark_context_consumed(&self, session: &str, run: &str, seq: i64) {
        if let Some(control) = self
            .runs
            .locked()
            .get(session)
            .filter(|control| control.run_id == run)
        {
            control.steering.consumed.fetch_max(seq, Ordering::SeqCst);
        }
    }

    /// The same admission lock serializes Send's final classification and this
    /// final-answer decision. After false, every new send queues before append.
    pub(super) fn extend_or_close_steering(
        &self,
        session: &str,
        run: &str,
        cancelled: bool,
    ) -> SteeringOutcome {
        let _admission = self.forecast_admission.locked();
        let runs = self.runs.locked();
        let Some(control) = runs.get(session).filter(|control| control.run_id == run) else {
            return SteeringOutcome::Completed;
        };
        let interrupted = cancelled
            || control.cancel.is_cancelled()
            || control.cancel_flag.load(Ordering::SeqCst);
        if !interrupted
            && control.steering.accepted.load(Ordering::SeqCst)
                > control.steering.consumed.load(Ordering::SeqCst)
        {
            return SteeringOutcome::Extend;
        }
        control.steering.accepting.store(false, Ordering::SeqCst);
        if interrupted {
            SteeringOutcome::Interrupted
        } else {
            SteeringOutcome::Completed
        }
    }

    pub(super) fn close_run_steering(&self, session: &str, run: &str) {
        let _admission = self.forecast_admission.locked();
        if let Some(control) = self
            .runs
            .locked()
            .get(session)
            .filter(|control| control.run_id == run)
        {
            control.steering.accepting.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
#[path = "steering_daemon_tests.rs"]
mod tests;
