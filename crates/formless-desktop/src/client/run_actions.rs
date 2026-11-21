use super::*;
use crate::run_actions::{Context, Dispatch, Owner, Target};

#[derive(Debug)]
pub(super) struct Work {
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
    fn compaction_pending(&self, session: &str) -> bool {
        self.compacting.lock().unwrap_or_else(|e| e.into_inner()).contains(session)
    }
    fn action_target(&self, session: &str, value: &Value) -> Result<Target, String> {
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
            && !(matches!(work.dispatch.action.kind, crate::run_actions::Kind::Edit | crate::run_actions::Kind::Retry | crate::run_actions::Kind::Compact)
                && self.compaction_pending(&work.dispatch.action.owner.session))
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
            || (matches!(dispatch.action.kind, crate::run_actions::Kind::Edit | crate::run_actions::Kind::Retry | crate::run_actions::Kind::Compact)
                && self.compaction_pending(&dispatch.action.owner.session))
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
        if w.dispatch.action.kind == crate::run_actions::Kind::Compact {
            self.compacting.lock().unwrap_or_else(|e| e.into_inner()).insert(w.dispatch.action.owner.session.clone());
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
        e.receive(ServerMessage::ok(request.id, json!({"started":true,"up_to":1}))).unwrap();
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
        e.receive(ServerMessage::Event(Event::Note { session_id: Some("other".into()), text: "compaction failed: test".into() })).unwrap();
        assert!(e.compaction_pending("s"));
        e.receive(ServerMessage::Event(Event::Note { session_id: Some("s".into()), text: "compaction failed: test".into() })).unwrap();
        assert!(!e.compaction_pending("s"));
        assert!(e.action_target("s", &snapshot()).unwrap().allowed(Kind::Retry));
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
