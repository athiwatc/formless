//! Current note fields associated with a selector, through RunMemories only.
use anyhow::{Context, Result};
use formless_protocol::Request;
use serde_json::Value;
use std::io::Write;

pub(crate) enum Selector {
    Session { id: String, name: String },
    Run(String),
}

pub(crate) fn used(
    selector: Selector,
    json: bool,
    mut fetch: impl FnMut(Request) -> Result<Value>,
    out: &mut impl Write,
) -> Result<()> {
    let (request, heading) = match selector {
        Selector::Session { id, name } => (
            Request::RunMemories {
                run_id: None,
                session_id: Some(id.clone()),
            },
            format!("Latest-Run memory report for Session {name} ({id})"),
        ),
        Selector::Run(id) => (
            Request::RunMemories {
                run_id: Some(id.clone()),
                session_id: None,
            },
            format!("Memory report for requested Run id {id}"),
        ),
    };
    let value = fetch(request)?;
    let text = if json {
        format!("{}\n", serde_json::to_string_pretty(&value)?)
    } else {
        let notes = value
            .as_array()
            .context("daemon Run memory report was not an array")?;
        let mut text = format!("{heading}\nCurrent note fields; report may be incomplete.\n");
        if notes.is_empty() {
            text.push_str("no carried memories reported for this selector\n");
        }
        for note in notes {
            let source = if let Some(session) = note["session_id"].as_str() {
                format!("from session {session}")
            } else {
                note["author"].as_str().unwrap_or_default().into()
            };
            text.push_str(&format!(
                "{:<10} {:<8} {:<20} {}  ({source})\n",
                note["id"].as_str().unwrap_or_default(),
                if note["enabled"] == true { "on" } else { "off" },
                note["scope"].as_str().unwrap_or_default(),
                note["text"].as_str().unwrap_or_default()
            ));
        }
        text
    };
    out.write_all(text.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    #[test]
    fn exactly_one_memory_used_selector_is_required_by_the_parser() {
        for args in [
            vec!["formless", "memory", "used"],
            vec!["formless", "memory", "used", "--session", "s", "--run", "r"],
        ] {
            let error = crate::Cli::try_parse_from(args).err().expect("usage error");
            assert_eq!(error.exit_code(), 2);
        }
        for (flag, value) in [
            ("--session", "Session ไทย"),
            ("--run", "exact-unmatched-run"),
        ] {
            let cli =
                crate::Cli::try_parse_from(["formless", "memory", "used", flag, value, "--json"])
                    .unwrap();
            let Some(crate::Cmd::Memory {
                action: crate::MemoryAction::Used { session, run, json },
            }) = cli.cmd
            else {
                panic!("used")
            };
            assert!(json);
            assert_eq!(session.as_deref(), (flag == "--session").then_some(value));
            assert_eq!(run.as_deref(), (flag == "--run").then_some(value));
        }
    }

    #[test]
    fn current_note_reports_keep_order_disabled_notes_full_text_and_unmatched_ids() {
        let report = json!([
            {"id":"first","text":"Unicode ไทย\nsecond line","scope":"project:/日本語","enabled":false,"author":"agent","session_id":"日本語-source","last_used":"other-run-time","future":7},
            {"id":"second","text":"x".repeat(1000),"scope":"global","enabled":true,"author":"user","session_id":null}
        ]);
        for json in [false, true] {
            let mut bytes = Vec::new();
            let mut calls = 0;
            used(Selector::Run("retained-association-missing-run".into()),json,|req| {
                calls+=1;assert!(matches!(req,Request::RunMemories{run_id:Some(id),session_id:None} if id=="retained-association-missing-run"));Ok(report.clone())
            },&mut bytes).unwrap();
            assert_eq!(calls, 1);
            if json {
                assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), report);
            } else {
                let text = String::from_utf8(bytes).unwrap();
                assert!(text.contains("requested Run id retained-association-missing-run"));
                assert!(text.contains("report may be incomplete"));
                assert!(text.contains("Unicode ไทย\nsecond line"));
                assert!(text.contains(&"x".repeat(1000)));
                assert!(text.contains("off"));
                assert!(text.contains("from session 日本語-source"));
                assert!(text.contains("(user)"));
                assert!(!text.contains("other-run-time"));
                assert!(text.find("first").unwrap() < text.find("second ").unwrap());
            }
        }
        let mut bytes = Vec::new();
        used(Selector::Session{id:"full-session".into(),name:"Current name".into()},false,|req|{
            assert!(matches!(req,Request::RunMemories{run_id:None,session_id:Some(id)} if id=="full-session"));Ok(json!([]))
        },&mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("Latest-Run memory report for Session Current name (full-session)"));
        assert!(text.contains("no carried memories reported for this selector"));
        let mut bytes = Vec::new();
        used(
            Selector::Run("unmatched".into()),
            true,
            |_| Ok(json!([])),
            &mut bytes,
        )
        .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), json!([]));
        let mut bytes = Vec::new();
        let error = used(
            Selector::Run("failed".into()),
            false,
            |_| anyhow::bail!("daemon failure"),
            &mut bytes,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "daemon failure");
        assert!(bytes.is_empty());
    }
}
