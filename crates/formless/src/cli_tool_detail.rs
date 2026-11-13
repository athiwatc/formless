//! Bounded human history and an explicit, exact current-source projection.
use super::*;
use formless_protocol::ToolDetail;

const VERSION_PREVIEW: usize = 10;
const CALL_PREVIEW: usize = 5;

pub(super) fn current_source(detail: &ToolDetail) -> Result<&str> {
    detail
        .versions
        .iter()
        .find(|version| version.version == detail.tool.current_version)
        .map(|version| version.code.as_str())
        .ok_or_else(|| {
            usage(format!(
                "current Tool version {} is missing from stored detail",
                detail.tool.current_version
            ))
        })
}

pub(super) fn history(detail: &ToolDetail) -> String {
    use std::fmt::Write;
    let mut versions = detail.versions.iter().collect::<Vec<_>>();
    versions.sort_by_key(|version| std::cmp::Reverse(version.version));
    let mut out = String::from("versions (newest first):\n");
    if versions.is_empty() {
        out.push_str("  (none)\n");
    }
    for version in versions.iter().take(VERSION_PREVIEW) {
        writeln!(
            out,
            "  v{} · {} · {}",
            version.version,
            table::cell_text(&version.author),
            table::cell_text(version.created_at.get(..10).unwrap_or(&version.created_at))
        )
        .unwrap();
    }
    if versions.len() > VERSION_PREVIEW {
        writeln!(
            out,
            "  {} versions omitted; use --json for full history.",
            versions.len() - VERSION_PREVIEW
        )
        .unwrap();
    }
    out.push_str("recent calls (newest first, up to 5 returned calls):\n");
    if detail.recent_calls.is_empty() {
        out.push_str("  (none)\n");
    }
    for call in detail.recent_calls.iter().take(CALL_PREVIEW) {
        write!(
            out,
            "  {} · v{} · {} · {}ms",
            table::cell_text(&call.created_at),
            call.version,
            table::cell_text(&call.status),
            call.duration_ms
        )
        .unwrap();
        if !call.error.is_empty() {
            write!(
                out,
                " · {}",
                table::fit(&table::cell_text(&call.error), 120)
            )
            .unwrap();
        }
        out.push('\n');
    }
    if detail.recent_calls.len() > CALL_PREVIEW {
        writeln!(
            out,
            "  {} returned calls omitted; use --json for all returned call metadata.",
            detail.recent_calls.len() - CALL_PREVIEW
        )
        .unwrap();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn detail() -> ToolDetail {
        let tool = serde_json::json!({"id":"reader-id","name":"reader","pinned":true,"description":"fixture","retired":false,"current_version":15,"capabilities":[],"memory_mb":16,"invocations":0,"success_rate":null,"needs_attention":false,"created_at":"","updated_at":""});
        let versions=(0..15).map(|i| {let version=(i*7)%15+1;serde_json::json!({"version":version,"author":"user","created_at":"2026-11-01T00:00:00Z","capabilities":[],"memory_mb":16,"input_schema":"{}","output_schema":"{}","code":format!("// source 日本語 v{version}\n")})}).collect::<Vec<_>>();
        let calls=(0..8).map(|i|serde_json::json!({"id":format!("call{i}"),"version":15,"status":"failed","input":"PrivateInputSentinel","output":"PrivateOutputSentinel","error":"error detail ".repeat(30),"duration_ms":i,"created_at":format!("time-{i}")})).collect::<Vec<_>>();
        serde_json::from_value(serde_json::json!({"tool":tool,"versions":versions,"metrics":formless_protocol::ToolMetricsInfo::default(),"recent_calls":calls})).unwrap()
    }
    #[test]
    fn bounded_summaries_order_versions_and_never_add_raw_call_payloads() {
        let text = history(&detail());
        let version_rows = text
            .lines()
            .filter(|line| line.starts_with("  v"))
            .collect::<Vec<_>>();
        assert_eq!(version_rows.len(), 10);
        assert!(version_rows[0].starts_with("  v15 ·") && version_rows[9].starts_with("  v6 ·"));
        assert!(text.contains("5 versions omitted"));
        assert_eq!(
            text.lines()
                .filter(|line| line.starts_with("  time-"))
                .count(),
            5
        );
        assert!(text.contains("3 returned calls omitted"));
        assert!(text.contains("time-0 · v15 · failed · 0ms"));
        assert!(text.contains('…') && text.contains("--json"));
        for private in ["PrivateInputSentinel", "PrivateOutputSentinel", "// source"] {
            assert!(!text.contains(private));
        }
        let mut empty = detail();
        empty.versions.clear();
        empty.recent_calls.clear();
        assert_eq!(history(&empty).matches("(none)").count(), 2);
    }
    #[test]
    fn current_source_is_exact_and_missing_current_never_selects_another_version() {
        let mut detail = detail();
        assert_eq!(current_source(&detail).unwrap(), "// source 日本語 v15\n");
        let version = detail
            .versions
            .iter_mut()
            .find(|version| version.version == 15)
            .unwrap();
        version.code = "// ไทย without newline".into();
        assert_eq!(current_source(&detail).unwrap(), "// ไทย without newline");
        detail.tool.current_version = 999;
        let error = current_source(&detail).unwrap_err();
        assert_eq!(exit_code(&error), 2);
        assert!(error.to_string().contains("999"));
    }
    #[test]
    fn source_mode_conflicts_with_json_before_dispatch() {
        let cli = Cli::try_parse_from(["formless", "tools", "show", "reader", "--code"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Tools {
                action: ToolsAction::Show {
                    code: true,
                    json: false,
                    ..
                }
            })
        ));
        assert!(
            Cli::try_parse_from(["formless", "tools", "show", "reader", "--code", "--json"])
                .is_err()
        );
    }
}
