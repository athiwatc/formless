//! v2.2 built-ins and host primitives (SPEC §7.1, §7.5): honest outcomes,
//! fair smoke tests, process/fs/fetch primitives with binary data, async
//! Tools, and lazy Tool loading via find_tools/read_tool/test_tool.

use formless_core::daemon::Daemon;
use formless_core::storage::Store;
use formless_core::tools as toolreg;
use formless_core::{Config, Paths};
use formless_runtime::{
    execute, Capabilities, ExecError, ExecErrorKind, ExecRequest, HostMode, NoHostOps,
};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn exec(code: &str, input: Value, caps: &[&str], mode: HostMode) -> Result<Value, ExecError> {
    let req = ExecRequest {
        code: code.to_string(),
        input,
        caps: Capabilities::parse(&caps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
        memory_mb: 64,
        mode,
        overlay: None,
        activity: None,
        modules: Default::default(),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    execute(&req, Arc::new(NoHostOps)).map(|o| o.output)
}

fn live(code: &str, input: Value, caps: &[&str]) -> Result<Value, ExecError> {
    exec(code, input, caps, HostMode::Live)
}

fn temp_dir(tag: &str) -> String {
    let d = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "fl-{tag}-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().into_owned()
}

#[test]
fn returned_error_counts_as_failure() {
    let e = live(
        "function run(){ return {error: 'disk on fire'}; }",
        json!({}),
        &[],
    )
    .unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::Js);
    assert!(e.message.contains("disk on fire"));
    let ok = live(
        "function run(){ return {error: null, value: 1}; }",
        json!({}),
        &[],
    )
    .unwrap();
    assert_eq!(ok["value"], 1);
}

#[test]
fn async_run_resolves_and_rejects() {
    let out = live(
        "async function run(i){ const x = await Promise.resolve(i.n); return {n: x + 1}; }",
        json!({"n": 1}),
        &[],
    )
    .unwrap();
    assert_eq!(out["n"], 2);
    let e = live(
        "async function run(){ throw new Error('nope'); }",
        json!({}),
        &[],
    )
    .unwrap_err();
    assert!(e.message.contains("nope"));
    let e = live(
        "function run(){ return new Promise(() => {}); }",
        json!({}),
        &[],
    )
    .unwrap_err();
    assert!(e.message.contains("never settled"));
}

#[cfg(unix)]
#[test]
fn process_scope_matches_the_real_binary_only() {
    let dir = temp_dir("proc");
    let fake = format!("{dir}/ls");
    std::fs::copy("/bin/ls", &fake).unwrap();
    let call = |cmd: &str| {
        live(
            &format!("function run(){{ return process.run({cmd:?}, ['/']).code; }}"),
            json!({}),
            &["process:ls"],
        )
    };
    assert_eq!(call("ls").unwrap(), 0);
    let e = call(&fake).unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::Denied, "{}", e.message);
}

#[cfg(unix)]
#[test]
fn process_run_takes_cwd_stdin_env_and_binary_output() {
    let dir = temp_dir("procopt");
    let out = live(
        &format!(
            "function run(){{
               const a = process.run('cat', [], {{stdin: 'piped in'}});
               const b = process.run('pwd', [], {{cwd: {dir:?}}});
               const c = process.run('printenv', ['FL_X'], {{env: {{FL_X: 'yes'}}}});
               const d = process.run('printf', ['\\\\001\\\\002'], {{encoding: 'base64'}});
               return {{a: a.stdout, b: b.stdout.trim(), c: c.stdout.trim(), d: d.stdout}};
             }}"
        ),
        json!({}),
        &["process"],
    )
    .unwrap();
    assert_eq!(out["a"], "piped in");
    assert_eq!(out["b"], dir);
    assert_eq!(out["c"], "yes");
    assert_eq!(out["d"], "AQI=");
}

#[test]
fn fs_primitives_cover_binary_nested_and_metadata() {
    let dir = temp_dir("fsops");
    let w = format!("fs:write:{dir}");
    let r = format!("fs:read:{dir}");
    let out = live(
        &format!(
            "function run(){{
               const d = {dir:?};
               fs.write(d + '/new/deep/a.bin', 'AAEC/w==', {{encoding: 'base64'}});
               fs.append(d + '/log.txt', 'one\\n');
               fs.append(d + '/log.txt', 'two\\n');
               fs.mkdir(d + '/empty');
               fs.rename(d + '/log.txt', d + '/moved.txt');
               fs.write(d + '/gone.txt', 'x');
               fs.remove(d + '/gone.txt');
               return {{
                 bin: fs.read(d + '/new/deep/a.bin', {{encoding: 'base64'}}),
                 log: fs.read(d + '/moved.txt'),
                 stat: fs.stat(d + '/new/deep/a.bin'),
                 missing: fs.stat(d + '/gone.txt'),
                 list: fs.list(d, {{details: true}}),
               }};
             }}"
        ),
        json!({}),
        &[&r, &w],
    )
    .unwrap();
    assert_eq!(out["bin"], "AAEC/w==");
    assert_eq!(out["log"], "one\ntwo\n");
    assert_eq!(out["stat"]["type"], "file");
    assert_eq!(out["stat"]["size"], 4);
    assert!(out["missing"].is_null());
    let names: Vec<(String, String)> = out["list"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap().into(),
                e["type"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("empty".into(), "dir".into()),
            ("moved.txt".into(), "file".into()),
            ("new".into(), "dir".into())
        ]
    );
}

#[cfg(unix)]
#[test]
fn fs_refuses_dangling_symlink_escape() {
    let dir = temp_dir("dangle");
    let outside = temp_dir("dangle-out");
    std::os::unix::fs::symlink(format!("{outside}/target.txt"), format!("{dir}/link")).unwrap();
    let e = live(
        &format!(
            "function run(){{ fs.write({:?}, 'x'); }}",
            format!("{dir}/link")
        ),
        json!({}),
        &[&format!("fs:write:{dir}")],
    )
    .unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::Denied, "{}", e.message);
    assert!(!std::path::Path::new(&format!("{outside}/target.txt")).exists());
}

#[test]
fn read_chunks_never_split_utf8() {
    let dir = temp_dir("utf8");
    let text = "héllo wörld € 😀 ".repeat(50);
    std::fs::write(format!("{dir}/t.txt"), &text).unwrap();
    let out = live(
        &format!(
            "function run(){{ let s = ''; let bad = 0; fs.readChunks({:?}, 5, c => {{ if (c.indexOf('\\ufffd') >= 0) bad++; s += c; }}); return {{s, bad}}; }}",
            format!("{dir}/t.txt")
        ),
        json!({}),
        &[&format!("fs:read:{dir}")],
    )
    .unwrap();
    assert_eq!(out["bad"], 0);
    assert_eq!(out["s"], text);
}

#[test]
fn overlay_modes_keep_new_fs_ops_off_disk() {
    let dir = temp_dir("overlay");
    std::fs::write(format!("{dir}/keep.txt"), "real").unwrap();
    let out = exec(
        &format!(
            "function run(){{ const d = {dir:?};
               fs.append(d + '/keep.txt', '+more');
               fs.rename(d + '/keep.txt', d + '/k2.txt');
               return {{k2: fs.read(d + '/k2.txt'), old: fs.exists(d + '/keep.txt'), list: fs.list(d)}}; }}"
        ),
        json!({}),
        &[&format!("fs:read:{dir}"), &format!("fs:write:{dir}")],
        HostMode::Replay,
    )
    .unwrap();
    assert_eq!(out["k2"], "real+more");
    assert_eq!(out["old"], false);
    assert_eq!(out["list"], json!(["k2.txt"]));
    assert_eq!(
        std::fs::read_to_string(format!("{dir}/keep.txt")).unwrap(),
        "real"
    );
    assert!(!std::path::Path::new(&format!("{dir}/k2.txt")).exists());
}

#[test]
fn smoke_failures_after_stubs_are_inconclusive() {
    // Input-bound fs scope: smoke grants nothing it can't bind.
    let e = exec(
        "function run(i){ return fs.read(i.path); }",
        json!({"path": ""}),
        &["fs:read:$input.path"],
        HostMode::Smoke,
    )
    .unwrap_err();
    assert!(e.after_stub, "{}", e.message);
    // Stubbed fetch body can't be parsed as JSON.
    let e = exec(
        "function run(i){ return fetch(i.url).json().items.length; }",
        json!({"url": ""}),
        &["net:$input.url"],
        HostMode::Smoke,
    )
    .unwrap_err();
    assert!(e.after_stub, "{}", e.message);
    // A plain bug before any host call is a real failure.
    let e = exec(
        "function run(){ return undefinedThing(); }",
        json!({}),
        &[],
        HostMode::Smoke,
    )
    .unwrap_err();
    assert!(!e.after_stub);
    // Undeclared capability is still a real failure, not a stub.
    let e = exec(
        "function run(){ return fs.read('/etc/hosts'); }",
        json!({}),
        &[],
        HostMode::Smoke,
    )
    .unwrap_err();
    assert!(!e.after_stub);
    assert_eq!(e.kind, ExecErrorKind::Denied);
}

async fn http_fixture() -> String {
    use axum::{response::Redirect, routing::get, Router};
    let app = Router::new()
        .route(
            "/bin",
            get(|| async { ([("x-test", "yes")], vec![0u8, 1, 2, 255]) }),
        )
        .route("/text", get(|| async { "€😀é".repeat(200) }))
        .route("/hop", get(|| async { Redirect::temporary("/text") }))
        .route(
            "/away",
            get(|| async { Redirect::temporary("http://localhost:9/") }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_headers_binary_streaming_and_scoped_redirects() {
    let base = http_fixture().await;
    let run = move |code: String| {
        tokio::task::spawn_blocking(move || live(&code, json!({}), &["net:127.0.0.1"]))
    };
    let out = run(format!(
        "function run(){{ const r = fetch('{base}/bin', {{encoding: 'base64'}}); return {{b: r.body, h: r.headers['x-test']}}; }}"
    ))
    .await
    .unwrap()
    .unwrap();
    assert_eq!(out["b"], "AAEC/w==");
    assert_eq!(out["h"], "yes");

    let out = run(format!(
        "function run(){{ let s = ''; let bad = 0; const st = fetchChunks('{base}/hop', {{}}, 7, c => {{ if (c.indexOf('\\ufffd') >= 0) bad++; s += c; }}); return {{st, bad, s}}; }}"
    ))
    .await
    .unwrap()
    .unwrap();
    assert_eq!(out["st"], 200);
    assert_eq!(out["bad"], 0);
    assert_eq!(out["s"], "€😀é".repeat(200));

    let e = run(format!(
        "function run(){{ return fetch('{base}/away').status; }}"
    ))
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(e.kind, ExecErrorKind::Denied, "{}", e.message);
}

/// Redirects are followed by hand on one shared client: methods change
/// like a browser's, credentials stay on their host, loops are cut off.
#[tokio::test(flavor = "multi_thread")]
async fn fetch_redirect_hops_follow_browser_rules() {
    use axum::{
        http::{HeaderMap, Method},
        response::Redirect,
        routing::any,
        Router,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let port = addr.port();
    let app =
        Router::new()
            .route(
                "/echo",
                any(
                    |method: Method, headers: HeaderMap, body: String| async move {
                        format!(
                            "{method} auth={} body={}",
                            headers.contains_key("authorization"),
                            body.len()
                        )
                    },
                ),
            )
            .route("/see-other", any(|| async { Redirect::to("/echo") }))
            .route("/keep", any(|| async { Redirect::temporary("/echo") }))
            .route(
                "/elsewhere",
                any(move || async move {
                    Redirect::temporary(&format!("http://localhost:{port}/echo"))
                }),
            )
            .route("/loop", any(|| async { Redirect::temporary("/loop") }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}");
    let fetch = |path: &str| {
        let code = format!(
            "function run(){{ return fetch('{base}{path}', {{method: 'POST', body: 'abc', headers: {{authorization: 'secret'}}}}).body; }}"
        );
        tokio::task::spawn_blocking(move || live(&code, json!({}), &["net"]))
    };
    assert_eq!(
        fetch("/see-other").await.unwrap().unwrap(),
        "GET auth=true body=0",
        "303 turns into a bodiless GET"
    );
    assert_eq!(
        fetch("/keep").await.unwrap().unwrap(),
        "POST auth=true body=3",
        "307 keeps the method and body"
    );
    assert_eq!(
        fetch("/elsewhere").await.unwrap().unwrap(),
        "POST auth=false body=3",
        "credentials are not sent on to another host"
    );
    let e = fetch("/loop").await.unwrap().unwrap_err();
    assert!(e.message.contains("too many redirects"), "{}", e.message);
}

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

fn seed(store: &Store, name: &str, desc: &str, code: &str, caps: &[&str]) {
    store
        .create_tool_version(
            name,
            desc,
            r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
            "{}",
            code,
            &Capabilities::parse(&caps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
            256,
            "agent",
        )
        .unwrap();
}

fn names(defs: &[Value]) -> Vec<String> {
    defs.iter()
        .map(|d| d["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn large_registries_load_tools_lazily_through_find_tools() {
    let paths = temp_paths("lazy");
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    for i in 0..45 {
        seed(
            &store,
            &format!("filler_{i:02}"),
            "does filler things",
            "function run(){ return 1; }",
            &[],
        );
    }
    seed(
        &store,
        "zz_parse_ledger",
        "Parse a ledger file into entries",
        "function run(){ return 1; }",
        &[],
    );
    let session = store.create_session(None).unwrap().id;

    let defs = toolreg::tool_definitions(&store, Some(&session), 40).unwrap();
    let listed = names(&defs);
    assert!(!listed.contains(&"zz_parse_ledger".to_string()));
    assert!(listed.contains(&"find_tools".to_string()));
    let find_desc = defs
        .iter()
        .find(|d| d["function"]["name"] == "find_tools")
        .unwrap()
        .clone();
    // Registry size never appears in a definition (prompt-cache prefix).
    let desc = find_desc["function"]["description"].as_str().unwrap();
    assert!(desc.contains("more Tools than are listed"), "{desc}");
    assert!(!desc.chars().any(|c| c.is_ascii_digit()), "{desc}");

    // The listing is name-sorted and frozen: usage elsewhere doesn't move it.
    let builtins = listed.iter().filter(|n| toolreg::is_builtin(n)).count();
    assert!(!listed.contains(&"spawn_subagent".to_string()));
    let mut sorted = listed[builtins..].to_vec();
    sorted.sort();
    assert_eq!(listed[builtins..], sorted[..]);
    let other = store.create_session(None).unwrap().id;
    let (busy, _) = store.get_tool_by_name("filler_44").unwrap().unwrap();
    for _ in 0..5 {
        store
            .record_invocation(
                &busy.id,
                1,
                Some(&other),
                None,
                None,
                "success",
                "",
                "{}",
                "1",
                "",
                1,
                100,
            )
            .unwrap();
    }
    assert_eq!(
        names(&toolreg::tool_definitions(&store, Some(&session), 40).unwrap()),
        listed
    );
    // A fresh Session does pick up the newly most-used Tool.
    let fresh = store.create_session(None).unwrap().id;
    assert!(
        names(&toolreg::tool_definitions(&store, Some(&fresh), 40).unwrap())
            .contains(&"filler_44".to_string())
    );

    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let res: Value = serde_json::from_str(
        &daemon
            .call_tool_direct(&session, "find_tools", json!({"query": "ledger parsing"}))
            .await,
    )
    .unwrap();
    assert_eq!(res["tools"][0]["name"], "zz_parse_ledger");
    // Finding a Tool adds exactly that Tool, in name order.
    let after = names(&toolreg::tool_definitions(&store, Some(&session), 40).unwrap());
    let mut expected = listed.clone();
    expected.push("zz_parse_ledger".into());
    assert_eq!(after, expected);
    // Small registries are listed whole.
    assert_eq!(
        names(&toolreg::tool_definitions(&store, None, 100).unwrap()).len(),
        46 + builtins
    );
    seed(
        &store,
        "zz_one_more",
        "another",
        "function run(){ return 1; }",
        &[],
    );
    let again = toolreg::tool_definitions(&store, Some(&session), 40).unwrap();
    let again_desc = again
        .iter()
        .find(|d| d["function"]["name"] == "find_tools")
        .unwrap();
    assert_eq!(again_desc, &find_desc, "a new Tool does not change it");
}

#[tokio::test(flavor = "multi_thread")]
async fn read_tool_and_test_tool() {
    let paths = temp_paths("rt");
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    seed(
        &store,
        "shout",
        "Upper-case text",
        "function run(i){ return String(i.path).toUpperCase(); }",
        &[],
    );
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());

    let read: Value = serde_json::from_str(
        &daemon
            .call_tool_direct(&session, "read_tool", json!({"name": "shout"}))
            .await,
    )
    .unwrap();
    assert!(read["implementation_js"]
        .as_str()
        .unwrap()
        .contains("toUpperCase"));
    assert_eq!(read["version"], 1);

    // Existing Tool, real input; not recorded as an Invocation.
    let t: Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &session,
                "test_tool",
                json!({"name": "shout", "input": {"path": "hi"}}),
            )
            .await,
    )
    .unwrap();
    assert_eq!(t["ok"], true, "{t}");
    assert_eq!(t["output"], "HI");
    assert_eq!(
        store
            .get_tool_by_name("shout")
            .unwrap()
            .unwrap()
            .0
            .invocations,
        0
    );

    // Unsaved draft whose output breaks its schema: reported, nothing saved.
    let t: Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &session,
                "test_tool",
                json!({
                    "name": "draft_len",
                    "implementation_js": "async function run(i){ console.log('len'); return i.s.length; }",
                    "input_schema": {"type": "object", "properties": {"s": {"type": "string"}}},
                    "output_schema": {"type": "string"},
                    "capabilities": [],
                    "input": {"s": "abc"}
                }),
            )
            .await,
    )
    .unwrap();
    assert_eq!(t["ok"], false);
    assert_eq!(t["stage"], "output_validation");
    assert_eq!(t["output"], 3);
    assert_eq!(t["logs"], json!(["len"]));
    assert!(store.get_tool_by_name("draft_len").unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn create_tool_accepts_generic_tools_and_rejects_real_bugs() {
    let paths = temp_paths("ct");
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let def = |name: &str, code: &str| {
        json!({
            "name": name,
            "description": "Read a text file",
            "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]},
            "output_schema": {"type": "string"},
            "implementation_js": code,
            "capabilities": ["fs:read:$input.path"]
        })
    };
    let ok: Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &session,
                "create_tool",
                def(
                    "read_text_file",
                    "function run(i){ return fs.read(i.path); }",
                ),
            )
            .await,
    )
    .unwrap();
    assert_eq!(ok["created"], "read_text_file");
    assert!(ok["smoke_test"].as_str().unwrap().contains("inconclusive"));
    assert!(store.session_tool_ids(&session).unwrap().len() == 1);

    let bad = daemon
        .call_tool_direct(
            &session,
            "create_tool",
            def("broken_reader", "function run(i){ return nope(i.path); }"),
        )
        .await;
    assert!(
        bad.starts_with("error: create_tool failed: smoke test failed"),
        "{bad}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_calls_still_need_approval_for_sensitive_tools() {
    let paths = temp_paths("appr");
    let mut cfg = Config::default();
    cfg.security.secure = true;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    seed(
        &store,
        "pinger",
        "Ping",
        "function run(){ return 1; }",
        &["net"],
    );
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    // No client is attached to answer, so the approval fails closed.
    let out = daemon.call_tool_direct(&session, "pinger", json!({})).await;
    assert!(out.contains("denied approval"), "{out}");
}

/// Mock provider for the sub-agent: with no tool result yet it asks to
/// call `delegate` again (recursing); once it has one it answers with it.
async fn subagent_provider() -> String {
    use axum::{response::IntoResponse, routing::post, Json, Router};
    let app = Router::new().route(
        "/chat/completions",
        post(|Json(body): Json<Value>| async move {
            let msgs = body["messages"].as_array().cloned().unwrap_or_default();
            let tool = msgs.iter().find(|m| m["role"] == "tool");
            let delta = match tool {
                None => json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1",
                    "function": {"name": "delegate", "arguments": "{\"task\":\"again\"}"}}]}}]}),
                Some(t) => json!({"choices": [{"delta": {"content": format!("inner: {}", t["content"].as_str().unwrap_or_default())}}]}),
            };
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                format!("data: {delta}\n\ndata: [DONE]\n\n"),
            )
                .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn sub_agents_cannot_recurse_past_the_depth_limit() {
    std::env::set_var("FORMLESS_API_KEY", "test-key");
    let paths = temp_paths("depth");
    let cfg = Config {
        provider: formless_core::config::ProviderCfg {
            base_url: subagent_provider().await,
            model: "mock-1".into(),
            max_retries: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    seed(
        &store,
        "delegate",
        "Delegate a task to a sub-agent",
        "function run(i){ return agents.run(i.task); }",
        &["agent"],
    );
    let (info, _) = store.get_tool_by_name("delegate").unwrap().unwrap();
    store
        .set_approval(
            &info.id,
            &Capabilities::parse(&["agent".into()]).unwrap(),
            256,
        )
        .unwrap();
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());

    // Main agent -> delegate -> sub-agent -> delegate -> refused.
    let out = daemon
        .call_tool_direct(&session, "delegate", json!({"task": "start"}))
        .await;
    assert!(out.starts_with("inner: "), "{out}");
    assert!(out.contains("sub-agent depth limit (1) reached"), "{out}");
    // Both delegate calls were recorded as measured Invocations.
    assert_eq!(
        store
            .get_tool_by_name("delegate")
            .unwrap()
            .unwrap()
            .0
            .invocations,
        2
    );
}

/// A Session keeps the Tool definition it first listed when a new version
/// lands elsewhere (prompt-cache prefix), until its pin is refreshed.
#[test]
fn listed_tool_definitions_are_pinned_per_session() {
    let store = Store::open_in_memory().unwrap();
    let schema = |field: &str| {
        format!(r#"{{"type":"object","properties":{{"{field}":{{"type":"string"}}}}}}"#)
    };
    let make = |field: &str| {
        store
            .create_tool_version(
                "greeter",
                "greets",
                &schema(field),
                "{}",
                "function run(){ return 1; }",
                &Capabilities::default(),
                256,
                "agent",
            )
            .unwrap()
    };
    let (info, _) = make("name");
    let session = store.create_session(None).unwrap().id;
    let params = |defs: &[Value]| {
        defs.iter()
            .find(|d| d["function"]["name"] == "greeter")
            .unwrap()["function"]["parameters"]["properties"]
            .clone()
    };
    let first = toolreg::tool_definitions(&store, Some(&session), 40).unwrap();
    assert!(params(&first).get("name").is_some());

    make("who"); // v2 with a different input schema
    let again = toolreg::tool_definitions(&store, Some(&session), 40).unwrap();
    assert_eq!(again, first, "the listed definition is pinned");
    let fresh = store.create_session(None).unwrap().id;
    let other = toolreg::tool_definitions(&store, Some(&fresh), 40).unwrap();
    assert!(params(&other).get("who").is_some(), "new Sessions list v2");

    let dropped = store
        .unpin_definitions(&session, Some(std::slice::from_ref(&info.id)))
        .unwrap();
    assert_eq!(dropped, vec![(info.id.clone(), 1)]);
    let refreshed = toolreg::tool_definitions(&store, Some(&session), 40).unwrap();
    assert!(params(&refreshed).get("who").is_some());
}

/// Find-before-create eval: paraphrased requests must find the Tool that
/// already does the job (the keyword matcher misses most of these).
#[test]
fn bm25_search_finds_tools_by_meaning_not_exact_words() {
    let store = Store::open_in_memory().unwrap();
    let tools = [
        (
            "http_get_text",
            "Fetch a URL and return the response body as text",
            r#"{"type":"object","properties":{"url":{"type":"string"}}}"#,
        ),
        (
            "text_stats",
            "Character, word and line statistics for a string",
            r#"{"type":"object","properties":{"text":{"type":"string"}}}"#,
        ),
        (
            "list_dir",
            "Names of the entries in a directory",
            r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
        ),
        (
            "delete_path",
            "Remove a file",
            r#"{"type":"object","properties":{"path":{"type":"string"}}}"#,
        ),
        (
            "now_iso",
            "Current timestamp in ISO 8601",
            r#"{"type":"object","properties":{}}"#,
        ),
        (
            "json_pretty",
            "Reformat JSON with indentation",
            r#"{"type":"object","properties":{"json":{"type":"string"}}}"#,
        ),
        (
            "sha256_hex",
            "Hash input bytes",
            r#"{"type":"object","properties":{"data":{"type":"string","description":"bytes to hash"}}}"#,
        ),
        (
            "csv_to_rows",
            "Parse CSV into rows",
            r#"{"type":"object","properties":{"csv":{"type":"string"}}}"#,
        ),
    ];
    for (name, desc, schema) in tools {
        store
            .create_tool_version(
                name,
                desc,
                schema,
                "{}",
                "function run(){ return 1; }",
                &Capabilities::default(),
                256,
                "agent",
            )
            .unwrap();
    }
    let cases = [
        ("download web page", "http_get_text"),
        ("count words", "text_stats"),
        ("list files in a folder", "list_dir"),
        ("erase a file", "delete_path"),
        ("what time is it", "now_iso"),
        ("checksum of some data", "sha256_hex"),
    ];
    let top = |q: &str, method: &str| {
        toolreg::search_tools_with(&store, q, 3, method)
            .unwrap()
            .first()
            .map(|t| t.name.clone())
    };
    let mut keyword_hits = 0;
    for (query, want) in cases {
        assert_eq!(top(query, "bm25").as_deref(), Some(want), "query {query:?}");
        keyword_hits += usize::from(top(query, "keyword").as_deref() == Some(want));
    }
    assert!(
        keyword_hits < cases.len(),
        "the eval should be harder than exact words"
    );
}

/// Unloading takes effect at compaction only; loading again revives a
/// Tool; the per-Session cap drops the oldest loaded Tools first.
#[test]
fn session_tools_unload_at_compaction_and_respect_the_cap() {
    let store = Store::open_in_memory().unwrap();
    let mut ids = Vec::new();
    for name in ["alpha", "beta", "gamma", "delta"] {
        let (info, _) = store
            .create_tool_version(
                name,
                name,
                r#"{"type":"object"}"#,
                "{}",
                "function run(){ return 1; }",
                &Capabilities::default(),
                256,
                "agent",
            )
            .unwrap();
        ids.push(info.id);
    }
    let s = store.create_session(None).unwrap().id;
    for id in &ids {
        store
            .load_session_tools(&s, std::slice::from_ref(id), "found")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(store.request_unload(&s, &ids[..1]).unwrap(), 1);
    assert!(
        store.session_tool_ids(&s).unwrap().contains(&ids[0]),
        "not before compaction"
    );
    assert_eq!(store.apply_unloads(&s, 60).unwrap(), 1);
    let listed = store.session_tool_ids(&s).unwrap();
    assert!(!listed.contains(&ids[0]));
    assert_eq!(listed.len(), 3);
    // find_tools loads it again.
    store.load_session_tools(&s, &ids[..1], "found").unwrap();
    assert!(store.session_tool_ids(&s).unwrap().contains(&ids[0]));
    // Cap of 2: the two oldest go (beta, gamma), the newest stay.
    assert_eq!(store.apply_unloads(&s, 2).unwrap(), 2);
    let listed = store.session_tool_ids(&s).unwrap();
    assert!(
        listed.contains(&ids[0]) && listed.contains(&ids[3]),
        "{listed:?}"
    );
    assert_eq!(listed.len(), 2);
}

/// The frozen most-used filler is re-picked after compaction, so a
/// long-lived Session sees Tools that became popular later.
#[test]
fn filler_is_repicked_after_compaction() {
    let store = Store::open_in_memory().unwrap();
    let make = |name: &str| {
        store
            .create_tool_version(
                name,
                name,
                r#"{"type":"object"}"#,
                "{}",
                "function run(){ return 1; }",
                &Capabilities::default(),
                256,
                "agent",
            )
            .unwrap()
            .0
    };
    let old = make("old_tool");
    let s = store.create_session(None).unwrap().id;
    let listed = |store: &Store| names(&toolreg::tool_definitions(store, Some(&s), 1).unwrap());
    assert!(listed(&store).contains(&"old_tool".to_string()));
    // A newer Tool becomes the most used elsewhere.
    let hot = make("hot_tool");
    let other = store.create_session(None).unwrap().id;
    for _ in 0..5 {
        let inv = store
            .start_invocation(&hot.id, 1, Some(&other), None, None, "{}")
            .unwrap();
        store
            .finish_invocation(&inv, &hot.id, "success", "", "{}", "", 1, 1000)
            .unwrap();
    }
    assert!(
        !listed(&store).contains(&"hot_tool".to_string()),
        "frozen until compaction"
    );
    store.reset_filler(&s).unwrap();
    let after = listed(&store);
    assert!(after.contains(&"hot_tool".to_string()), "{after:?}");
    assert!(!after.contains(&"old_tool".to_string()), "{after:?}");
    let _ = old;
}

/// The agent can find and read shared modules (and see who uses them)
/// before versioning one, instead of rewriting it from memory.
#[tokio::test(flavor = "multi_thread")]
async fn agent_can_find_and_read_modules() {
    let paths = temp_paths("readmod");
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    store
        .create_module_version(
            "strings",
            "string helpers: shout and trim",
            "module.exports = { v: 1 };",
            "agent",
        )
        .unwrap();
    store
        .create_module_version(
            "strings",
            "string helpers: shout and trim",
            "module.exports = { v: 2 };",
            "agent",
        )
        .unwrap();
    seed(
        &store,
        "loud",
        "uses strings",
        "function run(i){ return requireModule('strings').v; }",
        &[],
    );
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let call = |name: &'static str, args: Value| {
        let (d, s) = (daemon.clone(), session.clone());
        async move { serde_json::from_str::<Value>(&d.call_tool_direct(&s, name, args).await).unwrap() }
    };
    let m = call("read_module", json!({"name": "strings"})).await;
    assert_eq!(m["version"], 2);
    assert_eq!(m["implementation_js"], "module.exports = { v: 2 };");
    assert_eq!(m["versions"].as_array().unwrap().len(), 2);
    assert_eq!(m["required_by"], json!(["loud"]));
    let old = call("read_module", json!({"name": "strings", "version": 1})).await;
    assert_eq!(old["implementation_js"], "module.exports = { v: 1 };");
    let found = call("find_tools", json!({"query": "string helpers"})).await;
    assert_eq!(found["modules"][0]["name"], "strings");
}

/// The agent reads its own stored outputs without building an fs Tool;
/// other Sessions' artifacts stay out of reach.
#[tokio::test(flavor = "multi_thread")]
async fn agent_reads_its_own_artifacts_by_id() {
    let paths = temp_paths("readart");
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let mine = store.create_session(None).unwrap().id;
    let theirs = store.create_session(None).unwrap().id;
    let file = paths.data_dir.join("big.txt");
    let body: String = (1..=500).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&file, &body).unwrap();
    let id = store
        .save_artifact(Some(&mine), &file.to_string_lossy(), "", body.len() as i64)
        .unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let call = |session: String, args: Value| {
        let d = daemon.clone();
        async move { d.call_tool_direct(&session, "read_artifact", args).await }
    };
    let slice: Value = serde_json::from_str(
        &call(mine.clone(), json!({"id": id, "offset": 7, "length": 7})).await,
    )
    .unwrap();
    assert_eq!(slice["text"], "line 2\n");
    assert_eq!(slice["total_chars"], body.len());
    let grep: Value =
        serde_json::from_str(&call(mine.clone(), json!({"id": id, "grep": "line 49"})).await)
            .unwrap();
    let lines: Vec<i64> = grep["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["line"].as_i64().unwrap())
        .collect();
    assert_eq!(
        lines,
        vec![49, 490, 491, 492, 493, 494, 495, 496, 497, 498, 499]
    );
    let denied = call(theirs, json!({"id": id})).await;
    assert!(denied.contains("no artifact"), "{denied}");
}

#[tokio::test(flavor = "multi_thread")]
async fn read_tool_and_protocol_return_version_diffs() {
    let paths = temp_paths("tooldiff");
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    seed(&store, "adder", "adds", "function run(i){ return 1; }", &[]);
    seed(&store, "adder", "adds", "function run(i){ return 2; }", &[]);
    let id = store.get_tool_by_name("adder").unwrap().unwrap().0.id;
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let read: Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &session,
                "read_tool",
                json!({"name": "adder", "diff_from": 1}),
            )
            .await,
    )
    .unwrap();
    let diff = read["diff"].as_str().unwrap();
    assert!(
        diff.contains("- function run(i){ return 1; }")
            && diff.contains("+ function run(i){ return 2; }"),
        "{diff}"
    );
    let r = daemon
        .handle(formless_protocol::Request::DiffToolVersions { id, from: 1, to: 2 })
        .await
        .unwrap();
    assert_eq!(r["diff"], diff);
}

/// With nobody to approve, an agent cannot add or remove MCP servers.
#[tokio::test(flavor = "multi_thread")]
async fn agent_mcp_server_changes_fail_closed_headless() {
    let paths = temp_paths("mcpheadless");
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let out = daemon
        .call_tool_direct(
            &session,
            "add_mcp_server",
            json!({"name": "x", "transport": "stdio", "command": "evil", "reason": "r"}),
        )
        .await;
    assert!(out.contains("did not approve"), "{out}");
    assert!(Config::load(&paths).unwrap().mcp.servers.is_empty());
    assert!(
        toolreg::validate_name("add_mcp_server").is_err(),
        "reserved"
    );
}
