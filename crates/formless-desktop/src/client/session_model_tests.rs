use super::*;
use crate::session_edit::{Change, Field, Metadata, Outcome};
use formless_protocol::SendMode;
use serde_json::json;
fn session() -> SessionInfo {
    let mut s = crate::fixture::sessions(1).remove(0);
    s.id = "s".into();
    s.model = Some("old-model".into());
    s.reasoning_effort = Some("low".into());
    s
}
fn engine() -> Engine {
    Engine {
        instance: uuid::Uuid::new_v4(),
        ready: true,
        connection: 2,
        epoch: 3,
        generation: 4,
        active: Some("s".into()),
        ..Default::default()
    }
}
fn change() -> Change {
    Change::from_input(
        Field::Model,
        "role:reviewer",
        &Metadata::from_session(&session()).unwrap(),
    )
    .unwrap()
}
fn command(operation: uuid::Uuid, change: Change) -> Command {
    Command::EditSession {
        connection: 2,
        operation,
        session_id: "s".into(),
        change,
    }
}
fn inspect(e: &mut Engine, value: Value) {
    let r = e.outgoing.pop_front().unwrap();
    assert!(matches!(r.request, Request::ListSessions));
    e.receive(ServerMessage::ok(r.id, value)).unwrap();
}
fn selected() -> SessionInfo {
    let mut s = session();
    s.model = Some("role:reviewer".into());
    s
}
#[test]
fn session_model_fresh_pair_writes_exact_selection_once_and_already_selected_is_read_only() {
    let mut e = engine();
    let op = uuid::Uuid::new_v4();
    let c = change();
    e.command(command(op, c.clone()));
    e.command(command(op, c.clone()));
    assert_eq!(e.outgoing.len(), 1);
    inspect(&mut e, json!([session()]));
    let r = e.outgoing.pop_front().unwrap();
    assert!(
        matches!(r.request,Request::SetSessionModel{id,model:Some(model),reasoning_effort:Some(e)}if id=="s"&&model=="role:reviewer"&&e=="low")
    );
    e.command(command(op, c.clone()));
    assert!(e.outgoing.is_empty());
    e.receive(ServerMessage::Event(Event::SessionUpdated {
        session: selected(),
    }))
    .unwrap();
    e.receive(ServerMessage::ok(r.id, json!(selected())))
        .unwrap();
    assert!(e.updates.iter().any(|u|matches!(u,Update::SessionEdited{operation,outcome:Outcome::Applied(_),..}if *operation==op)));
    e.command(command(op, c));
    inspect(&mut e, json!([selected()]));
    assert!(
        e.outgoing.is_empty(),
        "already-selected pair must not run another paid model test"
    );
}
#[test]
fn session_model_changed_missing_duplicate_or_malformed_pair_never_reaches_setter() {
    for mode in 0..7 {
        let mut e = engine();
        e.command(command(uuid::Uuid::new_v4(), change()));
        let mut value = json!([session()]);
        match mode {
            0 => value = json!([]),
            1 => value = json!([session(), session()]),
            2 => value[0]["model"] = json!("other"),
            3 => value[0]["reasoning_effort"] = json!("high"),
            4 => {
                value[0].as_object_mut().unwrap().remove("reasoning_effort");
            }
            5 => value[0]["model"] = json!("x".repeat(crate::session_edit::MAX_MODEL_BYTES + 1)),
            _ => value[0]["id"] = json!("other"),
        };
        inspect(&mut e, value);
        assert!(e.outgoing.is_empty(), "{mode}");
        assert!(
            e.updates.iter().any(|u| matches!(
                u,
                Update::SessionEdited {
                    outcome: Outcome::Rejected(_),
                    ..
                }
            )),
            "{mode}"
        );
    }
}
#[test]
fn session_model_observed_metadata_changes_retire_each_unsent_phase() {
    for commit in [false, true] {
        let mut e = engine();
        let op = uuid::Uuid::new_v4();
        e.command(command(op, change()));
        if commit {
            inspect(&mut e, json!([session()]));
        }
        let mut changed = session();
        changed.reasoning_effort = Some("high".into());
        e.receive(ServerMessage::Event(Event::SessionUpdated {
            session: changed,
        }))
        .unwrap();
        assert!(e.outgoing.is_empty());
        assert!(e.updates.iter().any(|u|matches!(u,Update::SessionEdited{operation,outcome:Outcome::Rejected(_),..}if *operation==op)));
    }
}
#[test]
fn session_model_pending_read_and_write_block_new_forecasts_and_work_but_not_stop() {
    for commit in [false, true] {
        let mut e = engine();
        e.command(command(uuid::Uuid::new_v4(), change()));
        if commit {
            inspect(&mut e, json!([session()]));
        }
        e.outgoing.clear();
        let intent = Intent::SendMessage {
            session_id: "s".into(),
            content: "kept draft".into(),
            mode: SendMode::Send,
        };
        e.command(Command::Review {
            intent: intent.clone(),
            draft_basis: "kept draft".into(),
            epoch: 3,
        });
        e.command(Command::Intent(intent));
        assert!(e.outgoing.is_empty());
        assert!(e
            .updates
            .iter()
            .any(|u| matches!(u, Update::PreparationFailed { epoch: 3, .. })));
        e.command(Command::Intent(Intent::InterruptRun {
            session_id: "s".into(),
        }));
        assert!(
            matches!(e.outgoing.pop_front().unwrap().request,Request::InterruptRun{session_id}if session_id=="s")
        );
    }
    let mut e = engine();
    e.command(Command::Intent(Intent::ResumeRun {
        session_id: "s".into(),
    }));
    e.outgoing.clear();
    e.command(command(uuid::Uuid::new_v4(), change()));
    assert!(e.outgoing.is_empty());
    assert!(e.updates.iter().any(|u| matches!(
        u,
        Update::SessionEdited {
            outcome: Outcome::Rejected(_),
            ..
        }
    )));
}
#[test]
fn session_model_positive_receipt_must_expose_both_exact_values_or_stays_unknown() {
    for field in ["model", "reasoning_effort"] {
        let mut e = engine();
        e.command(command(uuid::Uuid::new_v4(), change()));
        inspect(&mut e, json!([session()]));
        let r = e.outgoing.pop_front().unwrap();
        let mut ack = json!(selected());
        ack.as_object_mut().unwrap().remove(field);
        e.receive(ServerMessage::ok(r.id, ack)).unwrap();
        assert!(e.updates.iter().any(|u| matches!(
            u,
            Update::SessionEdited {
                outcome: Outcome::Unknown,
                ..
            }
        )));
        assert!(e
            .updates
            .iter()
            .any(|u| matches!(u,Update::Notice(text) if text.contains("outcome unknown"))));
        assert!(e.outgoing.is_empty());
    }
}

#[test]
fn session_model_unrelated_metadata_events_preserve_the_same_paired_baseline() {
    let mut e = engine();
    e.command(command(uuid::Uuid::new_v4(), change()));
    let mut updated = session();
    updated.name = "A different name".into();
    e.receive(ServerMessage::Event(Event::SessionUpdated {
        session: updated,
    }))
    .unwrap();
    assert_eq!(e.outgoing.len(), 1);
    inspect(&mut e, json!([session()]));
    assert!(matches!(
        e.outgoing.pop_front().unwrap().request,
        Request::SetSessionModel { .. }
    ));
}

#[test]
fn model_catalog_worker_is_explicit_bounded_and_bound_to_the_physical_connection() {
    let mut e = engine();
    assert!(e.outgoing.is_empty());
    let op = uuid::Uuid::new_v4();
    e.command(Command::ReadModelCatalog {
        connection: 2,
        operation: op,
    });
    e.command(Command::ReadModelCatalog {
        connection: 2,
        operation: uuid::Uuid::new_v4(),
    });
    assert_eq!(e.outgoing.len(), 1);
    let r = e.outgoing.pop_front().unwrap();
    assert!(matches!(r.request, Request::ListModels));
    assert!(e
        .updates
        .iter()
        .any(|u| matches!(u, Update::ModelCatalog { result: Err(_), .. })));
    e.receive(ServerMessage::ok(
        r.id,
        json!({"models":["m"],"aliases":{},"roles":[],"recent":[],"error":null}),
    ))
    .unwrap();
    assert!(e.updates.iter().any(|u|matches!(u,Update::ModelCatalog{instance,connection:2,operation,result:Ok(c)}if *instance==e.instance&&*operation==op&&c.choices[0].selector=="m")));
    assert!(
        e.outgoing.is_empty(),
        "a catalog response must not initiate a setter, probe, forecast or Run"
    );
    for connection in [0, 3] {
        e.command(Command::ReadModelCatalog {
            connection,
            operation: uuid::Uuid::new_v4(),
        });
        assert!(e.outgoing.is_empty());
    }
    e.command(Command::ReadModelCatalog {
        connection: 2,
        operation: uuid::Uuid::nil(),
    });
    assert!(e.outgoing.is_empty());
    e.ready = false;
    e.command(Command::ReadModelCatalog {
        connection: 2,
        operation: uuid::Uuid::new_v4(),
    });
    assert!(e.outgoing.is_empty());
}
#[test]
fn model_catalog_worker_does_not_display_provider_errors_or_accept_unbounded_replies() {
    let mut e = engine();
    let op = uuid::Uuid::new_v4();
    e.command(Command::ReadModelCatalog {
        connection: 2,
        operation: op,
    });
    let r = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::err(r.id, "private-provider-token".into()))
        .unwrap();
    let notice = e
        .updates
        .iter()
        .find_map(|u| match u {
            Update::ModelCatalog { result: Err(e), .. } => Some(e),
            _ => None,
        })
        .unwrap();
    assert!(!notice.contains("private-provider-token"));
    e.command(Command::ReadModelCatalog {
        connection: 2,
        operation: uuid::Uuid::new_v4(),
    });
    let r = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::ok(
        r.id,
        json!({"models":["m".repeat(513)],"aliases":{},"roles":[],"recent":[],"error":null}),
    ))
    .unwrap();
    assert!(matches!(
        e.updates.last(),
        Some(Update::ModelCatalog { result: Err(_), .. })
    ));
}
