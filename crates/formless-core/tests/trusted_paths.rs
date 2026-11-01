//! Trusted folders (SPEC §10.3): writes inside them need no approval.
#![cfg(unix)]

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

async fn write_with(trust_writes: bool, target: &str) -> (bool, std::path::PathBuf) {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-trust-{short}"));
    let project = dir.join("project");
    std::fs::create_dir_all(project.join(".ssh")).unwrap();
    std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
    let project = project.canonicalize().unwrap();
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.security.secure = true;
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.security.trusted_paths = vec![project.display().to_string()];
    cfg.security.trust_writes = trust_writes;
    cfg.save(&paths).unwrap();
    let base = dir.canonicalize().unwrap().display().to_string();
    let out = format!("{base}/{target}");
    let (id, session) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[format!("fs:write:{out}")]).unwrap();
        let (info, _) = store
            .create_tool_version(
                "writer",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                &format!("function run(i){{ fs.write('{out}', 'x'); return {{ok: true}}; }}"),
                &caps,
                64,
                "agent",
            )
            .unwrap();
        (info.id, store.create_session(None).unwrap().id)
    };
    // No client is connected: anything that would ask fails closed.
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let r = daemon
        .handle(Request::RunTool {
            id,
            input: serde_json::json!({}),
            session_id: session,
        })
        .await
        .unwrap();
    let ran = !r["result"].as_str().unwrap_or_default().contains("denied");
    let file = std::path::PathBuf::from(&out);
    if ran {
        let store = Store::open(&paths.db_file()).unwrap();
        let audit = store.recent_audit(Some("tool.approve_trusted"), 5).unwrap();
        assert_eq!(audit.len(), 1, "logged");
    }
    let _ = std::fs::remove_dir_all(&dir);
    (ran, file)
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_inside_a_trusted_folder_need_no_approval() {
    assert!(write_with(true, "project/out.txt").await.0, "inside: runs");
    assert!(
        !write_with(true, "elsewhere/out.txt").await.0,
        "outside: asks"
    );
    assert!(
        !write_with(true, "project/.ssh/config").await.0,
        "secrets folder: asks"
    );
    assert!(
        !write_with(false, "project/out.txt").await.0,
        "trust_writes = false: asks"
    );
}

#[test]
fn trusted_hosts_match_exactly_or_by_wildcard_and_never_everything() {
    let sec = formless_core::config::SecurityCfg {
        trusted_hosts: vec![
            "api.example.com".into(),
            "*.github.com".into(),
            "*".into(),
            "com".into(),
        ],
        ..Default::default()
    };
    assert!(sec.host_trusted("api.example.com"));
    assert!(
        !sec.host_trusted("v2.api.example.com"),
        "a bare trusted host is exact only"
    );
    assert!(
        !sec.host_trusted("GitHub.com"),
        "an explicit wildcard excludes its apex"
    );
    assert!(sec.host_trusted("raw.github.com"));
    assert!(!sec.host_trusted("example.com"));
    assert!(
        !sec.host_trusted("evilgithub.com"),
        "a look-alike isn't a subdomain"
    );
    assert!(
        !sec.host_trusted("anything.com"),
        "`*` and a bare TLD trust nothing"
    );
    assert!(!sec.host_trusted(""));
    let scoped = formless_core::config::SecurityCfg {
        trusted_hosts: vec![
            "*.github.io:443".into(),
            "*.workers.dev".into(),
            "api.example.com:8443".into(),
        ],
        ..Default::default()
    };
    assert!(scoped.host_trusted("tenant.github.io:443"));
    assert!(!scoped.host_trusted("tenant.github.io:80"));
    assert!(!scoped.host_trusted("github.io:443"));
    assert!(!scoped.host_trusted("sub.api.example.com:8443"));
    assert!(scoped
        .network_scope_warnings()
        .iter()
        .any(|warning| warning.contains("PUBLIC SUFFIX") && warning.contains("github.io")));
    assert!(scoped
        .network_scope_warnings()
        .iter()
        .any(|warning| warning.contains("workers.dev")));
}

/// A Tool that declares `net:HOST` (and never calls it), run with no
/// client connected: it runs only when the host is trusted.
async fn net_with(trusted: &[&str], grant: &str) -> bool {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-host-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.security.secure = true;
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.naming.enabled = false;
    cfg.security.trusted_hosts = trusted.iter().map(|s| s.to_string()).collect();
    cfg.save(&paths).unwrap();
    let (id, session) = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[grant.to_string()]).unwrap();
        let (info, _) = store
            .create_tool_version(
                "caller",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                "function run(i){ return {ok: true}; }",
                &caps,
                64,
                "agent",
            )
            .unwrap();
        (info.id, store.create_session(None).unwrap().id)
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let r = daemon
        .handle(Request::RunTool {
            id,
            input: serde_json::json!({}),
            session_id: session,
        })
        .await
        .unwrap();
    let ran = !r["result"].as_str().unwrap_or_default().contains("denied");
    if ran {
        let store = Store::open(&paths.db_file()).unwrap();
        assert_eq!(
            store
                .recent_audit(Some("tool.approve_trusted"), 5)
                .unwrap()
                .len(),
            1,
            "logged"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
    ran
}

#[tokio::test(flavor = "multi_thread")]
async fn net_grants_for_trusted_hosts_need_no_approval() {
    assert!(net_with(&["api.example.com"], "net:api.example.com").await);
    assert!(net_with(&["*.github.com"], "net:api.github.com").await);
    assert!(
        !net_with(&["api.example.com"], "net:example.com").await,
        "wider than trusted: asks"
    );
    assert!(
        !net_with(&["api.example.com"], "net").await,
        "bare net never trusted"
    );
    assert!(
        !net_with(&[], "net:api.example.com").await,
        "nothing trusted: asks"
    );
}
