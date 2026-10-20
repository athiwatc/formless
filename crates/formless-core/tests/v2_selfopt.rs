//! v2 prompt/memory self-optimisation (SPEC §7.7/§17): versioned,
//! measured proposals and their lifecycle gates.

use formless_core::optimiser::{prompt_eval_cases, proposal_beats_baseline, score_eval_responses};
use formless_core::{Paths, Store};

fn temp_store(tag: &str) -> Store {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    Store::open(&paths.db_file()).unwrap()
}

#[test]
fn proposal_lifecycle_is_measured_and_versioned() {
    let store = temp_store("selfopt");
    let p = store
        .create_proposal(
            "prompt",
            "Always stream large files in chunks.",
            "OOM failures",
            12,
        )
        .unwrap();
    assert_eq!(p.status, "proposed");
    // Cannot apply before measurement.
    assert!(store
        .set_proposal_status(&p.id, "applied")
        .unwrap()
        .is_none());
    let measured = store
        .update_proposal_eval(&p.id, 0.5, 0.83, 6)
        .unwrap()
        .unwrap();
    assert_eq!(measured.status, "measured");
    assert_eq!(measured.candidate_score, Some(0.83));

    let applied = store
        .set_proposal_status(&p.id, "applied")
        .unwrap()
        .unwrap();
    assert!(applied.applied_at.is_some());
    let prompt = store.effective_system_prompt("BASE").unwrap();
    assert!(prompt.starts_with("BASE"), "{prompt}");
    assert!(
        prompt.contains("Learned guidance (v1): Always stream"),
        "{prompt}"
    );

    // A memory proposal applies after it, preserving order.
    let m = store
        .create_proposal("memory", "User prefers concise answers.", "feedback", 3)
        .unwrap();
    store.update_proposal_eval(&m.id, 0.83, 1.0, 6).unwrap();
    store.set_proposal_status(&m.id, "applied").unwrap();
    let prompt2 = store.effective_system_prompt("BASE").unwrap();
    assert!(prompt2.contains("Learned memory (v2)"), "{prompt2}");

    // Rollback removes it from the effective prompt; reject measured ones.
    store.set_proposal_status(&p.id, "rolled_back").unwrap();
    let prompt3 = store.effective_system_prompt("BASE").unwrap();
    assert!(!prompt3.contains("Always stream"), "{prompt3}");
    assert!(store
        .set_proposal_status(&p.id, "rolled_back")
        .unwrap()
        .is_none());
}

#[test]
fn eval_scoring_matches_expected_phrases() {
    let cases = prompt_eval_cases();
    assert!(!cases.is_empty());
    let perfect: Vec<String> = cases
        .iter()
        .map(|c| format!("I would {}", c.expect_any[0]))
        .collect();
    assert_eq!(score_eval_responses(&cases, &perfect), 1.0);
    let empty: Vec<String> = cases.iter().map(|_| "dunno".to_string()).collect();
    assert_eq!(score_eval_responses(&cases, &empty), 0.0);
    let mut half = perfect.clone();
    for r in half.iter_mut().skip(3) {
        *r = "dunno".into();
    }
    assert_eq!(
        score_eval_responses(&cases, &half),
        3.0 / cases.len() as f64
    );
}

#[test]
fn proposal_apply_requires_margin_win() {
    // Exactly the margin passes despite float rounding (0.6 - 0.5 < 0.1).
    assert!(proposal_beats_baseline(0.5, 0.6, 10));
    assert!(proposal_beats_baseline(0.5, 0.83, 10));
    assert!(!proposal_beats_baseline(0.5, 0.59, 10));
    assert!(!proposal_beats_baseline(0.8, 0.7, 10));
    // A perfect candidate may apply over a perfect (or near) baseline.
    assert!(proposal_beats_baseline(1.0, 1.0, 10));
    assert!(proposal_beats_baseline(0.95, 1.0, 10));
    // The margin is configurable.
    assert!(proposal_beats_baseline(0.5, 0.55, 5));
    assert!(!proposal_beats_baseline(0.5, 0.55, 20));
}
