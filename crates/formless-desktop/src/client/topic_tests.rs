use super::*;
use crate::topics::{Action as TopicAction, Reply};

fn engine() -> Engine {
    Engine {
        ready: true,
        topics_available: true,
        connection: 2,
        ..Default::default()
    }
}
fn command(action: TopicAction, operation: uuid::Uuid) -> Command {
    Command::Topic {
        connection: 2,
        operation: Some(operation),
        action,
    }
}

#[test]
fn explicit_move_and_start_chains_once_but_move_only_and_reject_never_start() {
    for action in [
        TopicAction::MoveAndStart("topic-formless".into()),
        TopicAction::Approve("topic-formless".into()),
        TopicAction::Reject("topic-formless".into()),
    ] {
        let mut e = engine();
        let operation = uuid::Uuid::new_v4();
        let (_, mut topic) = crate::fixture::topic_workspace();
        topic.status = formless_protocol::TopicStatus::Approved;
        e.command(command(action.clone(), operation));
        e.command(command(action.clone(), operation));
        assert_eq!(
            e.outgoing.len(),
            1,
            "double-click must not duplicate consent"
        );
        let first = e.outgoing.pop_front().unwrap();
        e.receive(ServerMessage::ok(
            first.id,
            serde_json::to_value(&topic).unwrap(),
        ))
        .unwrap();
        if matches!(action, TopicAction::MoveAndStart(_)) {
            let start = e.outgoing.pop_front().unwrap();
            assert!(
                matches!(start.request, Request::StartTopicWorkflow {topic_id} if topic_id==topic.id)
            );
            assert!(
                !e.updates.iter().any(|u| matches!(
                    u,
                    Update::Topics {
                        operation: Some(_),
                        ..
                    }
                )),
                "no intermediate success before start receipt"
            );
            e.receive(ServerMessage::ok(
                start.id,
                serde_json::to_value(&topic).unwrap(),
            ))
            .unwrap();
        }
        assert!(e.outgoing.is_empty());
        assert!(e.updates.iter().any(|u| matches!(u, Update::Topics{operation:Some(op),result:Ok(Reply::Topic(_)),..} if *op==operation)));
    }
}
#[test]
fn stale_inventory_cannot_replace_a_mutation_and_lost_connections_never_replay_start() {
    let mut e = engine();
    e.topic_command(None, TopicAction::List);
    let inventory = e.outgoing.pop_front().unwrap();
    e.command(command(
        TopicAction::MoveAndStart("topic-formless".into()),
        uuid::Uuid::new_v4(),
    ));
    let approve = e.outgoing.pop_front().unwrap();
    e.receive(ServerMessage::ok(inventory.id, serde_json::json!([])))
        .unwrap();
    assert!(
        e.updates.is_empty(),
        "retired inventory cannot erase a topic"
    );
    let new = engine();
    assert!(
        new.outgoing.is_empty(),
        "new connection never reconstructs a start continuation"
    );
    assert!(matches!(
        e.pending.get(&approve.id),
        Some(Pending::Topic {
            action: TopicAction::MoveAndStart(_),
            ..
        })
    ));
}
#[test]
fn failed_approval_and_unsupported_daemons_never_start_a_workflow() {
    let mut e = engine();
    e.topics_available = false;
    e.command(command(
        TopicAction::MoveAndStart("t".into()),
        uuid::Uuid::new_v4(),
    ));
    assert!(e.outgoing.is_empty());
    e.topics_available = true;
    e.command(command(
        TopicAction::MoveAndStart("t".into()),
        uuid::Uuid::new_v4(),
    ));
    let r = e.outgoing.pop_front().unwrap();
    // A malformed/failed response is not an approval receipt.
    e.receive(ServerMessage::ok(
        r.id,
        serde_json::json!({"approved":true}),
    ))
    .unwrap();
    assert!(e.outgoing.is_empty());
    assert!(e
        .updates
        .iter()
        .any(|u| matches!(u, Update::Topics { result: Err(_), .. })));
}
