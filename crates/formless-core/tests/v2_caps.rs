//! v2 additional Capabilities (SPEC §7.4/§7.5): clipboard, notify, and
//! LLM calls from inside Tools. Live clipboard/notify are never exercised
//! against the developer's machine — Smoke mode proves gating + stubs.

use formless_runtime::{
    execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, HostOps, NoHostOps,
};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

struct MockLlm;
impl HostOps for MockLlm {
    fn llm_complete(
        &self,
        prompt: &str,
        system: Option<&str>,
        model: Option<&str>,
    ) -> Result<String, String> {
        Ok(format!(
            "llm[{}|{}]: {prompt}",
            system.unwrap_or("-"),
            model.unwrap_or("default")
        ))
    }
}

fn run_with(
    code: &str,
    caps: &[&str],
    mode: HostMode,
    ops: Arc<dyn HostOps>,
) -> Result<Value, formless_runtime::ExecError> {
    let req = ExecRequest {
        output_schema: None,
        code: code.to_string(),
        input: json!({}),
        caps: Capabilities::parse(&caps.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
        memory_mb: 256,
        mode,
        overlay: None,
        activity: None,
        modules: Default::default(),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    execute(&req, ops).map(|o| o.output)
}

#[test]
fn new_capabilities_parse_round_trip_cover_and_sensitive() {
    let caps = Capabilities::parse(&[
        "clipboard".into(),
        "notify".into(),
        "llm:deepseek-flash".into(),
    ])
    .unwrap();
    assert!(caps.clipboard_read && caps.clipboard_write && caps.notify);
    assert!(caps.is_sensitive());
    let specs = caps.to_specs();
    assert!(specs.contains(&"clipboard".to_string()), "{specs:?}");
    assert!(
        specs.contains(&"llm:deepseek-flash".to_string()),
        "{specs:?}"
    );
    let read_only = Capabilities::parse(&["clipboard:read".into()]).unwrap();
    assert!(caps.covers(&read_only));
    assert!(!read_only.covers(&caps));
    assert!(Capabilities::parse(&["clipboard:sideways".into()]).is_err());
}

#[test]
fn llm_complete_is_gated_and_scoped() {
    let code = "function run(){ return { t: llm.complete('hello', {system: 'be terse'}) }; }";
    // Unscoped grant reaches the host with the default model.
    let out = run_with(code, &["llm"], HostMode::Live, Arc::new(MockLlm)).unwrap();
    assert_eq!(out["t"], "llm[be terse|default]: hello");
    // Scoped grant defaults to the granted model.
    let out = run_with(
        code,
        &["llm:deepseek-flash"],
        HostMode::Live,
        Arc::new(MockLlm),
    )
    .unwrap();
    assert_eq!(out["t"], "llm[be terse|deepseek-flash]: hello");
    // Out-of-scope model is denied before reaching the host.
    let err = run_with(
        "function run(){ return llm.complete('x', {model: 'other'}); }",
        &["llm:deepseek-flash"],
        HostMode::Live,
        Arc::new(MockLlm),
    )
    .unwrap_err();
    assert_eq!(err.kind, ExecErrorKind::Denied);
    assert!(err.message.contains("denied:"), "{err:?}");
    // No grant at all is denied.
    let err = run_with(code, &[], HostMode::Live, Arc::new(MockLlm)).unwrap_err();
    assert!(err.message.contains("denied:"), "{err:?}");
    // Smoke stubs, replay denies (never touches a real provider).
    let out = run_with(code, &["llm"], HostMode::Smoke, Arc::new(NoHostOps)).unwrap();
    assert_eq!(out["t"], "");
    assert!(run_with(code, &["llm"], HostMode::Replay, Arc::new(MockLlm)).is_err());
}

#[test]
fn clipboard_and_notify_are_gated_and_stubbed_in_smoke() {
    let code =
        "function run(){ clipboard.write('x'); notify('t','b'); return { r: clipboard.read() }; }";
    let out = run_with(
        code,
        &["clipboard", "notify"],
        HostMode::Smoke,
        Arc::new(NoHostOps),
    )
    .unwrap();
    assert_eq!(out["r"], "");
    // Split grants: read without write fails on write.
    let err = run_with(
        code,
        &["clipboard:read", "notify"],
        HostMode::Smoke,
        Arc::new(NoHostOps),
    )
    .unwrap_err();
    assert!(err.message.contains("clipboard:write"), "{err:?}");
    // Replay never touches the real clipboard/notifications.
    assert!(run_with(
        code,
        &["clipboard", "notify"],
        HostMode::Replay,
        Arc::new(NoHostOps)
    )
    .is_err());
}
