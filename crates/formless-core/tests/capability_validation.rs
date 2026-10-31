//! Malformed scopes fail at the Tool creation gate, before approval or execution.
use formless_core::{Config, Paths, Store};
use serde_json::json;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn tool_creation_validates_and_stores_canonical_scopes() {
    let base = std::env::temp_dir().join(format!("fl-scope-validation-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let draft = |name: &str, scope: &str| {
        json!({
            "name": name, "description":"Scope validation", "input_schema":{"type":"object"},
            "output_schema":{"type":"integer"}, "implementation_js":"function run() { return 1; }",
            "capabilities":[scope]
        })
    };
    for (index, scope) in [
        "net:",
        "process:",
        "net:https://api.example.com",
        "net:api.example.com:443/v1",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("bad_scope_{index}");
        let (text, ok) = daemon
            .call_tool_direct_outcome(&sid, "create_tool", draft(&name, scope))
            .await;
        assert!(!ok, "{scope} was accepted: {text}");
        assert!(
            text.contains("needs a") || text.contains("bare host"),
            "{text}"
        );
        assert!(store.get_tool_by_name(&name).unwrap().is_none());
    }
    let (text, ok) = daemon
        .call_tool_direct_outcome(
            &sid,
            "create_tool",
            draft("canonical_scope", "net:API.Example.COM:8443"),
        )
        .await;
    assert!(ok, "{text}");
    let (tool, version) = store.get_tool_by_name("canonical_scope").unwrap().unwrap();
    assert_eq!(tool.capabilities, ["net:api.example.com:8443"]);
    assert_eq!(version.capabilities, ["net:api.example.com:8443"]);
    drop(daemon);
    drop(store);
    std::fs::remove_dir_all(base).unwrap();
}
