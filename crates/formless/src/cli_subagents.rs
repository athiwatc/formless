//! Read the existing unpaged stored reports without reconstructing messages.
use anyhow::{Context, Result};
use formless_protocol::Request;
use serde_json::Value;
use std::io::Write;

fn record(agent: &Value) -> String {
    let mut text = String::new();
    for (label, key) in [
        ("Id", "id"),
        ("Session", "session_id"),
        ("Run", "run_id"),
        ("Status", "status"),
        ("Model", "model"),
        ("Task", "task"),
        ("Created", "created_at"),
        ("Updated", "updated_at"),
    ] {
        text.push_str(&format!(
            "{label}: {}\n",
            crate::comparison_terminal_text(agent[key].as_str().unwrap_or("(not reported)"))
        ));
    }
    if let Some(served) = agent.get("served_model").filter(|value| value.is_object()) {
        let name = served["name"].as_str().unwrap_or("(not reported)");
        let via = served["via"].as_str().unwrap_or("(not reported)");
        text.push_str(&format!(
            "Served model: {} (via {})\n",
            crate::comparison_terminal_text(name),
            crate::comparison_terminal_text(via)
        ));
    }
    for (label, key) in [("Depth", "depth"), ("Steps", "steps")] {
        text.push_str(&format!(
            "{label}: {}\n",
            agent[key]
                .as_i64()
                .map_or_else(|| "(not reported)".into(), |number| number.to_string())
        ));
    }
    match agent["result"].as_str() {
        None => text.push_str("Result: (not reported)\n"),
        Some("") => text.push_str("Result: (empty)\n"),
        Some(result) => {
            text.push_str("Result:\n");
            for line in result.split('\n') {
                text.push_str("  ");
                text.push_str(&crate::comparison_terminal_text(line));
                text.push('\n');
            }
        }
    }
    text
}

pub(crate) fn list(
    session: String,
    json: bool,
    mut fetch: impl FnMut(Request) -> Result<Value>,
    out: &mut impl Write,
) -> Result<()> {
    let value = fetch(Request::ListSubagents {
        session_id: session,
    })?;
    let text = if json {
        format!("{}\n", serde_json::to_string_pretty(&value)?)
    } else {
        let agents = value
            .as_array()
            .context("daemon sub-agent inventory was not an array")?;
        let mut text = if agents.is_empty() {
            "no stored sub-agents reported for this Session\n".into()
        } else {
            String::from("Stored sub-agents:\n")
        };
        for agent in agents {
            text.push('\n');
            text.push_str(&record(agent));
        }
        text
    };
    out.write_all(text.as_bytes())?;
    Ok(())
}

pub(crate) fn detail(
    id: String,
    json: bool,
    mut fetch: impl FnMut(Request) -> Result<Value>,
    out: &mut impl Write,
) -> Result<()> {
    let value = fetch(Request::GetSubagent { id })?;
    let text = if json {
        format!("{}\n", serde_json::to_string_pretty(&value)?)
    } else {
        let agent = value
            .get("subagent")
            .filter(|v| v.is_object())
            .context("daemon sub-agent detail did not contain a sub-agent record")?;
        let messages = value
            .get("messages")
            .filter(|v| v.is_array())
            .context("daemon sub-agent detail did not contain a messages array")?;
        format!(
            "{}\nMessages (unpaged transcript returned by daemon):\n{}\n",
            record(agent),
            crate::escape_json_terminal_controls(&serde_json::to_string_pretty(messages)?)
        )
    };
    out.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    fn agent(id: &str, result: Value) -> Value {
        json!({"id":id,"session_id":"session-full","run_id":"run-full","task":"Task ไทย\nsecond line","depth":2,"model":"role · 日本語","status":if result.is_null(){"running"}else{"completed"},"steps":3,"result":result,"created_at":"first-time","updated_at":"last-time","future":7})
    }

    #[test]
    fn parser_exposes_both_subagent_reports_and_json() {
        for (verb, arg) in [
            ("subagents", "Session name"),
            ("subagent", "exact-subagent-id"),
        ] {
            let cli =
                crate::Cli::try_parse_from(["formless", "sessions", verb, arg, "--json"]).unwrap();
            match cli.cmd {
                Some(crate::Cmd::Sessions {
                    action: crate::SessionsAction::Subagents { id, json },
                }) => {
                    assert_eq!(id, arg);
                    assert!(json);
                }
                Some(crate::Cmd::Sessions {
                    action: crate::SessionsAction::Subagent { id, json },
                }) => {
                    assert_eq!(id, arg);
                    assert!(json);
                }
                _ => panic!("sub-agent action"),
            }
        }
    }

    #[test]
    fn human_reports_escape_controls_in_multiline_results_and_transcripts() {
        let result = "first\nsecond \u{1b} \u{9b} \u{202e}";
        let mut subagent = agent("fixture", json!(result));
        subagent["served_model"] = json!({
            "name":"fallback-model",
            "via":"fallback",
            "requested":"requested-model"
        });
        let messages = json!([{"role":"assistant", "content":result, "key\u{202e}":true}]);
        let report = json!({"subagent":subagent,"messages":messages});
        let mut inventory = Vec::new();
        list(
            "session".into(),
            false,
            |_| Ok(json!([subagent])),
            &mut inventory,
        )
        .unwrap();
        let mut detail_text = Vec::new();
        detail(
            "fixture".into(),
            false,
            |_| Ok(report.clone()),
            &mut detail_text,
        )
        .unwrap();
        let inventory = String::from_utf8(inventory).unwrap();
        let detail_text = String::from_utf8(detail_text).unwrap();
        for text in [&inventory, &detail_text] {
            assert!(
                text.contains("Result:\n  first\n  second \\u{1b} \\u{9b} \\u{202e}"),
                "{text}"
            );
            assert!(
                text.contains("Served model: fallback-model (via fallback)"),
                "{text}"
            );
            assert!(!text.contains(['\u{1b}', '\u{9b}', '\u{202e}']), "{text}");
        }
        let (_, transcript) = detail_text
            .split_once("Messages (unpaged transcript returned by daemon):\n")
            .unwrap();
        assert!(transcript.contains("\\u001b \\u009b \\u202e"));
        assert_eq!(serde_json::from_str::<Value>(transcript).unwrap(), messages);
        let mut machine = Vec::new();
        detail("fixture".into(), true, |_| Ok(report.clone()), &mut machine).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&machine).unwrap(), report);
    }

    #[test]
    fn reports_retain_full_ids_null_empty_results_and_provider_messages() {
        let id = "subagent-".to_owned() + &"日本語".repeat(100);
        let inventory = json!([agent(&id, Value::Null), agent("second", json!(""))]);
        let messages = json!([{"role":"assistant","content":[{"type":"thinking","thinking":"ไทย"},{"type":"tool_use","opaque":{"x":[1,"🦀"]}}],"provider-field":7},{"unfamiliar":"retained"}]);
        let report = json!({"subagent":inventory[0],"messages":messages,"future":["preserved"]});
        for json in [false, true] {
            let mut bytes = Vec::new();
            let mut calls = 0;
            list("session-full".into(),json,|req|{calls+=1;assert!(matches!(req,Request::ListSubagents{session_id} if session_id=="session-full"));Ok(inventory.clone())},&mut bytes).unwrap();
            assert_eq!(calls, 1);
            if json {
                assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), inventory);
            } else {
                let text = String::from_utf8(bytes).unwrap();
                assert!(text.contains(&format!("Id: {id}\n")));
                assert!(text.contains("Result: (not reported)"));
                assert!(text.contains("Result: (empty)"));
                assert!(text.find(&id).unwrap() < text.find("Id: second").unwrap());
                assert!(text.contains("Depth: 2"));
                assert!(text.contains("Steps: 3"));
                assert!(text.contains("first-time"));
            }
            let mut bytes = Vec::new();
            detail(
                id.clone(),
                json,
                |req| {
                    assert!(matches!(req,Request::GetSubagent{id:got} if got==id));
                    Ok(report.clone())
                },
                &mut bytes,
            )
            .unwrap();
            if json {
                assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), report);
            } else {
                let text = String::from_utf8(bytes).unwrap();
                let (_, array) = text
                    .split_once("Messages (unpaged transcript returned by daemon):\n")
                    .unwrap();
                assert_eq!(serde_json::from_str::<Value>(array).unwrap(), messages);
            }
        }
        let mut bytes = Vec::new();
        list("s".into(), false, |_| Ok(json!([])), &mut bytes).unwrap();
        assert!(String::from_utf8(bytes)
            .unwrap()
            .contains("no stored sub-agents reported"));
        for json in [false, true] {
            let mut bytes = Vec::new();
            assert!(list(
                "s".into(),
                json,
                |_| anyhow::bail!("inventory failure"),
                &mut bytes
            )
            .is_err());
            assert!(bytes.is_empty());
            assert!(detail(id.clone(), json, |_| anyhow::bail!("not found"), &mut bytes).is_err());
            assert!(bytes.is_empty());
        }
    }
}
