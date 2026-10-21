//! v2 prompt/memory self-optimisation (SPEC §7.7/§17): versioned,
//! measured proposals and their lifecycle gates.

use formless_core::optimiser::{
    proposal_beats_baseline, scenario_cases, significant_improvement, wilson_interval,
};
use formless_core::provider::ToolCall;
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
    // Memory proposals become memories (author optimiser), not prompt text.
    let prompt2 = store.effective_system_prompt("BASE").unwrap();
    assert!(!prompt2.contains("concise"), "{prompt2}");
    let mems = store.list_memories().unwrap();
    assert_eq!(mems.len(), 1);
    assert_eq!(
        (mems[0].text.as_str(), mems[0].author.as_str()),
        ("User prefers concise answers.", "optimiser")
    );
    store.set_proposal_status(&m.id, "rolled_back").unwrap();
    assert!(
        store.list_memories().unwrap().is_empty(),
        "rollback removes it"
    );

    // Rollback removes it from the effective prompt; reject measured ones.
    store.set_proposal_status(&p.id, "rolled_back").unwrap();
    let prompt3 = store.effective_system_prompt("BASE").unwrap();
    assert!(!prompt3.contains("Always stream"), "{prompt3}");
    assert!(store
        .set_proposal_status(&p.id, "rolled_back")
        .unwrap()
        .is_none());
}

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: "c".into(),
        name: name.into(),
        arguments: args.to_string(),
    }
}

fn create(js: &str, caps: &[&str]) -> ToolCall {
    call(
        "create_tool",
        serde_json::json!({"name": "count_lines", "description": "d",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}},
            "output_schema": {}, "implementation_js": js, "capabilities": caps}),
    )
}

#[test]
fn scenarios_judge_actions_not_words() {
    let cases = scenario_cases();
    let case = |n: &str| cases.iter().find(|c| c.name == n).unwrap().check;
    let find = call("find_tools", serde_json::json!({"query": "csv"}));
    assert!(case("find_before_create")(std::slice::from_ref(&find)));
    assert!(!case("find_before_create")(&[create("x", &[])]));
    assert!(case("least_privilege")(&[create(
        "x",
        &["fs:read:$input.path"]
    )]));
    assert!(!case("least_privilege")(&[create("x", &["fs:read:/"])]));
    assert!(!case("least_privilege")(&[create(
        "x",
        &["fs:read:$input.path", "net"]
    )]));
    assert!(case("stream_large_data")(&[create(
        "for await (const c of fs.readChunks(i.path)) n++;",
        &[]
    )]));
    assert!(!case("stream_large_data")(&[create(
        "fs.read(i.path).split('\\n')",
        &[]
    )]));
    assert!(case("throw_on_failure")(&[create(
        "if (!r.ok) throw new Error('bad');",
        &[]
    )]));
    assert!(!case("throw_on_failure")(&[create(
        "if (!r.ok) return { error: 'bad' }; throw x",
        &[]
    )]));
    assert!(case("generic_tools")(&[create("fs.read(i.path)", &[])]));
    assert!(!case("generic_tools")(&[create(
        "fs.read('/home/a/proj/notes.txt')",
        &[]
    )]));
    assert!(case("reuse_tools")(&[call(
        "csv_column_sum",
        serde_json::json!({})
    )]));
    assert!(!case("reuse_tools")(&[create("x", &[])]));
    assert!(case("no_secrets_in_code")(&[create(
        "x",
        &["net", "env:WEBHOOK_KEY"]
    )]));
    // Saying the right words without acting fails everything.
    assert!(cases.iter().all(|c| !(c.check)(&[])));
}

#[test]
fn eval_statistics() {
    let (lo, hi) = wilson_interval(15, 21);
    assert!(
        lo > 0.49 && lo < 0.52 && hi > 0.85 && hi < 0.88,
        "{lo} {hi}"
    );
    assert_eq!(wilson_interval(0, 0), (0.0, 1.0));
    // 21 runs each: 50% -> 90% is significant, 50% -> 62% is not.
    assert!(significant_improvement(0.5, 0.9, 21));
    assert!(!significant_improvement(0.5, 0.62, 21));
    assert!(!significant_improvement(0.8, 0.7, 21));
    assert!(
        significant_improvement(0.5, 0.62, 400),
        "more trials, smaller effects"
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

#[test]
fn applied_memory_proposals_migrate_into_the_memory_store() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let db = std::path::PathBuf::from(format!("/tmp/fl-memmig-{short}/f.db"));
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    drop(Store::open(&db).unwrap());
    {
        // As an older build left it: an applied memory proposal, schema v17.
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch(
            "INSERT INTO prompt_proposals(id,kind,content,rationale,status,est_tokens,created_at,applied_at)
               VALUES('p1','memory','Prefers tabs.','x','applied',1,'2026-01-01','2026-01-02');
             PRAGMA user_version=17;",
        )
        .unwrap();
    }
    let store = Store::open(&db).unwrap();
    let mems = store.list_memories().unwrap();
    assert_eq!(mems.len(), 1);
    assert_eq!(
        (mems[0].text.as_str(), mems[0].author.as_str()),
        ("Prefers tabs.", "optimiser")
    );
    // Rollback still removes it.
    store.set_proposal_status("p1", "rolled_back").unwrap();
    assert!(store.list_memories().unwrap().is_empty());
}
