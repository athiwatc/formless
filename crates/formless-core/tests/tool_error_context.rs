//! Readable errors use per-Tool context and redact the same detail before
//! returning it or recording the Invocation (#1004, #1005).
#[path = "support/env.rs"]
mod credential_env;

use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::{Request, SendMode};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn create(daemon: &Arc<Daemon>, session: &str, name: &str, code: &str) {
    let (result, ok) = daemon
        .call_tool_direct_outcome(
            session,
            "create_tool",
            json!({
                "name":name, "description":"Exercise readable failure context",
                "input_schema":{"type":"object"}, "output_schema":{"type":"object"},
                "implementation_js":code, "capabilities":[]
            }),
        )
        .await;
    assert!(ok && result.contains("\"created\""), "{name}: {result}");
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_errors_preserve_inner_detail_and_redact_recorded_and_returned_inputs() {
    let env = credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let base =
        Fixture(std::env::temp_dir().join(format!("fl-error-context-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(base.0.join("d"), base.0.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.security
        .secret_patterns
        .push("fixture-secret-[A-Z]+".into());
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let inner = "function explode(input) {\n  const marker = 'INNER_CONTEXT';\n  if (input.fail) {\n    throw new Error('INNER_FAILURE');\n  }\n  return {ok:true};\n}\nfunction run(input) { return explode(input); }";
    let outer = "async function run(input) {\n  const marker = 'OUTER_CONTEXT';\n  if (!input.fail) return {ok:true};\n  return await tools.call('inner_diagnostic', {fail:true, inner_visible:'inner marker', api_key:input.api_key, password:'pw'});\n}";
    create(&daemon, &session, "inner_diagnostic", inner).await;
    create(&daemon, &session, "outer_diagnostic", outer).await;
    let secret = "sk-abcdefghijklmnop1234567890";
    let escaped = "short\"escaped\\credential";
    let input = json!({"fail":true,"outer_visible":"outer marker","api_key":secret,
        "nested":[{"headers":{"Authorization":"pw"},"refreshToken":escaped,"passphrase":"pp!","pwd":"p!"}],
        "payload":"fixture-secret-OPAQUE", "ordinary":"keep \"quoted\" text"});
    let (returned, ok) = daemon
        .call_tool_direct_outcome(&session, "outer_diagnostic", input.clone())
        .await;
    assert!(!ok, "nested failure counted as success");
    let conn =
        Connection::open_with_flags(paths.db_file(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut statement = conn.prepare("SELECT t.name,i.error,i.input FROM invocations i JOIN tools t ON t.id=i.tool_id WHERE i.status='failure' ORDER BY t.name").unwrap();
    let failures: Vec<(String, String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(failures.len(), 2);
    let inner_detail = &failures
        .iter()
        .find(|row| row.0 == "inner_diagnostic")
        .unwrap()
        .1;
    let outer_detail = &failures
        .iter()
        .find(|row| row.0 == "outer_diagnostic")
        .unwrap()
        .1;
    assert!(
        returned.contains(inner_detail),
        "inner detail was not retained verbatim"
    );
    assert!(
        returned.contains(outer_detail),
        "recorded and returned details diverged"
    );
    assert!(
        outer_detail.starts_with("Tool call chain: outer_diagnostic → inner_diagnostic\n"),
        "{outer_detail}"
    );
    assert!(inner_detail.contains("explode (line 4)"), "{inner_detail}");
    assert!(
        inner_detail.contains("> 4 |     throw new Error('INNER_FAILURE');"),
        "{inner_detail}"
    );
    assert!(
        outer_detail.contains("> 4 |   return await tools.call("),
        "{outer_detail}"
    );
    let own = outer_detail.strip_suffix(inner_detail).unwrap();
    assert!(own.contains("outer_visible"));
    assert!(!own.contains("inner_visible"));
    assert!(inner_detail.contains("inner_visible"));
    assert!(!inner_detail.contains("outer_visible"));
    for text in std::iter::once(&returned).chain(failures.iter().flat_map(|row| [&row.1, &row.2])) {
        for secret in [secret, escaped, "fixture-secret-OPAQUE"] {
            assert!(
                !text.contains(secret),
                "credential appeared in failure surface"
            );
        }
    }
    let recorded: Value = serde_json::from_str(
        &failures
            .iter()
            .find(|row| row.0 == "outer_diagnostic")
            .unwrap()
            .2,
    )
    .unwrap();
    assert_eq!(recorded["api_key"], "[redacted]");
    assert_eq!(
        recorded["nested"][0]["headers"]["Authorization"],
        "[redacted]"
    );
    assert_eq!(recorded["nested"][0]["passphrase"], "[redacted]");
    assert_eq!(recorded["nested"][0]["pwd"], "[redacted]");
    drop(statement);
    drop(conn);
    // Exercise the actual agent/provider boundary too: the second mock HTTP
    // request sees the same redacted Tool result that was recorded locally.
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let posts = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route("/chat/completions", axum::routing::post({
        let seen = seen.clone();
        move |axum::Json(body): axum::Json<Value>| {
            let seen = seen.clone();
            let posts = posts.clone();
            let input = input.clone();
            async move {
                for message in body["messages"].as_array().into_iter().flatten() {
                    if message["role"]=="tool" {
                        if let Some(content)=message["content"].as_str() {
                            seen.lock().unwrap().push(content.to_string());
                        }
                    }
                }
                let chunks = if posts.fetch_add(1,Ordering::SeqCst)==0 {
                    vec![json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"error-fixture-call","type":"function","function":{"name":"outer_diagnostic","arguments":input.to_string()}}]},"finish_reason":null}]}),
                         json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})]
                } else {
                    vec![json!({"choices":[{"delta":{"content":"Checked the failure"},"finish_reason":"stop"}]})]
                };
                let events = chunks.into_iter().map(|value| axum::response::sse::Event::default().data(value.to_string()))
                    .chain(std::iter::once(axum::response::sse::Event::default().data("[DONE]")))
                    .map(Ok::<_,std::convert::Infallible>);
                axum::response::Sse::new(futures_util::stream::iter(events))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.provider.model = "error-fixture".into();
    cfg.provider.max_retries = 0;
    cfg.save(&paths).unwrap();
    env.set("FORMLESS_API_KEY", "test-only-provider-key");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    daemon
        .handle(Request::SendMessage {
            session_id: session.clone(),
            content: "Exercise the diagnostic Tool".into(),
            mode: SendMode::Send,
            client_msg_id: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if store
                .get_session(&session)
                .unwrap()
                .unwrap()
                .last_run_status
                .as_deref()
                == Some("completed")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("mock-provider diagnostic Run did not complete");
    let model_results = seen.lock().unwrap();
    assert!(
        model_results
            .iter()
            .any(|text| text.contains("Tool call chain: outer_diagnostic → inner_diagnostic")),
        "mock provider saw no nested diagnostic result"
    );
    for text in model_results.iter() {
        for secret in [secret, escaped, "fixture-secret-OPAQUE"] {
            assert!(
                !text.contains(secret),
                "credential appeared in model-visible result"
            );
        }
    }
    drop(model_results);
    server.abort();
    let _ = server.await;
    drop(daemon);
    drop(store);
}

#[tokio::test(flavor = "multi_thread")]
async fn required_module_frames_do_not_render_against_tool_source() {
    let env = credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    let base =
        Fixture(std::env::temp_dir().join(format!("fl-module-context-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(base.0.join("d"), base.0.join("c")).unwrap();
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let (created, ok) = daemon.call_tool_direct_outcome(&session,"create_module",json!({
        "name":"failure_module","description":"Throw at a module-owned source location",
        "implementation_js":"module.exports.explodeModule = function explodeModule() {\n  throw new Error('MODULE_FAILURE');\n};"
    })).await;
    assert!(ok, "{created}");
    create(&daemon,&session,"module_diagnostic", "function run(input) {\n  const wrong = 'TOOL_LINE_TWO_IS_NOT_MODULE_SOURCE';\n  const library = requireModule('failure_module');\n  if (input.fail) return library.explodeModule();\n  return {ok:true};\n}").await;
    let (returned, ok) = daemon
        .call_tool_direct_outcome(&session, "module_diagnostic", json!({"fail":true}))
        .await;
    assert!(!ok);
    assert!(returned.contains("MODULE_FAILURE"), "{returned}");
    assert!(!returned.contains("explodeModule (line 2)"), "{returned}");
    assert!(
        !returned.contains("> 2 |"),
        "module line mapped to Tool line two: {returned}"
    );
    assert!(
        returned.contains("> 4 |   if (input.fail) return library.explodeModule();"),
        "{returned}"
    );
    drop(daemon);
    drop(store);
}
