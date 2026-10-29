//! `formless export-all` / `import-all` (SPEC §15): everything for a new
//! machine in one bundle, without secrets.

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use serde_json::json;
use std::sync::Arc;

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    Paths::from_dirs(base.join("d"), base.join("c")).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn everything_moves_to_a_new_machine_without_secrets() {
    let from = temp_paths("expall");
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.roles.insert("explorer".into(), Default::default());
    let mut p = formless_core::config::ProviderCfg::default();
    p.base_url = "https://example.com/v1".into();
    p.extra_headers.insert("X-Key".into(), "sekrit".into());
    cfg.providers.insert("other".into(), p);
    let mut server = formless_core::config::McpServerCfg::default();
    server.command = "npx".into();
    server.env.insert("TOKEN".into(), "sekrit".into());
    cfg.mcp.servers.insert("files".into(), server);
    cfg.save(&from).unwrap();
    {
        let store = Store::open(&from.db_file()).unwrap();
        store
            .create_module_version(
                "strutil",
                "string helpers",
                "module.exports = { up: (s) => s.toUpperCase() };",
                "agent",
            )
            .unwrap();
        store
            .create_tool_version(
                "shout",
                "upper-cases text",
                r#"{"type":"object","properties":{"t":{"type":"string"}}}"#,
                "{}",
                "function run(i){ return {t: String(i.t || '').toUpperCase()}; }",
                &Capabilities::default(),
                64,
                "agent",
            )
            .unwrap();
        store
            .add_memory("Uses pnpm, not npm", "global", "user", None)
            .unwrap();
        let s = store.create_session(Some("History".into())).unwrap();
        store.append_message(&s.id, "user", "hello there").unwrap();
        store
            .create_job(
                "check the news",
                None,
                Some("0 9 * * 1-5"),
                "2030-01-01T09:00:00+00:00",
                true,
                "user",
            )
            .unwrap();
    }
    let a = Arc::new(formless_core::daemon::Daemon::new(from.clone()).unwrap());
    let bundle = a
        .handle(Request::ExportAll { sessions: true })
        .await
        .unwrap();
    assert_eq!(bundle["format"], "formless.all/1");
    let text = bundle.to_string();
    assert!(!text.contains("sekrit"), "secrets are blanked");
    let blanked: Vec<&str> = bundle["blanked"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        blanked.contains(&"providers.other.extra_headers.X-Key"),
        "{blanked:?}"
    );
    assert!(
        blanked.contains(&"mcp.servers.files.env.TOKEN"),
        "{blanked:?}"
    );

    let to = temp_paths("impall");
    let mut fresh = Config::default();
    fresh.naming.enabled = false;
    fresh.save(&to).unwrap();
    let b = Arc::new(formless_core::daemon::Daemon::new(to.clone()).unwrap());
    let r = b.handle(Request::ImportAll { bundle }).await.unwrap();
    assert_eq!(r["failed"], json!([]), "{r}");
    assert_eq!(
        (
            r["tools"].as_i64(),
            r["modules"].as_i64(),
            r["memories"].as_i64()
        ),
        (Some(1), Some(1), Some(1)),
        "{r}"
    );
    assert_eq!(
        (r["jobs"].as_i64(), r["sessions"].as_i64()),
        (Some(1), Some(1)),
        "{r}"
    );
    assert_eq!(
        (
            r["providers"].as_i64(),
            r["roles"].as_i64(),
            r["mcp_servers"].as_i64()
        ),
        (Some(1), Some(1), Some(1)),
        "{r}"
    );
    let store = Store::open(&to.db_file()).unwrap();
    assert!(store.get_tool_by_name("shout").unwrap().is_some());
    assert!(store.get_module_by_name("strutil").unwrap().is_some());
    let jobs = store.list_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(!jobs[0].enabled, "jobs come in paused");
    assert!(store
        .list_sessions()
        .unwrap()
        .iter()
        .any(|s| s.name == "History"));
    let cfg = Config::load(&to).unwrap();
    assert!(cfg.roles.contains_key("explorer"));
    assert_eq!(
        cfg.mcp.servers["files"].env["TOKEN"], "",
        "blank, to fill in"
    );
    // A second import adds no duplicate modules or config entries.
    let again = b
        .handle(Request::ImportAll {
            bundle: a
                .handle(Request::ExportAll { sessions: false })
                .await
                .unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(
        (again["modules"].as_i64(), again["providers"].as_i64()),
        (Some(0), Some(0)),
        "{again}"
    );
}
