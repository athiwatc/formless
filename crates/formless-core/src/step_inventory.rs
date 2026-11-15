//! Read-only recovery inventory for Step prompts; no new decision authority.
use super::*;
use formless_protocol::{PendingStep, PendingSteps, MAX_PENDING_STEPS, STEP_PREVIEW_BYTES};

pub(super) struct Waiter {
    pub prompt: PendingStep,
    pub decision: tokio::sync::oneshot::Sender<(String, Option<String>)>,
}
impl Waiter {
    pub(super) fn new(
        prompt: &Event,
        decision: tokio::sync::oneshot::Sender<(String, Option<String>)>,
    ) -> Self {
        let Event::StepPending {
            session_id,
            run_id,
            step_id,
            tool_name,
            arguments,
            index,
            count,
        } = prompt
        else {
            unreachable!()
        };
        let mut end = arguments.len().min(STEP_PREVIEW_BYTES);
        while !arguments.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            prompt: PendingStep {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
                step_id: step_id.clone(),
                tool_name: tool_name.clone(),
                arguments_preview: arguments[..end].into(),
                arguments_bytes: arguments.len(),
                index: *index,
                count: *count,
            },
            decision,
        }
    }
}

/// The waiting future owns retirement, including when it is dropped/aborted.
/// A successful decision removes authority immediately; this guard still emits
/// the existing resolution event exactly once when the wait ends.
pub(super) struct Waiting<'a> {
    pub daemon: &'a Daemon,
    pub session: &'a str,
    pub id: &'a str,
}
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.daemon.step_waiters.locked().remove(self.id);
        self.daemon.broadcast(Event::StepResolved {
            session_id: self.session.into(),
            step_id: self.id.into(),
        });
    }
}

impl Daemon {
    pub(super) fn pending_steps_snapshot(&self, session: &str) -> PendingSteps {
        let waiters = self.step_waiters.locked();
        // Keep the lowest identities with bounded scratch memory, regardless
        // of total waiting Sessions or HashMap iteration order.
        let mut selected = BTreeMap::new();
        let mut more = false;
        for waiter in waiters.values().filter(|w| w.prompt.session_id == session) {
            selected.insert(waiter.prompt.step_id.as_str(), &waiter.prompt);
            if selected.len() > MAX_PENDING_STEPS {
                selected.pop_last();
                more = true;
            }
        }
        let prompts = selected.into_values().cloned().collect();
        PendingSteps { prompts, more }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    struct Fixture {
        daemon: Arc<Daemon>,
        session: String,
        other: String,
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("fl-step-inventory-{}", uuid::Uuid::new_v4()));
            let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
            Config::default().save(&paths).unwrap();
            let daemon = Arc::new(Daemon::new(paths).unwrap());
            let (session, other) = daemon
                .with_store(|s| Ok((s.create_session(None)?.id, s.create_session(None)?.id)))
                .unwrap();
            Self {
                daemon,
                session,
                other,
                root,
            }
        }
        fn subscribe(&self) -> mpsc::Receiver<ServerMessage> {
            let (tx, rx) = mpsc::channel(256);
            self.daemon.subs.locked().push(Subscriber {
                conn: 1,
                tx: tx.into(),
                filter: Arc::new(StdMutex::new(EventFilter::default())),
                kick: CancellationToken::new(),
                gaps: BTreeMap::new(),
            });
            rx
        }
        fn start(
            &self,
            cancel: CancellationToken,
        ) -> tokio::task::JoinHandle<Result<(Vec<ToolCall>, HashSet<String>)>> {
            self.daemon
                .step_sessions
                .locked()
                .insert(self.session.clone());
            let daemon = self.daemon.clone();
            let session = self.session.clone();
            tokio::spawn(async move {
                let mut calls = vec![ToolCall {
                    id: "call".into(),
                    name: "fixture-read".into(),
                    arguments: "{\"value\":1}".into(),
                }];
                let skipped = daemon
                    .step_gate(&session, "fixture-run", &mut calls, &cancel)
                    .await?;
                Ok((calls, skipped))
            })
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    async fn pending(rx: &mut mpsc::Receiver<ServerMessage>) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Some(ServerMessage::Event(Event::StepPending { step_id, .. })) =
                    rx.recv().await
                {
                    return step_id;
                }
            }
        })
        .await
        .unwrap()
    }
    async fn resolved(rx: &mut mpsc::Receiver<ServerMessage>) -> Option<ServerMessage> {
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("Step resolution deadline")
    }
    #[tokio::test]
    async fn reconnect_snapshot_recovers_identity_and_resolved_authority_is_removed() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new();
        let mut events = f.subscribe();
        let task = f.start(CancellationToken::new());
        let id = pending(&mut events).await;
        // A later client needs no historical event to discover the existing prompt.
        let reply = f
            .daemon
            .handle(Request::GetSession {
                id: f.session.clone(),
            })
            .await
            .unwrap();
        let inventory: PendingSteps =
            serde_json::from_value(reply["pending_steps"].clone()).unwrap();
        assert_eq!(inventory.prompts.len(), 1);
        let p = &inventory.prompts[0];
        assert_eq!(p.step_id, id);
        assert_eq!(p.run_id, "fixture-run");
        assert_eq!(p.arguments_preview, "{\"value\":1}");
        assert_eq!(p.arguments_bytes, p.arguments_preview.len());
        assert_eq!((p.index, p.count), (0, 1));
        let other = f
            .daemon
            .handle(Request::GetSession {
                id: f.other.clone(),
            })
            .await
            .unwrap();
        assert_eq!(other["pending_steps"]["prompts"], json!([]));
        assert!(f
            .daemon
            .handle(Request::StepDecision {
                step_id: id.clone(),
                action: "edit".into(),
                arguments: Some("[]".into())
            })
            .await
            .is_err());
        assert_eq!(f.daemon.pending_steps_snapshot(&f.session).prompts.len(), 1);
        f.daemon
            .handle(Request::StepDecision {
                step_id: id.clone(),
                action: "edit".into(),
                arguments: Some("{\"value\":2}".into()),
            })
            .await
            .unwrap();
        assert!(f
            .daemon
            .pending_steps_snapshot(&f.session)
            .prompts
            .is_empty());
        let (calls, skipped) = task.await.unwrap().unwrap();
        assert_eq!(calls[0].arguments, "{\"value\":2}");
        assert!(skipped.is_empty());
        assert!(f
            .daemon
            .handle(Request::StepDecision {
                step_id: id,
                action: "continue".into(),
                arguments: None
            })
            .await
            .is_err());
    }
    #[tokio::test]
    async fn existing_continue_skip_and_all_actions_retain_their_semantics() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new();
        let mut events = f.subscribe();
        for action in ["continue", "skip", "all"] {
            let task = f.start(CancellationToken::new());
            let id = pending(&mut events).await;
            f.daemon
                .handle(Request::StepDecision {
                    step_id: id,
                    action: action.into(),
                    arguments: None,
                })
                .await
                .unwrap();
            let (calls, skipped) = task.await.unwrap().unwrap();
            assert_eq!(calls[0].arguments, "{\"value\":1}");
            assert_eq!(skipped.contains("call"), action == "skip");
            assert_eq!(
                f.daemon.step_sessions.locked().contains(&f.session),
                action != "all"
            );
            assert!(f
                .daemon
                .pending_steps_snapshot(&f.session)
                .prompts
                .is_empty());
            assert!(matches!(
                resolved(&mut events).await,
                Some(ServerMessage::Event(Event::StepResolved { .. }))
            ));
        }
    }
    #[tokio::test]
    async fn cancellation_and_owner_abort_retire_inventory_and_emit_resolution() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new();
        let mut events = f.subscribe();
        for abort in [false, true] {
            let cancel = CancellationToken::new();
            let task = f.start(cancel.clone());
            let id = pending(&mut events).await;
            if abort {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                cancel.cancel();
                assert!(task.await.unwrap().is_err());
            }
            assert!(f
                .daemon
                .pending_steps_snapshot(&f.session)
                .prompts
                .is_empty());
            assert!(
                matches!(resolved(&mut events).await,Some(ServerMessage::Event(Event::StepResolved{step_id,..})) if step_id==id)
            );
            assert!(f
                .daemon
                .handle(Request::StepDecision {
                    step_id: id,
                    action: "continue".into(),
                    arguments: None
                })
                .await
                .is_err());
        }
    }
    #[tokio::test]
    async fn inventory_is_session_scoped_stable_bounded_and_utf8_exact() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let f = Fixture::new();
        let text = "ก".repeat(5000);
        let mut receivers = Vec::new();
        for i in (0..MAX_PENDING_STEPS + 2).rev() {
            let id = format!("step-{i:03}");
            let (tx, rx) = tokio::sync::oneshot::channel();
            receivers.push(rx);
            f.daemon.step_waiters.locked().insert(
                id.clone(),
                Waiter::new(
                    &Event::StepPending {
                        session_id: f.session.clone(),
                        run_id: "r".into(),
                        step_id: id,
                        tool_name: "fixture-read".into(),
                        arguments: text.clone(),
                        index: 0,
                        count: 1,
                    },
                    tx,
                ),
            );
        }
        let a = f.daemon.pending_steps_snapshot(&f.session);
        assert_eq!(a, f.daemon.pending_steps_snapshot(&f.session));
        assert!(a.more);
        assert_eq!(a.prompts.len(), MAX_PENDING_STEPS);
        assert!(a.prompts.windows(2).all(|p| p[0].step_id < p[1].step_id));
        assert!(a
            .prompts
            .iter()
            .all(|p| p.arguments_preview.len() <= STEP_PREVIEW_BYTES
                && text.starts_with(&p.arguments_preview)
                && p.arguments_bytes == text.len()));
        assert!(f.daemon.pending_steps_snapshot(&f.other).prompts.is_empty());
    }
}
