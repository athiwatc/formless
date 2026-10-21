//! MCP stdio servers (SPEC §7.9). The test binary doubles as a fake MCP
//! server: run with FL_FAKE_MCP=1, `fake_mcp_server` speaks JSON-RPC on
//! stdin/stdout instead of testing anything.

use formless_core::config::{McpCfg, McpServerCfg};
use formless_core::mcp::{call_result, registry_name, split_name, sync_registry, Manager};
use formless_core::{Paths, Store};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

#[test]
fn fake_mcp_server() {
    if std::env::var("FL_FAKE_MCP").is_err() {
        return;
    }
    let log = std::env::var("FL_FAKE_LOG").unwrap_or_default();
    let out = std::sync::Arc::new(std::sync::Mutex::new(std::io::stdout()));
    let send = {
        let out = out.clone();
        move |v: Value| {
            let mut o = out.lock().unwrap();
            let _ = writeln!(o, "{v}");
            let _ = o.flush();
        }
    };
    let mut extra = false;
    for line in std::io::stdin().lock().lines() {
        let Ok(msg) = serde_json::from_str::<Value>(&line.unwrap()) else {
            continue;
        };
        let id = msg["id"].clone();
        let result = match msg["method"].as_str().unwrap_or_default() {
            "initialize" => {
                json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {"listChanged": true}}, "serverInfo": {"name": "fake"}})
            }
            "tools/list" => {
                let mut tools = vec![
                    json!({"name": "echo", "description": "Echo text back", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}),
                    json!({"name": "env", "description": "Read an environment variable", "inputSchema": {"type": "object"}}),
                    json!({"name": "boom", "description": "Always fails", "inputSchema": {"type": "object"}}),
                    json!({"name": "slow", "description": "Takes forever", "inputSchema": {"type": "object"}}),
                ];
                if extra {
                    tools.push(json!({"name": "extra.tool", "description": "Added later", "inputSchema": {"type": "object"}}));
                }
                json!({"tools": tools})
            }
            "tools/call" => {
                let args = &msg["params"]["arguments"];
                match msg["params"]["name"].as_str().unwrap_or_default() {
                    "echo" => {
                        if args["text"] == "change-tools" {
                            extra = true;
                            send(
                                json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
                            );
                        }
                        json!({"content": [{"type": "text", "text": args["text"]}]})
                    }
                    "env" => {
                        let key = args["key"].as_str().unwrap_or_default();
                        json!({"content": [{"type": "text", "text": std::env::var(key).unwrap_or_else(|_| "<unset>".into())}]})
                    }
                    "boom" => {
                        json!({"isError": true, "content": [{"type": "text", "text": "it broke"}]})
                    }
                    "slow" => {
                        let send = send.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_secs(30));
                            send(json!({"jsonrpc": "2.0", "id": id, "result": {"content": []}}));
                        });
                        continue;
                    }
                    _ => json!({"isError": true, "content": [{"type": "text", "text": "unknown"}]}),
                }
            }
            "notifications/cancelled" => {
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log)
                    .and_then(|mut f| writeln!(f, "cancelled {}", msg["params"]["requestId"]));
                continue;
            }
            _ => continue, // notifications
        };
        send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }
    std::process::exit(0);
}

pub fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

/// Config for the fake server (this test binary, re-run as a server).
pub fn fake_server(paths: &Paths) -> (McpCfg, std::path::PathBuf) {
    let log = paths.data_dir.join("fake.log");
    let server = McpServerCfg {
        command: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: vec![
            "--exact".into(),
            "fake_mcp_server".into(),
            "--nocapture".into(),
        ],
        env: [
            ("FL_FAKE_MCP".to_string(), "1".to_string()),
            (
                "FL_FAKE_LOG".to_string(),
                log.to_string_lossy().into_owned(),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let cfg = McpCfg {
        servers: [("fake".to_string(), server)].into(),
        ..Default::default()
    };
    (cfg, log)
}

#[test]
fn registry_names_round_trip_and_are_function_safe() {
    let n = registry_name("my server", "do.thing");
    assert_eq!(n, "mcp__my_server__do_thing");
    assert_eq!(split_name(&n), Some(("my_server", "do_thing")));
    assert!(registry_name("s", &"x".repeat(100)).len() <= 64);
    assert_eq!(
        call_result(&json!({"content": [{"type": "text", "text": "hi"}]})).unwrap(),
        json!("hi")
    );
    assert_eq!(
        call_result(&json!({"structuredContent": {"a": 1}, "content": []})).unwrap(),
        json!({"a": 1})
    );
    assert_eq!(
        call_result(&json!({"isError": true, "content": [{"type": "text", "text": "no"}]}))
            .unwrap_err(),
        "no"
    );
}

#[tokio::test]
async fn stdio_servers_list_call_cancel_restart_and_stay_isolated() {
    std::env::set_var("FL_DAEMON_SECRET", "leak");
    let paths = temp_paths("mcpstdio");
    let (cfg, log) = fake_server(&paths);
    let mcp = Manager::default();

    let conn = mcp.connection(&paths, &cfg, "fake").await.unwrap();
    let names: Vec<String> = conn
        .list_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, ["echo", "env", "boom", "slow"]);
    let r = mcp
        .call(&paths, &cfg, "fake", "echo", json!({"text": "hi"}))
        .await
        .unwrap();
    assert_eq!(call_result(&r).unwrap(), json!("hi"));
    let r = mcp
        .call(&paths, &cfg, "fake", "boom", json!({}))
        .await
        .unwrap();
    assert_eq!(call_result(&r).unwrap_err(), "it broke");

    // The daemon's environment does not leak; declared values do.
    let env = |key: &str| {
        let (mcp, paths, cfg) = (&mcp, &paths, &cfg);
        let key = key.to_string();
        async move {
            call_result(
                &mcp.call(paths, cfg, "fake", "env", json!({"key": key}))
                    .await
                    .unwrap(),
            )
            .unwrap()
        }
    };
    assert_eq!(env("FL_DAEMON_SECRET").await, json!("<unset>"));
    assert_eq!(env("FL_FAKE_MCP").await, json!("1"));

    // Cancelling a call tells the server.
    let slow = mcp.call(&paths, &cfg, "fake", "slow", json!({}));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), slow)
            .await
            .is_err()
    );
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let logged = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logged.contains("cancelled"),
        "server saw the cancellation: {logged:?}"
    );

    // A dead server is started again on the next call.
    conn.stop();
    let r = mcp
        .call(&paths, &cfg, "fake", "echo", json!({"text": "back"}))
        .await
        .unwrap();
    assert_eq!(call_result(&r).unwrap(), json!("back"));

    // tools/list_changed is noticed, and sync adds/retires registry rows.
    let store = Store::open(&paths.db_file()).unwrap();
    let conn = mcp.connection(&paths, &cfg, "fake").await.unwrap();
    sync_registry(&store, "fake", &conn.list_tools().await.unwrap()).unwrap();
    mcp.call(
        &paths,
        &cfg,
        "fake",
        "echo",
        json!({"text": "change-tools"}),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(mcp.changed_servers().await, ["fake"]);
    let tools = conn.list_tools().await.unwrap();
    sync_registry(&store, "fake", &tools).unwrap();
    let (extra, v) = store
        .get_tool_by_name("mcp__fake__extra_tool")
        .unwrap()
        .unwrap();
    assert_eq!(
        formless_core::mcp::server_tool_name(&v.code),
        Some("extra.tool")
    );
    assert!(!extra.retired);
    sync_registry(&store, "fake", &tools[..1]).unwrap();
    assert!(
        store
            .get_tool_by_name("mcp__fake__slow")
            .unwrap()
            .unwrap()
            .0
            .retired
    );
    mcp.stop_all().await;
}

/// End to end: the daemon lists a configured server's tools into the
/// registry at start, and an approved call runs on the server and is
/// recorded as an Invocation.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn daemon_registers_and_runs_mcp_tools() {
    use std::sync::Arc;
    let paths = temp_paths("mcpdaemon");
    let (mcp, _) = fake_server(&paths);
    let mut cfg = formless_core::Config {
        mcp,
        ..Default::default()
    };
    cfg.daemon.idle_grace_secs = 3600;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    let store = Store::open(&paths.db_file()).unwrap();
    let mut echo = None;
    for _ in 0..100 {
        echo = store.get_tool_by_name("mcp__fake__echo").unwrap();
        if echo.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (info, ver) = echo.expect("registry synced at start");
    assert_eq!(ver.capabilities, ["mcp:fake"]);
    // MCP tools are sensitive: without an approver the call fails closed.
    let session = store.create_session(None).unwrap().id;
    let denied = daemon
        .call_tool_direct(&session, "mcp__fake__echo", json!({"text": "hi"}))
        .await;
    assert!(denied.contains("denied"), "{denied}");
    let caps = formless_runtime::Capabilities::parse(&["mcp:fake".to_string()]).unwrap();
    store.set_approval(&info.id, &caps, 256).unwrap();
    let out = daemon
        .call_tool_direct(&session, "mcp__fake__echo", json!({"text": "hi"}))
        .await;
    assert!(out.contains("hi"), "{out}");
    let m = store.tool_metrics(&info.id, None).unwrap();
    assert_eq!((m.success, m.denied), (1, 1));
    // Agents cannot create Tools in the MCP namespace.
    assert!(formless_core::tools::validate_name("mcp__fake__mine").is_err());
    daemon
        .handle(formless_protocol::Request::Shutdown)
        .await
        .unwrap();
    let _ = server.await;
}

fn answer(id: &Value, method: &str, params: &Value) -> Value {
    let result = match method {
        "initialize" => {
            json!({"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "http"}})
        }
        "tools/list" => {
            json!({"tools": [{"name": "echo", "description": "Echo", "inputSchema": {"type": "object"}}]})
        }
        "tools/call" => json!({"content": [{"type": "text", "text": params["arguments"]["text"]}]}),
        _ => json!({}),
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Streamable HTTP: JSON and SSE answers, session id carried, static
/// headers sent, transient 503s retried.
#[tokio::test]
async fn streamable_http_servers_work() {
    use axum::{http::HeaderMap, response::IntoResponse, routing::post, Json, Router};
    let failures = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new().route(
        "/mcp",
        post(move |headers: HeaderMap, Json(msg): Json<Value>| {
            let failures = failures.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer t0k");
                let method = msg["method"].as_str().unwrap_or_default().to_string();
                if msg.get("id").is_none() {
                    return axum::http::StatusCode::ACCEPTED.into_response();
                }
                if method != "initialize" {
                    assert_eq!(headers["mcp-session-id"], "sess-1", "session id is carried");
                }
                if method == "tools/call"
                    && failures.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                {
                    return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                let reply = answer(&msg["id"], &method, &msg["params"]);
                if method == "tools/list" {
                    // As an SSE body, with a server notice before the answer.
                    let body = format!(
                        "event: message\ndata: {}\n\nevent: message\ndata: {}\n\n",
                        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
                        reply
                    );
                    return ([("content-type", "text/event-stream")], body).into_response();
                }
                ([("mcp-session-id", "sess-1")], Json(reply)).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let server = McpServerCfg {
        transport: "http".into(),
        url,
        headers: [("authorization".to_string(), "Bearer t0k".to_string())].into(),
        ..Default::default()
    };
    let cfg = McpCfg {
        servers: [("web".to_string(), server)].into(),
        ..Default::default()
    };
    let paths = temp_paths("mcphttp");
    let mcp = Manager::default();
    let conn = mcp.connection(&paths, &cfg, "web").await.unwrap();
    assert_eq!(conn.list_tools().await.unwrap()[0].name, "echo");
    assert_eq!(
        mcp.changed_servers().await,
        ["web"],
        "notice inside the SSE body"
    );
    let r = mcp
        .call(&paths, &cfg, "web", "echo", json!({"text": "over http"}))
        .await
        .unwrap();
    assert_eq!(call_result(&r).unwrap(), json!("over http"), "503 retried");
}

/// Legacy HTTP+SSE: answers arrive on the GET event stream after the
/// `endpoint` event; messages are POSTed to the announced endpoint.
#[tokio::test]
async fn legacy_sse_servers_work() {
    use axum::{
        extract::State,
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use tokio::sync::mpsc;
    type Tx = std::sync::Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<String>>>>;
    let tx: Tx = Default::default();
    let app = Router::new()
        .route(
            "/mcp",
            post(|| async { axum::http::StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route(
            "/mcp",
            get(|State(tx): State<Tx>| async move {
                let (send, recv) = mpsc::unbounded_channel::<String>();
                *tx.lock().await = Some(send.clone());
                send.send("event: endpoint\ndata: /messages?session=1\n\n".into())
                    .unwrap();
                let stream = futures_util::stream::unfold(recv, |mut r| async move {
                    r.recv().await.map(|m| (Ok::<_, std::io::Error>(m), r))
                });
                (
                    [("content-type", "text/event-stream")],
                    axum::body::Body::from_stream(stream),
                )
                    .into_response()
            }),
        )
        .route(
            "/messages",
            post(|State(tx): State<Tx>, Json(msg): Json<Value>| async move {
                if msg.get("id").is_some() && msg.get("method").is_some() {
                    let reply = answer(&msg["id"], msg["method"].as_str().unwrap(), &msg["params"]);
                    if let Some(tx) = tx.lock().await.as_ref() {
                        let _ = tx.send(format!("event: message\ndata: {reply}\n\n"));
                    }
                }
                axum::http::StatusCode::ACCEPTED
            }),
        )
        .with_state(tx);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cfg = McpCfg {
        servers: [(
            "old".to_string(),
            McpServerCfg {
                transport: "http".into(),
                url,
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    };
    let paths = temp_paths("mcpsse");
    let mcp = Manager::default();
    let conn = mcp.connection(&paths, &cfg, "old").await.unwrap();
    assert_eq!(conn.list_tools().await.unwrap()[0].name, "echo");
    let r = mcp
        .call(&paths, &cfg, "old", "echo", json!({"text": "legacy"}))
        .await
        .unwrap();
    assert_eq!(call_result(&r).unwrap(), json!("legacy"));
}
