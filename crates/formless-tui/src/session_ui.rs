//! Read-only Session presentation. Rows retain their original Session index so
//! painting, selection, clicks and bulk actions all address the same identity.
use crate::text;
use formless_protocol::SessionInfo;
use std::collections::{HashMap, HashSet};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, PartialEq)]
pub struct SessionRow {
    pub index: usize,
    pub depth: usize,
    pub note: String,
}

pub fn rows(sessions: &[SessionInfo], visible: &[usize], tree: bool) -> Vec<SessionRow> {
    if !tree {
        return visible
            .iter()
            .map(|&index| SessionRow {
                index,
                depth: 0,
                note: String::new(),
            })
            .collect();
    }
    let by_id: HashMap<&str, usize> = visible
        .iter()
        .map(|&i| (sessions[i].id.as_str(), i))
        .collect();
    let all: HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for &i in visible {
        match sessions[i]
            .forked_from
            .as_deref()
            .filter(|parent| by_id.contains_key(parent))
        {
            Some(parent) => children.entry(parent).or_default().push(i),
            None => roots.push(i),
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(visible.len());
    // The second pass includes malformed cycles without recursion or lost rows.
    for (candidates, detached) in [(&roots[..], false), (visible, true)] {
        for &root in candidates {
            if seen.contains(&root) {
                continue;
            }
            let mut stack = vec![(root, 0usize)];
            while let Some((index, depth)) = stack.pop() {
                if !seen.insert(index) {
                    continue;
                }
                let session = &sessions[index];
                let mut note = session
                    .fork_point_seq
                    .map(|seq| format!("fork at seq {seq}"))
                    .unwrap_or_default();
                if let Some(parent) = session.forked_from.as_deref() {
                    let label = if detached && depth == 0 {
                        Some("lineage cycle")
                    } else if !all.contains(parent) {
                        Some("parent unavailable")
                    } else if !by_id.contains_key(parent) {
                        Some("parent filtered")
                    } else {
                        None
                    };
                    if let Some(label) = label {
                        note.push_str(&format!(" · {label}: {parent}"));
                    }
                }
                if depth > 12 {
                    note.push_str(" · deep lineage (indent capped)");
                }
                out.push(SessionRow {
                    index,
                    depth: depth.min(12),
                    note,
                });
                if let Some(next) = children.get(session.id.as_str()) {
                    stack.extend(next.iter().rev().map(|&i| (i, depth.saturating_add(1))));
                }
            }
        }
    }
    out
}

/// Fit a label and its optional tree prefix into the name column.
pub(crate) fn name_cell(name: &str, depth: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if depth > 0 && width <= 2 {
        return crate::ui::pad("↳", width);
    }

    // Keep the branch sign and leave up to eight columns for the name.
    let indent = depth.min(width.saturating_sub(10) / 2) * 2;
    let branch = if depth == 0 { "" } else { "↳ " };
    let budget = width - indent - text::width(branch);
    // Match List rendering, which paints graphemes individually. Whole
    // strings can report a narrower width when adjacent characters join.
    let (shown, ellipsis) = if text::take_width(name, budget) == name {
        (name, "")
    } else {
        (text::take_width(name, budget - 1), "…")
    };
    let columns = shown.graphemes(true).map(text::width).sum::<usize>() + text::width(ellipsis);
    format!(
        "{}{branch}{shown}{ellipsis}{}",
        " ".repeat(indent),
        " ".repeat(budget - columns)
    )
}

pub fn resolve(sessions: &[SessionInfo], query: &str) -> Result<String, String> {
    if let Some(session) = sessions.iter().find(|s| s.id == query) {
        return Ok(session.id.clone());
    }
    let matching: Vec<_> = sessions
        .iter()
        .filter(|s| !query.is_empty() && (s.id.starts_with(query) || s.name == query))
        .collect();
    match matching.as_slice() {
        [session] => Ok(session.id.clone()),
        [] => Err(format!("No Session matches {query:?}; refresh /sessions and use a Session ID. Model comparison is /compare-models.")),
        _ => Err(format!("Ambiguous Session {query:?}; use its full Session ID.")),
    }
}

pub fn comparison_lines(comparison: &formless_protocol::SessionComparison) -> Vec<String> {
    let mut lines = vec!["Stored Session comparison — no model request".into()];
    lines.push(format!(
        "Common ancestor: {} · fork point: {} · diff after seq {}",
        comparison.common_ancestor.as_deref().unwrap_or("unknown"),
        comparison
            .fork_point_seq
            .map_or("unknown".into(), |n| n.to_string()),
        comparison.diff_from_seq
    ));
    if comparison.prefix_changed {
        lines.push("Copied prefix changed: showing full histories.".into());
    }
    lines.extend(
        comparison
            .warnings
            .iter()
            .map(|warning| format!("Warning: {warning}")),
    );
    for (label, side) in [("LEFT", &comparison.left), ("RIGHT", &comparison.right)] {
        let usage = &side.usage;
        lines.push(format!(
            "{label}: {} ({})",
            side.session.name, side.session.id
        ));
        lines.push(format!("  {}", usage.scope));
        lines.push(format!(
            "  Recorded: {} input / {} output tokens · {} cached / {} cache-write · {} requests",
            usage.recorded_prompt_tokens,
            usage.recorded_completion_tokens,
            usage.recorded_cached_tokens,
            usage.recorded_cache_write_tokens,
            usage.requests
        ));
        lines.push(format!("  Usage coverage: {} · {} unallocated requests ({} input / {} output) · {} unknown Runs", if usage.usage_complete { "complete" } else { "incomplete; missing usage is not zero" }, usage.unallocated_requests, usage.unallocated_prompt_tokens, usage.unallocated_completion_tokens, usage.unknown_runs));
        lines.push(match usage.cost {
            Some(cost) => format!("  Recorded branch cost: ${cost:.6}"),
            None => format!(
                "  Total cost: unknown · priced allocated subtotal only: ${:.6}",
                usage.priced_cost_subtotal
            ),
        });
        if let Some([low, high]) = usage.known_records_cost_range {
            lines.push(format!("  Observed priced rows allocation range only: ${low:.6}–${high:.6}; excludes missing usage and unpriced rows"));
        }
        if usage.unallocated_requests > 0 {
            lines.push(format!(
                "  Unallocated priced subtotal only: ${:.6}",
                usage.unallocated_priced_cost_subtotal
            ));
        }
        if usage.estimated_requests > 0 {
            lines.push(format!(
                "  {} request prices estimated from local prices",
                usage.estimated_requests
            ));
        }
        if !usage.unpriced.is_empty() {
            lines.push(format!("  Unpriced: {}", usage.unpriced.join(", ")));
        }
        lines.extend(
            usage
                .warnings
                .iter()
                .map(|warning| format!("  Warning: {warning}")),
        );
    }
    lines.push(String::new());
    for diff in &comparison.messages {
        for (label, message) in [
            (
                if diff.kind == "unchanged" {
                    "="
                } else {
                    "< LEFT"
                },
                diff.left.as_ref(),
            ),
            (
                "> RIGHT",
                diff.right.as_ref().filter(|_| diff.kind != "unchanged"),
            ),
        ] {
            if let Some(message) = message {
                lines.push(format!(
                    "{label} {} seq {} ({})",
                    message.role, message.seq, message.id
                ));
                lines.extend(message.content.lines().map(|line| format!("  {line}")));
                if let Some(metadata) = &message.meta {
                    lines.push(format!(
                        "  Metadata (quoted data): {}",
                        serde_json::to_string(metadata).unwrap_or_default()
                    ));
                }
                lines.push(String::new());
            }
        }
    }
    lines
        .into_iter()
        .map(|line| crate::text::escape_bidi(&line).into_owned())
        .collect()
}

/// Body for an empty Sessions list.
pub const NO_SESSIONS: &str = "No sessions yet — press n to start one.";
pub const NO_MATCH: &str = "No match — Esc clears the filter.";
/// The default `all` quick filter hides archived Sessions and Esc has nothing
/// to clear, so name the recovery: `v` cycles all → running → archived.
pub const ARCHIVED_ONLY: &str =
    "All sessions are archived — press v twice to cycle filters to archived, or n to start one.";

/// What an empty Sessions list says, from the full Session count, how many
/// of them are archived and whether the filter is active (a query or a
/// non-default quick filter, exactly as `ListFilter::active`).
pub fn empty_body(total: usize, archived: usize, filter_active: bool) -> &'static str {
    if total == 0 {
        NO_SESSIONS
    } else if !filter_active && archived > 0 {
        ARCHIVED_ONLY
    } else {
        NO_MATCH
    }
}

/// Title segment explaining the default view's shown/total gap: `all` leaves
/// archived Sessions out of the shown count while the total includes them.
/// Any other filter already names itself in the title, so it stays silent.
pub fn archived_hidden_segment(filter_active: bool, archived: usize) -> Option<String> {
    (!filter_active && archived > 0).then(|| format!("· {archived} archived hidden"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(id: &str, parent: Option<&str>) -> SessionInfo {
        serde_json::from_value(serde_json::json!({"id":id,"name":id,"created_at":"","updated_at":"","forked_from":parent,"fork_point_seq":parent.map(|_|3)})).unwrap()
    }
    #[test]
    fn tree_keeps_every_filtered_identity_and_labels_unknown_parents_and_cycles() {
        let sessions = vec![
            session("child", Some("root")),
            session("root", None),
            session("sibling", Some("root")),
            session("orphan", Some("deleted")),
            session("cycle-a", Some("cycle-b")),
            session("cycle-b", Some("cycle-a")),
        ];
        let visible = [0, 1, 2, 3, 4, 5];
        let tree = rows(&sessions, &visible, true);
        assert_eq!(
            tree.iter().map(|r| r.index).collect::<Vec<_>>(),
            [1, 0, 2, 3, 4, 5]
        );
        assert_eq!(
            tree.iter().map(|r| r.depth).collect::<Vec<_>>(),
            [0, 1, 1, 0, 0, 1]
        );
        assert!(tree[1].note.contains("fork at seq 3"));
        assert!(tree[3].note.contains("parent unavailable: deleted"));
        assert!(tree[4].note.contains("lineage cycle"));
        assert_eq!(
            rows(&sessions, &[0], true)[0].note,
            "fork at seq 3 · parent filtered: root"
        );
        assert_eq!(
            rows(&sessions, &visible, false)
                .iter()
                .map(|r| r.index)
                .collect::<Vec<_>>(),
            visible
        );
    }
    #[test]
    fn empty_body_names_archived_only_recovery_and_keeps_other_texts() {
        assert_eq!(
            empty_body(0, 0, false),
            "No sessions yet — press n to start one."
        );
        assert_eq!(empty_body(0, 0, true), NO_SESSIONS);
        assert_eq!(empty_body(3, 3, false), ARCHIVED_ONLY);
        assert!(ARCHIVED_ONLY.contains("v twice") && ARCHIVED_ONLY.contains("archived"));
        assert!(!ARCHIVED_ONLY.contains("Esc"));
        // A query or the running/archived quick filter is active: Esc clears it.
        assert_eq!(empty_body(3, 3, true), "No match — Esc clears the filter.");
        assert_eq!(empty_body(15, 3, true), NO_MATCH);
        assert_eq!(empty_body(4, 0, true), NO_MATCH);
    }
    #[test]
    fn archived_hidden_segment_only_explains_the_default_view() {
        assert_eq!(
            archived_hidden_segment(false, 3).as_deref(),
            Some("· 3 archived hidden")
        );
        assert_eq!(
            archived_hidden_segment(false, 1).as_deref(),
            Some("· 1 archived hidden")
        );
        assert_eq!(archived_hidden_segment(false, 0), None);
        assert_eq!(archived_hidden_segment(true, 3), None);
    }
    #[test]
    fn archived_is_two_v_presses_from_the_default_quick_filter() {
        // `v` advances quick by one (lists::key); `ARCHIVED_ONLY` says "twice".
        let quicks = crate::lists::SESSION_QUICK;
        assert_eq!(quicks[0], "all");
        assert_eq!(quicks.iter().position(|q| *q == "archived"), Some(2));
    }
    #[test]
    fn resolution_never_guesses_models_or_ambiguous_session_prefixes() {
        let sessions = vec![session("abc-one", None), session("abc-two", None)];
        assert_eq!(resolve(&sessions, "abc-one").unwrap(), "abc-one");
        assert!(resolve(&sessions, "abc").unwrap_err().contains("Ambiguous"));
        assert!(resolve(&sessions, "gpt-model")
            .unwrap_err()
            .contains("/compare-models"));
        assert!(resolve(&sessions, "").is_err());
    }
}

#[cfg(test)]
pub(crate) fn comparison_fixture() -> formless_protocol::SessionComparison {
    let usage = serde_json::json!({"scope":"whole Runs after fork boundary; straddling usage unallocated","recorded_prompt_tokens":21,"recorded_completion_tokens":8,"recorded_cached_tokens":3,"recorded_cache_write_tokens":2,"requests":1,"unallocated_requests":1,"unallocated_prompt_tokens":9,"unallocated_completion_tokens":4,"unknown_runs":1,"usage_complete":false,"cost":null,"priced_cost_subtotal":0.25,"unallocated_priced_cost_subtotal":0.5,"known_records_cost_range":[0.25,0.75],"estimated_requests":1,"unpriced":["private/model"],"warnings":["Missing provider usage"]});
    let session = |id: &str| serde_json::json!({"id":id,"name":id,"created_at":"","updated_at":""});
    serde_json::from_value(serde_json::json!({"left":{"session":session("left"),"usage":usage},"right":{"session":session("right"),"usage":usage},"common_ancestor":"parent","fork_point_seq":3,"diff_from_seq":0,"prefix_changed":true,"warnings":["Prefix edited"],"messages":[{"kind":"left_only","left":{"id":"m1","session_id":"left","seq":4,"role":"assistant","content":"private answer\n<script>data only</script>","created_at":""},"right":null}]})).unwrap()
}

#[cfg(test)]
mod comparison_tests {
    #[test]
    fn metadata_only_changes_remain_visible_as_quoted_data() {
        let mut comparison = super::comparison_fixture();
        let left_meta = r#"{"tool_calls":[{"name":"reader","arguments":{"value":"left <script>data</script>"}}]}"#;
        let right_meta = "{\"tool_calls\":[{\"name\":\"reader\",\"arguments\":{\"value\":\"right سلام ไทย \u{202e}\"}}]}";
        let mut left = comparison.messages[0].left.clone().unwrap();
        left.content.clear();
        left.meta = Some(left_meta.into());
        let mut right = left.clone();
        right.id = "m2".into();
        right.meta = Some(right_meta.into());
        comparison.messages = vec![
            formless_protocol::SessionMessageDiff {
                kind: "left_only".into(),
                left: Some(left),
                right: None,
            },
            formless_protocol::SessionMessageDiff {
                kind: "right_only".into(),
                left: None,
                right: Some(right),
            },
        ];
        let lines = super::comparison_lines(&comparison).join("\n");
        assert_eq!(lines.matches("Metadata (quoted data):").count(), 2);
        assert!(lines.contains("left <script>data</script>"));
        assert!(lines.contains("right سلام ไทย ⟨U+202E⟩"));
        assert!(!lines.contains('\u{202e}'));
        assert!(lines.contains(r#"\"tool_calls\""#));
        assert!(
            comparison.messages[1]
                .right
                .as_ref()
                .unwrap()
                .meta
                .as_ref()
                .unwrap()
                .contains('\u{202e}'),
            "display escaping must preserve stored metadata"
        );
    }

    #[test]
    fn comparison_preserves_message_text_and_honest_usage_coverage() {
        let lines = super::comparison_lines(&super::comparison_fixture()).join("\n");
        for expected in [
            "no model request",
            "parent",
            "fork point: 3",
            "Copied prefix changed",
            "Prefix edited",
            "21 input / 8 output",
            "9 input / 4 output",
            "missing usage is not zero",
            "Total cost: unknown",
            "subtotal only: $0.250000",
            "allocation range only",
            "excludes missing usage and unpriced rows",
            "Unpriced: private/model",
            "Missing provider usage",
            "< LEFT assistant seq 4",
            "private answer",
            "<script>data only</script>",
        ] {
            assert!(lines.contains(expected), "lost {expected}: {lines}");
        }
        assert!(!lines.contains("Recorded branch cost:"));
    }
}
