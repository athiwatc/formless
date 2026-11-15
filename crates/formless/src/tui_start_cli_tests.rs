//! Parse and validate only; no profile, daemon or TUI startup.
use super::*;
fn flags(args: &[&str]) -> Result<(bool, bool, bool)> {
    let mut cli = Cli::try_parse_from(args).map_err(anyhow::Error::from)?;
    validate_tui_start(&mut cli)?;
    Ok((cli.plain, cli.continue_last, cli.new))
}
#[test]
fn tui_start_options_work_before_after_and_without_the_explicit_command() {
    for (root, explicit, expected) in [
        (
            vec!["formless", "--new"],
            vec!["formless", "tui", "--new"],
            (false, false, true),
        ),
        (
            vec!["formless", "-c"],
            vec!["formless", "tui", "-c"],
            (false, true, false),
        ),
        (
            vec!["formless", "--plain"],
            vec!["formless", "tui", "--plain"],
            (true, false, false),
        ),
    ] {
        assert_eq!(flags(&root).unwrap(), expected);
        assert_eq!(flags(&explicit).unwrap(), expected);
        let mut before = root;
        before.push("tui");
        assert_eq!(flags(&before).unwrap(), expected);
    }
    assert_eq!(
        flags(&["formless", "--plain", "tui", "--new"]).unwrap(),
        (true, false, true)
    );
}
#[test]
fn tui_start_conflicts_cross_the_command_boundary_without_changing_run_continue() {
    for args in [
        vec!["formless", "tui", "--continue", "--new"],
        vec!["formless", "--continue", "tui", "--new"],
        vec!["formless", "--new", "tui", "-c"],
    ] {
        assert!(flags(&args).is_err());
    }
    for args in [
        vec!["formless", "--plain", "sessions", "list"],
        vec!["formless", "--new", "tools", "list"],
        vec!["formless", "--continue", "run", "fixture"],
    ] {
        assert!(flags(&args).is_err());
    }
    let mut cli = Cli::try_parse_from(["formless", "run", "fixture", "-c"]).unwrap();
    validate_tui_start(&mut cli).unwrap();
    assert!(!cli.continue_last);
    let Some(Cmd::Run { continue_last, .. }) = cli.cmd else {
        panic!("wrong command")
    };
    assert!(continue_last);
    assert!(Cli::try_parse_from(["formless", "sessions", "list", "--plain"]).is_err());
}
#[test]
fn explicit_tui_help_lists_only_scoped_start_options() {
    let err = Cli::try_parse_from(["formless", "help", "tui"])
        .err()
        .unwrap();
    assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    let help = err.to_string();
    for option in ["--plain", "--continue", "--new"] {
        assert!(help.contains(option), "{help}");
    }
}
