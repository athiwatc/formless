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

#[test]
fn builtin_outcomes_and_stats() {
    use formless_core::daemon::builtin_outcome;
    assert_eq!(
        builtin_outcome(
            "create_tool",
            "error: create_tool failed: syntax check failed: x"
        ),
        (false, "syntax")
    );
    assert_eq!(
        builtin_outcome(
            "create_tool",
            "error: create_tool failed: smoke test failed: y"
        ),
        (false, "smoke")
    );
    assert_eq!(
        builtin_outcome("find_tools", r#"{"tools":[],"note":"No matching Tool"}"#),
        (true, "empty")
    );
    assert_eq!(
        builtin_outcome("find_tools", r#"{"tools":[{"name":"a"}]}"#),
        (true, "")
    );
    assert_eq!(
        builtin_outcome("test_tool", r#"{"ok":false,"stage":"run"}"#),
        (false, "test-failed")
    );

    let store = Store::open_in_memory().unwrap();
    store
        .record_builtin_call("find_tools", "s", "r1", true, "empty", 4)
        .unwrap();
    store
        .record_builtin_call("find_tools", "s", "r1", true, "", 6)
        .unwrap();
    store
        .record_builtin_call("create_tool", "s", "r1", false, "syntax", 10)
        .unwrap();
    store
        .record_builtin_call("create_tool", "s", "r1", true, "near-duplicate", 30)
        .unwrap();
    assert!(store.run_had_empty_find("r1").unwrap());
    assert!(!store.run_had_empty_find("r2").unwrap());
    let stats = store.builtin_stats().unwrap();
    let create = stats.iter().find(|s| s.name == "create_tool").unwrap();
    assert_eq!((create.calls, create.failures), (2, 1));
    assert_eq!(create.kinds.get("near-duplicate"), Some(&1));
    assert!((create.avg_ms - 20.0).abs() < 1e-9);
    let find = stats.iter().find(|s| s.name == "find_tools").unwrap();
    assert_eq!(
        (find.calls, find.failures, find.kinds.get("empty")),
        (2, 0, Some(&1))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn built_in_calls_are_recorded_without_payloads() {
    use std::sync::Arc;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-bic-{short}"));
    let paths = formless_core::Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-bic-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    daemon
        .call_tool_direct(
            &s,
            "find_tools",
            serde_json::json!({"query": "zebra telescope"}),
        )
        .await;
    let stats = daemon
        .handle(formless_protocol::Request::ListBuiltinStats)
        .await
        .unwrap();
    assert_eq!(stats[0]["name"], "find_tools");
    assert_eq!(stats[0]["kinds"]["empty"], 1);
}

#[test]
fn cost_per_success_counts_attributed_tokens_and_time() {
    let store = Store::open_in_memory().unwrap();
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let (t, v) = store
        .create_tool_version(
            "echo",
            "d",
            "{}",
            "{}",
            "function run(i){return i;}",
            &caps,
            64,
            "agent",
        )
        .unwrap();
    let s = store.create_session(None).unwrap().id;
    let run = store.create_run(&s).unwrap();
    let since = "2000-01-01T00:00:00+00:00";
    let mut ids = Vec::new();
    for (status, ms) in [("success", 100), ("failure", 300), ("success", 200)] {
        ids.push(
            store
                .record_invocation(
                    &t.id,
                    v,
                    Some(&s),
                    Some(&run),
                    None,
                    status,
                    "",
                    "{}",
                    "",
                    "",
                    ms,
                    100,
                )
                .unwrap(),
        );
    }
    // A step of 900 tokens that made 3 calls; one Tool also spent 60 itself.
    store.attribute_step_tokens(&run, since, 900, 3).unwrap();
    store.add_invocation_tokens(&ids[0], 60).unwrap();
    let m = store.tool_metrics(&t.id, None).unwrap();
    assert_eq!(m.success, 2);
    assert_eq!(m.ms_per_success, Some(300.0), "600ms over 2 successes");
    assert_eq!(
        m.tokens_per_success,
        Some(480.0),
        "960 tokens over 2 successes"
    );
}
