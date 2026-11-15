fn msg(role: &str, content: &str, meta: Option<&str>) -> MessageInfo {
    MessageInfo {
        id: String::new(),
        session_id: String::new(),
        seq: 0,
        role: role.into(),
        content: content.into(),
        created_at: String::new(),
        meta: meta.map(Into::into),
    }
}
use super::*;

#[test]
fn markdown_turn_boundaries_keep_spaces_tabs_and_indented_first_content_lines() {
    for content in [
        "    fn main() {\n        call();\n    }",
        "\tfirst\n\tsecond",
        "\n \t\r\n    first\n  second\n\n",
        "plain\ntext\n",
        "ไทย\n  日本語",
        "\n\n",
        "   ",
    ] {
        let expected = match content {
            "\n \t\r\n    first\n  second\n\n" => "    first\n  second",
            "plain\ntext\n" => "plain\ntext",
            "\n\n" | "   " => "",
            text => text,
        };
        for role in ["user", "assistant"] {
            let md = markdown("fixture", &[msg(role, content, None)]);
            let label = if role == "user" { "You" } else { "Assistant" };
            if role == "assistant" && content.trim().is_empty() {
                assert_eq!(md, "# fixture\n");
            } else {
                assert_eq!(md, format!("# fixture\n\n## {label}\n\n{expected}\n"));
            }
        }
    }
}

fn call(args: &str) -> Vec<MessageInfo> {
    vec![msg("assistant","",Some(&serde_json::json!({"tool_calls":[{"id":"fixture-call","function":{"name":"fixture","arguments":args}}]}).to_string())),msg("tool","unchanged result",Some(r#"{"tool_call_id":"fixture-call"}"#))]
}

#[test]
fn markdown_arguments_keep_original_tokens_and_do_not_expand_compact_json() {
    let compact = format!("[{}]", vec!["0"; 1500].join(","));
    assert_eq!(compact.chars().count(), 3001);
    for args in [
        compact.as_str(),
        r#"{"x":1,"x":2,"ไทย":"日本語"}"#,
        "not JSON\n  input",
        r#"{"n":123456789012345678901234567890,"e":1e+09}"#,
    ] {
        let md = markdown("fixture", &call(args));
        assert!(md.contains(&format!("```json\n{args}\n```")), "{md}");
        assert!(!md.contains("… (cut)"));
        assert!(md.contains("Result:\n\n```\nunchanged result\n```"));
    }
}

#[test]
fn markdown_argument_budget_counts_original_scalars_and_html_policy_stays_pretty() {
    for length in [3999, 4000, 4001, 20_000] {
        let args: String = "éไทย👩‍💻".chars().cycle().take(length).collect();
        let md = markdown("fixture", &call(&args));
        let kept: String = args.chars().take(4000).collect();
        let expected = if length <= 4000 {
            args.clone()
        } else {
            format!("{kept}\n… (cut)")
        };
        assert!(md.contains(&format!("```json\n{expected}\n```")));
        assert_eq!(md.contains("… (cut)"), length > 4000);
    }
    for args in [
        r#"{"x":1,"x":2,"a":0}"#,
        "not JSON",
        &format!("[{}]", vec!["0"; 1500].join(",")),
    ] {
        let pretty = serde_json::from_str::<Value>(args)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| args.to_string());
        assert_eq!(html_arguments(args), clip(&pretty));
        let rendered = html("fixture", &call(args));
        assert!(rendered.contains(&escape(&clip(&pretty))));
        assert!(rendered.contains("unchanged result"));
    }
}
