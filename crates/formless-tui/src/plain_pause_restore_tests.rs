//! Persisted pause restoration uses the real plain snapshot handler.
use super::*;
use serde_json::json;

fn snapshot(run: &str, reason: &str) -> serde_json::Value {
    json!({"latest_run":{"run_id":run,"status":"paused"},"unfinished_run":{"run_id":run,"status":"paused","error":reason},"running":false,"plan":[],"last_recap":null})
}

#[test]
fn paused_reason_restores_once_and_preserves_resume_guidance() {
    let value = snapshot(
        "r",
        "stuck: repeated daemon correction; revise arguments or switch model then Resume",
    );
    let mut state = RunState::default();
    let lines = state.snapshot(0, &value, false);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("Run paused: stuck: repeated daemon correction"));
    assert!(lines[0].contains("revise arguments or switch model then Resume"));
    assert!(lines[0].contains("Resume this Session in the TUI or web"));
    assert!(state.snapshot(0, &value, false).is_empty());
}

#[test]
fn newer_run_and_live_pause_prevent_duplicate_or_stale_snapshot_reason() {
    let old = snapshot("old", "stuck: old failure");
    let mut state = RunState::default();
    assert!(state.accept(&Event::RunStarted {
        session_id: "s".into(),
        run_id: "new".into()
    }));
    assert!(state.snapshot(0, &old, false).is_empty());
    assert!(state.unfinished.is_none());
    let pause = Event::RunPaused {
        session_id: "s".into(),
        run_id: "new".into(),
        error: "stuck: new failure".into(),
        explained: None,
    };
    assert!(state.accept(&pause));
    assert!(state
        .snapshot(
            state.generation,
            &snapshot("new", "stuck: new failure"),
            false
        )
        .is_empty());
    assert!(!state.accept(&Event::RunPaused {
        session_id: "s".into(),
        run_id: "old".into(),
        error: "old".into(),
        explained: None
    }));
}

#[test]
fn running_or_explicit_recap_snapshot_does_not_repeat_pause_notice() {
    let mut value = snapshot("r", "stuck: repeated");
    value["running"] = json!(true);
    let mut state = RunState::default();
    assert!(state.snapshot(0, &value, false).is_empty());
    assert!(state.unfinished.is_none());
    let mut state = RunState::default();
    let recap = state.snapshot(0, &snapshot("r", "stuck: repeated"), true);
    assert_eq!(recap, vec!["No recorded recap for the latest Run."]);
    assert!(state.unfinished.is_none());
    assert_eq!(
        state
            .snapshot(0, &snapshot("r", "stuck: repeated"), false)
            .len(),
        1
    );
}

#[test]
fn paused_reason_is_bounded_sanitized_and_missing_reason_is_not_invented() {
    let raw = format!("stuck:\u{1b}[31m\u{202e}\n{}", "x".repeat(5000));
    let text = unfinished_run_text(&json!({"run_id":"r","status":"paused","error":raw})).unwrap();
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains('\u{202e}'));
    assert!(text.contains("[reason truncated]"));
    assert!(text.len() < 2500);
    let empty = unfinished_run_text(&json!({"status":"interrupted"})).unwrap();
    assert!(empty.starts_with("Run interrupted\n"));
    assert!(!empty.contains("stuck"));
    assert!(unfinished_run_text(&json!({"status":"completed","error":"old failure"})).is_none());
}
