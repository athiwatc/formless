//! Raw MCP tools that a Tool wraps rank below it in find_tools, and say so.

use formless_core::storage::Store;
use formless_core::Paths;
use formless_runtime::Capabilities;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn find_tools_lists_wrapped_mcp_tools_after_their_wrapper() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-wrap-{short}"));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let tool = |name: &str, desc: &str, code: &str, caps: &[&str]| {
        let caps =
            Capabilities::parse(&caps.iter().map(|c| c.to_string()).collect::<Vec<_>>()).unwrap();
        store
            .create_tool_version(
                name,
                desc,
                r#"{"type":"object"}"#,
                "{}",
                code,
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
    };
    let raw = tool(
        "mcp__issues__list_issues",
        "List issues in a tracker project",
        "// MCP tool list_issues",
        &["mcp:issues"],
    );
    let wrapper = tool(
        "open_issue_titles",
        "Titles of open issues in a tracker project",
        "async function run(i){ return tools.call('mcp__issues__list_issues', i); }",
        &["mcp:issues"],
    );
    let parent = store
        .record_invocation(
            &wrapper.id,
            1,
            Some(&sid),
            None,
            None,
            "success",
            "",
            "{}",
            "[]",
            "",
            1,
            100,
        )
        .unwrap();
    store
        .record_invocation(
            &raw.id,
            1,
            Some(&sid),
            None,
            Some(&parent),
            "success",
            "",
            "{}",
            "[]",
            "",
            1,
            100,
        )
        .unwrap();
    drop(store);
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let find = |query: &'static str| {
        let daemon = daemon.clone();
        let sid = sid.clone();
        async move {
            let out = daemon
                .call_tool_direct(&sid, "find_tools", serde_json::json!({"query": query}))
                .await;
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            v["tools"].as_array().unwrap().clone()
        }
    };
    let names = |tools: &[serde_json::Value]| -> Vec<String> {
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };

    let general = find("list issues tracker project").await;
    let order = names(&general);
    let w = order.iter().position(|n| n == "open_issue_titles");
    let r = order.iter().position(|n| n == "mcp__issues__list_issues");
    assert!(
        matches!((w, r), (Some(w), Some(r)) if w < r),
        "wrapper first, raw still listed: {order:?}"
    );
    let raw_row = &general[r.unwrap()];
    assert_eq!(
        raw_row["wrapped_by"],
        serde_json::json!(["open_issue_titles"])
    );

    let exact = find("mcp__issues__list_issues").await;
    assert_eq!(
        names(&exact).first().map(String::as_str),
        Some("mcp__issues__list_issues"),
        "an exact search keeps its rank"
    );
    let _ = std::fs::remove_dir_all(base);
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_raw_mcp_chains_are_suggested_until_a_tool_wraps_them() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-chain-{short}"));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let caps = Capabilities::parse(&["mcp:a".to_string()]).unwrap();
    let tool = |name: &str, code: &str| {
        store
            .create_tool_version(
                name,
                "d",
                r#"{"type":"object"}"#,
                "{}",
                code,
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
    };
    let x = tool("mcp__a__x", "// MCP tool x");
    let y = tool("mcp__a__y", "// MCP tool y");
    let call = |id: &str, run: &str, parent: Option<&str>| {
        store
            .record_invocation(
                id,
                1,
                None,
                Some(run),
                parent,
                "success",
                "",
                "{}",
                "{}",
                "",
                1,
                100,
            )
            .unwrap()
    };
    for run in ["r1", "r2", "r3"] {
        call(&x.id, run, None);
        call(&y.id, run, None);
    }
    // A single call with nothing after it is no chain.
    call(&x.id, "r4", None);
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let found = daemon.mcp_chain_suggestions().unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0]["calls"],
        serde_json::json!(["mcp__a__x", "mcp__a__y"])
    );
    assert_eq!(found[0]["runs"], 3);

    let w = tool("x_then_y", "async function run(){}");
    let p = call(&w.id, "r5", None);
    call(&x.id, "r5", Some(&p));
    call(&y.id, "r5", Some(&p));
    assert!(
        daemon.mcp_chain_suggestions().unwrap().is_empty(),
        "wrapped now"
    );
    let _ = std::fs::remove_dir_all(base);
}
