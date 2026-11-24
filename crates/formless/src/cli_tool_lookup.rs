//! Exact selectors remain exact; fuzzy names only help recover from a miss.
use super::*;
use formless_protocol::ToolSummary;

pub(super) fn resolve<'a>(tools: &'a [ToolSummary], query: &str) -> Result<&'a ToolSummary> {
    tools
        .iter()
        .find(|tool| tool.name == query || tool.id == query)
        .ok_or_else(|| missing(tools, query))
}

fn missing(tools: &[ToolSummary], query: &str) -> anyhow::Error {
    let mut message = format!(
        "no tool '{}' (formless tools list)",
        comparison_terminal_text(query)
    );
    let suggestions = suggest(tools, query);
    if !suggestions.is_empty() {
        message.push_str("\nPartial-name suggestions (use an exact name):");
        for name in suggestions {
            message.push_str("\n  ");
            // Quoting preserves spelling while making control characters visible.
            message.push_str(&comparison_quoted_metadata(name).expect("serializing text"));
        }
    }
    usage(message)
}

fn suggest<'a>(tools: &'a [ToolSummary], query: &str) -> Vec<&'a str> {
    if query.trim().is_empty() {
        return Vec::new();
    }
    let mut matches = tools
        .iter()
        .filter_map(|tool| {
            formless_core::fuzzy::fuzzy_match(query, &tool.name)
                .map(|(score, _)| (score, tool.name.as_str()))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    matches.dedup_by(|a, b| a.1 == b.1);
    matches.into_iter().take(3).map(|(_, name)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tool(id: &str, name: &str) -> ToolSummary {
        serde_json::from_value(serde_json::json!({"id":id,"name":name,"description":"","retired":false,"current_version":1,"capabilities":[],"memory_mb":16,"invocations":0,"success_rate":null,"needs_attention":false,"created_at":"","updated_at":""})).unwrap()
    }

    #[test]
    fn exact_resolution_keeps_original_precedence_and_never_accepts_suggestions() {
        let tools = vec![tool("full-id", "FetchPage"), tool("FetchPage", "another")];
        assert!(std::ptr::eq(
            resolve(&tools, "FetchPage").unwrap(),
            &tools[0]
        ));
        assert!(std::ptr::eq(resolve(&tools, "full-id").unwrap(), &tools[0]));
        for query in ["full", "fetchpage", "FP"] {
            let error = resolve(&tools, query).unwrap_err();
            assert_eq!(exit_code(&error), 2);
        }
        assert_eq!(suggest(&tools, "FP"), ["FetchPage"]);
    }

    #[test]
    fn suggestions_are_bounded_deduplicated_and_deterministic_for_equal_scores() {
        let mut tools = vec![
            tool("1", "ab_z"),
            tool("2", "ab_a"),
            tool("3", "ab_y"),
            tool("4", "ab_b"),
            tool("5", "ab_a"),
        ];
        assert_eq!(suggest(&tools, "ab"), ["ab_a", "ab_b", "ab_y"]);
        tools.reverse();
        assert_eq!(suggest(&tools, "ab"), ["ab_a", "ab_b", "ab_y"]);
        assert!(suggest(&tools, "").is_empty());
        assert!(suggest(&tools, " \t\n").is_empty());
        assert!(suggest(&tools, "zzzz").is_empty());
        let error = resolve(&tools, "zzzz").unwrap_err().to_string();
        assert!(error.contains("formless tools list"));
        assert!(!error.contains("suggestions"));
    }

    #[test]
    fn unicode_suggestions_keep_original_spelling_without_changing_exact_matching() {
        let tools = vec![
            tool("1", "café_reader"),
            tool("2", "日本語_読取"),
            tool("3", "fetch\npage"),
        ];
        assert_eq!(suggest(&tools, "CAFE"), ["café_reader"]);
        assert_eq!(suggest(&tools, "日読"), ["日本語_読取"]);
        assert!(resolve(&tools, "CAFE").is_err());
        assert!(resolve(&tools, "日読").is_err());
        let error = resolve(&tools, "fp").unwrap_err().to_string();
        assert!(error.contains("fetch\\npage"));
        assert!(!error.contains("fetch\npage"));
    }
}
