//! The real scoped observer forwards closure only for approvals it observed.
use super::*;
use serde_json::json;

fn observer(json: bool) -> Observer {
    Observer {
        session: "own".into(),
        seen: HashSet::new(),
        approvals: HashSet::new(),
        json,
    }
}
fn approval(session: &str, id: &str) -> Event {
    serde_json::from_value(json!({"type":"approval_requested","approval_id":id,"session_id":session,"tool_name":"read","version":1,"capabilities":[],"memory_mb":64})).unwrap()
}

#[test]
fn scoped_jsonl_observer_forwards_known_close_once_and_ignores_unknown_ids() {
    let mut observer = observer(true);
    let mut output = Vec::new();
    observer
        .emit_to(approval("own", "known"), &mut output)
        .unwrap();
    observer
        .emit_to(approval("foreign", "foreign"), &mut output)
        .unwrap();
    output.clear();
    observer
        .emit_to(
            Event::ApprovalClosed {
                approval_id: "unknown".into(),
            },
            &mut output,
        )
        .unwrap();
    observer
        .emit_to(
            Event::ApprovalClosed {
                approval_id: "foreign".into(),
            },
            &mut output,
        )
        .unwrap();
    assert!(output.is_empty());
    observer
        .emit_to(
            Event::ApprovalClosed {
                approval_id: "known".into(),
            },
            &mut output,
        )
        .unwrap();
    let lines = String::from_utf8(output.clone()).unwrap();
    assert_eq!(lines.lines().count(), 1);
    assert!(
        matches!(serde_json::from_str::<Event>(lines.trim()).unwrap(), Event::ApprovalClosed { approval_id } if approval_id == "known")
    );
    output.clear();
    observer
        .emit_to(
            Event::ApprovalClosed {
                approval_id: "known".into(),
            },
            &mut output,
        )
        .unwrap();
    observer
        .emit_to(
            Event::ApprovalAnswered {
                approval_id: "known".into(),
                approved: false,
                by: None,
            },
            &mut output,
        )
        .unwrap();
    assert!(output.is_empty());
    assert!(observer.approvals.is_empty());
}

#[test]
fn human_observer_close_does_not_claim_an_approval_decision() {
    let mut observer = observer(false);
    let mut output = Vec::new();
    observer
        .emit_to(approval("own", "known"), &mut output)
        .unwrap();
    output.clear();
    observer
        .emit_to(
            Event::ApprovalClosed {
                approval_id: "known".into(),
            },
            &mut output,
        )
        .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("approval_closed"));
    assert!(!text.contains("approval_answered"));
    assert!(!text.contains("approved"));
    assert!(!text.contains("denied"));
}
