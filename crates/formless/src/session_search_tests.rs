use super::*;
use clap::error::ErrorKind;

fn hit() -> formless_protocol::SearchHit {
    formless_protocol::SearchHit {
        session_id: "ไทย日本語-id".into(),
        session_name: "โครงการ 日本語".into(),
        message_id: "message".into(),
        seq: 42,
        role: "assistant".into(),
        snippet: "staging  was deployed\nไทย".into(),
        created_at: "2026-11-14T12:34:56+07:00".into(),
    }
}

#[test]
fn search_limits_parse_only_the_documented_range_before_dispatch() {
    for limit in ["1", "20", "200"] {
        let cli =
            Cli::try_parse_from(["formless", "sessions", "search", "query", "--limit", limit])
                .unwrap();
        let Some(Cmd::Sessions {
            action: SessionsAction::Search { limit: parsed, .. },
        }) = cli.cmd
        else {
            panic!("wrong command")
        };
        assert_eq!(parsed, limit.parse::<usize>().unwrap());
    }
    for limit in ["0", "201", "-1", "184467440737095516160000", "not-a-number"] {
        let error = Cli::try_parse_from([
            "formless",
            "sessions",
            "search",
            "query",
            &format!("--limit={limit}"),
        ])
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::ValueValidation, "{error}");
    }
    let error = Cli::try_parse_from(["formless", "sessions", "search", "--help"])
        .err()
        .unwrap();
    assert_eq!(error.kind(), ErrorKind::DisplayHelp);
    let help = error.to_string();
    assert!(
        help.contains("1–200") && help.contains("default 20"),
        "{help}"
    );
}

#[test]
fn search_rows_keep_snippet_unicode_and_add_message_position_and_stored_time() {
    let hits = vec![hit()];
    let before = serde_json::to_value(&hits).unwrap();
    let text = session_search_text("deploy", 20, &hits);
    assert!(
        text.contains("โครงการ 日本語  #42 2026-11-14 12:34 [assistant] staging  was deployed ไทย"),
        "{text}"
    );
    assert!(!text.contains("more matches"));
    assert_eq!(
        serde_json::to_value(&hits).unwrap(),
        before,
        "human projection cannot mutate JSON fields"
    );
    assert_eq!(before[0]["message_id"], "message");
    assert_eq!(before[0]["created_at"], "2026-11-14T12:34:56+07:00");
}

#[test]
fn search_cap_hints_are_conditional_and_do_not_suggest_limits_above_200() {
    let one = [hit()];
    let text = session_search_text("deploy", 1, &one);
    assert!(text.contains("more matches may exist") && text.contains("up to 200"));
    assert!(!text.contains("truncated"));
    let many = vec![hit(); 200];
    let text = session_search_text("deploy", 200, &many);
    assert!(text.contains("Result limit 200 reached; more matches may exist."));
    assert!(text.contains("Narrow the query"));
    assert!(!text.contains("Increase"));
    assert_eq!(
        session_search_text("deploy", 20, &[]),
        "no messages match 'deploy'\n"
    );
    assert_eq!(session_search_text("  ", 20, &[]), "empty search query\n");
}

#[test]
fn search_time_fallbacks_and_visible_controls_are_boundary_safe() {
    for time in ["", "ไทยประเทศไทย", "x", "2026-11-14T12:34\u{1b}[2J"] {
        let mut hit = hit();
        hit.created_at = time.into();
        hit.session_name = "name\u{1b}[2J".into();
        let text = session_search_text("deploy", 20, &[hit]);
        assert!(!text.contains('\u{1b}'));
        assert!(text.contains("\\u{1b}"));
    }
}
