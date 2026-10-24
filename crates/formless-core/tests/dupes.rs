//! Near-duplicate Tools (SPEC §8.10).

use formless_core::dupes::{near_duplicates, similarity};
use formless_core::{Paths, Store};
use formless_protocol::{ToolInfo, ToolVersionInfo};
use std::sync::Arc;

fn tool(name: &str, desc: &str, schema: &str, code: &str) -> (ToolInfo, ToolVersionInfo) {
    (
        ToolInfo {
            id: name.into(),
            name: name.into(),
            title: String::new(),
            tags: Vec::new(),
            description: desc.into(),
            retired: false,
            current_version: 1,
            capabilities: vec![],
            memory_mb: 64,
            invocations: 0,
            success_rate: None,
            needs_attention: false,
            created_at: String::new(),
            updated_at: String::new(),
            recent: String::new(),
            author: "agent".into(),
        },
        ToolVersionInfo {
            version: 1,
            author: "agent".into(),
            created_at: String::new(),
            capabilities: vec![],
            memory_mb: 64,
            input_schema: schema.into(),
            output_schema: "{}".into(),
            code: code.into(),
        },
    )
}

#[test]
fn similar_tools_are_paired_and_different_ones_are_not() {
    let path = r#"{"type":"object","properties":{"path":{"type":"string"}}}"#;
    let a = tool(
        "read_file",
        "Read a text file and return its contents",
        path,
        "function run(i){ return fs.read(i.path); }",
    );
    let b = tool(
        "read_text_file",
        "Reads the text contents of a file",
        path,
        "function run(i){ return {text: fs.read(i.path)}; }",
    );
    let c = tool(
        "http_get_json",
        "Fetch a URL and parse JSON",
        r#"{"properties":{"url":{}}}"#,
        "function run(i){ return fetch(i.url).json(); }",
    );
    let d = tool(
        "copy_of_c",
        "Something else entirely",
        "{}",
        "function run(i){  return fetch(i.url).json(); }",
    );
    let (score, why) = similarity((&a.0, &a.1), (&b.0, &b.1));
    assert!(score >= 0.6, "{score} {why}");
    assert!(why.contains("same input fields"));
    assert!(similarity((&a.0, &a.1), (&c.0, &c.1)).0 < 0.3);
    assert_eq!(
        similarity((&c.0, &c.1), (&d.0, &d.1)),
        (1.0, "identical code".into())
    );
    let pairs = near_duplicates(&[a, b, c, d]);
    let names: Vec<(String, String)> = pairs.iter().map(|p| (p.a.clone(), p.b.clone())).collect();
    assert_eq!(
        names,
        vec![
            ("copy_of_c".into(), "http_get_json".into()),
            ("read_file".into(), "read_text_file".into())
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn find_tools_and_the_background_check_flag_duplicates() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-dup-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-dup-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    for (name, desc) in [
        ("read_file", "Read a text file"),
        ("file_read", "Read the text of a file"),
    ] {
        let out = daemon
            .call_tool_direct(&sid, "create_tool", serde_json::json!({
                "name": name, "description": desc,
                "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}},
                "output_schema": {}, "implementation_js": format!("function run(i){{ return '{name}'; }}"),
                "capabilities": []}))
            .await;
        assert!(out.contains("\"created\""), "{out}");
    }
    let found: serde_json::Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &sid,
                "find_tools",
                serde_json::json!({"query": "read file"}),
            )
            .await,
    )
    .unwrap();
    let hit = found["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "read_file")
        .unwrap();
    assert_eq!(hit["similar_tools"], serde_json::json!(["file_read"]));
    assert!(hit["merge_hint"].is_string());
    assert_eq!(daemon.flag_duplicates(), 1);
    assert_eq!(daemon.flag_duplicates(), 0, "flagged once");
    let dupes: serde_json::Value = serde_json::from_str(
        &daemon
            .call_tool_direct(
                &sid,
                "read_metrics",
                serde_json::json!({"kind": "duplicates"}),
            )
            .await,
    )
    .unwrap();
    assert_eq!(
        dupes["duplicates"][0]["tools"],
        serde_json::json!(["file_read", "read_file"])
    );
}
