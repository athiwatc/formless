//! The slash-command table (SPEC §10.2): one list drives the `/` palette,
//! `/help` and "did you mean", and a test checks the dispatcher handles
//! every entry, so they can't drift apart.

use formless_core::fuzzy::fuzzy_match;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Session,
    Run,
    Tools,
    Settings,
    Integrations,
    App,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Session => "Session",
            Group::Run => "Run control",
            Group::Tools => "Tools",
            Group::Settings => "Settings",
            Group::Integrations => "Integrations",
            Group::App => "App",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Command {
    /// What follows the `/`, including any sub-command (`role add`).
    pub name: &'static str,
    /// Argument syntax; `[...]` marks optional ones.
    pub args: &'static str,
    pub desc: &'static str,
    pub group: Group,
}

impl Command {
    /// Whether the command needs an argument before it can run.
    pub fn needs_args(&self) -> bool {
        !self.args.is_empty() && !self.args.starts_with('[')
    }

    pub fn usage(&self) -> String {
        if self.args.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.args)
        }
    }
}

const fn cmd(name: &'static str, args: &'static str, desc: &'static str, group: Group) -> Command {
    Command {
        name,
        args,
        desc,
        group,
    }
}

use Group::*;

pub const COMMANDS: &[Command] = &[
    cmd("new", "", "start a new Session", Session),
    cmd("sessions", "", "list and switch Sessions", Session),
    cmd("fork", "", "fork the current Session", Session),
    cmd("search", "", "search all Sessions", Session),
    cmd(
        "edit",
        "",
        "edit your last message and retry from there",
        Session,
    ),
    cmd(
        "attach",
        "PATH",
        "attach a file to your next message",
        Session,
    ),
    cmd("open", "[ID]", "show a stored output full-screen", Session),
    cmd(
        "copy",
        "[code [N]]",
        "copy the last answer or a code block",
        Session,
    ),
    cmd("usage", "", "tokens and cost of this Session", Session),
    cmd("undo", "", "undo the last Session delete", Session),
    cmd("stop", "", "stop the current Run (Esc)", Run),
    cmd("cancel", "", "cancel the stalled or pending Tool call", Run),
    cmd("resend", "", "re-send a stalled provider request", Run),
    cmd("resume", "", "continue an interrupted or paused Run", Run),
    cmd("abandon", "", "drop an interrupted or paused Run", Run),
    cmd("approve", "", "list approvals waiting for you", Run),
    cmd("invocations", "", "list pending Invocations", Run),
    cmd("kill", "HANDLE", "cancel a pending Invocation", Run),
    cmd(
        "drop",
        "HANDLE",
        "stop waiting for a pending Invocation",
        Run,
    ),
    cmd(
        "agents",
        "[N]",
        "list sub-agents, or show one's transcript",
        Run,
    ),
    cmd("thinking", "", "show or hide model thinking", Run),
    cmd("tools", "", "open the Tools view", Tools),
    cmd("metrics", "", "open the Metrics view", Tools),
    cmd("proposals", "", "open the prompt Proposals view", Tools),
    cmd("propose", "", "generate a measured prompt proposal", Tools),
    cmd(
        "revoke",
        "TOOL [CAPABILITY]",
        "revoke a Tool's granted Capabilities",
        Tools,
    ),
    cmd("audit", "[FILTER]", "show the audit log", Tools),
    cmd("settings", "", "open the Settings view", Settings),
    cmd(
        "model",
        "[ID]",
        "pick the model and effort (fuzzy search)",
        Settings,
    ),
    cmd("roles", "", "list providers and Roles", Settings),
    cmd(
        "role add",
        "NAME PROVIDER MODEL DESCRIPTION",
        "add or replace a Role",
        Settings,
    ),
    cmd("role rm", "NAME", "remove a Role", Settings),
    cmd("panes", "", "toggle the multi-pane layout", Settings),
    cmd("mouse", "", "release the mouse to select text", Settings),
    cmd("harness", "", "list the agent's harness changes", Settings),
    cmd(
        "harness undo",
        "[ID]",
        "undo a harness change (default: the last)",
        Settings,
    ),
    cmd("mcp", "", "list MCP servers", Integrations),
    cmd("mcp restart", "NAME", "restart an MCP server", Integrations),
    cmd("prompts", "", "list MCP prompts", Integrations),
    cmd(
        "prompt",
        "SERVER:NAME [KEY=VALUE …]",
        "insert an MCP prompt",
        Integrations,
    ),
    cmd("skills", "", "list Skills", Integrations),
    cmd("skills enable", "NAME", "enable a Skill", Integrations),
    cmd("skills disable", "NAME", "disable a Skill", Integrations),
    cmd("memory", "", "list saved memories", Integrations),
    cmd("memory edit", "ID TEXT", "change a memory", Integrations),
    cmd("memory rm", "ID", "delete a memory", Integrations),
    cmd("memory on", "ID", "use a memory again", Integrations),
    cmd("memory off", "ID", "stop using a memory", Integrations),
    cmd("jobs", "", "list scheduled jobs", Integrations),
    cmd("jobs enable", "ID", "enable (approve) a job", Integrations),
    cmd("jobs disable", "ID", "pause a job", Integrations),
    cmd("jobs rm", "ID", "delete a job", Integrations),
    cmd(
        "help",
        "",
        "all commands and keys (also ? on an empty input)",
        App,
    ),
    cmd("quit", "", "quit the TUI (the daemon keeps running)", App),
];

/// Commands matching what is typed after `/`, best first. An empty query
/// lists them all in table order.
pub fn matches(query: &str) -> Vec<&'static Command> {
    let q = query.trim_start();
    if q.is_empty() {
        return COMMANDS.iter().collect();
    }
    let mut scored: Vec<(i64, usize, &Command)> = COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(i, c)| fuzzy_match(q, c.name).map(|(s, _)| (s, i, c)))
        .collect();
    // Exact prefixes first (so `mem` lists `memory` before `harness undo`).
    scored.sort_by(|a, b| {
        let pa = a.2.name.starts_with(q);
        let pb = b.2.name.starts_with(q);
        pb.cmp(&pa).then(b.0.cmp(&a.0)).then(a.1.cmp(&b.1))
    });
    scored.into_iter().map(|(_, _, c)| c).collect()
}

/// The closest command name to a mistyped one, for "did you mean".
pub fn closest(typed: &str) -> Option<&'static str> {
    let word = typed.split_whitespace().next().unwrap_or("");
    if word.is_empty() {
        return None;
    }
    COMMANDS
        .iter()
        .map(|c| c.name.split(' ').next().unwrap_or(c.name))
        .map(|n| (edit_distance(word, n), n))
        .filter(|(d, n)| *d <= 2.max(n.len() / 3))
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// `/help`: commands grouped by area, then keys.
pub fn help_lines() -> Vec<String> {
    let mut out = Vec::new();
    let width = COMMANDS
        .iter()
        .map(|c| c.usage().chars().count())
        .max()
        .unwrap_or(0);
    let mut group = None;
    let mut sorted: Vec<&Command> = COMMANDS.iter().collect();
    sorted.sort_by_key(|c| c.group);
    for c in sorted {
        if group != Some(c.group) {
            if group.is_some() {
                out.push(String::new());
            }
            out.push(c.group.title().to_string());
            group = Some(c.group);
        }
        out.push(format!("  {:<width$}  {}", c.usage(), c.desc));
    }
    out.push(String::new());
    out.push("Keys".into());
    for (k, d) in KEYS {
        out.push(format!("  {k:<width$}  {d}"));
    }
    out
}

const KEYS: &[(&str, &str)] = &[
    ("Enter", "send, or steer the running Run"),
    ("Ctrl+J / Ctrl+Enter / Alt+Enter", "queue for after the Run"),
    ("Esc", "stop the Run / close a dialog"),
    ("Ctrl+C", "clear the input; twice to detach"),
    ("Tab / Shift+Tab", "switch views"),
    ("PgUp / PgDn, ↑ / ↓", "scroll the chat"),
    ("/", "command palette (Tab completes)"),
    ("@", "reference a file"),
    ("?", "this help (on an empty input)"),
    (
        "Shift+drag",
        "select text (Option in iTerm2, Fn in Terminal.app)",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_args_well_formed() {
        for (i, c) in COMMANDS.iter().enumerate() {
            assert!(
                !COMMANDS[..i].iter().any(|o| o.name == c.name),
                "duplicate /{}",
                c.name
            );
            assert!(!c.desc.is_empty() && !c.name.starts_with('/'));
        }
        assert!(COMMANDS
            .iter()
            .find(|c| c.name == "attach")
            .unwrap()
            .needs_args());
        assert!(!COMMANDS
            .iter()
            .find(|c| c.name == "model")
            .unwrap()
            .needs_args());
    }

    #[test]
    fn palette_filters_fuzzily() {
        assert_eq!(matches("ses")[0].name, "sessions");
        assert_eq!(matches("mem")[0].name, "memory");
        assert_eq!(matches("rladd")[0].name, "role add");
        assert_eq!(matches("").len(), COMMANDS.len());
        assert!(matches("zzz").is_empty());
    }

    #[test]
    fn suggests_the_closest_command() {
        assert_eq!(closest("sesions"), Some("sessions"));
        assert_eq!(closest("tols"), Some("tools"));
        assert_eq!(closest("modle gpt"), Some("model"));
        assert_eq!(closest("xyzzyplugh"), None);
    }

    #[test]
    fn help_is_grouped() {
        let help = help_lines();
        let session = help.iter().position(|l| l == "Session").unwrap();
        let run = help.iter().position(|l| l == "Run control").unwrap();
        let attach = help
            .iter()
            .position(|l| l.contains("/attach PATH"))
            .unwrap();
        assert!(session < attach && attach < run);
        assert!(help.iter().any(|l| l == "Keys"));
    }
}
