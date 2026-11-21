use super::*;
use serde_json::json;

fn msg(role: &str, content: &str, meta: Value) -> MessageInfo {
    MessageInfo {
        generated_by: None,
        id: "fixture".into(),
        session_id: "fixture".into(),
        seq: 1,
        role: role.into(),
        content: content.into(),
        created_at: "fixture".into(),
        meta: Some(meta.to_string()),
    }
}
fn call() -> MessageInfo {
    msg(
        "assistant",
        "",
        json!({"tool_calls":[{"id":"c","function":{"name":"original","arguments":"{}"}}]}),
    )
}
fn outputs(messages: &[MessageInfo]) -> [String; 2] {
    [markdown("fixture", messages), html("fixture", messages)]
}

#[test]
fn matched_export_results_mark_only_explicit_true_without_text_sniffing() {
    let baseline = outputs(&[
        call(),
        msg("tool", "error: unchanged ไทย", json!({"tool_call_id":"c"})),
    ]);
    for flag in [
        Value::Null,
        json!(false),
        json!("true"),
        json!(1),
        json!({}),
    ] {
        assert_eq!(
            outputs(&[
                call(),
                msg(
                    "tool",
                    "error: unchanged ไทย",
                    json!({"tool_call_id":"c","is_error":flag,"name":"ignored"})
                )
            ]),
            baseline
        );
    }
    for output in baseline {
        assert!(output.contains("Tool: original</summary>"));
        assert!(!output.contains("(failed)"));
    }
    for output in outputs(&[
        call(),
        msg(
            "tool",
            "neutral result",
            json!({"tool_call_id":"c","is_error":true,"name":"ignored"}),
        ),
    ]) {
        assert!(output.contains("Tool: original (failed)</summary>"));
        assert!(output.contains("neutral result"));
        assert!(!output.contains("ignored"));
    }
}

#[test]
fn orphan_export_name_precedence_uses_only_strings_and_existing_escaping() {
    for (meta, name) in [
        (
            json!({"data_source_tool":"<ไทย>&\"","name":"ignored","is_error":true}),
            "<ไทย>&\"",
        ),
        (json!({"data_source_tool":2,"name":"named"}), "named"),
        (json!({"data_source_tool":"","name":"ignored"}), ""),
        (json!({"name":"stored"}), "stored"),
        (json!({"data_source_tool":{},"name":false}), "tool"),
    ] {
        for output in outputs(&[msg("tool", "result", meta.clone())]) {
            let suffix = if meta["is_error"] == true {
                " (failed)"
            } else {
                ""
            };
            assert!(
                output.contains(&format!("Tool: {}{suffix}</summary>", escape(name))),
                "{output}"
            );
            assert!(!output.contains("<ไทย>"));
        }
    }
}

#[test]
fn replacement_results_replace_text_and_flag_together_without_reordering_orphans() {
    let first = msg("tool", "first", json!({"tool_call_id":"c","is_error":true}));
    let last = msg("tool", "last", json!({"tool_call_id":"c"}));
    assert_eq!(
        outputs(&[call(), first.clone(), last.clone()]),
        outputs(&[call(), last])
    );
    let orphan = msg(
        "tool",
        "earlier orphan",
        json!({"tool_call_id":"c","name":"orphan","is_error":true}),
    );
    let later = msg("tool", "later matched", json!({"tool_call_id":"c"}));
    for output in outputs(&[orphan, call(), later]) {
        assert!(
            output.find("Tool: orphan (failed)").unwrap()
                < output.find("Tool: original</summary>").unwrap()
        );
        assert_eq!(output.matches("<summary>").count(), 2);
        assert!(output.contains("earlier orphan") && output.contains("later matched"));
    }
}

#[test]
fn failure_marker_keeps_result_clipping_fences_and_source_messages_unchanged() {
    let content = format!("```ไทย\n{}", "é".repeat(4100));
    let messages = vec![
        call(),
        msg(
            "tool",
            &content,
            json!({"tool_call_id":"c","is_error":true}),
        ),
    ];
    let before = serde_json::to_value(&messages).unwrap();
    let md = markdown("fixture", &messages);
    assert!(md.contains(&fenced("", &clip(&content))));
    assert!(md.contains("Tool: original (failed)</summary>"));
    let html = html("fixture", &messages);
    assert!(html.contains(&escape(&clip(&content))));
    assert_eq!(serde_json::to_value(&messages).unwrap(), before);
}
