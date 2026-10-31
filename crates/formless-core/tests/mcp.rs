//! MCP stdio servers (SPEC §7.9). The test binary doubles as a fake MCP
//! server: run with FL_FAKE_MCP=1, `fake_mcp_server` speaks JSON-RPC on
//! stdin/stdout instead of testing anything.

#[path = "support/env.rs"]
mod credential_env;

use formless_core::config::{McpCfg, McpServerCfg};
use formless_core::mcp::{call_result, registry_name, split_name, sync_registry, Manager};
use formless_core::{Paths, Store};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

#[test]
#[ignore = "subprocess fixture, invoked explicitly by MCP integration tests"]
fn fake_mcp_server() {
    let _env = credential_env::EnvGuard::isolated();
    assert!(
        std::env::var("FL_FAKE_MCP").is_ok(),
        "fixture requires FL_FAKE_MCP"
    );
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
                    json!({"name": "echo", "description": "Echo text back", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}, "annotations": {"readOnlyHint": true}}),
                    json!({"name": "env", "description": "Read an environment variable", "inputSchema": {"type": "object"}}),
                    json!({"name": "boom", "description": "Always fails", "inputSchema": {"type": "object"}, "annotations": {"destructiveHint": true}}),
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
            "resources/list" => json!({"resources": [
                {"uri": "file:///notes/a.md", "name": "a.md", "description": "Short note", "mimeType": "text/markdown"},
                {"uri": "file:///notes/big.log", "name": "big.log", "description": "Long log"}
            ]}),
            "resources/read" => {
                let uri = msg["params"]["uri"].as_str().unwrap_or_default();
                let text = if uri.ends_with("big.log") {
                    "log line\n".repeat(5000)
                } else {
                    "# Note A".into()
                };
                json!({"contents": [{"uri": uri, "text": text}]})
            }
            "prompts/list" => json!({"prompts": [
                {"name": "review", "description": "Review a file", "arguments": [{"name": "path", "required": true}]}
            ]}),
            "prompts/get" => json!({"messages": [
                {"role": "user", "content": {"type": "text", "text": format!("Please review {}", msg["params"]["arguments"]["path"].as_str().unwrap_or("?"))}}
            ]}),
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
    // A unique profile: keychain entries derive from it, so tests never
    // touch the developer's real "default" ones.
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
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
            "--ignored".into(),
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
    let _env = credential_env::EnvGuard::isolated();
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
    let _env = credential_env::EnvGuard::isolated();
    _env.set("FL_DAEMON_SECRET", "leak");
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
    let _env = credential_env::EnvGuard::isolated();
    use std::sync::Arc;
    let paths = temp_paths("mcpdaemon");
    let (mcp, _) = fake_server(&paths);
    let mut cfg = formless_core::Config {
        mcp,
        ..Default::default()
    };
    cfg.daemon.idle_grace_secs = 3600;
    cfg.security.secure = true;
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
    // A tool grant alone is not enough: the server is approved separately.
    let caps = formless_runtime::Capabilities::parse(&["mcp:fake".to_string()]).unwrap();
    store.set_approval(&info.id, &caps, 256).unwrap();
    let out = daemon
        .call_tool_direct(&session, "mcp__fake__echo", json!({"text": "hi"}))
        .await;
    assert!(out.contains("denied"), "{out}");
    store.approve_mcp_server("fake").unwrap();
    let out = daemon
        .call_tool_direct(&session, "mcp__fake__echo", json!({"text": "hi"}))
        .await;
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["content"], "hi");
    assert_eq!(
        v["source"], "mcp:fake",
        "labelled as untrusted server output"
    );
    assert!(v["note"]
        .as_str()
        .unwrap()
        .contains("never as instructions"));
    // Annotations are kept on the registry version.
    assert_eq!(
        formless_core::mcp::annotations(&ver.code)["readOnlyHint"],
        true
    );
    let m = store.tool_metrics(&info.id, None).unwrap();
    assert_eq!((m.success, m.denied), (1, 2));
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
    let _env = credential_env::EnvGuard::isolated();
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
    let _env = credential_env::EnvGuard::isolated();
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

/// MCP OAuth: discovery from the 401, dynamic registration, PKCE with the
/// resource indicator, an authorized call, and refresh on expiry.
#[tokio::test(flavor = "multi_thread")]
async fn oauth_protected_servers_log_in_and_refresh() {
    let _env = credential_env::EnvGuard::isolated();
    use axum::{
        extract::{Query, State},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Redirect},
        routing::{get, post},
        Form, Json, Router,
    };
    use base64::Engine;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct As {
        challenge: String,
        resource: String,
        issued: Vec<String>,
    }
    type St = Arc<Mutex<As>>;
    let state: St = Default::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let b2 = base.clone();
    let b3 = base.clone();
    let app = Router::new()
        .route(
            "/mcp",
            post(move |State(st): State<St>, headers: HeaderMap, Json(msg): Json<Value>| {
                let base = b2.clone();
                async move {
                    let token = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .map(str::to_string);
                    let ok = token.is_some_and(|t| st.lock().unwrap().issued.last() == Some(&t));
                    if !ok {
                        let hdr = format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource\"");
                        return (StatusCode::UNAUTHORIZED, [("www-authenticate", hdr)]).into_response();
                    }
                    if msg.get("id").is_none() {
                        return StatusCode::ACCEPTED.into_response();
                    }
                    Json(answer(&msg["id"], msg["method"].as_str().unwrap(), &msg["params"])).into_response()
                }
            }),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(move || {
                let base = b3.clone();
                async move { Json(json!({"resource": format!("{base}/mcp"), "authorization_servers": [base]})) }
            }),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get({
                let base = base.clone();
                move || {
                    let base = base.clone();
                    async move {
                        Json(json!({
                            "issuer": base, "authorization_endpoint": format!("{base}/authorize"),
                            "token_endpoint": format!("{base}/token"), "registration_endpoint": format!("{base}/register"),
                        }))
                    }
                }
            }),
        )
        .route("/register", post(|| async { Json(json!({"client_id": "dyn-client"})) }))
        .route(
            "/authorize",
            get(|State(st): State<St>, Query(q): Query<HashMap<String, String>>| async move {
                assert_eq!(q["client_id"], "dyn-client");
                assert_eq!(q["code_challenge_method"], "S256");
                let mut s = st.lock().unwrap();
                s.challenge = q["code_challenge"].clone();
                s.resource = q["resource"].clone();
                Redirect::to(&format!("{}?code=c0de&state={}", q["redirect_uri"], q["state"]))
            }),
        )
        .route(
            "/token",
            post(|State(st): State<St>, Form(f): Form<HashMap<String, String>>| async move {
                let mut s = st.lock().unwrap();
                assert!(f["resource"].ends_with("/mcp"), "resource indicator on token requests");
                if f["grant_type"] == "authorization_code" {
                    let digest = Sha256::digest(f["code_verifier"].as_bytes());
                    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);
                    assert_eq!(challenge, s.challenge, "PKCE verifier matches");
                    assert_eq!(s.resource, f["resource"]);
                } else {
                    assert_eq!(f["refresh_token"], "rt-1");
                }
                let token = format!("at-{}", s.issued.len() + 1);
                s.issued.push(token.clone());
                // The first token is already expired, to exercise refresh.
                let expires_in = if s.issued.len() == 1 { 1 } else { 3600 };
                Json(json!({"access_token": token, "refresh_token": "rt-1", "token_type": "Bearer", "expires_in": expires_in}))
            }),
        )
        .with_state(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let paths = temp_paths("mcpoauth");
    let scfg = McpServerCfg {
        transport: "http".into(),
        url: format!("{base}/mcp"),
        ..Default::default()
    };
    let cfg = McpCfg {
        servers: [("secure".to_string(), scfg.clone())].into(),
        ..Default::default()
    };
    // Not logged in: a clear instruction instead of a hang.
    let err = Manager::default()
        .connection(&paths, &cfg, "secure")
        .await
        .err()
        .unwrap();
    assert!(
        format!("{err:#}").contains("formless mcp login secure"),
        "{err:#}"
    );

    // Log in (blocking, outside Tokio); "the browser" follows the redirect.
    let (p2, s2) = (paths.clone(), scfg.clone());
    let login = std::thread::spawn(move || {
        formless_core::mcp_auth::login(&p2, "secure", &s2, |url| {
            let url = url.to_string();
            std::thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap();
                let to = client.get(&url).send().unwrap().headers()["location"]
                    .to_str()
                    .unwrap()
                    .to_string();
                client.get(&to).send().unwrap();
            });
        })
    });
    tokio::task::spawn_blocking(move || login.join().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(formless_core::mcp_auth::has_login(&paths, "secure"));

    // at-1 expires at once, so the first call refreshes to at-2.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let mcp = Manager::default();
    let r = mcp
        .call(
            &paths,
            &cfg,
            "secure",
            "echo",
            json!({"text": "authorized"}),
        )
        .await
        .unwrap();
    assert_eq!(call_result(&r).unwrap(), json!("authorized"));
    assert_eq!(state.lock().unwrap().issued, ["at-1", "at-2"]);
    formless_core::mcp_auth::logout(&paths, "secure");
    assert!(!formless_core::mcp_auth::has_login(&paths, "secure"));
}

/// Servers are managed through the daemon: add lists tools in, list shows
/// state, restart re-lists, remove retires the tools and edits config.
#[tokio::test(flavor = "multi_thread")]
async fn mcp_servers_are_managed_through_the_daemon() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_protocol::Request;
    use std::sync::Arc;
    let paths = temp_paths("mcpmanage");
    formless_core::Config::default().save(&paths).unwrap();
    let (cfg, _) = fake_server(&paths);
    let server = serde_json::to_value(&cfg.servers["fake"]).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let bad = daemon
        .handle(Request::AddMcpServer {
            name: "a__b".into(),
            server: server.clone(),
        })
        .await;
    assert!(bad.is_err(), "names may not contain '__'");
    let added = daemon
        .handle(Request::AddMcpServer {
            name: "fake".into(),
            server,
        })
        .await
        .unwrap();
    assert_eq!(added["tools"], 4);
    assert!(formless_core::Config::load(&paths)
        .unwrap()
        .mcp
        .servers
        .contains_key("fake"));
    let list = daemon.handle(Request::ListMcpServers).await.unwrap();
    assert_eq!(list[0]["name"], "fake");
    assert_eq!(list[0]["running"], true);
    assert_eq!(list[0]["tool_count"], 4);
    assert_eq!(list[0]["state"], "ready");
    // Per-tool on/off (SPEC §7.9): an off tool leaves the registry.
    let tools = daemon
        .handle(Request::ListMcpTools {
            name: "fake".into(),
        })
        .await
        .unwrap();
    let all = tools["tools"].as_array().unwrap();
    assert_eq!(all.len(), 4);
    assert!(all
        .iter()
        .all(|t| t["enabled"] == true && t["tokens"].as_u64().unwrap() > 0));
    let before = tools["enabled_tokens"].as_u64().unwrap();
    daemon
        .handle(Request::SetMcpToolEnabled {
            name: "fake".into(),
            tool: "echo".into(),
            enabled: false,
        })
        .await
        .unwrap();
    let list = daemon.handle(Request::ListMcpServers).await.unwrap();
    assert_eq!(list[0]["tool_count"], 3);
    assert_eq!(list[0]["disabled_tools"], json!(["echo"]));
    let tools = daemon
        .handle(Request::ListMcpTools {
            name: "fake".into(),
        })
        .await
        .unwrap();
    assert!(tools["enabled_tokens"].as_u64().unwrap() < before);
    daemon
        .handle(Request::SetMcpToolEnabled {
            name: "fake".into(),
            tool: "echo".into(),
            enabled: true,
        })
        .await
        .unwrap();
    // The stderr log is readable (empty is fine for this server).
    let log = daemon
        .handle(Request::McpServerLog {
            name: "fake".into(),
            lines: Some(10),
        })
        .await
        .unwrap();
    assert!(log["lines"].is_array());
    let restarted = daemon
        .handle(Request::RestartMcpServer {
            name: "fake".into(),
        })
        .await
        .unwrap();
    assert_eq!(restarted["tools"], 4);
    let removed = daemon
        .handle(Request::RemoveMcpServer {
            name: "fake".into(),
        })
        .await
        .unwrap();
    assert_eq!(removed["retired_tools"], 4);
    let store = Store::open(&paths.db_file()).unwrap();
    assert!(
        store
            .get_tool_by_name("mcp__fake__echo")
            .unwrap()
            .unwrap()
            .0
            .retired
    );
    assert!(!formless_core::Config::load(&paths)
        .unwrap()
        .mcp
        .servers
        .contains_key("fake"));
    assert_eq!(
        daemon.handle(Request::ListMcpServers).await.unwrap(),
        json!([])
    );
}

/// Trusted servers' read-only tools run without a per-tool approval; any
/// other tool (here destructive) still needs one.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn trusted_read_only_mcp_tools_skip_per_tool_approval() {
    let _env = credential_env::EnvGuard::isolated();
    use std::sync::Arc;
    let paths = temp_paths("mcptrust");
    let (mut mcp, _) = fake_server(&paths);
    mcp.servers.get_mut("fake").unwrap().trusted = true;
    let mut cfg = formless_core::Config {
        mcp,
        ..Default::default()
    };
    cfg.security.secure = true;
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    daemon
        .handle(formless_protocol::Request::RestartMcpServer {
            name: "fake".into(),
        })
        .await
        .unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    store.approve_mcp_server("fake").unwrap();
    let session = store.create_session(None).unwrap().id;
    let out = daemon
        .call_tool_direct(&session, "mcp__fake__echo", json!({"text": "free"}))
        .await;
    assert!(out.contains("free"), "read-only on a trusted server: {out}");
    let out = daemon
        .call_tool_direct(&session, "mcp__fake__boom", json!({}))
        .await;
    assert!(
        out.contains("denied"),
        "destructive still asks (and fails closed): {out}"
    );
}

/// Resources (agent built-ins) and prompts (protocol, for slash commands).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn mcp_resources_and_prompts() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_protocol::Request;
    use std::sync::Arc;
    let paths = temp_paths("mcpres");
    let (mcp, _) = fake_server(&paths);
    formless_core::Config {
        mcp,
        ..Default::default()
    }
    .save(&paths)
    .unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let store = Store::open(&paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    // Not approved yet: headless, the server stays closed.
    let out = daemon
        .call_tool_direct(
            &session,
            "read_mcp_resource",
            json!({"server": "fake", "uri": "file:///notes/a.md"}),
        )
        .await;
    assert!(out.contains("did not approve"), "{out}");
    store.approve_mcp_server("fake").unwrap();
    let list: Value = serde_json::from_str(
        &daemon
            .call_tool_direct(&session, "list_mcp_resources", json!({"query": "short"}))
            .await,
    )
    .unwrap();
    assert_eq!(list["resources"][0]["uri"], "file:///notes/a.md");
    assert_eq!(
        list["resources"].as_array().unwrap().len(),
        1,
        "query narrows"
    );
    let small = daemon
        .call_tool_direct(
            &session,
            "read_mcp_resource",
            json!({"server": "fake", "uri": "file:///notes/a.md"}),
        )
        .await;
    assert!(
        small.contains("# Note A") && small.contains("mcp:fake"),
        "{small}"
    );
    let big = daemon
        .call_tool_direct(
            &session,
            "read_mcp_resource",
            json!({"server": "fake", "uri": "file:///notes/big.log"}),
        )
        .await;
    assert!(
        big.contains("stored as artifact"),
        "large contents go to an artifact: {}",
        &big[..200.min(big.len())]
    );
    let prompts = daemon.handle(Request::ListMcpPrompts).await.unwrap();
    assert_eq!(prompts[0]["name"], "review");
    let text = daemon
        .handle(Request::GetMcpPrompt {
            server: "fake".into(),
            name: "review".into(),
            arguments: [("path".to_string(), json!("src/main.rs"))]
                .into_iter()
                .collect(),
            positional: vec![],
        })
        .await
        .unwrap();
    assert_eq!(text["text"], "Please review src/main.rs");
    // `/mcp:fake:review lib.rs`: positional values fill declared arguments.
    let text = daemon
        .handle(Request::GetMcpPrompt {
            server: "fake".into(),
            name: "review".into(),
            arguments: Default::default(),
            positional: vec!["lib.rs".into()],
        })
        .await
        .unwrap();
    assert_eq!(text["text"], "Please review lib.rs");
    let too_many = daemon
        .handle(Request::GetMcpPrompt {
            server: "fake".into(),
            name: "review".into(),
            arguments: Default::default(),
            positional: vec!["a".into(), "b".into()],
        })
        .await;
    assert!(too_many.is_err());
}

/// When a server drops a tool, the registry retires it at once, but a
/// Session that listed it keeps the same listing until its next refresh
/// point (compaction), so its cached prefix survives.
#[test]
fn dropped_mcp_tools_leave_session_listings_alone_until_compaction() {
    let _env = credential_env::EnvGuard::isolated();
    use formless_core::mcp::McpTool;
    use formless_core::tools::tool_definitions;
    let store = Store::open_in_memory().unwrap();
    let tool = |name: &str| McpTool {
        name: name.into(),
        description: format!("{name} tool"),
        input_schema: json!({"type": "object"}),
        annotations: Value::Null,
    };
    sync_registry(&store, "srv", &[tool("alpha"), tool("beta"), tool("gamma")]).unwrap();
    let session = store.create_session(None).unwrap().id;
    let before = tool_definitions(&store, Some(&session), 40).unwrap();
    // The server drops beta and adds delta.
    sync_registry(
        &store,
        "srv",
        &[tool("alpha"), tool("gamma"), tool("delta")],
    )
    .unwrap();
    assert!(
        store
            .get_tool_by_name("mcp__srv__beta")
            .unwrap()
            .unwrap()
            .0
            .retired
    );
    assert_eq!(
        tool_definitions(&store, Some(&session), 40).unwrap(),
        before,
        "byte-identical listing mid-session"
    );
    // A new Session sees the current set.
    let fresh = store.create_session(None).unwrap().id;
    let names = |defs: Vec<Value>| -> Vec<String> {
        defs.iter()
            .filter_map(|d| d["function"]["name"].as_str())
            .filter(|n| n.starts_with("mcp__"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        names(tool_definitions(&store, Some(&fresh), 40).unwrap()),
        ["mcp__srv__alpha", "mcp__srv__delta", "mcp__srv__gamma"]
    );
    // At compaction the Session refreshes: beta goes (delta is reached via find_tools).
    store.unpin_definitions(&session, None).unwrap();
    assert_eq!(
        names(tool_definitions(&store, Some(&session), 40).unwrap()),
        ["mcp__srv__alpha", "mcp__srv__gamma"]
    );
}

/// MCP calls are measured with error classes, and a tool that keeps
/// failing is flagged in find_tools.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn mcp_calls_are_measured_with_error_classes() {
    let _env = credential_env::EnvGuard::isolated();
    use std::sync::Arc;
    let paths = temp_paths("mcpmetrics");
    let (mcp, _) = fake_server(&paths);
    let mut cfg = formless_core::Config {
        mcp,
        ..Default::default()
    };
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    daemon
        .handle(formless_protocol::Request::RestartMcpServer {
            name: "fake".into(),
        })
        .await
        .unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    store.approve_mcp_server("fake").unwrap();
    let (boom, _) = store.get_tool_by_name("mcp__fake__boom").unwrap().unwrap();
    let caps = formless_runtime::Capabilities::parse(&["mcp:fake".to_string()]).unwrap();
    store.set_approval(&boom.id, &caps, 256).unwrap();
    let session = store.create_session(None).unwrap().id;
    for _ in 0..5 {
        let out = daemon
            .call_tool_direct(&session, "mcp__fake__boom", json!({}))
            .await;
        assert!(out.contains("it broke"), "{out}");
    }
    let fails = store.recent_failures(&boom.id, 10).unwrap();
    assert!(fails.iter().all(|f| f.1 == "mcp-tool-error"), "{fails:?}");
    let found: Value = serde_json::from_str(
        &daemon
            .call_tool_direct(&session, "find_tools", json!({"query": "always fails"}))
            .await,
    )
    .unwrap();
    let hit = found["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "mcp__fake__boom")
        .unwrap()
        .clone();
    assert!(
        hit["warning"].as_str().unwrap().contains("unreliable"),
        "{hit}"
    );
    // A server that cannot start is a transport failure.
    cfg.mcp.servers.get_mut("fake").unwrap().command = "/no/such/server".into();
    cfg.save(&paths).unwrap();
    daemon
        .handle(formless_protocol::Request::RestartMcpServer {
            name: "fake".into(),
        })
        .await
        .unwrap();
    daemon
        .call_tool_direct(&session, "mcp__fake__boom", json!({}))
        .await;
    let fails = store.recent_failures(&boom.id, 1).unwrap();
    assert_eq!(fails[0].1, "mcp-transport", "{fails:?}");
}
