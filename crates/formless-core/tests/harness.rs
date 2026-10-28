//! The harness Capability (SPEC §7.13): the agent reads and changes its
//! own harness, every change approved with a diff, logged and undoable.
#![cfg(unix)]

use formless_core::{Config, Paths, Store};
use formless_protocol::{
    ApprovalScope, Event, Request, RequestEnvelope, ServerMessage, PROTOCOL_VERSION,
};
use formless_runtime::Capabilities;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[test]
fn harness_scopes_parse_and_only_read_is_free() {
    let read = Capabilities::parse(&["harness:read".into()]).unwrap();
    assert!(!read.is_sensitive());
    let roles = Capabilities::parse(&["harness:roles".into()]).unwrap();
    assert!(roles.is_sensitive());
    assert!(Capabilities::parse(&["harness:everything".into()]).is_err());
    assert_eq!(roles.to_specs(), ["harness:roles"]);
}

fn temp(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

async fn make_tool(
    d: &Arc<formless_core::daemon::Daemon>,
    store: &Store,
    sid: &str,
    name: &str,
    cap: &str,
    js: &str,
) {
    let out = d
        .call_tool_direct(
            sid,
            "create_tool",
            serde_json::json!({
            "name": name, "description": "d", "input_schema": {"type": "object"},
            "output_schema": {}, "implementation_js": js, "capabilities": [cap]}),
        )
        .await;
    assert!(out.contains("\"created\""), "{out}");
    let (tool, ver) = store.get_tool_by_name(name).unwrap().unwrap();
    store
        .set_approval(
            &tool.id,
            &Capabilities::parse(&ver.capabilities).unwrap(),
            ver.memory_mb,
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_changes_are_approved_with_a_diff_logged_and_undoable() {
    std::env::set_var(
        "FORMLESS_API_KEY_SECRETPROV",
        "sk-very-secret-value-1234567890",
    );
    let paths = temp("harn");
    let mut cfg = Config::default();
    cfg.providers.insert(
        "secretprov".into(),
        formless_core::config::ProviderCfg {
            base_url: "http://127.0.0.1:9".into(),
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());

    // harness:read — no approval, no secrets.
    make_tool(
        &daemon,
        &store,
        &sid,
        "inspect",
        "harness:read",
        "function run(){ return harness.read(); }",
    )
    .await;
    let r = daemon
        .call_tool_direct(&sid, "inspect", serde_json::json!({}))
        .await;
    assert!(
        r.contains("secretprov") && r.contains("\"has_key\":true"),
        "{r}"
    );
    assert!(!r.contains("sk-very-secret"), "keys never readable: {r}");

    // A role change with nobody to ask fails closed.
    let js = "function run(i){ return harness.call('roles.upsert', {name: 'scout', role: {description: 'fast search', model: 'tiny'}}, 'searching is slow on the big model'); }";
    make_tool(&daemon, &store, &sid, "add_scout", "harness:roles", js).await;
    let r = daemon
        .call_tool_direct(&sid, "add_scout", serde_json::json!({}))
        .await;
    assert!(r.contains("did not approve"), "{r}");
    assert!(Config::load_file_only(&paths).unwrap().roles.is_empty());

    // With a client: the approval shows before/after and the reason.
    tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (rd, mut w) = tokio::io::split(stream);
    let mut lines = tokio::io::BufReader::new(rd).lines();
    while daemon.subscriber_count() == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let call = tokio::spawn({
        let (d, s) = (daemon.clone(), sid.clone());
        async move {
            d.call_tool_direct(&s, "add_scout", serde_json::json!({}))
                .await
        }
    });
    let (approval_id, detail) = loop {
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Ok(ServerMessage::Event(Event::ApprovalRequested {
            approval_id,
            detail,
            ..
        })) = serde_json::from_str::<ServerMessage>(&line)
        {
            break (approval_id, detail.unwrap_or_default());
        }
    };
    assert!(
        detail.contains("roles.upsert scout")
            && detail.contains("Before: null")
            && detail.contains("fast search"),
        "{detail}"
    );
    assert!(
        detail.contains("searching is slow"),
        "the reason is shown: {detail}"
    );
    let env = RequestEnvelope {
        id: 1,
        protocol: PROTOCOL_VERSION,
        request: Request::ApprovalDecision {
            paths: Default::default(),
            approval_id,
            approved: true,
            scope: ApprovalScope::Once,
        },
    };
    w.write_all(format!("{}\n", serde_json::to_string(&env).unwrap()).as_bytes())
        .await
        .unwrap();
    let r = tokio::time::timeout(std::time::Duration::from_secs(10), call)
        .await
        .unwrap()
        .unwrap();
    assert!(r.contains("\"applied\":\"roles.upsert\""), "{r}");
    assert_eq!(
        Config::load_file_only(&paths).unwrap().roles["scout"].model,
        "tiny"
    );

    // Logged, and undone by the user.
    let log = daemon.handle(Request::ListHarnessChanges).await.unwrap();
    assert_eq!(
        (log[0]["op"].as_str(), log[0]["tool"].as_str()),
        (Some("roles.upsert"), Some("add_scout"))
    );
    daemon
        .handle(Request::UndoHarnessChange { id: None })
        .await
        .unwrap();
    assert!(Config::load_file_only(&paths).unwrap().roles.is_empty());
    assert!(
        daemon
            .handle(Request::UndoHarnessChange { id: None })
            .await
            .is_err(),
        "nothing left"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_self_escalation_and_no_keys() {
    let paths = temp("harn2");
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    // Undeclared scope: refused by the sandbox (already at creation).
    let out = daemon
        .call_tool_direct(&sid, "create_tool", serde_json::json!({
            "name": "sneaky", "description": "d", "input_schema": {"type": "object"}, "output_schema": {},
            "implementation_js": "function run(){ return harness.call('config.set', {key: 'tools.inline_limit', value: 1}, 'x'); }",
            "capabilities": ["harness:read"]}))
        .await;
    assert!(out.contains("harness:config not declared"), "{out}");
    // Snapshots refuse unknown operations; security keys stay off-limits
    // even when approved (set_key's allow-list).
    let mut cfg = Config::default();
    assert!(cfg
        .set_key("relay.enabled", serde_json::json!(true))
        .is_err());
    assert!(cfg
        .set_key("provider.base_url", serde_json::json!("http://evil"))
        .is_err());
}
