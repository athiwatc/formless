//! Exercise the actual plain reply acknowledgement handler, including close
//! events racing the request response. No daemon, provider or credentials.
use super::*;
use serde_json::json;

#[test]
fn approval_and_question_success_requires_own_positive_ack_once() {
    let mut replies = ReplyAcks::default();
    replies.insert(10, PendingReply::Approval(true));
    replies.insert(11, PendingReply::Approval(false));
    replies.insert(12, PendingReply::Question);
    assert!(replies
        .response(99, true, Some(&json!({"resolved":true})), None)
        .is_none());
    assert_eq!(replies.0.len(), 3);
    assert_eq!(
        replies
            .response(10, true, Some(&json!({"resolved":true})), None)
            .as_deref(),
        Some("Approved.")
    );
    assert_eq!(
        replies
            .response(11, true, Some(&json!({"resolved":true})), None)
            .as_deref(),
        Some("Denied.")
    );
    assert_eq!(
        replies
            .response(12, true, Some(&json!({"answered":true})), None)
            .as_deref(),
        Some("Answered.")
    );
    assert!(replies
        .response(10, true, Some(&json!({"resolved":true})), None)
        .is_none());
    assert!(replies.0.is_empty());
}

#[test]
fn cancelled_stale_or_failed_replies_never_claim_application() {
    let mut replies = ReplyAcks::default();
    for (id, pending, response) in [
        (1, PendingReply::Approval(true), json!({"resolved":false})),
        (2, PendingReply::Question, json!({"answered":false})),
        (3, PendingReply::Promotion, json!({"resolved":false})),
        (4, PendingReply::Approval(false), json!({})),
    ] {
        replies.insert(id, pending);
        let notice = replies.response(id, true, Some(&response), None).unwrap();
        assert!(notice.contains("was not applied"), "{notice}");
        assert!(!["Approved.", "Denied.", "Answered."].contains(&notice.as_str()));
    }
    replies.insert(5, PendingReply::Question);
    let failed = replies
        .response(5, false, None, Some("cancelled\u{1b}[31m"))
        .unwrap();
    assert!(failed.starts_with("Answer was not applied:"));
    assert!(!failed.contains('\u{1b}'));
    assert!(replies.0.is_empty());
}

#[test]
fn close_events_before_ack_keep_the_next_prompt_and_do_not_discard_confirmation() {
    let mut prompts = VecDeque::from([
        Prompt::Approval {
            id: "submitted".into(),
            promotion: false,
        },
        Prompt::Approval {
            id: "next".into(),
            promotion: false,
        },
    ]);
    let mut replies = ReplyAcks::default();
    replies.insert(7, PendingReply::Approval(true));
    prompts.pop_front(); // Actual input handler submits and removes only its prompt.
    remove_resolved_prompt(
        &mut prompts,
        &Event::ApprovalAnswered {
            approval_id: "submitted".into(),
            approved: true,
            by: None,
        },
    );
    assert!(matches!(prompts.front(), Some(Prompt::Approval { id, .. }) if id == "next"));
    assert_eq!(
        replies
            .response(7, true, Some(&json!({"resolved":true})), None)
            .as_deref(),
        Some("Approved.")
    );
    assert!(matches!(prompts.front(), Some(Prompt::Approval { id, .. }) if id == "next"));
    replies.insert(8, PendingReply::Question);
    remove_resolved_prompt(
        &mut prompts,
        &Event::UserQuestionClosed {
            question_id: "stale".into(),
        },
    );
    assert!(replies
        .response(8, true, Some(&json!({"answered":false})), None)
        .unwrap()
        .contains("not applied"));
}

#[test]
fn reply_backlog_is_bounded_and_promotion_ack_is_only_decision_acceptance() {
    let mut replies = ReplyAcks::default();
    for id in 0..128 {
        assert!(replies.has_room());
        replies.insert(id, PendingReply::Question);
    }
    assert!(!replies.has_room());
    replies.response(0, true, Some(&json!({"answered":true})), None);
    assert!(replies.has_room());
    replies.insert(128, PendingReply::Promotion);
    assert_eq!(
        replies
            .response(128, true, Some(&json!({"resolved":true})), None)
            .as_deref(),
        Some("Promotion decision accepted.")
    );
}

#[test]
fn cancellation_close_retires_only_matching_approval_without_a_denial() {
    let mut prompts = VecDeque::from([
        Prompt::Approval {
            id: "cancelled".into(),
            promotion: false,
        },
        Prompt::Approval {
            id: "next".into(),
            promotion: false,
        },
        Prompt::Approval {
            id: "cancelled".into(),
            promotion: true,
        },
    ]);
    let event = Event::ApprovalClosed {
        approval_id: "cancelled".into(),
    };
    remove_resolved_prompt(&mut prompts, &event);
    assert_eq!(prompts.len(), 2);
    assert!(matches!(prompts.front(), Some(Prompt::Approval { id, .. }) if id == "next"));
    let text = format_event(&event, &mut false).unwrap();
    assert_eq!(text, "Approval prompt closed.");
    assert!(!text.contains("Denied"));
    assert!(!text.contains("Approved"));
    let mut replies = ReplyAcks::default();
    replies.insert(1, PendingReply::Approval(true));
    assert!(replies
        .response(1, true, Some(&json!({"resolved":false})), None)
        .unwrap()
        .contains("not applied"));
}
