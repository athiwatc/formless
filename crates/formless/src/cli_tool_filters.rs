//! Explicit stored-state selection; default list cells and order stay intact.
use formless_protocol::ToolSummary;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(super) enum Filter {
    /// Not retired, including disabled Tools
    NotRetired,
    /// Retired regardless of other stored flags
    Retired,
    /// Known success rate below 0.8
    Failing,
    /// Marked as needing attention
    NeedsAttention,
    /// Disabled regardless of retirement state
    Off,
    /// Pinned regardless of other stored flags
    Pinned,
}
impl Filter {
    fn matches(self, tool: &ToolSummary) -> bool {
        match self {
            Self::NotRetired => !tool.retired,
            Self::Retired => tool.retired,
            Self::Failing => tool.success_rate.is_some_and(|rate| rate < 0.8),
            Self::NeedsAttention => tool.needs_attention,
            Self::Off => tool.disabled,
            Self::Pinned => tool.pinned,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::NotRetired => "not-retired",
            Self::Retired => "retired",
            Self::Failing => "failing",
            Self::NeedsAttention => "needs-attention",
            Self::Off => "off",
            Self::Pinned => "pinned",
        }
    }
}

pub(super) fn select(tools: &mut Vec<ToolSummary>, filter: Option<Filter>, tag: Option<&str>) {
    let tag = tag.map(|tag| tag.trim_start_matches('#').to_lowercase());
    tools.retain(|tool| {
        filter.is_none_or(|filter| filter.matches(tool))
            && tag.as_ref().is_none_or(|tag| tool.tags.contains(tag))
    });
}

pub(super) fn empty_message(
    original_count: usize,
    filter: Option<Filter>,
    tag: Option<&str>,
) -> String {
    if original_count == 0 {
        return "(no tools yet - the agent builds them via create_tool)".into();
    }
    let mut labels = Vec::new();
    let mut flags = Vec::new();
    if let Some(filter) = filter {
        labels.push(format!("--filter {}", filter.name()));
        flags.push("--filter");
    }
    if let Some(tag) = tag {
        labels.push(format!("--tag {:?}", tag));
        flags.push("--tag");
    }
    format!(
        "No Tools match {}. Clear {} to list all Tools.",
        labels.join(" and "),
        flags.join(" and ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tools() -> Vec<ToolSummary> {
        serde_json::from_str(include_str!("../tests/fixtures/tool-filter-list.json")).unwrap()
    }
    #[test]
    fn predicates_combine_with_exact_tags_and_preserve_input_order() {
        for (filter, expected) in [
            (Filter::NotRetired, vec!["Alpha", "Beta", "Delta"]),
            (Filter::Retired, vec!["Gamma"]),
            (Filter::Failing, vec!["Beta", "Gamma"]),
            (Filter::NeedsAttention, vec![]),
            (Filter::Off, vec!["Beta"]),
            (Filter::Pinned, vec!["Alpha", "Gamma"]),
        ] {
            let mut rows = tools();
            select(&mut rows, Some(filter), None);
            assert_eq!(
                rows.iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        let mut rows = tools();
        select(&mut rows, Some(Filter::Failing), Some("#WORK"));
        assert_eq!(
            rows.iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["Gamma"]
        );
        let mut rows = tools();
        select(&mut rows, None, Some("wor"));
        assert!(rows.is_empty());
        let mut rows = tools();
        rows[0].success_rate = Some(0.8);
        rows[0].needs_attention = true;
        assert!(!Filter::Failing.matches(&rows[0]));
        assert!(Filter::NeedsAttention.matches(&rows[0]));
        rows[1].retired = true;
        assert!(Filter::Off.matches(&rows[1]));
    }
    #[test]
    fn empty_registry_and_no_matching_rows_have_distinct_guidance() {
        assert!(empty_message(0, Some(Filter::Pinned), Some("work")).contains("no tools yet"));
        let message = empty_message(4, Some(Filter::Failing), Some("missing"));
        assert!(message.contains("--filter failing and --tag \"missing\""));
        assert!(message.contains("Clear --filter and --tag"));
        assert!(!message.contains("no tools yet"));
    }
}
