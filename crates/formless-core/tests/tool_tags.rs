//! Tool tags (SPEC §7.2): normalised, set by the agent or the user, and
//! counted by find_tools.

use formless_core::tools::{normalize_tags, search_tools, search_tools_bm25};
use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

#[test]
fn tags_are_normalised() {
    assert_eq!(
        normalize_tags(["#Git", " web pages ", "git", "a/b!", ""]),
        ["git", "web-pages", "ab"]
    );
    assert_eq!(normalize_tags((0..12).map(|_| "x")).len(), 1);
    let many: Vec<String> = (0..12).map(|i| format!("t{i}")).collect();
    assert_eq!(normalize_tags(many.iter().map(String::as_str)).len(), 8);
}

#[test]
fn find_tools_counts_tags() {
    let store = Store::open_in_memory().unwrap();
    let caps = Capabilities::parse(&[]).unwrap();
    let mk = |name: &str, desc: &str| {
        store
            .create_tool_version(
                name,
                desc,
                "{}",
                "{}",
                "function run(i){}",
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
    };
    let ledger = mk("ledger_sum", "Add up rows of a table");
    mk("row_counter", "Count rows of a table");
    // Nothing in the name or description says "finance"; the tag does.
    assert!(search_tools_bm25(&store, "finance", 5).unwrap().is_empty());
    store
        .set_tool_tags(&ledger.id, &["finance".into()])
        .unwrap();
    for hits in [
        search_tools_bm25(&store, "finance", 5).unwrap(),
        search_tools(&store, "finance", 5).unwrap(),
    ] {
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].name, "ledger_sum");
        assert_eq!(hits[0].tags, ["finance"]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn users_set_tags_through_the_daemon() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-tags-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.save(&paths).unwrap();
    let id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[]).unwrap();
        store
            .create_tool_version(
                "fetch_page",
                "d",
                "{}",
                "{}",
                "function run(i){}",
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
            .id
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths).unwrap());
    let t = daemon
        .handle(Request::SetToolTags {
            id: id.clone(),
            tags: vec!["#Web".into(), "http".into()],
        })
        .await
        .unwrap();
    assert_eq!(t["tags"], serde_json::json!(["web", "http"]));
    let t = daemon
        .handle(Request::SetToolTags { id, tags: vec![] })
        .await
        .unwrap();
    assert_eq!(t["tags"], serde_json::json!([]));
    let _ = std::fs::remove_dir_all(dir);
}
