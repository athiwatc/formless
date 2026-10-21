//! Multiple providers and Roles (SPEC §4.5).

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
    std::env::set_var("FORMLESS_API_KEY", "key-a");
    std::env::set_var("FORMLESS_API_KEY_LOCAL", "key-b");
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
    std::env::set_var("FORMLESS_API_KEY", "key-a");
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
    for i in 0..6 {
        store
            .record_task(
                "scout",
                "fast-bad",
                "s",
                "r",
                &format!("a{i}"),
                500,
                10,
                2,
                if i < 2 { "completed" } else { "failed" },
            )
            .unwrap();
        store
            .record_task(
                "scout",
                "good",
                "s",
                "r",
                &format!("b{i}"),
                4000,
                90,
                3,
                "completed",
            )
            .unwrap();
    }
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
