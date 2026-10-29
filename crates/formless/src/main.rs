mod table;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use formless_core::daemon::{daemon_reachable, Daemon};
use formless_core::provider::probe_blocking;
use formless_core::{Config, Paths};
use formless_protocol::{
    ApprovalScope, Event, Question, QuestionAnswer, Request, SendMode, ServerMessage, SessionInfo,
};
use serde_json::Value;
use std::io::Write;
use std::sync::Arc;
use table::{c, t, Table, Tone};

#[derive(Parser)]
#[command(
    name = "formless",
    version,
    about = "General-purpose agent harness (SPEC.md)"
)]
struct Cli {
    /// Isolated profile (separate daemon + data dir)
    #[arg(long, global = true)]
    profile: Option<String>,
    /// Plain, line-oriented mode for screen readers (no redraws,
    /// spinners or borders); also `tui.screen_reader = true`
    #[arg(long)]
    plain: bool,
    /// Ask for approval before sensitive Tool calls (default: run them
    /// without asking); also `security.secure = true`
    #[arg(long, global = true)]
    secure: bool,
    /// Open the most recently active Session
    #[arg(short = 'c', long = "continue", conflicts_with = "new")]
    continue_last: bool,
    /// Start in a fresh Session
    #[arg(long)]
    new: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Launch the TUI (default)
    Tui,
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Line-mode chat: no full screen, for dumb terminals, SSH and screen
    /// readers (approvals and questions are answered inline)
    Chat {
        /// Open this Session (default: the most recently used one)
        #[arg(long, conflicts_with = "new")]
        session: Option<String>,
        /// Start a new Session
        #[arg(long)]
        new: bool,
    },
    /// Every approval grant, and revoking them
    Approvals {
        #[command(subcommand)]
        action: ApprovalsAction,
    },
    /// What the daemon can reach: grants, denials, trusted folders, MCP
    /// servers, trusted hooks, relay and web UI
    Security {
        #[command(subcommand)]
        action: SecurityAction,
    },
    /// List or remove profiles (isolated data dirs made with --profile)
    Profiles {
        #[command(subcommand)]
        action: ProfilesAction,
    },
    /// Diagnostics: config, dirs, keychain, endpoint probe
    Doctor {
        /// Apply the safe fixes: clear a stale socket or lock, restart a
        /// stuck daemon, tighten secret file permissions, then check again
        #[arg(long)]
        fix: bool,
        /// A machine-readable report, secrets redacted
        #[arg(long, conflicts_with = "fix")]
        json: bool,
    },
    /// Write a redacted bug report (doctor, versions, OS, recent log) to attach to an issue
    BugReport {
        /// Where to write it (default: ./formless-bug-report-<time>.md)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
    /// Skills: SKILL.md know-how the agent loads on demand (SPEC §7.10)
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Open the web UI from this machine's daemon on 127.0.0.1 (SPEC §12)
    Web {
        /// Port (default: a free one)
        #[arg(long)]
        port: Option<u16>,
        /// Print the one-time link instead of opening a browser
        #[arg(long)]
        no_open: bool,
        /// Stop serving the web UI
        #[arg(long)]
        stop: bool,
    },
    /// Extra model providers (SPEC §4.5)
    Providers {
        #[command(subcommand)]
        action: ProvidersAction,
    },
    /// Sessions: list, show, search, rename, delete, export or import them
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
    /// Print the man page (roff), for packaging: `formless man > formless.1`
    #[command(hide = true)]
    Man,
    /// Show the daemon log (daemon.log and its rotated files)
    Logs {
        /// Keep printing new lines as they're written
        #[arg(short, long)]
        follow: bool,
        /// Only this level and worse: error, warn, info, debug, trace
        #[arg(long, value_parser = ["error", "warn", "info", "debug", "trace"])]
        level: Option<String>,
        /// Only entries from this long ago on (10m, 2h, 1d)
        #[arg(long, value_parser = parse_duration)]
        since: Option<std::time::Duration>,
        /// How many of the latest entries to show (default 200; 0 = all)
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: usize,
    },
    /// Print a shell completion script (bash, zsh, fish, powershell, elvish)
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Settings: get, set (validated by the daemon), edit or locate config.toml
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Roles: a purpose mapped to a provider + model (SPEC §4.5)
    Roles {
        #[command(subcommand)]
        action: RolesAction,
    },
    /// Scheduled jobs: prompts that run later or on a cron (SPEC §9.7)
    Jobs {
        #[command(subcommand)]
        action: JobsAction,
    },
    /// Recent Runs and their outcomes: steps, tool calls, tokens (SPEC §8.7)
    Runs {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// User memory: notes the agent keeps about your preferences (SPEC §7.11)
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// External MCP servers (SPEC §7.9)
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Provider credential login/status/logout (API key or OAuth)
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Hosted relay + web frontend (SPEC §12, v2)
    Relay {
        #[command(subcommand)]
        action: RelayAction,
    },
    /// Prompt/memory self-optimisation proposals
    Prompts {
        #[command(subcommand)]
        action: PromptsAction,
    },
    /// Check for a newer release, show what changed, and how to upgrade
    Update {
        #[command(subcommand)]
        action: Option<UpdateAction>,
        /// Only report (same as `update check`)
        #[arg(long)]
        check: bool,
    },
    /// Tool registry inspection/transfer (SPEC §7.8, §12)
    Tools {
        #[command(subcommand)]
        action: ToolsAction,
    },
    /// Embedded session-naming model: status, install, try it out
    Naming {
        #[command(subcommand)]
        action: NamingAction,
    },
    /// Disk used by Sessions, artifacts, Tool versions, logs, backups
    Storage {
        #[arg(long)]
        json: bool,
    },
    /// Free disk space: old artifacts and snapshots, long MCP logs, old
    /// migration copies, database free space
    Gc {
        /// List what would be freed without removing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Back up the database now (kept with the daily ones in <data>/backups)
    Backup,
    /// Everything for a new machine in one file: Tools, modules, Skills,
    /// memories, jobs, providers/Roles/MCP servers (no keys or secrets)
    ExportAll {
        /// Include every Session's transcript
        #[arg(long)]
        sessions: bool,
        /// Where to write it (default: ./formless-export-<date>.json)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
    /// Load a file from `formless export-all` (adds; never overwrites)
    ImportAll { file: std::path::PathBuf },
    /// Replace the database with a backup (stop the daemon first); the
    /// current one is saved next to the backups
    Restore {
        file: std::path::PathBuf,
    },
    /// Wipe config, keychain entry, and the entire data dir (SPEC §15)
    Reset {
        #[arg(long)]
        force: bool,
    },
    /// Headless single Run (SPEC §12). Approvals fail closed unless --yes.
    Run {
        /// Task text; `-` reads it from stdin. With a task, piped stdin is
        /// attached as stdin.txt (`git diff | formless run "review this"`)
        task: String,
        /// Continue an existing session by id, or `last` (default: ephemeral)
        #[arg(long)]
        session: Option<String>,
        /// Continue the Session the last `formless run` used (= --session last)
        #[arg(long = "continue", short = 'c', conflicts_with = "session")]
        continue_last: bool,
        /// Auto-approve sensitive tools for this Run only
        #[arg(long)]
        yes: bool,
        /// Answer for the agent's ask_user questions (default: ask on a
        /// terminal, otherwise tell the agent nobody can answer)
        #[arg(long)]
        answer: Option<String>,
        /// Emit one JSON event per line (same as --output jsonl-events)
        #[arg(long)]
        json: bool,
        /// text: stream the answer and progress; final: only the final
        /// answer on stdout; json: one result object at the end;
        /// jsonl-events: every event, one per line
        #[arg(long, value_enum, default_value = "text")]
        output: RunOutput,
        /// No progress lines ([tool], [approval], …) on stderr; errors still show
        #[arg(long, short)]
        quiet: bool,
        /// Interrupt the Run after this long (90s, 10m, 1h30m) and exit 124
        #[arg(long, value_parser = parse_duration)]
        timeout: Option<std::time::Duration>,
        /// Working directory for the Run's Session (default: the current
        /// directory; with --session, this Run only)
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// Model for this Run (with --session: this Run only)
        #[arg(long, conflicts_with = "role")]
        model: Option<String>,
        /// Reasoning effort for this Run: low, medium or high
        #[arg(long, value_parser = ["low", "medium", "high"])]
        effort: Option<String>,
        /// Run on a configured Role's provider and model
        #[arg(long)]
        role: Option<String>,
        /// Attach a file or image to the task (repeatable; same limits and
        /// image handling as /attach)
        #[arg(long = "attach", value_name = "FILE")]
        attach: Vec<std::path::PathBuf>,
        /// Pause the Run after this many steps (as /limit; overrides
        /// limits.run_steps for this Run)
        #[arg(long, value_name = "N")]
        max_steps: Option<u32>,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    /// Log in with the configured OAuth provider in a browser
    Login {
        /// Print the authorization URL without opening a browser
        #[arg(long)]
        no_browser: bool,
    },
    /// Show credential mode and stored-token status
    Status,
    /// Delete stored OAuth tokens for this profile
    Logout,
}

/// The pairing link as a terminal QR code, for a phone's camera; only on a
/// terminal (it holds the secret, so it never goes into a pipe or a log).
fn print_pairing_qr(url: &str) {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return;
    }
    if let Some(code) = formless_tui::qr::ansi(url) {
        println!();
        print!("{code}");
        println!("            Scan with your phone. This code contains the pairing secret:");
        println!(
            "            anyone who sees it can use your daemon. Rotate with --rotate if it leaks."
        );
        println!();
    }
}

#[derive(Subcommand)]
enum RelayAction {
    /// Enable the relay (generates a pairing secret on first use) and print the browser URL
    Enable {
        /// Relay websocket base URL
        #[arg(long, default_value = "wss://formless.athiwat.xyz")]
        url: String,
        /// Generate a fresh pairing secret (invalidates old browser links)
        #[arg(long)]
        rotate: bool,
    },
    /// Disable the relay (pairing secret is kept unless --forget)
    Disable {
        #[arg(long)]
        forget: bool,
    },
    /// Show relay config and the browser URL
    Status {
        /// Also print the browser URL as a QR code (it contains the secret)
        #[arg(long)]
        qr: bool,
        /// One JSON object (the browser URL holds the secret, so it's left out)
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum UpdateAction {
    /// Compare this build with the latest GitHub release
    Check,
}

#[derive(Subcommand)]
enum PromptsAction {
    /// List proposals with eval scores
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Generate and measure a new proposal
    Generate,
    /// Apply a measured proposal
    Apply { id: String },
    /// Reject a proposal
    Reject { id: String },
    /// Roll back an applied proposal
    Rollback { id: String },
}

#[derive(Subcommand)]
enum ToolsAction {
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
        /// Only Tools with this tag
        #[arg(long)]
        tag: Option<String>,
    },
    /// Switch a Tool off: hidden from the agent and uncallable, nothing else changes
    Disable { name: String },
    /// Switch a Tool back on
    Enable { name: String },
    /// Revert the newest registry change (create, update, retire, restore
    /// or rollback, by anyone)
    Undo,
    /// Always list a Tool to the agent (from each Session's next listing)
    Pin { name: String },
    /// Stop always listing a Tool
    Unpin { name: String },
    /// Set a Tool's tags (none clears them): `tools tag fetch_page web http`
    Tag {
        /// Tool name or id
        name: String,
        tags: Vec<String>,
    },
    /// A Tool's description, schemas, Capabilities, versions and stats
    Show {
        /// Tool name or id
        name: String,
        /// The full detail as JSON
        #[arg(long)]
        json: bool,
    },
    /// Retire a Tool: hidden from the agent, kept with its history
    Retire { name: String },
    /// Bring a retired Tool back
    Restore { name: String },
    /// Make an earlier version current again (default: the one before)
    Rollback {
        name: String,
        #[arg(long)]
        to: Option<i64>,
    },
    /// Delete a Tool and all its versions and history for good
    Purge {
        name: String,
        /// Required without a terminal to confirm on
        #[arg(long)]
        force: bool,
    },
    /// Run a Tool directly; sensitive Capabilities are denied unless --yes
    Run {
        /// Tool name or id
        name: String,
        /// The input, a JSON object
        #[arg(long, default_value = "{}")]
        input: String,
        /// Approve this run's Capabilities once (nothing is remembered)
        #[arg(long)]
        yes: bool,
        /// Run as part of this Session (default: a new one, archived after)
        #[arg(long)]
        session: Option<String>,
        /// No real side effects: writes stay in memory, processes and
        /// non-GET requests aren't made; prints what it would have done
        #[arg(long)]
        dry_run: bool,
    },
    Export {
        /// Tool name or id (omit with --all)
        name: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Import a bundle from a file, or from an https URL or gist (shown
    /// in full and confirmed first)
    Import {
        /// A bundle file, or an https:// URL (gists and GitHub file pages work)
        file: String,
        /// Import from a URL without asking (non-interactive use)
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum SkillsAction {
    /// Install from a folder, a git URL or a .zip (installed disabled)
    Add {
        source: String,
        /// Folder inside the source that holds SKILL.md
        #[arg(long)]
        subdir: Option<String>,
        /// git branch or tag
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Enable right away instead of reviewing first
        #[arg(long)]
        enable: bool,
    },
    Remove {
        name: String,
    },
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    /// Print a skill's SKILL.md and files, to review before enabling
    Show {
        name: String,
    },
    /// Write a skill (or --all) as a bundle file (no metrics)
    Export {
        name: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Install a bundle; imported skills start disabled for review
    Import {
        file: String,
    },
}

#[derive(Subcommand)]
enum ProvidersAction {
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Add or update a provider; the API key is read from stdin (not argv)
    Add {
        name: String,
        #[arg(long, required_unless_present = "preset")]
        base_url: Option<String>,
        /// A known endpoint instead of --base-url: openai, anthropic,
        /// openrouter, gemini, groq, ollama, lmstudio
        #[arg(long, conflicts_with = "base_url")]
        preset: Option<String>,
        /// Its default model
        #[arg(long)]
        model: Option<String>,
        /// Skip asking for an API key (local servers)
        #[arg(long)]
        no_key: bool,
    },
    Remove {
        name: String,
    },
    /// Fetch the provider's /models list
    Test {
        name: String,
    },
}

#[derive(Subcommand)]
enum RolesAction {
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Create or replace a role
    Add {
        name: String,
        /// When to use it, for the agent ("fast; broad read-only search")
        #[arg(long)]
        description: String,
        /// Provider name (default: [provider])
        #[arg(long)]
        provider: Option<String>,
        #[arg(long, default_value = "")]
        model: String,
        #[arg(long)]
        effort: Option<String>,
        /// Extra system prompt for sub-agents in this role
        #[arg(long)]
        system: Option<String>,
    },
    Rm {
        name: String,
    },
}

#[derive(Subcommand)]
enum JobsAction {
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Schedule a prompt: --every "weekday at 9am" or --cron "0 3 * * *"
    /// (recurring), or --at "in 30m"
    Add {
        prompt: String,
        /// Cron expression, or plain English ("every 2 hours")
        #[arg(long, visible_alias = "every")]
        cron: Option<String>,
        #[arg(long)]
        at: Option<String>,
        /// Run in this Session (default: a new Session each time)
        #[arg(long)]
        session: Option<String>,
        /// When the previous firing is still running: skip (default) or queue
        #[arg(long)]
        overlap: Option<String>,
        /// Deliver the answer as a desktop notification
        #[arg(long)]
        notify: bool,
        /// Append the answer to this file
        #[arg(long)]
        to_file: Option<String>,
        /// POST the answer (JSON) to this URL
        #[arg(long)]
        webhook: Option<String>,
    },
    /// Fire a job now (its schedule is unchanged)
    Run {
        id: String,
    },
    /// A job's recent firings: time, trigger, outcome, cost, Session
    History {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Enable (and approve) a job
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Rm {
        id: String,
    },
}

#[derive(Subcommand)]
enum SecurityAction {
    /// One-screen audit of the attack surface
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum MemoryAction {
    /// List notes with id, scope and source
    List {
        /// Print JSON (for export)
        #[arg(long)]
        json: bool,
    },
    /// Add a note
    Add {
        text: String,
        /// "global" or "project:/abs/path"
        #[arg(long, default_value = "global")]
        scope: String,
    },
    /// Delete a note
    Rm { id: String },
    /// Change a note's text or scope, or disable/enable it
    Edit {
        id: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
        #[arg(long)]
        enable: bool,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// Add a server: `add NAME -- COMMAND [ARGS]` (stdio), `add NAME --url
    /// URL` (http), or `add ID` for a catalog server (`mcp catalog`)
    Add {
        name: String,
        /// A catalog server's setting, `key=value` (e.g. `path=~/notes`)
        #[arg(long = "param")]
        params: Vec<String>,
        /// Remote server URL (Streamable HTTP or HTTP+SSE)
        #[arg(long)]
        url: Option<String>,
        /// Static HTTP header, `Key=Value` (repeatable)
        #[arg(long = "header")]
        headers: Vec<String>,
        /// Daemon environment variable to pass to a stdio server (repeatable)
        #[arg(long = "env-key")]
        env_keys: Vec<String>,
        /// stdio: the command and its arguments, after `--`
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Remove a server (its tools are retired, metrics kept)
    Remove { name: String },
    /// Configured servers: state, tool count, last error
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Restart a server and list its tools again
    Test { name: String },
    /// Authorize Formless with a remote (http) MCP server via OAuth
    Login { server: String },
    /// Delete a server's stored OAuth tokens
    Logout { server: String },
    /// Popular servers you can add by name (`formless mcp add ID`)
    Catalog,
    /// Find servers configured in Claude Desktop, Cursor or VS Code and
    /// add them (each command is shown and asked about first)
    Import {
        /// Add every server found without asking
        #[arg(long)]
        yes: bool,
    },
    /// The end of a stdio server's stderr log
    Log {
        name: String,
        #[arg(long, default_value_t = 50)]
        lines: usize,
    },
    /// A server's tools, on or off, with their token cost
    Tools { name: String },
    /// Turn one of a server's tools on or off
    Tool {
        name: String,
        tool: String,
        #[arg(long, conflicts_with = "off")]
        on: bool,
        #[arg(long)]
        off: bool,
    },
}

#[derive(Subcommand)]
enum NamingAction {
    /// Show the configured model and whether it is installed
    Status,
    /// Download the model (default: the configured one)
    Pull {
        /// Model id: qwen2.5-0.5b or lfm2.5-350m
        model: Option<String>,
    },
    /// Print the title the namer would give a first message
    Test { text: String },
}

#[derive(Subcommand)]
enum ApprovalsAction {
    /// Every grant: Tool, Capability, scope, when, by which client, last used
    List {
        /// Only this Capability family (fs, net, process, env, …)
        #[arg(long)]
        family: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Revoke a Tool's grant (one Capability, or all of it), or --all
    Revoke {
        /// Tool name (omit with --all)
        tool: Option<String>,
        /// One Capability, e.g. net:example.com (default: the whole grant)
        capability: Option<String>,
        /// Which grant: always (stored), session or hour
        #[arg(long, default_value = "always")]
        scope: String,
        /// The Session of a session grant
        #[arg(long)]
        session: Option<String>,
        /// Every grant (with --family, every grant of that family)
        #[arg(long)]
        all: bool,
        #[arg(long)]
        family: Option<String>,
    },
    /// Capabilities you always deny; --forget CAP (or --all) to ask again
    Denials {
        #[arg(long)]
        forget: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
}

/// A Capability's family: "fs" for fs:read:/x, "net" for net:host.
fn cap_family(cap: &str) -> &str {
    cap.split(':').next().unwrap_or(cap)
}

fn approvals_cmd(paths: &Paths, action: ApprovalsAction) -> Result<()> {
    use formless_protocol::{ApprovalScope, GrantInfo};
    use formless_tui::client::request_once;
    formless_core::daemon::ensure_daemon(paths)?;
    let grants = |family: &Option<String>| -> Result<Vec<GrantInfo>> {
        let mut g: Vec<GrantInfo> =
            serde_json::from_value(request_once(paths, Request::ListApprovals)?)?;
        if let Some(f) = family {
            g.retain(|x| cap_family(&x.capability) == f);
        }
        Ok(g)
    };
    let scope_word = |s: ApprovalScope| match s {
        ApprovalScope::Session => "session",
        ApprovalScope::Hour => "hour",
        _ => "always",
    };
    match action {
        ApprovalsAction::Denials { forget, all, json } => {
            if forget.is_some() || all {
                let r = request_once(paths, Request::ForgetDenial { capability: forget })?;
                println!(
                    "forgot {} denial(s); the next use asks again",
                    r["forgotten"]
                );
                return Ok(());
            }
            let list: Vec<formless_protocol::DenialInfo> =
                serde_json::from_value(request_once(paths, Request::ListDenials)?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
                return Ok(());
            }
            if list.is_empty() {
                println!("(nothing always denied)");
                return Ok(());
            }
            let mut tbl = Table::new(&["CAPABILITY", "DENIED ON", "WHEN", "BY"]);
            for d in &list {
                tbl.row(vec![
                    t(d.capability.clone(), Tone::Warn),
                    c(d.tool_name.clone()),
                    c(d.created_at
                        .get(..16)
                        .unwrap_or(&d.created_at)
                        .replace('T', " ")),
                    c(if d.denied_by.is_empty() {
                        "-".to_string()
                    } else {
                        d.denied_by.clone()
                    }),
                ]);
            }
            tbl.print();
            Ok(())
        }
        ApprovalsAction::List { family, json } => {
            let list = grants(&family)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
                return Ok(());
            }
            if list.is_empty() {
                println!("(nothing approved)");
                return Ok(());
            }
            let when = |t: &Option<String>| {
                t.as_deref()
                    .map(|s| s.get(..16).unwrap_or(s).replace('T', " "))
                    .unwrap_or_else(|| "-".into())
            };
            let mut tbl = Table::new(&[
                "TOOL",
                "CAPABILITY",
                "SCOPE",
                "GRANTED",
                "BY",
                "ENDS",
                "LAST USED",
            ]);
            for g in &list {
                tbl.row(vec![
                    t(g.tool_name.clone(), Tone::Strong),
                    c(g.capability.clone()),
                    t(
                        scope_word(g.scope),
                        if g.scope == ApprovalScope::Always {
                            Tone::Plain
                        } else {
                            Tone::Warn
                        },
                    ),
                    c(when(&g.granted_at)),
                    c(if g.granted_by.is_empty() {
                        "-".to_string()
                    } else {
                        g.granted_by.clone()
                    }),
                    c(when(&g.expires_at)),
                    c(when(&g.last_used)),
                ]);
                if g.trusted {
                    tbl.row(vec![
                        c(String::new()),
                        t(
                            "↳ also trusted in config.toml: revoking won't make it ask",
                            Tone::Dim,
                        ),
                        c(String::new()),
                        c(String::new()),
                        c(String::new()),
                        c(String::new()),
                        c(String::new()),
                    ]);
                }
            }
            tbl.print();
            Ok(())
        }
        ApprovalsAction::Revoke {
            tool,
            capability,
            scope,
            session,
            all,
            family,
        } => {
            let scope_of = |s: &str| -> Result<ApprovalScope> {
                Ok(match s {
                    "always" => ApprovalScope::Always,
                    "session" => ApprovalScope::Session,
                    "hour" => ApprovalScope::Hour,
                    other => anyhow::bail!("--scope is always, session or hour, not {other}"),
                })
            };
            let targets: Vec<(String, Option<String>, ApprovalScope, Option<String>)> = if all {
                grants(&family)?
                    .into_iter()
                    .map(|g| {
                        // Without a family, whole grants; with one, each Capability.
                        let cap = family.as_ref().map(|_| g.capability.clone());
                        (g.tool_id, cap, g.scope, g.session_id)
                    })
                    .collect()
            } else {
                let tool = tool.ok_or_else(|| anyhow::anyhow!("name a Tool, or pass --all"))?;
                let id = find_tool(paths, &tool)?.id;
                vec![(id, capability, scope_of(&scope)?, session)]
            };
            let mut seen = std::collections::HashSet::new();
            let mut n = 0;
            for (tool_id, capability, scope, session_id) in targets {
                if !seen.insert((
                    tool_id.clone(),
                    capability.clone(),
                    scope_word(scope),
                    session_id.clone(),
                )) {
                    continue;
                }
                if request_once(
                    paths,
                    Request::RevokeGrant {
                        tool_id,
                        capability,
                        scope,
                        session_id,
                    },
                )
                .is_ok()
                {
                    n += 1;
                }
            }
            if n == 0 {
                anyhow::bail!("nothing to revoke");
            }
            println!("revoked {n} grant(s)");
            Ok(())
        }
    }
}

#[derive(Subcommand)]
enum ProfilesAction {
    /// Every profile, whether its daemon runs, and where its data lives
    List {
        #[arg(long)]
        json: bool,
    },
    /// Delete a profile's data, config and stored secrets
    Rm {
        name: String,
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
    },
}

fn profiles_cmd(paths: &Paths, action: ProfilesAction) -> Result<()> {
    match action {
        ProfilesAction::List { json } => {
            let rows: Vec<Value> = paths
                .list_profiles()
                .iter()
                .map(|p| {
                    let running = daemon_reachable(p);
                    let last_used = std::fs::metadata(p.db_file())
                        .and_then(|m| m.modified())
                        .ok()
                        .map(|t| {
                            chrono::DateTime::<chrono::Local>::from(t)
                                .format("%Y-%m-%d %H:%M")
                                .to_string()
                        });
                    serde_json::json!({
                        "name": p.profile,
                        "current": p.profile == paths.profile,
                        "daemon_running": running,
                        "last_used": last_used,
                        "data_dir": p.data_dir,
                        "config_dir": p.config_dir,
                    })
                })
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }
            let mut tbl = Table::new(&["PROFILE", "DAEMON", "LAST USED", "DATA DIR"]);
            for r in &rows {
                let mut name = r["name"].as_str().unwrap_or_default().to_string();
                if r["current"] == true {
                    name.push_str(" *");
                }
                tbl.row(vec![
                    c(name),
                    if r["daemon_running"] == true {
                        t("running", Tone::Good)
                    } else {
                        t("stopped", Tone::Dim)
                    },
                    c(r["last_used"].as_str().unwrap_or("never")),
                    c(r["data_dir"].as_str().unwrap_or_default()),
                ]);
            }
            tbl.print();
            Ok(())
        }
        ProfilesAction::Rm { name, yes } => {
            if name == "default" {
                anyhow::bail!("the default profile can't be removed; `formless reset` wipes it");
            }
            let Some(target) = paths
                .list_profiles()
                .into_iter()
                .find(|p| p.profile == name)
            else {
                anyhow::bail!("no profile named '{name}' (see `formless profiles list`)");
            };
            if !yes {
                eprint!(
                    "This deletes profile '{name}': its config, API key, Tools and Sessions in {}. Type the profile name to confirm: ",
                    target.data_dir.display()
                );
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if line.trim() != name {
                    eprintln!("aborted");
                    std::process::exit(2);
                }
            }
            if daemon_reachable(&target) {
                let _ = formless_tui::client::request_once(&target, Request::Shutdown);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while daemon_reachable(&target) || target.lock_file().exists() {
                    if std::time::Instant::now() > deadline {
                        anyhow::bail!(
                            "profile '{name}''s daemon did not stop; nothing was deleted"
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            formless_core::config::reset_all(&target)?;
            println!("removed profile '{name}'");
            Ok(())
        }
    }
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Run the daemon (used by client auto-start; blocks)
    Serve {
        /// Never stop for being idle (how the login service runs it)
        #[arg(long)]
        stay: bool,
    },
    /// Start the daemon at login (launchd, systemd user unit or a Windows
    /// logon task), so scheduled jobs run on time
    InstallService {
        /// Print the service file and commands without changing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the login service
    UninstallService,
    /// Start the daemon if not running
    Start,
    Stop,
    /// Stop the daemon once its Runs finish, then start a fresh one
    Restart {
        /// Interrupt running Runs instead of waiting for them
        #[arg(long)]
        force: bool,
    },
    Status {
        /// JSON even when the daemon isn't running ({"running": false})
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // The daemon logs warnings and info to daemon.log (`formless logs`);
    // other commands only errors. RUST_LOG overrides both. Colour codes
    // only on a terminal, never into the log file.
    let serving = matches!(
        cli.cmd,
        Some(Cmd::Daemon {
            action: DaemonAction::Serve { .. }
        })
    );
    {
        use std::io::IsTerminal;
        let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new(if serving { "info" } else { "error" })
        });
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(std::io::stderr().is_terminal())
            .init();
    }
    // Completions and the man page need nothing else: no profile, config
    // or daemon.
    if let Some(Cmd::Completions { shell }) = cli.cmd {
        use clap::CommandFactory;
        clap_complete::generate(
            shell,
            &mut Cli::command(),
            "formless",
            &mut std::io::stdout(),
        );
        return Ok(());
    }
    if let Some(Cmd::Man) = cli.cmd {
        use clap::CommandFactory;
        clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
        return Ok(());
    }
    if cli.secure {
        // A daemon started from here inherits it (see `Config::apply_env`).
        std::env::set_var("FORMLESS_SECURE", "1");
    }
    let paths = Paths::resolve(cli.profile.as_deref())?;
    match cli.cmd {
        None | Some(Cmd::Tui) if cli.plain => formless_tui::plain::run(paths),
        None | Some(Cmd::Tui) => formless_tui::run_with(
            paths,
            match (cli.continue_last, cli.new) {
                (true, _) => Some("last"),
                (_, true) => Some("new"),
                _ => None,
            },
        ),
        Some(Cmd::Daemon { action }) => daemon_cmd(paths, action),
        Some(Cmd::Profiles { action }) => profiles_cmd(&paths, action),
        Some(Cmd::Approvals { action }) => approvals_cmd(&paths, action),
        Some(Cmd::Chat { session, new }) => {
            use formless_tui::plain::{run_with, ChatStart};
            let start = match (session, new) {
                (Some(id), _) => ChatStart::Session(id),
                (None, true) => ChatStart::New,
                (None, false) => ChatStart::Recent,
            };
            run_with(paths, start, "chat")
        }
        Some(Cmd::Doctor { json: true, .. }) => {
            println!("{}", serde_json::to_string_pretty(&doctor_json(&paths)?)?);
            Ok(())
        }
        Some(Cmd::Doctor { fix, .. }) => doctor(paths, fix),
        Some(Cmd::BugReport { output }) => bug_report(&paths, output),
        Some(Cmd::Mcp { action }) => mcp_cmd(paths, action),
        Some(Cmd::Skills { action }) => skills_cmd(paths, action),
        Some(Cmd::Memory { action }) => memory_cmd(paths, action),
        Some(Cmd::Runs { limit, json }) => runs_cmd(paths, limit, json),
        Some(Cmd::Jobs { action }) => jobs_cmd(paths, action),
        Some(Cmd::Security {
            action: SecurityAction::Status { json },
        }) => {
            formless_core::daemon::ensure_daemon(&paths)?;
            let v = formless_tui::client::request_once(&paths, Request::SecurityStatus)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{}", formless_protocol::security_report(&v));
            }
            Ok(())
        }
        Some(Cmd::Providers { action }) => providers_cmd(paths, action),
        Some(Cmd::Sessions { action }) => sessions_cmd(paths, action),
        Some(Cmd::Config { action }) => config_cmd(paths, action),
        Some(Cmd::Logs {
            follow,
            level,
            since,
            lines,
        }) => logs_cmd(&paths, follow, level, since, lines),
        Some(Cmd::Completions { .. } | Cmd::Man) => {
            unreachable!("handled before the profile is set up")
        }
        Some(Cmd::Roles { action }) => roles_cmd(paths, action),
        Some(Cmd::Web {
            port,
            no_open,
            stop,
        }) => web_cmd(paths, port, no_open, stop),
        Some(Cmd::Auth { action }) => auth_cmd(paths, action),
        Some(Cmd::Relay { action }) => relay_cmd(paths, action),
        Some(Cmd::Prompts { action }) => prompts_cmd(paths, action),
        Some(Cmd::Update { action, check }) => {
            update_cmd(check || matches!(action, Some(UpdateAction::Check)))
        }
        Some(Cmd::Tools { action }) => tools_cmd(paths, action),
        Some(Cmd::Naming { action }) => naming_cmd(paths, action),
        Some(Cmd::Reset { force }) => reset_cmd(paths, force),
        Some(Cmd::Storage { json }) => storage_cmd(&paths, json),
        Some(Cmd::Gc { dry_run }) => gc_cmd(&paths, dry_run),
        Some(Cmd::Backup) => backup_cmd(&paths),
        Some(Cmd::ExportAll { sessions, output }) => {
            formless_core::daemon::ensure_daemon(&paths)?;
            let bundle = formless_tui::client::request_once(&paths, Request::ExportAll { sessions })?;
            let file = output.unwrap_or_else(|| {
                std::path::PathBuf::from(format!(
                    "formless-export-{}.json",
                    chrono::Local::now().format("%Y%m%d-%H%M")
                ))
            });
            formless_core::config::write_secret_file(&file, &serde_json::to_string_pretty(&bundle)?)?;
            let n = |k: &str| bundle[k].as_array().map_or(0, Vec::len);
            println!("exported:  {}", file.display());
            println!(
                "contains:  {} Tools, {} modules, {} Skills, {} memories, {} jobs{}",
                n("tools"), n("modules"), n("skills"), n("memories"), n("jobs"),
                if sessions { format!(", {} Sessions", n("sessions")) } else { String::new() }
            );
            if n("blanked") > 0 {
                println!("left out:  {} secret value(s) (API keys never leave the keychain); fill them in after import", n("blanked"));
            }
            Ok(())
        }
        Some(Cmd::ImportAll { file }) => {
            let text = std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
            let bundle: serde_json::Value = serde_json::from_str(&text).context("not a JSON export")?;
            formless_core::daemon::ensure_daemon(&paths)?;
            let r = formless_tui::client::request_once(&paths, Request::ImportAll { bundle })?;
            println!(
                "imported:  {} Tools, {} modules, {} Skills (disabled until you enable them), {} memories, {} jobs (paused), {} Sessions",
                r["tools"], r["modules"], r["skills"], r["memories"], r["jobs"], r["sessions"]
            );
            println!(
                "config:    {} providers, {} Roles, {} MCP servers added",
                r["providers"], r["roles"], r["mcp_servers"]
            );
            for b in r["blanked"].as_array().into_iter().flatten() {
                println!("fill in:   {}", b.as_str().unwrap_or_default());
            }
            for f in r["failed"].as_array().into_iter().flatten() {
                println!("failed:    {}", f.as_str().unwrap_or_default());
            }
            println!("next:      formless auth login / set API keys, then `formless jobs` to enable jobs");
            Ok(())
        }
        Some(Cmd::Restore { file }) => {
            if daemon_reachable(&paths) {
                anyhow::bail!("stop the daemon first: formless daemon stop");
            }
            let saved = formless_core::housekeeping::restore(&paths, &file)?;
            println!("restored:  {}", file.display());
            println!("previous:  saved as {}", saved.display());
            Ok(())
        }
        Some(Cmd::Run {
            task,
            session,
            continue_last,
            yes,
            answer,
            json,
            output,
            quiet,
            timeout,
            cwd,
            model,
            effort,
            role,
            attach,
            max_steps,
        }) => {
            let output = if json { RunOutput::JsonlEvents } else { output };
            let session = if continue_last || session.as_deref() == Some("last") {
                match last_run_session(&paths) {
                    Ok(id) => Some(id),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(2);
                    }
                }
            } else {
                session
            };
            // Piped input: the task itself (`-`) or an attachment.
            use std::io::{IsTerminal, Read};
            let piped = if std::io::stdin().is_terminal() {
                None
            } else {
                let mut buf = Vec::new();
                std::io::stdin().read_to_end(&mut buf)?;
                Some(buf)
            };
            let (task, attachment) = match run_input(task, piped) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            // `--attach` files, read and checked before anything starts.
            let mut files = match read_attachments(&attach) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            if let Some(bytes) = attachment {
                files.push(("stdin.txt".into(), bytes));
            }
            let pick = match run_model(&paths, model, effort, role) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(2);
                }
            };
            // An existing Session gets its own model back after this Run.
            // The folder the Run acts in: --cwd, else where it was started
            // (a continued Session keeps its own unless --cwd is given).
            let dir = match &cwd {
                Some(d) => match d.canonicalize() {
                    Ok(p) if p.is_dir() => Some(p),
                    _ => {
                        eprintln!("--cwd {}: no such directory", d.display());
                        std::process::exit(2);
                    }
                },
                None if session.is_none() => std::env::current_dir().ok(),
                None => None,
            };
            let restore = match (&session, pick.is_some() || cwd.is_some()) {
                (Some(id), true) => {
                    formless_core::daemon::ensure_daemon(&paths)?;
                    let list: Vec<SessionInfo> = serde_json::from_value(
                        formless_tui::client::request_once(&paths, Request::ListSessions)?,
                    )?;
                    list.into_iter().find(|s| &s.id == id)
                }
                _ => None,
            };
            // `--effort` alone keeps the Session's model.
            let pick = pick.map(|(model, effort)| {
                (
                    model.or_else(|| restore.as_ref().and_then(|s| s.model.clone())),
                    effort,
                )
            });
            let pick_was_set = pick.is_some();
            let code = run_headless(
                paths.clone(),
                task,
                files,
                session,
                pick,
                dir,
                yes,
                answer,
                output,
                quiet,
                timeout,
                max_steps,
            );
            if let Some(s) = restore {
                if pick_was_set {
                    let _ = formless_tui::client::request_once(
                        &paths,
                        Request::SetSessionModel {
                            id: s.id.clone(),
                            model: s.model,
                            reasoning_effort: s.reasoning_effort,
                        },
                    );
                }
                if cwd.is_some() {
                    let _ = formless_tui::client::request_once(
                        &paths,
                        Request::SetSessionCwd {
                            id: s.id,
                            cwd: s.cwd,
                        },
                    );
                }
            }
            let code = code?;
            std::process::exit(code);
        }
    }
}

fn http_get(url: &str) -> Result<String> {
    let resp = reqwest::blocking::Client::builder()
        .user_agent(concat!("formless/", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(url)
        .send()?
        .error_for_status()?;
    Ok(resp.text()?)
}

pub fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let core = tag
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()?;
    let mut parts = core.split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next().unwrap_or("0").parse().ok()?,
        parts.next().unwrap_or("0").parse().ok()?,
    ))
}

pub fn compare_versions(current: &str, latest_tag: &str) -> Option<std::cmp::Ordering> {
    Some(parse_version(current)?.cmp(&parse_version(latest_tag)?))
}

/// Published releases newer than `current`, oldest first, as (tag, notes,
/// url). Drafts and pre-releases are left out.
fn newer_releases(releases: &Value, current: &str) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = releases
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["draft"] != true && r["prerelease"] != true)
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            (compare_versions(current, tag)? == std::cmp::Ordering::Less).then(|| {
                (
                    tag.to_string(),
                    r["body"].as_str().unwrap_or_default().trim().to_string(),
                    r["html_url"].as_str().unwrap_or_default().to_string(),
                )
            })
        })
        .collect();
    out.sort_by_key(|(tag, _, _)| parse_version(tag));
    out
}

/// How this binary was installed, as the command that upgrades it.
fn upgrade_command() -> &'static str {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    if exe.contains("/Cellar/") || exe.contains("/homebrew/") || exe.contains("/linuxbrew/") {
        "brew upgrade formless"
    } else if exe.contains("/.cargo/bin/") {
        "cargo install --git https://github.com/athiwatc/formless --path crates/formless --locked --force"
    } else {
        "curl -fsSL https://formless.athiwat.xyz/install.sh | sh"
    }
}

/// `formless update`: what's newer than this build and what changed in
/// each release, then how to upgrade (`--check` stops after the report).
fn update_cmd(check: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let body = match http_get("https://api.github.com/repos/athiwatc/formless/releases?per_page=50")
    {
        Ok(b) => b,
        Err(e) if e.to_string().contains("404") => {
            println!("installed: v{current}");
            println!("latest:    (no published releases yet)");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let releases: Value = serde_json::from_str(&body).context("parse releases")?;
    let newer = newer_releases(&releases, current);
    println!("installed: v{current}");
    let Some((latest, _, url)) = newer.last() else {
        println!("up to date");
        return Ok(());
    };
    println!("latest:    {latest}  ({url})");
    println!();
    for (tag, notes, _) in &newer {
        println!("── {tag} ──");
        println!(
            "{}",
            if notes.is_empty() {
                "(no release notes)"
            } else {
                notes
            }
        );
        println!();
    }
    if check {
        println!(
            "{} newer release(s); `formless update` shows how to upgrade.",
            newer.len()
        );
    } else {
        println!("To upgrade to {latest}:\n  {}", upgrade_command());
    }
    Ok(())
}

fn prompts_cmd(paths: Paths, action: PromptsAction) -> Result<()> {
    use formless_tui::client::request_once;
    formless_core::daemon::ensure_daemon(&paths)?;
    match action {
        PromptsAction::List { json } => {
            let v = request_once(&paths, Request::ListProposals)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            }
            let arr = v.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                println!("no proposals yet (formless prompts generate)");
            }
            let pct = |v: &serde_json::Value| {
                v.as_f64()
                    .map(|x| format!("{:.0}%", x * 100.0))
                    .unwrap_or_else(|| "-".into())
            };
            let mut tbl = Table::new(&["ID", "KIND", "STATUS", "CAND", "BASE", "CONTENT"]);
            for p in arr {
                let status = p["status"].as_str().unwrap_or_default();
                tbl.row(vec![
                    c(p["id"].as_str().unwrap_or_default()),
                    c(p["kind"].as_str().unwrap_or_default()),
                    t(
                        status,
                        match status {
                            "applied" => Tone::Good,
                            "rejected" | "rolled_back" => Tone::Dim,
                            _ => Tone::Warn,
                        },
                    ),
                    c(pct(&p["candidate_score"])),
                    c(pct(&p["baseline_score"])),
                    t(p["content"].as_str().unwrap_or_default(), Tone::Dim),
                ]);
            }
            if !tbl.is_empty() {
                tbl.print();
            }
            Ok(())
        }
        PromptsAction::Generate => {
            request_once(&paths, Request::GenerateProposal)?;
            println!("generating and measuring a proposal in the background; check `formless prompts list` shortly");
            Ok(())
        }
        PromptsAction::Apply { id } => {
            let v = request_once(&paths, Request::ProposalDecision { id, approved: true })?;
            println!("applied: {}", v["content"].as_str().unwrap_or_default());
            Ok(())
        }
        PromptsAction::Reject { id } => {
            request_once(
                &paths,
                Request::ProposalDecision {
                    id,
                    approved: false,
                },
            )?;
            println!("rejected");
            Ok(())
        }
        PromptsAction::Rollback { id } => {
            request_once(&paths, Request::RollbackProposal { id })?;
            println!("rolled back");
            Ok(())
        }
    }
}

fn relay_cmd(paths: Paths, action: RelayAction) -> Result<()> {
    use formless_core::config::{
        delete_relay_secret, ensure_relay_secret, generate_relay_secret, load_relay_secret,
        save_relay_secret,
    };
    let mut cfg = Config::load(&paths)?;
    match action {
        RelayAction::Enable { url, rotate } => {
            let secret = if rotate {
                let s = generate_relay_secret();
                save_relay_secret(&paths, &s)?;
                s
            } else {
                ensure_relay_secret(&paths)?
            };
            cfg.relay.enabled = true;
            cfg.relay.url = url.trim_end_matches('/').to_string();
            cfg.save(&paths)?;
            println!("relay:      enabled ({})", cfg.relay.url);
            println!(
                "browser:    {}",
                formless_core::relay::browser_url(&cfg.relay, &secret)
            );
            println!("            (the #k= secret never leaves your machines; the relay only forwards ciphertext)");
            print_pairing_qr(&formless_core::relay::browser_url(&cfg.relay, &secret));
            if daemon_reachable(&paths) {
                println!("daemon:     running — restart it to connect: formless daemon stop, then any formless command");
            } else {
                println!("daemon:     not running — it will connect on next start (any formless command)");
            }
            Ok(())
        }
        RelayAction::Disable { forget } => {
            cfg.relay.enabled = false;
            cfg.save(&paths)?;
            if forget {
                delete_relay_secret(&paths);
            }
            println!(
                "relay:      disabled{}",
                if forget {
                    " (pairing secret deleted)"
                } else {
                    ""
                }
            );
            Ok(())
        }
        RelayAction::Status { qr, json } => {
            if json {
                let running = daemon_reachable(&paths);
                let devices = if running {
                    formless_tui::client::request_once(&paths, Request::ListRelayClients)
                        .unwrap_or_default()
                } else {
                    serde_json::json!([])
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "enabled": cfg.relay.enabled,
                        "url": cfg.relay.url,
                        "paired": load_relay_secret(&paths).is_some(),
                        "daemon_running": running,
                        "devices": devices,
                    }))?
                );
                return Ok(());
            }
            println!("enabled:    {}", cfg.relay.enabled);
            println!("url:        {}", cfg.relay.url);
            match load_relay_secret(&paths) {
                Some(secret) => {
                    let url = formless_core::relay::browser_url(&cfg.relay, &secret);
                    println!("browser:    {url}");
                    if qr {
                        print_pairing_qr(&url);
                    }
                }
                None => println!("secret:     none yet (run: formless relay enable)"),
            }
            let running = daemon_reachable(&paths);
            println!(
                "daemon:     {}",
                if running { "running" } else { "not running" }
            );
            // The browsers on the relay now (disconnect one from the web
            // Settings; `enable --rotate` revokes them all).
            if running {
                let list: Vec<formless_protocol::RelayClientInfo> =
                    formless_tui::client::request_once(&paths, Request::ListRelayClients)
                        .ok()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
                if list.is_empty() {
                    println!("devices:    none connected");
                }
                for (i, c) in list.iter().enumerate() {
                    println!(
                        "{}{}  (since {}, last seen {})",
                        if i == 0 {
                            "devices:    "
                        } else {
                            "            "
                        },
                        if c.user_agent.is_empty() {
                            "unknown browser"
                        } else {
                            &c.user_agent
                        },
                        c.first_seen.get(..16).unwrap_or(&c.first_seen),
                        c.last_seen.get(..16).unwrap_or(&c.last_seen)
                    );
                }
            }
            Ok(())
        }
    }
}

fn daemon_cmd(paths: Paths, action: DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Serve { stay } => {
            if stay {
                std::env::set_var(formless_core::daemon::STAY_ENV, "1");
            }
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let d = Arc::new(Daemon::new(paths)?);
                d.serve().await
            })
        }
        DaemonAction::InstallService { dry_run } => {
            let plan = formless_core::service::plan_here(&paths)?;
            if dry_run {
                println!("# {} ({})\n{}", plan.file.display(), plan.manager, plan.contents);
                for c in &plan.enable {
                    println!("$ {}", c.join(" "));
                }
                return Ok(());
            }
            formless_core::service::install(&plan)?;
            println!("installed: {} ({})", plan.file.display(), plan.manager);
            println!("the daemon now starts at login and stays up, so jobs run on schedule");
            println!("remove it: formless daemon uninstall-service");
            Ok(())
        }
        DaemonAction::UninstallService => {
            let plan = formless_core::service::plan_here(&paths)?;
            formless_core::service::uninstall(&plan)?;
            println!("removed: {}", plan.file.display());
            Ok(())
        }
        DaemonAction::Start => {
            formless_core::daemon::ensure_daemon(&paths)?;
            println!("daemon running");
            Ok(())
        }
        DaemonAction::Stop => {
            if !daemon_reachable(&paths) {
                println!("daemon not running");
                return Ok(());
            }
            let _ = formless_tui::client::request_once(&paths, Request::Shutdown);
            println!("daemon stopping");
            Ok(())
        }
        DaemonAction::Restart { force } => {
            formless_tui::client::restart_daemon(&paths, force, |line| println!("{line}"))
        }
        DaemonAction::Status { json } => {
            if daemon_reachable(&paths) {
                let v = formless_tui::client::request_once(&paths, Request::Status)?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                } else {
                    print_daemon_status(&v);
                }
                for w in formless_tui::client::status_warnings(&v) {
                    eprintln!("warning: {w}");
                }
            } else if json {
                println!("{}", serde_json::json!({ "running": false }));
            } else {
                println!("daemon not running");
            }
            Ok(())
        }
    }
}

/// `formless daemon status`: who it is and what it is using (SPEC §9.3).
fn print_daemon_status(v: &serde_json::Value) {
    use formless_core::housekeeping::human_bytes;
    let r = &v["resources"];
    let b = |k: &str| human_bytes(r[k].as_u64().unwrap_or(0));
    println!(
        "daemon:      running (pid {}, version {}, protocol {})",
        v["pid"], v["version"].as_str().unwrap_or_default(), v["protocol"]
    );
    if let Some(up) = r["uptime_secs"].as_u64() {
        println!("uptime:      {}", if up >= 3600 { format!("{}h {}m", up / 3600, up % 3600 / 60) } else { format!("{}m {}s", up / 60, up % 60) });
    }
    if r["memory_bytes"].is_u64() {
        println!(
            "memory:      {}  ·  cpu {}% (total {})",
            b("memory_bytes"),
            r["cpu_percent"].as_f64().map_or("?".into(), |c| format!("{c:.1}")),
            r["cpu_time"].as_str().unwrap_or("?")
        );
    }
    println!("database:    {}  ·  artifacts {}", b("db_bytes"), b("artifacts_bytes"));
    let running = v["running_sessions"].as_array().map_or(0, Vec::len);
    println!(
        "work:        {} Session(s) · {running} running Run(s) · {} pending Invocation(s)",
        v["sessions"], r["running_invocations"]
    );
    println!("approvals:   {}", if v["secure"] == true { "on (--secure)" } else { "off" });
}

#[cfg(target_os = "macos")]
fn open_browser(url: &str) -> bool {
    std::process::Command::new("open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(target_os = "windows")]
fn open_browser(url: &str) -> bool {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_browser(url: &str) -> bool {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_browser(_url: &str) -> bool {
    false
}

const OAUTH_CONFIG_EXAMPLE: &str = "\
Configure OAuth in config.toml first, for example:\n\
\n\
[provider.auth]\n\
mode = \"oauth\"\n\
authorize_url = \"https://provider.example/oauth/authorize\"\n\
token_url = \"https://provider.example/oauth/token\"\n\
client_id = \"your-public-client-id\"\n\
scopes = \"openid profile\"\n\
redirect_port = 8977\n\
\n\
Register http://127.0.0.1:8977/callback as the redirect URI \
(or use redirect_port = 0 only if the provider permits any loopback port).";

fn auth_cmd(paths: Paths, action: AuthAction) -> Result<()> {
    let cfg = Config::load(&paths)?;
    match action {
        AuthAction::Login { no_browser } => {
            if !cfg.provider.oauth() {
                return Err(anyhow::anyhow!(
                    "provider.auth.mode is {:?}, not \"oauth\".\n{OAUTH_CONFIG_EXAMPLE}",
                    cfg.provider.auth.mode
                ));
            }
            if let Err(error) = formless_core::auth::validate_oauth_config(&cfg.provider) {
                return Err(error.context(OAUTH_CONFIG_EXAMPLE.to_string()));
            }
            let tokens = formless_core::auth::login_with_callback(&paths, &cfg.provider, |url| {
                println!("Open this URL to authorize Formless:");
                println!("{url}");
                if no_browser {
                    println!("Waiting for the browser callback...");
                } else if open_browser(url) {
                    println!("Browser opened; waiting for the callback...");
                } else {
                    println!("Could not open a browser automatically; open the URL manually.");
                }
            })?;
            println!("OAuth login complete.");
            println!(
                "refresh token: {}",
                if tokens.has_refresh_token() {
                    "stored"
                } else {
                    "not returned by provider"
                }
            );
            if daemon_reachable(&paths) {
                println!("The running daemon will use the new token on its next provider call.");
            }
            Ok(())
        }
        AuthAction::Status => {
            let status = formless_core::auth::auth_status(&paths, &cfg.provider);
            println!("mode:           {}", status.mode);
            println!(
                "credential:     {}",
                if status.credential_present {
                    "present"
                } else {
                    "MISSING"
                }
            );
            if cfg.provider.oauth() {
                println!(
                    "source:         {}",
                    if std::env::var_os("FORMLESS_OAUTH_ACCESS_TOKEN").is_some() {
                        "FORMLESS_OAUTH_ACCESS_TOKEN"
                    } else {
                        "keychain / 0600 token file"
                    }
                );
                match status.expires_at {
                    Some(expires) => {
                        let now = formless_core::auth::now_epoch();
                        if expires > now {
                            println!("expires:        Unix {expires} (in {}s)", expires - now);
                        } else {
                            println!("expires:        Unix {expires} (expired)");
                        }
                    }
                    None => println!("expires:        (provider supplied no expiry)"),
                }
                println!(
                    "refresh token:  {}",
                    if status.has_refresh_token {
                        "stored"
                    } else {
                        "none"
                    }
                );
                if let Some(scope) = status.scope {
                    println!("scope:          {scope}");
                }
                if !status.credential_present {
                    println!("Run `formless auth login` to sign in.");
                }
            }
            Ok(())
        }
        AuthAction::Logout => {
            formless_core::auth::delete_oauth_tokens(&paths);
            println!("Stored OAuth tokens deleted for this profile.");
            Ok(())
        }
    }
}

/// A problem `doctor` found: what's wrong, what to run, and a safe fix.
struct Problem {
    what: String,
    next: Option<String>,
    fix: Option<Fix>,
}

enum Fix {
    /// Leftovers of a daemon that is gone.
    RemoveStale(Vec<std::path::PathBuf>),
    /// A daemon process that no longer answers: stop it, start a new one.
    RestartStuck(u32),
    /// Secret files others can read.
    Chmod600(Vec<std::path::PathBuf>),
}

/// Whether `pid` is a live formless process (unix: `ps`).
#[cfg(unix)]
fn formless_pid_alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("formless"))
        .unwrap_or(false)
}

/// The daemon and file checks `--fix` can act on.
fn doctor_local_problems(paths: &Paths) -> Vec<Problem> {
    let mut out = Vec::new();
    // Jobs that will not fire on schedule (SPEC §9.7).
    if let (Ok(cfg), true) = (Config::load(paths), paths.db_file().exists()) {
        let active = formless_core::Store::open(&paths.db_file())
            .and_then(|s| s.has_active_jobs())
            .unwrap_or(false);
        if active && !cfg.daemon.keep_alive_for_jobs {
            out.push(Problem {
                what:
                    "scheduled jobs run only while Formless is running (the daemon stops when idle)"
                        .into(),
                next: Some("formless config set daemon.keep_alive_for_jobs true".into()),
                fix: None,
            });
        }
    }
    if let Ok(cfg) = Config::load(paths) {
        for (p, why) in cfg.security.roots_checked().1 {
            out.push(Problem {
                what: format!("security.trusted_paths entry {p:?} is ignored: {why}"),
                next: Some(format!(
                    "use an absolute folder in {}",
                    paths.config_file().display()
                )),
                fix: None,
            });
        }
    }
    #[cfg(unix)]
    if !daemon_reachable(paths) {
        let sock = paths.socket_path();
        let lock = paths.lock_file();
        let pid = std::fs::read_to_string(&lock)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        match pid {
            Some(pid) if formless_pid_alive(pid) => out.push(Problem {
                what: format!("the daemon (pid {pid}) is running but doesn't answer on its socket"),
                next: Some(format!(
                    "kill {pid}; then any formless command starts a new one"
                )),
                fix: Some(Fix::RestartStuck(pid)),
            }),
            _ => {
                let stale: Vec<std::path::PathBuf> =
                    [sock, lock].into_iter().filter(|p| p.exists()).collect();
                if !stale.is_empty() {
                    out.push(Problem {
                        what: format!(
                            "a daemon that is gone left {}",
                            stale
                                .iter()
                                .map(|p| p
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned())
                                .collect::<Vec<_>>()
                                .join(" and ")
                        ),
                        next: Some(format!(
                            "rm {}",
                            stale
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(" ")
                        )),
                        fix: Some(Fix::RemoveStale(stale)),
                    });
                }
            }
        }
    }
    // A data dir too deep for a unix socket uses a short socket path
    // instead; that folder must be usable (SPEC §9.1).
    #[cfg(unix)]
    if paths.socket_path().parent() != Some(paths.data_dir.as_path()) {
        if let Err(e) = paths.prepare_socket_dir() {
            out.push(Problem {
                what: format!("{e:#}"),
                next: Some("set a shorter FORMLESS_DATA_DIR".into()),
                fix: None,
            });
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let open: Vec<std::path::PathBuf> = [
            paths.key_fallback_file(),
            paths.relay_secret_file(),
            paths.oauth_tokens_file(),
            paths.config_file(),
        ]
        .into_iter()
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o077 != 0))
        .collect();
        if !open.is_empty() {
            out.push(Problem {
                what: format!(
                    "others can read {}",
                    open.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                next: Some(format!(
                    "chmod 600 {}",
                    open.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                )),
                fix: Some(Fix::Chmod600(open)),
            });
        }
    }
    out
}

/// Apply one fix; what was done, or why it wasn't.
fn apply_fix(paths: &Paths, fix: &Fix) -> Result<String, String> {
    match fix {
        Fix::RemoveStale(files) => {
            for f in files {
                std::fs::remove_file(f).map_err(|e| format!("{}: {e}", f.display()))?;
            }
            Ok("removed the stale files".into())
        }
        Fix::RestartStuck(pid) => {
            let killed = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status()
                .map_err(|e| e.to_string())?;
            if !killed.success() {
                return Err(format!("could not stop pid {pid}"));
            }
            for _ in 0..50 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                #[cfg(unix)]
                if !formless_pid_alive(*pid) {
                    break;
                }
            }
            let _ = std::fs::remove_file(paths.lock_file());
            #[cfg(unix)]
            let _ = std::fs::remove_file(paths.socket_path());
            formless_core::daemon::ensure_daemon(paths).map_err(|e| format!("{e:#}"))?;
            Ok(format!("stopped pid {pid} and started a new daemon"))
        }
        Fix::Chmod600(files) => {
            #[cfg(unix)]
            for f in files {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("{}: {e}", f.display()))?;
            }
            Ok("made the secret files private (600)".into())
        }
    }
}

fn doctor(paths: Paths, fix: bool) -> Result<()> {
    let problems = doctor_report(&paths, false)?;
    if problems.is_empty() {
        println!("\nno problems found");
        return Ok(());
    }
    println!("\n{} problem(s):", problems.len());
    for p in &problems {
        println!("  ✗ {}", p.what);
        if let Some(next) = &p.next {
            println!("    → {next}");
        }
        if fix && p.fix.is_none() {
            println!("    (not fixed automatically)");
        }
    }
    let fixes: Vec<&Fix> = problems.iter().filter_map(|p| p.fix.as_ref()).collect();
    if !fix {
        if !fixes.is_empty() {
            println!(
                "\n`formless doctor --fix` can fix {} of these.",
                fixes.len()
            );
        }
        return Ok(());
    }
    if fixes.is_empty() {
        return Ok(());
    }
    println!("\nfixing:");
    for f in fixes {
        match apply_fix(&paths, f) {
            Ok(done) => println!("  ✓ {done}"),
            Err(e) => println!("  ✗ {e}"),
        }
    }
    // Check again (this re-runs the probe too).
    println!("\nchecking again:");
    let left = doctor_report(&paths, false)?;
    if left.is_empty() {
        println!("\nno problems left");
    } else {
        println!("\n{} problem(s) left:", left.len());
        for p in &left {
            println!("  ✗ {}", p.what);
            if let Some(next) = &p.next {
                println!("    → {next}");
            }
        }
    }
    Ok(())
}

/// Secrets out of text meant for others: bearer tokens, API keys, pairing
/// and login fragments, URL credentials, long tokens, and the home path.
fn redact(text: &str) -> String {
    use std::sync::OnceLock;
    static RULES: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        [
            (r"(?i)(bearer\s+)[^\s\x22',]+", "${1}[redacted]"),
            (r"(?i)((?:api[_-]?key|token|secret|password|authorization)\x22?\s*[:=]\s*\x22?)[^\s\x22',]+", "${1}[redacted]"),
            (r"#(k|local)=[^\s&\x22']+", "#${1}=[redacted]"),
            (r"(://)[^/\s:@]+:[^/\s@]+@", "${1}[redacted]@"),
            (r"\b(sk|pk|rk|ghp|gho|xox[abp])[-_][A-Za-z0-9_\-]{8,}", "${1}-[redacted]"),
            (r"[A-Za-z0-9+/_\-]{40,}={0,2}", "[redacted]"),
        ]
        .into_iter()
        .map(|(re, to)| (regex::Regex::new(re).expect("redaction rule"), to))
        .collect()
    });
    let mut out = text.to_string();
    for (re, to) in rules {
        out = re.replace_all(&out, *to).into_owned();
    }
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        out = out.replace(&*home.to_string_lossy(), "~");
    }
    out
}

/// `doctor --json`: the facts and problems, redacted.
fn doctor_json(paths: &Paths) -> Result<serde_json::Value> {
    let cfg = Config::load(paths)?;
    let problems = doctor_report(paths, true)?;
    let auth = formless_core::auth::auth_status(paths, &cfg.provider);
    let report = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "profile": paths.profile,
        "data_dir": paths.data_dir.display().to_string(),
        "config_dir": paths.config_dir.display().to_string(),
        "db_present": paths.db_file().exists(),
        "log_bytes": formless_core::logs::log_usage(paths),
        "daemon_running": daemon_reachable(paths),
        "provider": {
            "configured": cfg.provider.is_configured(),
            "base_url": cfg.provider.base_url,
            "model": cfg.provider.model,
            "api_style": cfg.provider.api_style,
            "tool_mode": cfg.provider.tool_mode,
            "auth_mode": auth.mode,
            "credential_present": auth.credential_present,
        },
        "relay": { "enabled": cfg.relay.enabled, "url": cfg.relay.url },
        "naming": naming_model_status(paths, &cfg),
        "problems": problems.iter().map(|p| serde_json::json!({
            "what": p.what,
            "next": p.next,
            "fixable": p.fix.is_some(),
        })).collect::<Vec<_>>(),
    });
    Ok(serde_json::from_str(&redact(&report.to_string()))?)
}

/// `formless bug-report`: doctor, versions, OS and the last 200 log lines,
/// all redacted, in one Markdown file to attach to an issue.
fn bug_report(paths: &Paths, output: Option<std::path::PathBuf>) -> Result<()> {
    let doctor = doctor_json(paths)?;
    let log = std::fs::read_to_string(paths.log_file()).unwrap_or_default();
    let tail: Vec<&str> = log
        .lines()
        .rev()
        .take(200)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let text = format!(
        "# formless bug report\n\n\
         - formless {} on {} {}\n\
         - created {}\n\n\
         Secrets (keys, tokens, pairing links, URL credentials, long tokens) and your home path are \
         redacted. Read it through before attaching it.\n\n\
         ## What happened\n\n(describe it here)\n\n\
         ## Doctor\n\n```json\n{}\n```\n\n\
         ## Daemon log (last {} lines)\n\n```\n{}\n```\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        serde_json::to_string_pretty(&doctor)?,
        tail.len(),
        redact(&tail.join("\n")),
    );
    let path = output.unwrap_or_else(|| {
        std::path::PathBuf::from(format!(
            "formless-bug-report-{}.md",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        ))
    });
    std::fs::write(&path, text)?;
    println!(
        "wrote {} (secrets redacted; read it before attaching it to an issue)",
        path.display()
    );
    Ok(())
}

/// Print the doctor report and return the problems found.
/// `formless storage`: disk use by kind (SPEC §9.6).
fn storage_cmd(paths: &Paths, json: bool) -> Result<()> {
    use formless_core::housekeeping::human_bytes;
    let u = if daemon_reachable(paths) {
        formless_tui::client::request_once(paths, Request::StorageUsage)?
    } else {
        let store = formless_core::Store::open(&paths.db_file())?;
        formless_core::housekeeping::usage(paths, &store)?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&u)?);
        return Ok(());
    }
    let mut tbl = Table::new(&["KIND", "SIZE", "WHAT"]);
    for k in u["kinds"].as_array().into_iter().flatten() {
        let inside = k["inside_db"] == true;
        tbl.row(vec![
            t(
                format!("{}{}", if inside { "  " } else { "" }, k["kind"].as_str().unwrap_or_default()),
                if inside { Tone::Dim } else { Tone::Strong },
            ),
            c(human_bytes(k["bytes"].as_u64().unwrap_or(0))),
            t(k["detail"].as_str().unwrap_or_default(), Tone::Dim),
        ]);
    }
    tbl.print();
    println!(
        "total:     {} in {}",
        human_bytes(u["total_bytes"].as_u64().unwrap_or(0)),
        u["data_dir"].as_str().unwrap_or_default()
    );
    println!("free some: formless gc --dry-run");
    Ok(())
}

/// `formless gc [--dry-run]`.
fn gc_cmd(paths: &Paths, dry_run: bool) -> Result<()> {
    use formless_core::housekeeping::human_bytes;
    let r = if daemon_reachable(paths) {
        formless_tui::client::request_once(paths, Request::Gc { dry_run })?
    } else {
        let store = formless_core::Store::open(&paths.db_file())?;
        formless_core::housekeeping::gc(paths, &store, &Config::load(paths)?, dry_run)?
    };
    let mut tbl = Table::new(&["KIND", "ITEMS", "FREES", "WHAT"]);
    for k in r["kinds"].as_array().into_iter().flatten() {
        tbl.row(vec![
            t(k["kind"].as_str().unwrap_or_default(), Tone::Strong),
            c(k["items"].to_string()),
            c(human_bytes(k["bytes"].as_u64().unwrap_or(0))),
            t(k["detail"].as_str().unwrap_or_default(), Tone::Dim),
        ]);
    }
    tbl.print();
    let total = human_bytes(r["total_bytes"].as_u64().unwrap_or(0));
    if dry_run {
        println!("would free about {total} · run `formless gc` to do it");
    } else {
        println!("freed about {total}");
    }
    Ok(())
}

/// `formless backup`.
fn backup_cmd(paths: &Paths) -> Result<()> {
    let r = if daemon_reachable(paths) {
        formless_tui::client::request_once(paths, Request::Backup)?
    } else {
        let store = formless_core::Store::open(&paths.db_file())?;
        let keep = Config::load(paths)?.daemon.backups_keep.max(1);
        let f = formless_core::housekeeping::backup(paths, &store, keep)?;
        serde_json::json!({"file": f.display().to_string(), "bytes": formless_core::housekeeping::path_bytes(&f)})
    };
    println!(
        "backed up: {} ({})",
        r["file"].as_str().unwrap_or_default(),
        formless_core::housekeeping::human_bytes(r["bytes"].as_u64().unwrap_or(0))
    );
    println!("restore:   formless daemon stop && formless restore FILE");
    Ok(())
}

fn doctor_report(paths: &Paths, quiet: bool) -> Result<Vec<Problem>> {
    // `quiet`: only the problems (for --json and bug reports).
    macro_rules! say {
        ($($t:tt)*) => {
            if !quiet {
                println!($($t)*);
            }
        };
    }
    let paths = paths.clone();
    let mut problems = doctor_local_problems(&paths);
    use formless_core::Store;
    say!("data dir:   {}", paths.data_dir.display());
    say!("config dir: {}", paths.config_dir.display());
    say!(
        "db:         {} ({})",
        paths.db_file().display(),
        if paths.db_file().exists() {
            "present"
        } else {
            "absent"
        }
    );
    let cfg = Config::load(&paths)?;
    if let Ok((n, bytes)) =
        Store::open(&paths.db_file()).and_then(|s| formless_core::artifacts::usage(&s))
    {
        say!(
            "artifacts:  {n} ({:.1} MB; caps {} MB, {} days)",
            bytes as f64 / 1e6,
            cfg.artifacts.max_total_mb,
            cfg.artifacts.max_age_days
        );
    }
    match formless_core::service::installed(&paths) {
        Some((manager, file)) => say!("service:    starts at login ({manager}: {})", file.display()),
        None => say!("service:    not installed (formless daemon install-service starts it at login)"),
    }
    match formless_core::housekeeping::last_backup(&paths) {
        Some((file, at)) => {
            let age = at.elapsed().unwrap_or_default().as_secs();
            say!(
                "backups:    last {} ago ({}; keeps {})",
                if age >= 86_400 {
                    format!("{}d", age / 86_400)
                } else {
                    format!("{}h", age / 3600)
                },
                file.display(),
                cfg.daemon.backups_keep
            );
        }
        None if cfg.daemon.backups_keep == 0 => say!("backups:    off (daemon.backups_keep = 0)"),
        None => say!("backups:    none yet (the daemon takes one daily; formless backup now)"),
    }
    say!(
        "logs:       {:.1} MB (rotates at {:.1} MB, keeps {})",
        formless_core::logs::log_usage(&paths) as f64 / 1e6,
        cfg.daemon.log_max_bytes as f64 / 1e6,
        cfg.daemon.log_keep
    );
    say!(
        "approvals:  {} (trusted folders: {}, trusted hosts: {}; security.secure in {} or --secure)",
        if cfg.security.secure {
            "on: sensitive Tool calls ask first"
        } else {
            "OFF: Tools run without asking"
        },
        cfg.security.roots().len(),
        cfg.security.trusted_hosts.len(),
        paths.config_file().display()
    );
    say!(
        "base_url:   {}",
        if cfg.provider.base_url.is_empty() {
            "(unset)"
        } else {
            &cfg.provider.base_url
        }
    );
    say!(
        "model:      {}",
        if cfg.provider.model.is_empty() {
            "(unset)"
        } else {
            &cfg.provider.model
        }
    );
    say!(
        "api style:  {} (tools: {}, fallbacks: [{}])",
        cfg.provider.api_style,
        cfg.provider.tool_mode,
        cfg.provider.fallback_models.join(", ")
    );
    if formless_core::config::ProviderCfg::is_auto_model(&cfg.provider.model) {
        say!(
            "routing:    auto over [{}]",
            cfg.provider.routing_candidates().join(", ")
        );
        if let Ok(store) = Store::open(&paths.db_file()) {
            if let Ok(stats) = store.list_model_stats() {
                let picked = formless_core::routing::select_model(&cfg.provider, &stats, false);
                say!(
                    "            simple task now routes to: {}",
                    picked.as_deref().unwrap_or("(no candidates)")
                );
                for st in &stats {
                    say!(
                        "            {}: ok={} fail={} total_ms={}",
                        st.model,
                        st.successes,
                        st.failures,
                        st.total_ms
                    );
                }
            }
        }
    }
    let auth_status = formless_core::auth::auth_status(&paths, &cfg.provider);
    say!(
        "auth:       {} ({})",
        auth_status.mode,
        if auth_status.credential_present {
            "credential present"
        } else {
            "MISSING"
        }
    );
    if cfg.provider.oauth() {
        say!(
            "oauth:      refresh={} expires_at={}",
            if auth_status.has_refresh_token {
                "stored"
            } else {
                "none"
            },
            auth_status
                .expires_at
                .map(|expires| expires.to_string())
                .unwrap_or_else(|| "(none)".into())
        );
    }
    let credential = formless_core::auth::resolve_provider_token_blocking(&paths, &cfg.provider);
    if let Err(error) = &credential {
        say!("auth error: {error:#}");
        if cfg.provider.is_configured() {
            problems.push(Problem {
                what: format!("no usable credential for the provider ({error})"),
                next: Some(if cfg.provider.oauth() {
                    "formless auth login".into()
                } else {
                    "formless (the wizard stores the key), or set FORMLESS_API_KEY".into()
                }),
                fix: None,
            });
        }
    }
    say!("naming:     {}", naming_model_status(&paths, &cfg));
    say!(
        "daemon:     {}",
        if daemon_reachable(&paths) {
            "running"
        } else {
            "not running"
        }
    );
    if cfg.provider.is_configured() {
        let probe_model = if formless_core::config::ProviderCfg::is_auto_model(&cfg.provider.model)
        {
            cfg.provider.routing_candidates().into_iter().next()
        } else {
            Some(cfg.provider.model.clone())
        };
        match (probe_model, credential.as_ref()) {
            (None, _) => {
                say!("probe:      skipped (model=auto with no routing candidates)")
            }
            (_, Err(_)) => say!("probe:      skipped (credential unavailable)"),
            (Some(probe_model), Ok(credential)) => {
                match probe_blocking(&cfg.provider, credential, &probe_model) {
                    Ok(r) => {
                        say!(
                            "probe:      models={} streaming={} tool_calling={} prompt_fallback={}",
                            r.models.len(),
                            r.streaming,
                            r.tool_calling,
                            r.prompt_fallback
                        );
                        if !r.tool_calling {
                            say!("WARNING: endpoint/model does not support tool-calling; sessions will be refused (SPEC §4.3).");
                            problems.push(Problem {
                                what: format!("{probe_model} doesn't call tools on this endpoint"),
                                next: Some("formless config set provider.tool_mode prompt (or pick a model that supports tools: formless config set provider.model …)".into()),
                                fix: None,
                            });
                        }
                        match formless_core::provider::cache_probe_blocking(
                            &cfg.provider,
                            credential,
                            &probe_model,
                        ) {
                            Ok(Some(u)) if u.cached_tokens > 0 => say!(
                                "cache:      yes ({} of {} prompt tokens cached on a repeat)",
                                u.cached_tokens,
                                u.prompt_tokens
                            ),
                            Ok(Some(u)) => say!(
                                "cache:      no cached tokens on a repeated {}-token prompt",
                                u.prompt_tokens
                            ),
                            Ok(None) => {
                                say!("cache:      unknown (endpoint reports no token usage)")
                            }
                            Err(e) => say!("cache:      probe failed: {e}"),
                        }
                    }
                    Err(e) => {
                        let x = formless_core::errors::explain(
                            &format!("{e:#}"),
                            &formless_core::errors::ErrorContext::from_config(&cfg, &probe_model),
                        );
                        say!("probe:      FAILED ({}): {}", x.category, x.message);
                        if let Some(action) = &x.action {
                            say!("            next: {action}");
                        }
                        problems.push(Problem {
                            what: format!("the provider probe failed: {}", x.message),
                            next: x.action.clone(),
                            fix: None,
                        });
                        say!("            details: {e:#}");
                    }
                }
            }
        }
    } else {
        say!("probe:      skipped (not configured; run `formless` for the wizard)");
        problems.push(Problem {
            what: "no provider is configured".into(),
            next: Some("formless (the setup wizard)".into()),
            fix: None,
        });
    }
    Ok(problems)
}

/// Headless Run (SPEC §12): streams events, answers approvals fail-closed
/// unless `yes`, exits 0 answered / 1 errored-paused / 2 usage.
/// `90`, `90s`, `10m`, `2h`, `1h30m` as a duration (bare numbers are
/// seconds).
fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    let bad = || format!("'{s}' is not a duration (like 90s, 10m or 1h30m)");
    if let Ok(secs) = s.parse::<u64>() {
        return Ok(std::time::Duration::from_secs(secs));
    }
    let (mut total, mut num) = (0u64, String::new());
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().map_err(|_| bad())?;
        num.clear();
        total += n * match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return Err(bad()),
        };
    }
    if !num.is_empty() || total == 0 {
        return Err(bad());
    }
    Ok(std::time::Duration::from_secs(total))
}

/// `1h30m`, `10m`, `45s`.
fn format_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    let (h, m, sec) = (s / 3600, s % 3600 / 60, s % 60);
    let mut out = String::new();
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if sec > 0 || out.is_empty() {
        out.push_str(&format!("{sec}s"));
    }
    out
}

/// What `formless run` prints (SPEC §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum RunOutput {
    Text,
    Final,
    Json,
    #[value(name = "jsonl-events")]
    JsonlEvents,
}

/// The `--attach` files as (name, bytes); a missing, unreadable or too
/// large file is an error before anything runs.
fn read_attachments(paths: &[std::path::PathBuf]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let max = formless_core::daemon::MAX_ATTACHMENT_BYTES;
    paths
        .iter()
        .map(|p| {
            let bytes =
                std::fs::read(p).map_err(|e| format!("cannot attach {}: {e}", p.display()))?;
            if bytes.len() > max {
                return Err(format!(
                    "cannot attach {}: larger than {} MB",
                    p.display(),
                    max / 1024 / 1024
                ));
            }
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "attachment".into());
            Ok((name, bytes))
        })
        .collect()
}

/// A model and effort to set on a Run's Session.
type ModelPick = (Option<String>, Option<String>);

/// `--model` / `--effort` / `--role` as the (model, effort) to set on the
/// Run's Session, or None to leave it. A role must exist; it becomes the
/// `role:<name>` model the daemon resolves.
fn run_model(
    paths: &Paths,
    model: Option<String>,
    effort: Option<String>,
    role: Option<String>,
) -> Result<Option<ModelPick>, String> {
    let model = match role {
        Some(r) => {
            let cfg = Config::load(paths).map_err(|e| e.to_string())?;
            if !cfg.roles.contains_key(&r) {
                let known: Vec<&String> = cfg.roles.keys().collect();
                return Err(format!("no role '{r}' (configured: {known:?})"));
            }
            Some(format!("role:{r}"))
        }
        None => model.filter(|m| !m.trim().is_empty()),
    };
    Ok((model.is_some() || effort.is_some()).then_some((model, effort)))
}

/// The task and optional stdin attachment for `formless run`: `-` takes
/// the task from stdin; otherwise non-empty piped stdin rides along as
/// stdin.txt.
fn run_input(task: String, stdin: Option<Vec<u8>>) -> Result<(String, Option<Vec<u8>>), String> {
    let piped = stdin.filter(|b| !b.iter().all(u8::is_ascii_whitespace));
    if task == "-" {
        let bytes = piped.ok_or("`-` reads the task from stdin, but nothing was piped in")?;
        let text = String::from_utf8(bytes).map_err(|_| "the task on stdin is not UTF-8 text")?;
        return Ok((text.trim().to_string(), None));
    }
    Ok((task, piped))
}

/// The Session the last `formless run` used, if it still exists.
fn last_run_session(paths: &Paths) -> Result<String, String> {
    let id = std::fs::read_to_string(paths.last_run_session_file())
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or("no earlier `formless run` Session to continue; run once without --continue")?;
    formless_core::daemon::ensure_daemon(paths).map_err(|e| format!("{e:#}"))?;
    match formless_tui::client::request_once(paths, Request::GetSession { id: id.clone() }) {
        Ok(_) => Ok(id),
        Err(e) if !e.to_string().contains("session not found") => Err(format!("{e:#}")),
        Err(_) => Err(format!(
            "the last `formless run` Session ({id}) no longer exists; run once without --continue"
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_headless(
    paths: Paths,
    task: String,
    files: Vec<(String, Vec<u8>)>,
    session: Option<String>,
    pick: Option<ModelPick>,
    dir: Option<std::path::PathBuf>,
    yes: bool,
    answer: Option<String>,
    output: RunOutput,
    quiet: bool,
    timeout: Option<std::time::Duration>,
    max_steps: Option<u32>,
) -> Result<i32> {
    use std::io::IsTerminal;
    let json_out = output == RunOutput::JsonlEvents;
    // The answer streams only in text mode; progress goes to stderr in
    // text mode, and in final mode when someone is watching.
    let stream = output == RunOutput::Text;
    let progress = !quiet
        && match output {
            RunOutput::Text => true,
            RunOutput::Final => std::io::stderr().is_terminal(),
            _ => false,
        };
    let interactive = matches!(output, RunOutput::Text | RunOutput::Final);
    let mut final_answer: Option<String> = None;
    let mut run_error: Option<String> = None;
    let mut summary = RunSummary::default();
    let cfg = Config::load(&paths)?;
    if !cfg.provider.is_configured() {
        eprintln!("not configured; run `formless` for the wizard");
        return Ok(2);
    }
    formless_core::daemon::ensure_daemon(&paths)?;
    if let Some(warning) = formless_tui::client::check_daemon_version(&paths)? {
        if !quiet {
            eprintln!("warning: {warning}");
        }
    }
    let (client, rx) = formless_tui::client::DaemonClient::connect(&paths)?;
    // Resolve/create session.
    let session_id = match session {
        Some(id) => id,
        None => {
            let id = client.send(Request::CreateSession {
                name: Some("headless".into()),
            });
            let mut found = None;
            while found.is_none() {
                match rx.recv() {
                    Ok(ServerMessage::Response {
                        id: rid,
                        ok: true,
                        result,
                        ..
                    }) if rid == id => {
                        let s: SessionInfo = serde_json::from_value(result.unwrap_or_default())?;
                        found = Some(s.id);
                    }
                    Ok(_) => {}
                    Err(e) => return Err(anyhow::anyhow!("daemon closed: {e}")),
                }
            }
            found.unwrap()
        }
    };
    let _ = std::fs::write(paths.last_run_session_file(), &session_id);
    client.send(Request::Subscribe {
        sessions: Some(vec![session_id.clone()]),
        deltas: true,
        client: Some("CLI".into()),
    });
    // Requests on one connection run in order: the folder and model are
    // set and the attachments stored before the message.
    if let Some(d) = dir {
        client.send(Request::SetSessionCwd {
            id: session_id.clone(),
            cwd: Some(d.to_string_lossy().into_owned()),
        });
    }
    if let Some((model, effort)) = pick {
        client.send(Request::SetSessionModel {
            id: session_id.clone(),
            model,
            reasoning_effort: effort,
        });
    }
    for (name, bytes) in files {
        use base64::Engine;
        // The daemon guesses the type from the name (images go to vision
        // models inline), as for /attach.
        client.send(Request::AttachFile {
            session_id: session_id.clone(),
            name,
            data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime: None,
        });
    }
    if max_steps.is_some() {
        client.send(Request::SetStepLimit {
            session_id: session_id.clone(),
            max_steps,
        });
    }
    client.send(Request::SendMessage {
        session_id: session_id.clone(),
        content: task,
        mode: SendMode::Send,
    });
    let sigints = count_sigints();
    let mut interrupting = false;
    let mut code = 1;
    let started = std::time::Instant::now();
    // --timeout: interrupt, give the Run a moment to stop, then exit 124.
    let mut timed_out: Option<std::time::Instant> = None;
    loop {
        if let Some(limit) = timeout {
            match timed_out {
                None if started.elapsed() >= limit => {
                    eprintln!(
                        "timed out after {}; interrupting the run",
                        format_duration(limit)
                    );
                    client.send(Request::InterruptRun {
                        session_id: session_id.clone(),
                    });
                    timed_out = Some(std::time::Instant::now());
                }
                Some(at) if at.elapsed() >= std::time::Duration::from_secs(10) => break,
                _ => {}
            }
        }
        // Ctrl+C (SPEC §12): first interrupts the Run and waits for it to
        // stop (exit 130); a second one exits at once.
        match sigints.load(std::sync::atomic::Ordering::SeqCst) {
            0 => {}
            1 if !interrupting => {
                interrupting = true;
                eprintln!("interrupting the run… (Ctrl+C again to quit now)");
                client.send(Request::InterruptRun {
                    session_id: session_id.clone(),
                });
            }
            1 => {}
            _ => return Ok(130),
        }
        let msg = match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(msg) => Ok(msg),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(e) => Err(e),
        };
        match msg {
            Ok(ServerMessage::Event(ev)) => {
                // The daemon broadcasts every Session's events; only act on ours.
                if ev.session_id() != Some(session_id.as_str()) {
                    continue;
                }
                if json_out {
                    println!("{}", serde_json::to_string(&ev).unwrap_or_default());
                }
                match ev {
                    Event::MessageDelta { delta, .. } => {
                        if stream {
                            print!("{delta}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Event::MessageComplete { message, .. } => {
                        if stream {
                            println!();
                        }
                        // The last answer the agent gave is the final one.
                        if message.role == "assistant" && !message.content.trim().is_empty() {
                            final_answer = Some(message.content);
                        }
                    }
                    Event::ToolCallStarted { tool_name, .. } => {
                        if progress {
                            eprintln!("[tool] {tool_name}");
                        }
                        summary.tool_started(&tool_name);
                    }
                    Event::ToolCallFinished { tool_name, ok, .. } => {
                        if ok && tool_name == formless_core::tools::CREATE_TOOL {
                            summary.tools_created += 1;
                        }
                    }
                    Event::RunUsage { usage, .. } => summary.usage = usage,
                    Event::UserQuestion {
                        question_id,
                        question,
                        options,
                        questions,
                        ..
                    } => {
                        let (reply, answers) = match &answer {
                            // --answer is one free-text reply to the whole call.
                            Some(a) => (a.clone(), Vec::new()),
                            None if std::io::stdin().is_terminal() && interactive => {
                                // An older daemon sends only the single-question
                                // form (and ignores `answers`).
                                let questions = if questions.is_empty() {
                                    vec![Question::single(question, options)]
                                } else {
                                    questions
                                };
                                let answers: Vec<QuestionAnswer> =
                                    questions.iter().map(ask_on_terminal).collect();
                                (QuestionAnswer::summary_of(&answers), answers)
                            }
                            None => ("[no answer: this is a non-interactive run; proceed on your best assumption and state it]".to_string(), Vec::new()),
                        };
                        if progress {
                            eprintln!("[answer] {reply}");
                        }
                        client.send(Request::AnswerQuestion {
                            question_id,
                            answer: reply,
                            answers,
                        });
                    }
                    Event::ApprovalRequested {
                        approval_id,
                        tool_name,
                        ..
                    } => {
                        if progress {
                            eprintln!(
                                "[approval] {} -> {}",
                                tool_name,
                                if yes {
                                    "approved once (--yes)"
                                } else {
                                    "DENIED (fail closed; pass --yes or approve in the TUI)"
                                }
                            );
                        }
                        // `--yes` approves for this Run only (SPEC §12): nothing persists.
                        client.send(Request::ApprovalDecision {
                            remember_denial: Default::default(),
                            paths: Default::default(),
                            approval_id,
                            approved: yes,
                            scope: ApprovalScope::Once,
                        });
                    }
                    Event::RunPaused {
                        error, explained, ..
                    } => {
                        run_error = Some(
                            explained
                                .as_ref()
                                .map_or_else(|| error.clone(), |x| x.message.clone()),
                        );
                        match explained {
                            Some(x) => {
                                eprintln!("run paused: {}", x.message);
                                if let Some(action) = x.action {
                                    eprintln!("  next: {action}");
                                }
                                eprintln!("  details: {error}");
                            }
                            None => eprintln!("run paused: {error}"),
                        }
                        code = 1;
                        break;
                    }
                    Event::RunFinished { status, .. } => {
                        code = match status.as_str() {
                            "completed" => 0,
                            "interrupted" => 130,
                            _ => 1,
                        };
                        break;
                    }
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    if timed_out.is_some() {
        code = 124;
        run_error.get_or_insert_with(|| "timed out".into());
    }
    let status = match code {
        0 => "completed",
        124 => "timed_out",
        130 => "interrupted",
        _ if run_error.is_some() => "paused",
        _ => "failed",
    };
    if progress {
        eprintln!(
            "{}",
            summary.line(&status.replace('_', " "), started.elapsed())
        );
    }
    match output {
        RunOutput::Final => {
            if let Some(a) = &final_answer {
                println!("{}", a.trim_end());
            }
        }
        RunOutput::Json => println!(
            "{}",
            serde_json::json!({
                "session_id": session_id,
                "status": status,
                "answer": final_answer,
                "error": run_error,
            })
        ),
        _ => {}
    }
    Ok(code)
}

/// What a headless Run did, for the line `formless run` ends with on stderr
/// (the TUI status line's figures).
#[derive(Default)]
struct RunSummary {
    usage: formless_protocol::UsageInfo,
    tools_created: usize,
    tools_used: std::collections::BTreeSet<String>,
}

impl RunSummary {
    fn tool_started(&mut self, name: &str) {
        if !formless_core::tools::is_builtin(name) {
            self.tools_used.insert(name.to_string());
        }
    }

    fn line(&self, status: &str, elapsed: std::time::Duration) -> String {
        let u = &self.usage;
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let mut parts = vec![
            format!("{status} in {}", format_duration(elapsed)),
            plural(u.steps as usize, "step"),
        ];
        if self.tools_created > 0 || !self.tools_used.is_empty() {
            parts.push(format!(
                "Tools: {} created, {} used",
                self.tools_created,
                self.tools_used.len()
            ));
        }
        let tokens = u.prompt_tokens + u.completion_tokens;
        parts.push(format!(
            "{} tokens ({} in, {} out)",
            compact_count(tokens),
            compact_count(u.prompt_tokens),
            compact_count(u.completion_tokens)
        ));
        if let Some(cost) = u.cost {
            parts.push(format!("${cost:.4}"));
        }
        parts.join(" · ")
    }
}

/// 950, 1.2k, 3.4M.
fn compact_count(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// Count Ctrl+C presses on a background thread instead of dying on the
/// first one, so the headless client can stop its Run cleanly.
fn count_sigints() -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    use std::sync::atomic::Ordering;
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = count.clone();
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return count;
    };
    // Installed here, before the Run starts, so no early Ctrl+C is missed.
    #[cfg(unix)]
    let listener = rt.block_on(async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    });
    std::thread::spawn(move || {
        rt.block_on(async move {
            #[cfg(unix)]
            if let Ok(mut sig) = listener {
                while sig.recv().await.is_some() {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            }
            #[cfg(not(unix))]
            while tokio::signal::ctrl_c().await.is_ok() {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        })
    });
    count
}

fn skills_cmd(paths: Paths, action: SkillsAction) -> Result<()> {
    let daemon = |req: Request| -> Result<serde_json::Value> {
        formless_core::daemon::ensure_daemon(&paths)?;
        formless_tui::client::request_once(&paths, req)
    };
    let set = |name: String, enabled: bool| {
        daemon(Request::SetSkillEnabled {
            name: name.clone(),
            enabled,
        })
        .map(|_| println!("{} {name}", if enabled { "enabled" } else { "disabled" }))
    };
    match action {
        SkillsAction::Add {
            source,
            subdir,
            git_ref,
            enable,
        } => {
            // A local folder is resolved here: the daemon may run elsewhere.
            let source = match std::fs::canonicalize(&source) {
                Ok(p) => p.to_string_lossy().into_owned(),
                Err(_) => source,
            };
            let info = daemon(Request::AddSkill {
                source,
                subdir,
                git_ref,
            })?;
            let name = info["name"].as_str().unwrap_or_default().to_string();
            println!(
                "installed {name} v{} — {}",
                info["version"],
                info["description"].as_str().unwrap_or_default()
            );
            if info["has_scripts"] == true {
                println!("note: it bundles scripts; Formless never runs them on the host (port them to a Tool).");
            }
            if enable {
                set(name, true)?;
            } else {
                println!("review it with `formless skills show {name}`, then `formless skills enable {name}`");
            }
        }
        SkillsAction::Remove { name } => {
            daemon(Request::RemoveSkill { name: name.clone() })?;
            println!("removed {name}");
        }
        SkillsAction::List { json } => {
            let list = daemon(Request::ListSkills)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
                return Ok(());
            }
            let skills = list.as_array().cloned().unwrap_or_default();
            if skills.is_empty() {
                println!("no skills installed (formless skills add …)");
            }
            let mut tbl = Table::new(&["SKILL", "VER", "STATE", "LOADS", "RUNS OK", "DESCRIPTION"]);
            for s in skills {
                let state = if s["retired"] == true {
                    t("retired", Tone::Dim)
                } else if s["enabled"] == true {
                    t("enabled", Tone::Good)
                } else {
                    t("disabled", Tone::Warn)
                };
                let mut desc = s["description"].as_str().unwrap_or_default().to_string();
                if s["has_scripts"] == true {
                    desc.push_str(" [scripts]");
                }
                if s["author"] == "auto" {
                    desc.push_str(" [auto]");
                }
                tbl.row(vec![
                    t(s["name"].as_str().unwrap_or_default(), Tone::Strong),
                    c(format!("v{}", s["version"])),
                    state,
                    c(s["loads"].to_string()),
                    c(match (s["runs"].as_i64(), s["runs_completed"].as_i64()) {
                        (Some(r), Some(done)) if r > 0 => format!("{}%", 100 * done / r),
                        _ => "-".into(),
                    }),
                    t(desc, Tone::Dim),
                ]);
            }
            if !tbl.is_empty() {
                tbl.print();
            }
        }
        SkillsAction::Enable { name } => set(name, true)?,
        SkillsAction::Disable { name } => set(name, false)?,
        SkillsAction::Show { name } => {
            let r = daemon(Request::GetSkill { name })?;
            println!("{}", r["body"].as_str().unwrap_or_default());
            if let Some(scripts) = r["scripts"].as_array().filter(|s| !s.is_empty()) {
                println!(
                    "\nscripts (never run on the host; the agent ports them to sandboxed Tools):"
                );
                for s in scripts {
                    println!("  {}", s.as_str().unwrap_or_default());
                }
            }
            println!("\nfiles:");
            for f in r["files"].as_array().into_iter().flatten() {
                println!("  {}", f.as_str().unwrap_or_default());
            }
        }
        SkillsAction::Export { name, all, output } => {
            let bundle = if all {
                daemon(Request::ExportSkills)?
            } else {
                let name = name.ok_or_else(|| anyhow::anyhow!("give a skill name or --all"))?;
                daemon(Request::ExportSkill { name })?
            };
            let text = serde_json::to_string_pretty(&bundle)?;
            match output {
                Some(f) => {
                    std::fs::write(&f, text)?;
                    println!("wrote {f}");
                }
                None => println!("{text}"),
            }
        }
        SkillsAction::Import { file } => {
            let bundle: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
            let imported = daemon(Request::ImportSkill { bundle })?;
            for info in imported.as_array().into_iter().flatten() {
                let name = info["name"].as_str().unwrap_or_default();
                println!(
                    "imported {name} v{} (disabled) — {}",
                    info["version"],
                    info["description"].as_str().unwrap_or_default()
                );
                // Show the contents before the user enables it.
                let r = daemon(Request::GetSkill {
                    name: name.to_string(),
                })?;
                println!("{}", r["body"].as_str().unwrap_or_default());
                for f in r["files"].as_array().into_iter().flatten() {
                    println!("  {}", f.as_str().unwrap_or_default());
                }
                if info["has_scripts"] == true {
                    println!("note: it bundles scripts; Formless never runs them on the host.");
                }
                println!("enable it with `formless skills enable {name}`\n");
            }
        }
    }
    Ok(())
}

fn web_cmd(paths: Paths, port: Option<u16>, no_open: bool, stop: bool) -> Result<()> {
    formless_core::daemon::ensure_daemon(&paths)?;
    if stop {
        formless_tui::client::request_once(&paths, Request::StopWebUi)?;
        println!("web UI stopped");
        return Ok(());
    }
    let r = formless_tui::client::request_once(&paths, Request::StartWebUi { port })?;
    let url = r["url"].as_str().unwrap_or_default().to_string();
    println!("Web UI on 127.0.0.1:{} (this link works once):", r["port"]);
    println!("{url}");
    if !no_open {
        let opener = if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg(&url).status()
        } else if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", &url])
                .status()
        } else {
            std::process::Command::new("xdg-open").arg(&url).status()
        };
        if opener.map(|s| !s.success()).unwrap_or(true) {
            println!("(could not open a browser; open the link above)");
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum SessionsAction {
    /// List Sessions (archived ones only with --archived)
    List {
        #[arg(long)]
        archived: bool,
        /// One JSON array of SessionInfo
        #[arg(long)]
        json: bool,
    },
    /// Show a Session's transcript
    Show {
        id: String,
        /// The Session and its messages as JSON
        #[arg(long)]
        json: bool,
    },
    /// Rename a Session
    Rename { id: String, name: String },
    /// Delete a Session (restorable for daemon.delete_undo_secs)
    Rm { id: String },
    /// Bring back a Session deleted moments ago (full id)
    Restore { id: String },
    /// Import a Session bundle (from `sessions export --format json`)
    Import { file: std::path::PathBuf },
    /// Search every Session's messages
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Hide a Session from the default list (kept and searchable)
    Archive { id: String },
    /// Bring an archived Session back
    Unarchive { id: String },
    /// Export a Session: the re-importable JSON bundle, or a readable
    /// Markdown or HTML transcript
    Export {
        id: String,
        /// json, markdown (md) or html
        #[arg(long, default_value = "json")]
        format: String,
        /// Write here instead of standard output
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
}

fn sessions_cmd(paths: Paths, action: SessionsAction) -> Result<()> {
    formless_core::daemon::ensure_daemon(&paths)?;
    let daemon = |r| formless_tui::client::request_once(&paths, r);
    match action {
        SessionsAction::List { archived, json } => {
            let list: Vec<SessionInfo> = serde_json::from_value(daemon(Request::ListSessions)?)?;
            if json {
                let shown: Vec<&SessionInfo> =
                    list.iter().filter(|s| s.archived == archived).collect();
                println!("{}", serde_json::to_string_pretty(&shown)?);
                return Ok(());
            }
            let mut tbl = Table::new(&["ID", "UPDATED", "MSGS", "NAME", "LAST MESSAGE"]);
            for s in list.iter().filter(|s| s.archived == archived) {
                tbl.row(vec![
                    t(&s.id[..8.min(s.id.len())], Tone::Dim),
                    c(s.updated_at.get(..16).unwrap_or(&s.updated_at)),
                    c(s.message_count.to_string()),
                    t(
                        format!("{}{}", if s.pinned { "★ " } else { "" }, s.name),
                        Tone::Strong,
                    ),
                    t(s.preview.clone().unwrap_or_default(), Tone::Dim),
                ]);
            }
            tbl.print();
        }
        SessionsAction::Show { id, json } => {
            let s = find_session(&paths, &id)?;
            let r = daemon(Request::GetSession { id: s.id.clone() })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            println!("{}  ({})", s.name, s.id);
            println!(
                "{} messages · updated {}{}{}",
                s.message_count,
                s.updated_at.get(..16).unwrap_or(&s.updated_at),
                s.model
                    .as_deref()
                    .map(|m| format!(" · {m}"))
                    .unwrap_or_default(),
                s.cwd
                    .as_deref()
                    .map(|c| format!(" · in {c}"))
                    .unwrap_or_default()
            );
            let messages: Vec<formless_protocol::MessageInfo> =
                serde_json::from_value(r["messages"].clone()).unwrap_or_default();
            for m in messages {
                let who = match m.role.as_str() {
                    "user" => "You",
                    "assistant" => "Formless",
                    "tool" => "  tool result",
                    _ => "  note",
                };
                if m.role == "tool" {
                    let first = m.content.lines().next().unwrap_or_default();
                    println!("{who}: {}", first.chars().take(120).collect::<String>());
                } else if !m.content.trim().is_empty() {
                    println!("\n{who}:\n{}", m.content.trim_end());
                }
            }
        }
        SessionsAction::Rename { id, name } => {
            let s = find_session(&paths, &id)?;
            daemon(Request::RenameSession {
                id: s.id,
                name: name.clone(),
            })?;
            println!("renamed {} to {name}", s.name);
        }
        SessionsAction::Rm { id } => {
            let s = find_session(&paths, &id)?;
            let r = daemon(Request::DeleteSession { id: s.id.clone() })?;
            match r["undo_secs"].as_u64() {
                Some(secs) => println!(
                    "deleted {} (restore within {secs}s: formless sessions restore {})",
                    s.name, s.id
                ),
                None => println!("deleted {}", s.name),
            }
        }
        SessionsAction::Restore { id } => {
            let s: SessionInfo = serde_json::from_value(daemon(Request::RestoreSession { id })?)?;
            println!("restored {}", s.name);
        }
        SessionsAction::Import { file } => {
            let bundle: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file)?)
                .with_context(|| format!("{} is not a JSON bundle", file.display()))?;
            let s: SessionInfo =
                serde_json::from_value(daemon(Request::ImportSession { bundle })?)?;
            println!("imported {}  ({})", s.name, s.id);
        }
        SessionsAction::Search { query, limit, json } => {
            let hits: Vec<formless_protocol::SearchHit> =
                serde_json::from_value(daemon(Request::SearchSessions {
                    query: query.clone(),
                    limit: Some(limit),
                })?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
                return Ok(());
            }
            if hits.is_empty() {
                println!("no messages match '{query}'");
            }
            for h in hits {
                println!(
                    "{}  {}  [{}] {}",
                    &h.session_id[..8.min(h.session_id.len())],
                    h.session_name,
                    h.role,
                    h.snippet.replace('\n', " ")
                );
            }
        }
        SessionsAction::Archive { id } => set_archived(&paths, &id, true)?,
        SessionsAction::Unarchive { id } => set_archived(&paths, &id, false)?,
        SessionsAction::Export { id, format, output } => {
            let s = find_session(&paths, &id)?;
            let r = daemon(Request::ExportSession {
                id: s.id,
                format: Some(format),
            })?;
            let text = match r["content"].as_str() {
                Some(c) => c.to_string(),
                None => serde_json::to_string_pretty(&r)?,
            };
            match output {
                Some(path) => {
                    std::fs::write(&path, text)?;
                    eprintln!("exported {} to {}", s.name, path.display());
                }
                None => print!("{text}"),
            }
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print where config.toml is
    Path,
    /// Print a setting's value (with FORMLESS_* overrides applied)
    Get { key: String },
    /// Set a setting through the daemon: validated, audited and applied at
    /// once. VALUE is JSON when it parses as JSON (5, true, ["a"]), else text
    Set { key: String, value: String },
    /// Edit config.toml in $EDITOR; it is replaced only if it still parses
    Edit,
}

fn config_cmd(paths: Paths, action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Path => println!("{}", paths.config_file().display()),
        ConfigAction::Get { key } => {
            let cfg = serde_json::to_value(Config::load(&paths)?)?;
            match cfg.pointer(&format!("/{}", key.replace('.', "/"))) {
                Some(serde_json::Value::String(s)) => println!("{s}"),
                Some(v) => println!("{}", serde_json::to_string_pretty(v)?),
                None => {
                    eprintln!("no setting '{key}' (see SPEC §13, or `formless config edit`)");
                    std::process::exit(2);
                }
            }
        }
        ConfigAction::Set { key, value } => {
            let value = serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value));
            formless_core::daemon::ensure_daemon(&paths)?;
            let r = formless_tui::client::request_once(
                &paths,
                Request::SetSetting {
                    key: key.clone(),
                    value: value.clone(),
                },
            )?;
            println!(
                "{key} = {value}{}",
                if r["applies"] == "on_tui_start" {
                    " (applies when the TUI next starts)"
                } else {
                    ""
                }
            );
        }
        ConfigAction::Edit => config_edit(&paths)?,
    }
    Ok(())
}

/// `config edit`: edit a copy, check it parses as a Config, then replace
/// config.toml (a broken edit never lands; you can edit again).
fn config_edit(paths: &Paths) -> Result<()> {
    use std::io::{BufRead, IsTerminal, Write};
    let file = paths.config_file();
    if !file.exists() {
        Config::default().save(paths)?;
    }
    let draft = file.with_extension("toml.edit");
    std::fs::copy(&file, &draft)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    loop {
        // $EDITOR may carry arguments and quotes ("code --wait"): let the
        // shell read it, as git does.
        #[cfg(unix)]
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg("sh")
            .arg(&draft)
            .status();
        #[cfg(not(unix))]
        let status = {
            let mut words = editor.split_whitespace();
            std::process::Command::new(words.next().unwrap_or("notepad"))
                .args(words)
                .arg(&draft)
                .status()
        };
        let status = status.with_context(|| format!("start editor '{editor}'"))?;
        if !status.success() {
            let _ = std::fs::remove_file(&draft);
            anyhow::bail!("the editor exited with {status}; config.toml unchanged");
        }
        match Config::load_from(&draft) {
            Ok(_) => {
                std::fs::rename(&draft, &file)?;
                println!(
                    "saved {} (the daemon picks it up on its next read)",
                    file.display()
                );
                return Ok(());
            }
            Err(e) => {
                eprintln!("config.toml would not parse:\n{e:#}");
                if !std::io::stdin().is_terminal() {
                    let _ = std::fs::remove_file(&draft);
                    anyhow::bail!("config.toml unchanged");
                }
                eprint!("Edit again? [Y/n] ");
                let _ = std::io::stderr().flush();
                let mut answer = String::new();
                std::io::stdin().lock().read_line(&mut answer)?;
                if answer.trim().eq_ignore_ascii_case("n") {
                    let _ = std::fs::remove_file(&draft);
                    println!("config.toml unchanged");
                    return Ok(());
                }
            }
        }
    }
}

/// The Session with this id or id prefix.
fn find_session(paths: &Paths, id: &str) -> Result<SessionInfo> {
    let list: Vec<SessionInfo> = serde_json::from_value(formless_tui::client::request_once(
        paths,
        Request::ListSessions,
    )?)?;
    let mut matches: Vec<SessionInfo> = list.into_iter().filter(|s| s.id.starts_with(id)).collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => anyhow::bail!("no Session '{id}' (formless sessions list)"),
        n => anyhow::bail!("'{id}' matches {n} Sessions; give more of the id"),
    }
}

/// Archive or restore a Session given its id or an id prefix.
fn set_archived(paths: &Paths, id: &str, archived: bool) -> Result<()> {
    let s = find_session(paths, id)?;
    formless_tui::client::request_once(
        paths,
        Request::SetSessionArchived {
            id: s.id.clone(),
            archived,
        },
    )?;
    println!(
        "{} {}",
        if archived { "archived" } else { "restored" },
        s.name
    );
    Ok(())
}

fn providers_cmd(paths: Paths, action: ProvidersAction) -> Result<()> {
    formless_core::daemon::ensure_daemon(&paths)?;
    let daemon = |req: Request| formless_tui::client::request_once(&paths, req);
    match action {
        ProvidersAction::List { json } => {
            let r = daemon(Request::ListProviders)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r["providers"])?);
                return Ok(());
            }
            let mut tbl = Table::new(&["NAME", "MODEL", "KEY", "BASE URL"]);
            for p in r["providers"].as_array().into_iter().flatten() {
                tbl.row(vec![
                    t(p["name"].as_str().unwrap_or_default(), Tone::Strong),
                    c(p["model"].as_str().filter(|m| !m.is_empty()).unwrap_or("-")),
                    if p["has_key"] == true {
                        t("stored", Tone::Good)
                    } else {
                        t("none", Tone::Warn)
                    },
                    t(p["base_url"].as_str().unwrap_or_default(), Tone::Dim),
                ]);
            }
            tbl.print();
        }
        ProvidersAction::Add {
            name,
            base_url,
            preset,
            model,
            no_key,
        } => {
            let preset = match preset.as_deref() {
                Some(id) => Some(formless_core::presets::find(id).with_context(|| {
                    let ids: Vec<_> = formless_core::presets::PRESETS
                        .iter()
                        .map(|p| p.id)
                        .collect();
                    format!("unknown preset '{id}' (one of: {})", ids.join(", "))
                })?),
                None => None,
            };
            let base_url = match (base_url, preset) {
                (Some(url), _) => url,
                (None, Some(p)) => p.base_url.to_string(),
                (None, None) => unreachable!("clap requires one"),
            };
            let read_line = || -> Result<String> {
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                Ok(line.trim().to_string())
            };
            let env_key = preset.and_then(formless_core::presets::env_key);
            let api_key = if no_key || preset.is_some_and(|p| p.local) {
                None
            } else if let Some((var, key)) = env_key {
                eprint!("Use ${var} as the API key for {name}? [Y/n] ");
                if read_line()?.eq_ignore_ascii_case("n") {
                    eprint!("API key for {name} (empty for none): ");
                    Some(read_line()?).filter(|k| !k.is_empty())
                } else {
                    Some(key)
                }
            } else {
                if let Some(url) = preset.and_then(|p| p.key_url) {
                    eprintln!("Get a key at {url}");
                }
                eprint!("API key for {name} (empty for none): ");
                Some(read_line()?).filter(|k| !k.is_empty())
            };
            let model = model.or_else(|| {
                preset
                    .map(|p| p.default_model.to_string())
                    .filter(|m| !m.is_empty())
            });
            daemon(Request::UpsertProvider {
                name: name.clone(),
                base_url,
                model,
                api_key,
            })?;
            println!("provider {name} saved; check it with `formless providers test {name}`");
        }
        ProvidersAction::Remove { name } => {
            daemon(Request::RemoveProvider { name: name.clone() })?;
            println!("removed {name}");
        }
        ProvidersAction::Test { name } => {
            let r = daemon(Request::TestProvider { name })?;
            let models = r["models"].as_array().cloned().unwrap_or_default();
            println!("{} model(s):", models.len());
            for m in models {
                println!("  {}", m.as_str().unwrap_or_default());
            }
        }
    }
    Ok(())
}

fn roles_cmd(paths: Paths, action: RolesAction) -> Result<()> {
    formless_core::daemon::ensure_daemon(&paths)?;
    let daemon = |req: Request| formless_tui::client::request_once(&paths, req);
    match action {
        RolesAction::List { json } => {
            let r = daemon(Request::ListProviders)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r["roles"])?);
                return Ok(());
            }
            let roles = r["roles"].as_object().cloned().unwrap_or_default();
            if roles.is_empty() {
                println!(
                    "no roles (formless roles add NAME --description … --provider … --model …)"
                );
            }
            let mut tbl = Table::new(&["ROLE", "PROVIDER", "MODEL", "DESCRIPTION"]);
            for (name, role) in roles {
                tbl.row(vec![
                    t(name, Tone::Strong),
                    c(role["provider"].as_str().unwrap_or("default")),
                    c(role["model"]
                        .as_str()
                        .filter(|m| !m.is_empty())
                        .unwrap_or("provider default")),
                    t(role["description"].as_str().unwrap_or_default(), Tone::Dim),
                ]);
            }
            if !tbl.is_empty() {
                tbl.print();
            }
        }
        RolesAction::Add {
            name,
            description,
            provider,
            model,
            effort,
            system,
        } => {
            daemon(Request::UpsertRole {
                name: name.clone(),
                role: serde_json::json!({"description": description, "provider": provider,
                    "model": model, "reasoning_effort": effort, "system": system}),
            })?;
            println!("role {name} saved");
        }
        RolesAction::Rm { name } => {
            daemon(Request::DeleteRole { name: name.clone() })?;
            println!("deleted role {name}");
        }
    }
    Ok(())
}

fn jobs_cmd(paths: Paths, action: JobsAction) -> Result<()> {
    formless_core::daemon::ensure_daemon(&paths)?;
    let daemon = |req: Request| formless_tui::client::request_once(&paths, req);
    match action {
        JobsAction::List { json } => {
            let list = daemon(Request::ListJobs)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
                return Ok(());
            }
            let jobs = list.as_array().cloned().unwrap_or_default();
            if jobs.is_empty() {
                println!("no scheduled jobs (formless jobs add …)");
            }
            let mut tbl = Table::new(&["ID", "STATE", "SCHEDULE", "NEXT RUN", "LAST", "TASK"]);
            for j in jobs {
                let state = if j["approved"] == false {
                    t("awaiting approval", Tone::Warn)
                } else if j["enabled"] == true {
                    t("on", Tone::Good)
                } else {
                    t("off", Tone::Dim)
                };
                let last = j["last_status"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("-");
                tbl.row(vec![
                    c(j["id"].as_str().unwrap_or_default()),
                    state,
                    c(j["cron"].as_str().unwrap_or("once")),
                    c(j["next_run"]
                        .as_str()
                        .map(|n| n.get(..16).unwrap_or(n))
                        .unwrap_or("-")),
                    t(
                        last,
                        if last == "completed" {
                            Tone::Good
                        } else if last == "-" {
                            Tone::Dim
                        } else {
                            Tone::Warn
                        },
                    ),
                    t(j["prompt"].as_str().unwrap_or_default(), Tone::Dim),
                ]);
            }
            if !tbl.is_empty() {
                tbl.print();
            }
        }
        JobsAction::Add {
            prompt,
            cron,
            at,
            session,
            overlap,
            notify,
            to_file,
            webhook,
        } => {
            let mut deliver = serde_json::Map::new();
            if notify {
                deliver.insert("notify".into(), true.into());
            }
            if let Some(f) = to_file {
                let f = std::path::absolute(&f).unwrap_or_else(|_| f.into());
                deliver.insert("file".into(), f.display().to_string().into());
            }
            if let Some(w) = webhook {
                deliver.insert("webhook".into(), w.into());
            }
            let j = daemon(Request::AddJob {
                prompt,
                session_id: session,
                cron,
                at,
                overlap,
                deliver: (!deliver.is_empty()).then(|| deliver.into()),
            })?;
            println!(
                "scheduled {} ({})",
                j["id"].as_str().unwrap_or_default(),
                j["cron"].as_str().unwrap_or("once")
            );
            // The next runs, to check the schedule reads as meant.
            let next = match j["cron"].as_str() {
                Some(c) => daemon(Request::PreviewSchedule {
                    schedule: c.to_string(),
                })?["next"]
                    .clone(),
                None => serde_json::json!([j["next_run"]]),
            };
            for t in next.as_array().into_iter().flatten() {
                println!("  next: {}", t.as_str().unwrap_or("-"));
            }
            if let Some(w) = j["warning"].as_str() {
                eprintln!("warning: {w}");
            }
        }
        JobsAction::Run { id } => {
            let r = daemon(Request::RunJobNow { id: id.clone() })?;
            println!("{id}: {}", r["status"].as_str().unwrap_or_default());
        }
        JobsAction::History { id, json } => {
            let runs = daemon(Request::JobRuns {
                id: id.clone(),
                limit: None,
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&runs)?);
                return Ok(());
            }
            let runs: Vec<formless_protocol::JobRunInfo> = serde_json::from_value(runs)?;
            if runs.is_empty() {
                println!("{id} has not run yet");
            }
            let mut tbl = Table::new(&[
                "STARTED", "TRIGGER", "OUTCOME", "TOOK", "COST", "SESSION", "DETAIL",
            ]);
            for r in runs {
                let took = r
                    .finished_at
                    .as_deref()
                    .and_then(|f| {
                        let a = chrono::DateTime::parse_from_rfc3339(&r.started_at).ok()?;
                        let b = chrono::DateTime::parse_from_rfc3339(f).ok()?;
                        Some(format!("{}s", (b - a).num_seconds()))
                    })
                    .unwrap_or_else(|| "-".into());
                tbl.row(vec![
                    c(r.started_at
                        .get(..16)
                        .unwrap_or(&r.started_at)
                        .replace('T', " ")),
                    c(r.trigger),
                    t(
                        r.status.clone(),
                        if r.status == "completed" {
                            Tone::Good
                        } else if r.status == "running" {
                            Tone::Plain
                        } else {
                            Tone::Warn
                        },
                    ),
                    c(took),
                    c(r.cost.map_or("-".into(), |c| format!("${c:.3}"))),
                    c(r.session_id.unwrap_or_default()),
                    t(r.detail, Tone::Dim),
                ]);
            }
            if !tbl.is_empty() {
                tbl.print();
            }
        }
        JobsAction::Enable { id } => {
            let r = daemon(Request::SetJobEnabled {
                id: id.clone(),
                enabled: true,
            })?;
            println!("enabled {id}");
            if let Some(w) = r["warning"].as_str() {
                eprintln!("warning: {w}");
            }
        }
        JobsAction::Disable { id } => {
            daemon(Request::SetJobEnabled {
                id: id.clone(),
                enabled: false,
            })?;
            println!("disabled {id}");
        }
        JobsAction::Rm { id } => {
            daemon(Request::DeleteJob { id: id.clone() })?;
            println!("deleted {id}");
        }
    }
    Ok(())
}

fn runs_cmd(paths: Paths, limit: usize, json: bool) -> Result<()> {
    formless_core::daemon::ensure_daemon(&paths)?;
    let v = formless_tui::client::request_once(
        &paths,
        Request::RunMetrics {
            limit: Some(limit),
            session_id: None,
            since: None,
        },
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let summary: formless_protocol::RunSummary = serde_json::from_value(v["summary"].clone())?;
    println!("{}", formless_core::runs::summary_line(&summary));
    for r in v["runs"].as_array().into_iter().flatten() {
        println!(
            "{}  {:<11} {:>3} steps {:>3} tools {:>8} tokens {:>6.1}s",
            r["created_at"]
                .as_str()
                .unwrap_or_default()
                .get(..19)
                .unwrap_or_default(),
            r["outcome"].as_str().unwrap_or_default(),
            r["steps"],
            r["tool_calls"],
            r["prompt_tokens"].as_i64().unwrap_or(0) + r["completion_tokens"].as_i64().unwrap_or(0),
            r["duration_ms"].as_i64().unwrap_or(0) as f64 / 1000.0
        );
    }
    Ok(())
}

fn memory_cmd(paths: Paths, action: MemoryAction) -> Result<()> {
    let daemon = |req: Request| -> Result<serde_json::Value> {
        formless_core::daemon::ensure_daemon(&paths)?;
        formless_tui::client::request_once(&paths, req)
    };
    let show = |m: &serde_json::Value| {
        println!(
            "{:<10} {:<8} {:<20} {}{}",
            m["id"].as_str().unwrap_or_default(),
            if m["enabled"] == true { "on" } else { "off" },
            m["scope"].as_str().unwrap_or_default(),
            m["text"].as_str().unwrap_or_default(),
            match m["session_id"].as_str() {
                Some(s) => format!("  (from session {})", &s[..s.len().min(8)]),
                None => format!("  ({})", m["author"].as_str().unwrap_or_default()),
            }
        )
    };
    match action {
        MemoryAction::List { json } => {
            let list = daemon(Request::ListMemories)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
                return Ok(());
            }
            let notes = list.as_array().cloned().unwrap_or_default();
            if notes.is_empty() {
                println!("no memories (the agent saves preferences with `remember`; or formless memory add …)");
            }
            notes.iter().for_each(show);
        }
        MemoryAction::Add { text, scope } => show(&daemon(Request::UpsertMemory {
            id: None,
            text: Some(text),
            scope: Some(scope),
            enabled: None,
        })?),
        MemoryAction::Rm { id } => {
            daemon(Request::DeleteMemory { id: id.clone() })?;
            println!("deleted {id}");
        }
        MemoryAction::Edit {
            id,
            text,
            scope,
            disable,
            enable,
        } => show(&daemon(Request::UpsertMemory {
            id: Some(id),
            text,
            scope,
            enabled: (disable || enable).then_some(enable),
        })?),
    }
    Ok(())
}

fn mcp_cmd(paths: Paths, action: McpAction) -> Result<()> {
    let cfg = Config::load(&paths)?;
    let daemon = |req: Request| -> Result<serde_json::Value> {
        formless_core::daemon::ensure_daemon(&paths)?;
        formless_tui::client::request_once(&paths, req)
    };
    let report = |r: &serde_json::Value| match r["error"].as_str() {
        Some(e) => println!("error:   {e}"),
        None => println!("tools:   {}", r["tools"]),
    };
    match action {
        McpAction::Add {
            name,
            params,
            url,
            headers,
            env_keys,
            command,
        } => {
            if url.is_none() && command.is_empty() {
                // A catalog server by id.
                let params: serde_json::Map<String, serde_json::Value> = params
                    .iter()
                    .map(|p| {
                        let (k, v) = p.split_once('=').context("--param takes key=value")?;
                        Ok((k.trim().to_string(), serde_json::json!(v.trim())))
                    })
                    .collect::<Result<_>>()?;
                let r = daemon(Request::AddMcpFromCatalog {
                    id: name.clone(),
                    name: None,
                    params,
                })?;
                println!("added:   {}", r["added"].as_str().unwrap_or(&name));
                report(&r);
                for k in r["env_keys"].as_array().into_iter().flatten() {
                    println!("needs:   {} in the daemon's environment", k.as_str().unwrap_or_default());
                }
                if r["needs_login"] == true {
                    println!("next:    formless mcp login {name}");
                }
                return Ok(());
            }
            let server = match url {
                Some(url) => {
                    let headers: serde_json::Map<String, serde_json::Value> = headers
                        .iter()
                        .map(|h| {
                            let (k, v) = h.split_once('=').context("--header takes Key=Value")?;
                            Ok((k.trim().to_string(), serde_json::json!(v.trim())))
                        })
                        .collect::<Result<_>>()?;
                    serde_json::json!({"transport": "http", "url": url, "headers": headers})
                }
                None => {
                    let (cmd, args) = command
                        .split_first()
                        .context("give the server command after `--`, or --url")?;
                    serde_json::json!({"command": cmd, "args": args, "env_keys": env_keys})
                }
            };
            let r = daemon(Request::AddMcpServer {
                name: name.clone(),
                server,
            })?;
            println!("added:   {name}");
            report(&r);
        }
        McpAction::Remove { name } => {
            let r = daemon(Request::RemoveMcpServer { name: name.clone() })?;
            println!("removed: {name} ({} tool(s) retired)", r["retired_tools"]);
        }
        McpAction::List { json } => {
            let list = daemon(Request::ListMcpServers)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
                return Ok(());
            }
            let servers = list.as_array().cloned().unwrap_or_default();
            if servers.is_empty() {
                println!("no MCP servers configured (formless mcp add …)");
            }
            let mut tbl = Table::new(&[
                "NAME",
                "TRANSPORT",
                "STATE",
                "TOOLS",
                "TARGET",
                "LAST ERROR",
            ]);
            for s in servers {
                let state = match s["state"].as_str().unwrap_or("idle") {
                    "disabled" => t("disabled", Tone::Dim),
                    "ready" => t("ready", Tone::Good),
                    st @ ("failed" | "auth needed") => t(st, Tone::Bad),
                    st => c(st),
                };
                tbl.row(vec![
                    t(s["name"].as_str().unwrap_or_default(), Tone::Strong),
                    c(s["transport"].as_str().unwrap_or_default()),
                    state,
                    c(s["tool_count"].to_string()),
                    t(s["target"].as_str().unwrap_or_default(), Tone::Dim),
                    t(s["last_error"].as_str().unwrap_or_default(), Tone::Bad),
                ]);
            }
            if !tbl.is_empty() {
                tbl.print();
            }
        }
        McpAction::Catalog => {
            let list = daemon(Request::McpCatalog)?;
            let mut tbl = Table::new(&["ID", "SERVER", "WHAT IT DOES", "NEEDS"]);
            for e in list.as_array().into_iter().flatten() {
                let mut needs: Vec<String> = e["params"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p["name"].as_str().map(|n| format!("--param {n}=…")))
                    .collect();
                needs.extend(
                    e["env_keys"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|k| k.as_str().map(|k| format!("${k}"))),
                );
                if e["transport"] == "http" {
                    needs.push("login".into());
                }
                let id = e["id"].as_str().unwrap_or_default();
                tbl.row(vec![
                    t(id, if e["added"] == true { Tone::Dim } else { Tone::Strong }),
                    c(e["title"].as_str().unwrap_or_default()),
                    t(e["description"].as_str().unwrap_or_default(), Tone::Dim),
                    c(needs.join(" ")),
                ]);
            }
            tbl.print();
            println!("add one with: formless mcp add ID [--param key=value]");
        }
        McpAction::Import { yes } => {
            let cwd = std::env::current_dir()
                .ok()
                .map(|d| d.to_string_lossy().into_owned());
            let list = daemon(Request::McpImportCandidates { cwd })?;
            let found = list.as_array().cloned().unwrap_or_default();
            if found.is_empty() {
                println!("no MCP servers found in Claude Desktop, Cursor or VS Code configs");
                return Ok(());
            }
            use std::io::{BufRead, IsTerminal, Write};
            let interactive = std::io::stdin().is_terminal();
            for cand in found {
                let name = cand["name"].as_str().unwrap_or_default().to_string();
                println!(
                    "\n{name}  (from {})\n  {}",
                    cand["source"].as_str().unwrap_or_default(),
                    cand["target"].as_str().unwrap_or_default()
                );
                let env: Vec<&str> = cand["env"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.as_str())
                    .collect();
                if !env.is_empty() {
                    println!("  env: {}", env.join(", "));
                }
                for m in cand["missing_env"].as_array().into_iter().flatten() {
                    println!("  not imported: {} (a placeholder; set it yourself)", m.as_str().unwrap_or_default());
                }
                if cand["exists"] == true {
                    println!("  skipped: a server named {name} is already configured");
                    continue;
                }
                let add = if yes {
                    true
                } else if interactive {
                    print!("  add it? [y/N] ");
                    std::io::stdout().flush()?;
                    let mut line = String::new();
                    std::io::stdin().lock().read_line(&mut line)?;
                    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
                } else {
                    println!("  (rerun with --yes to add, or from a terminal to choose)");
                    false
                };
                if add {
                    let r = daemon(Request::AddMcpServer {
                        name: name.clone(),
                        server: cand["server"].clone(),
                    })?;
                    println!("  added:   {name}");
                    report(&r);
                }
            }
        }
        McpAction::Log { name, lines } => {
            let r = daemon(Request::McpServerLog {
                name,
                lines: Some(lines),
            })?;
            let tail = r["lines"].as_array().cloned().unwrap_or_default();
            if tail.is_empty() {
                println!("(no output in {})", r["file"].as_str().unwrap_or_default());
            }
            for l in tail {
                println!("{}", l.as_str().unwrap_or_default());
            }
        }
        McpAction::Tools { name } => {
            let r = daemon(Request::ListMcpTools { name: name.clone() })?;
            let mut tbl = Table::new(&["TOOL", "ON", "TOKENS", "DESCRIPTION"]);
            for tool in r["tools"].as_array().into_iter().flatten() {
                let on = tool["enabled"] == true;
                tbl.row(vec![
                    t(tool["name"].as_str().unwrap_or_default(), if on { Tone::Strong } else { Tone::Dim }),
                    if on { t("on", Tone::Good) } else { t("off", Tone::Dim) },
                    c(tool["tokens"].to_string()),
                    t(tool["description"].as_str().unwrap_or_default().lines().next().unwrap_or_default(), Tone::Dim),
                ]);
            }
            tbl.print();
            println!("tool list cost: ~{} tokens per request (enabled tools)", r["enabled_tokens"]);
        }
        McpAction::Tool { name, tool, on, off } => {
            if on == off {
                anyhow::bail!("pass --on or --off");
            }
            daemon(Request::SetMcpToolEnabled {
                name: name.clone(),
                tool: tool.clone(),
                enabled: on,
            })?;
            println!("{name}: {tool} is {}", if on { "on" } else { "off" });
        }
        McpAction::Test { name } => {
            let r = daemon(Request::RestartMcpServer { name: name.clone() })?;
            println!("server:  {name}");
            report(&r);
        }
        McpAction::Login { server } => {
            let scfg = cfg
                .mcp
                .servers
                .get(&server)
                .with_context(|| format!("no MCP server '{server}' in config.toml"))?;
            formless_core::mcp_auth::login(&paths, &server, scfg, |url| {
                println!("Open this URL in a browser on this machine to authorize Formless:\n\n  {url}\n");
                let _ = open_browser(url);
            })?;
            println!("Logged in to '{server}'.");
        }
        McpAction::Logout { server } => {
            formless_core::mcp_auth::logout(&paths, &server);
            println!("Deleted stored OAuth tokens for '{server}'.");
        }
    }
    Ok(())
}

fn naming_cmd(paths: Paths, action: NamingAction) -> Result<()> {
    use formless_core::namer;
    let cfg = Config::load(&paths)?;
    match action {
        NamingAction::Status => {
            println!("enabled:    {}", cfg.naming.enabled);
            println!("model:      {}", naming_model_status(&paths, &cfg));
            println!("models dir: {}", namer::models_dir(&paths).display());
            println!(
                "available:  {}",
                namer::MODELS
                    .iter()
                    .map(|m| format!("{} ({} MB)", m.id, m.gguf_bytes / 1_000_000))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        NamingAction::Pull { model } => {
            let id = model.unwrap_or_else(|| cfg.naming.model.clone());
            let spec = namer::spec(&id).with_context(|| format!("unknown naming model '{id}'"))?;
            if spec.installed(&paths) {
                println!("{} already installed", spec.id);
                return Ok(());
            }
            println!(
                "downloading {} ({} MB)…",
                spec.id,
                spec.gguf_bytes / 1_000_000
            );
            tokio::runtime::Runtime::new()?.block_on(namer::download(&paths, &spec))?;
            println!("installed {}", spec.gguf_path(&paths).display());
            if !id.eq_ignore_ascii_case(&cfg.naming.model) {
                println!(
                    "set `[naming] model = \"{}\"` in config.toml to use it",
                    spec.id
                );
            }
        }
        NamingAction::Test { text } => {
            let spec = cfg.naming.spec();
            let started = std::time::Instant::now();
            let title = namer::title(&paths, spec.as_ref(), &text);
            println!(
                "{}  ({:.2?}, {})",
                title.as_deref().unwrap_or("(none)"),
                started.elapsed(),
                naming_model_status(&paths, &cfg)
            );
        }
    }
    Ok(())
}

fn naming_model_status(paths: &Paths, cfg: &Config) -> String {
    match cfg.naming.spec() {
        _ if !formless_core::namer::embedded_available() => {
            "heuristic (built without embedded-llm)".into()
        }
        Some(spec) if spec.installed(paths) => format!("{} (installed)", spec.id),
        Some(spec) => format!("{} (not installed: `formless naming pull`)", spec.id),
        None => format!("heuristic (unknown model '{}')", cfg.naming.model),
    }
}

/// `tools disable|enable`.
fn set_tool_disabled(paths: &Paths, name: &str, disabled: bool) -> Result<()> {
    use formless_tui::client::request_once;
    let tool = find_tool(paths, name)?;
    request_once(
        paths,
        Request::SetToolDisabled {
            id: tool.id,
            disabled,
        },
    )?;
    if disabled {
        println!(
            "switched off {}: the agent can't find or call it until `formless tools enable {}`",
            tool.name, tool.name
        );
    } else {
        println!("switched on {}", tool.name);
    }
    Ok(())
}

/// `tools pin|unpin`: pinning says what it costs per request.
fn set_tool_pinned(paths: &Paths, name: &str, pinned: bool) -> Result<()> {
    use formless_tui::client::request_once;
    let tool = find_tool(paths, name)?;
    let detail: formless_protocol::ToolDetail = serde_json::from_value(request_once(
        paths,
        Request::GetTool {
            id: tool.id.clone(),
        },
    )?)?;
    request_once(
        paths,
        Request::SetToolPinned {
            id: tool.id,
            pinned,
        },
    )?;
    if pinned {
        println!(
            "pinned {}: every Session lists it to the agent from its next listing, ~{} tokens per request",
            tool.name, detail.listing_tokens
        );
    } else {
        println!("unpinned {}", tool.name);
    }
    Ok(())
}

fn tools_cmd(paths: Paths, action: ToolsAction) -> Result<()> {
    use formless_tui::client::request_once;
    formless_core::daemon::ensure_daemon(&paths)?;
    match action {
        ToolsAction::List { json, tag } => {
            let v = request_once(&paths, Request::ListTools)?;
            let mut tools: Vec<formless_protocol::ToolInfo> = serde_json::from_value(v)?;
            if let Some(tag) = tag {
                let tag = tag.trim_start_matches('#').to_lowercase();
                tools.retain(|t| t.tags.contains(&tag));
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&tools)?);
                return Ok(());
            }
            if tools.is_empty() {
                println!("(no tools yet - the agent builds them via create_tool)");
            }
            let mut tbl = Table::new(&[
                "TOOL",
                "TITLE",
                "TAGS",
                "VER",
                "CALLS",
                "SUCCESS",
                "STATUS",
                "CAPABILITIES",
            ]);
            for tool in tools {
                let status = if tool.retired {
                    t("retired", Tone::Dim)
                } else if tool.disabled {
                    t("off", Tone::Warn)
                } else if tool.needs_attention {
                    t("needs attention", Tone::Bad)
                } else {
                    t("active", Tone::Good)
                };
                let rate = tool.success_rate;
                tbl.row(vec![
                    t(tool.name, Tone::Strong),
                    c(tool.title),
                    t(tool.tags.join(" "), Tone::Dim),
                    c(format!("v{}", tool.current_version)),
                    c(tool.invocations.to_string()),
                    t(
                        rate.map(|r| format!("{:.0}%", r * 100.0))
                            .unwrap_or_else(|| "-".into()),
                        match rate {
                            Some(r) if r < 0.8 => Tone::Bad,
                            Some(_) => Tone::Good,
                            None => Tone::Dim,
                        },
                    ),
                    status,
                    t(tool.capabilities.join(", "), Tone::Dim),
                ]);
            }
            if !tbl.is_empty() {
                tbl.print();
            }
            Ok(())
        }
        ToolsAction::Export { name, all, output } => {
            let bundle = if all {
                request_once(&paths, Request::ExportTools)?
            } else {
                let name = name.ok_or_else(|| anyhow::anyhow!("give a tool name/id or --all"))?;
                // Resolve name -> id via the list.
                let v = request_once(&paths, Request::ListTools)?;
                let tools: Vec<formless_protocol::ToolInfo> = serde_json::from_value(v)?;
                let tool = tools
                    .iter()
                    .find(|t| t.name == name || t.id == name)
                    .ok_or_else(|| anyhow::anyhow!("no tool '{name}'"))?;
                request_once(
                    &paths,
                    Request::ExportTool {
                        id: tool.id.clone(),
                    },
                )?
            };
            let text = serde_json::to_string_pretty(&bundle)?;
            match output {
                Some(f) => {
                    std::fs::write(&f, text)?;
                    println!("wrote {f}");
                }
                None => println!("{text}"),
            }
            Ok(())
        }
        ToolsAction::Show { name, json } => {
            let tool = find_tool(&paths, &name)?;
            let d: formless_protocol::ToolDetail =
                serde_json::from_value(request_once(&paths, Request::GetTool { id: tool.id })?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
                return Ok(());
            }
            print_tool_detail(&d);
            Ok(())
        }
        ToolsAction::Run {
            name,
            input,
            yes,
            session,
            dry_run,
        } => {
            let input: serde_json::Value = match serde_json::from_str(&input) {
                Ok(v @ serde_json::Value::Object(_)) => v,
                _ => {
                    eprintln!("--input must be a JSON object, like '{{\"url\": \"https://…\"}}'");
                    std::process::exit(2);
                }
            };
            let tool = find_tool(&paths, &name)?;
            let code = tool_run(&paths, tool, input, yes, session, dry_run)?;
            std::process::exit(code);
        }
        ToolsAction::Undo => {
            let v = request_once(&paths, Request::UndoRegistryChange)?;
            println!("undone: {}", v["undone"].as_str().unwrap_or_default());
            Ok(())
        }
        ToolsAction::Disable { name } => set_tool_disabled(&paths, &name, true),
        ToolsAction::Enable { name } => set_tool_disabled(&paths, &name, false),
        ToolsAction::Pin { name } => set_tool_pinned(&paths, &name, true),
        ToolsAction::Unpin { name } => set_tool_pinned(&paths, &name, false),
        ToolsAction::Tag { name, tags } => {
            let tool = find_tool(&paths, &name)?;
            let t: formless_protocol::ToolInfo = serde_json::from_value(request_once(
                &paths,
                Request::SetToolTags { id: tool.id, tags },
            )?)?;
            if t.tags.is_empty() {
                println!("{}: no tags", t.name);
            } else {
                println!("{}: {}", t.name, t.tags.join(" "));
            }
            Ok(())
        }
        ToolsAction::Retire { name } => set_tool_retired(&paths, &name, true),
        ToolsAction::Restore { name } => set_tool_retired(&paths, &name, false),
        ToolsAction::Rollback { name, to } => {
            let tool = find_tool(&paths, &name)?;
            let version = to.unwrap_or(tool.current_version - 1);
            if version < 1 || version == tool.current_version {
                eprintln!(
                    "{} is at v{}; give --to with an earlier version",
                    tool.name, tool.current_version
                );
                std::process::exit(2);
            }
            request_once(
                &paths,
                Request::RollbackTool {
                    id: tool.id.clone(),
                    version,
                },
            )?;
            println!("{} now runs v{version}", tool.name);
            Ok(())
        }
        ToolsAction::Purge { name, force } => {
            use std::io::{BufRead, IsTerminal, Write};
            let tool = find_tool(&paths, &name)?;
            let dependents = tool_dependents(&paths, &tool.id);
            if !dependents.is_empty() {
                eprintln!(
                    "warning: {} is used by {}; their calls to it will fail",
                    tool.name,
                    dependents.join(", ")
                );
            }
            if !force {
                if !std::io::stdin().is_terminal() {
                    eprintln!(
                        "purge deletes {} for good; pass --force to confirm without a terminal",
                        tool.name
                    );
                    std::process::exit(2);
                }
                eprint!(
                    "This deletes {} and all its versions and history. Type its name to confirm: ",
                    tool.name
                );
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                std::io::stdin().lock().read_line(&mut line)?;
                if line.trim() != tool.name {
                    eprintln!("aborted");
                    std::process::exit(2);
                }
            }
            request_once(
                &paths,
                Request::PurgeTool {
                    id: tool.id.clone(),
                },
            )?;
            println!("purged {}", tool.name);
            Ok(())
        }
        ToolsAction::Import { file, yes } => {
            let bundle: serde_json::Value = if file.contains("://") {
                let fetched = request_once(&paths, Request::FetchToolBundle { url: file })?;
                print_bundle_preview(&fetched);
                if !yes && !confirm(
                    "Import it? Imported Tools still ask before their first sensitive call. [y/N] ",
                )? {
                    eprintln!("not imported");
                    std::process::exit(2);
                }
                fetched["bundle"].clone()
            } else {
                serde_json::from_str(&std::fs::read_to_string(&file)?)?
            };
            let v = request_once(&paths, Request::ImportTool { bundle })?;
            println!("imported: {v}");
            Ok(())
        }
    }
}

/// What a fetched bundle holds, in full, before anything is imported
/// (SPEC §7.8): its source, and each Tool's Capabilities and code.
fn print_bundle_preview(fetched: &serde_json::Value) {
    let b = &fetched["bundle"];
    let tools: Vec<&serde_json::Value> = match b["tools"].as_array() {
        Some(list) => list.iter().collect(),
        None => vec![b],
    };
    print!("source: {}", fetched["source"].as_str().unwrap_or_default());
    match fetched["file"].as_str() {
        Some(f) => println!(" ({f})"),
        None => println!(),
    }
    println!("{} Tool(s):", tools.len());
    for t in tools {
        let caps: Vec<&str> = t["capabilities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c.as_str())
            .collect();
        println!();
        println!(
            "── {} ──  {}",
            t["name"].as_str().unwrap_or("?"),
            t["description"].as_str().unwrap_or_default()
        );
        println!(
            "capabilities: {}",
            if caps.is_empty() {
                "none".to_string()
            } else {
                caps.join(", ")
            }
        );
        println!("memory: {} MB", t["memory_mb"].as_u64().unwrap_or(256));
        println!("code:");
        for line in t["implementation_js"].as_str().unwrap_or_default().lines() {
            println!("  {line}");
        }
    }
    println!();
}

/// A yes/no question on the terminal; no terminal means no.
fn confirm(question: &str) -> Result<bool> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        eprintln!("(no terminal to confirm on: pass --yes to import without asking)");
        return Ok(false);
    }
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Tools whose code calls this one (SPEC §10.4).
fn tool_dependents(paths: &Paths, id: &str) -> Vec<String> {
    formless_tui::client::request_once(paths, Request::GetTool { id: id.into() })
        .ok()
        .and_then(|v| serde_json::from_value(v["used_by"].clone()).ok())
        .unwrap_or_default()
}

/// Retire or restore a Tool by name or id. Retiring one that others call
/// warns, and asks on a terminal.
fn set_tool_retired(paths: &Paths, name: &str, retire: bool) -> Result<()> {
    let tool = find_tool(paths, name)?;
    let id = tool.id.clone();
    let dependents = if retire {
        tool_dependents(paths, &id)
    } else {
        Vec::new()
    };
    if !dependents.is_empty() {
        eprintln!(
            "warning: {} is used by {}; their calls to it will fail until it is restored",
            tool.name,
            dependents.join(", ")
        );
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() && !confirm("Retire it anyway? [y/N] ")? {
            eprintln!("not retired");
            std::process::exit(2);
        }
    }
    formless_tui::client::request_once(
        paths,
        if retire {
            Request::RetireTool { id }
        } else {
            Request::RestoreTool { id }
        },
    )?;
    println!(
        "{} {}",
        if retire { "retired" } else { "restored" },
        tool.name
    );
    Ok(())
}

/// One daemon log entry: its first line starts with an RFC 3339 time and
/// a level; continuation lines (backtraces, wrapped text) belong to it.
struct LogEntry {
    at: Option<chrono::DateTime<chrono::FixedOffset>>,
    level: u8,
    text: String,
}

/// Severity: 0 error … 4 trace; lines without a level count as info.
fn log_level(word: &str) -> Option<u8> {
    Some(match word {
        "ERROR" | "error" => 0,
        "WARN" | "warn" => 1,
        "INFO" | "info" => 2,
        "DEBUG" | "debug" => 3,
        "TRACE" | "trace" => 4,
        _ => return None,
    })
}

/// Group log text into entries.
fn parse_log(text: &str) -> Vec<LogEntry> {
    let mut out: Vec<LogEntry> = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let at = words
            .next()
            .and_then(|w| chrono::DateTime::parse_from_rfc3339(w).ok());
        match at {
            Some(at) => out.push(LogEntry {
                at: Some(at),
                level: words.next().and_then(log_level).unwrap_or(2),
                text: line.to_string(),
            }),
            None => match out.last_mut() {
                Some(e) => {
                    e.text.push('\n');
                    e.text.push_str(line);
                }
                None => out.push(LogEntry {
                    at: None,
                    level: 2,
                    text: line.to_string(),
                }),
            },
        }
    }
    out
}

/// `formless logs`: the rotated files oldest first, then daemon.log,
/// filtered by level and age; `-f` keeps reading (and follows rotation).
fn logs_cmd(
    paths: &Paths,
    follow: bool,
    level: Option<String>,
    since: Option<std::time::Duration>,
    lines: usize,
) -> Result<()> {
    let max_level = level.as_deref().and_then(log_level).unwrap_or(4);
    let cutoff =
        since.map(|d| chrono::Utc::now() - chrono::Duration::from_std(d).unwrap_or_default());
    let keep = |e: &LogEntry| {
        e.level <= max_level
            && match (cutoff, e.at) {
                (Some(c), Some(at)) => at >= c,
                (Some(_), None) => false,
                _ => true,
            }
    };
    let live = paths.log_file();
    let mut files: Vec<std::path::PathBuf> = (1..=20)
        .map(|n| std::path::PathBuf::from(format!("{}.{n}", live.display())))
        .filter(|p| p.exists())
        .collect();
    files.reverse();
    files.push(live.clone());
    let text: String = files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .collect::<Vec<_>>()
        .join("\n");
    let entries: Vec<LogEntry> = parse_log(&text).into_iter().filter(|e| keep(e)).collect();
    let skip = if lines == 0 {
        0
    } else {
        entries.len().saturating_sub(lines)
    };
    for e in &entries[skip..] {
        println!("{}", e.text);
    }
    if entries.is_empty() && !follow {
        eprintln!(
            "no log entries{} in {}",
            if level.is_some() || since.is_some() {
                " match"
            } else {
                ""
            },
            live.display()
        );
    }
    if !follow {
        return Ok(());
    }
    // Follow: print what's appended; a smaller file means it was rotated.
    let mut pos = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let len = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
        if len < pos {
            pos = 0;
        }
        if len > pos {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(&live)?;
            f.seek(SeekFrom::Start(pos))?;
            let mut new = String::new();
            f.read_to_string(&mut new)?;
            pos = len;
            for e in parse_log(&new).into_iter().filter(|e| keep(e)) {
                println!("{}", e.text);
            }
        }
    }
}

/// The Tool with this name or id.
fn find_tool(paths: &Paths, name: &str) -> Result<formless_protocol::ToolInfo> {
    let tools: Vec<formless_protocol::ToolInfo> = serde_json::from_value(
        formless_tui::client::request_once(paths, Request::ListTools)?,
    )?;
    tools
        .into_iter()
        .find(|t| t.name == name || t.id == name)
        .ok_or_else(|| anyhow::anyhow!("no tool '{name}' (formless tools list)"))
}

fn print_tool_detail(d: &formless_protocol::ToolDetail) {
    let t = &d.tool;
    let status = if t.retired {
        "retired"
    } else if t.disabled {
        "switched off"
    } else if t.needs_attention {
        "needs attention"
    } else {
        "active"
    };
    println!("{} v{}  [{status}]", t.name, t.current_version);
    if !t.title.is_empty() {
        println!("{}", t.title);
    }
    if !t.tags.is_empty() {
        println!("tags: {}", t.tags.join(" "));
    }
    if !d.uses.is_empty() || !d.modules.is_empty() {
        println!(
            "uses:         {}",
            d.uses
                .iter()
                .cloned()
                .chain(d.modules.iter().map(|m| format!("module {m}")))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !d.used_by.is_empty() {
        println!("used by:      {}", d.used_by.join(", "));
    }
    println!("{}", t.description);
    println!();
    println!(
        "capabilities: {}",
        if t.capabilities.is_empty() {
            "none".into()
        } else {
            t.capabilities.join(", ")
        }
    );
    println!("memory:       {} MB", t.memory_mb);
    let m = &d.metrics;
    println!(
        "calls:        {} ({} ok, {} failed, {} denied) · success {} · p50 {}",
        m.total,
        m.success,
        m.failure,
        m.denied,
        m.success_rate
            .map(|r| format!("{:.0}%", r * 100.0))
            .unwrap_or_else(|| "-".into()),
        m.p50_ms
            .map(|v| format!("{v:.0}ms"))
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "latency:      p50 {} · p95 {}",
        m.p50_ms
            .map(|v| format!("{v:.0}ms"))
            .unwrap_or_else(|| "-".into()),
        m.p95_ms
            .map(|v| format!("{v:.0}ms"))
            .unwrap_or_else(|| "-".into())
    );
    let model_use = formless_tui::model_use_text(m);
    if !model_use.is_empty() {
        println!("model use:    {model_use}");
    }
    for reason in &d.attention {
        println!("attention:    {reason}");
    }
    println!(
        "versions:     {}",
        d.versions
            .iter()
            .map(|v| format!(
                "v{} {} {}",
                v.version,
                v.author,
                v.created_at.get(..10).unwrap_or(&v.created_at)
            ))
            .collect::<Vec<_>>()
            .join(" · ")
    );
    if let Some(v) = d.versions.iter().find(|v| v.version == t.current_version) {
        let pretty = |s: &str| {
            serde_json::from_str::<serde_json::Value>(s)
                .ok()
                .and_then(|v| serde_json::to_string_pretty(&v).ok())
                .unwrap_or_else(|| s.to_string())
        };
        println!("\ninput schema:\n{}", pretty(&v.input_schema));
        println!("\noutput schema:\n{}", pretty(&v.output_schema));
        if let Some(ex) = &d.example {
            println!("\nexample (the latest successful call, v{}):", ex.version);
            println!("input:\n{}", pretty(&ex.input));
            println!("output:\n{}", pretty(&ex.output));
        }
    }
}

/// `tools run`: run a Tool directly through the daemon (`RunTool`), the
/// way the agent would. Approval requests are answered here: once with
/// `--yes`, else denied (fail closed). Exit 0 on success, 1 on a failed
/// or denied call.
fn tool_run(
    paths: &Paths,
    tool: formless_protocol::ToolInfo,
    input: serde_json::Value,
    yes: bool,
    session: Option<String>,
    dry_run: bool,
) -> Result<i32> {
    let (client, rx) = formless_tui::client::DaemonClient::connect(paths)?;
    let wait = |id: u64| -> Result<serde_json::Value> {
        loop {
            match rx.recv()? {
                ServerMessage::Response {
                    id: rid,
                    ok,
                    result,
                    error,
                } if rid == id => {
                    return if ok {
                        Ok(result.unwrap_or_default())
                    } else {
                        Err(anyhow::anyhow!(error.unwrap_or_default()))
                    };
                }
                _ => {}
            }
        }
    };
    let (session_id, ephemeral) = match session {
        Some(id) => (find_session(paths, &id)?.id, false),
        None => {
            let id = client.send(Request::CreateSession {
                name: Some(format!("tools run {}", tool.name)),
            });
            (
                wait(id)?["id"].as_str().unwrap_or_default().to_string(),
                true,
            )
        }
    };
    let run = client.send(if dry_run {
        Request::DryRunTool {
            id: tool.id.clone(),
            input,
            session_id: session_id.clone(),
        }
    } else {
        Request::RunTool {
            id: tool.id.clone(),
            input,
            session_id: session_id.clone(),
        }
    });
    let result = loop {
        match rx.recv()? {
            ServerMessage::Event(Event::ApprovalRequested {
                approval_id,
                tool_name,
                capabilities,
                ..
            }) => {
                eprintln!(
                    "[approval] {tool_name} wants {} -> {}",
                    capabilities.join(", "),
                    if yes {
                        "approved once (--yes)"
                    } else {
                        "DENIED (fail closed; pass --yes)"
                    }
                );
                client.send(Request::ApprovalDecision {
                    remember_denial: Default::default(),
                    paths: Default::default(),
                    approval_id,
                    approved: yes,
                    scope: ApprovalScope::Once,
                });
            }
            ServerMessage::Response {
                id,
                ok,
                result,
                error,
            } if id == run => {
                break if ok {
                    Ok(result.unwrap_or_default())
                } else {
                    Err(error.unwrap_or_default())
                };
            }
            _ => {}
        }
    };
    // The throwaway Session stays out of the way (kept, archived).
    if ephemeral {
        let id = client.send(Request::SetSessionArchived {
            id: session_id,
            archived: true,
        });
        let _ = wait(id);
    }
    match result {
        Ok(v) if dry_run => {
            let effects: Vec<&str> = v["effects"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e.as_str())
                .collect();
            eprintln!("dry run: nothing was changed.");
            if effects.is_empty() {
                eprintln!("it would have had no side effects");
            } else {
                eprintln!("it would have:");
                for e in &effects {
                    eprintln!("  - {e}");
                }
            }
            if v["ok"] == true {
                println!("{}", serde_json::to_string_pretty(&v["result"])?);
                Ok(0)
            } else {
                eprintln!("error: {}", v["error"].as_str().unwrap_or_default());
                Ok(1)
            }
        }
        Ok(v) => {
            let text = v["result"].as_str().unwrap_or_default().to_string();
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(j) => println!("{}", serde_json::to_string_pretty(&j)?),
                Err(_) => println!("{text}"),
            }
            // The agent's result text marks failures with "error:".
            Ok(if text.trim_start().starts_with("error") {
                1
            } else {
                0
            })
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(1)
        }
    }
}

fn reset_cmd(paths: Paths, force: bool) -> Result<()> {
    if !force {
        eprint!("This deletes ALL Formless data (config, API key, tools, sessions). Type 'reset' to confirm: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim() != "reset" {
            eprintln!("aborted");
            std::process::exit(2);
        }
    }
    if daemon_reachable(&paths) {
        let _ = formless_tui::client::request_once(&paths, Request::Shutdown);
        for _ in 0..50 {
            if !daemon_reachable(&paths) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    formless_core::config::reset_all(&paths)?;
    println!("reset complete");
    Ok(())
}

/// Ask one `ask_user` question on the terminal (SPEC §5.5): option
/// numbers pick options, anything else is the user's own answer.
fn ask_on_terminal(q: &Question) -> QuestionAnswer {
    match &q.header {
        Some(h) => eprintln!("[question] [{h}] {}", q.question),
        None => eprintln!("[question] {}", q.question),
    }
    for (i, o) in q.options.iter().enumerate() {
        match &o.description {
            Some(d) => eprintln!("  {}) {} — {d}", i + 1, o.label),
            None => eprintln!("  {}) {}", i + 1, o.label),
        }
    }
    if q.multi_select && !q.options.is_empty() {
        eprintln!("  (several allowed, e.g. 1,3)");
    }
    eprint!("> ");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    q.parse_reply(&line)
}

#[cfg(test)]
mod tests {

    #[test]
    fn run_summary_line() {
        let mut s = RunSummary::default();
        s.tool_started("fetch_weather");
        s.tool_started("fetch_weather");
        s.tool_started("create_tool");
        s.tools_created = 1;
        s.usage = formless_protocol::UsageInfo {
            steps: 3,
            prompt_tokens: 12_400,
            completion_tokens: 610,
            cost: Some(0.01234),
            ..Default::default()
        };
        assert_eq!(
            s.line("completed", std::time::Duration::from_secs(75)),
            "completed in 1m15s · 3 steps · Tools: 1 created, 1 used · 13.0k tokens (12.4k in, 610 out) · $0.0123"
        );
        let empty = RunSummary::default().line("paused", std::time::Duration::from_secs(2));
        assert_eq!(empty, "paused in 2s · 0 steps · 0 tokens (0 in, 0 out)");
    }

    #[test]
    fn update_lists_newer_releases_oldest_first() {
        let releases = serde_json::json!([
            {"tag_name": "v0.3.0", "body": "New: run --timeout", "html_url": "u3"},
            {"tag_name": "v0.2.1-rc1", "prerelease": true, "body": "rc", "html_url": "x"},
            {"tag_name": "v0.2.0", "body": "New: sessions CLI", "html_url": "u2"},
            {"tag_name": "v0.4.0", "draft": true, "body": "draft", "html_url": "x"},
            {"tag_name": "v0.1.0", "body": "first", "html_url": "u1"},
        ]);
        let newer = newer_releases(&releases, "0.1.0");
        let tags: Vec<&str> = newer.iter().map(|(t, _, _)| t.as_str()).collect();
        assert_eq!(tags, ["v0.2.0", "v0.3.0"]);
        assert_eq!(newer[1].1, "New: run --timeout");
        assert!(newer_releases(&releases, "0.3.0").is_empty());
    }

    #[test]
    fn bug_reports_redact_secrets() {
        let home = std::env::var("HOME").unwrap_or_default();
        let text = format!(
            "Authorization: Bearer abc.def.ghi\n\
             api_key = \"sk-live-1234567890abcdef\"\n\
             open https://formless.example/#k=c2VjcmV0c2VjcmV0c2VjcmV0\n\
             http://127.0.0.1:5000/#local=0123abcd&s=x\n\
             proxy https://user:hunter2@proxy.example:8080/v1\n\
             token ghp_ABCDEFGHIJKLMNOPQRSTUV\n\
             blob {}\n\
             path {home}/projects/app\n\
             session 79d338f2-4f6c-4116-b2d7-3043bbfd084b ok",
            "x".repeat(48)
        );
        let r = redact(&text);
        for secret in [
            "abc.def.ghi",
            "sk-live",
            "c2VjcmV0",
            "0123abcd",
            "hunter2",
            "ABCDEFGHIJ",
            &"x".repeat(48),
        ] {
            assert!(!r.contains(secret), "{secret} leaked:\n{r}");
        }
        if !home.is_empty() {
            assert!(r.contains("path ~/projects/app"), "{r}");
        }
        // Ordinary ids and words survive.
        assert!(r.contains("79d338f2-4f6c-4116-b2d7-3043bbfd084b ok"), "{r}");
    }

    #[test]
    fn log_lines_group_into_entries_with_levels() {
        let text = "2026-10-23T10:00:00.1Z  INFO formless_core::daemon: listening\n\
                    2026-10-23T10:00:01.2Z  WARN formless_core::daemon: MCP server 'x': failed\n\
                    \x20 caused by: exit 1\n\
                    2026-10-23T10:00:02.3Z ERROR formless_core::relay: boom";
        let entries = parse_log(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries.iter().map(|e| e.level).collect::<Vec<_>>(),
            [2, 1, 0]
        );
        assert!(entries[1].text.ends_with("caused by: exit 1"));
        assert!(entries[0].at.is_some());
    }

    #[test]
    fn durations_parse_and_print() {
        let d = |s: &str| parse_duration(s).map(|d| d.as_secs());
        assert_eq!(d("90"), Ok(90));
        assert_eq!(d("90s"), Ok(90));
        assert_eq!(d("10m"), Ok(600));
        assert_eq!(d("1h30m"), Ok(5400));
        assert!(d("10x").is_err() && d("m").is_err() && d("1h3").is_err() && d("0s").is_err());
        assert_eq!(
            format_duration(std::time::Duration::from_secs(5400)),
            "1h30m"
        );
        assert_eq!(format_duration(std::time::Duration::from_secs(45)), "45s");
    }

    #[test]
    fn attachments_are_read_and_checked_up_front() {
        let dir = std::env::temp_dir().join(format!("fl-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let note = dir.join("notes.md");
        std::fs::write(&note, "# hi").unwrap();
        let files = read_attachments(&[note.clone()]).unwrap();
        assert_eq!(files, vec![("notes.md".to_string(), b"# hi".to_vec())]);
        let missing = read_attachments(&[dir.join("nope.png")]).unwrap_err();
        assert!(missing.starts_with("cannot attach"), "{missing}");
        let big = dir.join("big.bin");
        std::fs::write(
            &big,
            vec![0u8; formless_core::daemon::MAX_ATTACHMENT_BYTES + 1],
        )
        .unwrap();
        assert!(read_attachments(&[big])
            .unwrap_err()
            .contains("larger than 20 MB"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_picks_a_model_effort_or_role() {
        let dir = std::env::temp_dir().join(format!("fl-runmodel-{}", std::process::id()));
        let paths = Paths {
            data_dir: dir.join("d"),
            config_dir: dir.join("c"),
            profile: "test-runmodel".into(),
        };
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut cfg = Config::default();
        cfg.roles.insert("fast".into(), Default::default());
        cfg.save(&paths).unwrap();
        assert_eq!(run_model(&paths, None, None, None).unwrap(), None);
        assert_eq!(
            run_model(&paths, Some("gpt-x".into()), Some("low".into()), None).unwrap(),
            Some((Some("gpt-x".into()), Some("low".into())))
        );
        assert_eq!(
            run_model(&paths, None, None, Some("fast".into())).unwrap(),
            Some((Some("role:fast".into()), None))
        );
        assert!(run_model(&paths, None, None, Some("nope".into()))
            .unwrap_err()
            .contains("no role 'nope'"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_takes_its_task_or_an_attachment_from_stdin() {
        let piped = |s: &str| Some(s.as_bytes().to_vec());
        // `-`: the task is stdin.
        assert_eq!(
            run_input("-".into(), piped("summarise this\n")).unwrap(),
            ("summarise this".to_string(), None)
        );
        assert!(run_input("-".into(), None).is_err());
        assert!(run_input("-".into(), piped("  \n")).is_err());
        // A task with piped input: the input rides along.
        assert_eq!(
            run_input("review this".into(), piped("diff --git a b")).unwrap(),
            ("review this".to_string(), Some(b"diff --git a b".to_vec()))
        );
        // Nothing (or only blanks) piped: no attachment.
        assert_eq!(
            run_input("hi".into(), None).unwrap(),
            ("hi".to_string(), None)
        );
        assert_eq!(
            run_input("hi".into(), piped("\n")).unwrap(),
            ("hi".to_string(), None)
        );
    }

    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn version_comparison_handles_tags() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.1"), Some((0, 1, 0)));
        assert_eq!(compare_versions("0.1.0", "v0.2.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("1.0.0", "v1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("2.0.0", "v1.9.9"), Some(Ordering::Greater));
        assert_eq!(parse_version("nope"), None);
    }
}
