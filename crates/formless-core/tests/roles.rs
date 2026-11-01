//! Multiple providers and Roles (SPEC §4.5).

#[path = "support/env.rs"]
mod credential_env;

use axum::routing::post;
use formless_core::config::{ProviderCfg, RoleCfg};
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use std::sync::{Arc, Mutex};

type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

/// A mock OpenAI-compatible provider that records requests and answers
/// with `answer` (streamed or not, as asked).
async fn mock(answer: &'static str) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    let stream = body["stream"] == true;
                    seen.lock().unwrap().push(body);
                    if stream {
                        let sse = format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            serde_json::json!({"choices": [{"delta": {"content": answer}}]})
                        );
                        axum::response::Response::builder()
                            .header("content-type", "text/event-stream")
                            .body(axum::body::Body::from(sse))
                            .unwrap()
                    } else {
                        axum::response::IntoResponse::into_response(axum::Json(
                            serde_json::json!({"choices": [{"message": {"content": answer}}]}),
                        ))
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

#[test]
fn providers_and_roles_resolve_with_clear_errors() {
    let _env = credential_env::EnvGuard::isolated();
    let mut cfg = Config::default();
    cfg.provider.model = "big".into();
    assert_eq!(cfg.provider_cfg(None).unwrap().model, "big");
    assert_eq!(cfg.provider_cfg(Some("default")).unwrap().model, "big");
    assert!(cfg
        .provider_cfg(Some("nope"))
        .unwrap_err()
        .to_string()
        .contains("unknown provider"));
    assert!(cfg
        .role("x")
        .unwrap_err()
        .to_string()
        .contains("no roles are configured"));
    cfg.roles.insert(
        "explorer".into(),
        RoleCfg {
            description: "search".into(),
            ..Default::default()
        },
    );
    assert!(cfg
        .role("x")
        .unwrap_err()
        .to_string()
        .contains("roles: explorer"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_role_on_a_second_provider_serves_llm_calls_and_the_session() {
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FORMLESS_API_KEY", "key-a");
    _env.set("FORMLESS_API_KEY_LOCAL", "key-b");
    let (a, seen_a) = mock("from A").await;
    let (b, seen_b) = mock("from B").await;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-roles-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-roles-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = a;
    cfg.provider.model = "big".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.providers.insert(
        "local".into(),
        ProviderCfg {
            base_url: b,
            model: "small".into(),
            max_retries: 1,
            ..Default::default()
        },
    );
    cfg.roles.insert(
        "explorer".into(),
        RoleCfg {
            description: "cheap broad search".into(),
            provider: Some("local".into()),
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    // The agent sees the role and its description.
    let prompt = daemon.effective_system_prompt();
    assert!(
        prompt.contains("- explorer: cheap broad search [local · provider default]"),
        "{prompt}"
    );

    // A Tool asks for the role: provider B, its default model.
    let out = daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "ask_scout", "description": "asks", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": "function run(i){ return {text: llm.complete('hi', {role: 'explorer'})}; }",
            "capabilities": ["llm"]}))
        .await;
    assert!(out.contains("\"created\""), "{out}");
    let (tool, ver) = store.get_tool_by_name("ask_scout").unwrap().unwrap();
    store
        .set_approval(
            &tool.id,
            &formless_runtime::Capabilities::parse(&ver.capabilities).unwrap(),
            ver.memory_mb,
        )
        .unwrap();
    let r = daemon
        .call_tool_direct(&sid, "ask_scout", serde_json::json!({}))
        .await;
    assert!(r.contains("from B"), "{r}");
    assert_eq!(seen_b.lock().unwrap().last().unwrap()["model"], "small");
    assert!(seen_a.lock().unwrap().is_empty(), "provider A untouched");
    // An unknown role is a clear error.
    let r = daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "bad_role", "description": "d", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": "function run(i){ return {t: llm.complete('x', {role: 'ghost'})}; }",
            "capabilities": ["llm"]}))
        .await;
    assert!(r.contains("\"created\""), "smoke stubs llm: {r}");
    let (tool, ver) = store.get_tool_by_name("bad_role").unwrap().unwrap();
    store
        .set_approval(
            &tool.id,
            &formless_runtime::Capabilities::parse(&ver.capabilities).unwrap(),
            ver.memory_mb,
        )
        .unwrap();
    let r = daemon
        .call_tool_direct(&sid, "bad_role", serde_json::json!({}))
        .await;
    assert!(r.contains("unknown role 'ghost' (roles: explorer)"), "{r}");

    // The primary Session itself on the role: the Run goes to B.
    store
        .set_session_model(&sid, Some("role:explorer".into()), None)
        .unwrap();
    daemon
        .handle(Request::SendMessage {
            session_id: sid.clone(),
            content: "hello".into(),
            mode: formless_protocol::SendMode::Send,
            client_msg_id: None,
        })
        .await
        .unwrap();
    for _ in 0..100 {
        if store
            .last_run(&sid)
            .unwrap()
            .is_some_and(|(_, s)| s == "completed")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(store.last_run(&sid).unwrap().unwrap().1, "completed");
    let last = seen_b.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        (last["model"].as_str(), last["stream"].as_bool()),
        (Some("small"), Some(true))
    );
    assert!(seen_a.lock().unwrap().is_empty());

    // Management: a role's provider can't be removed from under it.
    assert!(daemon
        .handle(Request::RemoveProvider {
            name: "local".into()
        })
        .await
        .is_err());
    let list = daemon.handle(Request::ListProviders).await.unwrap();
    assert_eq!(list["providers"][1]["name"], "local");
    assert!(
        list.to_string().find("key-b").is_none(),
        "keys never listed"
    );
}

#[test]
fn per_task_routing_gates_on_quality_then_cost() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::routing::{select_task_model, TaskStat};
    let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let stat = |m: &str, samples, good, ms, tok| TaskStat {
        model: m.into(),
        samples,
        good,
        avg_ms: ms,
        avg_tokens: tok,
    };
    // Fast but poor quality loses to a slower model that clears the gate.
    let stats = vec![
        stat("tiny", 10, 5, 2_000.0, 1_000.0),
        stat("big", 10, 9, 9_000.0, 4_000.0),
    ];
    assert_eq!(
        select_task_model(&c(&["tiny", "big"]), &stats, 5, 0.8, false).as_deref(),
        Some("big")
    );
    // Faster per call but slower per task (15 × 1.5 s vs 3 × 4 s): per-task
    // numbers decide, so the strong model wins.
    let stats = vec![
        stat("small", 10, 9, 22_500.0, 30_000.0),
        stat("strong", 10, 9, 12_000.0, 9_000.0),
    ];
    assert_eq!(
        select_task_model(&c(&["small", "strong"]), &stats, 5, 0.8, false).as_deref(),
        Some("strong")
    );
    // Too few samples: the first listed candidate…
    let stats = vec![stat("b", 2, 2, 100.0, 10.0)];
    assert_eq!(
        select_task_model(&c(&["a", "b"]), &stats, 5, 0.8, false).as_deref(),
        Some("a")
    );
    // …unless exploring, which tries the least-sampled one.
    assert_eq!(
        select_task_model(&c(&["a", "b"]), &stats, 5, 0.8, true).as_deref(),
        Some("a")
    );
    let stats = vec![stat("a", 9, 9, 100.0, 10.0), stat("b", 1, 1, 100.0, 10.0)];
    assert_eq!(
        select_task_model(&c(&["a", "b"]), &stats, 5, 0.8, true).as_deref(),
        Some("b")
    );
}

#[test]
fn task_stats_count_respawns_and_limits_as_bad() {
    let _env = credential_env::EnvGuard::isolated();
    let store = Store::open_in_memory().unwrap();
    store
        .record_task("scout", "m1", "s", "r1", "h1", 1000, 50, 3, "completed")
        .unwrap();
    store
        .record_task("scout", "m1", "s", "r1", "h2", 3000, 150, 5, "completed")
        .unwrap();
    store
        .record_task("scout", "m1", "s", "r1", "h3", 2000, 100, 40, "limit")
        .unwrap();
    // The primary agent ran task h1 again in the same Run.
    store.mark_task_respawned("scout", "r1", "h1").unwrap();
    let stats = store.task_stats("scout").unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!((stats[0].samples, stats[0].good), (3, 1));
    assert!((stats[0].avg_ms - 2000.0).abs() < 1e-9);
    assert!(store.task_stats("other").unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_auto_role_uses_the_measured_winner() {
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FORMLESS_API_KEY", "key-a");
    let (a, seen) = mock("ok").await;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-auto-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-auto-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = a;
    cfg.provider.model = "big".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.tools.task_explore_rate = 0.0;
    cfg.roles.insert(
        "scout".into(),
        RoleCfg {
            description: "search".into(),
            model: "auto".into(),
            candidates: vec!["fast-bad".into(), "good".into()],
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    // Legacy task_stats alone intentionally has no provable provider/endpoint,
    // window or paid-call correlation. Seed the finalized, exactly linked
    // receipts the current selector consumes; retain the original quality
    // contrast (fast-bad 2/6, good 6/6), not a cold-start candidate-order test.
    let db = rusqlite::Connection::open(paths.db_file()).unwrap();
    let endpoint = formless_core::tokens::provider_key("default", &cfg.provider);
    let finished = chrono::Utc::now().timestamp_millis();
    for i in 0..6 {
        for (model, hash, ms, tokens, turns, good) in [
            ("fast-bad", format!("a{i}"), 500, 10, 2, i < 2),
            ("good", format!("b{i}"), 4000, 90, 3, true),
        ] {
            store
                .record_task(
                    "scout",
                    model,
                    "s",
                    "r",
                    &hash,
                    ms,
                    tokens,
                    turns,
                    if good { "completed" } else { "failed" },
                )
                .unwrap();
            let task_id: String = db
                .query_row(
                    "SELECT id FROM task_stats WHERE role='scout' AND task_hash=?1",
                    [&hash],
                    |row| row.get(0),
                )
                .unwrap();
            let call_id = format!("call-{hash}");
            let identity =
                serde_json::json!({"provider":"default","endpoint":endpoint,"model":model});
            let outcome = if good {
                serde_json::json!({"Success": identity})
            } else {
                serde_json::json!({"ContractFailure": identity})
            };
            let call = serde_json::json!({
                "id":call_id,"outcome":outcome,
                "attempts":[{"id":format!("attempt-{hash}"),"identity":identity,"sent":true,
                    "status":200,"neutral":null,"response_complete":true,"usage":{
                        "prompt_tokens":tokens,"completion_tokens":0,"reasoning_tokens":0,
                        "cached_tokens":0,"cache_write_tokens":0,"cost":null,"cost_unknown":false,"reported":3
                    }}],
                "task_ids":[task_id],"resource_only":false,"truncated":false,
                "started_at_ms":finished-ms,"finished_at_ms":finished,"elapsed_ms":ms
            });
            let task = serde_json::json!({
                "id":task_id,"role":"scout","parent_ids":[],
                "outcome":if good { "Completed" } else { "ContractFailure" },
                "responsible":identity,"mixed":false,"expected_calls":1,"capture_complete":true,
                "started_at_ms":finished-ms,"finished_at_ms":finished,"elapsed_ms":ms
            });
            db.execute(
                "INSERT INTO routing_calls(id,finished_at_ms,provider,endpoint,model,payload) VALUES(?1,?2,'default',?3,?4,?5)",
                rusqlite::params![call_id,finished,endpoint,model,call.to_string()],
            ).unwrap();
            db.execute(
                "INSERT INTO routing_tasks(id,finished_at_ms,role,provider,endpoint,model,final,payload) VALUES(?1,?2,'scout','default',?3,?4,1,?5)",
                rusqlite::params![task_id,finished,endpoint,model,task.to_string()],
            ).unwrap();
            db.execute(
                "INSERT INTO routing_call_tasks(call_id,task_id) VALUES(?1,?2)",
                rusqlite::params![call_id, task_id],
            )
            .unwrap();
        }
    }
    assert_eq!(db.query_row("SELECT COUNT(*) FROM routing_tasks t JOIN routing_call_tasks l ON l.task_id=t.id JOIN routing_calls c ON c.id=l.call_id WHERE t.final=1 AND t.endpoint=?1 AND c.endpoint=?1",[&endpoint],|r|r.get::<_,i64>(0)).unwrap(),12);
    drop(db);
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "ask", "description": "asks", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": "function run(i){ return {t: llm.complete('hi', {role: 'scout'})}; }",
            "capabilities": ["llm"]}))
        .await;
    let (tool, ver) = store.get_tool_by_name("ask").unwrap().unwrap();
    store
        .set_approval(
            &tool.id,
            &formless_runtime::Capabilities::parse(&ver.capabilities).unwrap(),
            ver.memory_mb,
        )
        .unwrap();
    let r = daemon
        .call_tool_direct(&sid, "ask", serde_json::json!({}))
        .await;
    assert!(r.contains("ok"), "{r}");
    assert_eq!(seen.lock().unwrap().last().unwrap()["model"], "good");
}

#[test]
fn global_auto_migrates_to_a_primary_role() {
    let _env = credential_env::EnvGuard::isolated();
    let mut cfg = Config::default();
    assert_eq!(cfg.migrate_global_auto(), None);
    cfg.provider.model = "auto".into();
    cfg.provider.routing_models = vec!["m1".into(), "m2".into()];
    let warning = cfg.migrate_global_auto().unwrap();
    assert!(
        warning.contains("deprecated") && warning.contains("m1, m2"),
        "{warning}"
    );
    assert_eq!(cfg.provider.model, "role:primary");
    let role = &cfg.roles["primary"];
    assert_eq!(
        (role.model.as_str(), role.candidates.clone()),
        ("auto", vec!["m1".to_string(), "m2".to_string()])
    );
    assert_eq!(cfg.migrate_global_auto(), None, "once");
}

#[tokio::test(flavor = "multi_thread")]
async fn old_auto_configs_migrate_on_start_and_primary_runs_feed_routing() {
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FORMLESS_API_KEY", "key-a");
    let (a, seen) = mock("done").await;
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-mig-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-mig-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = a;
    cfg.provider.model = "auto".into();
    cfg.provider.routing_models = vec!["alpha".into(), "beta".into()];
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.tools.task_explore_rate = 0.0;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let saved = Config::load_file_only(&paths).unwrap();
    assert_eq!(
        saved.provider.model, "role:primary",
        "the file was rewritten"
    );
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    daemon
        .handle(Request::SendMessage {
            session_id: sid.clone(),
            content: "hi".into(),
            mode: formless_protocol::SendMode::Send,
            client_msg_id: None,
        })
        .await
        .unwrap();
    for _ in 0..100 {
        if store
            .last_run(&sid)
            .unwrap()
            .is_some_and(|(_, s)| s == "completed")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Too few samples: the first candidate; the Run became a task.
    assert_eq!(seen.lock().unwrap().last().unwrap()["model"], "alpha");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let stats = store.task_stats("primary").unwrap();
    assert_eq!(
        (stats.len(), stats[0].model.as_str(), stats[0].good),
        (1, "alpha", 1)
    );
    // The user's "that's wrong" marks it bad.
    store.mark_last_run_followup(&sid, "corrected").unwrap();
    assert_eq!(store.task_stats("primary").unwrap()[0].good, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_llm_spend_counts_toward_the_run_budget() {
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FORMLESS_API_KEY", "key-a");
    let seen: Seen = Arc::default();
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(body);
                    axum::Json(serde_json::json!({
                        "choices": [{"message": {"content": "ok"}}],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 10, "cost": 1.0}
                    }))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-budget-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-budget-{short}"),
    };
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "m".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.limits.run_usd = Some(2.5);
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let out = daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "spender", "description": "loops", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": "function run(i){ for (let n = 0; n < 10; n++) llm.complete('hi'); return {}; }",
            "capabilities": ["llm"]}))
        .await;
    assert!(out.contains("\"created\""), "{out}");
    let (tool, ver) = store.get_tool_by_name("spender").unwrap().unwrap();
    store
        .set_approval(
            &tool.id,
            &formless_runtime::Capabilities::parse(&ver.capabilities).unwrap(),
            ver.memory_mb,
        )
        .unwrap();
    let before = seen.lock().unwrap().len();
    let r = daemon
        .call_tool_direct(&sid, "spender", serde_json::json!({}))
        .await;
    assert!(r.contains("budget"), "{r}");
    // $1 a call against $2.50: the fourth call is refused.
    assert_eq!(seen.lock().unwrap().len() - before, 3);
    assert_eq!(store.run_usage("direct").unwrap().prompt_tokens, 30);
}

/// An empty refusal is stored as a marked placeholder: the transcript says
/// why, and the next request never hands it back as the model's words (#1104).
#[tokio::test(flavor = "multi_thread")]
async fn a_refusal_placeholder_is_never_sent_back_as_the_models_words() {
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FORMLESS_API_KEY", "key-a");
    let seen: Seen = Arc::default();
    let app = axum::Router::new().route(
        "/chat/completions",
        post({
            let seen = seen.clone();
            move |axum::Json(body): axum::Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    let first = {
                        let mut s = seen.lock().unwrap();
                        s.push(body);
                        s.len() == 1
                    };
                    let chunk = if first {
                        serde_json::json!({"choices": [{"delta": {}, "finish_reason": "refusal"}]})
                    } else {
                        serde_json::json!({"choices": [{"delta": {"content": "fine"}, "finish_reason": "stop"}]})
                    };
                    axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(format!("data: {chunk}\n\ndata: [DONE]\n\n")))
                        .unwrap()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-refuse-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-refuse-{short}"),
    };
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = base;
    cfg.provider.model = "m".into();
    cfg.provider.max_retries = 1;
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    for (n, text) in ["first", "second"].iter().enumerate() {
        daemon
            .handle(Request::SendMessage {
                session_id: sid.clone(),
                content: text.to_string(),
                mode: formless_protocol::SendMode::Send,
                client_msg_id: None,
            })
            .await
            .unwrap();
        for _ in 0..100 {
            if seen.lock().unwrap().len() > n
                && store
                    .last_run(&sid)
                    .unwrap()
                    .is_some_and(|(_, s)| s == "completed")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    let msgs = store.list_messages(&sid).unwrap();
    let refused = msgs
        .iter()
        .find(|m| m.content.contains("declined"))
        .expect("placeholder stored");
    let meta: serde_json::Value = serde_json::from_str(refused.meta.as_deref().unwrap()).unwrap();
    assert_eq!(meta["stop_reason"], "refusal");
    assert_eq!(meta["placeholder"], true);
    let second = seen.lock().unwrap()[1].to_string();
    assert!(!second.contains("declined to answer"), "{second}");
    assert!(second.contains("No answer was given (refusal)"), "{second}");
}

/// `RunTool` says whether the call succeeded from its outcome, not its
/// text: a result reading "errors: 0" is a success (#901).
#[tokio::test(flavor = "multi_thread")]
async fn run_tool_reports_ok_from_the_outcome() {
    let _env = credential_env::EnvGuard::isolated();
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-runok-{short}"));
    let paths = Paths {
        data_dir: dir.join("d"),
        config_dir: dir.join("c"),
        profile: format!("test-runok-{short}"),
    };
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    for (name, body) in [
        ("count_errors", "return 'errors: 0';"),
        (
            "fails_on_ask",
            "if (i.fail) throw new Error('boom'); return 1;",
        ),
    ] {
        let out = daemon
            .call_tool_direct(
                &sid,
                "create_tool",
                serde_json::json!({
                "name": name, "description": "d", "input_schema": {"type": "object"},
                "output_schema": {}, "implementation_js": format!("function run(i){{ {body} }}"),
                "capabilities": []}),
            )
            .await;
        assert!(
            out.contains("\"created\"") || out.contains("smoke"),
            "{out}"
        );
    }
    let id = |n: &str| store.get_tool_by_name(n).unwrap().unwrap().0.id;
    let run = |tid: String, input: serde_json::Value| {
        let d = daemon.clone();
        let sid = sid.clone();
        async move {
            d.handle(Request::RunTool {
                id: tid,
                input,
                session_id: sid,
            })
            .await
            .unwrap()
        }
    };
    let ok = run(id("count_errors"), serde_json::json!({})).await;
    assert_eq!(ok["ok"], true, "{ok}");
    assert!(ok["result"].as_str().unwrap().contains("errors: 0"));
    let bad = run(id("fails_on_ask"), serde_json::json!({"fail": true})).await;
    assert_eq!(bad["ok"], false, "{bad}");
}
