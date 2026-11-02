//! Stall reporting reads the same invocation-local evidence as the public list.
use super::*;

impl Daemon {
    pub(super) fn scan_invocation_stalls(&self, now_ms: u64, stall_ms: u64) {
        let entries: Vec<_> = self
            .pending
            .locked()
            .iter()
            .map(|(handle, entry)| (handle.clone(), entry.clone()))
            .collect();
        for (handle, entry) in entries {
            if entry.result.locked().is_some() {
                continue;
            }
            let silent_ms = now_ms.saturating_sub(entry.activity.load(Ordering::SeqCst));
            if silent_ms >= stall_ms {
                if !entry.stalled_noted.swap(true, Ordering::SeqCst) {
                    let progress = entry.progress.snapshot();
                    self.broadcast(Event::InvocationStalled {
                        invocation_id: Some(entry.progress.id.clone()),
                        session_id: entry.session_id.clone(),
                        handle,
                        tool_name: entry.tool_name.clone(),
                        silent_secs: silent_ms / 1000,
                        last_fraction: progress.as_ref().and_then(|value| value.fraction),
                        last_label: progress.map(|value| value.label),
                    });
                }
            } else if entry.stalled_noted.swap(false, Ordering::SeqCst) {
                self.broadcast(Event::InvocationActivity {
                    invocation_id: Some(entry.progress.id.clone()),
                    session_id: entry.session_id.clone(),
                    handle,
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "progress_idle_tests.rs"]
mod idle_tests;
