//! Read the daemon's existing hypothetical proposal comparison.
use super::*;

pub(super) fn show(paths: &Paths, id: String, json: bool) -> Result<()> {
    let value = formless_tui::client::request_once(paths, Request::ProposalDiff { id })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print!("{}", describe(&value)?);
    }
    Ok(())
}

fn describe(value: &Value) -> Result<String> {
    use std::fmt::Write;
    let target = value["target"]
        .as_str()
        .context("proposal comparison target was not text")?;
    let applied = value["applied"]
        .as_bool()
        .context("proposal comparison applied state was not a boolean")?;
    let rows = value["diff"]
        .as_array()
        .context("proposal comparison diff was not an array")?;
    let mut out = format!(
        "Proposal comparison\nTarget: {}\nAlready applied: {}\n\n",
        comparison_terminal_text(target),
        if applied { "yes" } else { "no" }
    );
    for row in rows {
        let pair = row
            .as_array()
            .filter(|pair| pair.len() == 2)
            .context("proposal comparison row was not an operation/line pair")?;
        let operation = pair[0]
            .as_str()
            .context("proposal comparison operation was not text")?;
        let line = pair[1]
            .as_str()
            .context("proposal comparison line was not text")?;
        writeln!(
            out,
            "{}{}",
            comparison_terminal_text(operation),
            comparison_terminal_text(line)
        )?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_parser_and_human_report_preserve_order_and_complete_lines() {
        for json in [false, true] {
            let mut args = vec!["formless", "prompts", "diff", "proposal-id"];
            if json {
                args.push("--json");
            }
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(
                matches!(cli.cmd,Some(Cmd::Prompts{action:PromptsAction::Diff{id,json:parsed}}) if id=="proposal-id"&&parsed==json)
            );
        }
        let long = "日本語 ไทย ".repeat(100);
        let text=describe(&serde_json::json!({"target":"learned guidance","applied":false,"diff":[["-","old"],["+",long],[" ",""]]})).unwrap();
        assert!(text.ends_with(&format!("-old\n+{long}\n \n")));
        assert!(
            describe(&serde_json::json!({"target":"memory","applied":true,"diff":[]}))
                .unwrap()
                .contains("Already applied: yes")
        );
        assert!(describe(&serde_json::json!({"target":"memory","applied":false,"diff":[["+","first"],["broken"]]})).is_err());
    }
}
