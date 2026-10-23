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
    // One Session's Runs only (for the TUI's answer footers).
    let other = store.create_session(None).unwrap().id;
    store.create_run(&other).unwrap();
    assert_eq!(store.run_metrics_for(Some(&s), 10).unwrap().len(), 4);
    assert_eq!(store.run_metrics_for(Some(&other), 10).unwrap().len(), 1);
    // The Sessions list's summary: count, last user message, last Run.
    store.append_message(&s, "user", "first ask").unwrap();
    store
        .append_message(&s, "user", "\n  latest ask, which goes on\nand on")
        .unwrap();
    let listed = store.list_sessions().unwrap();
    let row = listed.iter().find(|x| x.id == s).unwrap();
    assert_eq!(row.preview.as_deref(), Some("latest ask, which goes on"));
    assert!(row.message_count >= 2);
    assert_eq!(row.last_run_status.as_deref(), Some("running"));
    // Pinned Sessions list first, even when older.
    let pinned = store.set_session_pinned(&other, true).unwrap().unwrap();
    assert!(pinned.pinned);
    let newest = store.create_session(None).unwrap().id;
    let order: Vec<String> = store
        .list_sessions()
        .unwrap()
        .into_iter()
        .map(|x| x.id)
        .collect();
    assert_eq!(order[0], other);
    assert_eq!(order[1], newest);
    // Archived Sessions list last (clients hide them by default) and
    // stay searchable.
    let a = store.set_session_archived(&s, true).unwrap().unwrap();
    assert!(a.archived);
    let listed = store.list_sessions().unwrap();
    assert_eq!(listed.last().unwrap().id, s);
    assert!(!store.search_sessions("latest ask", 10).unwrap().is_empty());
    // Tags are normalised: no `#`, lowercase, unique, sorted.
    let t = store
        .set_session_tags(
            &other,
            &["#Work".into(), "client-a".into(), "work".into(), " ".into()],
        )
        .unwrap()
        .unwrap();
    assert_eq!(t.tags, ["client-a", "work"]);
    let listed = store.list_sessions().unwrap();
    assert_eq!(
        listed.iter().find(|x| x.id == other).unwrap().tags,
        ["client-a", "work"]
    );
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

#[tokio::test(flavor = "multi_thread")]
async fn session_usage_totals_and_priced_cost() {
    use formless_core::config::ModelPrice;
    use formless_protocol::Request;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-usage-{short}"));
    let paths = formless_core::Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-usage-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = formless_core::Config::default();
    cfg.provider.prices.insert(
        "m".into(),
        ModelPrice {
            input: 2.0,
            output: 8.0,
            cached_input: Some(0.5),
        },
    );
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap().id;
    let usage = |p, c, cached| formless_core::provider::Usage {
        prompt_tokens: p,
        completion_tokens: c,
        cached_tokens: cached,
        ..Default::default()
    };
    let r1 = store.create_run(&s).unwrap();
    store.bump_run_steps(&r1).unwrap();
    store
        .record_usage(&s, &r1, "m", &usage(1_000_000, 0, 0))
        .unwrap();
    store.set_run_status(&r1, "completed").unwrap();
    let r2 = store.create_run(&s).unwrap();
    store.bump_run_steps(&r2).unwrap();
    store.bump_run_steps(&r2).unwrap();
    store
        .record_usage(&s, &r2, "m", &usage(1_000_000, 500_000, 600_000))
        .unwrap();
    let daemon = std::sync::Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let r = daemon
        .handle(Request::SessionUsage {
            session_id: s.clone(),
        })
        .await
        .unwrap();
    assert_eq!(r["runs"], 2);
    assert_eq!(r["last_run"]["steps"], 2);
    // r2: 0.4M fresh × $2 + 0.6M cached × $0.5 + 0.5M out × $8 = 0.8 + 0.3 + 4.0
    assert!(
        (r["last_run"]["cost"].as_f64().unwrap() - 5.1).abs() < 1e-9,
        "{r}"
    );
    assert_eq!(r["total"]["prompt_tokens"], 2_000_000);
    assert!((r["total"]["cost"].as_f64().unwrap() - 7.1).abs() < 1e-9);
    // A model without a price: no cost guess.
    store
        .record_usage(&s, &r2, "unpriced", &usage(10, 10, 0))
        .unwrap();
    let r = daemon
        .handle(Request::SessionUsage { session_id: s })
        .await
        .unwrap();
    assert!(r["total"]["cost"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn running_invocations_are_listed_abandoned_and_cancelled() {
    use formless_protocol::{InvocationInfo, Request};
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-inv-{short}"));
    let paths = formless_core::Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-inv-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = std::sync::Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let out = daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "slowpoke", "description": "waits", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": "async function run(i){ await sleep(i.ms || 0); return {}; }",
            "capabilities": []}))
        .await;
    assert!(out.contains("\"created\""), "{out}");
    let call = tokio::spawn({
        let (d, s) = (daemon.clone(), sid.clone());
        async move {
            d.call_tool_direct(&s, "slowpoke", serde_json::json!({"ms": 20000}))
                .await
        }
    });
    let list = loop {
        let v = daemon.handle(Request::ListInvocations).await.unwrap();
        let list: Vec<InvocationInfo> = serde_json::from_value(v).unwrap();
        if !list.is_empty() {
            break list;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(
        (list[0].tool_name.as_str(), list[0].session_id.as_str()),
        ("slowpoke", sid.as_str())
    );
    assert!(!list[0].stalled && !list[0].abandoned);
    let handle = list[0].handle.clone();
    daemon
        .handle(Request::AbandonInvocation {
            handle: handle.clone(),
        })
        .await
        .unwrap();
    let v = daemon.handle(Request::ListInvocations).await.unwrap();
    assert_eq!(v[0]["abandoned"], true);
    daemon
        .handle(Request::CancelInvocation { handle })
        .await
        .unwrap();
    let r = tokio::time::timeout(std::time::Duration::from_secs(10), call)
        .await
        .unwrap()
        .unwrap();
    assert!(
        r.contains("cancel") || r.contains("interrupt") || r.contains("abandon"),
        "{r}"
    );
    let v = daemon.handle(Request::ListInvocations).await.unwrap();
    assert_eq!(v, serde_json::json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn unfinished_last_run_comes_with_the_session() {
    use formless_protocol::Request;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-unf-{short}"));
    let paths = formless_core::Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-unf-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let s = store.create_session(None).unwrap().id;
    let daemon = std::sync::Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let get = || {
        let (d, s) = (daemon.clone(), s.clone());
        async move { d.handle(Request::GetSession { id: s }).await.unwrap() }
    };
    assert!(get().await["unfinished_run"].is_null(), "no Runs yet");
    let r = store.create_run(&s).unwrap();
    store.set_run_status(&r, "paused").unwrap();
    store.set_run_error(&r, "provider returned 503").unwrap();
    let u = get().await["unfinished_run"].clone();
    assert_eq!(
        (u["status"].as_str(), u["error"].as_str()),
        (Some("paused"), Some("provider returned 503"))
    );
    // A crash leaves the next Run "running"; recovery explains it.
    let r2 = store.create_run(&s).unwrap();
    store.mark_orphan_runs_interrupted().unwrap();
    let u = get().await["unfinished_run"].clone();
    assert_eq!(u["run_id"].as_str(), Some(r2.as_str()));
    assert!(u["error"].as_str().unwrap().contains("daemon stopped"));
    store.set_run_status(&r2, "completed").unwrap();
    assert!(get().await["unfinished_run"].is_null());
}

#[test]
fn tools_list_their_recent_outcomes_oldest_first() {
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
    // Denied and cancelled calls say nothing about the Tool itself.
    for status in [
        "success",
        "denied",
        "failure",
        "failure",
        "cancelled",
        "success",
    ] {
        store
            .record_invocation(
                &t.id, v, None, None, None, status, "", "{}", "", "", 1, 1000,
            )
            .unwrap();
    }
    let listed = store.list_tools(false).unwrap();
    assert_eq!(listed[0].recent, "sffs");
    // The detail lists every call, newest first.
    let d = store.tool_detail(&t.id).unwrap().unwrap();
    let statuses: Vec<&str> = d.recent_calls.iter().map(|c| c.status.as_str()).collect();
    assert_eq!(
        statuses,
        [
            "success",
            "cancelled",
            "failure",
            "failure",
            "denied",
            "success"
        ]
    );
}
