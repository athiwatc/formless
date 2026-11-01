//! Real private SQLite writes: classification gates persistence, never execution.
use super::*;
use crate::tool_compatibility::{SchemaChangeBlocked, SchemaWritePolicy};
fn old_input() -> &'static str {
    r#"{"type":"object","properties":{"x":{"type":"string"}},"required":["x"]}"#
}
fn new_input() -> &'static str {
    r#"{"type":"object","properties":{"x":{"type":"string"},"extra":{"type":"string"}},"required":["x","extra"]}"#
}
fn tool(st: &Store, name: &str, code: &str) -> (ToolInfo, i64) {
    st.create_tool_version(
        name,
        "Private schema fixture",
        old_input(),
        "{}",
        code,
        &Capabilities::default(),
        64,
        "user",
    )
    .unwrap()
}
fn blocked(st: &Store) -> anyhow::Error {
    st.create_tool_version(
        "target",
        "Private schema fixture",
        new_input(),
        "{}",
        "function run(i){return i}",
        &Capabilities::default(),
        64,
        "user",
    )
    .unwrap_err()
}
#[test]
fn actual_store_blocks_uncertain_valid_javascript_callers_without_trusting_author_labels() {
    for code in [
        "function run(i){return tools.call('target',i)}",
        "function run(i){return tools.call ('target',i)}",
        "function run(i){return tools . call('target',i)}",
        "function run(i){const call=tools.call;return call('target',i)}",
        "function run(i){const {call}=tools;return call('target',i)}",
        "function run(i){return tools['call']('target',i)}",
        r"function run(i){return \u0074ools.call('target',i)}",
        r"function run(i){return tools['\u0063all']('target',i)}",
        "function run(i){return globalThis['too'+'ls'].call('target',i)}",
        r#"function run(i){return []["filter"]["con"+"structor"]("return t"+"ools")().call("target",i)}"#,
    ] {
        formless_runtime::check_syntax(code).unwrap();
        let st = Store::open_in_memory().unwrap();
        let (target, _) = tool(&st, "target", "function run(i){return i}");
        let (caller, _) = tool(&st, "caller", code);
        let error = blocked(&st);
        let impact = &error
            .downcast_ref::<SchemaChangeBlocked>()
            .expect("actual write must be gated")
            .0;
        assert!(impact
            .affected_dependents
            .iter()
            .any(|d| d.kind == "tool" && d.id == caller.id));
        assert_eq!(
            st.get_tool_current(&target.id)
                .unwrap()
                .unwrap()
                .0
                .current_version,
            1
        );
        let (_, version) = st
            .create_tool_version_with_schema_policy(
                "target",
                "Private schema fixture",
                new_input(),
                "{}",
                "function run(i){return i}",
                &Capabilities::default(),
                64,
                "not-an-authority",
                SchemaWritePolicy::Agent {
                    allow_breaking: true,
                },
            )
            .unwrap();
        assert_eq!(version, 2);
    }
}
#[test]
fn module_delegation_and_active_historical_callers_remain_live_consumers() {
    let st = Store::open_in_memory().unwrap();
    let (target, _) = tool(&st, "target", "function run(i){return i}");
    st.create_module_version(
        "adapter",
        "Private adapter",
        "module.exports.run=(i)=>tools.call('target',i)",
        "user",
    )
    .unwrap();
    let (wrapper, _) = tool(
        &st,
        "wrapper",
        "function run(i){return requireModule('adapter').run(i)}",
    );
    assert!(blocked(&st)
        .downcast_ref::<SchemaChangeBlocked>()
        .unwrap()
        .0
        .affected_dependents
        .iter()
        .any(|d| d.id == wrapper.id && d.certainty == "possible_dynamic"));
    st.set_tool_disabled(&wrapper.id, true).unwrap();
    let (caller, _) = tool(
        &st,
        "old_caller",
        "function run(i){return tools.call('target',i)}",
    );
    let invocation = st
        .start_invocation(&caller.id, 1, None, None, None, "{}")
        .unwrap();
    tool(&st, "old_caller", "function run(i){return i}");
    st.set_tool_retired(&caller.id, true).unwrap();
    let error = blocked(&st);
    let report = &error.downcast_ref::<SchemaChangeBlocked>().unwrap().0;
    assert!(report
        .affected_dependents
        .iter()
        .any(|d| d.kind == "active_invocation" && d.id == invocation && d.version == Some(1)));
    st.finish_invocation(&invocation, &caller.id, "success", "", "{}", "", 1, 100)
        .unwrap();
    assert!(!st
        .schema_change_report(&target.id, new_input(), "{}")
        .unwrap()
        .requires_acknowledgement());
}
#[test]
fn script_rows_are_correlated_and_jobs_and_definition_pins_are_truthfully_distinguished() {
    let st = Store::open_in_memory().unwrap();
    let (target, _) = tool(&st, "target", "function run(i){return i}");
    let session = st.create_session(None).unwrap();
    let run = st.create_run(&session.id).unwrap();
    let uses = vec!["target".into()];
    st.record_script(
        "script",
        &session.id,
        &run,
        "shape",
        "return tools.target(input)",
        &uses,
        "running",
        "",
        0,
        "",
    )
    .unwrap();
    let report = st
        .schema_change_report(&target.id, new_input(), "{}")
        .unwrap();
    assert!(report
        .affected_dependents
        .iter()
        .any(|d| d.kind == "script" && d.id == "script" && d.certainty == "declared"));
    assert!(st
        .record_script(
            "script",
            &session.id,
            &run,
            "other",
            "altered",
            &uses,
            "success",
            "",
            1,
            ""
        )
        .is_err());
    st.record_script(
        "script",
        &session.id,
        &run,
        "shape",
        "return tools.target(input)",
        &uses,
        "success",
        "",
        1,
        "",
    )
    .unwrap();
    let report = st
        .schema_change_report(&target.id, new_input(), "{}")
        .unwrap();
    assert!(report
        .affected_dependents
        .iter()
        .any(|d| d.kind == "script"
            && d.resolution == "stored_source_eligible_for_reuse_or_promotion"));
    st.pin_definition(&session.id, &target.id, 1, "{}").unwrap();
    let job = st
        .create_job(
            "Unbound future prompt",
            None,
            None,
            "2099-01-01T00:00:00Z",
            true,
            "user",
        )
        .unwrap();
    let report = st
        .schema_change_report(&target.id, new_input(), "{}")
        .unwrap();
    assert!(report
        .affected_dependents
        .iter()
        .any(|d| d.kind == "session_definition"
            && d.resolution == "definition_only_pin_calls_current"));
    assert!(report
        .affected_dependents
        .iter()
        .any(|d| d.kind == "scheduled_job" && d.id == job.id && d.certainty == "possible_dynamic"));
    let digest = report.context_digest().unwrap();
    st.set_job_enabled(&job.id, false).unwrap();
    assert_ne!(
        st.schema_change_report(&target.id, new_input(), "{}")
            .unwrap()
            .context_digest()
            .unwrap(),
        digest
    );
}
