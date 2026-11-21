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

#[test]
fn harness_permission_answers_are_bound_to_exact_prompt_and_send_once_without_chat_or_workflow_fallback(
) {
    for option in [None, Some("allow-once-id"), Some("allow-attempt-id")] {
        let mut e = engine();
        let event = crate::fixture::harness_permission("codex-chat");
        let open = questions::Open::from_event(&event).unwrap();
        e.receive(ServerMessage::Event(event)).unwrap();
        let answers = vec![formless_protocol::QuestionAnswer {
            selected: option.map(|id| vec![id.into()]).unwrap_or_default(),
            text: None,
        }];
        for _ in 0..2 {
            e.command(Command::AnswerQuestion {
                connection: 2,
                operation: uuid::Uuid::new_v4(),
                question: Box::new(open.clone()),
                answers: answers.clone(),
            });
        }
        let [request] = e.outgoing.make_contiguous() else {
            panic!("one explicit decision")
        };
        assert!(
            matches!(&request.request, Request::HarnessPermissionDecision { permission_id, option_id } if permission_id == "native-permission" && option_id.as_deref() == option)
        );
        let request_id = request.id;
        e.receive(ServerMessage::ok(
            request_id,
            serde_json::json!({"answered":true}),
        ))
        .unwrap();
        assert!(e.questions.get("native-permission").is_none());
    }
}

#[test]
fn closed_or_changed_harness_permission_never_reaches_the_wire() {
    let mut e = engine();
    let event = crate::fixture::harness_permission("codex-chat");
    let open = questions::Open::from_event(&event).unwrap();
    e.receive(ServerMessage::Event(event)).unwrap();
    e.receive(ServerMessage::Event(Event::HarnessPermissionClosed {
        permission_id: open.id.clone(),
        option_id: None,
        by: None,
    }))
    .unwrap();
    e.command(Command::AnswerQuestion {
        connection: 2,
        operation: uuid::Uuid::new_v4(),
        question: Box::new(open),
        answers: vec![formless_protocol::QuestionAnswer {
            selected: vec!["allow-once-id".into()],
            text: None,
        }],
    });
    assert!(e.outgoing.is_empty());
}

#[test]
fn reconnect_inventory_restores_permissions_without_hiding_security_approvals_or_granting_anything()
{
    let mut e = engine();
    e.refresh();
    let pending = e
        .outgoing
        .iter()
        .find(|request| matches!(request.request, Request::PendingApprovals))
        .unwrap()
        .id;
    let mut value = crate::approvals::fixture_inventory();
    value
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(crate::fixture::harness_permission("codex-chat")).unwrap());
    e.receive(ServerMessage::ok(pending, value.clone()))
        .unwrap();
    assert!(e.questions.get("native-permission").is_some());
    assert!(e.outgoing.iter().all(|request| !matches!(
        request.request,
        Request::HarnessPermissionDecision { .. }
            | Request::StartTopicWorkflow { .. }
            | Request::AnswerQuestion { .. }
    )));
    let operation = uuid::Uuid::new_v4();
    e.command(Command::ReadApprovals {
        connection: 2,
        operation,
    });
    let read = e.outgoing.back().unwrap();
    assert!(matches!(read.request, Request::PendingApprovals));
    e.receive(ServerMessage::ok(read.id, value)).unwrap();
    assert!(e.updates.iter().any(|update| matches!(update, Update::ApprovalInventory { result:Ok(inventory), .. } if inventory.prompts().len() == 1 && inventory.prompts()[0].id() == "approval-fixture")));
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

#[test]
fn resetting_an_item_and_cancelling_never_approve_or_start_a_workflow() {
    for action in [
        TopicAction::Retry {
            topic: "topic-formless".into(),
            feature: "feature-y".into(),
        },
        TopicAction::Cancel("topic-formless".into()),
    ] {
        let mut e = engine();
        let operation = uuid::Uuid::new_v4();
        e.command(command(action.clone(), operation));
        e.command(command(action.clone(), operation));
        assert_eq!(e.outgoing.len(), 1);
        let request = e.outgoing.pop_front().unwrap();
        assert!(matches!(
            request.request,
            Request::RetryTopicFeature { .. } | Request::CancelTopic { .. }
        ));
        let (_, mut topic) = crate::fixture::topic_workspace();
        topic.status = if matches!(action, TopicAction::Cancel(_)) {
            formless_protocol::TopicStatus::Cancelled
        } else {
            formless_protocol::TopicStatus::Approved
        };
        e.receive(ServerMessage::ok(
            request.id,
            serde_json::to_value(topic).unwrap(),
        ))
        .unwrap();
        assert!(
            e.outgoing.is_empty(),
            "neither reset nor cancel implies consent to start"
        );
    }
}

#[test]
fn failed_start_preserves_the_move_receipt_and_recovery_never_approves_again() {
    let mut e = engine();
    let (_, mut topic) = crate::fixture::topic_workspace();
    topic.status = formless_protocol::TopicStatus::Approved;
    let operation = uuid::Uuid::new_v4();
    e.command(command(
        TopicAction::MoveAndStart(topic.id.clone()),
        operation,
    ));
    let approve = e.outgoing.pop_front().unwrap();
    assert!(matches!(approve.request, Request::ApproveTopic { .. }));
    e.receive(ServerMessage::ok(
        approve.id,
        serde_json::to_value(&topic).unwrap(),
    ))
    .unwrap();
    assert!(e.updates.iter().any(|u| matches!(u,
        Update::Topics { operation: None, result: Ok(Reply::Topic(t)), .. }
        if t.topic_session_id == topic.topic_session_id && t.status == formless_protocol::TopicStatus::Approved
    )), "the committed move is visible even if launching later fails");
    let start = e.outgoing.pop_front().unwrap();
    assert!(matches!(start.request, Request::StartTopicWorkflow { .. }));
    e.receive(ServerMessage::err(
        start.id,
        "no child was launched; recover before retrying".into(),
    ))
    .unwrap();
    assert!(e.updates.iter().any(|u| matches!(u,
        Update::Topics { operation: Some(op), result: Err(error), .. }
        if *op == operation && error.starts_with("Workflow start failed.") && error.contains("no child was launched")
    )));
    assert!(
        e.outgoing.is_empty(),
        "start failures never replay a mutation"
    );
    topic.status = formless_protocol::TopicStatus::Failed;
    topic.features[0].status = formless_protocol::TopicFeatureStatus::Failed;
    e.command(command(
        TopicAction::Recover(topic.id.clone()),
        uuid::Uuid::new_v4(),
    ));
    let recover = e.outgoing.pop_front().unwrap();
    assert!(matches!(recover.request, Request::RecoverTopic { .. }));
    e.receive(ServerMessage::ok(
        recover.id,
        serde_json::to_value(&topic).unwrap(),
    ))
    .unwrap();
    assert!(
        e.outgoing.is_empty(),
        "recovery does not automatically start or move"
    );
    e.command(command(
        TopicAction::Retry {
            topic: topic.id.clone(),
            feature: topic.features[0].id.clone(),
        },
        uuid::Uuid::new_v4(),
    ));
    let retry = e.outgoing.pop_front().unwrap();
    assert!(matches!(retry.request, Request::RetryTopicFeature { .. }));
    topic.status = formless_protocol::TopicStatus::Approved;
    topic.features[0].status = formless_protocol::TopicFeatureStatus::Pending;
    e.receive(ServerMessage::ok(
        retry.id,
        serde_json::to_value(&topic).unwrap(),
    ))
    .unwrap();
    assert!(
        e.outgoing.is_empty(),
        "resetting an item never starts or moves"
    );
    e.command(command(TopicAction::Start(topic.id), uuid::Uuid::new_v4()));
    assert!(matches!(
        e.outgoing.pop_front().unwrap().request,
        Request::StartTopicWorkflow { .. }
    ));
    assert!(e.outgoing.is_empty());
}
