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
    cmd(
        "new",
        "[TEMPLATE]",
        "start a new Session, optionally from a template",
        Session,
    ),
    cmd(
        "template",
        "save|delete NAME [FIRST MESSAGE]",
        "save this Session's setup as a template, or delete one",
        Session,
    ),
    cmd("templates", "", "list Session templates", Session),
    cmd(
        "relay",
        "",
        "pair a phone or browser: the relay link as a QR code",
        Settings,
    ),
    cmd(
        "export",
        "[json|md|html]",
        "export this Session: a re-importable bundle, or a readable transcript",
        Session,
    ),
    cmd(
        "pin",
        "",
        "pin or unpin this Session (P in Sessions)",
        Session,
    ),
    cmd(
        "tag",
        "[NAME…]",
        "tag this Session, or list tags (filter Sessions with /#tag)",
        Session,
    ),
    cmd("untag", "NAME…", "remove tags from this Session", Session),
    cmd(
        "cd",
        "[PATH]",
        "show or change this Session's working directory",
        Session,
    ),
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
    cmd(
        "paste",
        "",
        "attach an image from the clipboard (also Ctrl+V)",
        Session,
    ),
    cmd("open", "[ID]", "show a stored output full-screen", Session),
    cmd(
        "open link",
        "[N]",
        "open the Nth link in the last answer in your browser",
        Session,
    ),
    cmd(
        "open path",
        "FILE[:LINE]",
        "open a file the chat mentions in $EDITOR",
        Session,
    ),
    cmd(
        "copy",
        "[N | code [N]]",
        "copy the last answer, the Nth message from the bottom, or a code block",
        Session,
    ),
    cmd("usage", "", "tokens and cost of this Session", Session),
    cmd(
        "pager diagram",
        "[N]",
        "a Mermaid diagram at full size (←/→ scroll)",
        Session,
    ),
    cmd(
        "open diagram",
        "[N]",
        "open a Mermaid diagram's source in mermaid.live",
        Session,
    ),
    cmd(
        "pager code",
        "[N]",
        "open a code block unwrapped with line numbers (←/→ scroll)",
        Session,
    ),
    cmd(
        "table",
        "[N]",
        "open the last answer's Nth table full-width (←/→ scroll)",
        Session,
    ),
    cmd(
        "compact",
        "[FOCUS]",
        "summarise the Session now, keeping what FOCUS asks",
        Session,
    ),
    cmd(
        "rename",
        "[NAME|auto]",
        "rename the Session; auto suggests a name from recent messages",
        Session,
    ),
    cmd(
        "instructions",
        "[TEXT|clear]",
        "standing instructions for this Session's agent",
        Session,
    ),
    cmd(
        "context",
        "",
        "how full the context window is, and what fills it",
        Session,
    ),
    cmd(
        "undo",
        "[tool]",
        "undo the last Session delete; `tool`: the last Tool registry change",
        Session,
    ),
    cmd("stop", "", "stop the current Run (Esc)", Run),
    cmd("cancel", "", "cancel the stalled or pending Tool call", Run),
    cmd("resend", "", "re-send a stalled provider request", Run),
    cmd("resume", "", "continue an interrupted or paused Run", Run),
    cmd("abandon", "", "drop an interrupted or paused Run", Run),
    cmd("details", "", "show the last provider error in full", Run),
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
    cmd(
        "timestamps",
        "",
        "show or hide message times and day separators",
        Settings,
    ),
    cmd("tools", "", "open the Tools view", Tools),
    cmd(
        "tool",
        "NAME",
        "open a Tool's detail in the Tools view",
        Tools,
    ),
    cmd("modules", "", "list shared modules", Tools),
    cmd(
        "module",
        "NAME [edit|rollback N|delete]",
        "open a shared module: code, versions, Tools using it",
        Tools,
    ),
    cmd(
        "tooloff",
        "NAME",
        "switch a Tool off for the agent, or back on (not a retirement)",
        Tools,
    ),
    cmd(
        "toolpin",
        "NAME",
        "pin or unpin a Tool: pinned Tools are always listed to the agent",
        Tools,
    ),
    cmd(
        "tooltag",
        "NAME [TAG…]",
        "set a Tool's tags (none clears them); filter the Tools view with #tag",
        Tools,
    ),
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
    cmd(
        "panes",
        "[wider|narrower|left|right|reset]",
        "toggle the multi-pane layout, or resize/hide its side panes",
        Settings,
    ),
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
    cmd("notes", "", "every note so far, in the pager", App),
    cmd(
        "help",
        "",
        "all commands and keys (also ? on an empty input)",
        App,
    ),
    cmd(
        "restart-daemon",
        "",
        "restart the daemon (e.g. after an upgrade); waits for no Run to be going",
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
        // Ties go to the command as long as what was typed ("modle" is
        // "model", not "module").
        .min_by_key(|(d, n)| (*d, n.chars().count().abs_diff(word.chars().count())))
        .map(|(_, n)| n)
}

/// Edit distance where swapping two adjacent letters counts as one edit
/// (optimal string alignment).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        d[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
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
    (
        "j / k, g / G, Ctrl+D / Ctrl+U",
        "move in list views (Sessions, Tools, Metrics, Proposals)",
    ),
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
        assert_eq!(closest("modlue text_utils"), Some("module"));
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
