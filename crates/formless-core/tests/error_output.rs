//! Declared error fields are domain data, across live and replay Invocations.
#[path = "support/env.rs"]
mod credential_env;

use formless_core::{Config, Paths, Store};
use formless_runtime::{new_overlay, Capabilities};
use serde_json::{json, Value};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn declared_error_outputs_succeed_and_nested_calls_resolve() {
    let _env = credential_env::EnvGuard::isolated();
    let base = std::env::temp_dir().join(format!("fl-error-output-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let schema = json!({"type":"object", "properties": {
        "valid":{"type":"boolean"}, "error":{"type":"string"}
    }, "required":["valid", "error"]});
    let code = "function run() { return {valid: false, error: 'Unexpected token'}; }";
    let wrapper = "async function run() { return await tools.call('check_json_syntax', {}); }";
    for (name, js) in [("check_json_syntax", code), ("check_json_wrapper", wrapper)] {
        // Creation exercises the smoke gate; a legitimate negative result must save.
        let (out, ok) = daemon.call_tool_direct_outcome(&sid, "create_tool", json!({
            "name":name, "description":"Check JSON syntax", "input_schema":{"type":"object"},
            "output_schema":schema, "implementation_js":js, "capabilities":[]
        })).await;
        assert!(ok, "{name} creation failed: {out}");
        assert!(out.contains("\"created\""), "{out}");
    }
    let expected = json!({"valid":false,"error":"Unexpected token"});
    for name in ["check_json_syntax", "check_json_wrapper"] {
        let (out, ok) = daemon.call_tool_direct_outcome(&sid, name, json!({})).await;
        assert!(ok, "{name}: {out}");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), expected);
    }
    for (name, count) in [("check_json_syntax", 2), ("check_json_wrapper", 1)] {
        let (tool, _) = store.get_tool_by_name(name).unwrap().unwrap();
        assert_eq!(
            store.windowed_success(&tool.id, 100).unwrap(),
            (count, Some(1.0))
        );
    }
    let (out, ok) = daemon
        .call_tool_direct_outcome(
            &sid,
            "test_tool",
            json!({
                "name":"check_json_syntax", "input":{}
            }),
        )
        .await;
    assert!(ok, "{out}");
    let tested: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(tested["output"], expected);
    assert_eq!(tested["output_matches_schema"], json!(true));
    // Optimizer replay must retain the same interpretation, including nested calls.
    assert!(
        daemon
            .replay_one(
                "check_json_wrapper",
                wrapper,
                &Capabilities::default(),
                64,
                &schema.to_string(),
                json!({}),
                None,
                0,
                new_overlay()
            )
            .await
    );
    drop(daemon);
    drop(store);
    std::fs::remove_dir_all(base).unwrap();
}

/// Composition and references must behave identically at creation, through a
/// nested Tool, and in the recorded success metrics.
#[tokio::test(flavor = "multi_thread")]
async fn composed_error_declarations_remain_domain_data() {
    let _env = credential_env::EnvGuard::isolated();
    let base = std::env::temp_dir().join(format!("fl-composed-error-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let declaration =
        json!({"type":"object", "properties":{"error":{"type":"string"}}, "required":["error"]});
    for (tag, schema) in [
        ("allof", json!({"allOf":[declaration]})),
        (
            "reference",
            json!({"$defs":{"result":declaration}, "$ref":"#/$defs/result"}),
        ),
    ] {
        let direct = format!("diagnostic_{tag}");
        let nested = format!("nested_{tag}");
        let wrapper =
            format!("async function run() {{ return await tools.call('{direct}', {{}}); }}");
        for (name, code) in [
            (
                direct.as_str(),
                "function run() { return {error:'negative finding'}; }",
            ),
            (nested.as_str(), wrapper.as_str()),
        ] {
            let (out, ok) = daemon.call_tool_direct_outcome(&sid, "create_tool", json!({
                "name":name, "description":"Inspect domain diagnostic", "input_schema":{"type":"object"},
                "output_schema":schema, "implementation_js":code, "capabilities":[]
            })).await;
            assert!(ok && out.contains("\"created\""), "{name}: {out}");
        }
        for name in [&direct, &nested] {
            let (out, ok) = daemon.call_tool_direct_outcome(&sid, name, json!({})).await;
            assert!(ok, "{name}: {out}");
            assert_eq!(
                serde_json::from_str::<Value>(&out).unwrap(),
                json!({"error":"negative finding"})
            );
        }
        for (name, count) in [(&direct, 2), (&nested, 1)] {
            let (tool, _) = store.get_tool_by_name(name).unwrap().unwrap();
            assert_eq!(
                store.windowed_success(&tool.id, 100).unwrap(),
                (count, Some(1.0))
            );
        }
    }
    drop(daemon);
    drop(store);
    std::fs::remove_dir_all(base).unwrap();
}
