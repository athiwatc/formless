//! v2 prompt/memory self-optimisation (SPEC §7.7/§17): versioned,
//! measured proposals and their lifecycle gates.

#[path = "support/env.rs"]
mod credential_env;
#[path = "support/historical.rs"]
mod historical;

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
    let _env = credential_env::EnvGuard::isolated();
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

#[test]
fn memory_proposal_refuses_disabled_duplicate_without_claiming_it() {
    let store = temp_store("selfopt-disabled-memory");
    let (note, _) = store
        .add_memory("User prefers concise answers.", "global", "user", None)
        .unwrap();
    store
        .update_memory(&note.id, None, None, Some(false))
        .unwrap();
    let proposal = store
        .create_proposal("memory", &note.text, "fixture", 3)
        .unwrap();
    store
        .update_proposal_eval(&proposal.id, 0.5, 0.83, 6)
        .unwrap();

    let error = store
        .set_proposal_status(&proposal.id, "applied")
        .unwrap_err()
        .to_string();
    assert!(error.contains("disabled note"), "{error}");
    assert_eq!(
        store.get_proposal(&proposal.id).unwrap().unwrap().status,
        "measured"
    );
    let unchanged = store.get_memory(&note.id).unwrap().unwrap();
    assert!(!unchanged.enabled);
    assert_eq!(unchanged.text, note.text);
}

#[test]
fn rolling_back_duplicate_memory_proposal_keeps_existing_note() {
    let store = temp_store("selfopt-existing-memory");
    let (note, _) = store
        .add_memory("User prefers direct answers.", "global", "user", None)
        .unwrap();
    let proposal = store
        .create_proposal("memory", &note.text, "fixture", 3)
        .unwrap();
    store
        .update_proposal_eval(&proposal.id, 0.5, 0.83, 6)
        .unwrap();

    store
        .set_proposal_status(&proposal.id, "applied")
        .unwrap()
        .unwrap();
    store
        .set_proposal_status(&proposal.id, "rolled_back")
        .unwrap()
        .unwrap();

    let existing = store.get_memory(&note.id).unwrap().unwrap();
    assert!(existing.enabled);
    assert_eq!(existing.text, note.text);
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
    let _env = credential_env::EnvGuard::isolated();
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
    let _env = credential_env::EnvGuard::isolated();
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
    let _env = credential_env::EnvGuard::isolated();
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
    let _env = credential_env::EnvGuard::isolated();
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let db = std::path::PathBuf::from(format!("/tmp/fl-memmig-{short}/f.db"));
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    {
        // As an older build left it: an applied memory proposal, schema v17.
        let c = historical::create(&db, 17);
        c.execute_batch(
            "INSERT INTO prompt_proposals(id,kind,content,rationale,status,est_tokens,created_at,applied_at)
               VALUES('p1','memory','Prefers tabs.','x','applied',1,'2026-01-01','2026-01-02');",
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

#[test]
fn optimiser_fix_plan_follows_failure_tags() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::optimiser::{fix_plan, tag_breakdown, FixPlan};
    use formless_core::storage::ReplayItem;
    let item = |tag: &str, ok: bool| ReplayItem {
        input: "{}".into(),
        error: String::new(),
        tag: tag.into(),
        recorded_success: ok,
    };
    let denied = vec![
        item("denied-capability", false),
        item("denied-capability", false),
        item("", true),
    ];
    assert_eq!(fix_plan(&denied), FixPlan::NeedsCapability(2));

    let mixed = vec![
        item("validation", false),
        item("validation", false),
        item("", false),
        item("denied-capability", false),
        item("", true),
    ];
    assert_eq!(
        tag_breakdown(&mixed),
        vec![
            ("validation".to_string(), 2),
            ("denied-capability".to_string(), 1),
            ("js".to_string(), 1)
        ]
    );
    let FixPlan::Rewrite(s) = fix_plan(&mixed) else {
        panic!("expected a rewrite")
    };
    assert!(s.contains("validation=2, denied-capability=1, js=1"), "{s}");
    let v = s.find("- validation").unwrap();
    assert!(v < s.find("- js").unwrap(), "most frequent tag first");
    assert!(s.contains("Fix the description and input_schema first"));
    assert!(s.contains("do not widen Capabilities"));
}

#[test]
fn optimiser_queue_persists_and_waits_only_for_busy_tools() {
    let _env = credential_env::EnvGuard::isolated();
    let store = temp_store("optq");
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let mk = |n: &str| {
        store
            .create_tool_version(
                n,
                "d",
                "{}",
                "{}",
                "function run(i){return i;}",
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
            .id
    };
    let (a, b) = (mk("tool_a"), mk("tool_b"));
    store.enqueue_optimise(&a).unwrap();
    store.enqueue_optimise(&b).unwrap();
    store.enqueue_optimise(&a).unwrap();
    assert_eq!(
        store.optimise_queue().unwrap(),
        vec![a.clone(), b.clone()],
        "no duplicates, oldest first"
    );
    let s = store.create_session(None).unwrap().id;
    let run = store.create_run(&s).unwrap();
    // tool_a was used by the active Run; tool_b is idle.
    store
        .record_invocation(
            &a,
            1,
            Some(&s),
            Some(&run),
            None,
            "success",
            "",
            "{}",
            "",
            "",
            1,
            100,
        )
        .unwrap();
    assert!(store.tool_busy(&a, std::slice::from_ref(&run)).unwrap());
    assert!(!store.tool_busy(&b, std::slice::from_ref(&run)).unwrap());
    assert!(
        !store.tool_busy(&a, &[]).unwrap(),
        "once that Run ends, tool_a is free"
    );
    // A still-running Invocation keeps a Tool busy regardless of Runs.
    store
        .start_invocation(&b, 1, Some(&s), None, None, "{}")
        .unwrap();
    assert!(store.tool_busy(&b, &[]).unwrap());
    store.dequeue_optimise(&a).unwrap();
    assert_eq!(store.optimise_queue().unwrap(), vec![b]);
}

#[test]
fn near_duplicate_proposals() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::optimiser::near_duplicate;
    assert!(near_duplicate(
        "Always stream large files in chunks.",
        "always stream large file in chunks"
    ));
    assert!(!near_duplicate(
        "Always stream large files in chunks.",
        "Retire Tools that keep failing validation."
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn generate_does_not_repeat_or_run_twice() {
    let _env = credential_env::EnvGuard::isolated();
    use axum::routing::post;
    use formless_protocol::Request;
    _env.set("FORMLESS_API_KEY", "test-key");
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let calls = calls.clone();
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let proposal = r#"{"kind":"prompt","content":"Always stream large files in chunks.","rationale":"oom"}"#;
                    assert_eq!(body["response_format"]["type"], "json_schema", "real guidance generation uses a schema");
                    assert_eq!(body["response_format"]["json_schema"]["schema"]["type"], "object");
                    assert!(body["response_format"]["json_schema"]["schema"].get("anyOf").is_none());
                    axum::Json(serde_json::json!({"choices": [{"message": {"content": proposal}}]}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-gen-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-gen-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = formless_core::Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "m".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    // The same lesson was already proposed and rejected.
    let p = store
        .create_proposal("prompt", "Always stream large files in chunks.", "x", 1)
        .unwrap();
    store.set_proposal_status(&p.id, "rejected").unwrap();
    let daemon = std::sync::Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let first = daemon.handle(Request::GenerateProposal).await.unwrap();
    let second = daemon.handle(Request::GenerateProposal).await.unwrap();
    assert_eq!(first["started"], true);
    assert_eq!(second["started"], false, "no parallel generation");
    for _ in 0..50 {
        if daemon.handle(Request::GenerateProposal).await.unwrap()["started"] == true {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // Both attempts repeated the rejected lesson: nothing new was stored.
    assert!(
        calls.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "retried once"
    );
    assert_eq!(store.list_proposals().unwrap().len(), 1);
}
