//! Actionable diagnostics without changing Session resolution semantics.
use super::*;
const CANDIDATE_LIMIT: usize = 10;

fn brief(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        let part = if table::terminal_control(ch) {
            ch.escape_unicode().to_string()
        } else {
            ch.to_string()
        };
        if out.chars().count() + part.chars().count() > 120 {
            out.push('…');
            break;
        }
        out.push_str(&part);
    }
    out
}

pub(super) fn ambiguous(query: &str, matches: &[SessionInfo], by_name: bool) -> String {
    use std::fmt::Write;
    let source = if by_name { "name" } else { "ID prefix" };
    let action = if by_name {
        "choose one of the displayed IDs"
    } else {
        "give more of the ID or use a displayed full ID"
    };
    let mut out = format!(
        "Session {source} '{}' matches {} Sessions; {action}.\n",
        brief(query),
        matches.len()
    );
    for session in matches.iter().take(CANDIDATE_LIMIT) {
        writeln!(
            out,
            "  {}  {} [{}]",
            comparison_terminal_text(&session.id),
            brief(&session.name),
            if session.archived {
                "archived"
            } else {
                "active"
            }
        )
        .unwrap();
    }
    let omitted = matches.len().saturating_sub(CANDIDATE_LIMIT);
    if omitted > 0 {
        writeln!(out, "{omitted} more matching Sessions omitted.").unwrap();
    }
    out.push_str(
        "Inspect Sessions with formless sessions list and formless sessions list --archived.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session(id: &str, name: &str, archived: bool) -> SessionInfo {
        serde_json::from_value(serde_json::json!({"id":id,"name":name,"created_at":"","updated_at":"","archived":archived})).unwrap()
    }

    #[test]
    fn empty_selector_is_a_usage_error_regardless_of_session_count() {
        let rows = [
            session("first", "One", false),
            session("second", "Two", true),
        ];
        for count in 0..=rows.len() {
            let err = find_session_in(&rows[..count], "").unwrap_err();
            assert_eq!(exit_code(&err), 2);
            assert_eq!(err.to_string(), "Session selector must not be empty");
        }
    }

    #[test]
    fn cli_rejects_empty_session_arguments_before_dispatch() {
        for args in [
            &["sessions", "fork", ""][..],
            &["sessions", "restore", ""],
            &["sessions", "show", "", "--follow"],
            &["sessions", "compare", "", "right"],
            &["sessions", "compare", "left", ""],
            &["run", "--estimate", "--session", "", "task"],
        ] {
            let result =
                Cli::try_parse_from(std::iter::once("formless").chain(args.iter().copied()));
            let Err(error) = result else {
                panic!("accepted an empty Session selector in {args:?}");
            };
            assert_eq!(error.exit_code(), 2, "{args:?}: {error}");
            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
        }
    }

    #[test]
    fn resolution_priority_ascii_names_and_archived_eligibility_stay_unchanged() {
        let rows = vec![
            session("same", "other", false),
            session("same-extra", "SAME", true),
            session("id-name", "same", true),
        ];
        assert_eq!(find_session_in(&rows, "same").unwrap().id, "same");
        assert_eq!(find_session_in(&rows, "same-e").unwrap().id, "same-extra");
        assert_eq!(find_session_in(&rows, "  OtHeR ").unwrap().id, "same");
        let err = find_session_in(&rows, "  SaMe ").unwrap_err();
        assert_eq!(exit_code(&err), 2);
        assert!(err.to_string().contains("Session name"));
        let unicode = vec![session("u", "日本語 Ä", true)];
        assert_eq!(find_session_in(&unicode, "日本語 Ä").unwrap().id, "u");
        assert!(find_session_in(&unicode, "日本語 ä").is_err());
    }
    #[test]
    fn diagnostics_bound_names_and_count_omitted_candidates_without_clipping_ids() {
        let rows: Vec<_> = (0..13)
            .map(|n| {
                session(
                    &format!("12345678-shared-prefix-full-id-{n}"),
                    &format!("日本語\n\u{1b}[31m{}", "界".repeat(150)),
                    n % 2 == 1,
                )
            })
            .collect();
        let err = find_session_in(&rows, "12345678").unwrap_err();
        assert_eq!(exit_code(&err), 2);
        let text = err.to_string();
        assert!(
            text.contains("matches 13 Sessions")
                && text.contains("3 more matching Sessions omitted")
        );
        for row in rows.iter().take(10) {
            assert!(text.contains(&row.id));
        }
        assert!(!text.contains(&rows[10].id));
        assert!(text.contains("[archived]") && text.contains("[active]"));
        assert!(text.contains("日本語\\u{a}\\u{1b}[31m") && !text.contains('\u{1b}'));
        assert!(text.contains("…") && text.len() < 6000);
        assert!(text.contains("sessions list --archived"));
    }
}
