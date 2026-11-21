use super::*;
use serde_json::json;
#[test]
fn pending_inventory_requests_are_explicit_bounded_and_do_not_queue_mutations() {
    let mut e = Engine {
        instance: uuid::Uuid::new_v4(),
        ready: true,
        connection: 2,
        active: Some("other-session".into()),
        ..Default::default()
    };
    let ops = [
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
    ];
    for operation in ops {
        e.command(Command::ReadPendingAttachments {
            connection: 2,
            operation,
            session_id: "chosen".into(),
        });
    }
    assert_eq!(e.outgoing.len(), 2);
    let first = e.outgoing.pop_front().unwrap();
    let second = e.outgoing.pop_front().unwrap();
    assert!(
        matches!(first.request,Request::ListPendingAttachments{session_id}if session_id=="chosen")
    );
    e.receive(ServerMessage::ok(
        second.id,
        json!({"session_id":"foreign","attachments":[]}),
    ))
    .unwrap();
    e.receive(ServerMessage::ok(
        first.id,
        json!({"session_id":"chosen","attachments":[]}),
    ))
    .unwrap();
    assert!(e.updates.iter().any(|u|matches!(u,Update::PendingAttachments{client,operation,result:Ok(r),..}if *client==e.instance&&*operation==ops[0]&&r.rows.is_empty())));
    assert!(e.updates.iter().any(
        |u| matches!(u,Update::PendingAttachments{operation,result:Err(_),..}if *operation==ops[1])
    ));
    assert!(e.outgoing.is_empty());
    for (connection, id) in [(1, "chosen"), (2, ""), (2, "bad\nidentity")] {
        e.command(Command::ReadPendingAttachments {
            connection,
            operation: uuid::Uuid::new_v4(),
            session_id: id.into(),
        });
        assert!(e.outgoing.is_empty());
    }
    e.ready = false;
    e.command(Command::ReadPendingAttachments {
        connection: 2,
        operation: uuid::Uuid::new_v4(),
        session_id: "chosen".into(),
    });
    assert!(e.outgoing.is_empty());
}
#[test]
fn pending_inventory_older_peer_error_never_becomes_an_empty_snapshot() {
    let mut e = Engine {
        instance: uuid::Uuid::new_v4(),
        ready: true,
        connection: 2,
        ..Default::default()
    };
    let op = uuid::Uuid::new_v4();
    e.command(Command::ReadPendingAttachments {
        connection: 2,
        operation: op,
        session_id: "s".into(),
    });
    let r = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::err(
        r.id,
        "unsupported: private detail".into(),
    ))
    .unwrap();
    assert!(e.updates.iter().any(|u|matches!(u,Update::PendingAttachments{operation,result:Err(error),..}if *operation==op&&!error.contains("private detail"))));
    assert!(e.outgoing.is_empty());
}
