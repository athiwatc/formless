//! Hooks (SPEC §9.8): pre_tool may deny, post_tool and run_end automate.

use formless_core::config::HookCfg;
use formless_core::hooks::{glob, matches, run_command, tool_verdict};
use formless_core::{Config, Paths, Store};
use std::sync::Arc;

#[test]
fn hook_matching() {
    assert!(glob("mcp__*", "mcp__github__search"));
    assert!(glob("*_file", "read_text_file") && !glob("*_file", "read_files"));
    assert!(glob("read_*_text", "read_csv_text") && !glob("read_*_text", "read_csv"));
    assert!(glob("echo", "echo") && !glob("echo", "echo2"));
    let h = HookCfg {
        event: "pre_tool".into(),
        tool: Some("fetch_*".into()),
        capability: Some("net".into()),
        ..Default::default()
    };
    assert!(matches(&h, "fetch_page", &["net:example.com".into()]));
    assert!(!matches(&h, "fetch_page", &["fs:read:/tmp".into()]));
    assert!(!matches(&h, "read_page", &["net".into()]));
    assert_eq!(
        tool_verdict(&serde_json::json!({"deny": true, "reason": "no"})),
        (false, "no".into())
    );
    assert!(tool_verdict(&serde_json::json!({"ok": 1})).0);
}

#[cfg(unix)]
#[tokio::test]
async fn hook_commands_get_the_event_on_stdin() {
    let t = std::time::Duration::from_secs(5);
    let payload = serde_json::json!({"tool": "x"});
    let (ok, out) = run_command("sh", &["-c".into(), "cat".into()], &payload, t).await;
    assert!(ok && out.contains("\"tool\":\"x\""), "{out}");
    let (ok, out) = run_command(
        "sh",
        &["-c".into(), "echo blocked; exit 3".into()],
        &payload,
        t,
    )
    .await;
    assert!(!ok && out == "blocked");
    let (ok, out) = run_command(
        "sh",
        &["-c".into(), "sleep 5".into()],
        &payload,
        std::time::Duration::from_millis(200),
    )
    .await;
    assert!(!ok && out.contains("timed out"));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn pre_and_post_hooks_around_tool_calls() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-hook-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-hook-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    let log = base.join("post.log");
    let cfg = Config {
        hooks: vec![
            HookCfg {
                event: "pre_tool".into(),
                tool: Some("blocked_*".into()),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "echo not on Fridays; exit 1".into(),
                ],
                trusted: true,
                ..Default::default()
            },
            HookCfg {
                event: "pre_tool".into(),
                tool: Some("guarded".into()),
                run_tool: Some("policy".into()),
                ..Default::default()
            },
            HookCfg {
                event: "pre_tool".into(),
                tool: Some("needs_ok".into()),
                command: vec!["true".into()],
                ..Default::default()
            },
            HookCfg {
                event: "post_tool".into(),
                tool: Some("echo".into()),
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    format!("cat >> {}", log.display()),
                ],
                trusted: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let mk = |name: &str, js: &str| {
        serde_json::json!({"name": name, "description": "d", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": js, "capabilities": []})
    };
    for (n, js) in [
        ("blocked_tool", "function run(i){ return {ran: true}; }"),
        ("guarded", "function run(i){ return {ran: true}; }"),
        ("needs_ok", "function run(i){ return {ran: true}; }"),
        ("echo", "function run(i){ return {echo: i.v}; }"),
        ("policy", "function run(e){ return ((e.arguments || {}).n || 0) > 5 ? {deny: true, reason: 'n too large'} : {}; }"),
    ] {
        let out = daemon.call_tool_direct(&sid, "create_tool", mk(n, js)).await;
        assert!(out.contains("\"created\""), "{out}");
    }
    let call = |name: &'static str, args: serde_json::Value| {
        let (d, s) = (daemon.clone(), sid.clone());
        async move { d.call_tool_direct(&s, name, args).await }
    };
    let r = call("blocked_tool", serde_json::json!({})).await;
    assert_eq!(r, "error: denied by a pre_tool hook: not on Fridays");
    let r = call("guarded", serde_json::json!({"n": 9})).await;
    assert!(r.contains("n too large"), "{r}");
    let r = call("guarded", serde_json::json!({"n": 1})).await;
    assert!(r.contains("\"ran\":true"), "{r}");
    // An untrusted command with nobody to approve fails closed.
    let r = call("needs_ok", serde_json::json!({})).await;
    assert!(r.contains("hook command not approved"), "{r}");
    // post_tool sees the result.
    let r = call("echo", serde_json::json!({"v": "hi"})).await;
    assert!(r.contains("\"echo\":\"hi\""), "{r}");
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(
        logged.contains("\"event\":\"post_tool\"") && logged.contains("echo"),
        "{logged}"
    );
}
