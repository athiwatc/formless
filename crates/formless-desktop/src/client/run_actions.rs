use super::*;
use crate::run_actions::{Context, Dispatch, Owner, Target};

#[derive(Clone, Debug)]
pub(super) struct Recovery {
    pub dispatch: Dispatch,
    pub replied: bool,
    pub error: Option<String>,
}
pub(super) fn definite_rejection(message: &ServerMessage) -> bool {
    use formless_protocol::codes;
    matches!(
        message.error_code(),
        Some(
            codes::BAD_REQUEST
                | codes::INVALID
                | codes::BUSY
                | codes::BUDGET
                | codes::NOT_FOUND
                | codes::PROTOCOL_MISMATCH
                | codes::UNSUPPORTED
        )
    )
}

#[derive(Debug)]
pub(super) struct Work {
    pub(super) sent_at: Option<std::time::Instant>,
    pub dispatch: Dispatch,
    revision: u64,
    quote: Option<formless_protocol::RunForecast>,
    consent: bool,
    confirmed: bool,
    pub sent: bool,
}
impl Drop for Work {
    fn drop(&mut self) {
        self.dispatch.lost();
    }
}
impl Engine {
    pub(super) fn recover_run_actions(&mut self) {
        if !self.ready {
            return;
        }
        let recoveries = self
            .run_recoveries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|r| r.dispatch.unknown())
            .cloned()
            .collect::<Vec<_>>();
        for recovery in recoveries {
            if !self.pending.values().any(|p| matches!(p, Pending::RunReconcile(r) if r.dispatch.action.operation == recovery.dispatch.action.operation)) {
                self.request(Request::GetSession { id: recovery.dispatch.action.owner.session.clone() }, Pending::RunReconcile(recovery));
            }
        }
    }
    pub(super) fn run_reconciled(&mut self, recovery: Recovery, value: Result<Value, String>) {
        let Ok(value) = value else {
            return;
        };
        let action = &recovery.dispatch.action;
        let Ok(target) = Target::decode(&action.owner.session, &value) else {
            return;
        };
        let changed_run = target.run.is_some() && target.run != action.target.run;
        let confirmed = recovery.replied
            || match action.kind {
                crate::run_actions::Kind::Cwd => {
                    target.cwd != action.target.cwd
                        && target.cwd.as_deref()
                            == (!action.text.trim().is_empty()).then_some(action.text.trim())
                }
                crate::run_actions::Kind::Edit => {
                    changed_run && target.user.as_ref().is_some_and(|u| u.1 == action.text)
                }
                crate::run_actions::Kind::Retry => changed_run,
                _ => false,
            };
        if !confirmed {
            return;
        }
        let unchanged_error = (action.kind == crate::run_actions::Kind::Cwd
            && target == action.target)
            .then_some(recovery.error.as_deref())
            .flatten();
        recovery.dispatch.reconcile(unchanged_error);
        if action.kind == crate::run_actions::Kind::Compact && recovery.error.is_some() {
            self.compacting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&action.owner.session);
        }
        self.reconciled_actions.push(Action {
            id: action.operation,
            session_id: action.owner.session.clone(),
            question_id: None,
        });
        self.run_recoveries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&action.operation);
        self.dirty_list = true;
        self.dirty_session = true;
    }
    pub(super) fn compaction_pending(&self, session: &str) -> bool {
        self.compacting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(session)
    }
    pub(super) fn action_target(&self, session: &str, value: &Value) -> Result<Target, String> {
        let mut target = Target::decode(session, value)?;
        target.compacting = self.compaction_pending(session);
        Ok(target)
    }
    pub(super) fn action_owner(&self) -> Option<Owner> {
        Some(Owner {
            client: self.instance,
            connection: self.connection,
            epoch: self.epoch,
            generation: self.generation,
            session: self.active.clone()?,
        })
    }
    fn action_current(&self, work: &Work) -> bool {
        self.ready
            && !(matches!(
                work.dispatch.action.kind,
                crate::run_actions::Kind::Edit
                    | crate::run_actions::Kind::Retry
                    | crate::run_actions::Kind::Compact
            ) && self.compaction_pending(&work.dispatch.action.owner.session))
            && self.action_owner().as_ref() == Some(&work.dispatch.action.owner)
            && self.status_revision == work.revision
            && work.dispatch.reserved()
    }
    pub(super) fn action_prepare(&mut self, dispatch: Dispatch) {
        if self
            .action_work
            .as_ref()
            .is_some_and(|w| !self.action_current(w))
        {
            self.action_work = None;
        }
        if self.prepared.is_some()
            || self.pending.values().any(|p| {
                matches!(
                    p,
                    Pending::Action(_)
                        | Pending::Forecast(_)
                        | Pending::Consent(_)
                        | Pending::SendInventory(_)
                        | Pending::ReviewedAction { .. }
                        | Pending::EditSession { .. }
                        | Pending::InspectModelSelection { .. }
                        | Pending::DetachReference(_)
                        | Pending::StepCommit(_)
                        | Pending::ModeCommit { .. }
                        | Pending::ApprovalCommit { .. }
                )
            })
        {
            dispatch
                .reject("Wait for the pending mutation receipt before reviewing another action.");
            return;
        }
        if !self.ready
            || (matches!(
                dispatch.action.kind,
                crate::run_actions::Kind::Edit
                    | crate::run_actions::Kind::Retry
                    | crate::run_actions::Kind::Compact
            ) && self.compaction_pending(&dispatch.action.owner.session))
            || self.action_owner().as_ref() != Some(&dispatch.action.owner)
            || !dispatch.action.valid()
            || !dispatch.reserved()
            || self.action_work.is_some()
            || self.pending.values().any(|p| {
                matches!(
                    p,
                    Pending::RunInspect(_)
                        | Pending::RunForecast(_)
                        | Pending::RunConsent(_)
                        | Pending::RunCommitInspect(_)
                        | Pending::RunWrite(_)
                )
            })
        {
            dispatch.reject("Connection, selection or pending action changed. Nothing was sent.");
            return;
        }
        let id = dispatch.action.owner.session.clone();
        let work = Box::new(Work {
            sent_at: None,
            dispatch,
            revision: self.status_revision,
            quote: None,
            consent: false,
            confirmed: false,
            sent: false,
        });
        self.request(Request::GetSession { id }, Pending::RunInspect(work));
    }
    pub(super) fn action_inspected(
        &mut self,
        work: Box<Work>,
        value: Result<Value, String>,
        commit: bool,
    ) {
        if !self.action_current(&work) {
            return;
        }
        let a = &work.dispatch.action;
        let valid = value
            .and_then(|v| self.action_target(&a.owner.session, &v))
            .is_ok_and(|target| target == a.target);
        if !valid {
            work.dispatch.reject(
                "Session transcript or latest Run changed. Reopen the action; nothing was sent.",
            );
            return;
        }
        if commit {
            if !work.confirmed || (a.forecast().is_some() && !work.consent) {
                work.dispatch
                    .reject("Forecast consent was not completed. Nothing was sent.");
                return;
            }
            self.request(a.request(), Pending::RunWrite(work));
        } else if let Some(input) = a.forecast() {
            self.request(
                Request::ForecastRun {
                    for_send: true,
                    input,
                },
                Pending::RunForecast(work),
            );
        } else {
            self.action_review(work);
        }
    }
    fn action_review(&mut self, work: Box<Work>) {
        if !self.action_current(&work) {
            return;
        }
        self.updates.push(Update::RunActionReview {
            owner: work.dispatch.action.owner.clone(),
            operation: work.dispatch.action.operation,
            quote: work.quote.clone(),
        });
        self.action_work = Some(work);
    }
    pub(super) fn action_forecast(&mut self, mut work: Box<Work>, value: Result<Value, String>) {
        if !self.action_current(&work) {
            return;
        }
        match value
            .and_then(|v| {
                serde_json::from_value::<formless_protocol::RunForecast>(v)
                    .map_err(|e| e.to_string())
            })
            .and_then(|q| {
                forecast::validate(&work.dispatch.action.owner.session, &q)?;
                Ok(q)
            }) {
            Ok(q) => {
                work.quote = Some(q);
                self.action_review(work);
            }
            Err(e) => work.dispatch.reject(&format!("Forecast unavailable: {e}")),
        }
    }
    pub(super) fn action_confirm(&mut self, operation: uuid::Uuid) {
        if self
            .action_work
            .as_ref()
            .is_none_or(|w| w.dispatch.action.operation != operation)
        {
            return;
        }
        let Some(mut work) = self.action_work.take() else {
            return;
        };
        if work.dispatch.action.operation != operation || !self.action_current(&work) {
            return;
        }
        work.confirmed = true;
        if let Some(q) = &work.quote {
            if q.confirmation_required {
                self.request(
                    Request::ConfirmForecast {
                        id: q.confirmation_id.clone().expect("validated forecast"),
                    },
                    Pending::RunConsent(work),
                );
                return;
            }
        }
        work.consent = true;
        self.action_ready(work);
    }
    fn action_ready(&mut self, work: Box<Work>) {
        if !self.action_current(&work) {
            return;
        }
        self.updates.push(Update::RunActionReady {
            owner: work.dispatch.action.owner.clone(),
            operation: work.dispatch.action.operation,
        });
        self.action_work = Some(work);
    }
    pub(super) fn action_consented(&mut self, mut work: Box<Work>, value: Result<Value, String>) {
        if !self.action_current(&work) {
            return;
        }
        if value.is_ok_and(|v| v["confirmed"] == true) {
            work.consent = true;
            self.action_ready(work);
        } else {
            work.dispatch
                .reject("Forecast consent was not acknowledged. Nothing was started.");
        }
    }
    pub(super) fn action_commit(&mut self, operation: uuid::Uuid) {
        if self
            .action_work
            .as_ref()
            .is_none_or(|w| w.dispatch.action.operation != operation)
        {
            return;
        }
        let Some(work) = self.action_work.take() else {
            return;
        };
        if work.dispatch.action.operation != operation
            || !self.action_current(&work)
            || !work.confirmed
            || !work.consent
        {
            return;
        }
        self.request(
            Request::GetSession {
                id: work.dispatch.action.owner.session.clone(),
            },
            Pending::RunCommitInspect(work),
        );
    }
    pub(super) fn action_before_write(&mut self, id: u64) -> bool {
        if matches!(self.pending.get(&id), Some(Pending::RestoreSession(t)) if !t.available()) {
            self.pending.remove(&id);
            self.updates.push(Update::Notice(
                "The delete Undo window has expired. Nothing was sent.".into(),
            ));
            return false;
        }
        let valid = match self.pending.get(&id) {
            Some(Pending::RunWrite(w)) => self.action_current(w),
            _ => return true,
        };
        if !valid {
            self.pending.remove(&id);
            return false;
        }
        let Some(Pending::RunWrite(w)) = self.pending.get_mut(&id) else {
            return false;
        };
        if !w.dispatch.before_write() {
            self.pending.remove(&id);
            return false;
        }
        w.sent = true;
        w.sent_at = Some(std::time::Instant::now());
        self.run_recoveries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                w.dispatch.action.operation,
                Recovery {
                    dispatch: w.dispatch.clone(),
                    replied: false,
                    error: None,
                },
            );
        if w.dispatch.action.kind == crate::run_actions::Kind::Compact {
            self.compacting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(w.dispatch.action.owner.session.clone());
        }
        true
    }
    pub(super) fn context_read(&mut self) {
        if !self.ready
            || self
                .pending
                .values()
                .any(|p| matches!(p, Pending::LiveContext(_)))
        {
            return;
        }
        if let Some(owner) = self.action_owner() {
            self.request(
                Request::ContextUsage {
                    session_id: owner.session.clone(),
                },
                Pending::LiveContext(owner),
            );
        }
    }
    pub(super) fn context_received(&mut self, owner: Owner, value: Result<Value, String>) {
        if self.action_owner().as_ref() != Some(&owner) {
            return;
        }
        self.updates.push(Update::LiveContext {
            owner,
            result: value.and_then(|v| Context::decode(&v)),
        });
    }
    pub(super) fn retire_run_review(&mut self) {
        if self
            .action_work
            .as_ref()
            .is_some_and(|w| !self.action_current(w))
        {
            self.action_work = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_actions::{Action, Kind, Ledger, Status};
    use serde_json::json;
    fn snapshot() -> Value {
        json!({"session":{"id":"s","cwd":null},"running":false,"latest_run":{"session_id":"s","run_id":"r","status":"completed"},"messages":[{"id":"u","session_id":"s","role":"user","seq":1,"content":"original"}]})
    }
    fn setup(kind: Kind) -> (Engine, Arc<Ledger>, Dispatch) {
        let e = Engine {
            ready: true,
            active: Some("s".into()),
            instance: uuid::Uuid::new_v4(),
            connection: 2,
            generation: 3,
            epoch: 4,
            ..Default::default()
        };
        let ledger = Arc::new(Ledger::default());
        let d = ledger
            .reserve(Action {
                owner: e.action_owner().unwrap(),
                operation: uuid::Uuid::new_v4(),
                target: Target::decode("s", &snapshot()).unwrap(),
                kind,
                text: if kind == Kind::Edit {
                    "replacement".into()
                } else {
                    String::new()
                },
            })
            .unwrap();
        (e, ledger, d)
    }
    fn reply(e: &mut Engine, v: Value) -> Request {
        let r = e.outgoing.pop_front().expect("one expected request");
        e.receive(ServerMessage::ok(r.id, v)).unwrap();
        r.request
    }
    fn review(e: &mut Engine, d: Dispatch, consent: bool) {
        e.command(Command::PrepareRunAction(d.clone()));
        assert!(matches!(reply(e, snapshot()), Request::GetSession { .. }));
        if matches!(d.action.kind, Kind::Edit | Kind::Retry) {
            assert!(matches!(
                reply(
                    e,
                    serde_json::to_value(crate::fixture::run_forecast("s", consent)).unwrap()
                ),
                Request::ForecastRun { for_send: true, .. }
            ));
        }
        assert!(e.outgoing.is_empty());
        assert!(e.action_work.is_some());
    }
    #[test]
    fn edit_and_retry_require_forecast_explicit_confirmation_and_fresh_commit_inspection() {
        for kind in [Kind::Edit, Kind::Retry] {
            for required in [false, true] {
                let (mut e, ledger, d) = setup(kind);
                review(&mut e, d.clone(), required);
                e.command(Command::CommitRunAction(uuid::Uuid::new_v4()));
                assert!(e.action_work.is_some());
                e.command(Command::ConfirmRunAction(uuid::Uuid::new_v4()));
                assert!(e.action_work.is_some());
                e.command(Command::ConfirmRunAction(d.action.operation));
                if required {
                    assert!(matches!(
                        reply(&mut e, json!({"confirmed":true})),
                        Request::ConfirmForecast { .. }
                    ));
                }
                assert!(e.outgoing.is_empty());
                e.command(Command::CommitRunAction(d.action.operation));
                assert!(matches!(
                    reply(&mut e, snapshot()),
                    Request::GetSession { .. }
                ));
                let r = e.outgoing.pop_front().unwrap();
                assert_eq!(
                    serde_json::to_value(r.request).unwrap(),
                    serde_json::to_value(d.action.request()).unwrap()
                );
                assert!(e.action_before_write(r.id));
                assert!(matches!(ledger.status(), Status::Sent(_)));
                e.command(Command::CommitRunAction(d.action.operation));
                assert!(e.outgoing.is_empty());
            }
        }
    }
    #[test]
    fn direct_actions_still_require_review_and_never_forecast_compaction() {
        for kind in [
            Kind::Good,
            Kind::Bad,
            Kind::Compact,
            Kind::Delete,
            Kind::Cwd,
        ] {
            let (mut e, _, d) = setup(kind);
            review(&mut e, d.clone(), false);
            e.command(Command::ConfirmRunAction(d.action.operation));
            assert!(e.outgoing.is_empty());
            e.command(Command::CommitRunAction(d.action.operation));
            reply(&mut e, snapshot());
            assert_eq!(e.outgoing.len(), 1);
        }
    }
    #[test]
    fn changed_transcript_owner_revision_or_cancelled_review_never_writes() {
        for reason in 0..6 {
            let (mut e, ledger, d) = setup(Kind::Compact);
            review(&mut e, d.clone(), false);
            e.command(Command::ConfirmRunAction(d.action.operation));
            e.command(Command::CommitRunAction(d.action.operation));
            let mut v = snapshot();
            match reason {
                0 => v["messages"][0]["content"] = json!("changed"),
                1 => e.generation += 1,
                2 => e.connection += 1,
                3 => e.epoch += 1,
                4 => e.status_revision += 1,
                _ => ledger.cancel(),
            }
            reply(&mut e, v);
            assert!(e.outgoing.is_empty());
            assert!(matches!(ledger.status(), Status::Rejected(_)));
        }
    }
    #[test]
    fn malformed_forecast_or_consent_cannot_start_provider_work() {
        for consent in [false, true] {
            let (mut e, ledger, d) = setup(Kind::Edit);
            e.command(Command::PrepareRunAction(d.clone()));
            reply(&mut e, snapshot());
            if consent {
                reply(
                    &mut e,
                    serde_json::to_value(crate::fixture::run_forecast("s", true)).unwrap(),
                );
                e.command(Command::ConfirmRunAction(d.action.operation));
                reply(&mut e, json!({"confirmed":false}));
            } else {
                reply(&mut e, json!({}));
            }
            assert!(e.outgoing.is_empty());
            assert!(matches!(ledger.status(), Status::Rejected(_)));
        }
    }
    #[test]
    fn stale_before_write_and_transport_loss_do_not_authorize_replay() {
        let (mut e, ledger, d) = setup(Kind::Compact);
        review(&mut e, d.clone(), false);
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, snapshot());
        let r = e.outgoing.pop_front().unwrap();
        e.epoch += 1;
        assert!(!e.action_before_write(r.id));
        assert!(matches!(ledger.status(), Status::Rejected(_)));
        let (mut e, ledger, d) = setup(Kind::Compact);
        review(&mut e, d.clone(), false);
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, snapshot());
        let r = e.outgoing.pop_front().unwrap();
        assert!(e.action_before_write(r.id));
        drop(e);
        assert_eq!(ledger.status(), Status::Unknown);
    }
    #[test]
    fn acknowledged_cwd_action_preserves_live_deltas_and_provider_activity() {
        let (mut e, ledger, mut d) = setup(Kind::Cwd);
        e.live_available = true;
        let mut value = snapshot();
        value["running"] = json!(true);
        value["latest_run"]["status"] = json!("running");
        d.action.target = Target::decode("s", &value).unwrap();
        e.receive(ServerMessage::Event(Event::MessageDelta {
            session_id: "s".into(),
            run_id: "r".into(),
            delta: "Live answer".into(),
        }))
        .unwrap();
        e.receive(ServerMessage::Event(Event::ReasoningDelta {
            session_id: "s".into(),
            run_id: "r".into(),
            delta: "Live reasoning".into(),
        }))
        .unwrap();
        e.command(Command::PrepareRunAction(d.clone()));
        assert!(matches!(
            reply(&mut e, value.clone()),
            Request::GetSession { .. }
        ));
        e.receive(ServerMessage::Event(Event::ProviderStalled {
            session_id: "s".into(),
            run_id: "r".into(),
            idle_secs: 120,
        }))
        .unwrap();
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, value);
        let request = e.outgoing.pop_front().unwrap();
        assert!(matches!(request.request, Request::SetSessionCwd { .. }));
        assert!(e.action_before_write(request.id));
        e.receive(ServerMessage::ok(request.id, json!({"id":"s","cwd":null})))
            .unwrap();
        assert!(matches!(ledger.status(), Status::Applied(_)));
        e.emit_live_overlay();
        assert!(e
            .updates
            .iter()
            .any(|u| matches!(u, Update::LiveOverlay { view, .. }
            if view.answer == "Live answer" && view.reasoning == "Live reasoning")));
        assert!(e.updates.iter().any(|u| matches!(
            u,
            Update::RunActivity {
                event: Event::ProviderStalled { .. },
                ..
            }
        )));
        assert!(!e.reader.requested);
    }
    #[test]
    fn compact_ack_does_not_unlock_history_until_completion() {
        let (mut e, _, d) = setup(Kind::Compact);
        review(&mut e, d.clone(), false);
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, snapshot());
        let request = e.outgoing.pop_front().unwrap();
        assert!(e.action_before_write(request.id));
        e.receive(ServerMessage::ok(
            request.id,
            json!({"started":true,"up_to":1}),
        ))
        .unwrap();
        assert!(e.compaction_pending("s"));
        let target = e.action_target("s", &snapshot()).unwrap();
        assert!(!target.allowed(Kind::Edit));
        assert!(!target.allowed(Kind::Retry));
        assert!(!target.allowed(Kind::Compact));
        assert!(target.allowed(Kind::Cwd));
        let mut next = d.action.clone();
        next.operation = uuid::Uuid::new_v4();
        next.kind = Kind::Retry;
        let ledger = Arc::new(Ledger::default());
        e.action_prepare(ledger.reserve(next).unwrap());
        assert!(e.outgoing.is_empty());
        e.receive(ServerMessage::Event(Event::Note {
            session_id: Some("other".into()),
            text: "compaction failed: test".into(),
        }))
        .unwrap();
        assert!(e.compaction_pending("s"));
        e.receive(ServerMessage::Event(Event::Note {
            session_id: Some("s".into()),
            text: "compaction failed: test".into(),
        }))
        .unwrap();
        assert!(!e.compaction_pending("s"));
        assert!(e
            .action_target("s", &snapshot())
            .unwrap()
            .allowed(Kind::Retry));
    }
    #[test]
    fn acknowledged_compaction_is_retired_with_its_connection() {
        let (mut e, ledger, d) = setup(Kind::Compact);
        review(&mut e, d.clone(), false);
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, snapshot());
        let request = e.outgoing.pop_front().unwrap();
        assert!(e.action_before_write(request.id));
        e.receive(ServerMessage::ok(
            request.id,
            json!({"started":true,"up_to":1}),
        ))
        .unwrap();
        assert!(e.compaction_pending("s"));
        let mut retained = Retained {
            compacting: e.compacting.clone(),
            ..Default::default()
        };
        drop(e);
        retained.retire_connection();
        assert!(matches!(ledger.status(), Status::Applied(_)));
        let e = Engine {
            compacting: retained.compacting.clone(),
            ..Default::default()
        };
        assert!(!e.compaction_pending("s"));
        let target = e.action_target("s", &snapshot()).unwrap();
        for kind in [Kind::Edit, Kind::Retry, Kind::Compact] {
            assert!(target.allowed(kind));
        }
        retained.retire_connection();
        assert!(e.outgoing.is_empty());
    }
    #[test]
    fn delete_undo_uses_the_receipt_window_exact_session_and_never_replays() {
        let (mut e, _, d) = setup(Kind::Delete);
        review(&mut e, d.clone(), false);
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, snapshot());
        let request = e.outgoing.pop_front().unwrap();
        assert!(e.action_before_write(request.id));
        let Some(Pending::RunWrite(work)) = e.pending.get(&request.id) else {
            panic!("delete write");
        };
        let sent_at = work.sent_at.unwrap();
        let daemon_started_at = sent_at + Duration::from_secs(1);
        e.receive(ServerMessage::ok(
            request.id,
            json!({"deleted":true,"undo_secs":30}),
        ))
        .unwrap();
        let ticket = e.delete_undo.clone().unwrap();
        assert!(ticket.available());
        assert_eq!(ticket.deadline, sent_at + Duration::from_secs(30));
        assert!(ticket.deadline <= daemon_started_at + Duration::from_secs(30));
        assert!(ticket.deadline <= std::time::Instant::now() + Duration::from_secs(30));
        e.active = Some("other".into());
        e.command(Command::UndoDelete(ticket.operation));
        assert!(
            matches!(reply(&mut e, json!({"id":"s"})), Request::RestoreSession { id } if id == "s")
        );
        e.command(Command::UndoDelete(ticket.operation));
        assert!(e.outgoing.is_empty());
        let mut expired = ticket;
        expired.deadline = std::time::Instant::now();
        e.delete_undo = Some(expired.clone());
        e.command(Command::UndoDelete(expired.operation));
        assert!(e.outgoing.is_empty());
    }
    #[test]
    fn a_delayed_delete_ack_cannot_extend_the_daemons_purge_window() {
        let (mut e, _, d) = setup(Kind::Delete);
        review(&mut e, d.clone(), false);
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, snapshot());
        let request = e.outgoing.pop_front().unwrap();
        assert!(e.action_before_write(request.id));
        let Some(Pending::RunWrite(work)) = e.pending.get_mut(&request.id) else {
            panic!("delete write");
        };
        work.sent_at = Some(std::time::Instant::now() - Duration::from_secs(60));
        e.receive(ServerMessage::ok(
            request.id,
            json!({"deleted":true,"undo_secs":30}),
        ))
        .unwrap();
        assert!(e.delete_undo.is_none());
        assert!(!e.updates.iter().any(|u| matches!(u, Update::DeleteUndo(_))));
    }
    #[test]
    fn daemon_mutation_error_is_unknown_and_reloads_canonical_state() {
        let (mut e, ledger, d) = setup(Kind::Cwd);
        review(&mut e, d.clone(), false);
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, snapshot());
        let request = e.outgoing.pop_front().unwrap();
        assert!(e.action_before_write(request.id));
        e.receive(ServerMessage::Response {
            id: request.id,
            ok: false,
            result: None,
            error: Some("persisted CWD, prompt refresh failed".into()),
        })
        .unwrap();
        assert_eq!(ledger.status(), Status::Unknown);
        assert!(ledger
            .status()
            .notice()
            .contains("may already have applied"));
        assert!(e
            .outgoing
            .iter()
            .any(|r| matches!(&r.request, Request::GetSession { id } if id == "s")));
        assert!(!e
            .outgoing
            .iter()
            .any(|r| matches!(r.request, Request::SetSessionCwd { .. })));
        assert!(ledger.reserve(d.action.clone()).is_err());
        let mut canonical = snapshot();
        canonical["session"]["cwd"] = json!("/persisted");
        reply(&mut e, canonical);
        assert_eq!(ledger.status(), Status::Reconciled);
        assert!(e.run_recoveries.lock().unwrap().is_empty());
        let mut uncertain = HashSet::from([crate::client::Action {
            id: d.action.operation,
            session_id: "s".into(),
            question_id: None,
        }]);
        for action in e.reconciled_actions.drain(..) {
            uncertain.remove(&action);
        }
        assert!(!e.blocked_by_unknown(&Command::PrepareRunAction(d.clone()), &uncertain));
        let mut next = d.action;
        next.operation = uuid::Uuid::new_v4();
        assert!(ledger.reserve(next).is_ok());
    }
    #[test]
    fn prewrite_rejections_and_unchanged_cwd_fail_definitely() {
        for code in [Some(formless_protocol::codes::INVALID), None] {
            let (mut e, ledger, mut d) = setup(Kind::Cwd);
            d.action.text = "/missing".into();
            review(&mut e, d.clone(), false);
            e.command(Command::ConfirmRunAction(d.action.operation));
            e.command(Command::CommitRunAction(d.action.operation));
            reply(&mut e, snapshot());
            let request = e.outgoing.pop_front().unwrap();
            assert!(e.action_before_write(request.id));
            e.receive(ServerMessage::Response {
                id: request.id,
                ok: false,
                result: code.map(|c| json!({"code":c})),
                error: Some("no directory '/missing'".into()),
            })
            .unwrap();
            if code.is_none() {
                assert_eq!(ledger.status(), Status::Unknown);
                reply(&mut e, snapshot());
            }
            assert!(matches!(ledger.status(), Status::Rejected(_)));
            assert!(e.run_recoveries.lock().unwrap().is_empty());
        }
    }
    #[test]
    fn canonical_recovery_requires_valid_state_and_never_replays_a_write() {
        let (mut e, ledger, mut d) = setup(Kind::Cwd);
        d.action.text = "/persisted".into();
        review(&mut e, d.clone(), false);
        e.command(Command::ConfirmRunAction(d.action.operation));
        e.command(Command::CommitRunAction(d.action.operation));
        reply(&mut e, snapshot());
        let request = e.outgoing.pop_front().unwrap();
        assert!(e.action_before_write(request.id));
        let recoveries = e.run_recoveries.clone();
        drop(e);
        assert_eq!(ledger.status(), Status::Unknown);
        let (mut e, _, _) = setup(Kind::Cwd);
        e.run_recoveries = recoveries;
        for value in [json!({}), snapshot()] {
            e.recover_run_actions();
            assert!(matches!(reply(&mut e, value), Request::GetSession { .. }));
            assert_eq!(ledger.status(), Status::Unknown);
            assert!(e.reconciled_actions.is_empty());
            assert!(e.outgoing.is_empty());
        }
        e.recover_run_actions();
        let request = e.outgoing.pop_front().unwrap();
        e.receive(ServerMessage::err_code(
            request.id,
            formless_protocol::codes::NOT_FOUND,
            "missing".into(),
        ))
        .unwrap();
        assert_eq!(ledger.status(), Status::Unknown);
        assert!(e.outgoing.is_empty());
        e.recover_run_actions();
        let mut canonical = snapshot();
        canonical["session"]["cwd"] = json!("/persisted");
        reply(&mut e, canonical);
        assert_eq!(ledger.status(), Status::Reconciled);
        assert!(e.outgoing.is_empty());
    }
    #[test]
    fn rejected_compaction_does_not_keep_history_locked() {
        for code in [Some(formless_protocol::codes::BUSY), None] {
            let (mut e, ledger, d) = setup(Kind::Compact);
            review(&mut e, d.clone(), false);
            e.command(Command::ConfirmRunAction(d.action.operation));
            e.command(Command::CommitRunAction(d.action.operation));
            reply(&mut e, snapshot());
            let request = e.outgoing.pop_front().unwrap();
            assert!(e.action_before_write(request.id));
            e.receive(ServerMessage::Response {
                id: request.id,
                ok: false,
                result: code.map(|c| json!({"code":c})),
                error: Some("compaction was not started".into()),
            })
            .unwrap();
            if code.is_none() {
                assert!(e.compaction_pending("s"));
                reply(&mut e, snapshot());
                assert_eq!(ledger.status(), Status::Reconciled);
            } else {
                assert!(matches!(ledger.status(), Status::Rejected(_)));
            }
            assert!(!e.compaction_pending("s"));
        }
    }
    #[test]
    fn completed_compaction_replaces_precompaction_context_reads() {
        let (mut e, _, _) = setup(Kind::Compact);
        e.context_read();
        let old = e.outgoing.pop_front().unwrap();
        e.receive(ServerMessage::Event(Event::CompactionNotice {
            session_id: "s".into(),
            summary_preview: "summary".into(),
        }))
        .unwrap();
        let fresh = e.outgoing.pop_front().unwrap();
        assert!(matches!(fresh.request, Request::ContextUsage { .. }));
        assert_ne!(old.id, fresh.id);
        e.receive(ServerMessage::ok(
            old.id,
            json!({"used_tokens":90,"context_tokens":100,"measured":false}),
        ))
        .unwrap();
        assert!(!e
            .updates
            .iter()
            .any(|u| matches!(u, Update::LiveContext { .. })));
        e.receive(ServerMessage::ok(
            fresh.id,
            json!({"used_tokens":12,"context_tokens":100,"measured":false}),
        ))
        .unwrap();
        assert!(e.updates.iter().any(|u| matches!(u, Update::LiveContext { result: Ok(c), .. } if c.used == 12 && !c.measured)));
    }
    #[test]
    fn context_reads_are_coalesced_and_old_selection_replies_are_discarded() {
        let (mut e, _, _) = setup(Kind::Compact);
        e.context_read();
        e.context_read();
        assert_eq!(e.outgoing.len(), 1);
        e.generation += 1;
        reply(
            &mut e,
            json!({"used_tokens":2,"context_tokens":10,"measured":false}),
        );
        assert!(!e
            .updates
            .iter()
            .any(|u| matches!(u, Update::LiveContext { .. })));
        e.context_read();
        reply(
            &mut e,
            json!({"used_tokens":0,"context_tokens":10,"measured":false}),
        );
        assert!(e
            .updates
            .iter()
            .any(|u| matches!(u, Update::LiveContext { result: Ok(_), .. })));
    }
}
