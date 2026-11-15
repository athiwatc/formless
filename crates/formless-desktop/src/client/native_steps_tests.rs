use super::*;
use crate::step_decision::{self as model, Choice, Outcome, Status};
use formless_protocol::{PendingStep, PendingSteps};
use serde_json::json;
fn setup(choice: Choice) -> (Engine, Arc<model::Ledger>, model::Dispatch) {
    let instance = uuid::Uuid::new_v4();
    let mut e = Engine {
        instance,
        ready: true,
        connection: 3,
        generation: 2,
        epoch: 7,
        active: Some("s".into()),
        ..Default::default()
    };
    let ledger = Arc::new(model::Ledger::default());
    let d = ledger
        .reserve(model::Action {
            client: instance,
            connection: 3,
            generation: 2,
            epoch: 7,
            operation: uuid::Uuid::new_v4(),
            prompt: PendingStep {
                session_id: "s".into(),
                run_id: "r".into(),
                step_id: "p".into(),
                tool_name: "tool".into(),
                arguments_preview: "{}".into(),
                arguments_bytes: 2,
                index: 0,
                count: 1,
            },
            choice,
        })
        .unwrap();
    e.command(Command::DecideStep(d.clone()));
    (e, ledger, d)
}
fn baseline(d: &model::Dispatch) -> Value {
    json!({"session":{"id":"s"},"pending_steps":PendingSteps{prompts:vec![d.action().prompt.clone()],more:false}})
}
fn revalidate(e: &mut Engine, d: &model::Dispatch) {
    let request = e.outgoing.pop_front().unwrap();
    assert!(matches!(request.request,Request::GetSession{id} if id=="s"));
    assert!(d.reserved());
    e.receive(ServerMessage::ok(request.id, baseline(d)))
        .unwrap();
}
#[test]
fn each_native_choice_revalidates_exact_prompt_before_one_matching_decision() {
    for choice in [
        Choice::Continue,
        Choice::Skip,
        Choice::Edit(" {\"ไทย\":1} ".into()),
        Choice::All,
    ] {
        let (mut e, ledger, d) = setup(choice.clone());
        e.command(Command::DecideStep(d.clone()));
        assert_eq!(e.outgoing.len(), 1);
        revalidate(&mut e, &d);
        assert_eq!(e.outgoing.len(), 1);
        let request = e.outgoing.pop_front().unwrap();
        assert_eq!(
            serde_json::to_value(request.request).unwrap(),
            serde_json::to_value(d.action().request()).unwrap()
        );
        assert!(d.before_write());
        e.receive(ServerMessage::ok(
            request.id,
            json!({"step_id":"p","action":choice.wire()}),
        ))
        .unwrap();
        assert!(matches!(
            ledger.status(),
            Status::Finished {
                outcome: Outcome::Accepted,
                ..
            }
        ));
    }
}
#[test]
fn changed_foreign_missing_or_truncated_prompt_never_queues_a_decision() {
    for change in [
        "session",
        "run",
        "step",
        "tool",
        "arguments",
        "missing",
        "truncated",
    ] {
        let (mut e, ledger, d) = setup(Choice::Continue);
        let req = e.outgoing.pop_front().unwrap();
        let mut value = baseline(&d);
        match change {
            "session" => value["session"]["id"] = json!("other"),
            "run" => value["pending_steps"]["prompts"][0]["run_id"] = json!("other"),
            "step" => value["pending_steps"]["prompts"][0]["step_id"] = json!("other"),
            "tool" => value["pending_steps"]["prompts"][0]["tool_name"] = json!("other"),
            "arguments" => {
                value["pending_steps"]["prompts"][0]["arguments_preview"] =
                    json!("{\"changed\":1}");
                value["pending_steps"]["prompts"][0]["arguments_bytes"] = json!(13);
            }
            "missing" => {
                value.as_object_mut().unwrap().remove("pending_steps");
            }
            _ => value["pending_steps"]["prompts"][0]["arguments_bytes"] = json!(9000),
        }
        e.receive(ServerMessage::ok(req.id, value)).unwrap();
        assert!(e.outgoing.is_empty(), "{change}");
        assert!(matches!(
            ledger.status(),
            Status::Finished {
                outcome: Outcome::Rejected(_),
                ..
            }
        ));
    }
}
#[test]
fn stale_context_and_cancelled_validation_reads_retire_without_a_write() {
    for change in [
        "client",
        "connection",
        "generation",
        "epoch",
        "session",
        "cancel",
    ] {
        for after_reply in [false, true] {
            let (mut e, ledger, d) = setup(Choice::Continue);
            if after_reply {
                revalidate(&mut e, &d);
            }
            match change {
                "client" => e.instance = uuid::Uuid::new_v4(),
                "connection" => e.connection += 1,
                "generation" => e.generation += 1,
                "epoch" => e.epoch += 1,
                "session" => e.active = Some("other".into()),
                _ => ledger.cancel_unsent("closed"),
            }
            e.retire_step_reviews(false);
            assert!(e.outgoing.is_empty());
            assert!(e.pending.is_empty());
            assert!(matches!(
                ledger.status(),
                Status::Finished {
                    outcome: Outcome::Rejected(_),
                    ..
                }
            ));
        }
    }
}
#[test]
fn dispatched_receipt_survives_context_turnover_and_ui_delivery_failure() {
    let (mut e, ledger, d) = setup(Choice::Skip);
    revalidate(&mut e, &d);
    let request = e.outgoing.pop_front().unwrap();
    assert!(d.before_write());
    e.epoch += 1;
    e.active = Some("other".into());
    e.retire_step_reviews(false);
    assert!(e.pending.contains_key(&request.id));
    e.receive(ServerMessage::ok(
        request.id,
        json!({"step_id":"p","action":"skip"}),
    ))
    .unwrap();
    let (tx, _rx) = mpsc::sync_channel(1);
    tx.try_send(Update::Notice("full".into())).unwrap();
    e.updates.push(Update::Notice("would overflow".into()));
    assert!(deliver(&tx, &mut e).is_err());
    drop(e);
    drop(d);
    assert!(matches!(
        ledger.status(),
        Status::Finished {
            outcome: Outcome::Accepted,
            ..
        }
    ));
}
#[test]
fn dropping_transport_before_or_after_decision_write_has_distinct_outcomes() {
    for sent in [false, true] {
        let (mut e, ledger, d) = setup(Choice::Continue);
        if sent {
            revalidate(&mut e, &d);
            assert!(d.before_write());
            e.outgoing.clear();
        }
        drop(e);
        drop(d);
        assert!(
            matches!(
                ledger.status(),
                Status::Finished {
                    outcome: Outcome::Unknown,
                    ..
                }
            ) == sent
        );
        if !sent {
            assert!(matches!(
                ledger.status(),
                Status::Finished {
                    outcome: Outcome::Rejected(_),
                    ..
                }
            ));
        }
    }
}

#[test]
fn command_queue_failure_is_known_unsent_and_a_foreign_client_cannot_dispatch_it() {
    let (commands, mut received) = queue::channel(1);
    let (_, updates) = mpsc::sync_channel(1);
    let (stop, _) = watch::channel(false);
    let client = Client {
        step_ledger: Arc::new(model::Ledger::default()),
        mode_ledger: Arc::new(crate::step_mode::Ledger::default()),
        approval_ledger: Arc::new(crate::approvals::Ledger::default()),
        commands,
        updates,
        stop,
        ready: Arc::new(AtomicBool::new(true)),
        epoch: Arc::new(AtomicU64::new(0)),
        connection: Arc::new(AtomicU64::new(0)),
        forks: Arc::new(session_fork::Ledger::default()),
        stream_epoch: Arc::new(AtomicU64::new(0)),
        instance: uuid::Uuid::new_v4(),
        future_attempts: false,
        #[cfg(feature = "test-support")]
        recorded: None,
    };
    let (_, _, d) = setup(Choice::Continue);
    let a = model::Action {
        client: client.instance(),
        connection: 0,
        epoch: 0,
        ..d.action().clone()
    };
    client.send(Command::Refresh).unwrap();
    assert!(client.decide_step(a.clone()).is_err());
    assert!(matches!(
        client.step_status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
    received.try_recv().unwrap();
    client
        .decide_step(model::Action {
            operation: uuid::Uuid::new_v4(),
            ..a
        })
        .unwrap();
    let Command::DecideStep(queued) = received.try_recv().unwrap() else {
        panic!("decision command")
    };
    assert!(queued.reserved());
    let foreign = Arc::new(model::Ledger::default())
        .reserve(model::Action {
            operation: uuid::Uuid::new_v4(),
            ..queued.action().clone()
        })
        .unwrap();
    assert!(client.send(Command::DecideStep(foreign)).is_err());
    assert!(received.try_recv().is_err());
    assert!(queued.reserved());
    advance_epoch(&client.connection);
    assert!(client.send(Command::DecideStep(queued.clone())).is_err());
    assert!(received.try_recv().is_err());
    drop(queued);
    assert!(matches!(
        client.step_status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
}

#[test]
fn lifecycle_retires_unsent_step_reads_and_writes_but_preserves_sent_receipts() {
    for phase in 0..3 {
        let (mut e, ledger, d) = setup(Choice::Continue);
        let mut id = e.outgoing.front().unwrap().id;
        if phase > 0 {
            revalidate(&mut e, &d);
            id = e.outgoing.front().unwrap().id;
        }
        if phase == 2 {
            assert!(d.before_write());
        }
        e.receive(ServerMessage::Event(Event::StepResolved {
            session_id: "s".into(),
            step_id: "p".into(),
        }))
        .unwrap();
        if phase == 2 {
            e.receive(ServerMessage::ok(
                id,
                json!({"step_id":"p","action":"continue"}),
            ))
            .unwrap();
            assert!(matches!(
                ledger.status(),
                Status::Finished {
                    outcome: Outcome::Accepted,
                    ..
                }
            ));
        } else {
            assert!(e.outgoing.is_empty());
            e.receive(ServerMessage::ok(id, baseline(&d))).unwrap();
            assert!(e.outgoing.is_empty());
            assert!(matches!(
                ledger.status(),
                Status::Finished {
                    outcome: Outcome::Rejected(_),
                    ..
                }
            ));
        }
    }
}
