use super::*;
use crate::approvals::{
    fixture_inventory, Action as Decision, Dispatch, Ledger, Outcome, Prompt, Status,
};
use serde_json::json;
fn setup(allow: bool) -> (Engine, Arc<Ledger>, Dispatch) {
    let l = Arc::new(Ledger::default());
    let a = Decision {
        client: uuid::Uuid::new_v4(),
        connection: 2,
        epoch: 3,
        operation: uuid::Uuid::new_v4(),
        prompt: Prompt::from_list(&fixture_inventory()).unwrap().unwrap(),
        allow,
    };
    let mut e = Engine {
        instance: a.client,
        connection: 2,
        epoch: 3,
        ready: true,
        active: Some("another-selected-session".into()),
        ..Default::default()
    };
    let d = l.reserve(a).unwrap();
    e.command(Command::DecideApproval(d.clone()));
    (e, l, d)
}
fn inspect(e: &mut Engine, value: Value) {
    let r = e.outgoing.pop_front().unwrap();
    assert!(matches!(r.request, Request::PendingApprovals));
    e.receive(ServerMessage::ok(r.id, value)).unwrap();
}
#[test]
fn exact_profile_approval_revalidates_once_and_never_writes_persistent_authority() {
    for allow in [false, true] {
        let (mut e, l, d) = setup(allow);
        e.command(Command::DecideApproval(d.clone()));
        assert_eq!(e.outgoing.len(), 1);
        inspect(&mut e, fixture_inventory());
        e.before_approval_writes().unwrap();
        let revision = e.approval_revision;
        e.before_approval_writes().unwrap();
        assert_eq!(e.approval_revision, revision);
        let r = e.outgoing.pop_front().unwrap();
        assert!(
            matches!(r.request,Request::ApprovalDecision{approval_id,approved,scope:formless_protocol::ApprovalScope::Once,paths,remember_denial:false}if approval_id==d.action().prompt.id()&&approved==allow&&paths.is_empty())
        );
        e.command(Command::DecideApproval(d.clone()));
        assert!(e.outgoing.is_empty());
        e.receive(ServerMessage::ok(
            r.id,
            json!({"resolved":true,"audit_recorded":true}),
        ))
        .unwrap();
        assert!(matches!(
            l.status(),
            Status::Finished {
                outcome: Outcome::Accepted {
                    audit_recorded: true
                },
                ..
            }
        ));
        e.command(Command::DecideApproval(d));
        assert!(e.outgoing.is_empty());
    }
}
#[test]
fn missing_changed_or_multiple_authority_never_reaches_a_decision_write() {
    for mode in 0..6 {
        let (mut e, l, _d) = setup(true);
        let mut v = fixture_inventory();
        match mode {
            0 => v = json!([]),
            1 => v[0]["session_id"] = json!("foreign"),
            2 => v[0]["capabilities"] = json!(["net:*"]),
            3 => v[0]["context"]["call"]["arguments"] = json!({"changed":true}),
            4 => v[0]["context"]["call"]["truncated"] = json!(true),
            _ => {
                let other = v[0].clone();
                v.as_array_mut().unwrap().push(other);
            }
        }
        inspect(&mut e, v);
        assert!(e.outgoing.is_empty(), "{mode}");
        assert!(matches!(
            l.status(),
            Status::Finished {
                outcome: Outcome::Rejected(_),
                ..
            }
        ));
    }
}
#[test]
fn every_changed_connection_or_review_owner_retires_before_write() {
    for before_read in [false, true] {
        for mode in 0..4 {
            let (mut e, l, _d) = setup(true);
            if !before_read {
                inspect(&mut e, fixture_inventory());
            }
            match mode {
                0 => e.instance = uuid::Uuid::new_v4(),
                1 => e.connection += 1,
                2 => e.epoch += 1,
                _ => e.ready = false,
            }
            e.before_approval_writes().unwrap();
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
fn observed_approval_resolution_and_inventory_replacement_retire_unsent_but_preserve_sent_receipts()
{
    for sent in [false, true] {
        for event in [
            Event::ApprovalClosed {
                approval_id: "approval-fixture".into(),
            },
            Event::ApprovalAnswered {
                approval_id: "approval-fixture".into(),
                approved: false,
                by: Some("other client".into()),
            },
            serde_json::from_value::<Event>(fixture_inventory()[0].clone()).unwrap(),
        ] {
            let (mut e, l, _d) = setup(true);
            inspect(&mut e, fixture_inventory());
            let id = e.outgoing.front().unwrap().id;
            if sent {
                e.before_approval_writes().unwrap();
                e.outgoing.clear();
            }
            e.receive(ServerMessage::Event(event)).unwrap();
            if sent {
                assert!(matches!(l.status(), Status::Sent(_)));
                e.receive(ServerMessage::ok(
                    id,
                    json!({"resolved":false,"audit_recorded":null}),
                ))
                .unwrap();
                assert!(matches!(
                    l.status(),
                    Status::Finished {
                        outcome: Outcome::Rejected(_),
                        ..
                    }
                ));
            } else {
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
}
#[test]
fn read_slots_retire_with_a_result_at_both_possible_write_and_receipt_boundaries() {
    let (mut e, _l, _d) = setup(true);
    let old = uuid::Uuid::new_v4();
    e.command(Command::ReadApprovals {
        connection: 2,
        operation: old,
    });
    let old_id = e.outgoing.back().unwrap().id;
    inspect(&mut e, fixture_inventory());
    e.before_approval_writes().unwrap();
    assert!(e.updates.iter().any(
        |u| matches!(u,Update::ApprovalInventory{operation,result:Err(_),..}if *operation==old)
    ));
    let setter = e.outgoing.pop_front().unwrap();
    assert!(matches!(setter.request, Request::ApprovalDecision { .. }));
    let middle = uuid::Uuid::new_v4();
    e.command(Command::ReadApprovals {
        connection: 2,
        operation: middle,
    });
    let middle_id = e.outgoing.back().unwrap().id;
    e.receive(ServerMessage::ok(
        setter.id,
        json!({"resolved":true,"audit_recorded":true}),
    ))
    .unwrap();
    assert!(e.updates.iter().any(
        |u| matches!(u,Update::ApprovalInventory{operation,result:Err(_),..}if *operation==middle)
    ));
    e.receive(ServerMessage::ok(old_id, fixture_inventory()))
        .unwrap();
    e.receive(ServerMessage::ok(middle_id, fixture_inventory()))
        .unwrap();
    let fresh = uuid::Uuid::new_v4();
    e.command(Command::ReadApprovals {
        connection: 2,
        operation: fresh,
    });
    let r = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::ok(r.id, json!([]))).unwrap();
    assert!(e.updates.iter().any(|u|matches!(u,Update::ApprovalInventory{operation,revision,result:Ok(None),..}if *operation==fresh&&*revision==e.approval_revision)));
}
#[test]
fn lost_possible_write_owner_is_unknown_and_blocks_explicit_retry() {
    let (mut e, l, d) = setup(true);
    inspect(&mut e, fixture_inventory());
    e.before_approval_writes().unwrap();
    e.pending.clear();
    e.outgoing.clear();
    drop(d);
    assert!(matches!(
        l.status(),
        Status::Finished {
            outcome: Outcome::Unknown,
            ..
        }
    ));
    let mut a = Decision {
        client: e.instance,
        connection: 2,
        epoch: 3,
        operation: uuid::Uuid::new_v4(),
        prompt: Prompt::from_list(&fixture_inventory()).unwrap().unwrap(),
        allow: false,
    };
    a.connection += 1;
    assert!(l.reserve(a).is_err());
}
#[test]
fn readonly_inventory_is_bounded_and_cannot_fabricate_empty_success_while_unready() {
    let mut e = Engine {
        instance: uuid::Uuid::new_v4(),
        ready: true,
        connection: 2,
        ..Default::default()
    };
    for _ in 0..3 {
        e.command(Command::ReadApprovals {
            connection: 2,
            operation: uuid::Uuid::new_v4(),
        });
    }
    assert_eq!(e.outgoing.len(), 2);
    assert!(e
        .updates
        .iter()
        .any(|u| matches!(u, Update::ApprovalInventory { result: Err(_), .. })));
    e.ready = false;
    e.command(Command::ReadApprovals {
        connection: 2,
        operation: uuid::Uuid::new_v4(),
    });
    assert_eq!(e.outgoing.len(), 2);
}

#[test]
fn client_queue_failure_foreign_ownership_and_synchronous_context_cancel_are_distinct() {
    let (commands, mut received) = queue::channel(1);
    let (_, updates) = mpsc::sync_channel(1);
    let (stop, _) = watch::channel(false);
    let c = Client {
        step_ledger: Arc::new(crate::step_decision::Ledger::default()),
        mode_ledger: Arc::new(crate::step_mode::Ledger::default()),
        approval_ledger: Arc::new(Ledger::default()),
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
    let action = || Decision {
        client: c.instance(),
        connection: c.connection(),
        epoch: c.epoch(),
        operation: uuid::Uuid::new_v4(),
        prompt: Prompt::from_list(&fixture_inventory()).unwrap().unwrap(),
        allow: true,
    };
    c.send(Command::Refresh).unwrap();
    assert!(c.decide_approval(action()).is_err());
    assert!(matches!(
        c.approval_status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
    received.try_recv().unwrap();
    let foreign = Arc::new(Ledger::default());
    let d = foreign.reserve(action()).unwrap();
    assert!(c.send(Command::DecideApproval(d)).is_err());
    assert!(received.try_recv().is_err());
    let d = c.approval_ledger.reserve(action()).unwrap();
    c.invalidate_context();
    assert!(!d.before_write());
    assert!(matches!(
        c.approval_status(),
        Status::Finished {
            outcome: Outcome::Rejected(_),
            ..
        }
    ));
    let sent = c.approval_ledger.reserve(action()).unwrap();
    assert!(sent.before_write());
    c.invalidate_context();
    assert!(sent.sent());
    drop(sent);
    assert!(matches!(
        c.approval_status(),
        Status::Finished {
            outcome: Outcome::Unknown,
            ..
        }
    ));
    assert!(c.decide_approval(action()).is_err());
}
