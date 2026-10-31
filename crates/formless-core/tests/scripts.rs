//! Orchestration scripts (SPEC §7.12): `run_script` fans out over registry
//! Tools in parallel with no Capabilities of its own, asks approval for its
//! `uses` list up front, records its nested Invocations under it, and can
//! be saved as a Tool.

use formless_core::daemon::Daemon;
use formless_core::storage::Store;
use formless_core::{Config, Paths};
use formless_runtime::Capabilities;
use serde_json::{json, Value};
use std::sync::Arc;

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

fn seed(store: &Store, name: &str, code: &str, caps: &[&str]) -> String {
    store
        .create_tool_version(
            name,
            "test tool",
            r#"{"type":"object","properties":{"n":{"type":"integer"}}}"#,
            "{}",
            code,
            &Capabilities::parse(&caps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
            256,
            "agent",
        )
        .unwrap()
        .0
        .id
}

/// A daemon with `slow_double` (300 ms, returns n*2) in the registry.
fn setup(tag: &str, cfg: Config) -> (Paths, Store, String, Arc<Daemon>) {
    let paths = temp_paths(tag);
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    seed(
        &store,
        "slow_double",
        "function run(i){ sleep(300); return i.n * 2; }",
        &[],
    );
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    (paths, store, session, daemon)
}

async fn script(daemon: &Arc<Daemon>, session: &str, args: Value) -> String {
    daemon.call_tool_direct(session, "run_script", args).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_named_call_cannot_replace_the_dispatcher() {
    let (_paths, store, session, daemon) = setup("call-name", Config::default());
    // These are legal registry names, including the special JS object key.
    for name in ["call", "other", "__proto__"] {
        formless_core::tools::validate_name(name).unwrap();
        seed(
            &store,
            name,
            &format!("function run(i) {{ return {{tool: '{name}', input: i}}; }}"),
            &[],
        );
    }
    let out = script(&daemon, &session, json!({
        "code": "return await Promise.all([tools.call('call', {n: 1}), tools.call('other', {n: 2}), tools.other({n: 3}), tools.__proto__({n: 4})]);",
        "uses": ["call", "other", "__proto__"],
        "save_as": {"name":"call_both", "description":"Call both tools", "input_schema":{"type":"object"}, "output_schema":{"type":"array"}}
    })).await;
    let out: Value = serde_json::from_str(&out).unwrap();
    let expected = json!([
        {"tool":"call", "input":{"n":1}}, {"tool":"other", "input":{"n":2}},
        {"tool":"other", "input":{"n":3}}, {"tool":"__proto__", "input":{"n":4}}
    ]);
    assert_eq!(out["result"], expected);
    assert_eq!(out["saved"]["created"], json!("call_both"));
    let (saved, ok) = daemon
        .call_tool_direct_outcome(&session, "call_both", json!({}))
        .await;
    assert!(ok, "{saved}");
    assert_eq!(serde_json::from_str::<Value>(&saved).unwrap(), expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn scripts_fan_out_in_parallel_and_return_only_the_result() {
    let (paths, _store, session, daemon) = setup("fan", Config::default());
    let started = std::time::Instant::now();
    let out = script(
        &daemon,
        &session,
        json!({
            "code": "const xs = await Promise.all([1, 2, 3, 4].map(n => tools.slow_double({n})));\nreturn xs.reduce((a, b) => a + b, 0);",
            "uses": ["slow_double"]
        }),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!({"result": 20}),
        "{out}"
    );
    // Four 300 ms calls one after another would take 1.2 s.
    assert!(started.elapsed() < std::time::Duration::from_millis(1100));

    let db = rusqlite::Connection::open(paths.db_file()).unwrap();
    let (children, parents): (i64, i64) = db
        .query_row(
            "SELECT count(*), count(DISTINCT parent_id) FROM invocations \
             WHERE parent_id LIKE 'script:%' AND status='success'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((children, parents), (4, 1));
    let status: String = db
        .query_row("SELECT status FROM scripts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "success");
}

#[tokio::test(flavor = "multi_thread")]
async fn scripts_have_no_ambient_authority_and_only_reach_their_uses() {
    let (_paths, store, session, daemon) = setup("authority", Config::default());
    seed(&store, "other", "function run(){ return 'reached'; }", &[]);
    let out = script(
        &daemon,
        &session,
        json!({
            "code": "return [typeof fetch, typeof fs, typeof process, typeof store, typeof llm, typeof agents].join(',');",
            "uses": []
        }),
    )
    .await;
    assert!(
        out.contains("undefined,undefined,undefined,undefined,undefined,undefined"),
        "{out}"
    );

    // Neither the script's `tools` nor the global bridge reaches a Tool
    // outside the uses list.
    let out = script(
        &daemon,
        &session,
        json!({
            "code": "const r = [];\n\
                     try { await tools.call('other', {}); r.push('local'); } catch (e) { r.push(e.message); }\n\
                     try { await globalThis.tools.call('other', {}); r.push('global'); } catch (e) { r.push(e.message); }\n\
                     return r;",
            "uses": ["slow_double"]
        }),
    )
    .await;
    let r: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(r["result"].as_array().map(Vec::len), Some(2), "{out}");
    for msg in r["result"].as_array().unwrap() {
        assert!(
            msg.as_str()
                .unwrap()
                .contains("not in this script's uses list"),
            "{out}"
        );
    }

    let out = script(
        &daemon,
        &session,
        json!({"code": "return 1;", "uses": ["create_tool"]}),
    )
    .await;
    assert!(out.contains("is a built-in"), "{out}");
    let out = script(
        &daemon,
        &session,
        json!({"code": "return 1;", "uses": ["missing"]}),
    )
    .await;
    assert!(out.contains("no tool named 'missing'"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_that_forgets_await_fails() {
    let (_paths, _store, session, daemon) = setup("noawait", Config::default());
    let out = script(
        &daemon,
        &session,
        json!({"code": "const x = tools.slow_double({n: 2});\nreturn x * 2;", "uses": ["slow_double"]}),
    )
    .await;
    assert!(out.contains("await every call"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn scripts_ask_approval_for_their_uses_before_running() {
    let mut cfg = Config::default();
    cfg.security.secure = true;
    let (paths, store, session, daemon) = setup("approve", cfg);
    seed(&store, "pinger", "function run(){ return 1; }", &["net"]);
    // No client is attached to answer, so the approval fails closed and
    // nothing runs: not even the Tool that needs no approval.
    let out = script(
        &daemon,
        &session,
        json!({"code": "await tools.slow_double({n: 1});\nreturn await tools.pinger({});", "uses": ["slow_double", "pinger"]}),
    )
    .await;
    assert!(
        out.contains("user denied approval for tool 'pinger'"),
        "{out}"
    );
    let db = rusqlite::Connection::open(paths.db_file()).unwrap();
    let n: i64 = db
        .query_row("SELECT count(*) FROM invocations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_script_becomes_a_composing_tool() {
    let (_paths, store, session, daemon) = setup("save", Config::default());
    let out = script(
        &daemon,
        &session,
        json!({
            "code": "const xs = await Promise.all(input.ns.map(n => tools.slow_double({n})));\nreturn xs;",
            "uses": ["slow_double"],
            "input": {"ns": [1, 2]},
            "save_as": {
                "name": "double_all",
                "description": "Double every number",
                "input_schema": {"type": "object", "properties": {"ns": {"type": "array", "items": {"type": "integer"}}}, "required": ["ns"]},
                "output_schema": {"type": "array"}
            }
        }),
    )
    .await;
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["result"], json!([2, 4]), "{out}");
    assert_eq!(v["saved"]["created"], "double_all", "{out}");
    let (_, ver) = store.get_tool_by_name("double_all").unwrap().unwrap();
    assert!(ver.capabilities.is_empty());

    let out = daemon
        .call_tool_direct(&session, "double_all", json!({"ns": [5]}))
        .await;
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!([10]),
        "{out}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_script_across_sessions_is_flagged_for_saving() {
    let mut cfg = Config::default();
    cfg.scripts.recurrence_threshold = 2;
    let (_paths, store, first, daemon) = setup("recur", cfg);
    let run = |session: String, n: i64| {
        let daemon = daemon.clone();
        async move {
            let out = script(
                &daemon,
                &session,
                json!({"code": format!("return await tools.slow_double({{n: {n}}});"), "uses": ["slow_double"]}),
            )
            .await;
            serde_json::from_str::<Value>(&out).unwrap()
        }
    };
    assert!(run(first.clone(), 1).await.get("hint").is_none());
    // Same session again: still one session.
    assert!(run(first, 2).await.get("hint").is_none());
    let second = store.create_session(None).unwrap().id;
    let v = run(second, 3).await;
    assert!(v["hint"].as_str().unwrap().contains("2 sessions"), "{v}");
    let recurring = store.recurring_scripts(2, 5).unwrap();
    assert_eq!(recurring.len(), 1);
    assert_eq!(recurring[0].0, 2);
    assert_eq!(recurring[0].1, vec!["slow_double".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_past_its_timeout_is_stopped() {
    let mut cfg = Config::default();
    cfg.scripts.timeout_secs = 1;
    let (_paths, _store, session, daemon) = setup("timeout", cfg);
    let started = std::time::Instant::now();
    let out = script(
        &daemon,
        &session,
        json!({"code": "await new Promise(r => setTimeout(r, 10000));\nreturn 1;", "uses": []}),
    )
    .await;
    assert!(out.contains("scripts.timeout_secs"), "{out}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn a_scripts_step_tokens_are_split_over_the_tools_it_called() {
    let paths = temp_paths("tokens");
    let store = Store::open(&paths.db_file()).unwrap();
    let tool = seed(&store, "t", "function run(){ return 1; }", &[]);
    let since = chrono::Utc::now().to_rfc3339();
    let direct = store
        .start_invocation(&tool, 1, None, Some("r"), None, "{}")
        .unwrap();
    let a = store
        .start_invocation(&tool, 1, None, Some("r"), Some("script:s1"), "{}")
        .unwrap();
    let b = store
        .start_invocation(&tool, 1, None, Some("r"), Some("script:s1"), "{}")
        .unwrap();
    let nested = store
        .start_invocation(&tool, 1, None, Some("r"), Some(&a), "{}")
        .unwrap();
    // One step made two calls: a registry Tool and a run_script.
    store.attribute_step_tokens("r", &since, 100, 2).unwrap();
    let db = rusqlite::Connection::open(paths.db_file()).unwrap();
    let tokens = |id: &str| -> i64 {
        db.query_row("SELECT tokens FROM invocations WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(tokens(&direct), 50);
    assert_eq!((tokens(&a), tokens(&b)), (25, 25));
    assert_eq!(tokens(&nested), 0);
}
