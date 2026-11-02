//! Control-group regression fixtures use the real target binding helper.
use super::*;
use serde_json::json;

#[test]
fn queued_acknowledgement_has_no_run_or_replayed_old_output() {
    let mut target = Target::default();
    target
        .buffer(Event::RunStarted {
            session_id: "s".into(),
            run_id: "old".into(),
        })
        .unwrap();
    target
        .buffer(Event::RunFinished {
            session_id: "s".into(),
            run_id: "old".into(),
            status: "completed".into(),
        })
        .unwrap();
    assert_eq!(
        target.acknowledge(&json!({"queued":true})).unwrap(),
        Acknowledged::Queued
    );
    assert!(target.run_id.is_none());
    assert!(target.next().is_none());
    assert_eq!(target.bytes, 0);
}

#[test]
fn steered_acknowledgement_uses_actual_consumer_and_filters_old_approval_context() {
    let mut target = Target::default();
    assert_eq!(
        target
            .acknowledge(&json!({"steered":true,"run_id":"consumer"}))
            .unwrap(),
        Acknowledged::Run
    );
    let approval = |run: Option<&str>| {
        serde_json::from_value::<Event>(json!({
        "type":"approval_requested", "approval_id":"a", "session_id":"s", "tool_name":"read", "version":1,
        "capabilities":[], "memory_mb":64, "context":{"run_id":run,"call":null}
    })).unwrap()
    };
    assert!(!target.accepts(&approval(Some("obsolete"))).unwrap());
    assert!(target.accepts(&approval(Some("consumer"))).unwrap());
    assert!(target.accepts(&approval(None)).unwrap()); // Honest legacy lack of identity.
    assert_eq!(target.run_id.as_deref(), Some("consumer"));
}

#[test]
fn missing_steered_identity_does_not_guess_a_future_or_observed_run() {
    let mut target = Target::default();
    target
        .buffer(Event::RunStarted {
            session_id: "s".into(),
            run_id: "unrelated".into(),
        })
        .unwrap();
    assert!(target
        .acknowledge(&json!({"steered":true,"run_id":null}))
        .is_err());
    assert!(target.run_id.is_none());
    assert!(target.waiting());
}
