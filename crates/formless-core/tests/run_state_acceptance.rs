//! Cross-component acceptance for durable Run recaps and authoritative plans.
//! All provider traffic terminates at a private loopback scripted server.
#[path = "support/env.rs"]
mod credential_env;

use axum::{routing::post, Json, Router};
use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::{Request, SendMode};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

struct Fixture {
    root: std::path::PathBuf,
    paths: Paths,
    provider: tokio::task::JoinHandle<()>,
    seen: Arc<Mutex<Vec<Value>>>,
}
impl Fixture {
    async fn new(env: &credential_env::EnvGuard, script: Vec<Value>) -> Self {
        env.set("FORMLESS_API_KEY", "private-run-state-fixture");
        env.remove("FORMLESS_BASE_URL");
        env.remove("FORMLESS_MODEL");
        let root = std::env::temp_dir().join(format!("fl-state-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new().route("/chat/completions", post({
            let seen = seen.clone();
            move |Json(body): Json<Value>| {
                let seen = seen.clone();
                let script = script.clone();
                async move {
                    let index = {
                        let mut seen = seen.lock().unwrap();
                        let index = seen.len();
                        seen.push(body);
                        index
                    };
                    let delta = script.get(index).expect("unexpected provider request");
                    let sse = format!("data: {}\n\ndata: [DONE]\n\n", json!({
                        "choices":[{"delta":delta,"finish_reason": if delta.get("tool_calls").is_some() { "tool_calls" } else { "stop" }}],
                        "usage":{"prompt_tokens":11,"completion_tokens":3}
                    }));
                    axum::response::Response::builder().header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(sse)).unwrap()
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = Config::default();
        config.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
        config.provider.model = "private-scripted".into();
        config.provider.api_style = "chat".into();
        config.provider.max_retries = 0;
        config.naming.enabled = false;
        config.skills.auto_create = false;
        config.save(&paths).unwrap();
        let provider = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            root,
            paths,
            provider,
            seen,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.provider.abort();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
}
async fn snapshot(daemon: &Arc<Daemon>, session: &str) -> Value {
    daemon
        .handle(Request::GetSession { id: session.into() })
        .await
        .unwrap()
}
async fn finish(daemon: &Arc<Daemon>, session: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let state = snapshot(daemon, session).await;
            if state["running"] == false {
                return state;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("private scripted Run did not finish")
}
async fn send(daemon: &Arc<Daemon>, session: &str, text: &str) -> Value {
    daemon
        .handle(Request::SendMessage {
            session_id: session.into(),
            content: text.into(),
            mode: SendMode::Send,
            client_msg_id: None,
        })
        .await
        .unwrap()
}
fn authoritative_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "system")
        .map(|message| message["content"].to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_observes_valid_plan_and_restart_restores_exact_terminal_recap() {
    let env = credential_env::EnvGuard::isolated();
    let plan = json!({"items":[{"content":"ACCEPTANCE_PLAN_STEP_761","status":"in_progress"}]});
    let invalid = json!({"items":[{"content":"INVALID_STEP_A","status":"in_progress"},{"content":"INVALID_STEP_B","status":"in_progress"}]});
    let fixture = Fixture::new(&env, vec![
        tool("plan-1", "update_plan", plan),
        tool("plan-invalid", "update_plan", invalid),
        tool("build-1", "create_tool", json!({"name":"acceptance_identity","description":"private acceptance identity","input_schema":{"type":"object"},"output_schema":{},"implementation_js":"function run(i){ return i; }","capabilities":[]})),
        json!({"content":"finished"}),
        json!({"content":"unrelated finished"}),
    ]).await;
    let store = Store::open(&fixture.paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(fixture.paths.clone()).unwrap());
    let ack = send(&daemon, &session, "Complete the private acceptance task").await;
    let acknowledged_run = ack["run_id"]
        .as_str()
        .expect("started SendMessage must identify its Run");
    let terminal = finish(&daemon, &session).await;
    assert_eq!(terminal["latest_run"]["run_id"], acknowledged_run);
    assert_eq!(terminal["plan_state"]["run_id"], acknowledged_run);
    assert_eq!(
        terminal["plan"].as_array().unwrap().len(),
        1,
        "invalid multi-active update must be atomic"
    );
    assert_eq!(terminal["plan"][0]["content"], "ACCEPTANCE_PLAN_STEP_761");
    let recap = terminal["last_recap"].clone();
    assert!(
        recap.is_object(),
        "eligible terminal recap must be durable before Run completion: {terminal}"
    );
    assert_eq!(recap["recap"]["status"], "completed");
    assert_eq!(recap["recap"]["steps"], 4);
    assert!(
        recap["recap"]["built"]
            .to_string()
            .contains("acceptance_identity"),
        "actual created Tool belongs in immutable recap: {recap}"
    );
    {
        let seen = fixture.seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        for body in &seen[1..] {
            let messages = body["messages"].as_array().unwrap();
            assert_eq!(messages[0]["role"], "system");
            assert_eq!(messages.iter().filter(|m| m["role"] == "system").count(), 1);
            assert!(
                authoritative_text(body).contains("ACCEPTANCE_PLAN_STEP_761"),
                "next provider turn must read authoritative plan"
            );
            assert!(!authoritative_text(body).contains("INVALID_STEP_A"));
        }
    }
    assert!(
        !store
            .session_system_prompt(&session)
            .unwrap()
            .unwrap()
            .contains("ACCEPTANCE_PLAN_STEP_761"),
        "dynamic plan must not mutate cached prompt base"
    );
    // A later Tool version cannot rewrite what the completed Run built.
    store
        .create_tool_version(
            "acceptance_identity",
            "later unrelated edit",
            r#"{"type":"object"}"#,
            "{}",
            "function run(){ return {later:true}; }",
            &formless_runtime::Capabilities::default(),
            64,
            "user",
        )
        .unwrap();
    daemon.handle(Request::Shutdown).await.unwrap();
    drop(daemon);
    let restarted = Arc::new(Daemon::new(fixture.paths.clone()).unwrap());
    let restored = snapshot(&restarted, &session).await;
    assert_eq!(
        restored["last_recap"], recap,
        "restart must restore the recorded terminal snapshot exactly"
    );
    assert_eq!(restored["plan_state"]["run_status"], "completed");
    send(&restarted, &session, "An unrelated new task").await;
    let fresh = finish(&restarted, &session).await;
    assert!(
        fresh["last_recap"].is_null(),
        "ineligible latest Run must not fall back to an older recap"
    );
    assert_eq!(fresh["plan_state"]["previous_task"], true);
    assert_ne!(fresh["latest_run"]["run_id"], recap["run_id"]);
    restarted.handle(Request::Shutdown).await.unwrap();
}

#[test]
fn historical_forks_use_actual_copied_boundary_and_clear_revisions() {
    let root = std::env::temp_dir().join(format!("fl-plan-cuts-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("history.db");
    {
        let store = Store::open(&db).unwrap();
        let session = store.create_session(None).unwrap().id;
        let run = store.create_run(&session).unwrap();
        let items = |name: &str| {
            vec![formless_protocol::PlanItem {
                content: name.into(),
                status: "in_progress".into(),
            }]
        };
        let first = store
            .append_message(&session, "user", "first boundary")
            .unwrap();
        store
            .set_run_plan(&session, &run, &items("first plan"))
            .unwrap();
        let second = store
            .append_message(&session, "assistant", "second boundary")
            .unwrap();
        store
            .set_run_plan(&session, &run, &items("second plan"))
            .unwrap();
        let clear = store
            .append_message(&session, "assistant", "clear boundary")
            .unwrap();
        store.set_run_plan(&session, &run, &[]).unwrap();
        let latest = store
            .append_message(&session, "assistant", "latest boundary")
            .unwrap();
        store
            .set_run_plan(&session, &run, &items("latest plan"))
            .unwrap();
        for (cut, expected) in [
            (first.seq, Some("first plan")),
            (second.seq, Some("second plan")),
            (clear.seq, None),
            (latest.seq, Some("latest plan")),
        ] {
            let fork = store
                .fork_session(&session, None, Some(cut))
                .unwrap()
                .unwrap();
            let plan = store.session_plan(&fork.id).unwrap();
            assert_eq!(
                plan.first().map(|item| item.content.as_str()),
                expected,
                "fork at {cut} must honor revision including clear"
            );
            if expected.is_some() {
                let state = store.session_plan_state(&fork.id).unwrap().unwrap();
                assert_eq!(
                    state.run_id, None,
                    "copied foreign Run must not grant local plan authority"
                );
                assert_eq!(state.origin, "copied");
            }
        }
        // Deleted message rows make the requested cut differ from the real
        // copied boundary. A revision anchored in that gap must not leak in.
        let raw = rusqlite::Connection::open(&db).unwrap();
        raw.execute(
            "DELETE FROM messages WHERE session_id=?1 AND seq IN (?2,?3)",
            rusqlite::params![session, second.seq, clear.seq],
        )
        .unwrap();
        let fork = store
            .fork_session(&session, None, Some(clear.seq))
            .unwrap()
            .unwrap();
        assert_eq!(fork.fork_point_seq, Some(first.seq));
        assert_eq!(
            store.session_plan(&fork.id).unwrap()[0].content,
            "first plan"
        );
        store.set_run_status(&run, "paused").unwrap();
        let resumed = store.create_run(&session).unwrap();
        store.resume_session_plan(&session, &run, &resumed).unwrap();
        let state = store.session_plan_state(&session).unwrap().unwrap();
        assert_eq!(state.run_id.as_deref(), Some(resumed.as_str()));
        assert_eq!(state.origin, "resumed");
        assert!(!state.previous_task);
        assert!(store
            .set_run_plan(&session, &run, &items("stale write"))
            .is_err());
        assert_eq!(
            store.session_plan(&session).unwrap()[0].content,
            "latest plan"
        );
        assert_eq!(
            store.session_plan_state(&session).unwrap().unwrap(),
            state,
            "rejected stale write must have no metadata effects"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn portable_plan_retains_steps_without_foreign_run_authority() {
    let env = credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(&env, vec![]).await;
    let store = Store::open(&fixture.paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    store
        .append_message(&session, "user", "portable plan boundary")
        .unwrap();
    let run = store.create_run(&session).unwrap();
    store
        .set_run_plan(
            &session,
            &run,
            &[formless_protocol::PlanItem {
                content: "portable acceptance step".into(),
                status: "in_progress".into(),
            }],
        )
        .unwrap();
    store.set_run_status(&run, "paused").unwrap();
    let daemon = Arc::new(Daemon::new(fixture.paths.clone()).unwrap());
    let bundle = daemon
        .handle(Request::ExportSession {
            id: session.clone(),
            format: Some("json".into()),
        })
        .await
        .unwrap();
    assert_eq!(bundle["plan"][0]["content"], "portable acceptance step");
    let imported = daemon
        .handle(Request::ImportSession { bundle })
        .await
        .unwrap();
    let imported_id = imported["id"].as_str().unwrap();
    assert_ne!(imported_id, session);
    let state = snapshot(&daemon, imported_id).await;
    assert_eq!(state["plan"][0]["content"], "portable acceptance step");
    assert_eq!(state["plan_state"]["origin"], "imported");
    assert!(state["plan_state"]["run_id"].is_null());
    assert!(
        store.set_run_plan(imported_id, &run, &[]).is_err(),
        "exported foreign Run cannot mutate imported plan"
    );
    assert_eq!(snapshot(&daemon, imported_id).await["plan"], state["plan"]);
    assert!(fixture.seen.lock().unwrap().is_empty());
    daemon.handle(Request::Shutdown).await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn terminal_recap_is_durable_when_both_ipc_subscribers_receive_it() {
    use formless_protocol::{Event, RequestEnvelope, ServerMessage, PROTOCOL_VERSION};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let env = credential_env::EnvGuard::isolated();
    let fixture = Fixture::new(
        &env,
        vec![
            tool(
                "p",
                "update_plan",
                json!({"items":[{"content":"IPC acceptance step","status":"in_progress"}]}),
            ),
            tool(
                "f1",
                "find_tools",
                json!({"query":"private_nonexistent_761"}),
            ),
            tool(
                "f2",
                "find_tools",
                json!({"query":"private_nonexistent_762"}),
            ),
            json!({"content":"IPC complete"}),
        ],
    )
    .await;
    let store = Store::open(&fixture.paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(fixture.paths.clone()).unwrap());
    let server = tokio::spawn({
        let daemon = daemon.clone();
        async move { daemon.serve().await }
    });
    let mut clients = Vec::new();
    for id in 1..=2 {
        let mut stream = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(stream) =
                    tokio::net::UnixStream::connect(fixture.paths.socket_path()).await
                {
                    break stream;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("private IPC listener not published");
        let request = RequestEnvelope {
            id,
            protocol: PROTOCOL_VERSION,
            request: Request::Subscribe {
                sessions: Some(vec![session.clone()]),
                deltas: true,
                client: Some(format!("run-state-acceptance-{id}")),
            },
        };
        stream
            .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
            .await
            .unwrap();
        let mut lines = tokio::io::BufReader::new(stream).lines();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if matches!(serde_json::from_str::<ServerMessage>(&line).unwrap(), ServerMessage::Response {id:actual,ok:true,..} if actual == id)
            {
                break;
            }
        }
        clients.push(lines);
    }
    let ack = send(&daemon, &session, "perform the IPC acceptance task").await;
    let acknowledged_run = ack["run_id"]
        .as_str()
        .expect("started SendMessage must identify its Run");
    for mut lines in clients {
        let (recap_run, recap) = tokio::time::timeout(Duration::from_secs(15), async {
            let mut received = None;
            let mut started = None;
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("private IPC disconnected");
                match serde_json::from_str::<ServerMessage>(&line).unwrap() {
                    ServerMessage::Event(Event::RunStarted { session_id, run_id })
                        if session_id == session =>
                    {
                        assert_eq!(
                            run_id, acknowledged_run,
                            "started event must match SendMessage acknowledgement"
                        );
                        started = Some(run_id);
                    }
                    ServerMessage::Event(Event::RunRecap {
                        session_id,
                        run_id,
                        recap,
                    }) if session_id == session => {
                        assert_eq!(started.as_deref(), Some(acknowledged_run));
                        assert_eq!(run_id, acknowledged_run);
                        let state = snapshot(&daemon, &session).await;
                        assert_eq!(state["plan_state"]["run_id"], acknowledged_run);
                        let recorded = state["last_recap"].clone();
                        assert_eq!(recorded["run_id"], run_id);
                        assert_eq!(
                            recorded["recap"],
                            serde_json::to_value(&recap).unwrap(),
                            "recap must be durable before its IPC broadcast"
                        );
                        assert_eq!(recap.status, "completed");
                        received = Some((run_id, recap));
                    }
                    ServerMessage::Event(Event::RunFinished {
                        session_id,
                        run_id,
                        status,
                    }) if session_id == session => {
                        assert_eq!(status, "completed");
                        let recorded =
                            received.expect("terminal status must follow eligible recap event");
                        assert_eq!(recorded.0, run_id);
                        break recorded;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("subscriber did not receive bounded terminal events");
        assert_eq!(store.last_run(&session).unwrap().unwrap().0, recap_run);
        assert_eq!(recap.steps, 4);
    }
    daemon.handle(Request::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("private daemon did not stop")
        .unwrap()
        .unwrap();
}
