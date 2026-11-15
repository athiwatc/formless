use super::*;
use crate::step_mode::{Action as ModeAction, Dispatch, Ledger, Outcome, Status};
use serde_json::json;
fn setup() -> (Engine, Arc<Ledger>, Dispatch) {
    setup_target(true)
}
fn setup_target(on: bool) -> (Engine, Arc<Ledger>, Dispatch) {
    let instance = uuid::Uuid::new_v4();
    let l = Arc::new(Ledger::default());
    let d = l
        .reserve(ModeAction {
            client: instance,
            connection: 1,
            generation: 2,
            view_epoch: 3,
            operation: uuid::Uuid::new_v4(),
            session_id: "s".into(),
            on,
        })
        .unwrap();
    let mut e = Engine {
        instance,
        ready: true,
        connection: 1,
        generation: 2,
        stream_epoch: 3,
        active: Some("s".into()),
        ..Default::default()
    };
    e.command(if on {
        Command::EnableStepMode(d.clone())
    } else {
        Command::DisableStepMode(d.clone())
    });
    (e, l, d)
}
fn value(on: bool) -> Value {
    json!({"session":{"id":"s"},"step_mode":on,"messages":[],"running":false})
}
fn inspect(e: &mut Engine, on: bool) {
    let r = e.outgoing.pop_front().unwrap();
    assert!(matches!(r.request,Request::GetSession{id} if id=="s"));
    e.receive(ServerMessage::ok(r.id, value(on))).unwrap();
}
#[test]
fn mode_enable_revalidates_and_spends_one_write_with_strict_receipt() {
    let (mut e, l, d) = setup();
    e.command(Command::EnableStepMode(d.clone()));
    assert_eq!(e.outgoing.len(), 1);
    inspect(&mut e, false);
    e.before_mode_writes().unwrap();
    let revision = e.status_revision;
    e.before_mode_writes().unwrap();
    assert_eq!(e.status_revision, revision);
    let r = e.outgoing.pop_front().unwrap();
    assert!(matches!(r.request,Request::SetStepMode{session_id,on:true} if session_id=="s"));
    e.command(Command::EnableStepMode(d.clone()));
    assert!(e.outgoing.is_empty());
    e.receive(ServerMessage::ok(r.id, json!({"session_id":"s","on":true})))
        .unwrap();
    assert!(matches!(
        l.status(),
        Status::Finished {
            outcome: Outcome::Enabled,
            ..
        }
    ));
    e.command(Command::EnableStepMode(d));
    assert!(e.outgoing.is_empty());
}
#[test]
fn mode_enable_observes_already_on_and_refuses_missing_malformed_or_foreign_state() {
    for v in [
        value(true),
        json!({"session":{"id":"s"}}),
        json!({"session":{"id":"s"},"step_mode":"false"}),
        json!({"session":{"id":"other"},"step_mode":false}),
    ] {
        let (mut e, l, _d) = setup();
        let r = e.outgoing.pop_front().unwrap();
        let on = v["step_mode"] == true;
        e.receive(ServerMessage::ok(r.id, v)).unwrap();
        assert!(e.outgoing.is_empty());
        assert!(if on {
            matches!(
                l.status(),
                Status::Finished {
                    outcome: Outcome::ObservedEnabled,
                    ..
                }
            )
        } else {
            matches!(
                l.status(),
                Status::Finished {
                    outcome: Outcome::Rejected(_),
                    ..
                }
            )
        });
    }
}
#[test]
fn mode_enable_retires_every_changed_owner_or_lifecycle_before_write() {
    for commit in [false, true] {
        for field in 0..7 {
            let (mut e, l, _d) = setup();
            if commit {
                inspect(&mut e, false);
            }
            match field {
                0 => e.connection += 1,
                1 => e.generation += 1,
                2 => e.active = Some("other".into()),
                3 => e.stream_epoch += 1,
                4 => e.ready = false,
                5 => e.instance = uuid::Uuid::new_v4(),
                _ => e.status_revision += 1,
            };
            e.retire_mode_changes();
            assert!(e.outgoing.is_empty());
            assert!(matches!(
                l.status(),
                Status::Finished {
                    outcome: Outcome::Rejected(_),
                    ..
                }
            ));
        }
    }
    let (mut e, _, d) = setup();
    e.sync_epoch(99);
    e.retire_mode_changes();
    assert!(
        d.reserved(),
        "ordinary draft/forecast changes do not retarget a Session setting"
    );
}
#[test]
fn mode_enable_fences_mode_samples_before_write_and_before_ack() {
    let (mut e, _, _d) = setup();
    let inspect = e.outgoing.pop_front().unwrap();
    e.dirty_session = true;
    e.refresh();
    let before = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::ok(inspect.id, value(false)))
        .unwrap();
    e.before_mode_writes().unwrap();
    let commit = e.outgoing.pop_front().unwrap();
    e.updates.clear();
    e.receive(ServerMessage::ok(before.id, value(false)))
        .unwrap();
    assert!(!e
        .updates
        .iter()
        .any(|u| matches!(u, Update::Snapshot { .. })));
    e.refresh();
    let during = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::ok(
        commit.id,
        json!({"session_id":"s","on":true}),
    ))
    .unwrap();
    e.updates.clear();
    e.receive(ServerMessage::ok(during.id, value(false)))
        .unwrap();
    assert!(!e
        .updates
        .iter()
        .any(|u| matches!(u, Update::Snapshot { .. })));
    e.refresh();
    let fresh = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::ok(fresh.id, value(true))).unwrap();
    assert!(e
        .updates
        .iter()
        .any(|u| matches!(u,Update::Snapshot{snapshot,..} if snapshot.step_mode==Some(true))));
}
#[test]
fn mode_enable_owner_drop_after_possible_write_is_unknown_not_replayed() {
    let (mut e, l, d) = setup();
    inspect(&mut e, false);
    e.before_mode_writes().unwrap();
    drop(d);
    drop(e);
    assert!(matches!(
        l.status(),
        Status::Finished {
            outcome: Outcome::Unknown,
            ..
        }
    ));
    assert!(l
        .reserve(ModeAction {
            client: uuid::Uuid::new_v4(),
            connection: 2,
            generation: 2,
            view_epoch: 4,
            operation: uuid::Uuid::new_v4(),
            session_id: "s".into(),
            on: true,
        })
        .is_err());
}
#[test]
fn mode_enable_queue_failure_is_unsent_and_foreign_dispatch_cannot_be_sent() {
    let (commands, _received) = queue::channel(1);
    let (_, updates) = mpsc::sync_channel(1);
    let (stop, _) = watch::channel(false);
    let client = Client {
        step_ledger: Arc::new(crate::step_decision::Ledger::default()),
        mode_ledger: Arc::new(Ledger::default()),
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
    let a = ModeAction {
        client: client.instance(),
        connection: 0,
        generation: 1,
        view_epoch: 0,
        operation: uuid::Uuid::new_v4(),
        session_id: "s".into(),
        on: true,
    };
    client.send(Command::Refresh).unwrap();
    assert!(client.enable_step_mode(a.clone()).is_err());
    assert!(matches!(
        client.step_mode_status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
    let other = Arc::new(Ledger::default());
    let d = other
        .reserve(ModeAction {
            operation: uuid::Uuid::new_v4(),
            ..a
        })
        .unwrap();
    assert!(client.send(Command::EnableStepMode(d)).is_err());
    assert!(matches!(
        other.status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
}
#[test]
fn mode_enable_view_retirement_cancels_unsent_synchronously_but_draft_edits_do_not() {
    let (commands, _receiver) = queue::channel(4);
    let (_, updates) = mpsc::sync_channel(4);
    let (stop, _) = watch::channel(false);
    let client = Client {
        step_ledger: Arc::new(crate::step_decision::Ledger::default()),
        mode_ledger: Arc::new(Ledger::default()),
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
    let a = ModeAction {
        client: client.instance(),
        connection: client.connection(),
        generation: 1,
        view_epoch: client.stream_epoch(),
        operation: uuid::Uuid::new_v4(),
        session_id: "s".into(),
        on: true,
    };
    let d = client.mode_ledger.reserve(a).unwrap();
    client.invalidate_context();
    assert!(d.reserved());
    let prompt=serde_json::from_value(json!({"session_id":"s","run_id":"r","step_id":"p","tool_name":"fixture","arguments_preview":"{}","arguments_bytes":2,"index":0,"count":1})).unwrap();
    let step = client
        .step_ledger
        .reserve(crate::step_decision::Action {
            client: client.instance(),
            connection: 0,
            generation: 1,
            epoch: client.epoch(),
            operation: uuid::Uuid::new_v4(),
            prompt,
            choice: crate::step_decision::Choice::Continue,
        })
        .unwrap();
    client.invalidate_context();
    assert!(!step.before_write());
    assert!(d.reserved());
    client
        .send(Command::Select {
            session_id: Some("other".into()),
            generation: 2,
        })
        .unwrap();
    assert!(!d.before_write());
    assert!(matches!(
        client.step_mode_status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
}

#[test]
fn mode_enable_observed_session_delete_retires_inspection_before_any_setting() {
    let (mut e, l, _d) = setup();
    let read = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::Event(Event::SessionDeleted {
        id: "s".into(),
    }))
    .unwrap();
    e.receive(ServerMessage::ok(read.id, value(false))).unwrap();
    assert!(!e
        .outgoing
        .iter()
        .any(|r| matches!(r.request, Request::SetStepMode { .. })));
    assert!(matches!(
        l.status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
}

fn idle_off_value() -> Value {
    let mut v = value(true);
    v["pending_steps"] = json!({"prompts":[],"more":false});
    v
}
fn inspect_off(e: &mut Engine, value: Value) {
    let r = e.outgoing.pop_front().unwrap();
    assert!(matches!(r.request,Request::GetSession{id} if id=="s"));
    e.receive(ServerMessage::ok(r.id, value)).unwrap();
}
#[test]
fn mode_disable_requires_a_fresh_idle_empty_inventory_and_writes_only_false_once() {
    let (mut e, l, d) = setup_target(false);
    e.command(Command::DisableStepMode(d.clone()));
    assert_eq!(e.outgoing.len(), 1);
    inspect_off(&mut e, idle_off_value());
    e.before_mode_writes().unwrap();
    let revision = e.status_revision;
    e.before_mode_writes().unwrap();
    assert_eq!(e.status_revision, revision);
    let r = e.outgoing.pop_front().unwrap();
    assert!(matches!(r.request,Request::SetStepMode{session_id,on:false} if session_id=="s"));
    e.command(Command::DisableStepMode(d.clone()));
    assert!(e.outgoing.is_empty());
    e.receive(ServerMessage::ok(
        r.id,
        json!({"session_id":"s","on":false}),
    ))
    .unwrap();
    assert!(matches!(
        l.status(),
        Status::Finished {
            outcome: Outcome::Disabled,
            ..
        }
    ));
    e.command(Command::DisableStepMode(d));
    assert!(e.outgoing.is_empty());
}
#[test]
fn mode_disable_rejects_running_recoverable_pending_missing_and_partial_state() {
    for field in 0..9 {
        let (mut e, l, _d) = setup_target(false);
        let mut v = idle_off_value();
        match field {
            0 => v["running"] = json!(true),
            1 => {
                v.as_object_mut().unwrap().remove("running");
            }
            2 => v["unfinished_run"] = json!({"id":"unfinished"}),
            3 => {
                v.as_object_mut().unwrap().remove("pending_steps");
            }
            4 => v["pending_steps"]["more"] = json!(true),
            5 => {
                v["pending_steps"]["prompts"] = json!([{"session_id":"s","run_id":"r","step_id":"p","tool_name":"tool","arguments_preview":"{}","arguments_bytes":2,"index":0,"count":1}])
            }
            6 => v["pending_steps"] = json!({"prompts":[],"more":"false"}),
            7 => v["session"]["id"] = json!("other"),
            _ => v["step_mode"] = json!("true"),
        }
        inspect_off(&mut e, v);
        assert!(e.outgoing.is_empty(), "{field}");
        assert!(
            matches!(
                l.status(),
                Status::Finished {
                    outcome: Outcome::Rejected(_),
                    ..
                }
            ),
            "{field}"
        );
    }
    let (mut e, l, _d) = setup_target(false);
    let mut v = value(false);
    v["running"] = json!(true);
    inspect_off(&mut e, v);
    assert!(e.outgoing.is_empty());
    assert!(matches!(
        l.status(),
        Status::Finished {
            outcome: Outcome::ObservedDisabled,
            ..
        }
    ));
}
#[test]
fn mode_disable_retires_all_changed_owners_and_status_before_write() {
    for commit in [false, true] {
        for field in 0..7 {
            let (mut e, l, _d) = setup_target(false);
            if commit {
                inspect_off(&mut e, idle_off_value());
            }
            match field {
                0 => e.connection += 1,
                1 => e.generation += 1,
                2 => e.active = Some("other".into()),
                3 => e.stream_epoch += 1,
                4 => e.ready = false,
                5 => e.instance = uuid::Uuid::new_v4(),
                _ => e.status_revision += 1,
            };
            e.retire_mode_changes();
            assert!(e.outgoing.is_empty());
            assert!(matches!(
                l.status(),
                Status::Finished {
                    outcome: Outcome::Rejected(_),
                    ..
                }
            ));
        }
    }
}
#[test]
fn mode_disable_requires_exact_false_receipt_and_unknown_blocks_both_targets() {
    let (mut e, l, _d) = setup_target(false);
    inspect_off(&mut e, idle_off_value());
    e.before_mode_writes().unwrap();
    let r = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::ok(r.id, json!({"session_id":"s","on":true})))
        .unwrap();
    assert!(matches!(
        l.status(),
        Status::Finished {
            outcome: Outcome::Unknown,
            ..
        }
    ));
    for on in [false, true] {
        let mut next = l.status().action().unwrap().clone();
        next.on = on;
        next.operation = uuid::Uuid::new_v4();
        assert!(l.reserve(next).is_err());
    }
}
