use super::*;

#[test]
fn runs_filters_use_existing_selectors_and_one_checked_cutoff() {
    let cli = Cli::try_parse_from([
        "formless",
        "runs",
        "--session",
        "prefix",
        "--since",
        "24h",
        "--limit",
        "7",
        "--json",
    ])
    .unwrap();
    validate_runs_filters(&cli).unwrap();
    assert!(
        matches!(cli.cmd,Some(Cmd::Runs{session:Some(session),since:Some(since),limit:7,json:true,action:None}) if session=="prefix" && chrono::DateTime::parse_from_rfc3339(&since).is_ok())
    );
    let cli = Cli::try_parse_from(["formless", "runs"]).unwrap();
    assert!(matches!(
        cli.cmd,
        Some(Cmd::Runs {
            session: None,
            since: None,
            limit: 20,
            json: false,
            action: None
        })
    ));
    let now = chrono::DateTime::parse_from_rfc3339("2026-11-13T12:00:00+00:00")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(cutoff_at("24h", now).unwrap(), "2026-11-12T12:00:00+00:00");
    for duration in [
        "bad",
        "18446744073709551615d",
        "18446744073709551615s1s",
        "18446744073709551615",
        "1000000000d",
    ] {
        let error = Cli::try_parse_from(["formless", "runs", "--since", duration])
            .err()
            .unwrap();
        assert_eq!(error.exit_code(), 2);
    }
}

#[test]
fn list_filters_with_subactions_are_usage_errors_before_profile_or_connection() {
    for action in ["show", "cancel"] {
        for filters in [
            vec!["--session", "X"],
            vec!["--since", "24h"],
            vec!["--session", "X", "--since", "24h"],
        ] {
            let mut args = vec!["formless", "runs"];
            args.extend(filters);
            args.extend([action, "run-id"]);
            let cli = Cli::try_parse_from(args).unwrap();
            let error = validate_runs_filters(&cli).unwrap_err();
            assert_eq!(exit_code(&error), 2);
        }
        assert!(
            Cli::try_parse_from(["formless", "runs", action, "run-id", "--session", "X"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["formless", "runs", action, "run-id", "--since", "24h"]).is_err()
        );
    }
}
