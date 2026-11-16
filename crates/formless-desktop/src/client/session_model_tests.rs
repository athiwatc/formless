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
        session_model_cas_available: true,
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
        matches!(r.request,Request::CompareSetSessionModel{id,expected,desired}if id=="s"&&expected.model.as_deref()==Some("old-model")&&expected.reasoning_effort.as_deref()==Some("low")&&desired.model.as_deref()==Some("role:reviewer")&&desired.reasoning_effort.as_deref()==Some("low"))
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
        Request::CompareSetSessionModel { .. }
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

#[test]
fn session_selection_cas_missing_capability_rejects_before_read_and_never_falls_back() {
    let mut e = engine();
    e.session_model_cas_available = false;
    e.command(command(uuid::Uuid::new_v4(), change()));
    assert!(e.outgoing.is_empty());
    assert!(e.updates.iter().any(|u|matches!(u,Update::SessionEdited{outcome:Outcome::Rejected(text),..}if text.contains("atomic"))));
    for features in [json!([]), json!(["session_model_compare_set_v1"])] {
        let mut e = Engine::default();
        e.begin();
        let ping = e.outgoing.pop_front().unwrap();
        e.receive(ServerMessage::ok(
            ping.id,
            json!({"min_protocol":1,"max_protocol":1,"features":features}),
        ))
        .unwrap();
        assert_eq!(
            e.session_model_cas_available,
            features.as_array().unwrap().len() == 1
        );
    }
}

#[cfg(feature = "daemon-tests")]
#[tokio::test]
async fn session_selection_cas_real_core_rejects_writer_between_native_read_and_write() {
    let _env = credential_env::EnvGuard::isolated();
    let root =
        std::env::temp_dir().join(format!("fl-native-selection-cas-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let paths = formless_core::Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = formless_core::Config::default();
    cfg.provider.auto_test_models = false;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let st = formless_core::Store::open(&paths.db_file()).unwrap();
    let sid = st.create_session(None).unwrap().id;
    st.set_session_model(&sid, Some("old-model".into()), Some("low".into()))
        .unwrap();
    drop(st);
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let (stream, server) = tokio::io::duplex(128 * 1024);
    let (tx, mut commands) = queue::channel(QUEUE);
    let (updates, rx) = mpsc::sync_channel(QUEUE);
    let (stop, mut cancelled) = watch::channel(false);
    let ready = Arc::new(AtomicBool::new(false));
    let epoch = AtomicU64::new(0);
    let connection = AtomicU64::new(0);
    let mut retained = Retained {
        instance: uuid::Uuid::new_v4(),
        ..Default::default()
    };
    let driver = tokio::spawn({
        let sid = sid.clone();
        let ready = ready.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !ready.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            let operation = uuid::Uuid::new_v4();
            let change = Change::Selection {
                expected: crate::session_edit::ModelSelection {
                    model: Some("old-model".into()),
                    reasoning_effort: Some("low".into()),
                },
                desired: crate::session_edit::ModelSelection {
                    model: Some("role:reviewer".into()),
                    reasoning_effort: Some("low".into()),
                },
            };
            tx.send(Command::EditSession {
                connection: 0,
                operation,
                session_id: sid,
                change,
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async move {
                loop {
                    while let Ok(update) = rx.try_recv() {
                        if let Update::SessionEdited { operation: found, outcome, .. } = update {
                            if found == operation {
                                assert!(matches!(outcome, Outcome::Rejected(text) if text.contains("changed")));
                                return;
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }).await.unwrap();
            stop.send(true).unwrap();
            (tx, stop)
        }
    });
    let peer = tokio::spawn({
        let file = paths.db_file();
        async move {
            let mut r = BufReader::new(server);
            let mut writes = 0;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                let request: RequestEnvelope = serde_json::from_str(&line).unwrap();
                match &request.request {
                    Request::Ping | Request::Subscribe { .. } | Request::ListSessions => {}
                    Request::CompareSetSessionModel {
                        id,
                        expected,
                        desired,
                    } => {
                        writes += 1;
                        assert_eq!(expected.model.as_deref(), Some("old-model"));
                        assert_eq!(expected.reasoning_effort.as_deref(), Some("low"));
                        assert_eq!(desired.model.as_deref(), Some("role:reviewer"));
                        let external = formless_core::Store::open(&file).unwrap();
                        external
                            .set_session_model(id, Some("old-model".into()), Some("medium".into()))
                            .unwrap();
                    }
                    other => panic!("unbound/fallback/Provider-starting request {other:?}"),
                }
                let result = if let Request::Subscribe { deltas: false, .. } = &request.request {
                    Ok(json!({"subscribed":true}))
                } else {
                    daemon
                        .handle(request.request)
                        .await
                        .map_err(|e| e.to_string())
                };
                let response = match result {
                    Ok(value) => ServerMessage::ok(request.id, value),
                    Err(error) => ServerMessage::err(request.id, error),
                };
                let mut bytes = serde_json::to_vec(&response).unwrap();
                bytes.push(b'\n');
                r.get_mut().write_all(&bytes).await.unwrap();
            }
            writes
        }
    });
    tokio::time::timeout(
        Duration::from_secs(10),
        connected(
            stream,
            &mut commands,
            &updates,
            &mut cancelled,
            &mut retained,
            ConnectionState {
                ready: &ready,
                epoch: &epoch,
                connection: &connection,
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let _sender = driver.await.unwrap();
    assert_eq!(peer.await.unwrap(), 1);
    let st = formless_core::Store::open_reader(&paths.db_file()).unwrap();
    let row = st.get_session(&sid).unwrap().unwrap();
    assert_eq!(row.model.as_deref(), Some("old-model"));
    assert_eq!(row.reasoning_effort.as_deref(), Some("medium"));
    assert!(st.list_messages(&sid).unwrap().is_empty());
    assert_eq!(st.queued_count(&sid).unwrap(), 0);
    drop(st);
    std::fs::remove_dir_all(root).unwrap();
}
