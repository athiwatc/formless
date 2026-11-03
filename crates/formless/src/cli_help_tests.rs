//! CLI documentation checks parse arguments or stop at Clap's help/version exits.
//! They never dispatch a command, open a profile, or contact a daemon/provider.

use clap::{error::ErrorKind, Command, CommandFactory, Parser};
use std::collections::BTreeSet;

fn command_tree() -> Command {
    let mut command = super::Cli::command();
    command.build(); // Include inherited arguments and Clap's generated help/version.
    command
}

fn informative(text: Option<String>, label: &str) -> bool {
    let Some(text) = text else { return false };
    let text = text.trim();
    let words: Vec<_> = text.split_whitespace().collect();
    words.len() >= 2
        && text.chars().any(char::is_alphabetic)
        && !text.eq_ignore_ascii_case(label)
        && !matches!(
            text.to_ascii_lowercase().as_str(),
            "todo" | "tbd" | "description"
        )
}

fn help_problems(command: &Command, path: &str, problems: &mut Vec<String>) {
    if !informative(
        command.get_about().map(ToString::to_string),
        command.get_name(),
    ) {
        problems.push(format!("{path}: missing/incomplete command description"));
    }
    for arg in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
        if !informative(
            arg.get_help().map(ToString::to_string),
            arg.get_id().as_str(),
        ) {
            problems.push(format!(
                "{path}: missing/incomplete help for {}",
                arg.get_id()
            ));
        }
    }
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
    {
        // build() gives generated `help` a duplicate, completion-only tree
        // without arguments. The actual command definitions are checked once.
        if child.get_name() == "help" {
            assert!(informative(
                child.get_about().map(ToString::to_string),
                "help"
            ));
        } else {
            help_problems(child, &format!("{path} {}", child.get_name()), problems);
        }
    }
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_rendered_help(command: &Command, path: &[String], count: &mut usize) {
    for help_flag in ["-h", "--help"] {
        let mut args = path.to_vec();
        args.push(help_flag.into());
        // This is the CLI's real typed parser, not a second command schema. A
        // DisplayHelp error proves parsing stopped before any operation runs,
        // even for commands with required positional arguments or credentials.
        let error = match super::Cli::try_parse_from(&args) {
            Err(error) => error,
            Ok(_) => panic!("{} did not stop at help", args.join(" ")),
        };
        assert_eq!(
            error.kind(),
            ErrorKind::DisplayHelp,
            "{}: {error}",
            args.join(" ")
        );
        let rendered = normalized(&error.to_string());
        assert!(
            rendered.contains("Usage:"),
            "{}: {rendered}",
            args.join(" ")
        );
        for arg in command.get_arguments().filter(|arg| {
            !arg.is_hide_set()
                && !(help_flag == "-h" && arg.is_hide_short_help_set())
                && !(help_flag == "--help" && arg.is_hide_long_help_set())
        }) {
            let help = if help_flag == "--help" {
                arg.get_long_help().or_else(|| arg.get_help())
            } else {
                arg.get_help()
            }
            .expect("metadata test requires argument help")
            .to_string();
            assert!(
                rendered.contains(&normalized(&help)),
                "{}: description of {} missing from rendered output: {help:?}",
                args.join(" "),
                arg.get_id()
            );
        }
        for child in command
            .get_subcommands()
            .filter(|child| !child.is_hide_set())
        {
            let about = child
                .get_about()
                .expect("metadata test requires command help");
            assert!(
                rendered.contains(&normalized(&about.to_string())),
                "{}: description of command {} missing from rendered output",
                args.join(" "),
                child.get_name()
            );
        }
        *count += 1;
    }
    // The generated help command intentionally has no --help flag of its own;
    // exercise its real `formless help [COMMAND...]` syntax for every path.
    let mut help_args = vec!["formless".to_owned(), "help".to_owned()];
    help_args.extend_from_slice(&path[1..]);
    let error = match super::Cli::try_parse_from(&help_args) {
        Err(error) => error,
        Ok(_) => panic!("{} did not stop at help", help_args.join(" ")),
    };
    assert_eq!(
        error.kind(),
        ErrorKind::DisplayHelp,
        "{}: {error}",
        help_args.join(" ")
    );
    assert!(error.to_string().contains("Usage:"));
    *count += 1;
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set() && child.get_name() != "help")
    {
        let mut child_path = path.to_vec();
        child_path.push(child.get_name().into());
        check_rendered_help(child, &child_path, count);
    }
}

#[test]
fn every_visible_command_and_argument_has_informative_help() {
    let mut problems = Vec::new();
    help_problems(&command_tree(), "formless", &mut problems);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_visible_path_renders_short_and_long_help_without_running_commands() {
    let mut count = 0;
    check_rendered_help(&command_tree(), &["formless".into()], &mut count);
    assert!(
        count > 100,
        "expected the full nested CLI, checked only {count} help exits"
    );
    eprintln!(
        "checked {count} safe help exits across {} visible command paths",
        count / 3
    );
    for flag in ["-V", "--version"] {
        let error = match super::Cli::try_parse_from(["formless", flag]) {
            Err(error) => error,
            Ok(_) => panic!("{flag} did not stop at version"),
        };
        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn root_and_command_groups_have_rendered_examples() {
    let root = command_tree();
    for command in std::iter::once(&root).chain(root.get_subcommands().filter(|child| {
        !child.is_hide_set() && child.get_name() != "help" && child.has_subcommands()
    })) {
        let examples = command
            .get_after_help()
            .expect("root/groups need examples")
            .to_string();
        assert!(
            examples.contains("formless "),
            "{}: {examples}",
            command.get_name()
        );
        let mut command = command.clone();
        assert!(normalized(&command.render_help().to_string()).contains(&normalized(&examples)));
    }
}

// Split only the final pipeline stage, respecting quoted/escaped literal pipes.
// shlex then handles shell quoting; no shell or command dispatcher is invoked.
fn example_args(line: &str) -> Result<Option<Vec<String>>, String> {
    let (mut quote, mut escaped, mut start) = (None, false, 0);
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if ch == '\'' || ch == '"' {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
        } else if ch == '|' && quote.is_none() {
            start = index + 1;
        }
    }
    let stage = line[start..].trim();
    if !stage.starts_with("formless ") && stage != "formless" {
        return Ok(None);
    }
    let mut args = shlex::split(stage).ok_or_else(|| format!("bad shell quoting: {line}"))?;
    for arg in &mut args {
        *arg = match arg.as_str() {
            "SESSION_ID" | "RUN_ID" | "JOB_ID" => "00000000-0000-0000-0000-000000000001".into(),
            "TOOL_NAME" => "fixture-tool".into(),
            "SERVER_NAME" => "fixture-server".into(),
            _ => continue,
        };
    }
    Ok(Some(args))
}

fn example_problems(text: &str) -> Vec<String> {
    let Some((_, examples)) = text.split_once("Examples:") else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    let mut count = 0;
    for line in examples.lines().filter(|line| !line.trim().is_empty()) {
        match example_args(line) {
            Ok(Some(args)) => {
                count += 1;
                if let Err(error) = super::Cli::try_parse_from(args) {
                    problems.push(format!("{}: {error}", line.trim()));
                }
            }
            Err(error) => problems.push(error),
            Ok(None) => {}
        }
    }
    if count == 0 {
        problems.push("Examples has no parseable formless command lines".into());
    }
    problems
}

#[test]
fn every_documented_help_example_parses_without_dispatch() {
    fn visit(command: &Command, problems: &mut Vec<String>, count: &mut usize) {
        if let Some(text) = command.get_after_help() {
            let text = text.to_string();
            if text.contains("Examples:") {
                *count += 1;
                problems.extend(
                    example_problems(&text)
                        .into_iter()
                        .map(|problem| format!("{}: {problem}", command.get_name())),
                );
            }
        }
        for child in command
            .get_subcommands()
            .filter(|child| !child.is_hide_set() && child.get_name() != "help")
        {
            visit(child, problems, count);
        }
    }
    let (mut problems, mut count) = (Vec::new(), 0);
    visit(&command_tree(), &mut problems, &mut count);
    assert!(count > 15, "root and command groups must be covered");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn example_gate_rejects_removed_flags_and_preserves_shell_arguments() {
    assert!(!example_problems("Examples:\n  formless run fixture --removed-docs-flag").is_empty());
    assert!(!example_problems("Examples:\n  formless run \"unclosed").is_empty());
    for line in [
        "formless sessions show SESSION_ID",
        "formless runs show RUN_ID",
        "formless jobs history JOB_ID --json",
        "git diff | formless run \"Review this diff\" --output json",
        "printf 'a|b' | formless run -",
    ] {
        assert!(
            example_problems(&format!("Examples:\n  {line}")).is_empty(),
            "{line}"
        );
    }
    let args = example_args("formless run 'literal | with spaces'")
        .unwrap()
        .unwrap();
    assert_eq!(args[2], "literal | with spaces");
    let args = example_args("printf fixture | formless run \"formless\"")
        .unwrap()
        .unwrap();
    assert_eq!(args[2], "formless");
}

#[test]
fn docs_no_topic_help_matches_the_gallery_default() {
    let root = command_tree();
    let help = root
        .find_subcommand("docs")
        .unwrap()
        .get_arguments()
        .find(|arg| arg.get_id() == "topic")
        .unwrap()
        .get_help()
        .unwrap()
        .to_string();
    assert!(help.contains("omit to open the gallery"), "{help}");
    assert!(formless_core::docs::resolve(None)
        .unwrap()
        .ends_with("docs/gallery.md"));
}

#[test]
fn run_output_choices_explain_their_distinct_formats() {
    let root = command_tree();
    let output = root
        .find_subcommand("run")
        .unwrap()
        .get_arguments()
        .find(|arg| arg.get_id() == "output")
        .unwrap();
    for value in output
        .get_possible_values()
        .into_iter()
        .filter(|value| !value.is_hide_set())
    {
        assert!(
            informative(value.get_help().map(ToString::to_string), value.get_name()),
            "run --output {} needs an explanation",
            value.get_name()
        );
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Inventory {
    commands: BTreeSet<String>,
    flags: BTreeSet<String>,
}

fn actual_inventory(root: &Command) -> Inventory {
    let mut inventory = Inventory::default();
    for command in root
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
    {
        inventory.commands.insert(command.get_name().into());
    }
    for arg in root.get_arguments().filter(|arg| !arg.is_hide_set()) {
        for long in arg.get_long_and_visible_aliases().unwrap_or_default() {
            inventory.flags.insert(format!("--{long}"));
        }
        for short in arg.get_short_and_visible_aliases().unwrap_or_default() {
            inventory.flags.insert(format!("-{short}"));
        }
    }
    inventory
}

fn rendered_inventory(help: &str) -> Inventory {
    let flags = regex::Regex::new(r"--[a-z][a-z0-9-]*|-[a-zA-Z]\b").unwrap();
    let mut inventory = Inventory::default();
    let mut section = "";
    for line in help.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            section = match line {
                "Commands:" => "commands",
                "Options:" => "options",
                _ => "",
            };
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let row = line.trim_start();
        // Clap root command labels start in column 3; options start in
        // column 3 (short) or 7 (long only). Wrapped descriptions are farther
        // right, and flags mentioned in prose must never become inventory.
        if section == "commands" && indent == 2 {
            if let Some(name) = row.split_whitespace().next() {
                inventory.commands.insert(name.into());
            }
        } else if section == "options" && (2..=6).contains(&indent) && row.starts_with('-') {
            let label = row.split_once("  ").map_or(row, |(label, _)| label);
            inventory
                .flags
                .extend(flags.find_iter(label).map(|flag| flag.as_str().into()));
        }
    }
    inventory
}

fn section<'a>(document: &'a str, heading: &str, next_heading: &str) -> &'a str {
    let (_, rest) = document
        .split_once(heading)
        .expect("CLI section heading exists");
    rest.split_once(next_heading)
        .expect("CLI section is bounded")
        .0
}

// Only reference-table first columns define the inventory. Prose and nested
// command examples can mention arbitrary flags without becoming root flags.
fn documented_inventory(document: &str) -> Result<Inventory, String> {
    let flags = regex::Regex::new(r"--[a-z][a-z0-9-]*|-[a-zA-Z]\b").unwrap();
    let mut inventory = Inventory::default();
    for line in document
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
    {
        let cells: Vec<_> = line.split('|').map(str::trim).collect();
        let Some(cell) = cells.get(1) else { continue };
        let cell = cell.replace('`', "");
        if let Some(command) = cell
            .strip_prefix("formless ")
            .and_then(|text| text.split_whitespace().next())
        {
            if !inventory.commands.insert(command.into()) {
                return Err(format!("duplicate command row: {command}"));
            }
        } else if cell.starts_with("--") {
            // The second column contains short aliases; subsequent scope and
            // description columns are deliberately not parsed as flag names.
            let names = format!("{cell} {}", cells.get(2).unwrap_or(&""));
            for flag in flags.find_iter(&names) {
                if !inventory.flags.insert(flag.as_str().into()) {
                    return Err(format!("duplicate flag: {}", flag.as_str()));
                }
            }
        }
    }
    Ok(inventory)
}

fn inventory_drift(expected: &Inventory, documented: &Inventory) -> Vec<String> {
    let mut errors = Vec::new();
    for (kind, actual, docs) in [
        ("commands", &expected.commands, &documented.commands),
        ("flags", &expected.flags, &documented.flags),
    ] {
        for missing in actual.difference(docs) {
            errors.push(format!("missing {kind}: {missing}"));
        }
        for stale in docs.difference(actual) {
            errors.push(format!("stale {kind}: {stale}"));
        }
    }
    errors
}

#[test]
fn readme_and_spec_reference_all_visible_root_commands_and_flags_exactly() {
    let actual = actual_inventory(&command_tree());
    let help = match super::Cli::try_parse_from(["formless", "--help"]) {
        Err(error) => {
            assert_eq!(error.kind(), ErrorKind::DisplayHelp);
            error.to_string()
        }
        Ok(_) => panic!("root --help must stop before command dispatch"),
    };
    let rendered = rendered_inventory(&help);
    assert_eq!(
        actual, rendered,
        "rendered root --help differs from Clap metadata"
    );
    for (name, document) in [
        (
            "README §3.11",
            section(
                include_str!("../../../README.md"),
                "### 3.11 CLI reference",
                "### 3.12 ",
            ),
        ),
        (
            "SPEC §12",
            section(include_str!("../../../SPEC.md"), "## 12. CLI", "## 13. "),
        ),
    ] {
        let documented = documented_inventory(document).expect("valid reference inventory");
        let drift = inventory_drift(&actual, &documented);
        assert!(drift.is_empty(), "{name}:\n{}", drift.join("\n"));
        assert!(inventory_drift(&rendered, &documented).is_empty());
    }
}

#[test]
fn drift_check_rejects_missing_stale_and_duplicate_entries() {
    let expected = Inventory {
        commands: ["run".into(), "help".into()].into(),
        flags: ["--help".into(), "-h".into()].into(),
    };
    let good = "| `formless run TASK` | Run a task |\n| `formless help` | Read help |\n| `--help` | `-h` | Root | Print help |\n";
    assert!(inventory_drift(&expected, &documented_inventory(good).unwrap()).is_empty());
    let missing = good
        .replace("| `formless help` | Read help |\n", "")
        .replace("`-h`", "—");
    let drift = inventory_drift(&expected, &documented_inventory(&missing).unwrap());
    assert_eq!(drift, ["missing commands: help", "missing flags: -h"]);
    let stale =
        format!("{good}| `formless obsolete` | Removed |\n| `--gone` | `-x` | Root | Removed |\n");
    let drift = inventory_drift(&expected, &documented_inventory(&stale).unwrap());
    assert_eq!(
        drift,
        [
            "stale commands: obsolete",
            "stale flags: --gone",
            "stale flags: -x"
        ]
    );
    assert!(documented_inventory(&format!("{good}{good}")).is_err());
    // References outside first-column inventory rows must not hide omissions.
    let prose =
        "See `formless help` and `--help` / `-h`.\n| `formless run TASK` | Supports --session |\n";
    assert_eq!(documented_inventory(prose).unwrap().flags.len(), 0);
    let rendered = "Usage: fixture\n\nCommands:\n  run   Run a task\n  help  Print command help\n\nOptions:\n  -h, --help  Print help; do not count --stale or -x here\n              --wrapped is only description text\n\nExamples:\n  fixture obsolete --gone\n";
    assert_eq!(rendered_inventory(rendered), expected);
}

#[test]
fn help_check_rejects_blank_and_label_only_descriptions() {
    let mut fixture = Command::new("fixture")
        .about("Exercise negative fixtures")
        .arg(clap::Arg::new("missing").long("missing"))
        .arg(clap::Arg::new("repeated").long("repeated").help("repeated"))
        .subcommand(Command::new("blank"));
    fixture.build();
    let mut problems = Vec::new();
    help_problems(&fixture, "fixture", &mut problems);
    assert!(problems.iter().any(|problem| problem.contains("missing")));
    assert!(problems.iter().any(|problem| problem.contains("repeated")));
    assert!(problems
        .iter()
        .any(|problem| problem.contains("fixture blank:")));
}
