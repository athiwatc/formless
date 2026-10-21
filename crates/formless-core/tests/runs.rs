//! Run-level outcomes (SPEC §8.7).

use formless_core::runs::{is_correction, summarize};
use formless_core::Store;

#[test]
fn corrections_are_recognised_narrowly() {
    for t in [
        "That's wrong, use the other file",
        "no, that is the old one",
        "It didn't work",
        "Not what I asked",
    ] {
        assert!(is_correction(t), "{t}");
    }
    for t in [
        "Thanks! Now do the same for March",
        "Is that wrong?",
        "Explain why the wrong value appears",
    ] {
        assert!(!is_correction(t), "{t}");
    }
}

#[test]
fn run_metrics_record_steps_tokens_and_followups() {
    let store = Store::open_in_memory().unwrap();
    let s = store.create_session(None).unwrap().id;
    let r1 = store.create_run(&s).unwrap();
    store.bump_run_steps(&r1).unwrap();
    store.bump_run_steps(&r1).unwrap();
    store
        .record_usage(
            &s,
            &r1,
            "m",
            &formless_core::provider::Usage {
                prompt_tokens: 100,
                completion_tokens: 20,
                ..Default::default()
            },
        )
        .unwrap();
    store.set_run_status(&r1, "completed").unwrap();
    // The user corrects it: the Run missed its goal.
    store.mark_last_run_followup(&s, "corrected").unwrap();
    store.mark_last_run_followup(&s, "edited").unwrap();
    let r2 = store.create_run(&s).unwrap();
    store.set_run_status(&r2, "completed").unwrap();
    let r3 = store.create_run(&s).unwrap();
    store.set_run_status(&r3, "interrupted").unwrap();
    let _running = store.create_run(&s).unwrap();

    let runs = store.run_metrics(10).unwrap();
    let first = runs.iter().find(|r| r.run_id == r1).unwrap();
    assert_eq!(
        (
            first.steps,
            first.prompt_tokens,
            first.completion_tokens,
            first.outcome.as_str(),
            first.status.as_str()
        ),
        (2, 100, 20, "corrected", "completed"),
        "the first follow-up wins"
    );
    let sum = summarize(&runs);
    assert_eq!(sum.runs, 3, "running Runs are left out");
    assert!((sum.completed - 1.0 / 3.0).abs() < 1e-9);
    assert!((sum.corrected - 1.0 / 3.0).abs() < 1e-9);
    assert!((sum.interrupted - 1.0 / 3.0).abs() < 1e-9);
    assert!((sum.avg_tokens - 40.0).abs() < 1e-9);
}
