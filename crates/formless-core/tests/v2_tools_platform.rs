//! v2 Tools platform (SPEC §7/§17): shared JS modules end-to-end
//! (storage -> runtime `requireModule`) and sub-agent capability gating.

use formless_core::{Paths, Store};
use formless_runtime::{
    execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, HostOps, NoHostOps,
};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

struct MockAgent;
impl HostOps for MockAgent {
    fn run_subagent(
        &self,
        task: &str,
        _system: Option<&str>,
        model: Option<&str>,
    ) -> Result<String, String> {
        Ok(format!("sub[{}]: {task}", model.unwrap_or("default")))
    }
}

fn run(
    code: &str,
    caps: &[&str],
    modules: Vec<(String, String)>,
    ops: Arc<dyn HostOps>,
) -> Result<Value, formless_runtime::ExecError> {
    let req = ExecRequest {
        code: code.to_string(),
        input: json!({}),
        caps: Capabilities::parse(&caps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
        memory_mb: 256,
        mode: HostMode::Live,
        overlay: None,
        activity: None,
        modules,
        cancel: Arc::new(AtomicBool::new(false)),
    };
    execute(&req, ops).map(|o| o.output)
}

#[test]
fn shared_modules_store_version_and_load_in_tools() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-modules-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-modules-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let (info, v) = store
        .create_module_version(
            "strings",
            "helpers",
            "module.exports = { shout: (s) => String(s).toUpperCase() + '!' };",
            "agent",
        )
        .unwrap();
    assert_eq!(v, 1);
    let (_, v2) = store
        .create_module_version(
            "strings",
            "helpers v2",
            "const inner = requireModule('base'); module.exports = { shout: inner.shout };",
            "agent",
        )
        .unwrap();
    assert_eq!(v2, 2);
    store
        .create_module_version(
            "base",
            "",
            "exports.shout = (s) => String(s).toUpperCase() + '?'",
            "agent",
        )
        .unwrap();
    assert_eq!(info.current_version, 1);
    assert_eq!(store.list_modules().unwrap().len(), 2);

    // A Tool loads the *current* module version, which itself loads another.
    let out = run(
        "function run(){ const s = requireModule('strings'); return { v: s.shout('hey'), list: modules.list() }; }",
        &[],
        store.list_module_code().unwrap(),
        Arc::new(NoHostOps),
    )
    .unwrap();
    assert_eq!(out["v"], "HEY?");
    assert!(out["list"].as_array().unwrap().len() == 2);

    let err = run(
        "function run(){ return requireModule('nope'); }",
        &[],
        store.list_module_code().unwrap(),
        Arc::new(NoHostOps),
    )
    .unwrap_err();
    assert!(err.message.contains("unknown module"), "{err:?}");
}

#[test]
fn agents_run_is_gated_scoped_and_stubbed() {
    let code = "function run(){ return { t: agents.run('do the thing') }; }";
    let out = run(code, &["agent"], vec![], Arc::new(MockAgent)).unwrap();
    assert_eq!(out["t"], "sub[default]: do the thing");
    let out = run(code, &["agent:m1"], vec![], Arc::new(MockAgent)).unwrap();
    assert_eq!(out["t"], "sub[m1]: do the thing");
    let err = run(code, &[], vec![], Arc::new(MockAgent)).unwrap_err();
    assert_eq!(err.kind, ExecErrorKind::Denied);
    // Capability summary round-trips and is sensitive.
    let caps = Capabilities::parse(&["agent:m1".into()]).unwrap();
    assert!(caps.is_sensitive());
    assert!(caps.to_specs().contains(&"agent:m1".to_string()));
}
