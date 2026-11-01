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
    cmd("sessions", "", "list and switch Sessions (T toggles lineage tree)", Session),
    cmd("fork", "", "fork the current Session", Session),
    cmd("search", "", "search all Sessions", Session),
    cmd(
        "edit",
        "",
        "edit your last message and retry from there",
        Session,
    ),
    cmd(
        "retry",
        "[--model MODEL]",
        "re-run your last message, optionally on another model; the old answer stays in a fork",
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
    cmd(
        "summarise-branch",
        "SOURCE_SESSION",
        "generate a branch summary for review, then insert one note here",
        Session,
    ),
    cmd(
        "compare",
        "SESSION_A SESSION_B",
        "compare stored branches and recorded usage (no model call)",
        Session,
    ),
    cmd(
        "compare-models",
        "A B [prompt]",
        "the same prompt on two models, side by side with time and cost",
        Run,
    ),
    cmd(
        "step",
        "",
        "step mode: pause before each Tool call (Enter run, e edit, s skip)",
        Run,
    ),
    cmd(
        "good",
        "[comment]",
        "rate the last answer 👍 (feeds Run metrics and routing)",
        Run,
    ),
    cmd(
        "bad",
        "[comment]",
        "rate the last answer 👎 (feeds Run metrics and routing)",
        Run,
    ),
    cmd("unrate", "", "clear your rating of the last answer", Run),
    cmd(
        "recap",
        "",
        "expand or fold the last long Run's \"What I did\"",
        Run,
    ),
    cmd(
        "files",
        "",
        "the files the last Run changed, with diffs",
        Run,
    ),
    cmd(
        "rewind",
        "[PATH]",
        "put the last Run's file changes back (one file, or all)",
        Run,
    ),
    cmd(
        "queue",
        "[edit|rm|up|down N]",
        "the messages queued for after this Run: edit, remove or reorder them",
        Run,
    ),
    cmd(
        "limit",
        "N|off",
        "cap the current (or next) Run at N steps; it pauses there",
        Run,
    ),
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
        "TOOL [CAPABILITY] | all [FAMILY]",
        "revoke a Tool's granted Capabilities, or every grant (of a family)",
        Tools,
    ),
    cmd(
        "approvals",
        "[FAMILY]",
        "every approval: Tool, Capability, scope, when, by whom, last used",
        Tools,
    ),
    cmd("audit", "[FILTER]", "show the audit log", Tools),
    cmd("descriptionreviews", "", "retained description warnings and before/after changes", Tools),
    cmd(
        "security",
        "",
        "what the daemon can reach: grants, denials, trusted folders, MCP, hooks, relay, web",
        Tools,
    ),
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
    cmd(
        "test",
        "[MODEL]",
        "test a model: latency, streaming, reasoning, tool calls",
        Settings,
    ),
    cmd(
        "routing",
        "",
        "why routing picked each model lately",
        Settings,
    ),
    cmd("pull", "MODEL", "Ollama: download a model", Settings),
    cmd("mcp log", "NAME", "an MCP server's stderr log", Integrations),
    cmd(
        "mcp tools",
        "NAME",
        "an MCP server's tools, on or off, with token cost",
        Integrations,
    ),
    cmd("mcp tool", "NAME TOOL on|off", "turn one MCP tool on or off", Integrations),
    cmd("mcp catalog", "", "popular MCP servers you can add", Integrations),
    cmd("mcp add", "ID [key=value]", "add a server from the catalog", Integrations),
    cmd(
        "mcp import",
        "[NAME]",
        "servers configured in Claude Desktop, Cursor or VS Code",
        Integrations,
    ),
    cmd(
        "mcp:",
        "SERVER:PROMPT [ARGS]",
        "send an MCP prompt as your message",
        Integrations,
    ),
    cmd("prompts", "", "list MCP prompts", Integrations),
    cmd(
        "prompt",
        "SERVER:NAME [KEY=VALUE …]",
        "insert an MCP prompt",
        Integrations,
    ),
    cmd(
        "skills",
        "[NAME]",
        "list Skills, or preview one",
        Integrations,
    ),
    cmd(
        "skill:",
        "NAME [ARGS]",
        "load a Skill into this Session",
        Integrations,
    ),
    cmd("skills enable", "NAME", "enable a Skill", Integrations),
    cmd("skills disable", "NAME", "disable a Skill", Integrations),
    cmd("memory", "", "list saved memories", Integrations),
    cmd(
        "remember",
        "TEXT",
        "save a preference or fact as a memory",
        Integrations,
    ),
    cmd(
        "forget",
        "TEXT",
        "find memories mentioning TEXT; the same again deletes them",
        Integrations,
    ),
    cmd("memory edit", "ID TEXT", "change a memory", Integrations),
    cmd("memory rm", "ID", "delete a memory", Integrations),
    cmd("memory on", "ID", "use a memory again", Integrations),
    cmd("memory off", "ID", "stop using a memory", Integrations),
    cmd(
        "jobs",
        "[enable|disable|run|history|rm ID]",
        "list scheduled jobs, or act on one",
        Integrations,
    ),
    cmd("jobs enable", "ID", "enable (approve) a job", Integrations),
    cmd("jobs disable", "ID", "pause a job", Integrations),
    cmd("jobs rm", "ID", "delete a job", Integrations),
    cmd(
        "status",
        "",
        "the daemon: memory, CPU, database, running work",
        App,
    ),
    cmd("storage", "", "disk used by Sessions, artifacts, logs, backups", App),
    cmd("gc", "[run]", "what clean-up would free; `run` frees it", App),
    cmd("backup", "", "back up the database now", App),
    cmd(
        "docs",
        "[topic]",
        "open the guides: gallery, capabilities, approvals, keys, troubleshooting, mcp, skills, jobs",
        App,
    ),
    cmd(
        "setup",
        "",
        "the setup wizard again: re-enter the API key, or change provider",
        App,
    ),
    cmd(
        "tour",
        "[skip]",
        "a 2-minute guided first task; `skip` hides the offer",
        App,
    ),
    cmd("notes", "", "every note so far, in the pager", App),
    cmd("vim", "[on|off]", "toggle persistent Vim editing in composer and Tool editor", App),
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
            out.push(crate::i18n::tr(c.group.title()).to_string());
            group = Some(c.group);
        }
        out.push(format!(
            "  {:<width$}  {}",
            c.usage(),
            crate::i18n::tr(c.desc)
        ));
    }
    out.push(String::new());
    out.push("Guides: /docs [topic] (gallery, capabilities, keys, troubleshooting…)".into());
    out.push(String::new());
    out.push(crate::i18n::tr("Keys").into());
    for (k, d) in KEYS {
        out.push(format!("  {k:<width$}  {d}"));
    }
    out
}

const KEYS: &[(&str, &str)] = &[
    ("Enter", "send, or steer the running Run"),
    ("Ctrl+J / Ctrl+Enter / Alt+Enter", "queue for after the Run"),
    (
        "Esc",
        "stop the Run / close a dialog (Vim Insert: enter Normal first)",
    ),
    (
        "Vim: i/a/I/A, Esc",
        "enter Insert / return to Normal; mode shown in input border",
    ),
    (
        "Vim: w/b/e/0/$/gg/G, h/j/k/l",
        "move within composer or Tool editor; list keys unchanged",
    ),
    (
        "Vim: d/c/y + motion or object",
        "iw/aw, iW/aW, quotes, ()/[]/{}/<>; dd/cc/yy for lines",
    ),
    (
        "Vim: . / p / P",
        "repeat last change / paste private yank register (no OS clipboard)",
    ),
    (
        "Vim Tool editor: Ctrl+S / Ctrl+Q",
        "save / explicitly cancel; Esc keeps edited text",
    ),
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

/// The web UI's keys (SPEC §10.5), as its help shows them.
const WEB_KEYS: &[(&str, &str)] = &[
    ("Enter", "send, or steer the running Run"),
    ("Shift+Enter", "new line"),
    ("Ctrl/⌘+Enter", "queue for after the Run"),
    ("Esc", "close a dialog, or stop the Run"),
    ("Ctrl/⌘+K", "switch Session"),
    ("/", "focus the message box"),
    ("?", "commands and keys"),
    ("1 2 3 4 5", "Chat · Tools · Metrics · Proposals · Settings"),
    ("[ ]", "previous / next view (TUI Shift+Tab / Tab)"),
    ("j / ↓, k / ↑", "next / previous Session or Tool"),
    ("n", "new Session"),
    ("a", "show the pending approval or question"),
    ("y · o · n", "approvals: always · once · deny"),
];

/// The keyboard and command reference (`docs/keys.md`, SPEC §10.5),
/// generated from the same tables as `/help`: `formless docs --keys`.
pub fn reference_markdown() -> String {
    let cell = |s: &str| s.replace('|', "\\|");
    let mut out = String::from(
        "# Keys and commands\n\nGenerated by `formless docs --keys` from the tables `/help` shows; do not edit by hand. These are the main keys and every command; each view, list and dialog shows its own keys in its footer.\n\n## TUI keys\n\n| Key | What it does |\n|---|---|\n",
    );
    for (k, d) in KEYS {
        out.push_str(&format!("| {} | {} |\n", cell(k), cell(d)));
    }
    out.push_str("\n## Web keys\n\n| Key | What it does |\n|---|---|\n");
    for (k, d) in WEB_KEYS {
        out.push_str(&format!("| {} | {} |\n", cell(k), cell(d)));
    }
    out.push_str("\n## Commands\n\nType `/` in the TUI or the web composer; the same commands work in both (a few are TUI-only).\n");
    let mut sorted: Vec<&Command> = COMMANDS.iter().collect();
    sorted.sort_by_key(|c| c.group);
    let mut group = None;
    for c in sorted {
        if group != Some(c.group) {
            out.push_str(&format!(
                "\n### {}\n\n| Command | What it does |\n|---|---|\n",
                c.group.title()
            ));
            group = Some(c.group);
        }
        out.push_str(&format!("| `{}` | {} |\n", cell(&c.usage()), cell(c.desc)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_page_matches_the_tables() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let page = std::fs::read_to_string(root.join("docs/keys.md")).unwrap_or_default();
        assert!(
            page.replace("\r\n", "\n") == reference_markdown(),
            "docs/keys.md is stale: run `formless docs --keys > docs/keys.md`"
        );
        // Every web key listed here has a handler in the web UI (#1085):
        // the code that reacts to it, not just help text naming it. A key
        // added to the table needs its handler named here too.
        let app = std::fs::read_to_string(root.join("relay/web/app.js")).unwrap();
        let handlers: &[(&str, &[&str])] = &[
            (
                "Enter",
                &[
                    r#"else if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); submit(false); }"#,
                ],
            ),
            (
                "Shift+Enter",
                &[r#"e.key === "Enter" && !e.shiftKey && !e.isComposing"#],
            ),
            (
                "Ctrl/⌘+Enter",
                &[
                    r#"if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submit(true); }"#,
                ],
            ),
            ("Esc", &[r#"else if (e.key === "Escape") {"#]),
            (
                "Ctrl/⌘+K",
                &[
                    r#"e.key.toLowerCase() === "k" && S.channel"#,
                    "openSwitcher();",
                ],
            ),
            ("/", &[r#"keys: ["/"],"#]),
            ("?", &[r#"e.key === "?" && !$("input").value"#]),
            ("1 2 3 4 5", &[r#"keys: ["1", "2", "3", "4", "5"]"#]),
            ("[ ]", &[r#"keys: ["[", "]"]"#]),
            (
                "j / ↓, k / ↑",
                &[r#"keys: ["j", "ArrowDown"]"#, r#"keys: ["k", "ArrowUp"]"#],
            ),
            ("n", &[r#"keys: ["n"],"#]),
            ("a", &[r#"keys: ["a"],"#]),
            (
                "y · o · n",
                &[
                    r#"if (e.key === "y")"#,
                    r#"if (e.key === "o""#,
                    r#"if (e.key === "n")"#,
                ],
            ),
        ];
        for (key, _) in WEB_KEYS {
            let (_, needles) = handlers
                .iter()
                .find(|(k, _)| k == key)
                .unwrap_or_else(|| panic!("web key {key} has no handler check"));
            for n in *needles {
                assert!(
                    app.contains(n),
                    "the web UI no longer handles {key}: missing `{n}`"
                );
            }
        }
        assert_eq!(
            handlers.len(),
            WEB_KEYS.len(),
            "a handler check for a key not in WEB_KEYS"
        );
    }

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
