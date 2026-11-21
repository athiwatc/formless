//! Composer commands. Actions retain the shell's normal admission and review paths.

use crate::commands::Command;
use crate::model::Destination;
use crate::run_actions::Kind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arguments {
    None,
    Run,
    Search,
    Name,
    Instructions,
    Tags,
    Pin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub name: &'static str,
    pub args: &'static str,
    pub description: &'static str,
    pub command: Command,
    pub arguments: Arguments,
}

impl Entry {
    pub fn needs_args(&self) -> bool {
        !self.args.is_empty() && !self.args.starts_with('[')
    }

    pub fn usage(&self) -> String {
        format!(
            "/{}{}{}",
            self.name,
            if self.args.is_empty() { "" } else { " " },
            self.args
        )
    }
}

macro_rules! entry {
    ($name:literal, $args:literal, $desc:literal, $command:expr, $arguments:ident) => {
        Entry {
            name: $name,
            args: $args,
            description: $desc,
            command: $command,
            arguments: Arguments::$arguments,
        }
    };
}

pub const COMMANDS: &[Entry] = &[
    entry!(
        "help",
        "",
        "all commands and keys",
        Command::ShowCommands,
        None
    ),
    entry!("new", "", "start a new Session", Command::NewSession, None),
    entry!(
        "sessions",
        "",
        "list and switch Sessions",
        Command::GoTo(Destination::Sessions),
        None
    ),
    entry!(
        "fork",
        "",
        "fork the current Session",
        Command::ReviewFork,
        None
    ),
    entry!(
        "search",
        "[QUERY]",
        "search all Sessions",
        Command::GoTo(Destination::ProfileSearch),
        Search
    ),
    entry!(
        "edit",
        "",
        "edit your last message and retry from there",
        Command::RunAction(Kind::Edit),
        None
    ),
    entry!(
        "retry",
        "[--model MODEL]",
        "re-run your last message, optionally on another model; the old answer stays in a fork",
        Command::RunAction(Kind::Retry),
        Run
    ),
    entry!(
        "good",
        "[comment]",
        "rate the last answer 👍 (feeds Run metrics and routing)",
        Command::RunAction(Kind::Good),
        Run
    ),
    entry!(
        "bad",
        "[comment]",
        "rate the last answer 👎 (feeds Run metrics and routing)",
        Command::RunAction(Kind::Bad),
        Run
    ),
    entry!(
        "resend",
        "",
        "re-send a stalled provider request",
        Command::RunAction(Kind::ProviderRetry),
        None
    ),
    entry!(
        "compact",
        "[FOCUS]",
        "summarise the Session now, keeping what FOCUS asks",
        Command::RunAction(Kind::Compact),
        Run
    ),
    entry!(
        "cd",
        "[PATH]",
        "show or change this Session's working directory",
        Command::RunAction(Kind::Cwd),
        Run
    ),
    entry!(
        "rename",
        "[NAME]",
        "rename the Session",
        Command::SessionDetails,
        Name
    ),
    entry!(
        "instructions",
        "[TEXT|clear]",
        "standing instructions for this Session's agent",
        Command::SessionDetails,
        Instructions
    ),
    entry!(
        "tag",
        "[NAME…]",
        "tag this Session, or list tags",
        Command::SessionDetails,
        Tags
    ),
    entry!(
        "pin",
        "",
        "pin or unpin this Session (P in Sessions)",
        Command::SessionDetails,
        Pin
    ),
    entry!(
        "model",
        "",
        "pick the model and effort (fuzzy search)",
        Command::SessionModel,
        None
    ),
    entry!(
        "stop",
        "",
        "stop the current Run (Esc)",
        Command::Stop,
        None
    ),
    entry!(
        "resume",
        "",
        "continue an interrupted or paused Run",
        Command::ResumeRun,
        None
    ),
    entry!(
        "abandon",
        "",
        "drop an interrupted or paused Run",
        Command::AbandonRun,
        None
    ),
    entry!(
        "approve",
        "",
        "list approvals waiting for you",
        Command::Approvals,
        None
    ),
    entry!(
        "invocations",
        "",
        "list pending Invocations",
        Command::ViewInvocations,
        None
    ),
    entry!(
        "context",
        "",
        "how full the context window is, and what fills it",
        Command::ToggleContext,
        None
    ),
    entry!(
        "tools",
        "",
        "open the Tools view",
        Command::GoTo(Destination::Tools),
        None
    ),
    entry!(
        "metrics",
        "",
        "open the Metrics view",
        Command::GoTo(Destination::Metrics),
        None
    ),
    entry!(
        "proposals",
        "",
        "open the prompt Proposals view",
        Command::GoTo(Destination::Proposals),
        None
    ),
    entry!(
        "settings",
        "",
        "open the Settings view",
        Command::GoTo(Destination::Settings),
        None
    ),
    entry!(
        "export",
        "",
        "export this Session: a re-importable bundle, or a readable transcript",
        Command::SessionDetails,
        None
    ),
    entry!(
        "usage",
        "",
        "tokens and cost of this Session",
        Command::SessionUsage,
        None
    ),
    entry!(
        "step",
        "",
        "step mode: pause before each Tool call (Enter run, e edit, s skip)",
        Command::ToggleStepMode,
        None
    ),
];

// Reserved TUI/web roots must never become model messages by accident.
const UNAVAILABLE: &str = "template templates relay untag editor attach paste open copy pager table undo cancel kill drop summarise-branch compare compare-models unrate recap review files rewind queue limit details agents thinking timestamps tool modules module tooloff toolpin tooltag propose revoke approvals audit descriptionreviews security roles role panes mouse harness mcp test routing pull prompts prompt skills memory remember forget jobs status storage gc backup docs setup tour notes vim restart-daemon quit pending providers tags";

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed<'a> {
    Message(&'a str),
    Command(&'static Entry, &'a str),
    Unavailable(&'a str),
    Unknown(&'a str),
}

pub fn message_text(text: &str) -> &str {
    if text.starts_with("//") {
        &text[1..]
    } else {
        text
    }
}

pub fn parse(text: &str) -> Parsed<'_> {
    if text.starts_with("//") {
        return Parsed::Message(message_text(text));
    }
    let Some(invocation) = text.strip_prefix('/') else {
        return Parsed::Message(text);
    };
    let invocation = invocation.trim();
    let (name, args) = invocation
        .split_once(char::is_whitespace)
        .unwrap_or((invocation, ""));
    let name = if name.is_empty() { "help" } else { name };
    if let Some(command) = COMMANDS.iter().find(|command| command.name == name) {
        Parsed::Command(command, args.trim())
    } else if UNAVAILABLE
        .split_whitespace()
        .any(|reserved| reserved == name)
        || name.starts_with("mcp:")
        || name.starts_with("skill:")
    {
        Parsed::Unavailable(name)
    } else {
        Parsed::Unknown(name)
    }
}

pub fn matches(query: &str) -> Vec<&'static Entry> {
    let q = query.trim_start();
    let mut scored: Vec<_> = COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| score(q, entry.name).map(|score| (score, index, entry)))
        .collect();
    scored.sort_by(|a, b| {
        b.2.name
            .starts_with(q)
            .cmp(&a.2.name.starts_with(q))
            .then(b.0.cmp(&a.0))
            .then(a.1.cmp(&b.1))
    });
    scored.into_iter().map(|(_, _, entry)| entry).collect()
}

// TUI's greedy best-start scoring; command names are ASCII.
fn score(query: &str, candidate: &str) -> Option<i64> {
    let query: Vec<_> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let candidate: Vec<_> = candidate.chars().collect();
    let mut best = None;
    for start in (0..candidate.len()).filter(|&i| candidate[i] == query[0]) {
        let mut matched = 0;
        let mut score = 0;
        let mut previous: Option<usize> = None;
        for i in start..candidate.len() {
            if matched == query.len() {
                break;
            }
            if candidate[i] != query[matched] {
                continue;
            }
            score += 1;
            if previous.is_some_and(|p| p + 1 == i) {
                score += 5;
            }
            if i == 0 || !candidate[i - 1].is_alphanumeric() {
                score += 8;
            }
            if let Some(p) = previous {
                score -= (i - p - 1).min(10) as i64;
            }
            matched += 1;
            previous = Some(i);
        }
        if matched != query.len() {
            break;
        }
        let score = score - (candidate.len() / 8) as i64;
        best = Some(best.map_or(score, |previous: i64| previous.max(score)));
    }
    best
}

pub fn closest(word: &str) -> Option<&'static str> {
    if word.is_empty() || word.chars().count() > 128 {
        return None;
    }
    COMMANDS
        .iter()
        .map(|entry| (distance(word, entry.name), entry.name))
        .filter(|(distance, name)| *distance <= 2.max(name.len() / 3))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, name)| name)
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<_> = b.chars().collect();
    let mut previous: Vec<_> = (0..=b.len()).collect();
    for (i, a) in a.chars().enumerate() {
        let mut row = vec![i + 1];
        for (j, b) in b.iter().enumerate() {
            row.push(
                (row[j] + 1)
                    .min(previous[j + 1] + 1)
                    .min(previous[j] + usize::from(a != *b)),
            );
        }
        previous = row;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_is_subsequence_prefix_first_and_stable() {
        assert_eq!(
            matches("").iter().map(|e| e.name).collect::<Vec<_>>(),
            COMMANDS.iter().map(|e| e.name).collect::<Vec<_>>()
        );
        assert_eq!(matches("se")[0].name, "search");
        let tied: Vec<_> = matches("se")
            .into_iter()
            .filter(|e| e.name == "sessions" || e.name == "settings")
            .map(|e| e.name)
            .collect();
        assert_eq!(tied, ["sessions", "settings"]);
        assert_eq!(matches("CMP")[0].name, "compact");
        assert!(matches("zzzz").is_empty());
        assert!(score("abc", "axbyc").unwrap() < score("abc", "abc").unwrap());
    }

    #[test]
    fn parsing_never_sends_unknown_or_unavailable_commands() {
        assert!(
            matches!(parse("/compact keep tests"), Parsed::Command(e, "keep tests") if e.name == "compact")
        );
        assert!(matches!(parse("/"), Parsed::Command(e, "") if e.name == "help"));
        assert!(matches!(parse("/  settings  "), Parsed::Command(e, "") if e.name == "settings"));
        assert_eq!(parse("/cancel"), Parsed::Unavailable("cancel"));
        assert_eq!(
            parse("/mcp:server:prompt x"),
            Parsed::Unavailable("mcp:server:prompt")
        );
        assert_eq!(parse("/nonsense"), Parsed::Unknown("nonsense"));
        assert_eq!(parse("hello"), Parsed::Message("hello"));
        assert_eq!(parse("//settings"), Parsed::Message("/settings"));
        assert_eq!(parse("//"), Parsed::Message("/"));
        assert_eq!(parse("///path"), Parsed::Message("//path"));
    }

    #[test]
    fn required_arguments_and_suggestions() {
        let mut entry = *COMMANDS.iter().find(|e| e.name == "new").unwrap();
        assert!(!entry.needs_args());
        entry.args = "[FOCUS]";
        assert!(!entry.needs_args());
        entry.args = "NAME";
        assert!(entry.needs_args());
        assert_eq!(entry.usage(), "/new NAME");
        assert_eq!(closest("setings"), Some("settings"));
        assert_eq!(closest("zzzzzzzzzzzzzzzzzzzz"), None);
        assert_eq!(closest(""), None);
        assert_eq!(closest(&"a".repeat(129)), None);
    }
}
