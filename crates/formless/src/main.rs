mod cli_artifacts;
mod cli_attach;
mod cli_commands;
#[cfg(test)]
mod cli_help_tests;
mod cli_mcp_prompts;
mod cli_mcp_test;
mod cli_metrics;
mod cli_models;
mod cli_modules;
mod cli_pipeline;
mod cli_price_import;
mod cli_prices;
mod cli_proposal_diff;
mod cli_provider_test;
mod cli_routing;
mod cli_run_identity;
mod cli_run_memories;
mod cli_run_report;
#[cfg(test)]
mod cli_runs_filter_tests;
mod cli_session_lookup;
mod cli_session_org;
mod cli_session_report;
mod cli_session_templates;
mod cli_session_usage;
mod cli_setup;
mod cli_subagents;
mod cli_tool_detail;
mod cli_tool_diff;
mod cli_tool_filters;
mod cli_topic_mcp;

mod cli_runs_list;
mod cli_tool_lookup;
mod cli_tool_run_input;
mod cli_tool_success;
mod cli_update;
mod cli_usage;
#[cfg(test)]
mod session_export_tests;
mod table;
mod update_http;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use formless_core::daemon::{daemon_reachable, Daemon};
use formless_core::provider::probe_blocking;
use formless_core::{Config, Paths};
use formless_protocol::{
    ApprovalScope, Event, Question, QuestionAnswer, Request, SendMode, ServerMessage, SessionInfo,
};
use formless_tui::local_timestamp;
use serde_json::Value;
use std::io::Write;
use std::sync::Arc;
use table::{c, t, Table, Tone};

/// File coverage is diagnostic output; stdout remains the requested Run result.
fn write_run_file_notice(
    out: &mut impl Write,
    files: &[formless_protocol::RunFileInfo],
    untracked: bool,
) {
    let _ = writeln!(out, "{}", formless_tui::run_files_text(files, untracked));
}

fn parse_profile_name(value: &str) -> std::result::Result<String, String> {
    formless_core::paths::validate_profile_name(value).map_err(|error| error.to_string())?;
    Ok(value.to_owned())
}

#[derive(Parser)]
#[command(
    name = "formless",
    version,
    about = "General-purpose agent harness (SPEC.md)",
    after_help = "Examples:\n  formless\n  formless chat --new\n  formless run \"Summarize README.md\" --estimate\n  formless sessions list --json\n  formless sessions subagents SESSION --json\n  formless sessions subagent EXACT_SUBAGENT_ID"
)]
struct Cli {
    /// Isolated profile (separate daemon + data dir)
    #[arg(long, global = true, value_parser = parse_profile_name)]
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

// Root commands are alphabetical; Clap appends its generated help command.
// Keep complete variants in this order so help, man and completions agree.
#[derive(Subcommand)]
enum Cmd {
    #[command(hide = true)]
    TopicMcp {
        #[arg(long)]
        attempt: String,
    },
    /// Every approval grant, and revoking them
    #[command(
        after_help = "Examples:\n  formless approvals pending\n  formless approvals list --json"
    )]
    Approvals {
        #[command(subcommand)]
        action: ApprovalsAction,
    },
    /// Read stored artifact text by ID (decoded text, not a binary download)
    #[command(
        after_help = "Examples:\n  formless artifacts show ARTIFACT_ID\n  formless artifacts show ARTIFACT_ID --offset 0 --length 128 --json\n\nOffsets and lengths count Unicode scalar values; invalid UTF-8 is replaced. The daemon reads and decodes the full source before slicing; a small page does not make that read streaming. JSON bytes is stored byte metadata, not slice length."
    )]
    Artifacts {
        #[command(subcommand)]
        action: cli_artifacts::Action,
    },
    /// Provider credential login/status/logout (API key or OAuth)
    #[command(
        after_help = "Examples:\n  formless auth status\n  formless auth status --provider local --json"
    )]
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Back up the database now (kept with the daily ones in <data>/backups)
    #[command(
        after_help = "Examples:\n  formless backup list\n  formless backup verify backup.sqlite\n  formless backup drill backup.sqlite"
    )]
    Backup {
        #[command(subcommand)]
        action: Option<BackupAction>,
    },
    /// Write a redacted bug report (doctor, versions, OS, recent log) to attach to an issue
    BugReport {
        /// Where to write it (default: ./formless-bug-report-<time>.md)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
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
    /// List local user-defined slash commands without starting a daemon
    #[command(after_help = "Examples:\n  formless commands list\n  formless commands list --json")]
    Commands {
        #[command(subcommand)]
        action: cli_commands::Action,
    },
    /// Print a shell completion script (bash, zsh, fish, powershell, elvish)
    Completions {
        /// Shell that will load the generated completion script
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Settings: get, set (validated by the daemon), edit or locate config.toml
    #[command(
        after_help = "Examples:\n  formless config path\n  formless config list --filter limits\n  formless config list --json\n  formless config get provider.model\n  formless config get provider.model --json"
    )]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Manage the background daemon and its login service
    #[command(after_help = "Examples:\n  formless daemon status\n  formless daemon status --json")]
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Open an online guide or read this build's bundled Markdown
    Docs {
        /// Guide name to open; omit to open the gallery
        #[arg(value_parser = DocsTopicParser::new())]
        topic: Option<String>,
        /// Print the current online URL (may differ from this build)
        #[arg(long)]
        print: bool,
        /// Print bundled Markdown without a browser, config or daemon
        #[arg(long, conflicts_with_all = ["print", "keys"])]
        text: bool,
        /// Print the keyboard and command reference as Markdown
        /// (docs/keys.md)
        #[arg(long, conflicts_with_all = ["topic", "print", "text"])]
        keys: bool,
    },
    /// Diagnostics: config, dirs, keychain, endpoint probe
    Doctor {
        /// Rehearse newest backup in an isolated offline profile, then exit
        #[arg(long, conflicts_with = "fix")]
        restore_drill: bool,
        /// Apply the safe fixes: clear a stale socket or lock, restart a
        /// stuck daemon, tighten secret file permissions, then check again
        #[arg(long)]
        fix: bool,
        /// A machine-readable report, secrets redacted
        #[arg(long, conflicts_with = "fix")]
        json: bool,
    },
    /// Everything for a new machine in one file: Tools, modules, Skills,
    /// memories, jobs, providers/Roles/MCP servers (no keys or secrets)
    ExportAll {
        /// Include every Session's transcript
        #[arg(long)]
        sessions: bool,
        /// Where to write it (default: ./formless-export-<date>.json)
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
        /// Replace an existing file at the explicit output path
        #[arg(long, requires = "output")]
        overwrite: bool,
    },
    /// Free disk space: old artifacts and snapshots, long MCP logs, old
    /// migration copies, database free space
    Gc {
        /// List what would be freed without removing anything
        #[arg(long)]
        dry_run: bool,
        /// Print the complete reported GC result as one JSON value
        #[arg(long)]
        json: bool,
    },
    /// Load a file from `formless export-all` (adds; never overwrites)
    ImportAll {
        /// Path to a bundle produced by export-all
        file: std::path::PathBuf,
    },
    /// Scheduled jobs: prompts that run later or on a cron (SPEC §9.7)
    #[command(
        after_help = "Examples:\n  formless jobs list\n  formless jobs preview \"every weekday at 9am\"\n  formless jobs history JOB_ID --limit 50 --json"
    )]
    Jobs {
        #[command(subcommand)]
        action: JobsAction,
    },
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
        /// Print one JSON object per selected entry (snapshot only)
        #[arg(long, conflicts_with = "follow")]
        jsonl: bool,
    },
    /// Print the man page (roff), for packaging: `formless man > formless.1`
    #[command(hide = true)]
    Man,
    /// External MCP servers (SPEC §7.9)
    #[command(
        after_help = "Examples:\n  formless mcp list\n  formless mcp catalog\n  formless mcp tools SERVER_NAME\n  formless mcp prompts --json\n  formless mcp prompt SERVER NAME --arg key=value"
    )]
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// User memory: notes the agent keeps about your preferences (SPEC §7.11)
    #[command(
        after_help = "Examples:\n  formless memory list\n  formless memory list --json\n  formless memory used --session SESSION --json\n  formless memory used --run RUN_ID"
    )]
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Read recorded model, Tool and built-in metrics without a model call
    #[command(
        after_help = "Examples:\n  formless metrics --since 7d\n  formless metrics --json\n\nTool rows identify stored tool_id values. Reports are independent reads at one cutoff, not a transactional snapshot."
    )]
    Metrics {
        /// Only records since this duration ago (7d, 24h, 1h30m); omitted: all history
        #[arg(long, value_name = "DURATION", value_parser = parse_since_cutoff)]
        since: Option<String>,
        /// Print all three complete replies as one JSON object
        #[arg(long)]
        json: bool,
    },
    /// List the default provider's endpoint models and existing picker report
    #[command(after_help = "Examples:\n  formless models list\n  formless models list --json")]
    Models {
        #[command(subcommand)]
        action: cli_models::Action,
    },
    /// Read the module inventory, metadata and stored source versions
    #[command(
        after_help = "Examples:\n  formless modules list --json\n  formless modules show MODULE_NAME\n  formless modules show MODULE_NAME --version 1"
    )]
    Modules {
        #[command(subcommand)]
        action: cli_modules::Action,
    },
    /// Embedded session-naming model: status, install, try it out
    #[command(
        after_help = "Examples:\n  formless naming status\n  formless naming test \"Plan my week\""
    )]
    Naming {
        #[command(subcommand)]
        action: NamingAction,
    },
    /// Model prices for spending limits and cost (provider.prices)
    #[command(
        after_help = "Examples:\n  formless prices import --dry-run\n  formless prices import --provider local --dry-run\n  formless prices list --provider default --json"
    )]
    Prices {
        #[command(subcommand)]
        action: PricesAction,
    },
    /// List or remove profiles (isolated data dirs made with --profile)
    #[command(after_help = "Examples:\n  formless profiles list\n  formless profiles list --json")]
    Profiles {
        #[command(subcommand)]
        action: ProfilesAction,
    },
    /// Prompt/memory self-optimisation proposals
    #[command(
        after_help = "Examples:\n  formless prompts list\n  formless prompts list --json\n  formless prompts diff PROPOSAL_ID --json"
    )]
    Prompts {
        #[command(subcommand)]
        action: PromptsAction,
    },
    /// Extra model providers (SPEC §4.5)
    #[command(
        after_help = "Examples:\n  formless providers list\n  formless providers test local"
    )]
    Providers {
        #[command(subcommand)]
        action: ProvidersAction,
    },
    /// Hosted relay + web frontend (SPEC §12, v2)
    #[command(
        after_help = "Examples:\n  formless relay status --json\n  formless relay status --qr"
    )]
    Relay {
        #[command(subcommand)]
        action: RelayAction,
    },
    /// Wipe config, keychain entry, and the entire data dir (SPEC §15)
    Reset {
        /// Skip the interactive reset confirmation
        #[arg(long)]
        force: bool,
    },
    /// Replace the database with a backup (stop the daemon first); the
    /// current one is saved next to the backups
    Restore {
        /// Path to the database backup to restore
        file: std::path::PathBuf,
    },
    /// Roles: a purpose mapped to a provider + model (SPEC §4.5)
    #[command(after_help = "Examples:\n  formless roles list\n  formless roles list --json")]
    Roles {
        #[command(subcommand)]
        action: RolesAction,
    },
    /// Read the daemon's currently available routing decisions without a model call
    Routing {
        /// Print the complete daemon reply as one JSON value
        #[arg(long)]
        json: bool,
    },
    /// Headless single Run (SPEC §12). Approvals fail closed unless --yes.
    /// A message queued while an earlier Run closes exits 2 without a
    /// foreground answer. JSON reports status queued; JSONL emits only
    /// actual events, and result output files remain unchanged.
    #[command(
        after_help = "Examples:\n  formless run \"Summarize README.md\" --estimate\n  formless run \"Summarize README.md\" --output final\n  printf 'Summarize README.md' | formless run -\n  git diff | formless run \"Review this diff\" --output json"
    )]
    Run {
        /// Print a local forecast and exit without starting or changing anything
        #[arg(long)]
        estimate: bool,
        /// Task text; `-` reads it from stdin. With a task, piped stdin is
        /// attached as stdin.txt (`git diff | formless run "review this"`)
        task: String,
        /// Continue an existing session by id, or `last` (default: ephemeral)
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
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
        /// Emit one JSON event per line (same as --output jsonl-events).
        /// With --estimate, print one JSON forecast instead.
        #[arg(long, conflicts_with = "output")]
        json: bool,
        /// text: stream the answer and progress; final: only the final
        /// answer on stdout; json: one result object at the end;
        /// jsonl-events: every event, one per line.
        /// With --estimate, use text or json to print the forecast.
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
        #[command(flatten)]
        pipeline: cli_pipeline::Args,
    },
    /// Recent Runs and their outcomes: steps, tool calls, tokens (SPEC §8.7)
    #[command(
        after_help = "Examples:\n  formless runs --limit 10\n  formless runs --session SESSION --since 24h\n  formless runs show RUN_ID --json\n  formless runs running --json"
    )]
    Runs {
        #[command(subcommand)]
        action: Option<cli_attach::RunsAction>,
        /// Maximum number of recent Runs to list (default: 20)
        #[arg(long)]
        limit: Option<usize>,
        /// Only this Session's Runs (existing ID/prefix/name selector)
        #[arg(long, value_name = "SESSION")]
        session: Option<String>,
        /// Only Runs since this duration ago; omitted: all history
        #[arg(long,value_name="DURATION",value_parser=parse_since_cutoff)]
        since: Option<String>,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// What the daemon can reach: grants, denials, trusted folders, MCP
    /// servers, trusted hooks, relay and web UI
    #[command(
        after_help = "Examples:\n  formless security status\n  formless security status --json"
    )]
    Security {
        #[command(subcommand)]
        action: SecurityAction,
    },
    /// Sessions: list, show, search, rename, delete, export or import them
    #[command(
        after_help = "Examples:\n  formless sessions list\n  formless sessions show SESSION_ID\n  formless sessions usage SESSION_ID --json\n  formless sessions export SESSION_ID --format markdown"
    )]
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
    /// Accessible line-mode first-time provider setup
    #[command(after_help = "Examples:\n  formless setup\n  formless --profile work setup")]
    Setup,
    /// Skills: SKILL.md know-how the agent loads on demand (SPEC §7.10)
    #[command(
        after_help = "Examples:\n  formless skills list\n  formless skills show example-skill"
    )]
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Disk used by Sessions, artifacts, Tool versions, logs, backups
    Storage {
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Tool registry inspection/transfer (SPEC §7.8, §12)
    #[command(
        after_help = "Examples:\n  formless tools list\n  formless tools list --filter not-retired --tag work --show-pinned\n  formless tools show TOOL_NAME --json\n  formless tools show TOOL_NAME --code\n  formless tools export TOOL_NAME --output tool.json"
    )]
    Tools {
        #[command(subcommand)]
        action: ToolsAction,
    },
    /// Launch the TUI (default)
    #[command(
        after_help = "Examples:\n  formless tui --new\n  formless tui -c\n  formless tui --plain"
    )]
    Tui {
        /// Use the existing line-oriented screen-reader client
        #[arg(long)]
        plain: bool,
        /// Open the most recently active Session; conflicts with --new
        #[arg(short = 'c', long = "continue", conflicts_with = "new")]
        continue_last: bool,
        /// Start a fresh Session; conflicts with --continue
        #[arg(long)]
        new: bool,
    },
    /// Remove Formless from this machine: stop the daemon, remove the login
    /// service, delete keychain entries and every profile's data, and the
    /// binary (SPEC §15)
    Uninstall {
        /// Keep Sessions, Tools and config (remove only the service,
        /// secrets and binary)
        #[arg(long)]
        keep_data: bool,
        /// Leave the binary in place
        #[arg(long)]
        keep_binary: bool,
        /// Don't ask for confirmation (required when not on a terminal)
        #[arg(long, short, visible_alias = "force")]
        yes: bool,
    },
    /// Check for a newer release, show what changed, and how to upgrade
    #[command(
        after_help = "Examples:\n  formless update check\n  formless update --check\n  formless update check --json\n\nJSON: {installed, status, newer_releases: [{tag, url, notes}]}. Status is update_available, up_to_date (within the fetched page), or release_check_unavailable (HTTP 404 status). Checks never execute an updater."
    )]
    Update {
        #[command(subcommand)]
        action: Option<UpdateAction>,
        /// Only report (same as `update check`)
        #[arg(long)]
        check: bool,
        /// One JSON report with installed version, response status and ordered release notes
        #[arg(long, global = true)]
        json: bool,
    },
    /// Read grouped token totals and the daemon's current cost estimates
    #[command(
        after_help = "Examples:\n  formless usage --by model --since 30d\n  formless usage --json"
    )]
    Usage {
        /// Group totals by day, model, provider, purpose, role or Session name
        #[arg(long, value_enum, default_value = "day")]
        by: cli_usage::Group,
        /// Only usage since this duration ago; omitted: all history
        #[arg(long,value_name="DURATION",value_parser=parse_since_cutoff)]
        since: Option<String>,
        /// Print the complete grouped usage array as JSON
        #[arg(long)]
        json: bool,
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
        #[arg(long, conflicts_with_all = ["port", "no_open"])]
        stop: bool,
    },
}

#[derive(clap::Subcommand)]
enum BackupAction {
    /// List daily, migration and pre-restore copies with sizes and receipts
    List {
        /// Machine-readable rows, including exact verification timestamps
        #[arg(long)]
        json: bool,
    },
    /// Verify a standalone snapshot without changing its bytes
    Verify {
        /// Path to the standalone backup snapshot to verify
        file: std::path::PathBuf,
    },
    /// Restore using exclusive ownership and seven bounded recovery copies
    Restore {
        /// Path to the database backup to restore
        file: std::path::PathBuf,
        /// Print the restore plan without verifying the backup or changing the profile
        #[arg(long)]
        dry_run: bool,
    },
    /// Migrate and read a private copy without serving or running any work
    Drill {
        /// Snapshot to rehearse; omit to use the newest available backup
        file: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    /// Log in with the configured OAuth provider in a browser
    Login {
        /// Provider name from [providers.NAME] (default: [provider])
        #[arg(long)]
        provider: Option<String>,
        /// Print the authorization URL without opening a browser
        #[arg(long)]
        no_browser: bool,
    },
    /// Show credential mode and stored-token status
    Status {
        /// Provider name from [providers.NAME] (default: [provider])
        #[arg(long)]
        provider: Option<String>,
        /// One JSON object (never the token itself)
        #[arg(long)]
        json: bool,
    },
    /// Revoke and delete the selected provider's stored OAuth tokens
    Logout {
        /// Provider name from [providers.NAME] (default: [provider])
        #[arg(long)]
        provider: Option<String>,
    },
}

/// The pairing link as a terminal QR code, for a phone's camera; only on a
/// terminal (it holds the secret, so it never goes into a pipe or a log).
fn pairing_qr_columns(lines: &[String]) -> usize {
    use unicode_width::UnicodeWidthStr;
    lines.iter().map(|line| line.width()).max().unwrap_or(0)
}

fn pairing_qr_fits(lines: &[String], width: Option<u16>) -> bool {
    !lines.is_empty() && width.is_some_and(|width| pairing_qr_columns(lines) <= width as usize)
}

fn print_pairing_qr(url: &str) {
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() || !table::color_allowed() {
        return;
    }
    if let Some(lines) = formless_tui::qr::lines(url) {
        let width = crossterm::terminal::size().ok().map(|(width, _)| width);
        if !pairing_qr_fits(&lines, width) {
            if width.is_some() {
                println!("Widen the terminal to at least {} columns to show the pairing QR code, or open the link above.", pairing_qr_columns(&lines));
            } else {
                println!(
                    "Open the link above; terminal width is unavailable for the pairing QR code."
                );
            }
            return;
        }
        println!();
        for line in lines {
            println!("\x1b[97;40m{line}\x1b[0m");
        }
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
        /// Relay websocket base URL (default: relay.url from config.toml)
        #[arg(long)]
        url: Option<String>,
        /// Generate a fresh pairing secret (invalidates old browser links)
        #[arg(long)]
        rotate: bool,
    },
    /// Disable the relay (pairing secret is kept unless --forget)
    Disable {
        /// Also delete the pairing secret instead of keeping it for later
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
    /// Show the existing proposal comparison without applying it
    Diff {
        /// ID of the prompt or memory proposal to compare
        id: String,
        /// Print the complete daemon response as JSON
        #[arg(long)]
        json: bool,
    },
    /// List proposals with eval scores
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Generate and measure a new proposal
    Generate,
    /// Apply a measured proposal
    Apply {
        /// ID of the prompt or memory proposal
        id: String,
    },
    /// Reject a proposal
    Reject {
        /// ID of the prompt or memory proposal
        id: String,
    },
    /// Roll back an applied proposal
    Rollback {
        /// ID of the prompt or memory proposal
        id: String,
    },
}

#[derive(Subcommand)]
enum ToolsAction {
    /// Retained description warnings and exact before/after changes
    Reviews {
        /// Tool name to inspect; omit to list retained reviews
        name: Option<String>,
        /// Filter retained reviews by their recorded kind
        #[arg(long)]
        kind: Option<String>,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Compare stored Tool versions using the daemon's sectioned report
    #[command(
        after_help = "Examples:\n  formless tools diff TOOL_NAME\n  formless tools diff TOOL_NAME --from 1 --to 2 --json\n\n--to defaults to the current version; --from defaults to its predecessor, which must be at least 1. Explicit missing versions are reported by the daemon. Lookup uses ListTools, then DiffToolVersions; the daemon currently loads full history internally."
    )]
    Diff {
        /// Exact Tool name or full ID, using the existing lookup
        name: String,
        /// Stored source version (positive integer; default target minus one)
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        from: Option<i64>,
        /// Stored target version (positive integer; default current version)
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        to: Option<i64>,
        /// Print the complete diff and schema-compatibility reply as JSON
        #[arg(long)]
        json: bool,
    },
    /// List registered Tools and their current status
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
        /// Only Tools with this tag
        #[arg(long)]
        tag: Option<String>,
        /// Select a stored-state predicate, combined with the exact tag by AND
        #[arg(long, value_enum)]
        filter: Option<cli_tool_filters::Filter>,
        /// Add a PINNED column to human output (changes the displayed layout)
        #[arg(long)]
        show_pinned: bool,
    },
    /// Switch a Tool off: hidden from the agent and uncallable, nothing else changes
    Disable {
        /// Tool name or ID
        name: String,
    },
    /// Switch a Tool back on
    Enable {
        /// Tool name or ID
        name: String,
    },
    /// Revert the newest registry change (create, update, retire, restore
    /// or rollback, by anyone)
    Undo,
    /// Always list a Tool to the agent (from each Session's next listing)
    Pin {
        /// Tool name or ID
        name: String,
    },
    /// Stop always listing a Tool
    Unpin {
        /// Tool name or ID
        name: String,
    },
    /// Read or replace a Tool's tags
    Tag {
        /// Tool name or id
        name: String,
        /// Replacement tags; omission only reads
        tags: Vec<String>,
        /// Explicitly remove all tags
        #[arg(long, conflicts_with = "tags")]
        clear: bool,
    },
    /// A Tool's description, schemas, Capabilities, versions and stats
    Show {
        /// Tool name or id
        name: String,
        /// The full detail as JSON
        #[arg(long)]
        json: bool,
        /// Write only the current version's exact source
        #[arg(long, conflicts_with = "json")]
        code: bool,
    },
    /// Retire a Tool: hidden from the agent, kept with its history
    Retire {
        /// Tool name or ID
        name: String,
        /// Retire even when other Tools use it (required when not on a terminal)
        #[arg(long)]
        force: bool,
    },
    /// Bring a retired Tool back
    Restore {
        /// Tool name or ID
        name: String,
    },
    /// Make an earlier version current again (default: the one before)
    Rollback {
        /// Tool name or ID
        name: String,
        /// Version number to make current; omit for the preceding version
        #[arg(long)]
        to: Option<i64>,
    },
    /// Delete a Tool and all its versions and history for good
    Purge {
        /// Tool name or ID
        name: String,
        /// Required without a terminal to confirm on
        #[arg(long)]
        force: bool,
    },
    /// Run a Tool directly; sensitive Capabilities are denied unless --yes
    Run {
        /// Tool name or id
        name: String,
        /// Inline JSON object (the default is {} when neither input flag is used)
        #[arg(long, value_name = "JSON", conflicts_with = "input_file")]
        input: Option<String>,
        /// Read a UTF-8 JSON object from a regular FILE or '-' (stdin), at most 1 MiB
        #[arg(long, value_name = "FILE|-")]
        input_file: Option<std::path::PathBuf>,
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
        /// Print the complete Tool result from the daemon as one JSON value
        #[arg(long)]
        json: bool,
    },
    /// Write one Tool or the whole registry as an importable bundle
    Export {
        /// Tool name or id (omit with --all)
        name: Option<String>,
        /// Export every Tool instead of naming one
        #[arg(long, conflicts_with = "name")]
        all: bool,
        /// Bundle destination; omit to write to standard output
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
    /// Retained Skill description warnings and changes
    Reviews {
        /// Skill name to inspect; omit to list retained reviews
        name: Option<String>,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Install from a folder, a git URL or a .zip (installed disabled)
    Add {
        /// Folder, Git URL or .zip archive containing the Skill
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
    /// Remove an installed Skill
    Remove {
        /// Installed Skill name
        name: String,
    },
    /// List installed Skills and whether they are enabled
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Make an installed Skill available to the agent
    Enable {
        /// Installed Skill name
        name: String,
    },
    /// Keep an installed Skill but hide it from the agent
    Disable {
        /// Installed Skill name
        name: String,
    },
    /// Print a skill's SKILL.md and files, to review before enabling
    Show {
        /// Installed Skill name
        name: String,
    },
    /// Write a skill (or --all) as a bundle file (no metrics)
    Export {
        /// Installed Skill name; omit with --all
        name: Option<String>,
        /// Export every installed Skill instead of naming one
        #[arg(long, conflicts_with = "name")]
        all: bool,
        /// Bundle destination; omit to write to standard output
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Install a bundle; imported skills start disabled for review
    Import {
        /// Path to an exported Skill bundle
        file: String,
    },
}

#[derive(Subcommand)]
enum PricesAction {
    /// Copy prices from the provider's /models listing (OpenRouter and
    /// others that publish them) into provider.prices, for the models
    /// you use (or every listed one with --all)
    Import {
        /// A named provider (default: the default one)
        #[arg(long)]
        provider: Option<String>,
        /// Every model the listing prices, not only the ones in use
        #[arg(long)]
        all: bool,
        /// Show what would change; write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Show stored prices and configured model references without contacting the provider
    List {
        /// Show only this provider (use "default" for the default provider)
        #[arg(long)]
        provider: Option<String>,
        /// One JSON value instead of tables
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ProvidersAction {
    /// List configured providers and their endpoint/model settings
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Add or update a provider; the API key is read from stdin (not argv)
    Add {
        /// Name to store for this provider configuration
        name: String,
        /// Provider API base URL; use --preset or Azure options instead when applicable
        #[arg(long, required_unless_present_any = ["preset", "azure_resource"])]
        base_url: Option<String>,
        /// Azure OpenAI resource name, host, or HTTPS endpoint
        #[arg(long, requires = "azure_deployment", conflicts_with_all = ["base_url", "preset"])]
        azure_resource: Option<String>,
        /// Azure OpenAI: the deployment name
        #[arg(long, requires = "azure_resource")]
        azure_deployment: Option<String>,
        /// Use the dated Azure deployment route with this API version (default: v1)
        #[arg(long, requires = "azure_resource")]
        api_version: Option<String>,
        /// Extra request header, `Key=Value` (repeatable)
        #[arg(long = "header")]
        headers: Vec<String>,
        /// Extra query parameter, `key=value` (repeatable)
        #[arg(long = "query")]
        query: Vec<String>,
        /// Send the key in this header instead of Authorization: Bearer
        #[arg(long)]
        key_header: Option<String>,
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
    /// Remove a named provider configuration
    Remove {
        /// Configured provider name
        name: String,
    },
    /// Fetch the provider's /models list; with --model, test that model:
    /// latency, streaming, reasoning and tool calls
    ///
    /// Model probe reports exit 1 after printing a negative verdict.
    Test {
        /// Configured provider name
        name: String,
        /// Model ID to probe; omit to list the provider's available models
        #[arg(long)]
        model: Option<String>,
        /// Write one JSON report instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Dismiss one configured model warning for this profile
    DismissWarning {
        /// Configured provider name, including default
        provider: String,
        /// Model ID shown by the warning
        model: String,
    },
}

#[derive(Subcommand)]
enum RolesAction {
    /// List configured Roles and their provider/model mappings
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Create or replace a role
    Add {
        /// Name to store for this Role
        name: String,
        /// When to use it, for the agent ("fast; broad read-only search")
        #[arg(long)]
        description: String,
        /// Provider name (default: [provider])
        #[arg(long)]
        provider: Option<String>,
        /// Model ID or configured alias for this Role
        #[arg(long, default_value = "")]
        model: String,
        /// Reasoning-effort override, for example low, medium or high
        #[arg(long)]
        effort: Option<String>,
        /// Extra system prompt for sub-agents in this role
        #[arg(long)]
        system: Option<String>,
    },
    /// Remove a configured Role
    Rm {
        /// Configured Role name
        name: String,
    },
}

#[derive(Subcommand)]
enum JobsAction {
    /// List scheduled jobs and their current state
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Schedule a prompt: --every "weekday at 9am" or --cron "0 3 * * *"
    /// (recurring), or --at "in 30m"
    Add {
        /// Task text to send to the agent when the job fires
        prompt: String,
        /// Cron expression, or plain English ("every 2 hours")
        #[arg(
            long,
            visible_alias = "every",
            conflicts_with = "at",
            required_unless_present = "at"
        )]
        cron: Option<String>,
        /// Once: relative offset ("in 30m", "2h"), local YYYY-MM-DD HH:MM, or RFC 3339
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
    /// Preview projected times for a recurring schedule
    #[command(
        long_about = "Preview a recurring schedule (cron or supported plain English). Times are projected from the daemon's current local time. The preview leaves jobs unchanged and is not a guarantee of validation by jobs add or future execution."
    )]
    Preview {
        /// Recurring schedule expression, quoted if it contains spaces
        schedule: String,
        /// Print the daemon's cron and next times as JSON
        #[arg(long)]
        json: bool,
    },
    /// Fire a job now (its schedule is unchanged)
    Run {
        /// Scheduled job ID
        id: String,
    },
    /// A job's recent firings: time, trigger, outcome, cost, Session
    History {
        /// Scheduled job ID
        id: String,
        /// Maximum number of recent firings to show (1–500)
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=500))]
        limit: u16,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Enable (and approve) a job
    Enable {
        /// Scheduled job ID
        id: String,
    },
    /// Pause a scheduled job without deleting it
    Disable {
        /// Scheduled job ID
        id: String,
    },
    /// Remove a scheduled job
    Rm {
        /// Scheduled job ID
        id: String,
    },
}

#[derive(Subcommand)]
enum SecurityAction {
    /// One-screen audit of the attack surface
    Status {
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum MemoryAction {
    /// Report current memory notes associated with a Run selector
    #[command(
        long_about = "Report current note fields associated with an exact Run id or a Session's latest Run (which may still be running). Associations can survive a deleted Run; empty or partial results do not establish prompt absence or a historical snapshot. This does not validate Run existence or change memory attribution."
    )]
    Used {
        /// Session id, unambiguous prefix or name for its latest-Run report
        #[arg(long, required_unless_present = "run", conflicts_with = "run")]
        session: Option<String>,
        /// Exact Run id sent unchanged, including unmatched or retained associations
        #[arg(long, required_unless_present = "session")]
        run: Option<String>,
        /// Preserve the complete daemon MemoryInfo array as JSON
        #[arg(long)]
        json: bool,
    },

    /// List notes with id, scope and source
    List {
        /// Print JSON (for export)
        #[arg(long)]
        json: bool,
    },
    /// Add a note
    Add {
        /// Note text to remember
        text: String,
        /// "global" or "project:/abs/path"
        #[arg(long, default_value = "global")]
        scope: String,
    },
    /// Delete a note
    Rm {
        /// ID of the memory note
        id: String,
    },
    /// Change a note's text or scope, or disable/enable it
    Edit {
        /// ID of the memory note
        id: String,
        /// Replacement note text; omit to leave the text unchanged
        #[arg(long)]
        text: Option<String>,
        /// Replacement scope: global or project:/absolute/path
        #[arg(long)]
        scope: Option<String>,
        /// Keep the note but stop including it in agent context
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
        /// Include a previously disabled note in agent context again
        #[arg(long)]
        enable: bool,
    },
}

fn parse_mcp_log_lines(value: &str) -> std::result::Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|lines| (1..=1000).contains(lines))
        .ok_or_else(|| "--lines must be between 1 and 1000".into())
}

#[derive(Subcommand)]
enum McpAction {
    /// Add a server: `add NAME -- COMMAND [ARGS]` (stdio), `add NAME --url
    /// URL` (http), or `add ID` for a catalog server (`mcp catalog`)
    Add {
        /// Server name to configure, or a catalog entry ID
        name: String,
        /// A catalog server's setting, `key=value` (e.g. `path=~/notes`)
        #[arg(long = "param")]
        params: Vec<String>,
        /// Read a secret catalog parameter from a named environment variable (key=ENV).
        #[arg(long = "param-env")]
        param_env: Vec<String>,
        /// Confirm downloading/running the displayed pinned catalog command.
        #[arg(long)]
        yes: bool,
        /// Confirm exposing the displayed root/home/credential directory.
        #[arg(long)]
        confirm_exposure: bool,
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
    Remove {
        /// Configured MCP server name
        name: String,
    },
    /// Configured servers: state, tool count, last error
    List {
        /// One JSON value instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Restart a server and list its tools again
    #[command(
        after_help = "Restarts the server. Prints the complete report, then exits 0 for success (including zero tools), or 1 for a reported restart error. Request failures retain their existing exit mapping."
    )]
    Test {
        /// Configured MCP server name
        name: String,
        /// Print the complete restart reply as one JSON value
        #[arg(long)]
        json: bool,
    },
    /// Authorize Formless with a remote (http) MCP server via OAuth
    Login {
        /// Configured remote MCP server name
        server: String,
    },
    /// Delete a server's stored OAuth tokens
    Logout {
        /// Configured remote MCP server name
        server: String,
    },
    /// Popular servers you can add by name (`formless mcp add ID`)
    Catalog,
    /// Find servers configured in Claude Desktop, Cursor or VS Code and
    /// add them (each command is shown and asked about first)
    Import {
        /// Add every server found without asking
        #[arg(long)]
        yes: bool,
        /// Copy env/header values to plaintext config.toml; default skips secrets.
        #[arg(long)]
        copy_secrets: bool,
    },
    /// The end of a stdio server's stderr log
    Log {
        /// Configured MCP server name
        name: String,
        /// Recent stderr log lines to show (minimum 1, maximum 1000)
        #[arg(long, default_value_t = 50, value_parser = parse_mcp_log_lines)]
        lines: usize,
    },
    /// A server's tools, on or off, with their token cost
    Tools {
        /// Configured MCP server name
        name: String,
    },
    /// Report prompts from reachable enabled servers (discovery may be incomplete)
    #[command(
        long_about = "Report prompts through live daemon MCP connections. This can start configured servers; disabled or unreachable servers and discovery failures are skipped, so discovery may be incomplete."
    )]
    Prompts {
        /// Preserve the full daemon discovery array as JSON
        #[arg(long)]
        json: bool,
    },
    /// Retrieve a live server prompt as the daemon's text projection (no Session submission)
    #[command(
        long_about = "Retrieve a live prompt through the daemon; this can start the configured server and sends named string arguments to it. Output is the daemon's text projection: content.text strings joined by blank lines, without roles or non-text parts. JSON preserves this text report, not raw MCP messages. Server/daemon errors are preserved; no local required/unknown argument check is promised."
    )]
    Prompt {
        /// Configured MCP server name
        server: String,
        /// Complete prompt identifier reported by the server
        name: String,
        /// Named string argument KEY=VALUE (repeatable; empty values and extra equals are preserved)
        #[arg(long = "arg", value_parser = cli_mcp_prompts::parse_arg)]
        args: Vec<cli_mcp_prompts::NamedArg>,
        /// Preserve the complete daemon text report as JSON
        #[arg(long)]
        json: bool,
    },
    /// Turn one of a server's tools on or off
    Tool {
        /// Configured MCP server name
        name: String,
        /// Name of the server's Tool to change
        tool: String,
        /// Enable this server Tool
        #[arg(long, conflicts_with = "off")]
        on: bool,
        /// Disable this server Tool
        #[arg(long)]
        off: bool,
    },
}

#[derive(Subcommand)]
enum NamingAction {
    /// Show the configured model and the files in the naming cache
    Status {
        /// One JSON object
        #[arg(long)]
        json: bool,
    },
    /// Download the model (default: the configured one)
    Pull {
        /// Model ID to download; omit to use the configured model
        #[arg(value_parser = NamingModelParser::new())]
        model: Option<String>,
    },
    /// Remove cached weights, tokenizers, and partial downloads
    Remove {
        /// Model ID to remove; omit to use the configured model
        #[arg(value_parser = NamingModelParser::new(), conflicts_with = "all")]
        model: Option<String>,
        /// Remove the files for all naming models
        #[arg(long)]
        all: bool,
    },
    /// Print the title the namer would give a first message
    Test {
        /// First-message text to turn into a Session title
        text: String,
    },
}

#[derive(Subcommand)]
enum ApprovalsAction {
    /// Outstanding capability requests (no daemon is started)
    Pending {
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Answer one pending request; Once unless --session is given
    Approve {
        /// ID or unambiguous prefix of the pending approval to answer
        id: String,
        /// Grant this pending request once (the default scope)
        #[arg(long, conflicts_with = "session")]
        once: bool,
        /// Grant this pending request for its Session
        #[arg(long)]
        session: bool,
        /// Confirm a broad Session grant without terminal input
        #[arg(long)]
        yes: bool,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Deny one pending request without a permanent denial
    Deny {
        /// ID or unambiguous prefix of the pending approval to deny
        id: String,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Every grant: Tool, Capability, scope, when, by which client, last used
    List {
        /// Only this Capability family (fs, net, process, env, …)
        #[arg(long)]
        family: Option<String>,
        /// Print machine-readable JSON instead of human-readable output
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
        #[arg(long, conflicts_with_all = ["tool", "capability", "scope", "session"])]
        all: bool,
        /// With --all, restrict revocation to one Capability family
        #[arg(long)]
        family: Option<String>,
    },
    /// Capabilities you always deny; --forget CAP (or --all) to ask again
    Denials {
        /// Forget the stored denial for this exact Capability
        #[arg(long)]
        forget: Option<String>,
        /// Forget every stored permanent denial
        #[arg(long, conflicts_with = "forget")]
        all: bool,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
}

/// A Capability's family: "fs" for fs:read:/x, "net" for net:host.
fn cap_family(cap: &str) -> &str {
    cap.split(':').next().unwrap_or(cap)
}

fn approvals_cmd(paths: &Paths, action: ApprovalsAction) -> Result<()> {
    let action = match action {
        ApprovalsAction::Pending { json } => return cli_attach::pending(paths, json),
        ApprovalsAction::Approve {
            id,
            session,
            yes,
            json,
            ..
        } => return cli_attach::decide(paths, &id, true, session, yes, json),
        ApprovalsAction::Deny { id, json } => {
            return cli_attach::decide(paths, &id, false, false, false, json)
        }
        other => other,
    };
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
        ApprovalsAction::Pending { .. }
        | ApprovalsAction::Approve { .. }
        | ApprovalsAction::Deny { .. } => unreachable!("handled before daemon startup"),
        ApprovalsAction::Denials { forget, all, json } => {
            if forget.is_some() || all {
                let r = request_once(paths, Request::ForgetDenial { capability: forget })?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    println!(
                        "forgot {} denial(s); the next use asks again",
                        r["forgotten"]
                    );
                }
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
                    c(local_timestamp(&d.created_at).replace('T', " ")),
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
                    .map(|s| local_timestamp(s).replace('T', " "))
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
                            Tone::Warn
                        } else {
                            Tone::Plain
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
                    other => {
                        return Err(usage(format!(
                            "--scope is always, session or hour, not {other}"
                        )))
                    }
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
                let tool = tool.ok_or_else(|| usage("name a Tool, or pass --all"))?;
                let id = find_tool(paths, &tool)?.id;
                vec![(id, capability, scope_of(&scope)?, session)]
            };
            let mut seen = std::collections::HashSet::new();
            let mut n = 0;
            let mut still: std::collections::BTreeSet<String> = Default::default();
            for (tool_id, capability, scope, session_id) in targets {
                if !seen.insert((
                    tool_id.clone(),
                    capability.clone(),
                    scope_word(scope),
                    session_id.clone(),
                )) {
                    continue;
                }
                if let Ok(v) = request_once(
                    paths,
                    Request::RevokeGrant {
                        tool_id,
                        capability,
                        scope,
                        session_id,
                    },
                ) {
                    n += 1;
                    for c in v["still_trusted"].as_array().into_iter().flatten() {
                        still.extend(c.as_str().map(str::to_string));
                    }
                }
            }
            if n == 0 {
                anyhow::bail!("nothing to revoke");
            }
            println!("revoked {n} grant(s)");
            // Config trust keeps approving these: revoking doesn't make them ask (#844).
            if !still.is_empty() {
                println!(
                    "still allowed without asking by security.trusted_paths / trusted_hosts: {}",
                    still.into_iter().collect::<Vec<_>>().join(", ")
                );
            }
            Ok(())
        }
    }
}

#[derive(Subcommand)]
enum ProfilesAction {
    /// Every profile, whether its daemon runs, and where its data lives
    List {
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Delete a profile's data, config and stored secrets
    Rm {
        /// Name of the non-default profile to remove
        #[arg(value_parser = parse_profile_name)]
        name: String,
        /// Don't ask for confirmation (required when not on a terminal)
        #[arg(long, visible_alias = "yes")]
        force: bool,
    },
}

/// Pure preflight: no daemon, service, keychain or destructive callbacks.
fn profile_removal_target(paths: &Paths, name: &str) -> Result<Paths> {
    let target = paths.named(name)?;
    if name == "default" {
        anyhow::bail!("the default profile can't be removed; `formless reset` wipes it");
    }
    target.validate_cleanup_namespace()?;
    if !paths
        .list_profiles()
        .iter()
        .any(|profile| profile.profile == name)
    {
        return Err(usage(format!(
            "no profile named {name:?} (see `formless profiles list`)"
        )));
    }
    Ok(target)
}

fn profiles_cmd(paths: &Paths, action: ProfilesAction) -> Result<()> {
    match action {
        ProfilesAction::List { json } => {
            let inventory = paths.profile_inventory();
            for diagnostic in &inventory.diagnostics {
                eprintln!("warning: {diagnostic}");
            }
            let rows: Vec<Value> = inventory.profiles
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
                        "backup_bytes": formless_core::backup_verification::copies(p).iter().map(|f|formless_core::housekeeping::path_bytes(f)).sum::<u64>(),
                        "backups": formless_core::backup_verification::list(p),
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
            let mut tbl =
                Table::new(&["PROFILE", "DAEMON", "LAST USED", "BACKUP BYTES", "DATA DIR"]);
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
                    c(r["backup_bytes"].to_string()),
                    c(r["data_dir"].as_str().unwrap_or_default()),
                ]);
            }
            tbl.print();
            Ok(())
        }
        ProfilesAction::Rm { name, force } => {
            // Programmatic callers receive the same preflight as clap callers.
            let target = profile_removal_target(paths, &name)?;
            if !force {
                confirm_typed(
                    &format!(
                        "This deletes profile '{name}': its config, API key, Tools and Sessions in {}.",
                        target.data_dir.display()
                    ),
                    &name,
                )?;
            }
            profile_maintenance(
                &target,
                || Ok(()),
                || stop_daemon(&target),
                |guard| formless_core::config::reset_all_guarded(&target, guard),
            )?;
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
    /// Stop the running daemon
    Stop,
    /// Stop the daemon once its Runs finish, then start a fresh one
    Restart {
        /// Interrupt running Runs instead of waiting for them
        #[arg(long)]
        force: bool,
    },
    /// Show whether the daemon is running and its current status
    Status {
        /// JSON even when the daemon isn't running ({"running": false})
        #[arg(long)]
        json: bool,
        /// Exit successfully when the daemon returns a live process status
        #[arg(long, conflicts_with = "json")]
        check: bool,
    },
}

/// A mistake in how a command was called (an unknown id, a missing or
/// conflicting choice, a bad value): exit 2, like clap's own errors (#896).
#[derive(Debug)]
struct Usage(String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Usage {}

/// A [`Usage`] error to return with `?` or `Err(...)`.
fn usage(msg: impl Into<String>) -> anyhow::Error {
    Usage(msg.into()).into()
}

/// Exit 2 for usage mistakes, ours or the daemon's (an unknown id, a bad
/// request), 1 for everything else.
fn exit_code(e: &anyhow::Error) -> i32 {
    use formless_protocol::codes;
    if e.downcast_ref::<Usage>().is_some() {
        return 2;
    }
    match e
        .downcast_ref::<formless_tui::client::DaemonError>()
        .and_then(|d| d.code.as_deref())
    {
        Some(codes::NOT_FOUND | codes::BAD_REQUEST | codes::INVALID | codes::UNSUPPORTED) => 2,
        _ => 1,
    }
}

// Possible values are suggestions from `docs::TOPICS` only. `StringValueParser`
// still accepts every string; `docs::resolve` does case, prefix, blank, and errors.
#[derive(Clone)]
struct DocsTopicParser(clap::builder::StringValueParser);

impl DocsTopicParser {
    fn new() -> Self {
        Self(clap::builder::StringValueParser::new())
    }
}

impl clap::builder::TypedValueParser for DocsTopicParser {
    type Value = String;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        self.0.parse_ref(cmd, arg, value)
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(formless_core::docs::TOPICS.iter().map(
            |(name, _, description)| clap::builder::PossibleValue::new(*name).help(*description),
        )))
    }
}

#[derive(Clone)]
struct NamingModelParser(clap::builder::StringValueParser);

impl NamingModelParser {
    fn new() -> Self {
        Self(clap::builder::StringValueParser::new())
    }
}

impl clap::builder::TypedValueParser for NamingModelParser {
    type Value = String;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<String, clap::Error> {
        let value = self.0.parse_ref(cmd, arg, value)?;
        if formless_core::namer::spec(&value).is_some() {
            return Ok(value);
        }
        let choices = formless_core::namer::MODELS
            .iter()
            .map(|model| model.id)
            .collect::<Vec<_>>()
            .join(", ");
        Err(cmd.clone().error(
            clap::error::ErrorKind::InvalidValue,
            format!("unknown naming model '{value}'; valid model IDs: {choices}"),
        ))
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(
            formless_core::namer::MODELS
                .iter()
                .map(|model| clap::builder::PossibleValue::new(model.id)),
        ))
    }
}

fn generate_completions(shell: clap_complete::Shell, out: &mut impl Write) -> std::io::Result<()> {
    use clap::CommandFactory;

    clap_complete::generate(shell, &mut Cli::command(), "formless", out);
    if shell == clap_complete::Shell::Fish {
        out.write_all(b"\n")?;
        out.write_all(FISH_DOCS_TOPIC_CONDITION.as_bytes())?;
        for (name, _, description) in formless_core::docs::TOPICS {
            writeln!(
                out,
                "complete -c formless -f -n '__fish_formless_docs_topic_argument' -a {} -d {}",
                fish_quote(name),
                fish_quote(description)
            )?;
        }
    }
    Ok(())
}

const FISH_DOCS_TOPIC_CONDITION: &str = r#"function __fish_formless_docs_topic_argument
    set -l tokens (commandline -opc)
    set -l command_seen 0
    set -l docs_seen 0
    set -l pending_value 0
    set -l docs_arguments

    for token in $tokens
        if test $docs_seen -eq 1
            set -a docs_arguments "$token"
            continue
        end
        if test $pending_value -eq 1
            set pending_value 0
            continue
        end
        if test $command_seen -eq 0
            if test (string replace -r '^.*/' '' -- "$token") = formless
                set command_seen 1
            end
            continue
        end
        switch "$token"
            case docs
                set docs_seen 1
            case --profile
                set pending_value 1
            case '--profile=*' '--secure'
                continue
            case '*'
                return 1
        end
    end

    test $docs_seen -eq 1; or return 1

    set pending_value 0
    set -l options_ended 0
    for token in $docs_arguments
        if test $pending_value -eq 1
            set pending_value 0
            continue
        end
        if test $options_ended -eq 1
            return 1
        end
        switch "$token"
            case --profile
                set pending_value 1
            case '--profile=*' '--secure' '--print' '--text'
                continue
            case '--keys' '--help' '-h' '--version'
                return 1
            case --
                set options_ended 1
            case '-*'
                return 1
            case '*'
                return 1
        end
    end

    test $pending_value -eq 0
end
"#;

fn fish_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn docs_cmd(topic: Option<&str>, print: bool, keys: bool, text: bool) -> Result<()> {
    let result: Result<()> = (|| {
        let mut out = std::io::stdout().lock();
        if keys {
            write!(out, "{}", formless_tui::keys_reference())?;
            return Ok(());
        }
        let url = formless_core::docs::resolve(topic).map_err(|error| {
            let topics = formless_core::docs::TOPICS
                .iter()
                .map(|(name, _, what)| format!("  {name:<16} {what}"))
                .collect::<Vec<_>>()
                .join("\n");
            usage(format!("{error}\n\nTOPIC            WHAT\n{topics}"))
        })?;
        if print {
            writeln!(out, "{url}")?;
        } else if text {
            write!(out, "{}", formless_core::docs::text(topic)?)?;
        } else {
            let guide = formless_core::docs::present(topic, false)?;
            if let Some(body) = guide.text {
                writeln!(
                    out,
                    "Bundled guide (Formless {}).\n",
                    env!("CARGO_PKG_VERSION")
                )?;
                write!(out, "{body}")?;
                if !body.ends_with('\n') {
                    writeln!(out)?;
                }
                writeln!(
                    out,
                    "\nCurrent online copy (may differ from this build): {}",
                    guide.url
                )?;
            } else {
                writeln!(out, "opened current online copy: {}", guide.url)?;
            }
        }
        Ok(())
    })();
    match result {
        // Readers such as `head` may finish before the full bundled SPEC.
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            Ok(())
        }
        result => result,
    }
}

fn exit_cli(code: i32) -> ! {
    formless_tui::console_encoding::restore_before_exit();
    std::process::exit(code);
}

fn main() {
    let _console = formless_tui::console_encoding::ConsoleEncoding::enable();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            exit_cli(error.exit_code());
        }
    };
    if let Err(e) = run_cli(cli) {
        if !e.is::<DoctorProblems>() {
            eprintln!("error: {e:#}");
        }
        exit_cli(exit_code(&e));
    }
}

fn validate_mcp_prompt_args(cli: &Cli) -> Result<()> {
    if let Some(Cmd::Mcp {
        action: McpAction::Prompt { args, .. },
    }) = &cli.cmd
    {
        cli_mcp_prompts::arguments(args)?;
    }
    Ok(())
}

fn validate_tui_start(cli: &mut Cli) -> Result<()> {
    if let Some(Cmd::Tui {
        plain,
        continue_last,
        new,
    }) = &cli.cmd
    {
        cli.plain |= *plain;
        cli.continue_last |= *continue_last;
        cli.new |= *new;
    }
    if cli.continue_last && cli.new {
        return Err(usage(
            "--continue and --new cannot be used together, including across tui",
        ));
    }
    // The TUI's own flags mean nothing to other commands (#897).
    if !matches!(cli.cmd, None | Some(Cmd::Tui { .. }))
        && (cli.plain || cli.continue_last || cli.new)
    {
        return Err(usage(
            "--plain, -c/--continue and --new start the TUI; use them without a command (or `formless tui`)",
        ));
    }
    Ok(())
}

fn run_cli(mut cli: Cli) -> Result<()> {
    cli_attach::validate_runs_limit(&cli)?;
    validate_runs_filters(&cli)?;
    validate_tui_start(&mut cli)?;
    // Export usage errors must precede profile setup, Session lookup and any
    // daemon connection. sessions_cmd validates again for direct callers.
    validate_cli_export_format(&cli)?;
    validate_mcp_prompt_args(&cli)?;
    // Reject invalid Tool input before resolving a profile or connecting.
    let tool_run_input = match &cli.cmd {
        Some(Cmd::Tools {
            action: ToolsAction::Run {
                input, input_file, ..
            },
        }) => Some(cli_tool_run_input::prepare(
            input.as_deref(),
            input_file.as_deref(),
            std::io::stdin(),
        )?),
        _ => None,
    };
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
            .with_ansi(std::io::stderr().is_terminal() && table::color_allowed())
            .init();
    }
    // Documentation needs no profile/config/daemon, even for invalid topics.
    if let Some(Cmd::Docs {
        topic,
        print,
        keys,
        text,
    }) = &cli.cmd
    {
        return docs_cmd(topic.as_deref(), *print, *keys, *text);
    }
    // Completions and the man page need nothing else: no profile, config
    // or daemon.
    if let Some(Cmd::Completions { shell }) = cli.cmd {
        generate_completions(shell, &mut std::io::stdout())?;
        return Ok(());
    }
    if let Some(Cmd::Man) = cli.cmd {
        render_man(&mut std::io::stdout())?;
        return Ok(());
    }
    if let Some(Cmd::Commands { action }) = &cli.cmd {
        let paths = Paths::resolve_read_only(cli.profile.as_deref())?;
        return cli_commands::run(&paths, action);
    }
    if matches!(
        cli.cmd,
        Some(Cmd::Daemon {
            action: DaemonAction::InstallService { dry_run: true }
        })
    ) {
        let paths = Paths::resolve_read_only(cli.profile.as_deref())?;
        return daemon_cmd(paths, DaemonAction::InstallService { dry_run: true });
    }
    if let Some(Cmd::Sessions {
        action: SessionsAction::Compare { left, right, json },
    }) = &cli.cmd
    {
        let paths = Paths::resolve_read_only(cli.profile.as_deref())?;
        return sessions_compare_cmd(&paths, left, right, *json);
    }
    if let Some(Cmd::Run {
        estimate: true,
        task,
        session,
        continue_last,
        model,
        effort,
        role,
        attach,
        cwd,
        json,
        output,
        pipeline,
        ..
    }) = &cli.cmd
    {
        let forecast_json = match output {
            RunOutput::Text => *json,
            RunOutput::Json => true,
            RunOutput::Final | RunOutput::JsonlEvents => {
                return Err(usage(
                    "--estimate does not support --output final or jsonl-events. Use --output text or json.",
                ));
            }
        };
        let paths = Paths::resolve_read_only(cli.profile.as_deref())?;
        let live = formless_core::daemon::daemon_reachable(&paths);
        let session = if *continue_last || session.as_deref() == Some("last") {
            let id = read_last_run_session_id(&paths).map_err(usage)?;
            if live {
                Some(check_last_run_session(&paths, id).map_err(usage)?)
            } else {
                let missing = || usage(missing_last_run_session(&id));
                let store =
                    formless_core::Store::open_forecast_snapshot(&paths)?.ok_or_else(missing)?;
                if store.get_session(&id)?.is_none() {
                    return Err(missing());
                }
                Some(id)
            }
        } else if let Some(wanted) = session.as_deref() {
            validate_session_selector(wanted)?;
            let sessions: Vec<SessionInfo> = if live {
                serde_json::from_value(formless_tui::client::request_once(
                    &paths,
                    Request::ListSessions,
                )?)?
            } else {
                formless_core::Store::open_forecast_snapshot(&paths)?
                    .context("forecast Session does not exist")?
                    .list_sessions()?
            };
            let matches: Vec<_> = sessions
                .into_iter()
                .filter(|s| s.id == wanted || s.id.starts_with(wanted) || s.name == wanted)
                .collect();
            if matches.len() != 1 {
                return Err(usage("forecast Session is missing or ambiguous"));
            }
            Some(matches[0].id.clone())
        } else {
            None
        };
        use std::io::{IsTerminal, Read};
        if task.as_str() == "-" && pipeline.schema.as_deref() == Some(std::path::Path::new("-")) {
            return Err(usage("--schema - and task '-' cannot both consume stdin"));
        }
        let piped = if std::io::stdin().is_terminal()
            && pipeline.schema.as_deref() != Some(std::path::Path::new("-"))
        {
            None
        } else {
            let mut bytes = Vec::new();
            let limit = if pipeline.schema.as_deref() == Some(std::path::Path::new("-")) {
                formless_core::pipeline::MAX_SCHEMA_BYTES
            } else {
                formless_core::daemon::MAX_ATTACHMENT_BYTES
            };
            std::io::stdin()
                .take((limit + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() > limit {
                return Err(usage("piped Run input exceeds its byte limit"));
            }
            Some(bytes)
        };
        let mut prepared = pipeline
            .prepare(task.clone(), piped)
            .map_err(|e| usage(e.to_string()))?;
        if pipeline.enabled() {
            prepared.options.model = role
                .as_ref()
                .map(|r| format!("role:{r}"))
                .or_else(|| model.clone());
            prepared.options.reasoning_effort = effort.clone();
            formless_core::pipeline::CompiledRunOptions::compile(prepared.options.clone())?;
        }
        let task = prepared.task;
        let stdin = prepared.attachment;
        let mut files = read_attachments(attach).map_err(usage)?;
        if let Some(bytes) = stdin {
            files.push(("stdin.txt".into(), bytes));
        }
        let attachments = files
            .into_iter()
            .map(|(name, bytes)| formless_core::forecast::attachment(name, &bytes))
            .collect();
        let cwd = cwd
            .clone()
            .or_else(|| {
                session
                    .is_none()
                    .then(|| std::env::current_dir().ok())
                    .flatten()
            })
            .map(|p| p.canonicalize())
            .transpose()?
            .map(|p| p.to_string_lossy().into_owned());
        let input = formless_protocol::ForecastInput {
            options: prepared.options,
            session_id: session,
            draft: task,
            model: role
                .as_ref()
                .map(|r| format!("role:{r}"))
                .or_else(|| model.clone()),
            cwd,
            attachments,
            ..Default::default()
        };
        let forecast: formless_protocol::RunForecast = if live {
            serde_json::from_value(formless_tui::client::request_once(
                &paths,
                Request::ForecastRun {
                    for_send: false,
                    input,
                },
            )?)?
        } else {
            formless_core::forecast::estimate(&paths, &input)?
        };
        // Only an owned DTO reaches output: private readers and maintenance
        // freezes have dropped, even if stdout is a backpressured pipe.
        if forecast_json {
            println!("{}", serde_json::to_string_pretty(&forecast)?);
        } else {
            println!("{}", formless_core::forecast::describe(&forecast));
        }
        return Ok(());
    }
    // Attach/control commands require an existing daemon and must not create a
    // profile (or start scheduled work) merely to observe an absent endpoint.
    let attach_existing = matches!(
        &cli.cmd,
        Some(Cmd::Runs {
            action: Some(_),
            ..
        }) | Some(Cmd::Sessions {
            action: SessionsAction::Show { follow: true, .. }
        }) | Some(Cmd::Approvals {
            action: ApprovalsAction::Pending { .. }
                | ApprovalsAction::Approve { .. }
                | ApprovalsAction::Deny { .. }
        })
    );
    if attach_existing {
        let paths = Paths::resolve_read_only(cli.profile.as_deref())?;
        return match cli.cmd {
            Some(Cmd::Runs {
                action: Some(action),
                json,
                ..
            }) => cli_attach::runs(&paths, action, json),
            Some(Cmd::Sessions { action }) => sessions_cmd(paths, action),
            Some(Cmd::Approvals { action }) => approvals_cmd(&paths, action),
            _ => unreachable!("matched an attach command"),
        };
    }
    if cli.secure {
        // A daemon started from here inherits it (see `Config::apply_env`).
        std::env::set_var("FORMLESS_SECURE", "1");
    }
    // A restore preview only needs paths. Return before claiming profile
    // leases or dispatching the actual restore.
    if let Some(Cmd::Backup {
        action: Some(BackupAction::Restore {
            file,
            dry_run: true,
        }),
    }) = &cli.cmd
    {
        let paths = Paths::resolve_read_only(cli.profile.as_deref())?;
        println!(
            "{}",
            serde_json::to_string_pretty(&formless_core::backup_verification::restore_plan(
                &paths, file
            ))?
        );
        return Ok(());
    }
    // Keep every ordinary command's family leased through its final write.
    // Destructive commands resolve read-only and obtain an explicit freeze;
    // `serve` already retains the same shared lease through its owner guard.
    let destructive = matches!(
        cli.cmd,
        Some(
            Cmd::Reset { .. }
                | Cmd::Uninstall { .. }
                | Cmd::Profiles {
                    action: ProfilesAction::Rm { .. }
                }
        )
    );
    let read_only = matches!(
        cli.cmd,
        Some(Cmd::Prices {
            action: PricesAction::List { .. }
        })
    );
    let paths = Paths::resolve_read_only(cli.profile.as_deref())?;
    let long_path_hint = matches!(&cli.cmd, Some(Cmd::Doctor { .. }))
        .then(|| windows_profile_path_hint(&paths))
        .flatten();
    let profile_directory_error = |error: anyhow::Error| match &long_path_hint {
        Some(hint) => error.context(hint.clone()),
        None => error,
    };
    let _operation = if destructive || serving || read_only {
        None
    } else {
        Some(
            formless_core::profile_lock::ProfileLock::hierarchy_shared(&paths)
                .map_err(profile_directory_error)?,
        )
    };
    let paths = if destructive || read_only {
        paths
    } else {
        Paths::resolve(cli.profile.as_deref()).map_err(profile_directory_error)?
    };
    // Metadata-only early commands above deliberately avoid reading config.
    // Ordinary table surfaces share the TUI colorblind palette for this profile.
    table::configure_theme(
        &Config::load_file_only(&paths)
            .map(|cfg| cfg.tui.theme)
            .unwrap_or_default(),
    );
    match cli.cmd {
        Some(Cmd::TopicMcp { attempt }) => cli_topic_mcp::serve(&paths, &attempt),
        None | Some(Cmd::Tui { .. }) if cli.plain => formless_tui::plain::run(paths),
        None | Some(Cmd::Tui { .. }) => formless_tui::run_with(
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
        Some(Cmd::Doctor {
            restore_drill: true,
            ..
        }) => run_restore_drill(&paths, None),
        Some(Cmd::Doctor { json, fix, .. }) => {
            let healthy = if json {
                let report = doctor_json(&paths)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                report["problems"].as_array().is_some_and(Vec::is_empty)
            } else {
                doctor(paths, fix)?
            };
            if !healthy {
                return Err(DoctorProblems.into());
            }
            Ok(())
        }
        Some(Cmd::BugReport { output }) => bug_report(&paths, output),
        Some(Cmd::Mcp { action }) => mcp_cmd(paths, action),
        Some(Cmd::Skills { action }) => skills_cmd(paths, action),
        Some(Cmd::Artifacts { action }) => cli_artifacts::run(&paths, action),
        Some(Cmd::Models { action }) => {
            let code = cli_models::run(&paths, action)?;
            if code != 0 {
                exit_cli(code);
            }
            Ok(())
        }
        Some(Cmd::Modules { action }) => cli_modules::run(&paths, action),
        Some(Cmd::Memory { action }) => memory_cmd(paths, action),
        Some(Cmd::Runs {
            limit,
            json,
            action,
            session,
            since,
        }) => match action {
            Some(action) => cli_attach::runs(&paths, action, json),
            None => runs_cmd(paths, limit.unwrap_or(20), json, session, since),
        },
        Some(Cmd::Jobs { action }) => jobs_cmd(paths, action),
        Some(Cmd::Metrics { since, json }) => {
            formless_core::daemon::ensure_daemon(&paths)?;
            cli_metrics::write_report(&mut std::io::stdout().lock(), since, json, |request| {
                formless_tui::client::request_once(&paths, request)
            })
        }
        Some(Cmd::Routing { json }) => {
            formless_core::daemon::ensure_daemon(&paths)?;
            cli_routing::write_report(&mut std::io::stdout().lock(), json, |request| {
                formless_tui::client::request_once(&paths, request)
            })
        }
        Some(Cmd::Usage { by, since, json }) => {
            formless_core::daemon::ensure_daemon(&paths)?;
            cli_usage::write_report(&mut std::io::stdout().lock(), by, since, json, |request| {
                formless_tui::client::request_once(&paths, request)
            })
        }
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
        Some(Cmd::Setup) => cli_setup::run(&paths),
        Some(Cmd::Prices { action }) => match action {
            PricesAction::Import {
                provider,
                all,
                dry_run,
            } => prices_import_cmd(paths, provider, all, dry_run),
            PricesAction::List { provider, json } => {
                cli_prices::list(&paths, provider.as_deref(), json)
            }
        },
        Some(Cmd::Sessions { action }) => sessions_cmd(paths, action),
        Some(Cmd::Config { action }) => config_cmd(paths, action),
        Some(Cmd::Logs {
            follow,
            level,
            since,
            lines,
            jsonl,
        }) => logs_cmd(&paths, follow, level, since, lines, jsonl),
        Some(Cmd::Completions { .. } | Cmd::Man | Cmd::Commands { .. }) => {
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
        Some(Cmd::Update {
            action,
            check,
            json,
        }) => update_cmd(check || matches!(action, Some(UpdateAction::Check)), json),
        Some(Cmd::Tools { action }) => tools_cmd(paths, action, tool_run_input),
        Some(Cmd::Naming { action }) => naming_cmd(paths, action),
        Some(Cmd::Reset { force }) => reset_cmd(paths, force),
        Some(Cmd::Uninstall {
            keep_data,
            keep_binary,
            yes,
        }) => uninstall_cmd(&paths, keep_data, keep_binary, yes),
        Some(Cmd::Storage { json }) => storage_cmd(&paths, json),
        Some(Cmd::Docs {
            topic,
            print,
            keys,
            text,
        }) => docs_cmd(topic.as_deref(), print, keys, text),
        Some(Cmd::Gc { dry_run, json }) => gc_cmd(&paths, dry_run, json),
        Some(Cmd::Backup { action }) => match action {
            None => backup_cmd(&paths),
            Some(BackupAction::List { json }) => {
                let rows = formless_core::backup_verification::list(&paths);
                if json {
                    println!("{}", serde_json::to_string_pretty(&rows)?);
                } else {
                    let mut table = Table::new(&["FILE", "SIZE", "VERIFIED AT"]);
                    for row in rows.as_array().into_iter().flatten() {
                        table.row(vec![
                            c(row["file"].as_str().unwrap_or_default()),
                            c(formless_core::housekeeping::human_bytes(
                                row["bytes"].as_u64().unwrap_or(0),
                            )),
                            c(row["verified_at"].as_str().unwrap_or("never verified")),
                        ]);
                    }
                    table.print();
                }
                Ok(())
            }
            Some(BackupAction::Verify { file }) => {
                let report = formless_core::backup_verification::verify(&paths, &file)?;
                println!("{}", serde_json::to_string_pretty(&report)?);
                Ok(())
            }
            Some(BackupAction::Restore { file, .. }) => restore_cmd(&paths, &file),
            Some(BackupAction::Drill { file }) => run_restore_drill(&paths, file),
        },
        Some(Cmd::ExportAll {
            sessions,
            output,
            overwrite,
        }) => {
            formless_core::daemon::ensure_daemon(&paths)?;
            let bundle =
                formless_tui::client::request_once(&paths, Request::ExportAll { sessions })?;
            let file = output.unwrap_or_else(|| {
                std::path::PathBuf::from(format!(
                    "formless-export-{}.json",
                    chrono::Local::now().format("%Y%m%d-%H%M%S")
                ))
            });
            let contents = serde_json::to_string_pretty(&bundle)?;
            if overwrite {
                formless_core::config::write_secret_file(&file, &contents)?;
            } else {
                write_new_bundle(&file, &contents)?;
            }
            let n = |k: &str| bundle[k].as_array().map_or(0, Vec::len);
            println!("exported:  {}", file.display());
            println!(
                "contains:  {} Tools, {} modules, {} Skills, {} memories, {} jobs{}",
                n("tools"),
                n("modules"),
                n("skills"),
                n("memories"),
                n("jobs"),
                if sessions {
                    format!(", {} Sessions", n("sessions"))
                } else {
                    String::new()
                }
            );
            if n("blanked") > 0 {
                println!("left out:  {} secret value(s) (API keys never leave the keychain); fill them in after import", n("blanked"));
            }
            Ok(())
        }
        Some(Cmd::ImportAll { file }) => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("read {}", file.display()))?;
            let bundle: serde_json::Value =
                serde_json::from_str(&text).context("not a JSON export")?;
            formless_core::daemon::ensure_daemon(&paths)?;
            print_bundle_description_preview(&paths, &bundle)?;
            let r = formless_tui::client::request_once(&paths, Request::ImportAll { bundle })?;
            println!(
                "imported:  {} Tools, {} modules, {} Skills (disabled until you enable them), {} memories, {} jobs (paused), {} Sessions",
                r["tools"], r["modules"], r["skills"], r["memories"], r["jobs"], r["sessions"]
            );
            println!(
                "config:    {} providers, {} Roles, {} MCP servers added",
                r["providers"], r["roles"], r["mcp_servers"]
            );
            println!("review:    MCP servers are disabled and untrusted; inspect commands before enabling.");
            for server in r["mcp_review"].as_array().into_iter().flatten() {
                println!(
                    "  {}: {}",
                    server["name"].as_str().unwrap_or_default(),
                    server["target"].as_str().unwrap_or_default()
                );
            }
            println!(
                "Tool capability families: {}",
                r["tool_capability_families"]
            );
            for b in r["blanked"].as_array().into_iter().flatten() {
                println!("fill in:   {}", b.as_str().unwrap_or_default());
            }
            for f in r["failed"].as_array().into_iter().flatten() {
                println!("failed:    {}", f.as_str().unwrap_or_default());
            }
            println!("next:      formless auth login / set API keys, then `formless jobs` to enable jobs");
            description_reviews_cmd(&paths, None, None, false)?;
            Ok(())
        }
        Some(Cmd::Restore { file }) => restore_cmd(&paths, &file),
        Some(Cmd::Run {
            estimate: _,
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
            pipeline,
        }) => {
            let output = if json { RunOutput::JsonlEvents } else { output };
            // Piped input: the task itself (`-`) or an attachment.
            use std::io::{IsTerminal, Read};
            if task.as_str() == "-" && pipeline.schema.as_deref() == Some(std::path::Path::new("-"))
            {
                return Err(usage("--schema - and task '-' cannot both consume stdin"));
            }
            let piped = if std::io::stdin().is_terminal()
                && pipeline.schema.as_deref() != Some(std::path::Path::new("-"))
            {
                None
            } else {
                let mut buf = Vec::new();
                let limit = if pipeline.schema.as_deref() == Some(std::path::Path::new("-")) {
                    formless_core::pipeline::MAX_SCHEMA_BYTES
                } else {
                    formless_core::daemon::MAX_ATTACHMENT_BYTES
                };
                std::io::stdin()
                    .take((limit + 1) as u64)
                    .read_to_end(&mut buf)?;
                if buf.len() > limit {
                    return Err(usage("piped Run input exceeds its byte limit"));
                }
                Some(buf)
            };
            let mut prepared = match pipeline.prepare(task, piped) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{e}");
                    exit_cli(2);
                }
            };
            let task = prepared.task;
            let attachment = prepared.attachment;
            // `--attach` files, read and checked before anything starts.
            let mut files = match read_attachments(&attach) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("{e}");
                    exit_cli(2);
                }
            };
            if let Some(bytes) = attachment {
                files.push(("stdin.txt".into(), bytes));
            }
            let session = if continue_last || session.as_deref() == Some("last") {
                match last_run_session(&paths) {
                    Ok(id) => Some(id),
                    Err(e) => {
                        eprintln!("{e}");
                        exit_cli(2);
                    }
                }
            } else {
                session
            };
            let pick = match run_model(&paths, model, effort, role) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("{e}");
                    exit_cli(2);
                }
            };
            let pipeline_mode = pipeline.enabled();
            if pipeline_mode {
                if let Some((model, effort)) = &pick {
                    prepared.options.model = model.clone();
                    prepared.options.reasoning_effort = effort.clone();
                }
                formless_core::pipeline::CompiledRunOptions::compile(prepared.options.clone())
                    .map_err(|e| usage(e.to_string()))?;
            }
            let pick_was_set = !pipeline_mode && pick.is_some();
            // An existing Session gets its own model back after this Run.
            // The folder the Run acts in: --cwd, else where it was started
            // (a continued Session keeps its own unless --cwd is given).
            let dir = match &cwd {
                Some(d) => match d.canonicalize() {
                    Ok(p) if p.is_dir() => Some(p),
                    _ => {
                        eprintln!("--cwd {}: no such directory", d.display());
                        exit_cli(2);
                    }
                },
                None if session.is_none() => std::env::current_dir().ok(),
                None => None,
            };
            // Capture the restoration record from the same Session lookup
            // that supplies the canonical ID for the Run.
            let mut restore = None;
            let pick = if pipeline_mode { None } else { pick };
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
                prepared.options,
                pipeline.output_file,
                &mut restore,
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
            exit_cli(code);
        }
    }
}

pub fn parse_version(tag: &str) -> Option<semver::Version> {
    let tag = tag.trim().trim_start_matches('v');
    let (core, suffix) = tag.split_at(tag.find(['-', '+']).unwrap_or(tag.len()));
    // Older tags may omit the minor or patch number.
    let fill = match core.split('.').count() {
        1 => ".0.0",
        2 => ".0",
        _ => "",
    };
    let mut version = if fill.is_empty() {
        semver::Version::parse(tag).ok()?
    } else {
        semver::Version::parse(&format!("{core}{fill}{suffix}")).ok()?
    };
    // SemVer precedence ignores build metadata, including when sorting notes.
    version.build = semver::BuildMetadata::EMPTY;
    Some(version)
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
    upgrade_command_for_exe(&exe)
}

fn is_homebrew_executable(exe: &str) -> bool {
    let resolved = std::fs::canonicalize(exe).ok();
    let exe = resolved
        .as_deref()
        .map(std::path::Path::to_string_lossy)
        .unwrap_or_else(|| std::borrow::Cow::Borrowed(exe));
    exe.contains("/Cellar/") || exe.contains("/homebrew/") || exe.contains("/linuxbrew/")
}

fn upgrade_command_for_exe(exe: &str) -> &'static str {
    platform_upgrade_command(exe, cfg!(windows))
}

fn is_cargo_build_executable(exe: &str) -> bool {
    let normalized = exe.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/target/debug/") || normalized.contains("/target/release/")
}

fn is_winget_executable(exe: &str) -> bool {
    let normalized = exe.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/winget/packages/") || normalized.contains("/winget/links/")
}

fn platform_upgrade_command(exe: &str, windows: bool) -> &'static str {
    if !windows && is_homebrew_executable(exe) {
        "brew upgrade formless"
    } else if exe.contains("/.cargo/bin/")
        || (windows
            && exe
                .replace('\\', "/")
                .to_ascii_lowercase()
                .contains("/.cargo/bin/"))
    {
        "cargo install --git https://github.com/athiwatc/formless --locked formless-bin --force"
    } else if windows && is_cargo_build_executable(exe) {
        WINDOWS_SOURCE_BUILD_UPGRADE_GUIDANCE
    } else if windows && is_winget_executable(exe) {
        "winget upgrade --id athiwatc.formless --exact"
    } else if windows {
        WINDOWS_MANUAL_UPGRADE_GUIDANCE
    } else {
        // Downloaded first, so a failed download stops with an error
        // instead of piping nothing into sh (#834).
        INSTALL_COMMAND
    }
}

/// Where the install script is served: the repository's own file, the
/// same URL the README gives.
#[cfg_attr(not(test), allow(dead_code))]
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/athiwatc/formless/main/scripts/install.sh";
const INSTALL_COMMAND: &str = concat!(
    "sh -c 'install_tmp=$(mktemp -d \"${TMPDIR:-/tmp}/formless-install.XXXXXXXXXX\") || exit; ",
    "cleanup() { rm -f \"$install_tmp/install.sh\"; rmdir \"$install_tmp\"; }; trap cleanup EXIT; ",
    "curl -fsSL ",
    "https://raw.githubusercontent.com/athiwatc/formless/main/scripts/install.sh",
    " -o \"$install_tmp/install.sh\" && sh \"$install_tmp/install.sh\"'"
);
const WINDOWS_MANUAL_UPGRADE_GUIDANCE: &str = "Download and verify the latest matching Windows ZIP from https://github.com/athiwatc/formless/releases (see docs/windows-install.md#manual-updates)";
const WINDOWS_SOURCE_BUILD_UPGRADE_GUIDANCE: &str =
    "Update this source checkout and rebuild it with cargo run";

/// `formless update`: what's newer than this build and what changed in
/// each release, then how to upgrade (`--check` stops after the report).
fn update_cmd(check: bool, json: bool) -> Result<()> {
    cli_update::report(
        &mut std::io::stdout().lock(),
        check,
        json,
        env!("CARGO_PKG_VERSION"),
        upgrade_command(),
        || update_http::get("https://api.github.com/repos/athiwatc/formless/releases?per_page=50"),
    )
}

fn prompts_cmd(paths: Paths, action: PromptsAction) -> Result<()> {
    use formless_tui::client::request_once;
    formless_core::daemon::ensure_daemon(&paths)?;
    match action {
        PromptsAction::Diff { id, json } => cli_proposal_diff::show(&paths, id, json),
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
            // A self-hosted relay.url stays unless --url replaces it (#897).
            if let Some(url) = url {
                cfg.relay.url = url.trim_end_matches('/').to_string();
            }
            cfg.relay.check().map_err(|e| usage(format!("{e:#}")))?;
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
                        local_timestamp(&c.first_seen),
                        local_timestamp(&c.last_seen)
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
                #[cfg(unix)]
                use tokio::signal::unix::{signal, SignalKind};
                #[cfg(unix)]
                let mut terminate = signal(SignalKind::terminate())?;
                #[cfg(unix)]
                let mut interrupt = signal(SignalKind::interrupt())?;
                #[cfg(windows)]
                let mut interrupt = tokio::signal::windows::ctrl_c()?;
                // Keep the listeners alive during teardown so a second
                // Windows Ctrl+C is still handled while cleanup finishes.
                let stop_signal = async {
                    #[cfg(unix)]
                    tokio::select! {
                        _ = terminate.recv() => {}
                        _ = interrupt.recv() => {}
                    }
                    #[cfg(windows)]
                    interrupt.recv().await;
                };
                let d = Arc::new(Daemon::new(paths)?);
                let serve = d.clone().serve();
                tokio::pin!(serve);
                tokio::select! {
                    result = &mut serve => result,
                    _ = stop_signal => {
                        d.request_shutdown();
                        serve.await
                    }
                }
            })
        }
        DaemonAction::InstallService { dry_run } => {
            let plan = formless_core::service::plan_here(&paths)?;
            if dry_run {
                println!("{}", formless_core::service::dry_run(&plan));
                return Ok(());
            }
            formless_core::service::handover(
                &paths,
                std::time::Duration::from_secs(60),
                || {
                    formless_tui::client::request_once_within(
                        &paths,
                        Request::Shutdown,
                        std::time::Duration::from_secs(10),
                    )
                    .map(|_| ())
                },
                || formless_core::service::install(&plan),
            )?;
            println!("installed: {} ({})", plan.file.display(), plan.manager);
            println!("the daemon is configured to start at login and stay active while the user service manager is running");
            if let Some(guidance) = formless_core::service::systemd_lingering_guidance(&plan) {
                println!("{guidance}");
            }
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
        DaemonAction::Status { check: true, .. } => {
            let status = formless_tui::client::request_once_within(
                &paths,
                Request::Status,
                std::time::Duration::from_secs(5),
            )
            .context("daemon status check failed")?;
            anyhow::ensure!(
                status["pid"].as_u64().is_some_and(|pid| pid > 0)
                    && status["version"]
                        .as_str()
                        .is_some_and(|version| !version.is_empty()),
                "daemon status did not identify a live process"
            );
            Ok(())
        }
        DaemonAction::Status { json, check: false } => {
            if daemon_reachable(&paths) {
                let mut v = formless_tui::client::request_once(&paths, Request::Status)?;
                #[cfg(unix)]
                let endpoint = paths.socket_path().to_string_lossy().into_owned();
                #[cfg(windows)]
                let endpoint = paths.pipe_name();
                v["endpoint"] = serde_json::Value::String(endpoint);
                if json {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                } else {
                    print_daemon_status(&v);
                }
                for w in formless_tui::client::status_warnings(&v)? {
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
        v["pid"],
        v["version"].as_str().unwrap_or_default(),
        v["protocol"]
    );
    if let Some(up) = r["uptime_secs"].as_u64() {
        println!(
            "uptime:      {}",
            if up >= 3600 {
                format!("{}h {}m", up / 3600, up % 3600 / 60)
            } else {
                format!("{}m {}s", up / 60, up % 60)
            }
        );
    }
    if r["memory_bytes"].is_u64() {
        println!(
            "memory:      {}  ·  cpu {}% (total {})",
            b("memory_bytes"),
            r["cpu_percent"]
                .as_f64()
                .map_or("?".into(), |c| format!("{c:.1}")),
            r["cpu_time"].as_str().unwrap_or("?")
        );
    }
    println!(
        "database:    {}  ·  artifacts {}",
        b("db_bytes"),
        b("artifacts_bytes")
    );
    let running = v["running_sessions"].as_array().map_or(0, Vec::len);
    println!(
        "work:        {} Session(s) · {running} running Run(s) · {} pending Invocation(s)",
        v["sessions"], r["running_invocations"]
    );
    println!(
        "approvals:   {}",
        if v["secure"] == true {
            "on (--secure)"
        } else {
            "off"
        }
    );
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

/// Credential presence for every configured OAuth provider; diagnostics never
/// redeem rotating refresh tokens or expose token values.
fn oauth_provider_statuses(paths: &Paths, cfg: &Config) -> Vec<serde_json::Value> {
    std::iter::once((formless_core::config::DEFAULT_PROVIDER, &cfg.provider))
        .chain(cfg.providers.iter().filter(|(name, _)| name.as_str() != formless_core::config::DEFAULT_PROVIDER).map(|(n,p)| (n.as_str(),p)))
        .filter(|(_, p)| p.oauth())
        .map(|(name, p)| {
            let status = formless_core::auth::auth_status(paths, p);
            serde_json::json!({"name":name,"mode":status.mode,"credential_present":status.credential_present,
                "expires_at":status.expires_at,"has_refresh_token":status.has_refresh_token,
                "warning":status.warning,"login_command":formless_core::auth::login_command(name)})
        }).collect()
}

fn auth_cmd(paths: Paths, action: AuthAction) -> Result<()> {
    let cfg = Config::load(&paths)?;
    let selector = match &action {
        AuthAction::Login { provider, .. }
        | AuthAction::Status { provider, .. }
        | AuthAction::Logout { provider } => provider.clone(),
    };
    let name = selector
        .as_deref()
        .filter(|n| !n.is_empty())
        .unwrap_or(formless_core::config::DEFAULT_PROVIDER);
    let pcfg = cfg.provider_cfg(selector.as_deref())?;
    let login = formless_core::auth::login_command(name);
    match action {
        AuthAction::Login { no_browser, .. } => {
            if !pcfg.oauth() {
                return Err(anyhow::anyhow!(
                    "provider.auth.mode is {:?}, not \"oauth\".\n{OAUTH_CONFIG_EXAMPLE}",
                    pcfg.auth.mode
                ));
            }
            if let Err(error) = formless_core::auth::validate_oauth_config(&pcfg) {
                return Err(error.context(OAUTH_CONFIG_EXAMPLE.to_string()));
            }
            let tokens = formless_core::auth::login_with_callback(&paths, &pcfg, |url| {
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
        AuthAction::Status { json, .. } => {
            let mut status = formless_core::auth::auth_status(&paths, &pcfg);
            if !pcfg.oauth() && name != formless_core::config::DEFAULT_PROVIDER {
                status.credential_present =
                    formless_core::config::provider_key(&paths, name).is_some();
            }
            let providers = oauth_provider_statuses(&paths, &cfg);
            if json {
                let source = if !pcfg.oauth() && name != formless_core::config::DEFAULT_PROVIDER {
                    "named provider environment / keychain / 0600 file"
                } else if !pcfg.oauth() {
                    formless_core::config::api_key_source(&paths).0
                } else if std::env::var_os("FORMLESS_OAUTH_ACCESS_TOKEN").is_some() {
                    "env"
                } else {
                    "keychain / 0600 token file"
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "provider": name,
                        "mode": status.mode,
                        "credential_present": status.credential_present,
                        "source": source,
                        "expires_at": status.expires_at,
                        "has_refresh_token": status.has_refresh_token,
                        "scope": status.scope,
                        "warning": status.warning,
                        "oauth_providers": providers,
                    }))?
                );
                return Ok(());
            }
            println!("provider:       {name}");
            println!("mode:           {}", status.mode);
            if let Some(warning) = &status.warning {
                println!("WARNING: {warning}");
            }
            for p in &providers {
                println!(
                    "OAuth provider {}: {}{}",
                    p["name"].as_str().unwrap_or_default(),
                    if p["credential_present"] == true {
                        "credential present"
                    } else {
                        "MISSING"
                    },
                    p["warning"]
                        .as_str()
                        .map(|w| format!("; {w}"))
                        .unwrap_or_default()
                );
            }
            println!(
                "credential:     {}",
                if status.credential_present {
                    "present"
                } else {
                    "MISSING"
                }
            );
            if pcfg.oauth() {
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
                    println!("Run `{login}` to sign in.");
                }
            }
            Ok(())
        }
        AuthAction::Logout { .. } => {
            for line in formless_core::auth::logout_provider(&paths, &pcfg) {
                println!("{line}");
            }
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
    /// Secret files others can read.
    Chmod600(Vec<std::path::PathBuf>),
    #[cfg(unix)]
    MakeDaemonEndpointPrivate,
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
        out.extend(
            formless_core::routing_policy::routing_subset_warnings(&cfg.provider)
                .into_iter()
                .map(|what| Problem {
                    what,
                    next: None,
                    fix: None,
                }),
        );
        if cfg.relay.enabled {
            if let Err(e) = cfg.relay.check() {
                out.push(Problem {
                    what: format!("the relay will not connect: {e:#}"),
                    next: Some("formless relay enable --url wss://HOST".into()),
                    fix: None,
                });
            }
        }
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
        match formless_core::profile_lock::owner_held(paths) {
            Ok(true) => out.push(Problem {
                what: "the profile has a kernel owner but its daemon does not answer".into(),
                next: Some("stop the daemon through its service manager or original binary; PID markers do not authenticate processes".into()),
                fix: None,
            }),
            Err(e) => out.push(Problem {
                what: format!("profile ownership is unknown: {e:#}"),
                next: None,
                fix: None,
            }),
            Ok(false) => {
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
    #[cfg(unix)]
    match paths.daemon_endpoint_permissions_need_repair() {
        Ok(true) => out.push(Problem {
            what: format!(
                "the daemon data directory or socket has incorrect permissions ({})",
                paths.socket_path().display()
            ),
            next: Some("formless doctor --fix".into()),
            fix: Some(Fix::MakeDaemonEndpointPrivate),
        }),
        Ok(false) => {}
        Err(error) => out.push(Problem {
            what: format!("{error:#}"),
            next: Some("inspect the daemon data directory and socket ownership".into()),
            fix: None,
        }),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Every secret file, named secrets included (#849).
        let config_backups = std::fs::read_dir(&paths.config_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file())
                    && entry.file_name().to_string_lossy().ends_with(".toml.bak")
            })
            .map(|entry| entry.path());
        let open: Vec<std::path::PathBuf> = formless_core::config::secret_files(paths)
            .into_iter()
            .chain([paths.config_file()])
            .chain(config_backups)
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
            let _owner = formless_core::profile_lock::ProfileLock::owner(paths)
                .map_err(|e| format!("{e:#}"))?;
            if formless_core::daemon::endpoint_occupied(paths).map_err(|e| format!("{e:#}"))? {
                return Err("daemon became reachable; stale files were kept".into());
            }
            for f in files {
                std::fs::remove_file(f).map_err(|e| format!("{}: {e}", f.display()))?;
            }
            Ok("removed the stale files".into())
        }
        #[cfg(unix)]
        Fix::MakeDaemonEndpointPrivate => {
            paths
                .prepare_socket_dir()
                .and_then(|()| paths.set_daemon_socket_permissions())
                .map_err(|error| format!("{error:#}"))?;
            Ok("made the daemon directories (700) and existing socket (600) private".into())
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

#[cfg(all(test, unix))]
#[test]
fn doctor_reports_and_repairs_loose_daemon_directory_and_socket_permissions() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = std::env::temp_dir().join(format!("fl-doctor-endpoint-{}", std::process::id()));
    let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
    paths.prepare_socket_dir().unwrap();
    std::fs::set_permissions(&paths.data_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = paths.socket_path();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o777)).unwrap();
    let problems = doctor_local_problems(&paths);
    let fix = problems
        .iter()
        .find_map(|problem| match &problem.fix {
            Some(fix @ Fix::MakeDaemonEndpointPrivate) => Some(fix),
            _ => None,
        })
        .unwrap();
    assert_eq!(std::fs::metadata(&socket).unwrap().mode() & 0o777, 0o777);
    apply_fix(&paths, fix).unwrap();
    assert_eq!(
        std::fs::metadata(&paths.data_dir).unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(std::fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
    assert!(!paths.daemon_endpoint_permissions_need_repair().unwrap());
    drop(listener);
    std::fs::remove_file(socket).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Doctor already wrote the full diagnostic report before returning this verdict.
#[derive(Debug)]
struct DoctorProblems;

impl std::fmt::Display for DoctorProblems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("doctor found unresolved diagnostic problems")
    }
}

impl std::error::Error for DoctorProblems {}

#[cfg(windows)]
fn windows_profile_path_hint(paths: &Paths) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    let length = paths.data_dir.as_os_str().encode_wide().count();
    (length >= 260).then(|| format!(
        "The Windows data directory path is {length} UTF-16 code units long. If Windows rejects this path, use a shorter FORMLESS_DATA_DIR or enable Windows long paths."
    ))
}

#[cfg(not(windows))]
fn windows_profile_path_hint(_paths: &Paths) -> Option<String> {
    None
}

fn doctor(paths: Paths, fix: bool) -> Result<bool> {
    let problems = doctor_report(&paths, false)?;
    if problems.is_empty() {
        println!("\nno problems found");
        return Ok(true);
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
        return Ok(false);
    }
    if fixes.is_empty() {
        return Ok(false);
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
    Ok(left.is_empty())
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
            (r"\b(sk|pk|rk|ghp|gho|ghr|xox[abp])[-_][A-Za-z0-9_\-]{8,}", "${1}-[redacted]"),
            (r"[A-Za-z0-9+/_\-]{40,}={0,2}", "[redacted]"),
        ]
        .into_iter()
        .map(|(re, to)| (regex::Regex::new(re).expect("redaction rule"), to))
        .collect()
    });
    let native =
        directories::BaseDirs::new().map(|dirs| dirs.home_dir().as_os_str().to_os_string());
    let homes = [
        native,
        std::env::var_os("USERPROFILE"),
        std::env::var_os("HOME"),
    ];
    let mut out = redact_report_home_paths(
        text,
        homes.iter().flatten().map(|home| home.to_string_lossy()),
    );
    for (re, to) in rules {
        out = re.replace_all(&out, *to).into_owned();
    }
    out
}

fn redact_report_home_paths<'a>(
    text: &str,
    homes: impl IntoIterator<Item = std::borrow::Cow<'a, str>>,
) -> String {
    let mut spellings = Vec::new();
    for home in homes.into_iter().filter(|home| !home.is_empty()) {
        // JSON escapes native separators before the report is redacted.
        let escaped = serde_json::Value::String(home.to_string()).to_string();
        spellings.push(escaped[1..escaped.len() - 1].to_owned());
        if home.contains('\\') {
            spellings.push(home.replace('\\', "/"));
        }
        spellings.push(home.into_owned());
    }
    spellings.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    spellings.dedup();
    spellings
        .into_iter()
        .fold(text.to_owned(), |out, spelling| out.replace(&spelling, "~"))
}

/// `doctor --json`: the facts and problems, redacted.
/// The approval mode the running daemon enforces (`Status.secure`), when
/// one answers and reports it.
fn daemon_secure(paths: &Paths) -> Option<bool> {
    if !daemon_reachable(paths) {
        return None;
    }
    formless_tui::client::request_once_within(
        paths,
        Request::Status,
        std::time::Duration::from_secs(5),
    )
    .ok()?["secure"]
        .as_bool()
}

/// Database-only facts: no provider, keyring, service or daemon calls.
fn run_file_doctor_facts(paths: &Paths) -> serde_json::Value {
    if !paths.db_file().exists() {
        return serde_json::Value::Null;
    }
    match formless_core::Store::open_reader(&paths.db_file())
        .and_then(|store| store.run_file_usage())
    {
        Ok(usage) => serde_json::json!(usage),
        Err(error) => serde_json::json!({"error": error.to_string()}),
    }
}

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
        "backup": formless_core::backup_verification::newest_status(paths),
        "profile_backups": paths.list_profiles().iter().map(|p|serde_json::json!({"profile":p.profile,"newest":formless_core::backup_verification::newest_status(p)})).collect::<Vec<_>>(),
        "db_present": paths.db_file().exists(),
        "run_files": run_file_doctor_facts(paths),
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
        "oauth_providers": oauth_provider_statuses(paths, &cfg),
        "relay": { "enabled": cfg.relay.enabled, "url": cfg.relay.url },
        "approvals": {
            // The running daemon's mode when there is one (#847).
            "enforced": daemon_secure(paths).unwrap_or(cfg.security.secure),
            "from": if daemon_secure(paths).is_some() { "daemon" } else { "config" },
            "configured": cfg.security.secure,
            "trusted_paths": cfg.security.roots().len(),
            "trusted_hosts": cfg.security.trusted_hosts.len(),
        },
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
    let log = bug_report_log(paths);
    let text = format!(
        "# formless bug report\n\n\
         - formless {} on {} {}\n\
         - created {}\n\n\
         Secrets (keys, tokens, pairing links, URL credentials, long tokens) and your home path are \
         redacted. Read it through before attaching it.\n\n\
         ## What happened\n\n(describe it here)\n\n\
         ## Doctor\n\n```json\n{}\n```\n\n\
         {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        serde_json::to_string_pretty(&doctor)?,
        log,
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

/// The report consumes the shared raw-line tail, not an entire live file.
fn bug_report_log(paths: &Paths) -> String {
    match formless_core::logs::tail_lines(paths, 200, 1024 * 1024) {
        Ok(tail) => {
            let mut section = format!(
                "## Daemon log (last {} lines)\n\n```\n{}\n```\n",
                tail.lines.len(),
                redact(&tail.lines.join("\n"))
            );
            if tail.byte_limit_hit {
                section.push_str("\nLog tail incomplete: the 1 MiB read limit was reached; the leading boundary line was omitted to avoid partial content.\n");
            }
            if tail.truncated_lines > 0 {
                section.push_str(&format!(
                    "\n{} log line(s) exceeded 64 KiB and were truncated.\n",
                    tail.truncated_lines
                ));
            }
            section
        }
        Err(error) => format!(
            "## Daemon log\n\nLog history unavailable: {}\n",
            redact(&error.to_string())
        ),
    }
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
                format!(
                    "{}{}",
                    if inside { "  " } else { "" },
                    k["kind"].as_str().unwrap_or_default()
                ),
                if inside { Tone::Dim } else { Tone::Strong },
            ),
            c(human_bytes(k["bytes"].as_u64().unwrap_or(0))),
            t(k["detail"].as_str().unwrap_or_default(), Tone::Dim),
        ]);
    }
    tbl.print();
    for copy in formless_core::housekeeping::list_restore_backups(paths) {
        println!(
            "recovery:  {} (before-restore; keeps {})",
            copy.display(),
            formless_core::housekeeping::RESTORE_BACKUPS_KEEP
        );
    }
    println!(
        "total:     {} in {}",
        human_bytes(u["total_bytes"].as_u64().unwrap_or(0)),
        u["data_dir"].as_str().unwrap_or_default()
    );
    println!("free some: formless gc --dry-run");
    Ok(())
}

/// `formless gc [--dry-run] [--json]`.
fn gc_cmd(paths: &Paths, dry_run: bool, json: bool) -> Result<()> {
    use formless_core::housekeeping::human_bytes;
    let r = if daemon_reachable(paths) {
        formless_tui::client::request_once(paths, Request::Gc { dry_run })?
    } else {
        let store = formless_core::Store::open(&paths.db_file())?;
        formless_core::housekeeping::gc(paths, &store, &Config::load(paths)?, dry_run)?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
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

fn restore_cmd(paths: &Paths, file: &std::path::Path) -> Result<()> {
    if daemon_reachable(paths) {
        anyhow::bail!("stop the daemon first: formless daemon stop");
    }
    eprintln!(
        "restore: close TUIs and pause login services; exclusive profile ownership is required"
    );
    let outcome = formless_core::housekeeping::restore_report(paths, file)?;
    println!("restored:  {}", file.display());
    if outcome.saved.is_file() {
        println!("previous:  saved as {}", outcome.saved.display());
    } else {
        println!("previous:  none (the destination had no database)");
    }
    for warning in outcome.warnings() {
        eprintln!("warning: {warning}");
    }
    println!("scope:     database only; external artifact/skill files and paths were not restored or rewritten");
    Ok(())
}

fn run_restore_drill(paths: &Paths, file: Option<std::path::PathBuf>) -> Result<()> {
    let file = file
        .or_else(|| formless_core::backup_verification::copies(paths).pop())
        .ok_or_else(|| anyhow::anyhow!("no backup available for an offline restore drill"))?;
    let report = tokio::runtime::Runtime::new()?
        .block_on(formless_core::backup_verification::drill(paths, &file))
        .context("offline restore drill failed")?;
    println!("{}", serde_json::to_string_pretty(&report)?);
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
    println!("scope:     database only; copy artifacts/ and skills/ separately");
    println!("excluded:  config.toml, credential files and OS keyring entries");
    println!("paths:     recorded file paths are not rewritten for another profile or machine");
    println!("preview:   formless backup restore FILE --dry-run (plan only)");
    Ok(())
}

fn doctor_auto_routing(store: &formless_core::Store, cfg: &Config) -> Result<Vec<String>> {
    let mut lines = vec![format!(
        "routing:    auto, last {} days of eligible calls",
        cfg.provider.routing_window_days
    )];
    for (task, complex) in [("simple", false), ("complex", true)] {
        let Some(choice) =
            store.routing_auto_choice(cfg, formless_core::config::DEFAULT_PROVIDER, complex)?
        else {
            lines.push(format!("            {task} task: no candidates"));
            continue;
        };
        let reason = choice.explanation["reason"].as_str().unwrap_or_default();
        lines.push(format!(
            "            {task} task: {} ({reason})",
            choice.model
        ));
        for row in choice.explanation["candidates"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let model = row["model"].as_str().unwrap_or_default();
            let samples = row["samples"].as_u64().unwrap_or_default();
            let rate = row["good_rate"]
                .as_f64()
                .map(|rate| format!(", {:.0}% successful", 100.0 * rate))
                .unwrap_or_default();
            let cost = row["usd_per_success"]
                .as_f64()
                .map(|usd| format!(", ${usd:.6} per success"))
                .unwrap_or_default();
            lines.push(format!(
                "              {model}: {samples} samples{rate}{cost}"
            ));
        }
    }
    Ok(lines)
}

#[cfg(test)]
#[test]
fn doctor_auto_routing_ignores_all_time_counters_and_matches_the_live_selector() {
    let store = formless_core::Store::open_in_memory().unwrap();
    let mut cfg = Config::default();
    cfg.provider.model = "auto".into();
    cfg.provider.routing_models = vec!["configured-first".into(), "historical-fast".into()];
    cfg.provider.routing_window_days = 7;
    for _ in 0..20 {
        store
            .record_model_outcome("historical-fast", true, 5)
            .unwrap();
    }
    let lines = doctor_auto_routing(&store, &cfg).unwrap().join("\n");
    let choice = store
        .routing_auto_choice(&cfg, formless_core::config::DEFAULT_PROVIDER, false)
        .unwrap()
        .unwrap();
    assert_eq!(choice.model, "configured-first");
    assert!(lines.contains("last 7 days of eligible calls"));
    let reason = choice.explanation["reason"].as_str().unwrap();
    assert!(
        lines.contains(&format!("simple task: configured-first ({reason})")),
        "{lines}"
    );
    assert!(
        lines.contains(&format!("complex task: configured-first ({reason})")),
        "{lines}"
    );
    assert!(lines.contains("historical-fast: 0 samples"), "{lines}");
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
    say!(
        "help:       {}",
        formless_core::docs::url(Some("troubleshooting")).unwrap_or_default()
    );
    say!("data dir:   {}", paths.data_dir.display());
    say!("config dir: {}", paths.config_dir.display());
    say!("profile:    {}", paths.profile);
    say!(
        "db:         {} ({})",
        paths.db_file().display(),
        if paths.db_file().exists() {
            "present"
        } else {
            "absent"
        }
    );
    // Secrets go to the OS keychain and a 0600 file; without a keychain
    // only the file holds them (on Windows its ACL is the user's profile
    // directory's, not 0600) (#868).
    if formless_core::auth::keychain_available() {
        say!("keychain:   available (secrets kept in it and in 0600 files)");
    } else {
        say!("keychain:   unavailable: file-only fallback in use (0600 files in the config dir)");
    }
    let cfg = Config::load(&paths)?;
    for (name, role) in &cfg.roles {
        if cfg.provider_cfg(role.provider.as_deref()).is_err() {
            problems.push(Problem {
                what: format!("role {name:?} references unknown provider {:?}", role.provider.as_deref().unwrap_or_default()),
                next: Some("formless config edit (choose an existing role provider), or formless providers add NAME".into()),
                fix: None,
            });
        }
    }
    match Store::open(&paths.db_file()) {
        Ok(store) => {
            if let Ok((n, bytes)) = formless_core::artifacts::usage(&store) {
                say!(
                    "artifacts:  {n} ({:.1} MB; caps {} MB, {} days)",
                    bytes as f64 / 1e6,
                    cfg.artifacts.max_total_mb,
                    cfg.artifacts.max_age_days
                );
            }
        }
        Err(error) => {
            if let Some(hint) = windows_profile_path_hint(&paths) {
                problems.push(Problem {
                    what: format!("SQLite could not open the profile database ({error:#}). {hint}"),
                    next: None,
                    fix: None,
                });
            }
        }
    }
    let snapshots = run_file_doctor_facts(&paths);
    if let Some(rows) = snapshots["rows"].as_u64() {
        say!(
            "snapshots:  {rows} paths, {} retained ({:.1} MiB including metadata; run_files.* retention)",
            snapshots["retained_files"].as_u64().unwrap_or(0),
            snapshots["retained_bytes"].as_u64().unwrap_or(0) as f64 / (1024.0 * 1024.0)
        );
    } else if let Some(error) = snapshots["error"].as_str() {
        say!("snapshots:  unavailable ({error})");
    }
    match formless_core::provider::proxy_in_use(&cfg.provider) {
        Some(p) => say!("proxy:      {p} (provider.proxy or HTTPS_PROXY; NO_PROXY is honoured)"),
        None => say!("proxy:      none"),
    }
    match formless_core::service::status(&paths) {
        Ok(service) => {
            let registration = match service.registration {
                formless_core::service::Registration::Registered => "registered".to_string(),
                formless_core::service::Registration::NotRegistered => "not registered".to_string(),
                formless_core::service::Registration::Unknown(why) => {
                    format!("registration unknown ({why})")
                }
            };
            say!(
                "service:    {}; definition {} ({}: {})",
                registration,
                if service.file_present {
                    "present"
                } else {
                    "missing"
                },
                service.manager,
                service.file.display()
            );
        }
        Err(e) => say!("service:    status unknown ({e:#})"),
    }
    let recovery = formless_core::housekeeping::list_restore_backups(&paths);
    say!(
        "recovery:   {} before-restore copies (keeps {}); {}",
        recovery.len(),
        formless_core::housekeeping::RESTORE_BACKUPS_KEEP,
        formless_core::housekeeping::backup_dir(&paths).display()
    );
    for profile in paths.list_profiles() {
        let status = formless_core::backup_verification::newest_status(&profile);
        if !status.is_null() {
            say!(
                "newest backup [{}]: {}s old; {}",
                profile.profile,
                status["age_seconds"],
                status["verified_at"]
                    .as_str()
                    .map(|at| format!(
                        "verified at {at} ({}s ago)",
                        status["verification_age_seconds"]
                    ))
                    .unwrap_or_else(|| "never verified (or bytes changed)".into())
            );
        }
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
    // Dollar limits that can't count a model, and prices gone stale (#809).
    if cfg.limits.run_usd.is_some() || cfg.limits.daily_usd.is_some() {
        let mut models: Vec<(String, String)> = std::iter::once(&cfg.provider.model)
            .chain(&cfg.provider.fallback_models)
            .map(|m| ("default".to_string(), m.clone()))
            .collect();
        for r in cfg.roles.values() {
            if !r.model.is_empty() {
                models.push((
                    r.provider.clone().unwrap_or_else(|| "default".into()),
                    r.model.clone(),
                ));
            }
        }
        let unpriced: Vec<String> = models
            .into_iter()
            .filter(|(p, m)| !m.is_empty() && cfg.cost_of(p, m, 1, 1, 0, 0, None).is_none())
            .map(|(_, m)| m)
            .collect();
        if !unpriced.is_empty() {
            problems.push(Problem {
                what: format!(
                    "spending limits are set but can't count {}: no price in provider.prices",
                    unpriced.join(", ")
                ),
                next: Some("formless prices import (or add provider.prices entries)".into()),
                fix: None,
            });
        }
    }
    let stale_before = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
    let stale: Vec<&String> = std::iter::once(&cfg.provider)
        .chain(cfg.providers.values())
        .flat_map(|p| p.prices.iter())
        .filter(|(_, pr)| {
            pr.retrieved_at
                .as_deref()
                .is_some_and(|t| t < stale_before.as_str())
        })
        .map(|(m, _)| m)
        .collect();
    if !stale.is_empty() {
        problems.push(Problem {
            what: format!(
                "{} imported price(s) are over 90 days old (e.g. {})",
                stale.len(),
                stale[0]
            ),
            next: Some("formless prices import".into()),
            fix: None,
        });
    }
    // What the running daemon enforces, not what this process would (#847).
    let enforced = daemon_secure(&paths);
    let secure = enforced.unwrap_or(cfg.security.secure);
    say!(
        "approvals:  {}{} (trusted folders: {}, trusted hosts: {}; security.secure in {} or --secure)",
        if secure {
            "on: sensitive Tool calls ask first"
        } else {
            "OFF: Tools run without asking"
        },
        if enforced.is_some() { ", in the running daemon" } else { "" },
        cfg.security.roots().len(),
        cfg.security.trusted_hosts.len(),
        paths.config_file().display()
    );
    if let Some(running) = enforced {
        if running != cfg.security.secure {
            problems.push(Problem {
                what: format!(
                    "the running daemon has approvals {}, but this command's settings say {}",
                    if running { "on" } else { "off" },
                    if cfg.security.secure { "on" } else { "off" }
                ),
                next: Some(format!(
                    "formless{} daemon restart",
                    if cfg.security.secure { " --secure" } else { "" }
                )),
                fix: None,
            });
        }
    }
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
    if !quiet && formless_core::config::ProviderCfg::is_auto_model(&cfg.provider.model) {
        match Store::open(&paths.db_file()).and_then(|store| doctor_auto_routing(&store, &cfg)) {
            Ok(lines) => {
                for line in lines {
                    println!("{line}");
                }
            }
            Err(error) => println!("routing:    preview unavailable ({error:#})"),
        }
    }
    for provider in oauth_provider_statuses(&paths, &cfg) {
        let name = provider["name"].as_str().unwrap_or_default();
        say!(
            "OAuth {name}: {}",
            if provider["credential_present"] == true {
                "credential present"
            } else {
                "MISSING"
            }
        );
        if let Some(warning) = provider["warning"].as_str() {
            say!("WARNING: {warning}");
        }
        if name != formless_core::config::DEFAULT_PROVIDER && provider["credential_present"] != true
        {
            problems.push(Problem {
                what: format!("no OAuth credential for provider '{name}'"),
                next: provider["login_command"].as_str().map(str::to_string),
                fix: None,
            });
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
    if !cfg.provider.oauth() {
        // Which source the key comes from, never the key (#1083).
        let (source, file_problem) = formless_core::config::api_key_source(&paths);
        say!("key from:   {source}");
        if let Some(why) = file_problem {
            problems.push(Problem {
                what: why,
                next: Some("fix the file or unset FORMLESS_API_KEY_FILE".into()),
                fix: None,
            });
        }
    }
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
    // Read-only: a diagnostic never redeems a refresh token (#865).
    let credential = formless_core::auth::peek_provider_token(&paths, &cfg.provider);
    if let Err(error) = &credential {
        say!("auth error: {error:#}");
        if cfg.provider.is_configured() {
            problems.push(Problem {
                what: format!("no usable credential for the provider ({error})"),
                next: Some(if cfg.provider.oauth() {
                    "formless auth login".into()
                } else {
                    "formless setup, or set FORMLESS_API_KEY".into()
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
        let setup = formless_core::profile_setup::missing_setup_message(&paths);
        say!("probe:      skipped ({setup})");
        problems.push(Problem {
            what: format!("no provider is configured for profile '{}'", paths.profile),
            next: Some(setup),
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
    let too_long = || format!("duration '{s}' is too long");
    match s.parse::<u64>() {
        Ok(0) => return Err(bad()),
        Ok(secs) => return Ok(std::time::Duration::from_secs(secs)),
        Err(e) if *e.kind() == std::num::IntErrorKind::PosOverflow => return Err(too_long()),
        Err(_) => {}
    }
    let (mut total, mut num) = (0u64, String::new());
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n = num.parse::<u64>().map_err(|e| match e.kind() {
            std::num::IntErrorKind::PosOverflow => too_long(),
            _ => bad(),
        })?;
        num.clear();
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return Err(bad()),
        };
        total = n
            .checked_mul(unit)
            .and_then(|part| total.checked_add(part))
            .ok_or_else(too_long)?;
    }
    if !num.is_empty() || total == 0 {
        return Err(bad());
    }
    Ok(std::time::Duration::from_secs(total))
}

fn cutoff_at(value: &str, now: chrono::DateTime<chrono::Utc>) -> Result<String, String> {
    let duration = parse_duration(value)?;
    let delta = chrono::Duration::from_std(duration)
        .map_err(|_| "duration exceeds the supported time range".to_string())?;
    now.checked_sub_signed(delta)
        .map(|cutoff| cutoff.to_rfc3339())
        .ok_or_else(|| "duration produces an unrepresentable UTC cutoff".to_string())
}

fn parse_since_cutoff(value: &str) -> Result<String, String> {
    cutoff_at(value, chrono::Utc::now())
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
    /// Stream the answer and progress as readable text
    Text,
    /// Print only the final answer on standard output
    Final,
    /// Print one result object after the Run ends
    Json,
    /// Print each actual Run event as one JSON line
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
fn read_last_run_session_id(paths: &Paths) -> Result<String, String> {
    std::fs::read_to_string(paths.last_run_session_file())
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "no earlier `formless run` Session to continue; run once without --continue".into()
        })
}

fn missing_last_run_session(id: &str) -> String {
    format!("the last `formless run` Session ({id}) no longer exists; run once without --continue")
}

fn check_last_run_session(paths: &Paths, id: String) -> Result<String, String> {
    match formless_tui::client::request_once(paths, Request::GetSession { id: id.clone() }) {
        Ok(_) => Ok(id),
        Err(e) if !e.to_string().contains("session not found") => Err(format!("{e:#}")),
        Err(_) => Err(missing_last_run_session(&id)),
    }
}

fn prepare_existing_headless_session(
    session: SessionInfo,
    pick: &mut Option<ModelPick>,
    override_cwd: bool,
    restore: &mut Option<SessionInfo>,
) -> String {
    let id = session.id.clone();
    if let Some((model, _)) = pick {
        // An effort-only override keeps the existing Session model.
        if model.is_none() {
            *model = session.model.clone();
        }
    }
    if pick.is_some() || override_cwd {
        *restore = Some(session);
    }
    id
}

fn last_run_session(paths: &Paths) -> Result<String, String> {
    let id = read_last_run_session_id(paths)?;
    formless_core::daemon::ensure_daemon(paths).map_err(|e| format!("{e:#}"))?;
    check_last_run_session(paths, id)
}

#[allow(clippy::too_many_arguments)]
fn run_headless(
    paths: Paths,
    task: String,
    files: Vec<(String, Vec<u8>)>,
    session: Option<String>,
    mut pick: Option<ModelPick>,
    dir: Option<std::path::PathBuf>,
    yes: bool,
    answer: Option<String>,
    output: RunOutput,
    quiet: bool,
    timeout: Option<std::time::Duration>,
    max_steps: Option<u32>,
    options: formless_protocol::RunOptions,
    output_file: Option<std::path::PathBuf>,
    restore: &mut Option<SessionInfo>,
) -> Result<i32> {
    use std::io::IsTerminal;
    let json_out = output == RunOutput::JsonlEvents;
    // The answer streams only in text mode; progress goes to stderr in
    // text mode, and in final mode when someone is watching.
    let policy = formless_core::pipeline::CompiledRunOptions::compile(options.clone())?;
    let stream = output == RunOutput::Text && !policy.has_schema() && output_file.is_none();
    let mut live_feedback = formless_tui::plain::LiveFeedback::default();
    let mut event_output = Vec::new();
    let mut target = cli_run_identity::Target::default();
    let progress = !quiet
        && match output {
            RunOutput::Text => true,
            RunOutput::Final => std::io::stderr().is_terminal(),
            _ => false,
        };
    let interactive = matches!(output, RunOutput::Text | RunOutput::Final);
    let mut final_answer: Option<String> = None;
    let mut validated_result: Option<Value> = None;
    let mut run_error: Option<String> = None;
    let mut paused_exit_code = None;
    let mut queued_delivery = false;
    let mut budget_stopped = false;
    let mut summary = RunSummary::default();
    let cfg = Config::load(&paths)?;
    if !cfg.provider.is_configured() {
        eprintln!(
            "{}",
            formless_core::profile_setup::missing_setup_message(&paths)
        );
        return Ok(2);
    }
    formless_core::daemon::ensure_daemon(&paths)?;
    // Asked for approvals, the daemon has none: refuse, whatever --quiet says.
    if let Err(e) = formless_tui::client::require_secure_daemon(&paths) {
        eprintln!("error: {e:#}");
        return Ok(2);
    }
    if let Some(warning) = formless_tui::client::check_daemon_version(&paths)? {
        if !quiet {
            eprintln!("warning: {warning}");
        }
    }
    let (client, rx) = formless_tui::client::DaemonClient::connect(&paths)?;
    // Resolve/create session.
    let session_id = match session {
        // An id, an id prefix or a name, confirmed to exist (#893).
        Some(id) => match find_session(&paths, &id) {
            Ok(s) => prepare_existing_headless_session(s, &mut pick, dir.is_some(), restore),
            Err(e) => {
                eprintln!("{e}");
                return Ok(2);
            }
        },
        None => {
            let id = client.send(Request::CreateSession {
                name: Some("headless".into()),
            });
            loop {
                match rx.recv() {
                    Ok(ServerMessage::Response {
                        id: rid,
                        ok,
                        result,
                        error,
                    }) if rid == id => {
                        if !ok {
                            let error = error
                                .as_deref()
                                .unwrap_or("the daemon could not create a Session");
                            eprintln!("error: {error}");
                            return Ok(2);
                        }
                        let s: SessionInfo = serde_json::from_value(result.unwrap_or_default())?;
                        break s.id;
                    }
                    Ok(_) => {}
                    Err(e) => return Err(anyhow::anyhow!("daemon closed: {e}")),
                }
            }
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
    // Requests whose failure means the Run can't go as asked (#893).
    let mut critical = std::collections::HashSet::new();
    if let Some((model, effort)) = pick {
        critical.insert(client.send(Request::SetSessionModel {
            id: session_id.clone(),
            model,
            reasoning_effort: effort,
        }));
    }
    for (name, bytes) in files {
        use base64::Engine;
        // The daemon guesses the type from the name (images go to vision
        // models inline), as for /attach.
        critical.insert(client.send(Request::AttachFile {
            session_id: session_id.clone(),
            name,
            data_b64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime: None,
        }));
    }
    if max_steps.is_some() {
        client.send(Request::SetStepLimit {
            session_id: session_id.clone(),
            max_steps,
        });
    }
    let mut sends = formless_tui::client::SendTracker::default();
    if cfg.limits.forecast_confirm_usd.is_some() {
        let forecast: formless_protocol::RunForecast =
            serde_json::from_value(headless_preview_request(
                &client,
                &rx,
                Request::ForecastRun {
                    for_send: true,
                    input: formless_protocol::ForecastInput {
                        session_id: Some(session_id.clone()),
                        draft: task.clone(),
                        options: options.clone(),
                        model: options.model.clone(),
                        ..Default::default()
                    },
                },
            )?)?;
        if forecast.confirmation_required {
            if !yes {
                eprintln!(
                    "{}; confirmation required (pass --yes to accept this forecast)",
                    forecast.hint
                );
                return Ok(2);
            }
            let id = forecast
                .confirmation_id
                .context("forecast confirmation token missing")?;
            headless_preview_request(&client, &rx, Request::ConfirmForecast { id })?;
        }
    }
    let send_request = if options.is_empty() {
        sends.send(&client, session_id.clone(), task, SendMode::Send)
    } else {
        client.send(Request::StartPipelineRun {
            session_id: session_id.clone(),
            content: task,
            options: options.clone(),
            client_msg_id: Some(format!("pipeline-{}", std::process::id())),
        })
    };
    critical.insert(send_request);
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
                    if let Some(run_id) = &target.run_id {
                        client.send(Request::CancelRunId {
                            run_id: run_id.clone(),
                        });
                    }
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
                if let Some(run_id) = &target.run_id {
                    client.send(Request::CancelRunId {
                        run_id: run_id.clone(),
                    });
                }
            }
            1 => {}
            _ => return Ok(130),
        }
        let msg = if let Some(event) = target.next() {
            Ok(ServerMessage::Event(event))
        } else {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(msg) => Ok(msg),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(e) => Err(e),
            }
        };
        if let Ok(ServerMessage::Response { id, .. }) = &msg {
            sends.answered(*id);
        }
        match msg {
            Ok(ServerMessage::Event(ev)) => {
                // The daemon broadcasts every Session's events; only act on ours.
                if ev.session_id() != Some(session_id.as_str()) {
                    continue;
                }
                if target.waiting() {
                    target.buffer(ev)?;
                    continue;
                }
                if !target.accepts(&ev)? {
                    continue;
                }
                if progress {
                    if let Some(text) = live_feedback.event(&ev) {
                        eprintln!("{text}");
                    }
                }
                if json_out {
                    let line = format!("{}\n", serde_json::to_string(&ev)?);
                    if output_file.is_some() {
                        if event_output.len().saturating_add(line.len())
                            > formless_core::pipeline::MAX_RESULT_FILE_BYTES
                        {
                            if let Some(run_id) = &target.run_id {
                                client.send(Request::CancelRunId {
                                    run_id: run_id.clone(),
                                });
                            }
                            return Err(anyhow::anyhow!("Run event output exceeded the 8 MiB result-file limit; output file unchanged"));
                        }
                        event_output.extend_from_slice(line.as_bytes());
                    } else {
                        print!("{line}");
                    }
                }
                match ev {
                    Event::RunStarted { .. } => {}
                    Event::RunFilesChanged {
                        files,
                        untracked_writes_possible,
                        ..
                    } => {
                        if progress {
                            write_run_file_notice(
                                &mut std::io::stderr().lock(),
                                &files,
                                untracked_writes_possible,
                            );
                        }
                    }
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
                            if policy.has_schema() {
                                match policy.validate_final(&message.content) {
                                    Ok(value) => validated_result = Some(value),
                                    Err(_) => {
                                        final_answer = None;
                                        validated_result = None;
                                        run_error = Some(
                                            "schema: daemon delivered an invalid final answer"
                                                .into(),
                                        );
                                        code = 3;
                                        break;
                                    }
                                }
                            }
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
                        capabilities,
                        context,
                        ..
                    } => {
                        if progress {
                            for capability in &capabilities {
                                if let Some(warning) =
                                    formless_protocol::sensitive_read_warning(capability)
                                {
                                    eprintln!("⚠ {capability}: {warning}");
                                }
                            }
                            if let Some(context) = &context {
                                for line in formless_protocol::approval_context_lines(context) {
                                    eprintln!("{line}");
                                }
                            }
                            eprintln!(
                                "[approval] {} -> {}",
                                tool_name,
                                if yes {
                                    "sending approval for this call (--yes)"
                                } else {
                                    "sending denial (pass --yes or approve in the TUI)"
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
                        let schema_mismatch = error.starts_with("schema:");
                        budget_stopped = error.starts_with("budget:");
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
                        code = if schema_mismatch { 3 } else { 1 };
                        paused_exit_code = Some(code);
                        // The terminal tail persists and emits the recap before
                        // RunFinished. Keep receiving it after the early pause
                        // notice, including in JSONL event output.
                    }
                    Event::RunFinished { status, .. } => {
                        code = match status.as_str() {
                            "paused" => paused_exit_code.unwrap_or(1),
                            "completed" => 0,
                            "interrupted" => 130,
                            _ => 1,
                        };
                        break;
                    }
                    _ => {}
                }
            }
            // A failed send, attachment or model choice: the Run won't
            // happen as asked, so say so and stop instead of waiting.
            Ok(ServerMessage::Response {
                id,
                ok: false,
                error,
                ..
            }) if critical.contains(&id) => {
                let e = error.unwrap_or_else(|| "daemon error".into());
                eprintln!("error: {e}");
                run_error = Some(e);
                code = 2;
                break;
            }
            Ok(ServerMessage::Response {
                id,
                ok: true,
                result: Some(result),
                ..
            }) if critical.contains(&id) => {
                if id == send_request {
                    if target.acknowledge(&result)? == cli_run_identity::Acknowledged::Queued {
                        queued_delivery = true;
                        code = if interrupting { 130 } else { 2 };
                        eprintln!("Message queued and persisted in Session {session_id}; no Run ID or foreground result is available for this message. Inspect delivery with `formless sessions show {session_id}`; add `--follow` once a Run is active. Result output files are unchanged.");
                        break;
                    }
                    if interrupting || timed_out.is_some() {
                        if let Some(run_id) = &target.run_id {
                            client.send(Request::CancelRunId {
                                run_id: run_id.clone(),
                            });
                        }
                    }
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
    let status = if queued_delivery && code == 2 {
        "queued"
    } else {
        match code {
            0 => "completed",
            124 => "timed_out",
            130 => "interrupted",
            3 => "schema_mismatch",
            _ if run_error.is_some() => "paused",
            _ => "failed",
        }
    };
    if progress {
        eprintln!(
            "{}",
            summary.line(&status.replace('_', " "), started.elapsed())
        );
    }
    if policy.has_schema() && code != 0 {
        final_answer = None;
        validated_result = None;
    }
    if budget_stopped {
        let spend = target
            .run_id
            .as_ref()
            .and_then(|run_id| {
                formless_tui::client::request_once(
                    &paths,
                    Request::GetRun {
                        run_id: run_id.clone(),
                    },
                )
                .ok()
            })
            .map(|run| run["spend"].clone());
        if let Some(spend) = spend {
            if let Some(known) = spend["known_usd"].as_f64() {
                let qualifier = if spend["total_usd"].is_number() {
                    ""
                } else {
                    "at least "
                };
                let basis = if spend["usage_records"] == 0 && spend["admitted_attempts"] == 0 {
                    " (no model dispatch admitted)"
                } else if spend["uses_local_prices"] == true {
                    " (from reported usage and local prices)"
                } else if spend["usage_records"] == 0 {
                    " (no provider usage reported)"
                } else {
                    " (provider-reported charges)"
                };
                eprintln!(
                    "observed spend: {qualifier}${known:.6}{basis}{}",
                    if spend["total_usd"].is_null() {
                        "; total unknown/incomplete"
                    } else {
                        ""
                    }
                );
            } else {
                eprintln!("observed spend: unknown (usage unavailable)");
            }
        } else {
            eprintln!("observed spend: unknown (Run usage unavailable)");
        }
    }
    if let Some(path) = output_file {
        if code == 0 {
            anyhow::ensure!(
                final_answer.is_some() || output == RunOutput::JsonlEvents,
                "Run completed without a final answer; output file unchanged"
            );
            let bytes = if output == RunOutput::JsonlEvents {
                event_output
            } else {
                cli_pipeline::render_result(
                    output,
                    &session_id,
                    status,
                    final_answer.as_deref(),
                    validated_result.as_ref(),
                    run_error.as_deref(),
                )?
            };
            let receipt = formless_core::pipeline::write_result_file(&path, &bytes)?;
            if let Some(warning) = receipt.durability_warning {
                eprintln!("{warning}");
            }
        }
    } else if output == RunOutput::Json
        || output == RunOutput::Final
        || (output == RunOutput::Text && policy.has_schema())
    {
        if output == RunOutput::Json || final_answer.is_some() {
            let bytes = cli_pipeline::render_result(
                output,
                &session_id,
                status,
                final_answer.as_deref(),
                validated_result.as_ref(),
                run_error.as_deref(),
            )?;
            std::io::stdout().write_all(&bytes)?;
        }
    }
    Ok(code)
}

/// Setup replies and the forecast use the same connection. No send intent is
/// allocated until consent, so a cached failed send is never replayed as approval.
fn headless_preview_request(
    client: &formless_tui::client::DaemonClient,
    rx: &std::sync::mpsc::Receiver<ServerMessage>,
    request: Request,
) -> Result<serde_json::Value> {
    let wanted = client.send(request);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let message =
            rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now()))?;
        if let ServerMessage::Response {
            id,
            ok,
            result,
            error,
        } = message
        {
            if !ok {
                return Err(anyhow::anyhow!(
                    "{}",
                    error.unwrap_or_else(|| "setup/forecast rejected".into())
                ));
            }
            if id == wanted {
                return Ok(result.unwrap_or_default());
            }
        }
    }
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

fn print_description_evidence(value: &Value) {
    let metadata: formless_protocol::DescriptionMetadata =
        serde_json::from_value(value["description_review"].clone()).unwrap_or_else(|_| {
            formless_core::descriptions::screen(value["description"].as_str().unwrap_or_default())
        });
    if !metadata.flags.is_empty() {
        println!("Description warning: {}", metadata.flags.join("; "));
        println!(
            "Third-party metadata: {}",
            serde_json::to_string(&metadata.text).unwrap()
        );
    }
    for review in value["description_reviews"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Ok(review) =
            serde_json::from_value::<formless_protocol::DescriptionReview>(review.clone())
        {
            println!("{}", formless_core::descriptions::notice(&review));
        }
    }
}

fn description_reviews_cmd(
    paths: &Paths,
    kind: Option<String>,
    name: Option<String>,
    json: bool,
) -> Result<()> {
    formless_core::daemon::ensure_daemon(paths)?;
    let rows = formless_tui::client::request_once(
        paths,
        Request::ListDescriptionReviews {
            kind,
            name,
            limit: Some(200),
        },
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        let reviews: Vec<formless_protocol::DescriptionReview> = serde_json::from_value(rows)?;
        if reviews.is_empty() {
            println!("No description reviews yet.");
        }
        for review in reviews {
            println!("{}\n", formless_core::descriptions::notice(&review));
        }
    }
    Ok(())
}

fn write_new_bundle(path: &std::path::Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("stage export in {}", parent.display()))?;
    staged.write_all(contents.as_bytes())?;
    staged.as_file().sync_all()?;
    match staged.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "{} already exists. Choose a new output path or pass --overwrite with --output.",
                path.display()
            );
        }
        Err(error) => {
            Err(error.error).with_context(|| format!("publish export {}", path.display()))
        }
    }
}

fn print_bundle_description_preview(paths: &Paths, bundle: &Value) -> Result<()> {
    let previews = formless_tui::client::request_once(
        paths,
        Request::PreviewDescriptionBundle {
            bundle: bundle.clone(),
        },
    )?;
    for preview in previews.as_array().into_iter().flatten() {
        let after: formless_protocol::DescriptionMetadata =
            serde_json::from_value(preview["after"].clone())?;
        println!(
            "{} {} · third-party description metadata",
            preview["kind"].as_str().unwrap_or_default(),
            preview["name"].as_str().unwrap_or_default()
        );
        if !preview["before"].is_null() {
            println!("Existing name (imports may rename): {}", preview["before"]);
        }
        println!("Incoming: {}", serde_json::to_string(&after.text)?);
        for flag in after.flags {
            println!("Warning: {flag}");
        }
    }
    Ok(())
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
        SkillsAction::Reviews { name, json } => {
            description_reviews_cmd(&paths, Some("skill".into()), name, json)?;
        }
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
            print_description_evidence(&info);
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
                if let Some(flags) = s["description_review"]["flags"]
                    .as_array()
                    .filter(|flags| !flags.is_empty())
                {
                    desc.push_str(&format!(
                        " · Warning: {}",
                        flags
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
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
            print_description_evidence(&r["skill"]);
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
                let name = name.ok_or_else(|| usage("give a skill name or --all"))?;
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
            let bundle = formless_core::bundle_json::read(&file)?;
            print_bundle_description_preview(&paths, &bundle)?;
            let imported = daemon(Request::ImportSkill { bundle })?;
            for info in imported.as_array().into_iter().flatten() {
                print_description_evidence(info);
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
    if !no_open && !formless_core::docs::open_url(&url) {
        println!("(could not open a browser; open the link above)");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum SessionExportFormat {
    /// Re-importable JSON Session bundle
    Json,
    /// Readable Markdown transcript (alias: md)
    #[value(alias = "md")]
    Markdown,
    /// Standalone HTML transcript
    Html,
}

impl SessionExportFormat {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Markdown => "markdown",
            Self::Html => "html",
        }
    }

    fn from_extension(path: &std::path::Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "json" => Some(Self::Json),
            "md" | "markdown" => Some(Self::Markdown),
            "html" | "htm" => Some(Self::Html),
            _ => None,
        }
    }
}

fn session_export_format(
    explicit: Option<SessionExportFormat>,
    output: Option<&std::path::Path>,
) -> Result<SessionExportFormat> {
    let inferred = output.and_then(SessionExportFormat::from_extension);
    if let (Some(explicit), Some(inferred)) = (explicit, inferred) {
        if explicit != inferred {
            return Err(usage(format!("--format {} conflicts with output extension for {} ({}); choose a matching format or filename",
                explicit.wire_name(), output.unwrap().display(), inferred.wire_name())));
        }
    }
    Ok(explicit.or(inferred).unwrap_or(SessionExportFormat::Json))
}

fn validate_cli_export_format(cli: &Cli) -> Result<()> {
    if let Some(Cmd::Sessions {
        action: SessionsAction::Export { format, output, .. },
    }) = &cli.cmd
    {
        session_export_format(*format, output.as_deref())?;
    }
    Ok(())
}

// Descriptor aliases keep the old stream-write behavior, even when redirected
// to a regular file. Renaming their underlying pathname would detach that fd.
fn export_descriptor_path(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        if ["/dev/stdout", "/dev/stderr", "/dev/stdin"]
            .iter()
            .any(|alias| path == std::path::Path::new(alias))
        {
            return true;
        }
        let parent = path.parent();
        let fd = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.parse::<u32>().is_ok());
        if fd
            && parent.is_some_and(|parent| {
                parent == std::path::Path::new("/dev/fd")
                    || (parent.starts_with("/proc")
                        && parent.file_name() == Some(std::ffi::OsStr::new("fd")))
            })
        {
            return true;
        }
    }
    #[cfg(windows)]
    {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let name = name
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        if matches!(
            name.as_str(),
            "NUL" | "CON" | "CONIN$" | "CONOUT$" | "PRN" | "AUX"
        ) || (name.len() == 4
            && (name.starts_with("COM") || name.starts_with("LPT"))
            && matches!(name.as_bytes()[3], b'1'..=b'9'))
        {
            return true;
        }
    }
    false
}

fn export_descriptor_alias(path: &std::path::Path) -> bool {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    export_descriptor_path(path)
        || std::fs::canonicalize(parent)
            .ok()
            .zip(path.file_name())
            .is_some_and(|(parent, name)| export_descriptor_path(&parent.join(name)))
}

#[cfg(unix)]
fn atomic_session_export_with(
    destination: &std::path::Path,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    struct Staged {
        path: std::path::PathBuf,
        committed: bool,
    }
    impl Drop for Staged {
        fn drop(&mut self) {
            if !self.committed {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    destination
        .file_name()
        .context("export destination has no filename")?;
    let metadata = std::fs::metadata(destination).ok();
    let permissions = metadata
        .as_ref()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.permissions());
    // Existing private exports must never become more readable while staged.
    // New outputs retain the same 0666-and-umask semantics as fs::write.
    let mode = if metadata
        .as_ref()
        .is_some_and(|metadata| !metadata.is_file())
    {
        0o600 // Invalid directory destinations must not stage readable data.
    } else {
        permissions
            .as_ref()
            .map_or(0o666, |permissions| permissions.mode())
    };
    let mut staged_file = None;
    for _ in 0..64 {
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = parent.join(format!(
            ".formless-export-{}-{serial}.tmp",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
        {
            Ok(file) => {
                staged_file = Some((
                    Staged {
                        path,
                        committed: false,
                    },
                    file,
                ));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let (mut staged, mut file) = staged_file.context("cannot reserve an export staging file")?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)?;
    }
    write(&mut file)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&staged.path, destination)?;
    staged.committed = true;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
mod session_export_windows;

fn write_regular_session_export(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        atomic_session_export_with(path, |file| file.write_all(bytes))
    }
    #[cfg(windows)]
    {
        session_export_windows::write(path, bytes).map_err(Into::into)
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Platforms other than Unix/Windows retain their previous output path.
        eprintln!("warning: atomic export replacement preserving access controls is not available on this platform; retaining direct output");
        std::fs::write(path, bytes).map_err(Into::into)
    }
}

fn write_session_export(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut destination = path.to_path_buf();
    // Follow leaf links (including dangling ones) as the previous fs::write did,
    // but stop cycles. Parent links retain normal filesystem resolution.
    for _ in 0..64 {
        if export_descriptor_alias(&destination) {
            return std::fs::write(&destination, bytes).map_err(Into::into);
        }
        match std::fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&destination)?;
                destination = if target.is_absolute() {
                    target
                } else {
                    destination
                        .parent()
                        .unwrap_or(std::path::Path::new("."))
                        .join(target)
                };
            }
            Ok(metadata) => {
                if !metadata.is_file() && !metadata.is_dir() {
                    // Never rename a regular file over a FIFO/device/socket.
                    return std::fs::write(&destination, bytes).map_err(Into::into);
                }
                if metadata.is_file() {
                    // Preserve existing write-access errors without truncating
                    // the old export before the atomic replacement is ready.
                    drop(std::fs::OpenOptions::new().write(true).open(&destination)?);
                }
                return write_regular_session_export(&destination, bytes);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return write_regular_session_export(&destination, bytes);
            }
            Err(error) => return Err(error.into()),
        }
    }
    bail!("export destination has too many symbolic links")
}

#[derive(Subcommand)]
enum SessionsAction {
    /// Inventory stored sub-agents for a resolved Session
    Subagents {
        /// Session id, unambiguous prefix or name
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Preserve the complete daemon sub-agent array as JSON
        #[arg(long)]
        json: bool,
    },
    /// Read a stored sub-agent and its unpaged returned transcript
    #[command(
        long_about = "Read a stored sub-agent by its full exact id, with the unpaged provider-shaped messages returned by the existing daemon handler. Malformed stored JSON rows may be skipped by that handler; this is not a dump of every stored row or a separate integrity check. No Run starts or Session changes."
    )]
    Subagent {
        /// Full exact sub-agent id reported by the inventory (no prefix lookup)
        id: String,
        /// Preserve the complete daemon record/messages object as JSON
        #[arg(long)]
        json: bool,
    },

    /// Read recorded Session usage and the current prompt/context estimate
    Usage {
        /// Session selector, using the existing ID/prefix/name resolution
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Print both complete daemon reports as one JSON value
        #[arg(long)]
        json: bool,
    },
    /// Fork stored history without starting a Run
    Fork {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Name for the new branch Session
        #[arg(long)]
        name: Option<String>,
        /// Last stored message sequence to include in the fork
        #[arg(long)]
        up_to_seq: Option<i64>,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Compare stored branch messages and recorded local usage (no model call)
    Compare {
        /// First Session ID or unambiguous prefix, in comparison order
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        left: String,
        /// Second Session ID or unambiguous prefix, in comparison order
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        right: String,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// List Sessions (archived ones only with --archived)
    List {
        /// List archived Sessions instead of active Sessions
        #[arg(long)]
        archived: bool,
        /// Keep only pinned Sessions (client-side, combined with other filters)
        #[arg(long)]
        pinned: bool,
        /// Exact stored tag after lowercasing TAG; a leading # is not stripped
        #[arg(long)]
        tag: Option<String>,
        /// One JSON array of SessionInfo
        #[arg(long)]
        json: bool,
    },
    /// Pin a Session using its stored returned state
    Pin {
        /// Session ID, unambiguous prefix or full name
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Print the returned Session as JSON
        #[arg(long)]
        json: bool,
    },
    /// Unpin a Session using its stored returned state
    Unpin {
        /// Session ID, unambiguous prefix or full name
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Print the returned Session as JSON
        #[arg(long)]
        json: bool,
    },
    /// Read tags, or replace the entire stored tag list
    #[command(
        after_help = "Examples:\n  formless sessions tag ID                 # read only\n  formless sessions tag ID work travel     # replace all tags\n  formless sessions tag ID --clear         # replace with no tags"
    )]
    Tag {
        /// Session ID, unambiguous prefix or full name
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Replacement tags, passed unchanged to storage; omission only reads
        tags: Vec<String>,
        /// Explicitly replace all tags with an empty list
        #[arg(long, conflicts_with = "tags")]
        clear: bool,
        /// Print the stored or returned Session as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show a Session's transcript
    Show {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Observe the current/latest Run without interrupting it on Ctrl+C
        #[arg(long)]
        follow: bool,
        /// Show only the newest N stored messages from the fetched transcript
        #[arg(long, conflicts_with = "follow")]
        last: Option<std::num::NonZeroUsize>,
        /// Tagged Event lines, as with run --output jsonl-events
        #[arg(long, requires = "follow")]
        jsonl: bool,
        /// The Session and its messages as JSON (Event lines with --follow)
        #[arg(long)]
        json: bool,
    },
    /// Show a Run's tracked files and diffs
    Files {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Run ID whose tracked files to inspect; omit for the latest Run
        #[arg(long)]
        run: Option<String>,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Attempt safe restoration of one path or all tracked files
    Rewind {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Tracked path to restore; omit to consider all tracked files
        path: Option<String>,
        /// Run ID whose tracked files to restore; omit for the latest Run
        #[arg(long)]
        run: Option<String>,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Rename a Session
    Rename {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// New display name for the Session
        name: String,
    },
    /// Delete a Session (restorable for daemon.delete_undo_secs)
    Rm {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
    },
    /// Bring back a Session deleted moments ago (full id)
    Restore {
        /// Full ID of the recently deleted Session
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
    },
    /// Import a Session bundle (from `sessions export --format json`)
    Import {
        /// Path to an exported JSON Session bundle
        file: std::path::PathBuf,
    },
    /// Search every Session's messages
    Search {
        /// Text to find in stored Session messages
        query: String,
        /// Maximum matching messages (1–200; default 20)
        #[arg(long, default_value_t = 20, value_parser = parse_session_search_limit)]
        limit: usize,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Read saved Session templates (separate from local commands list)
    #[command(
        after_help = "Examples:\n  formless sessions templates list --json\n  formless sessions templates show NAME\n\nNames are exact and case-sensitive. List order comes from the daemon; show prints complete stored instructions and first message. Unset and empty values are distinct. These commands only read saved templates."
    )]
    Templates {
        #[command(subcommand)]
        action: cli_session_templates::Action,
    },
    /// Hide a Session from the default list (kept and searchable)
    Archive {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
    },
    /// Bring an archived Session back
    Unarchive {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
    },
    /// Export a Session: the re-importable JSON bundle, or a readable
    /// Markdown or HTML transcript
    Export {
        /// Session ID or unambiguous prefix
        #[arg(value_parser = clap::builder::NonEmptyStringValueParser::new())]
        id: String,
        /// Export format; omit to infer from the output extension, otherwise json
        #[arg(long, value_enum)]
        format: Option<SessionExportFormat>,
        /// Write here instead of standard output; recognized extensions must match --format
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
}

fn sessions_cmd(paths: Paths, action: SessionsAction) -> Result<()> {
    if let SessionsAction::Export { format, output, .. } = &action {
        session_export_format(*format, output.as_deref())?;
    }

    let action = match action {
        SessionsAction::Show {
            id,
            follow: true,
            json,
            jsonl,
            ..
        } => {
            let code = cli_attach::follow(&paths, &id, json || jsonl)?;
            if code != 0 {
                exit_cli(code);
            }
            return Ok(());
        }
        SessionsAction::Compare { left, right, json } => {
            return sessions_compare_cmd(&paths, &left, &right, json)
        }
        other => other,
    };
    formless_core::daemon::ensure_daemon(&paths)?;
    let daemon = |r| formless_tui::client::request_once(&paths, r);
    match action {
        SessionsAction::Usage { id, json } => {
            let session = find_session(&paths, &id)?;
            cli_session_usage::write_report(
                &mut std::io::stdout().lock(),
                &session.id,
                json,
                daemon,
            )?;
        }

        SessionsAction::Fork {
            id,
            name,
            up_to_seq,
            json,
        } => {
            let source = find_session(&paths, &id)?;
            let fork: SessionInfo = serde_json::from_value(daemon(Request::ForkSession {
                id: source.id,
                name,
                up_to_seq,
            })?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&fork)?);
            } else {
                println!(
                    "forked {} ({}) from {} at seq {}",
                    fork.name,
                    fork.id,
                    fork.forked_from.as_deref().unwrap_or("unknown"),
                    fork.fork_point_seq.unwrap_or(0)
                );
            }
        }
        SessionsAction::Compare { .. } => unreachable!("handled before daemon startup"),
        SessionsAction::Pin { id, json } => cli_session_org::pin(&paths, &id, true, json)?,
        SessionsAction::Unpin { id, json } => cli_session_org::pin(&paths, &id, false, json)?,
        SessionsAction::Tag {
            id,
            tags,
            clear,
            json,
        } => cli_session_org::tag(&paths, &id, tags, clear, json)?,
        SessionsAction::List {
            archived,
            pinned,
            tag,
            json,
        } => {
            let list: Vec<SessionInfo> = serde_json::from_value(daemon(Request::ListSessions)?)?;
            let normalized_tag = tag.as_ref().map(|tag| tag.to_lowercase());
            let shown: Vec<&SessionInfo> = list
                .iter()
                .filter(|session| {
                    session.archived == archived
                        && (!pinned || session.pinned)
                        && normalized_tag
                            .as_ref()
                            .is_none_or(|tag| session.tags.contains(tag))
                })
                .collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&shown)?);
                return Ok(());
            }
            if shown.is_empty() {
                println!(
                    "{}",
                    cli_session_report::selection_empty(&list, archived, pinned, tag.as_deref())
                );
            } else {
                cli_session_report::list_table(&shown).print();
            }
        }
        SessionsAction::Show { id, last, json, .. } => {
            let s = find_session(&paths, &id)?;
            let mut r = daemon(Request::GetSession { id: s.id.clone() })?;
            let truncated = last.and_then(|last| {
                let messages = r.get_mut("messages")?.as_array_mut()?;
                let total = messages.len();
                let count = last.get();
                if total <= count {
                    return None;
                }
                drop(messages.drain(..total - count));
                Some((count, total))
            });
            if json {
                if let Some((_, total)) = truncated {
                    r["total_messages"] = serde_json::json!(total);
                }
                println!("{}", serde_json::to_string_pretty(&r)?);
                return Ok(());
            }
            println!("{}  ({})", s.name, s.id);
            if let Some(notice) = formless_tui::plain::unfinished_run_text(&r["unfinished_run"]) {
                println!("{notice}");
            }
            println!(
                "{} messages · updated {}{}{}",
                s.message_count,
                local_timestamp(&s.updated_at),
                s.model
                    .as_deref()
                    .map(|m| format!(" · {m}"))
                    .unwrap_or_default(),
                s.cwd
                    .as_deref()
                    .map(|c| format!(" · in {c}"))
                    .unwrap_or_default()
            );
            if let Some((count, total)) = truncated {
                println!("Showing last {count} of {total} stored messages.");
            }
            print!("{}", cli_session_report::metadata(&s));
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
                    println!("{who}: {}", tool_result_preview(&m.content));
                } else if !m.content.trim().is_empty() {
                    println!("\n{who}:\n{}", m.content.trim_end());
                }
            }
        }
        SessionsAction::Files { id, run, json } => {
            let session = find_session(&paths, &id)?;
            let result = daemon(Request::RunFiles {
                session_id: session.id,
                run_id: run,
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{}", formless_tui::plain::files_result_text(&result)?);
            }
        }
        SessionsAction::Rewind {
            id,
            path,
            run,
            json,
        } => {
            let session = find_session(&paths, &id)?;
            let result = daemon(Request::RewindRun {
                session_id: session.id,
                run_id: run,
                path,
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("{}", formless_tui::plain::rewind_result_text(&result));
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
            let bundle = formless_core::bundle_json::read(&file)?;
            let s: SessionInfo =
                serde_json::from_value(daemon(Request::ImportSession { bundle })?)?;
            println!("imported {}  ({})", s.name, s.id);
        }
        SessionsAction::Subagents { id, json } => {
            let session = find_session(&paths, &id)?;
            cli_subagents::list(session.id, json, daemon, &mut std::io::stdout().lock())?;
        }
        SessionsAction::Subagent { id, json } => {
            cli_subagents::detail(id, json, daemon, &mut std::io::stdout().lock())?;
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
            print!("{}", session_search_text(&query, limit, &hits));
        }
        SessionsAction::Templates { action } => cli_session_templates::run(&paths, action)?,
        SessionsAction::Archive { id } => set_archived(&paths, &id, true)?,
        SessionsAction::Unarchive { id } => set_archived(&paths, &id, false)?,
        SessionsAction::Export { id, format, output } => {
            let format = session_export_format(format, output.as_deref())?;
            let s = find_session(&paths, &id)?;
            let r = daemon(Request::ExportSession {
                id: s.id,
                format: Some(format.wire_name().to_string()),
            })?;
            let text = match r["content"].as_str() {
                Some(c) => c.to_string(),
                None => serde_json::to_string_pretty(&r)?,
            };
            match output {
                Some(path) => {
                    write_session_export(&path, text.as_bytes())?;
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
    /// List documented settings as loaded by this CLI, including defaults
    List {
        /// Match part of a dotted setting name, for example limits
        #[arg(long, value_name = "TEXT")]
        filter: Option<String>,
        /// Print the local settings catalog as one JSON object
        #[arg(long)]
        json: bool,
    },
    /// Print a locally loaded setting (with FORMLESS_* overrides applied)
    Get {
        /// Dotted setting name, for example provider.model
        key: String,
        /// Encode the value as compact JSON, preserving its type
        #[arg(long)]
        json: bool,
    },
    /// Set a setting through the daemon: validated, audited and applied at
    /// once. VALUE is JSON when it parses as JSON (5, true, ["a"]), else text
    Set {
        /// Dotted setting name, for example limits.run_steps
        key: String,
        /// New value as JSON when valid JSON, otherwise as literal text
        value: String,
        /// Replace the complete map instead of merging entries; use '{}' to clear
        #[arg(long)]
        replace: bool,
    },
    /// Edit config.toml in $EDITOR; it is replaced only if it still parses
    Edit,
}

/// Single-value encoding only; callers retain key lookup and error semantics.
fn config_get_output(value: &serde_json::Value, json: bool) -> serde_json::Result<String> {
    if json {
        serde_json::to_string(value)
    } else if let serde_json::Value::String(text) = value {
        Ok(text.clone())
    } else {
        serde_json::to_string_pretty(value)
    }
}

fn config_cmd(paths: Paths, action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Path => println!("{}", paths.config_file().display()),
        ConfigAction::List { filter, json } => {
            let config = Config::load(&paths)?;
            formless_core::secrets::set_patterns(&config.security.secret_patterns);
            let report = config.local_settings(filter.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
                return Ok(());
            }
            println!("Local configuration (file and environment overrides)");
            let mut table = Table::new(&["KEY", "EFFECTIVE", "DEFAULT", "SETTABLE", "DESCRIPTION"]);
            for (key, row) in report["settings"].as_object().into_iter().flatten() {
                let effective = match row["effective_kind"].as_str() {
                    Some("redacted") => "[redacted]".to_owned(),
                    Some("missing") => "(missing)".to_owned(),
                    _ => serde_json::to_string(&row["effective"])?,
                };
                let default = if row["default_kind"] == "documentation" {
                    format!(
                        "{} (documented)",
                        row["default_documentation"].as_str().unwrap_or_default()
                    )
                } else {
                    serde_json::to_string(&row["default"])?
                };
                let effective = if row["changed"] == true {
                    format!("{effective} (changed)")
                } else {
                    effective
                };
                table.row(vec![
                    t(key, Tone::Strong),
                    c(comparison_terminal_text(&effective)),
                    c(comparison_terminal_text(&default)),
                    c(if row["settable"] == true { "yes" } else { "no" }),
                    c(row["description"].as_str().unwrap_or_default()),
                ]);
            }
            table.print();
        }
        ConfigAction::Get { key, json } => match Config::load(&paths)?.setting_value(&key)? {
            Some(value) => println!("{}", config_get_output(&value, json)?),
            None => {
                eprintln!("no setting '{key}' (see `formless config list` or SPEC §13)");
                exit_cli(2);
            }
        },
        ConfigAction::Set {
            key,
            value,
            replace,
        } => {
            let value = serde_json::from_str(&value).unwrap_or(serde_json::Value::String(value));
            let request = if !replace && formless_core::config::SETTING_MAPS.contains(&key.as_str())
            {
                Request::MergeSetting {
                    key: key.clone(),
                    value: value.clone(),
                }
            } else {
                Request::SetSetting {
                    key: key.clone(),
                    value: value.clone(),
                }
            };
            formless_core::daemon::ensure_daemon(&paths)?;
            let r = formless_tui::client::request_once(&paths, request)?;
            if let Some(warnings) = r["routing_warnings"].as_array() {
                for warning in warnings.iter().filter_map(serde_json::Value::as_str) {
                    eprintln!("warning: {warning}");
                }
            }
            if r["applies"] == "env_override" {
                println!(
                    "saved {key} = {value}; daemon uses {} from {} (environment override)",
                    r["effective_value"],
                    r["override_source"].as_str().unwrap_or("its environment")
                );
                return Ok(());
            }
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
    } else if let Err(error) = Config::load_file_only(paths) {
        eprintln!(
            "warning: {} could not be loaded before editing: {error:#}",
            file.display()
        );
    }
    let draft = file.with_extension("toml.edit");
    #[cfg(unix)]
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    #[cfg(not(unix))]
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "notepad".into());
    #[cfg(not(unix))]
    let mut editor_command = windows_config_editor(&editor, &draft)?;
    std::fs::copy(&file, &draft)?;
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
        let status = editor_command.status();
        let status = status.with_context(|| {
            if cfg!(unix) {
                format!("start editor '{editor}'")
            } else {
                format!("start editor '{editor}'. Put double quotes around an executable path containing spaces.")
            }
        })?;
        if !status.success() {
            let _ = std::fs::remove_file(&draft);
            anyhow::bail!("the editor exited with {status}; config.toml unchanged");
        }
        let edited =
            std::fs::read_to_string(&draft).with_context(|| format!("read {}", draft.display()));
        match edited.and_then(|text| {
            Config::load_from_text_with_unmarked_download(&text, &draft)
                .map(|(_, unmarked_auto_download)| (text, unmarked_auto_download))
        }) {
            Ok((text, unmarked_auto_download)) => {
                formless_core::config::write_atomic(&file, text.as_bytes())?;
                if unmarked_auto_download {
                    eprintln!("warning: unmarked naming.auto_download = true is treated as false; use `formless config set naming.auto_download true` to opt in");
                }
                if let Err(error) = std::fs::remove_file(&draft) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        eprintln!(
                            "could not remove the saved draft {}: {error}",
                            draft.display()
                        );
                    }
                }
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

#[cfg(any(not(unix), test))]
fn windows_config_editor(editor: &str, draft: &std::path::Path) -> Result<std::process::Command> {
    let mut command = formless_tui::windows_editor_command(editor)?;
    command.arg(draft);
    Ok(command)
}

#[cfg(test)]
#[test]
fn config_edit_windows_argument_parsing_preserves_literal_paths() {
    use std::path::Path;

    let draft = Path::new(r"C:\Profile Folder\config.toml.edit");
    let quoted = windows_config_editor(
        r#""C:\Program Files\Editor\edit.exe" --wait "--name=two words""#,
        draft,
    )
    .unwrap();
    assert_eq!(quoted.get_program(), r"C:\Program Files\Editor\edit.exe");
    assert_eq!(
        quoted.get_args().collect::<Vec<_>>(),
        [
            std::ffi::OsStr::new("--wait"),
            std::ffi::OsStr::new("--name=two words"),
            draft.as_os_str(),
        ]
    );

    let unquoted = r"C:\Program Files\Editor\edit.exe --wait";
    let shared = formless_tui::windows_editor_command(unquoted).unwrap();
    let config = windows_config_editor(unquoted, draft).unwrap();
    assert_eq!(config.get_program(), shared.get_program());
    assert!(config
        .get_args()
        .eq(shared.get_args().chain([draft.as_os_str()])));
    assert!(windows_config_editor("\"unclosed", draft).is_err());
}

/// First-line tool output for the static human Session transcript.
fn tool_result_preview(content: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    use unicode_segmentation::UnicodeSegmentation;

    // An initial LF or CRLF ends the displayed line. Anything after that
    // terminator, including additional blank lines, is omitted content.
    let (first, omitted_lines) = match content.split_once('\n') {
        Some((line, rest)) => (line.strip_suffix('\r').unwrap_or(line), !rest.is_empty()),
        None => (content, false),
    };
    // The visible marker is outside the 120-scalar content budget.
    let mut remaining = 120;
    let mut end = 0;
    for (offset, grapheme) in first.grapheme_indices(true) {
        let scalars = grapheme.chars().take(remaining + 1).count();
        if scalars > remaining {
            break;
        }
        remaining -= scalars;
        end = offset + grapheme.len();
    }
    if omitted_lines || end < first.len() {
        Cow::Owned(format!("{}…", &first[..end]))
    } else {
        Cow::Borrowed(first)
    }
}

fn parse_session_search_limit(value: &str) -> std::result::Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|limit| (1..=200).contains(limit))
        .ok_or_else(|| "search limit must be an integer from 1 to 200".into())
}

fn session_search_text(query: &str, limit: usize, hits: &[formless_protocol::SearchHit]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if hits.is_empty() {
        if query.trim().is_empty() {
            out.push_str("empty search query\n");
        } else {
            let _ = writeln!(
                out,
                "no messages match '{}'",
                comparison_terminal_text(query)
            );
        }
        return out;
    }
    for hit in hits {
        let time = local_timestamp(&hit.created_at).replace('T', " ");
        let short_id: String = hit.session_id.chars().take(8).collect();
        let _ = writeln!(
            out,
            "{}  {}  #{} {} [{}] {}",
            comparison_terminal_text(&short_id),
            comparison_terminal_text(&hit.session_name),
            hit.seq,
            comparison_terminal_text(&time),
            comparison_terminal_text(&hit.role),
            comparison_terminal_text(&hit.snippet.replace('\n', " "))
        );
    }
    if hits.len() >= limit {
        let _ = writeln!(out, "Result limit {limit} reached; more matches may exist.");
        if limit < 200 {
            out.push_str("Increase --limit up to 200, or narrow the query.\n");
        } else {
            out.push_str("Narrow the query to find other matches.\n");
        }
    }
    out
}

fn validate_session_selector(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(usage("Session selector must not be empty"));
    }
    Ok(())
}

fn find_session(paths: &Paths, id: &str) -> Result<SessionInfo> {
    validate_session_selector(id)?;
    let list: Vec<SessionInfo> = serde_json::from_value(formless_tui::client::request_once(
        paths,
        Request::ListSessions,
    )?)?;
    find_session_in(&list, id)
}

fn find_session_in(list: &[SessionInfo], id: &str) -> Result<SessionInfo> {
    validate_session_selector(id)?;
    // An exact id, else an id prefix, else a name (SPEC §12, Session selectors).
    if let Some(s) = list.iter().find(|s| s.id == id) {
        return Ok(s.clone());
    }
    let mut matches: Vec<SessionInfo> = list
        .iter()
        .filter(|s| s.id.starts_with(id))
        .cloned()
        .collect();
    let by_name = matches.is_empty();
    if by_name {
        matches = list
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(id.trim()))
            .cloned()
            .collect();
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(usage(format!("no Session '{id}' (formless sessions list)"))),
        _ => Err(usage(cli_session_lookup::ambiguous(id, &matches, by_name))),
    }
}

fn comparison_terminal_text(text: &str) -> String {
    let mut visible = String::with_capacity(text.len());
    for c in text.chars() {
        if table::terminal_control(c) {
            visible.extend(c.escape_unicode());
        } else {
            visible.push(c);
        }
    }
    visible
}

fn comparison_quoted_metadata(text: &str) -> Result<String> {
    Ok(escape_json_terminal_controls(&serde_json::to_string(text)?))
}

/// Keep JSON text parseable while escaping terminal controls left by Serde.
fn escape_json_terminal_controls(json: &str) -> String {
    use std::fmt::Write as _;
    let mut visible = String::with_capacity(json.len());
    for c in json.chars() {
        if c != '\n' && table::terminal_control(c) {
            for unit in c.encode_utf16(&mut [0; 2]) {
                write!(&mut visible, "\\u{unit:04x}").expect("writing to String");
            }
        } else {
            visible.push(c);
        }
    }
    visible
}

fn sessions_compare_cmd(paths: &Paths, left: &str, right: &str, json: bool) -> Result<()> {
    let comparison: formless_protocol::SessionComparison =
        if formless_core::daemon::daemon_reachable(paths) {
            let left = find_session(paths, left)?;
            let right = find_session(paths, right)?;
            serde_json::from_value(formless_tui::client::request_once(
                paths,
                Request::CompareSessions {
                    left: left.id,
                    right: right.id,
                },
            )?)?
        } else {
            let store = formless_core::Store::open_forecast_snapshot(paths)?
                .context("No existing profile database to compare")?;
            let list = store.list_sessions()?;
            let left = find_session_in(&list, left)?;
            let right = find_session_in(&list, right)?;
            store.compare_sessions(&left.id, &right.id, &Config::load_file_only(paths)?)?
        };
    if json {
        println!("{}", serde_json::to_string_pretty(&comparison)?);
        return Ok(());
    }
    println!(
        "{} ({}) ↔ {} ({})",
        comparison_terminal_text(&comparison.left.session.name),
        comparison_terminal_text(&comparison.left.session.id),
        comparison_terminal_text(&comparison.right.session.name),
        comparison_terminal_text(&comparison.right.session.id)
    );
    match (&comparison.common_ancestor, comparison.fork_point_seq) {
        (Some(id), Some(seq)) => println!(
            "Common parent {}; fork point seq {seq}; diff after seq {}",
            comparison_terminal_text(id),
            comparison.diff_from_seq
        ),
        _ => println!("Full stored-history comparison"),
    }
    for warning in &comparison.warnings {
        println!("note: {}", comparison_terminal_text(warning));
    }
    for (label, side) in [("left", &comparison.left), ("right", &comparison.right)] {
        let usage = &side.usage;
        println!(
            "{label} usage scope: {}",
            comparison_terminal_text(&usage.scope)
        );
        let cost = usage
            .cost
            .map(|cost| format!("${cost:.6}"))
            .unwrap_or_else(|| "unknown (partial or unpriced)".into());
        println!("{label}: recorded {} prompt / {} completion tokens; {} requests; cost {cost}; {} unallocated requests / {} unknown Runs",usage.recorded_prompt_tokens,usage.recorded_completion_tokens,usage.requests,usage.unallocated_requests,usage.unknown_runs);
        if usage.unallocated_requests > 0 {
            println!("  unallocated observed work: {} prompt / {} completion tokens; priced subtotal ${:.6}",usage.unallocated_prompt_tokens,usage.unallocated_completion_tokens,usage.unallocated_priced_cost_subtotal);
        }
        if let Some([low, high]) = usage.known_records_cost_range {
            println!("  allocation range over observed priced records only: ${low:.6}–${high:.6} (excludes missing provider usage)");
        }
        for warning in &usage.warnings {
            println!("  note: {}", comparison_terminal_text(warning));
        }
    }
    for row in comparison.messages {
        let sign = match row.kind.as_str() {
            "left_only" => "-",
            "right_only" => "+",
            _ => " ",
        };
        if let Some(message) = row.left.or(row.right) {
            println!(
                "{sign} {} [seq {}]:",
                comparison_terminal_text(&message.role),
                message.seq
            );
            for line in message.content.split('\n') {
                println!("{sign} {}", comparison_terminal_text(line));
            }
            if let Some(meta) = message.meta {
                println!(
                    "{sign} metadata (quoted data): {}",
                    comparison_quoted_metadata(&meta)?
                );
            }
        }
    }
    Ok(())
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

/// `formless prices import` (#809): the endpoint's own prices, dated, so
/// dollar limits can count the models in use.
fn prices_import_cmd(
    paths: Paths,
    provider: Option<String>,
    all: bool,
    dry_run: bool,
) -> Result<()> {
    let live = Config::load(&paths)?;
    let pcfg = live.provider_cfg(provider.as_deref())?;
    let name = provider.clone().unwrap_or_else(|| "default".into());
    let rt = tokio::runtime::Runtime::new()?;
    let listed = rt.block_on(async {
        let p = formless_core::provider::Provider::for_named_profile_with(
            pcfg.clone(),
            paths.clone(),
            formless_core::provider::http_client(&pcfg.proxy)?,
            &name,
        )
        .await?;
        p.list_model_prices().await
    })?;
    if listed.is_empty() {
        return Err(anyhow::anyhow!(
            "{name}'s /models listing has no prices; add them to provider.prices by hand"
        ));
    }
    // The models in use: the provider's model and fallbacks, Roles on it,
    // and any it has served (from usage).
    let mut wanted: std::collections::BTreeSet<String> = Default::default();
    wanted.insert(pcfg.model.clone());
    wanted.extend(pcfg.fallback_models.iter().cloned());
    for r in live.roles.values() {
        if r.provider.as_deref().unwrap_or("default") == name {
            if !r.model.is_empty() {
                wanted.insert(r.model.clone());
            }
        }
    }
    if let Ok(store) = formless_core::Store::open(&paths.db_file()) {
        if let Ok(used) = store.usage_by_model(None) {
            wanted.extend(used.into_keys());
        }
    }
    let mut cfg = Config::load_file_only(&paths)?;
    let target = match provider.as_deref() {
        None => &mut cfg.provider,
        Some(n) => cfg
            .providers
            .get_mut(n)
            .with_context(|| format!("unknown provider '{n}'"))?,
    };
    let mut changed = Vec::new();
    for (model, price) in listed {
        let used = wanted.contains(&model)
            || wanted
                .iter()
                .any(|w| w.rsplit('/').next() == model.rsplit('/').next());
        if !all && !used {
            continue;
        }
        let old = target.prices.get(&model);
        if let Some(change) = cli_price_import::change(&model, old, &price) {
            changed.push(change);
            if !dry_run {
                target.prices.insert(model, price);
            }
        }
    }
    if changed.is_empty() {
        println!("prices for {name} are up to date");
        return Ok(());
    }
    for c in &changed {
        println!("{c}");
    }
    if dry_run {
        println!("(dry run: nothing written)");
    } else {
        cfg.save(&paths)?;
        println!("saved {} price(s) to provider.prices", changed.len());
    }
    Ok(())
}

fn providers_cmd(paths: Paths, action: ProvidersAction) -> Result<()> {
    let daemon = |req: Request| {
        formless_core::daemon::ensure_daemon(&paths)?;
        formless_tui::client::request_once(&paths, req)
    };
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
            azure_resource,
            azure_deployment,
            api_version,
            headers,
            query,
            key_header,
            preset,
            model,
            no_key,
        } => {
            let pairs = |items: &[String],
                         what: &str|
             -> Result<std::collections::BTreeMap<String, String>> {
                items
                    .iter()
                    .map(|h| {
                        let (k, v) = h
                            .split_once('=')
                            .with_context(|| format!("{what} takes key=value"))?;
                        Ok((k.trim().to_string(), v.trim().to_string()))
                    })
                    .collect()
            };
            let mut header_map = pairs(&headers, "--header")?;
            let mut query_map = pairs(&query, "--query")?;
            let mut key_header = key_header;
            let mut model = model;
            let azure_base = match (&azure_resource, &azure_deployment) {
                (Some(resource), Some(deployment)) => {
                    let (base, query, header) =
                        formless_core::presets::azure(resource, deployment, api_version.as_deref())
                            .map_err(|err| usage(err.to_string()))?;
                    if api_version.is_none() {
                        if model
                            .as_deref()
                            .is_some_and(|model| model != deployment.trim())
                        {
                            return Err(usage(
                                "--model must match --azure-deployment with Azure v1",
                            ));
                        }
                        if query_map.contains_key("api-version") {
                            return Err(usage("use --api-version to select the dated Azure API"));
                        }
                    }
                    query_map.extend(query);
                    key_header.get_or_insert_with(|| header.to_owned());
                    model.get_or_insert_with(|| deployment.trim().to_owned());
                    Some(base)
                }
                _ => None,
            };
            let use_azure = azure_base.is_some();
            header_map.retain(|k, _| !k.is_empty());
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
            let base_url = match (azure_base, base_url, preset) {
                (Some(url), _, _) | (None, Some(url), _) => url,
                (None, None, Some(p)) => p.base_url.to_string(),
                (None, None, None) => unreachable!("clap requires one"),
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
                headers: (!header_map.is_empty()).then_some(header_map),
                query_params: (use_azure || !query_map.is_empty()).then_some(query_map),
                api_key_header: key_header,
            })?;
            println!("provider {name} saved; check it with `formless providers test {name}`");
        }
        ProvidersAction::Remove { name } => {
            daemon(Request::RemoveProvider { name: name.clone() })?;
            println!("removed {name}");
        }
        ProvidersAction::DismissWarning { provider, model } => {
            let r = daemon(Request::DismissMissingModel { provider, model })?;
            if r["dismissed"] == true {
                println!("Model warning dismissed.");
            } else {
                println!("No active warning matched that provider and model.");
            }
        }
        ProvidersAction::Test {
            name,
            model: Some(model),
            json,
        } => {
            let provider = (name != formless_core::config::DEFAULT_PROVIDER).then_some(name);
            let t = daemon(Request::TestModel { model, provider })?;
            let ok = t["ok"] == true;
            if json {
                println!("{}", cli_provider_test::model_json(t)?);
            } else {
                let ms = |k: &str| t[k].as_u64().map_or("-".into(), |v| format!("{v} ms"));
                let yes = |k: &str| if t[k] == true { "yes" } else { "no" };
                println!("model:       {}", t["model"].as_str().unwrap_or_default());
                println!("result:      {}", if ok { "ok" } else { "problems" });
                println!(
                    "first token: {}  ·  total {}",
                    ms("first_token_ms"),
                    ms("total_ms")
                );
                println!("streaming:   {}", yes("streaming"));
                println!("reasoning:   {}", yes("reasoning"));
                println!(
                    "tool calls:  {}{}",
                    yes("tool_calling"),
                    if t["prompt_fallback"] == true {
                        " (prompt-based fallback only)"
                    } else {
                        ""
                    }
                );
                if let Some(e) = t["error"].as_str() {
                    println!("error:       {e}");
                }
            }
            if !ok {
                std::io::stdout().flush()?;
                exit_cli(1);
            }
        }
        ProvidersAction::Test {
            name,
            model: None,
            json,
        } => {
            let r = daemon(Request::TestProvider { name })?;
            if json {
                println!("{}", cli_provider_test::models_json(r)?);
                return Ok(());
            }
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

fn format_job_history_duration(started_at: &str, finished_at: Option<&str>) -> String {
    let duration = finished_at.and_then(|finished_at| {
        let started = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
        let finished = chrono::DateTime::parse_from_rfc3339(finished_at).ok()?;
        (finished - started).to_std().ok()
    });
    match duration {
        Some(duration) if duration.as_secs() == 0 => format!("{}ms", duration.as_millis()),
        Some(duration) => format_duration(duration),
        None => "-".into(),
    }
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
                        .map(local_timestamp)
                        .unwrap_or_else(|| "-".into())),
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
        JobsAction::Preview { schedule, json } => {
            let preview = daemon(Request::PreviewSchedule { schedule })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&preview)?);
                return Ok(());
            }
            let cron = preview["cron"]
                .as_str()
                .context("schedule preview did not return a cron expression")?;
            let next = preview["next"]
                .as_array()
                .context("schedule preview did not return a list of times")?
                .iter()
                .map(|time| time.as_str().context("invalid time in schedule preview"))
                .collect::<Result<Vec<_>>>()?;
            println!("Cron: {cron}");
            if next.is_empty() {
                println!("No upcoming times were returned.");
            } else {
                println!("Projected from the daemon's current local time:");
                for time in next {
                    println!("  {time}");
                }
            }
        }
        JobsAction::Run { id } => {
            let r = daemon(Request::RunJobNow { id: id.clone() })?;
            println!("{id}: {}", r["status"].as_str().unwrap_or_default());
        }
        JobsAction::History { id, limit, json } => {
            let limit = usize::from(limit);
            let runs = daemon(Request::JobRuns {
                id: id.clone(),
                limit: Some(limit),
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&runs)?);
                return Ok(());
            }
            let runs: Vec<formless_protocol::JobRunInfo> = serde_json::from_value(runs)?;
            if runs.is_empty() {
                println!("{id} has not run yet");
            }
            let at_limit = runs.len() == limit;
            let mut tbl = Table::new(&[
                "STARTED", "TRIGGER", "OUTCOME", "TOOK", "COST", "SESSION", "DETAIL",
            ]);
            for r in runs {
                let took = format_job_history_duration(&r.started_at, r.finished_at.as_deref());
                tbl.row(vec![
                    c(local_timestamp(&r.started_at).replace('T', " ")),
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
            if at_limit {
                if limit == 500 {
                    println!("Showing the maximum of 500 firings. More may exist.");
                } else {
                    println!("Showing newest {limit} firings. More may exist. Use --limit to request up to 500.");
                }
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

fn validate_runs_filters(cli: &Cli) -> Result<()> {
    if matches!(&cli.cmd,Some(Cmd::Runs{action:Some(_),session,since,..}) if session.is_some() || since.is_some())
    {
        return Err(usage(
            "--session and --since apply only to the Runs list, not its subcommands",
        ));
    }
    Ok(())
}

fn runs_cmd(
    paths: Paths,
    limit: usize,
    json: bool,
    session: Option<String>,
    since: Option<String>,
) -> Result<()> {
    formless_core::daemon::ensure_daemon(&paths)?;
    let session_id = session
        .as_deref()
        .map(|selector| find_session(&paths, selector).map(|session| session.id))
        .transpose()?;
    let v = formless_tui::client::request_once(
        &paths,
        Request::RunMetrics {
            limit: Some(limit),
            session_id,
            since,
        },
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    use std::io::IsTerminal;
    let width = std::io::stdout()
        .is_terminal()
        .then(|| {
            crossterm::terminal::size()
                .ok()
                .map(|(width, _)| width as usize)
        })
        .flatten();
    print!("{}", cli_runs_list::render(&v, width)?);
    Ok(())
}

fn memory_cmd(paths: Paths, action: MemoryAction) -> Result<()> {
    let daemon = |req: Request| -> Result<serde_json::Value> {
        formless_core::daemon::ensure_daemon(&paths)?;
        formless_tui::client::request_once(&paths, req)
    };
    let show = |m: &serde_json::Value| {
        let field = |key: &str| comparison_terminal_text(m[key].as_str().unwrap_or_default());
        println!(
            "{:<10} {:<8} {:<20} {}{}",
            field("id"),
            if m["enabled"] == true { "on" } else { "off" },
            field("scope"),
            field("text"),
            match m["session_id"].as_str() {
                Some(s) => format!(
                    "  (from session {})",
                    comparison_terminal_text(&s.chars().take(8).collect::<String>())
                ),
                None => format!("  ({})", field("author")),
            }
        )
    };
    match action {
        MemoryAction::Used { session, run, json } => {
            let selector = match (session, run) {
                (Some(session), None) => {
                    formless_core::daemon::ensure_daemon(&paths)?;
                    let resolved = find_session(&paths, &session)?;
                    cli_run_memories::Selector::Session {
                        id: resolved.id,
                        name: resolved.name,
                    }
                }
                (None, Some(run)) => cli_run_memories::Selector::Run(run),
                _ => {
                    return Err(usage(
                        "memory used requires exactly one of --session or --run",
                    ))
                }
            };
            cli_run_memories::used(selector, json, daemon, &mut std::io::stdout().lock())?;
        }
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
    if let McpAction::Prompt { args, .. } = &action {
        cli_mcp_prompts::arguments(args)?;
    }
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
            param_env,
            yes,
            confirm_exposure,
            url,
            headers,
            env_keys,
            command,
        } => {
            if url.is_none() && command.is_empty() {
                // A catalog server by id.
                let mut params: serde_json::Map<String, serde_json::Value> = params
                    .iter()
                    .map(|p| {
                        let (k, v) = p.split_once('=').context("--param takes key=value")?;
                        Ok((k.trim().to_string(), serde_json::json!(v.trim())))
                    })
                    .collect::<Result<_>>()?;
                let catalog = daemon(Request::McpCatalog)?;
                let entry = catalog
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|e| {
                        e["id"]
                            .as_str()
                            .is_some_and(|id| id.eq_ignore_ascii_case(&name))
                    })
                    .context("unknown catalog entry")?;
                for param in entry["params"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| p["kind"] == "secret")
                {
                    let key = param["name"].as_str().unwrap_or_default();
                    if params.contains_key(key) {
                        bail!("secret parameters must use a masked prompt or --param-env {key}=ENV, not --param");
                    }
                    let value = if let Some(source) = param_env
                        .iter()
                        .find_map(|p| p.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v))
                    {
                        std::env::var(source)
                            .context("parameter environment variable is missing")?
                    } else {
                        read_masked_mcp_parameter(key)?
                    };
                    params.insert(key.to_string(), serde_json::json!(value));
                }
                let preview = daemon(Request::PreviewMcpCatalog {
                    id: name.clone(),
                    params: params.clone(),
                })?;
                println!(
                    "command: {}",
                    preview["target"].as_str().unwrap_or_default()
                );
                if let Some(warning) = preview["execution_warning"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                {
                    println!("{warning}");
                }
                if !yes && !confirm("Add this reviewed server and run its command?")? {
                    return Ok(());
                }
                params.insert("confirm_run".into(), serde_json::json!(true));
                for warning in preview["warnings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                {
                    println!("exposure: {warning}");
                    if !confirm_exposure && !confirm(warning)? {
                        return Ok(());
                    }
                }
                params.insert("confirm_exposure".into(), serde_json::json!(true));
                let r = daemon(Request::AddMcpFromCatalog {
                    id: name.clone(),
                    name: None,
                    params,
                })?;
                println!("added:   {}", r["added"].as_str().unwrap_or(&name));
                report(&r);
                for k in r["env_keys"].as_array().into_iter().flatten() {
                    println!(
                        "needs:   {} in the daemon's environment",
                        k.as_str().unwrap_or_default()
                    );
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
        McpAction::Prompts { json } => {
            cli_mcp_prompts::list(json, daemon, &mut std::io::stdout().lock())?;
        }
        McpAction::Prompt {
            server,
            name,
            args,
            json,
        } => {
            cli_mcp_prompts::prompt(
                server,
                name,
                cli_mcp_prompts::arguments(&args)?,
                json,
                daemon,
                &mut std::io::stdout().lock(),
            )?;
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
            let mut tbl = Table::new(&["ID", "SERVER", "WHAT IT DOES", "COMMAND / URL", "NEEDS"]);
            for e in list.as_array().into_iter().flatten() {
                let mut needs: Vec<String> = e["params"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| {
                        p["name"].as_str().map(|n| {
                            if p["kind"] == "secret" {
                                format!("--param-env {n}=ENV (or masked prompt)")
                            } else {
                                format!("--param {n}=…")
                            }
                        })
                    })
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
                    t(
                        id,
                        if e["added"] == true {
                            Tone::Dim
                        } else {
                            Tone::Strong
                        },
                    ),
                    c(e["title"].as_str().unwrap_or_default()),
                    t(e["description"].as_str().unwrap_or_default(), Tone::Dim),
                    c(e["target"].as_str().unwrap_or_default()),
                    c(needs.join(" ")),
                ]);
            }
            tbl.print();
            println!("{}", formless_core::mcp_catalog::CODE_WARNING);
            // The summary table fits the terminal; review commands must remain
            // complete even when its command column was shortened.
            for entry in list.as_array().into_iter().flatten() {
                println!(
                    "{}: {}",
                    entry["id"].as_str().unwrap_or_default(),
                    entry["target"].as_str().unwrap_or_default()
                );
            }
            println!("add one with: formless mcp add ID [--param key=value]");
        }
        McpAction::Import { yes, copy_secrets } => {
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
                println!(
                    "  env/header keys with values: {} / {}",
                    cand["env"], cand["headers"]
                );
                println!(
                    "  {}",
                    if copy_secrets {
                        "Values will be copied into plaintext config.toml (--copy-secrets)."
                    } else {
                        "Skipping secret values; set env/header values yourself. Use --copy-secrets only to accept plaintext config.toml storage."
                    }
                );
                for m in cand["missing_env"].as_array().into_iter().flatten() {
                    println!(
                        "  not imported: {} (a placeholder; set it yourself)",
                        m.as_str().unwrap_or_default()
                    );
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
                    let r = daemon(Request::ImportMcpCandidate {
                        candidate_id: cand["candidate_id"]
                            .as_str()
                            .context("missing candidate reference")?
                            .into(),
                        skip_secrets: !copy_secrets,
                    })?;
                    println!("  added:   {name}");
                    println!(
                        "  disabled and untrusted; review command before enabling in config.toml"
                    );
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
                    t(
                        tool["name"].as_str().unwrap_or_default(),
                        if on { Tone::Strong } else { Tone::Dim },
                    ),
                    if on {
                        t("on", Tone::Good)
                    } else {
                        t("off", Tone::Dim)
                    },
                    c(tool["tokens"].to_string()),
                    t(
                        tool["description"]
                            .as_str()
                            .unwrap_or_default()
                            .lines()
                            .next()
                            .unwrap_or_default(),
                        Tone::Dim,
                    ),
                ]);
            }
            tbl.print();
            for tool in r["tools"].as_array().into_iter().flatten() {
                print_description_evidence(tool);
            }
            println!(
                "tool list cost: ~{} tokens per request (enabled tools)",
                r["enabled_tokens"]
            );
        }
        McpAction::Tool {
            name,
            tool,
            on,
            off,
        } => {
            if on == off {
                return Err(usage("pass --on or --off"));
            }
            daemon(Request::SetMcpToolEnabled {
                name: name.clone(),
                tool: tool.clone(),
                enabled: on,
            })?;
            println!("{name}: {tool} is {}", if on { "on" } else { "off" });
        }
        McpAction::Test { name, json } => {
            if !cli_mcp_test::run(&name, json, daemon, &mut std::io::stdout().lock())? {
                exit_cli(1);
            }
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
            for line in formless_core::mcp_auth::logout(&paths, &server) {
                println!("{line}");
            }
            println!("Deleted stored OAuth tokens for '{server}'.");
        }
    }
    Ok(())
}

fn naming_cmd(paths: Paths, action: NamingAction) -> Result<()> {
    use formless_core::namer;
    let cfg = Config::load(&paths)?;
    match action {
        NamingAction::Status { json: true } => {
            let spec = cfg.naming.spec();
            let cached = namer::cached_models(&paths)?
                .into_iter()
                .map(|model| {
                    serde_json::json!({
                        "id": model.id,
                        "bytes": model.bytes,
                        "installed": model.installed,
                        "configured": spec.is_some_and(|spec| spec.id == model.id),
                    })
                })
                .collect::<Vec<_>>();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "enabled": cfg.naming.enabled,
                    "model": cfg.naming.model,
                    "installed": spec.is_some_and(|s| s.installed(&paths)),
                    "status": naming_model_status(&paths, &cfg),
                    "models_dir": namer::models_dir(&paths).display().to_string(),
                    "cached": cached,
                    "available": namer::MODELS
                        .iter()
                        .map(|m| serde_json::json!({"id": m.id, "bytes": m.gguf_bytes}))
                        .collect::<Vec<_>>(),
                }))?
            );
        }
        NamingAction::Status { json: false } => {
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
            let cached = namer::cached_models(&paths)?;
            if cached.is_empty() {
                println!("cache:      empty");
            }
            let selected = cfg.naming.spec();
            for model in cached {
                let selection = if selected.is_some_and(|spec| spec.id == model.id) {
                    "configured"
                } else {
                    "unselected"
                };
                let state = if model.installed {
                    "installed"
                } else {
                    "partial"
                };
                println!(
                    "cached:     {} ({} bytes, {selection}, {state})",
                    model.id, model.bytes
                );
            }
        }
        NamingAction::Pull { model } => {
            let id = model.unwrap_or_else(|| cfg.naming.model.clone());
            let spec = namer::spec(&id).with_context(|| format!("unknown naming model '{id}'"))?;
            if spec.installed(&paths) {
                println!("{} already installed", spec.id);
            } else {
                println!(
                    "downloading {} ({} MB)…",
                    spec.id,
                    spec.gguf_bytes / 1_000_000
                );
                tokio::runtime::Runtime::new()?.block_on(namer::download(&paths, &spec))?;
                println!("installed {}", spec.gguf_path(&paths).display());
            }
            if !id.eq_ignore_ascii_case(&cfg.naming.model) {
                println!(
                    "set `[naming] model = \"{}\"` in config.toml to use it",
                    spec.id
                );
                if let Some(previous) = cfg
                    .naming
                    .spec()
                    .filter(|previous| previous.installed(&paths))
                {
                    println!("{} remains cached. Reclaim its disk space with `formless naming remove {}`.", previous.id, previous.id);
                }
            }
        }
        NamingAction::Remove { model, all } => {
            let selected = if all {
                None
            } else {
                let id = model.unwrap_or_else(|| cfg.naming.model.clone());
                Some(namer::spec(&id).with_context(|| format!("unknown naming model '{id}'"))?)
            };
            let models = selected
                .as_ref()
                .map_or(namer::MODELS, std::slice::from_ref);
            let removed = namer::remove_cached_models(&paths, models)?;
            println!(
                "Removed {} naming cache file(s) totaling {} bytes.",
                removed.files, removed.bytes
            );
            if removed.files > 0
                && cfg.naming.enabled
                && cfg.naming.auto_download
                && cfg
                    .naming
                    .spec()
                    .is_some_and(|configured| models.iter().any(|spec| spec.id == configured.id))
            {
                println!("The configured model can download again. Set `[naming] auto_download = false` to keep it removed.");
            }
        }
        NamingAction::Test { text } => {
            let spec = cfg.naming.spec();
            let started = std::time::Instant::now();
            let title = namer::title(&paths, spec.as_ref(), &text);
            println!(
                "{}  ({:.2?}, {}{})",
                title.as_ref().map_or("(none)", |(t, _)| t.as_str()),
                started.elapsed(),
                naming_model_status(&paths, &cfg),
                match title {
                    Some((_, namer::TitleSource::Heuristic)) => ", heuristic",
                    _ => "",
                }
            );
        }
    }
    Ok(())
}

fn naming_model_status(paths: &Paths, cfg: &Config) -> String {
    match cfg.naming.spec() {
        Some(spec) => {
            let installed = spec.installed(paths);
            let download = if !formless_core::namer::embedded_available() {
                "auto-download unavailable in this build"
            } else if installed {
                "auto-download not needed"
            } else if !cfg.naming.enabled {
                "auto-download will not happen (auto-naming disabled)"
            } else if cfg.naming.auto_download {
                "auto-download on first use"
            } else {
                "auto-download disabled"
            };
            format!(
                "{} ({}; {}; {}; install with `formless naming pull`)",
                spec.id,
                if installed {
                    "installed"
                } else {
                    "not installed"
                },
                download,
                formless_core::namer::display_size(spec.gguf_bytes)
            )
        }
        None => format!(
            "heuristic (unknown model '{}'; auto-download unavailable; size unknown)",
            cfg.naming.model
        ),
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

fn tools_cmd(paths: Paths, action: ToolsAction, prepared_input: Option<Value>) -> Result<()> {
    use formless_tui::client::request_once;
    formless_core::daemon::ensure_daemon(&paths)?;
    match action {
        ToolsAction::Diff {
            name,
            from,
            to,
            json,
        } => cli_tool_diff::run(&paths, &name, from, to, json),
        ToolsAction::Reviews { name, kind, json } => {
            description_reviews_cmd(&paths, kind, name, json)
        }
        ToolsAction::List {
            json,
            tag,
            filter,
            show_pinned,
        } => {
            let v = request_once(&paths, Request::ListTools)?;
            let mut tools: Vec<formless_protocol::ToolSummary> = serde_json::from_value(v)?;
            let original_count = tools.len();
            cli_tool_filters::select(&mut tools, filter, tag.as_deref());
            if json {
                println!("{}", serde_json::to_string_pretty(&tools)?);
                return Ok(());
            }
            if tools.is_empty() {
                println!(
                    "{}",
                    cli_tool_filters::empty_message(original_count, filter, tag.as_deref())
                );
                return Ok(());
            }
            let mut headers = vec![
                "TOOL",
                "TITLE",
                "TAGS",
                "VER",
                "CALLS",
                "SUCCESS",
                "STATUS",
                "CAPABILITIES",
            ];
            if show_pinned {
                headers.push("PINNED");
            }
            let mut tbl = Table::new(&headers);
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
                let (success_label, success_tone) = cli_tool_success::tool_success_cell(rate);
                let mut cells = vec![
                    t(tool.name, Tone::Strong),
                    c(tool.title),
                    t(tool.tags.join(" "), Tone::Dim),
                    c(format!("v{}", tool.current_version)),
                    c(tool.invocations.to_string()),
                    t(success_label, success_tone),
                    status,
                    t(tool.capabilities.join(", "), Tone::Dim),
                ];
                if show_pinned {
                    cells.push(c(if tool.pinned { "yes" } else { "no" }));
                }
                tbl.row(cells);
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
                let name = name.ok_or_else(|| usage("give a tool name/id or --all"))?;
                // Resolve name -> id via the list.
                let v = request_once(&paths, Request::ListTools)?;
                let tools: Vec<formless_protocol::ToolSummary> = serde_json::from_value(v)?;
                let tool = cli_tool_lookup::resolve(&tools, &name)?;
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
        ToolsAction::Show { name, json, code } => {
            let tool = find_tool(&paths, &name)?;
            let d: formless_protocol::ToolDetail =
                serde_json::from_value(request_once(&paths, Request::GetTool { id: tool.id })?)?;
            if code {
                std::io::stdout().write_all(cli_tool_detail::current_source(&d)?.as_bytes())?;
                return Ok(());
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
                return Ok(());
            }
            print_tool_detail(&d);
            Ok(())
        }
        ToolsAction::Run {
            name,
            yes,
            session,
            dry_run,
            json,
            ..
        } => {
            let input = prepared_input.context("Tool Run input was not prepared")?;
            let tool = find_tool(&paths, &name)?;
            if let Err(e) = formless_tui::client::require_secure_daemon(&paths) {
                eprintln!("error: {e:#}");
                exit_cli(2);
            }
            let code = tool_run(&paths, tool, input, yes, session, dry_run, json)?;
            exit_cli(code);
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
        ToolsAction::Tag { name, tags, clear } => {
            let tool = find_tool(&paths, &name)?;
            let tool: formless_protocol::ToolSummary = if clear || !tags.is_empty() {
                let updated: formless_protocol::ToolInfo = serde_json::from_value(request_once(
                    &paths,
                    Request::SetToolTags { id: tool.id, tags },
                )?)?;
                updated.into()
            } else {
                tool
            };
            let name = comparison_terminal_text(&tool.name);
            let tags = if tool.tags.is_empty() {
                "no tags".into()
            } else {
                comparison_terminal_text(&tool.tags.join(" "))
            };
            println!("{name}: {tags}");
            Ok(())
        }
        ToolsAction::Retire { name, force } => set_tool_retired(&paths, &name, true, force),
        ToolsAction::Restore { name } => set_tool_retired(&paths, &name, false, false),
        ToolsAction::Rollback { name, to } => {
            let tool = find_tool(&paths, &name)?;
            let version = to.unwrap_or(tool.current_version - 1);
            if version < 1 || version == tool.current_version {
                eprintln!(
                    "{} is at v{}; give --to with an earlier version",
                    tool.name, tool.current_version
                );
                exit_cli(2);
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
                    exit_cli(2);
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
                    exit_cli(2);
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
                    exit_cli(2);
                }
                fetched["bundle"].clone()
            } else {
                formless_core::bundle_json::read(&file)?
            };
            print_bundle_description_preview(&paths, &bundle)?;
            let v = request_once(&paths, Request::ImportTool { bundle })?;
            println!("imported: {v}");
            description_reviews_cmd(&paths, None, None, false)?;
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
            formless_core::descriptions::screen(t["description"].as_str().unwrap_or_default()).text
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
fn read_masked_mcp_parameter(name: &str) -> Result<String> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        bail!("{name} needs a masked terminal prompt or --param-env {name}=ENV");
    }
    print!("{name} (hidden): ");
    std::io::stdout().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
    let restore = Restore;
    let mut value = String::new();
    loop {
        if let crossterm::event::Event::Key(key) = crossterm::event::read()? {
            if key.kind == crossterm::event::KeyEventKind::Release {
                continue;
            }
            use crossterm::event::{KeyCode, KeyModifiers};
            match key.code {
                KeyCode::Enter => break,
                KeyCode::Esc => bail!("parameter entry cancelled"),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    bail!("parameter entry cancelled")
                }
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => value.push(c),
                _ => {}
            }
        }
    }
    drop(restore);
    println!();
    Ok(value)
}

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
fn set_tool_retired(paths: &Paths, name: &str, retire: bool, force: bool) -> Result<()> {
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
        if !force {
            if !std::io::stdin().is_terminal() {
                return Err(usage(
                    "not retired: pass --force to retire a Tool others use when not on a terminal",
                ));
            }
            if !confirm("Retire it anyway? [y/N] ")? {
                return Err(usage("not retired"));
            }
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

fn log_start(line: &str) -> Option<(chrono::DateTime<chrono::FixedOffset>, u8)> {
    let mut words = line.split_whitespace();
    let at = chrono::DateTime::parse_from_rfc3339(words.next()?).ok()?;
    Some((at, words.next().and_then(log_level).unwrap_or(2)))
}

#[path = "log_output.rs"]
mod log_output;

fn log_truncation_notice(summary: &log_output::OutputSummary) {
    if summary.read.truncated_lines > 0 || summary.truncated_entries > 0 {
        eprintln!(
            "warning: log output truncated {} long line(s) and {} oversized entry(s) at 64 KiB",
            summary.read.truncated_lines, summary.truncated_entries
        );
    }
}

/// Group log text into entries (small regression fixtures only).
#[cfg(test)]
fn parse_log(text: &str) -> Vec<LogEntry> {
    let mut out: Vec<LogEntry> = Vec::new();
    for line in text.lines() {
        match log_start(line) {
            Some((at, level)) => out.push(LogEntry {
                at: Some(at),
                level,
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
    jsonl: bool,
) -> Result<()> {
    let max_level = level.as_deref().and_then(log_level).unwrap_or(4);
    let cutoff = since.and_then(|d| {
        let delta = chrono::Duration::from_std(d).ok()?;
        chrono::Utc::now().checked_sub_signed(delta)
    });
    let keep = |e: &LogEntry| {
        e.level <= max_level
            && match (cutoff, e.at) {
                (Some(c), Some(at)) => at >= c,
                (Some(_), None) => false,
                _ => true,
            }
    };
    let live = paths.log_file();
    let format = if jsonl {
        log_output::Format::Jsonl
    } else {
        log_output::Format::Text
    };
    let summary = log_output::history(paths, lines, &keep, format, &mut std::io::stdout().lock())?;
    log_truncation_notice(&summary);
    if summary.entries == 0 && !follow {
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::pin!(ctrl_c);
        loop {
            tokio::select! {
                result = &mut ctrl_c => {
                    result?;
                    exit_cli(130);
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
            let len = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
            if len < pos {
                pos = 0;
            }
            if len > pos {
                let summary =
                    log_output::appended(&live, pos, len, &keep, &mut std::io::stdout().lock())?;
                pos = len;
                log_truncation_notice(&summary);
            }
        }
    })
}

/// The Tool with this name or id.
fn find_tool(paths: &Paths, name: &str) -> Result<formless_protocol::ToolSummary> {
    let tools: Vec<formless_protocol::ToolSummary> = serde_json::from_value(
        formless_tui::client::request_once(paths, Request::ListTools)?,
    )?;
    cli_tool_lookup::resolve(&tools, name).cloned()
}

fn tool_detail_calls_line(m: &formless_protocol::ToolMetricsInfo) -> String {
    format!(
        "calls:        {} ({} ok, {} failed, {} denied) · success {} · p50 {}",
        m.total,
        m.success,
        m.failure,
        m.denied,
        m.success_rate
            .map(formless_core::success_rate::label)
            .unwrap_or_else(|| "-".into()),
        m.p50_ms
            .map(|v| format!("{v:.0}ms"))
            .unwrap_or_else(|| "-".into())
    )
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
    println!("pinned:       {}", if t.pinned { "yes" } else { "no" });
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
    print_description_evidence(&serde_json::to_value(d).unwrap());
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
    println!("{}", tool_detail_calls_line(m));
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
    print!("{}", cli_tool_detail::history(d));
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
    tool: formless_protocol::ToolSummary,
    input: serde_json::Value,
    yes: bool,
    session: Option<String>,
    dry_run: bool,
    json: bool,
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
                context,
                ..
            }) => {
                for capability in &capabilities {
                    if let Some(warning) = formless_protocol::sensitive_read_warning(capability) {
                        eprintln!("⚠ {capability}: {warning}");
                    }
                }
                if let Some(context) = &context {
                    for line in formless_protocol::approval_context_lines(context) {
                        eprintln!("{line}");
                    }
                }
                eprintln!(
                    "[approval] {tool_name} wants {} -> {}",
                    capabilities.join(", "),
                    if yes {
                        "sending approval for this call (--yes)"
                    } else {
                        "sending denial (pass --yes)"
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
        Ok(v) if json => write_tool_run_json(&v, dry_run, &mut std::io::stdout().lock()),
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
            Ok(tool_run_exit(&v, &text))
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(1)
        }
    }
}

/// `formless man`: the top-level page, then every subcommand (three levels
/// deep) with its synopsis and options, so `--timeout` and friends are in
/// the one page that ships (#895).
fn render_man(out: &mut dyn std::io::Write) -> Result<()> {
    use clap::CommandFactory;
    let mut cli = Cli::command();
    cli.build();
    clap_mangen::Man::new(cli.clone()).render(out)?;
    fn section(
        cmd: &clap::Command,
        path: &str,
        depth: usize,
        out: &mut dyn std::io::Write,
    ) -> Result<()> {
        for sub in cmd.get_subcommands().filter(|c| !c.is_hide_set()) {
            if sub.get_name() == "help" {
                continue;
            }
            let name = format!("{path} {}", sub.get_name());
            let page = clap_mangen::Man::new(sub.clone().bin_name(&name));
            writeln!(out, ".SH \"{}\"", name.to_uppercase())?;
            if let Some(about) = sub.get_about() {
                writeln!(out, "{}", about.to_string().replace('\\', "\\\\"))?;
            }
            for render in [
                clap_mangen::Man::render_synopsis_section,
                clap_mangen::Man::render_options_section,
            ] {
                let mut buf = Vec::new();
                render(&page, &mut buf)?;
                // One heading per command, not SYNOPSIS/OPTIONS each time.
                let text = String::from_utf8_lossy(&buf)
                    .replace(".SH SYNOPSIS\n", ".PP\n")
                    .replace(".SH OPTIONS\n", ".PP\nOptions:\n");
                out.write_all(text.as_bytes())?;
            }
            if depth < 3 {
                section(sub, &name, depth + 1, out)?;
            }
        }
        Ok(())
    }
    section(&cli, "formless", 1, out)
}

fn write_tool_run_json(
    reply: &serde_json::Value,
    dry_run: bool,
    out: &mut impl Write,
) -> Result<i32> {
    serde_json::to_writer_pretty(&mut *out, reply)?;
    writeln!(out)?;
    out.flush()?;
    Ok(if dry_run {
        i32::from(reply["ok"] != true)
    } else {
        tool_run_exit(reply, reply["result"].as_str().unwrap_or_default())
    })
}

/// `tools run`'s exit code: from the daemon's `ok` (#901); an older daemon
/// without it marks failures with a leading "error".
fn tool_run_exit(v: &serde_json::Value, text: &str) -> i32 {
    match v["ok"].as_bool() {
        Some(ok) => i32::from(!ok),
        None => i32::from(text.trim_start().starts_with("error")),
    }
}

fn package_uninstall_command(exe: &str) -> Option<&'static str> {
    if is_homebrew_executable(exe) {
        Some("brew uninstall formless")
    } else if exe.starts_with("/usr/bin/") {
        Some("your package manager (apt remove formless / dnf remove formless / pacman -R formless-bin)")
    } else if exe.contains("WinGet") {
        Some("winget uninstall --id athiwatc.formless --exact")
    } else if exe.contains("/.cargo/bin/") || exe.contains("\\.cargo\\bin\\") {
        Some("cargo uninstall formless-bin")
    } else {
        None
    }
}

/// `formless uninstall` (SPEC §15): every profile's daemon, login service
/// and secrets, then its folders (unless --keep-data), then the binary
/// (unless --keep-binary or a package manager owns it).
fn uninstall_cmd(paths: &Paths, keep_data: bool, keep_binary: bool, yes: bool) -> Result<()> {
    paths.validate_cleanup_namespace()?;
    let profiles = paths.list_profiles();
    let exe = std::env::current_exe().ok();
    // A package manager's binary is removed with the package manager.
    let managed = exe
        .as_ref()
        .and_then(|exe| package_uninstall_command(&exe.to_string_lossy()));
    println!("This removes Formless from this machine:");
    for p in &profiles {
        println!(
            "  profile {}: daemon, login service, keychain entries{}",
            p.profile,
            if keep_data {
                String::new()
            } else {
                format!(", {} and {}", p.data_dir.display(), p.config_dir.display())
            }
        );
    }
    match (&exe, managed, keep_binary) {
        (_, _, true) => {}
        (_, Some(how), _) => {
            println!("  the binary: installed by a package manager, remove it with {how}")
        }
        (Some(e), None, false) => println!("  the binary: {}", e.display()),
        _ => {}
    }
    if !keep_data {
        println!("Sessions, Tools, memories and backups are deleted. Keep a copy first with `formless export-all`.");
    }
    if !yes {
        confirm_typed("", "uninstall")?;
    }
    // What could not be removed; any of it makes the exit non-zero.
    let mut failed: Vec<String> = Vec::new();
    let base = paths.named("default")?;
    let changed = profile_maintenance(
        &base,
        || {
            // Stop/disable every known profile before the first deletion. A
            // single failed preparation preserves all profile files and binary.
            let mut errors = Vec::new();
            for p in &profiles {
                let prepared = (|| -> Result<()> {
                    let plan = formless_core::service::plan_here(p)?;
                    if let Err(error) = formless_core::service::disable(&plan) {
                        let status = formless_core::service::status(p)?;
                        if status.file_present
                            || status.registration
                                != formless_core::service::Registration::NotRegistered
                        {
                            return Err(error);
                        }
                    }
                    stop_daemon(p)
                })();
                match prepared {
                    Ok(()) => println!(
                        "stopped profile {} and disabled its login service",
                        p.profile
                    ),
                    Err(error) => errors.push(format!("{}: {error:#}", p.profile)),
                }
            }
            if !errors.is_empty() {
                anyhow::bail!(
                    "profile preparation failed; all profile files are preserved: {}",
                    errors.join("; ")
                );
            }
            Ok(())
        },
        || Ok(()),
        |guard| {
            erase_profiles(&base, &profiles, guard, |p, guard| {
                let plan = formless_core::service::plan_here(p)?;
                formless_core::service::remove_definition(&plan)?;
                if keep_data {
                    let warnings = formless_core::config::delete_secrets(p);
                    println!(
                        "profile {}: deleted credentials; kept {} and {}",
                        p.profile,
                        p.data_dir.display(),
                        p.config_dir.display()
                    );
                    Ok(warnings)
                } else {
                    let warnings = formless_core::config::reset_all_guarded(p, guard)?;
                    println!("removed profile {}", p.profile);
                    Ok(warnings)
                }
            })
        },
    );
    match changed {
        Ok(warnings) => failed.extend(warnings),
        Err(error) => failed.push(format!(
            "{error:#}; profile cleanup did not complete; its binary is kept"
        )),
    }
    if let (true, Some(e), None, false) = (failed.is_empty(), &exe, managed, keep_binary) {
        // install.sh puts completions and the man page under the
        // binary's prefix.
        let extras: Vec<std::path::PathBuf> = e
            .parent()
            .and_then(|bin| bin.parent())
            .map(|prefix| {
                [
                    "share/bash-completion/completions/formless",
                    "share/zsh/site-functions/_formless",
                    "share/fish/vendor_completions.d/formless.fish",
                    "share/man/man1/formless.1",
                ]
                .iter()
                .map(|f| prefix.join(f))
                .filter(|f| f.exists())
                .collect()
            })
            .unwrap_or_default();
        for f in extras {
            match std::fs::remove_file(&f) {
                Ok(()) => println!("removed {}", f.display()),
                Err(err) => failed.push(format!("{}: {err}", f.display())),
            }
        }
        if cfg!(windows) {
            println!(
                "Windows can't delete a running program: delete {} yourself now.",
                e.display()
            );
        } else {
            match std::fs::remove_file(e) {
                Ok(()) => println!("removed {}", e.display()),
                Err(err) => failed.push(format!(
                    "{}: {err} (delete it yourself, e.g. with sudo)",
                    e.display()
                )),
            }
        }
    }
    println!(
        "Browsers you paired keep their own copy of the relay secret; clear the site data there (it no longer connects to anything)."
    );
    if !failed.is_empty() {
        eprintln!("Not everything was removed:");
        for f in &failed {
            eprintln!("  - {f}");
        }
        return Err(anyhow::anyhow!(
            "uninstall incomplete ({} item(s))",
            failed.len()
        ));
    }
    println!("Formless is uninstalled.");
    Ok(())
}

/// Recheck under the exclusive family freeze: new profiles cannot appear
/// after this point. Never recursively erase a parent after any child failed.
fn erase_profiles(
    base: &Paths,
    expected: &[Paths],
    guard: &formless_core::profile_lock::MaintenanceGuard,
    mut erase: impl FnMut(&Paths, &formless_core::profile_lock::MaintenanceGuard) -> Result<Vec<String>>,
) -> Result<Vec<String>> {
    let names = |profiles: &[Paths]| {
        profiles
            .iter()
            .map(|p| p.profile.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    base.validate_cleanup_namespace()?;
    if names(&base.list_profiles()) != names(expected) {
        anyhow::bail!(
            "profile inventory changed during preparation; nothing was deleted; retry uninstall"
        );
    }
    let mut warnings = Vec::new();
    // list_profiles is default first. Every named child must finish first;
    // any error returns before the recursive default cleanup is reachable.
    for p in expected.iter().rev() {
        if !guard.covers(p)? {
            anyhow::bail!(
                "profile {} moved outside the frozen cleanup namespace",
                p.profile
            );
        }
        for warning in erase(p, guard)? {
            warnings.push(format!("profile {}: {warning}", p.profile));
        }
    }
    Ok(warnings)
}

/// Hold automatic launchers out from preparation through all destructive
/// changes. Daemon constructors independently honor the owner lock, including
/// service-manager restarts that do not pass through automatic launchers.
fn profile_maintenance<T>(
    paths: &Paths,
    prepare: impl FnOnce() -> Result<()>,
    stop: impl FnOnce() -> Result<()>,
    change: impl FnOnce(&formless_core::profile_lock::MaintenanceGuard) -> Result<T>,
) -> Result<T> {
    paths.validate_cleanup_namespace()?;
    let gate = formless_core::profile_lock::ProfileLock::maintenance(
        paths,
        std::time::Duration::from_secs(30),
    )?;
    prepare()?;
    stop()?;
    let guard = gate.freeze()?;
    if formless_core::daemon::endpoint_occupied(paths)? {
        anyhow::bail!("profile daemon became reachable; destructive changes were refused");
    }
    change(&guard)
}

/// Ask a profile's daemon to stop, waiting for both transport and kernel
/// ownership to disappear. Unknown ownership is never an idle profile.
fn stop_daemon(paths: &Paths) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut requested = false;
    loop {
        let occupied = formless_core::daemon::endpoint_occupied(paths)?;
        if !occupied && !formless_core::profile_lock::owner_held(paths)? {
            return Ok(());
        }
        if occupied && !requested {
            formless_tui::client::request_once_within(
                paths,
                Request::Shutdown,
                std::time::Duration::from_secs(5),
            )?;
            requested = true;
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("profile daemon did not release ownership; stop it through its service manager or original binary and retry");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The one confirmation for destructive commands (SPEC §12, #898): on a
/// terminal, type `word`; anywhere else the command's `--force` is
/// required, so piped input can never confirm a wipe.
fn confirm_typed(what: &str, word: &str) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(usage(format!(
            "{what} Not on a terminal, so nothing was asked: pass --force to confirm"
        )));
    }
    eprint!(
        "{}",
        format!("{what} Type '{word}' to confirm: ").trim_start()
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    if line.trim() != word {
        return Err(usage("aborted"));
    }
    Ok(())
}

fn reset_cmd(paths: Paths, force: bool) -> Result<()> {
    if !force {
        confirm_typed(
            "This deletes ALL Formless data (config, API key, tools, sessions).",
            "reset",
        )?;
    }
    let warnings = profile_maintenance(
        &paths,
        || Ok(()),
        || stop_daemon(&paths),
        |guard| formless_core::config::reset_all_guarded(&paths, guard),
    )?;
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
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
#[path = "../../formless-core/tests/support/env.rs"]
mod oauth_credential_env;

#[cfg(test)]
mod cli_doctor_tests;

#[cfg(test)]
mod tests {
    fn override_fixture(id: &str, name: &str) -> super::SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": name,
            "model": "stored-model",
            "reasoning_effort": "low",
            "cwd": "stored-directory",
            "created_at": "",
            "updated_at": "",
        }))
        .unwrap()
    }

    #[test]
    fn headless_overrides_use_the_resolved_target_and_snapshot() {
        use super::{find_session_in, prepare_existing_headless_session};

        let sessions = [
            override_fixture("abc-one", "Primary"),
            override_fixture("abc-two", "Other"),
            override_fixture("xyz-three", "Other"),
        ];
        for selector in ["abc-one", "abc-o", "pRiMaRy"] {
            let resolved = find_session_in(&sessions, selector).unwrap();
            let mut pick = Some((None, Some("high".into())));
            let mut restore = None;
            let id = prepare_existing_headless_session(resolved, &mut pick, true, &mut restore);
            assert_eq!(id, "abc-one");
            assert_eq!(
                pick,
                Some((Some("stored-model".into()), Some("high".into())))
            );
            let snapshot = restore.unwrap();
            assert_eq!(snapshot.id, id);
            assert_eq!(snapshot.model.as_deref(), Some("stored-model"));
            assert_eq!(snapshot.reasoning_effort.as_deref(), Some("low"));
            assert_eq!(snapshot.cwd.as_deref(), Some("stored-directory"));
        }
        for selector in ["abc-", "Other", "missing", ""] {
            assert!(find_session_in(&sessions, selector).is_err());
        }
    }

    #[test]
    fn headless_snapshot_is_needed_only_for_temporary_overrides() {
        use super::prepare_existing_headless_session;

        let mut pick = None;
        for override_cwd in [false, true] {
            let mut restore = None;
            let id = prepare_existing_headless_session(
                override_fixture("abc-one", "Primary"),
                &mut pick,
                override_cwd,
                &mut restore,
            );
            assert_eq!(id, "abc-one");
            assert!(pick.is_none());
            assert_eq!(restore.is_some(), override_cwd);
        }
        let mut pick = Some((Some("explicit-model".into()), None));
        let mut restore = None;
        prepare_existing_headless_session(
            override_fixture("abc-one", "Primary"),
            &mut pick,
            false,
            &mut restore,
        );
        assert_eq!(pick, Some((Some("explicit-model".into()), None)));
        assert_eq!(restore.unwrap().model.as_deref(), Some("stored-model"));
    }

    #[test]
    fn tool_detail_calls_use_shared_success_rate_labels() {
        use super::tool_detail_calls_line;

        for (rate, label) in [
            (None, "-"),
            (Some(0.0), "0%"),
            (Some(2.0 / 3.0), "67%"),
            (Some(0.995), ">99%"),
            (Some(0.9999), ">99%"),
            (Some(1.0), "100%"),
        ] {
            let metrics = formless_protocol::ToolMetricsInfo {
                total: 5,
                success: 2,
                failure: 2,
                denied: 1,
                success_rate: rate,
                p50_ms: Some(12.0),
                ..Default::default()
            };
            assert_eq!(
                tool_detail_calls_line(&metrics),
                format!("calls:        5 (2 ok, 2 failed, 1 denied) · success {label} · p50 12ms")
            );
        }
    }

    #[test]
    fn tool_result_preview_marks_omission_at_complete_grapheme_boundaries() {
        use super::tool_result_preview;

        for (content, expected) in [
            ("", ""),
            ("value", "value"),
            ("value\n", "value"),
            ("value\r\n", "value"),
            ("value\n\n", "value…"),
            ("value\r\n\r\n", "value…"),
            ("value\r\nmore", "value…"),
            ("\nmore", "…"),
        ] {
            assert_eq!(tool_result_preview(content), expected);
        }
        let exact = "界".repeat(120);
        assert_eq!(tool_result_preview(&exact), exact);
        assert_eq!(
            tool_result_preview(&format!("{exact}x")),
            format!("{exact}…")
        );
        let prefix = "x".repeat(119);
        for grapheme in ["e\u{301}", "👍🏽", "👩‍💻"] {
            assert_eq!(
                tool_result_preview(&format!("{prefix}{grapheme}")),
                format!("{prefix}…")
            );
        }
        let oversized = format!("e{}", "\u{301}".repeat(120));
        assert_eq!(tool_result_preview(&oversized), "…");
    }

    #[test]
    fn profile_namespace_parser_refuses_reserved_names_before_running_cli_actions() {
        use clap::Parser;
        for name in [
            "commands",
            "COMMANDS",
            "api_key",
            "artifacts",
            "Default",
            "../outside",
            "a/b",
            "a\\b",
            ".hidden",
            "CON",
        ] {
            let error =
                super::Cli::try_parse_from(["formless", "--profile", name, "profiles", "list"])
                    .err()
                    .unwrap();
            assert!(
                error.to_string().contains("profile name"),
                "{name}: {error}"
            );
            assert!(
                super::Cli::try_parse_from(["formless", "profiles", "rm", name, "--force"])
                    .is_err()
            );
        }
        assert!(super::Cli::try_parse_from([
            "formless",
            "--profile",
            "work-2",
            "profiles",
            "list"
        ])
        .is_ok());
    }

    #[test]
    fn reserved_profile_removal_and_maintenance_never_reach_service_or_secret_hooks() {
        let root = std::env::temp_dir().join(format!(
            "fl-profile-preflight-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let base = formless_core::Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let commands = base.config_dir.join("commands");
        std::fs::create_dir(&commands).unwrap();
        std::fs::write(commands.join("config.toml"), "private legacy marker").unwrap();
        std::fs::write(commands.join("review.md"), "private default template").unwrap();
        assert!(super::profile_removal_target(&base, "commands").is_err());
        let forged = formless_core::Paths {
            data_dir: base.data_dir.join("commands"),
            config_dir: commands.clone(),
            profile: "commands".into(),
        };
        for target in [&forged, &base] {
            let calls = std::cell::Cell::new(0);
            let result = super::profile_maintenance(
                target,
                || {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
                || {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
                |_| {
                    calls.set(calls.get() + 1);
                    Ok(())
                },
            );
            assert!(result.is_err());
            assert_eq!(
                calls.get(),
                0,
                "namespace errors must precede service/secret preparation"
            );
        }
        assert_eq!(
            std::fs::read_to_string(commands.join("review.md")).unwrap(),
            "private default template"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn doctor_run_file_facts_use_only_private_database_and_count_paths_and_payload() {
        let root = std::env::temp_dir().join(format!(
            "fl-doctor-runfiles-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let paths = formless_core::Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        assert!(super::run_file_doctor_facts(&paths).is_null());
        assert!(!paths.db_file().exists());
        let store = formless_core::Store::open(&paths.db_file()).unwrap();
        let session = store.create_session(None).unwrap().id;
        let run = store.create_run(&session).unwrap();
        store
            .snapshot_run_file_capped(
                &session,
                &run,
                "private.txt",
                true,
                Some(b"abc"),
                Some("日"),
                "file",
                None,
                &formless_core::config::RunFilesCfg::default(),
            )
            .unwrap();
        let facts = super::run_file_doctor_facts(&paths);
        assert_eq!(facts["rows"], 1);
        assert_eq!(facts["retained_files"], 1);
        assert_eq!(facts["before_bytes"], 3);
        assert_eq!(facts["metadata_bytes"], 3);
        assert_eq!(facts["retained_bytes"], 6);
        store.forget_run_file(&run, "private.txt").unwrap();
        let facts = super::run_file_doctor_facts(&paths);
        assert_eq!(facts["rows"], 1);
        assert_eq!(facts["retained_files"], 0);
        assert_eq!(facts["retained_bytes"], 0);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn run_file_coverage_stays_on_stderr_in_shipped_cli_fixture() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::run_file_coverage_child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stdout.contains("{\"result\":\"machine-readable\"}"));
        assert!(!stdout.contains("Untracked writes"));
        assert!(stderr.contains("No tracked path changes."));
        assert!(stderr.contains("Untracked writes are possible"));
        assert!(stderr.contains("MCP tools"));
    }

    #[test]
    #[ignore = "private subprocess fixture for CLI output separation"]
    fn run_file_coverage_child_fixture() {
        super::write_run_file_notice(&mut std::io::stderr().lock(), &[], true);
        println!("{{\"result\":\"machine-readable\"}}");
    }

    #[test]
    fn auth_selectors_parse_for_every_action_and_resolve_named_config() {
        use clap::Parser;
        for verb in ["login", "logout", "status"] {
            let parsed =
                super::Cli::try_parse_from(["formless", "auth", verb, "--provider", "work"])
                    .unwrap();
            let Some(super::Cmd::Auth { action }) = parsed.cmd else {
                panic!("wrong command")
            };
            let name = match action {
                super::AuthAction::Login { provider, .. }
                | super::AuthAction::Status { provider, .. }
                | super::AuthAction::Logout { provider } => provider,
            };
            assert_eq!(name.as_deref(), Some("work"));
        }
        let mut cfg = super::Config::default();
        cfg.providers.insert(
            "work".into(),
            formless_core::config::ProviderCfg {
                model: "work-model".into(),
                ..Default::default()
            },
        );
        assert_eq!(cfg.provider_cfg(Some("work")).unwrap().model, "work-model");
        assert_eq!(cfg.provider_cfg(None).unwrap().model, cfg.provider.model);
        assert!(cfg
            .provider_cfg(Some("missing"))
            .unwrap_err()
            .to_string()
            .contains("unknown provider"));
    }

    #[test]
    fn oauth_diagnostics_report_all_providers_without_tokens_or_migration() {
        let _env = super::oauth_credential_env::EnvGuard::isolated();
        // Diagnostics still try the keyring before falling back to files.
        // This test binary uses only private in-memory entries; never restore
        // an OS backend after the serialized fixture has finished.
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let (_fixture, mut paths) = MaintenanceFixture::new();
        paths.profile = format!("oauth-status-{}", std::process::id());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let mut cfg = super::Config::default();
        cfg.provider.auth.mode = "oauth".into();
        cfg.provider.auth.token_url = "https://default.example/token".into();
        cfg.provider.auth.client_id = "default-client".into();
        let mut named = cfg.provider.clone();
        named.auth.token_url = "https://work.example/token".into();
        named.auth.client_id = "work-client".into();
        cfg.providers.insert("work".into(), named.clone());
        let tokens = serde_json::json!({"oauth_record_version":1,"token_endpoint":named.auth.token_url,"client_id":named.auth.client_id,"access_token":"private-work-access","refresh_token":"private-work-refresh","token_type":"Bearer","token_url":formless_core::auth::canonical_token_url(&named.auth.token_url).unwrap()});
        let work_file = formless_core::auth::oauth_tokens_file(&paths, &named.auth);
        formless_core::config::write_secret_file(&work_file, &tokens.to_string()).unwrap();
        let mut legacy = named.clone();
        legacy.auth.token_url = "https://legacy.example/token/".into();
        legacy.auth.client_id = "legacy-client".into();
        cfg.providers.insert("legacy".into(), legacy.clone());
        let legacy_file = formless_core::auth::oauth_tokens_file(&paths, &legacy.auth);
        let legacy_tokens = serde_json::json!({"access_token":"private-legacy-access","refresh_token":"private-legacy-refresh","token_type":"Bearer","token_url":legacy.auth.token_url});
        formless_core::config::write_secret_file(&legacy_file, &legacy_tokens.to_string()).unwrap();
        let before_work = std::fs::read(&work_file).unwrap();
        let before_legacy = std::fs::read(&legacy_file).unwrap();
        let config_files = || {
            let mut files: Vec<_> = std::fs::read_dir(&paths.config_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            files.sort();
            files
        };
        let before_files = config_files();
        let default_file = formless_core::auth::oauth_tokens_file(&paths, &cfg.provider.auth);
        assert!(!default_file.exists());
        let statuses = super::oauth_provider_statuses(&paths, &cfg);
        assert_eq!(statuses.len(), 3);
        assert_eq!(statuses[0]["name"], "default");
        assert_eq!(statuses[0]["credential_present"], false);
        assert!(statuses[0]["warning"].is_null());
        let work = statuses
            .iter()
            .find(|status| status["name"] == "work")
            .unwrap();
        let legacy = statuses
            .iter()
            .find(|status| status["name"] == "legacy")
            .unwrap();
        assert_eq!(work["credential_present"], true);
        assert_eq!(work["has_refresh_token"], true);
        assert!(work["warning"].is_null());
        assert_eq!(work["login_command"], "formless auth login --provider work");
        assert_eq!(legacy["credential_present"], false);
        assert!(legacy["warning"]
            .as_str()
            .unwrap()
            .contains("sign in again"));
        assert_eq!(
            legacy["login_command"],
            "formless auth login --provider legacy"
        );
        let json = serde_json::to_string(&statuses).unwrap();
        for secret in [
            "private-work-access",
            "private-work-refresh",
            "private-legacy-access",
            "private-legacy-refresh",
        ] {
            assert!(!json.contains(secret), "diagnostics exposed {secret}");
        }
        assert_eq!(super::oauth_provider_statuses(&paths, &cfg), statuses);
        assert_eq!(std::fs::read(&work_file).unwrap(), before_work);
        assert_eq!(std::fs::read(&legacy_file).unwrap(), before_legacy);
        assert_eq!(
            config_files(),
            before_files,
            "read-only diagnostics cannot create migration records"
        );
        assert!(!default_file.exists());
        assert!(!paths.oauth_tokens_file().exists());
    }

    struct MaintenanceFixture(std::path::PathBuf);
    impl MaintenanceFixture {
        fn new() -> (Self, super::Paths) {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "fl-maintenance-{}-{stamp}-{next}",
                std::process::id()
            ));
            let paths = super::Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
            (Self(base), paths)
        }
    }
    impl Drop for MaintenanceFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn manager_fixture(paths: &super::Paths, operation: &str) -> super::Result<()> {
        let out = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "tests::maintenance_manager_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("FORMLESS_TEST_MAINTENANCE_DATA", &paths.data_dir)
            .env("FORMLESS_TEST_MAINTENANCE_CONFIG", &paths.config_dir)
            .env("FORMLESS_TEST_MAINTENANCE_OPERATION", operation)
            .output()?;
        if !out.status.success() {
            anyhow::bail!("private manager fixture rejected {operation}");
        }
        Ok(())
    }

    #[test]
    fn uninstall_inventory_change_preserves_default_and_new_child_before_any_erase() {
        let (_fixture, base) = MaintenanceFixture::new();
        std::fs::write(base.db_file(), "default sentinel").unwrap();
        let initial = base.list_profiles();
        let named = base.named("new-child").unwrap();
        let erased = std::cell::Cell::new(false);
        let result = super::profile_maintenance(
            &base,
            || {
                super::Paths::from_dirs(named.data_dir.clone(), named.config_dir.clone())?;
                std::fs::write(named.config_file(), "new child sentinel")?;
                Ok(())
            },
            || Ok(()),
            |guard| {
                super::erase_profiles(&base, &initial, guard, |_, _| {
                    erased.set(true);
                    Ok(Vec::new())
                })
            },
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("inventory changed"));
        assert!(!erased.get());
        assert_eq!(
            std::fs::read_to_string(base.db_file()).unwrap(),
            "default sentinel"
        );
        assert_eq!(
            std::fs::read_to_string(named.config_file()).unwrap(),
            "new child sentinel"
        );
    }

    #[test]
    fn uninstall_never_erases_parent_after_child_failure_and_preparation_failure_erases_nothing() {
        let (_fixture, base) = MaintenanceFixture::new();
        let named = base.named("child").unwrap();
        super::Paths::from_dirs(named.data_dir.clone(), named.config_dir.clone()).unwrap();
        std::fs::write(base.db_file(), "default sentinel").unwrap();
        std::fs::write(named.db_file(), "child sentinel").unwrap();
        let initial = base.list_profiles();
        let erased = std::cell::RefCell::new(Vec::new());
        let result = super::profile_maintenance(
            &base,
            || Ok(()),
            || Ok(()),
            |guard| {
                super::erase_profiles(&base, &initial, guard, |p, _| {
                    erased.borrow_mut().push(p.profile.clone());
                    anyhow::bail!("injected child erase failure")
                })
            },
        );
        assert!(result.is_err());
        assert_eq!(*erased.borrow(), ["child"]);
        assert_eq!(
            std::fs::read_to_string(base.db_file()).unwrap(),
            "default sentinel"
        );
        erased.borrow_mut().clear();
        let result = super::profile_maintenance(
            &base,
            || manager_fixture(&base, "reject"),
            || Ok(()),
            |_| {
                erased.borrow_mut().push("unexpected".into());
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(erased.borrow().is_empty());
        assert_eq!(
            std::fs::read_to_string(named.db_file()).unwrap(),
            "child sentinel"
        );
    }

    // File locks inherited by another parallel test's fork survive until that
    // child execs, even with CLOEXEC. Own manager children are fully awaited;
    // isolate only the fixtures requiring immediate post-drop release from
    // unrelated process creation, preserving their exact assertions below.
    struct IsolatedMaintenanceProcess(std::process::Child);
    impl Drop for IsolatedMaintenanceProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn isolated_maintenance_assertions(mode: &str) {
        assert!(matches!(mode, "release" | "refusal"));
        let mut child = IsolatedMaintenanceProcess(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::maintenance_isolation_child_fixture",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("FORMLESS_TEST_MAINTENANCE_ISOLATION", mode)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "isolated maintenance {mode} fixture failed: {status}"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "isolated maintenance {mode} fixture did not finish within ten seconds"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn destructive_maintenance_holds_cross_process_locks_and_orders_disable_stop_erase() {
        isolated_maintenance_assertions("release");
    }

    #[test]
    fn destructive_maintenance_preserves_data_when_service_or_owner_cannot_release() {
        isolated_maintenance_assertions("refusal");
    }

    #[test]
    #[ignore = "private subprocess fixture for immediate maintenance lock assertions"]
    fn maintenance_isolation_child_fixture() {
        match std::env::var("FORMLESS_TEST_MAINTENANCE_ISOLATION").as_deref() {
            Ok("release") => maintenance_release_assertions(),
            Ok("refusal") => maintenance_refusal_assertions(),
            _ => panic!("invoke only through the isolated maintenance parent tests"),
        }
    }

    fn maintenance_release_assertions() {
        let (_fixture, paths) = MaintenanceFixture::new();
        super::profile_maintenance(
            &paths,
            || manager_fixture(&paths, "disable"),
            || manager_fixture(&paths, "stop"),
            |_| manager_fixture(&paths, "erase"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(paths.config_dir.join("operations")).unwrap(),
            "disable\nstop\nerase\n"
        );
        assert!(!formless_core::profile_lock::owner_held(&paths).unwrap());
        assert!(formless_core::profile_lock::ProfileLock::launcher(
            &paths,
            std::time::Duration::ZERO
        )
        .is_ok());
    }

    fn maintenance_refusal_assertions() {
        let (_fixture, paths) = MaintenanceFixture::new();
        let sentinel = paths.db_file();
        std::fs::write(&sentinel, "untouched data").unwrap();
        let owner = formless_core::profile_lock::ProfileLock::owner(&paths).unwrap();
        let result = super::profile_maintenance(
            &paths,
            || manager_fixture(&paths, "disable"),
            || manager_fixture(&paths, "stop"),
            |_| manager_fixture(&paths, "erase"),
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "untouched data"
        );
        assert_eq!(
            std::fs::read_to_string(paths.config_dir.join("operations")).unwrap(),
            "disable\nstop\n"
        );
        drop(owner);
        std::fs::remove_file(paths.config_dir.join("operations")).unwrap();
        let result = super::profile_maintenance(
            &paths,
            || manager_fixture(&paths, "reject"),
            || manager_fixture(&paths, "stop"),
            |_| manager_fixture(&paths, "erase"),
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "untouched data"
        );
        assert_eq!(
            std::fs::read_to_string(paths.config_dir.join("operations")).unwrap(),
            "reject\n"
        );
        assert!(formless_core::profile_lock::ProfileLock::owner(&paths).is_ok());
    }

    #[test]
    #[ignore = "private subprocess manager fixture invoked by maintenance tests"]
    fn maintenance_manager_fixture() {
        use std::io::Write;
        let paths = super::Paths {
            data_dir: std::env::var_os("FORMLESS_TEST_MAINTENANCE_DATA")
                .expect("private fixture data")
                .into(),
            config_dir: std::env::var_os("FORMLESS_TEST_MAINTENANCE_CONFIG")
                .expect("private fixture config")
                .into(),
            profile: "default".into(),
        };
        let operation = std::env::var("FORMLESS_TEST_MAINTENANCE_OPERATION").unwrap();
        assert!(
            formless_core::profile_lock::ProfileLock::launcher(&paths, std::time::Duration::ZERO)
                .is_err(),
            "launch gate spans every subprocess stage"
        );
        if operation == "erase" {
            assert!(
                formless_core::profile_lock::ProfileLock::try_owner(&paths)
                    .unwrap()
                    .is_none(),
                "destructive changes hold kernel ownership across processes"
            );
        }
        let mut record = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.config_dir.join("operations"))
            .unwrap();
        writeln!(record, "{operation}").unwrap();
        assert_ne!(
            operation, "reject",
            "injected private service-manager failure"
        );
    }

    /// Combinations that would silently drop a flag are refused by clap,
    /// and usage mistakes exit 2 (#896, #897).
    #[test]
    fn conflicting_flags_are_refused_and_usage_exits_2() {
        use clap::Parser;
        let bad: &[&[&str]] = &[
            &["formless", "run", "x", "--json", "--output", "final"],
            &["formless", "tools", "export", "t", "--all"],
            &["formless", "skills", "export", "s", "--all"],
            &["formless", "approvals", "revoke", "t", "--all"],
            &["formless", "jobs", "add", "x"],
            &[
                "formless",
                "jobs",
                "add",
                "x",
                "--cron",
                "0 3 * * *",
                "--at",
                "in 30m",
            ],
            &["formless", "web", "--stop", "--port", "9999"],
        ];
        for args in bad {
            assert!(
                super::Cli::try_parse_from(*args).is_err(),
                "{args:?} was accepted"
            );
        }
        for args in [
            &["formless", "run", "x", "--json"][..],
            &["formless", "jobs", "add", "x", "--every", "every 2 hours"],
            &["formless", "tools", "export", "--all"],
        ] {
            assert!(
                super::Cli::try_parse_from(args).is_ok(),
                "{args:?} was refused"
            );
        }
        assert_eq!(super::exit_code(&super::usage("no Session 'x'")), 2);
        let not_found: anyhow::Error = formless_tui::client::DaemonError {
            code: Some(formless_protocol::codes::NOT_FOUND.into()),
            message: "tool not found".into(),
        }
        .into();
        assert_eq!(super::exit_code(&not_found), 2);
        assert_eq!(super::exit_code(&anyhow::anyhow!("disk full")), 1);
    }

    /// The upgrade command names the install script that exists in the
    /// repository, at the URL the README gives (#834).
    #[test]
    fn the_upgrade_url_points_at_a_real_script() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = super::INSTALL_SCRIPT_URL
            .split("/main/")
            .nth(1)
            .expect("a path in the repository");
        assert!(root.join(path).is_file(), "{path}");
        let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
        assert!(readme.contains(super::INSTALL_SCRIPT_URL));
        // The generic Unix installer command names the checked-in script.
        let cmd = super::platform_upgrade_command("/usr/local/bin/formless", false);
        assert!(
            cmd.starts_with("brew")
                || cmd.starts_with("cargo")
                || cmd.contains(super::INSTALL_SCRIPT_URL),
            "{cmd}"
        );
        assert!(
            !cmd.contains("| sh"),
            "a failed download must not be piped into sh: {cmd}"
        );
        #[cfg(windows)]
        assert_eq!(
            super::upgrade_command(),
            super::WINDOWS_SOURCE_BUILD_UPGRADE_GUIDANCE
        );
    }

    #[cfg(unix)]
    #[test]
    fn upgrade_download_uses_a_private_temporary_path_and_runs_only_on_success() {
        use std::os::unix::fs::PermissionsExt;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("fl-install-{}-{stamp}", std::process::id()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let tools = root.join("bin");
        let temporary = root.join("temporary downloads");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::create_dir_all(&temporary).unwrap();
        let fake_curl = tools.join("curl");
        std::fs::write(
            &fake_curl,
            r#"#!/bin/sh
while [ "$#" -gt 1 ] && [ "$1" != "-o" ]; do shift; done
[ "$1" = "-o" ] || exit 2
printf '%s\n' "$2" > "$FORMLESS_FIXTURE_INSTALL_TARGET"
printf '%s\n' 'printf "ran\n" > "$FORMLESS_FIXTURE_INSTALL_MARKER"' > "$2"
exit "$FORMLESS_FIXTURE_CURL_STATUS"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&fake_curl, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths([tools, "/usr/bin".into(), "/bin".into()]).unwrap();
        let target = root.join("target");
        let marker = root.join("executed");
        let mut downloads = Vec::new();
        for (curl_status, installed) in [("19", false), ("0", true)] {
            let result = std::process::Command::new("sh")
                .args(["-c", super::INSTALL_COMMAND])
                .env("PATH", &path)
                .env("TMPDIR", &temporary)
                .env("FORMLESS_FIXTURE_INSTALL_TARGET", &target)
                .env("FORMLESS_FIXTURE_INSTALL_MARKER", &marker)
                .env("FORMLESS_FIXTURE_CURL_STATUS", curl_status)
                .output()
                .unwrap();
            assert_eq!(result.status.success(), installed, "{result:?}");
            assert_eq!(marker.exists(), installed);
            let downloaded =
                std::path::PathBuf::from(std::fs::read_to_string(&target).unwrap().trim());
            assert!(downloaded.starts_with(&temporary));
            assert!(!downloaded.parent().unwrap().exists());
            downloads.push(downloaded);
        }
        assert_ne!(downloads[0], downloads[1]);
    }

    #[test]
    fn cargo_upgrade_uses_git_package_not_a_checkout_path() {
        let cmd = super::upgrade_command_for_exe("/home/user/.cargo/bin/formless");
        let (install, force) = cmd.rsplit_once(' ').expect("Cargo upgrade flag");
        assert_eq!(force, "--force");
        assert_eq!(
            install.split_whitespace().collect::<Vec<_>>(),
            [
                "cargo",
                "install",
                "--git",
                "https://github.com/athiwatc/formless",
                "--locked",
                "formless-bin",
            ]
        );
        let readme = include_str!("../../../README.md");
        assert!(readme.contains(&format!("`{install}`")));
    }

    #[test]
    fn windows_upgrade_uses_cargo_or_winget() {
        assert!(
            super::platform_upgrade_command(r"C:\Users\person\.Cargo\bin\formless.exe", true)
                .starts_with("cargo install ")
        );
        assert_eq!(
            super::platform_upgrade_command(
                r"C:\Users\person\AppData\Local\Microsoft\WinGet\Packages\athiwatc.formless\formless.exe",
                true
            ),
            "winget upgrade --id athiwatc.formless --exact"
        );
        assert_eq!(
            super::platform_upgrade_command(
                r"C:\Users\person\AppData\Local\Microsoft\WinGet\Links\formless.exe",
                true
            ),
            "winget upgrade --id athiwatc.formless --exact"
        );
        assert_eq!(
            super::platform_upgrade_command(
                r"C:\Users\person\source\formless\target\debug\deps\formless-test.exe",
                true
            ),
            super::WINDOWS_SOURCE_BUILD_UPGRADE_GUIDANCE
        );
        assert_eq!(
            super::platform_upgrade_command(
                r"C:\Users\person\AppData\Local\Programs\Formless\manual\formless.exe",
                true
            ),
            super::WINDOWS_MANUAL_UPGRADE_GUIDANCE
        );
    }

    #[test]
    fn uninstall_uses_the_package_manager_for_winget_and_homebrew() {
        for exe in [
            "/home/linuxbrew/.linuxbrew/bin/formless",
            "/opt/homebrew/bin/formless",
            "/usr/local/Cellar/formless/1.0/bin/formless",
        ] {
            assert_eq!(
                super::package_uninstall_command(exe),
                Some("brew uninstall formless")
            );
            assert_eq!(
                super::platform_upgrade_command(exe, false),
                "brew upgrade formless"
            );
        }
        assert_eq!(
            super::package_uninstall_command(
                r"C:\Users\example\AppData\Local\Microsoft\WinGet\Packages\athiwatc.formless\formless.exe"
            ),
            Some("winget uninstall --id athiwatc.formless --exact")
        );
        assert_eq!(super::package_uninstall_command("/tmp/formless"), None);
    }

    #[cfg(unix)]
    #[test]
    fn linked_intel_homebrew_binary_uses_brew_for_update_and_uninstall() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("formless-brew-{}-{stamp}", std::process::id()));
        let prefix = root.join("usr/local");
        let bin = prefix.join("bin");
        let keg = prefix.join("Cellar/formless/0.2.0/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&keg).unwrap();
        std::fs::write(keg.join("formless"), b"fixture").unwrap();
        let exe = bin.join("formless");
        std::os::unix::fs::symlink("../Cellar/formless/0.2.0/bin/formless", &exe).unwrap();

        assert_eq!(
            super::upgrade_command_for_exe(exe.to_str().unwrap()),
            "brew upgrade formless"
        );
        assert_eq!(
            super::package_uninstall_command(exe.to_str().unwrap()),
            Some("brew uninstall formless")
        );
        assert_eq!(
            std::fs::read_link(&exe).unwrap(),
            std::path::PathBuf::from("../Cellar/formless/0.2.0/bin/formless")
        );
        let ordinary = root.join(".local/bin/formless");
        std::fs::create_dir_all(ordinary.parent().unwrap()).unwrap();
        std::fs::write(&ordinary, b"unmanaged fixture").unwrap();
        assert_eq!(
            super::package_uninstall_command(ordinary.to_str().unwrap()),
            None
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Root presentation order is deliberate (#1609), independently of inventory.
    #[test]
    fn root_help_and_man_use_alphabetical_command_order() {
        use clap::CommandFactory;
        let mut cli = super::Cli::command().term_width(160);
        cli.build();
        let mut expected: Vec<_> = cli
            .get_subcommands()
            .filter(|command| !command.is_hide_set() && command.get_name() != "help")
            .map(|command| command.get_name().to_owned())
            .collect();
        expected.sort();
        expected.push("help".into());
        for help in [
            cli.render_help().to_string(),
            cli.render_long_help().to_string(),
        ] {
            let commands = help
                .split("Commands:\n")
                .nth(1)
                .unwrap()
                .split("\nOptions:")
                .next()
                .unwrap();
            let actual: Vec<_> = commands
                .lines()
                .filter_map(|line| {
                    let line = line.strip_prefix("  ")?;
                    if line.starts_with(' ') {
                        return None;
                    }
                    line.split_whitespace().next()
                })
                .collect();
            assert_eq!(actual, expected, "{help}");
        }
        let mut man = Vec::new();
        clap_mangen::Man::new(cli)
            .render_subcommands_section(&mut man)
            .unwrap();
        let man = String::from_utf8(man).unwrap();
        let positions: Vec<_> = expected
            .iter()
            .map(|name| {
                let escaped = name.replace('-', "\\-");
                man.find(&format!("formless\\-{escaped}(1)"))
                    .unwrap_or_else(|| panic!("missing {name}: {man}"))
            })
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{man}");
        let mut page = Vec::new();
        super::render_man(&mut page).unwrap();
        let page = String::from_utf8(page).unwrap();
        let positions: Vec<_> = expected
            .iter()
            .filter(|name| name.as_str() != "help")
            .map(|name| {
                page.find(&format!(".SH \"FORMLESS {}\"", name.to_uppercase()))
                    .unwrap()
            })
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// The man page documents nested options (#895).
    #[test]
    fn the_man_page_has_subcommand_options() {
        let mut buf = Vec::new();
        super::render_man(&mut buf).unwrap();
        let page = String::from_utf8(buf).unwrap();
        for needle in [
            "FORMLESS RUN",
            "FORMLESS TOOLS RUN",
            "\\-\\-timeout",
            "\\-\\-dry\\-run",
        ] {
            assert!(page.contains(needle), "missing {needle}");
        }
    }

    /// SPEC §12: every `list`/`status` subcommand takes `--json` (#899).
    #[test]
    fn every_list_and_status_takes_json() {
        use clap::CommandFactory;
        fn walk(cmd: &clap::Command, path: String, missing: &mut Vec<String>) {
            for sub in cmd.get_subcommands() {
                let here = format!("{path} {}", sub.get_name());
                if matches!(sub.get_name(), "list" | "status")
                    && !sub.get_arguments().any(|a| a.get_id() == "json")
                {
                    missing.push(here.clone());
                }
                walk(sub, here, missing);
            }
        }
        let mut missing = Vec::new();
        walk(&super::Cli::command(), "formless".into(), &mut missing);
        assert!(missing.is_empty(), "no --json: {missing:?}");
    }
    #[test]
    fn tool_run_exits_from_the_outcome_not_the_text() {
        let ok = serde_json::json!({"result": "errors: 0", "ok": true});
        assert_eq!(super::tool_run_exit(&ok, "errors: 0"), 0);
        let failed = serde_json::json!({"result": "boom", "ok": false});
        assert_eq!(super::tool_run_exit(&failed, "boom"), 1);
        let old = serde_json::json!({"result": "error: x"});
        assert_eq!(super::tool_run_exit(&old, "error: x"), 1);
    }

    #[test]
    fn tool_run_json_propagates_output_errors() {
        struct BrokenPipe;
        impl std::io::Write for BrokenPipe {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        let reply = serde_json::json!({"ok":true,"result":"success"});
        assert!(super::write_tool_run_json(&reply, false, &mut BrokenPipe).is_err());
    }

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

        let final_release = serde_json::json!([
            {"tag_name": "v0.2.0-rc.2", "prerelease": true, "body": "rc", "html_url": "rc"},
            {"tag_name": "v0.2.0", "body": "Final release", "html_url": "final"},
        ]);
        assert_eq!(
            newer_releases(&final_release, "0.2.0-rc.1"),
            [("v0.2.0".into(), "Final release".into(), "final".into())]
        );
        assert!(newer_releases(&final_release, "0.2.0+build.5").is_empty());
    }

    #[test]
    fn bug_reports_redact_github_app_refresh_tokens() {
        let token = format!("ghr_{}", "0123456789abcdefghijklmnop");
        assert_eq!(
            redact(&format!("github: {token}, ghr_short")),
            "github: ghr-[redacted], ghr_short"
        );
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
    fn reports_redact_native_home_and_shell_spellings_in_text_and_json() {
        use std::borrow::Cow;
        let homes = [
            r"C:\Users\Example Person",
            "/c/Users/Example Person",
            "/home/fixture",
        ];
        let input = serde_json::json!({
            "native": r"C:\Users\Example Person\AppData\Formless",
            "native_slashes": "C:/Users/Example Person/AppData/Formless",
            "shell": "/c/Users/Example Person/.config/formless",
            "unix": "/home/fixture/.local/share/formless",
            "ordinary": r"C:\Work\Formless",
        });
        let masked = redact_report_home_paths(&input.to_string(), homes.map(Cow::Borrowed));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&masked).unwrap(),
            serde_json::json!({
                "native": r"~\AppData\Formless",
                "native_slashes": "~/AppData/Formless",
                "shell": "~/.config/formless",
                "unix": "~/.local/share/formless",
                "ordinary": r"C:\Work\Formless",
            })
        );
        assert_eq!(
            redact_report_home_paths(
                r"log at C:\Users\Example Person\logs",
                homes.map(Cow::Borrowed)
            ),
            r"log at ~\logs"
        );
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
        assert!(d("0").unwrap_err().contains("is not a duration"));
        for too_long in [
            "18446744073709551616",
            "5124095576030432h",
            "18446744073709551615s1s",
        ] {
            assert!(
                d(too_long).unwrap_err().contains("is too long"),
                "{too_long}"
            );
        }
        assert_eq!(
            format_duration(std::time::Duration::from_secs(5400)),
            "1h30m"
        );
        assert_eq!(format_duration(std::time::Duration::from_secs(45)), "45s");
    }

    #[test]
    fn jobs_history_formats_elapsed_time_and_missing_values() {
        let started_at = "2026-11-18T10:00:00.123Z";
        for (finished_at, expected) in [
            (Some("2026-11-18T10:00:00.579Z"), "456ms"),
            (Some("2026-11-18T10:00:01.122999999Z"), "999ms"),
            (Some("2026-11-18T10:00:01.123Z"), "1s"),
            (Some("2026-11-18T10:00:45.123Z"), "45s"),
            (Some("2026-11-18T12:30:00.123+01:00"), "1h30m"),
            (Some(started_at), "0ms"),
            (Some("2026-11-18T10:00:00.122999999Z"), "-"),
            (Some("2026-11-18T09:59:57.123Z"), "-"),
            (Some("not a timestamp"), "-"),
            (None, "-"),
        ] {
            assert_eq!(
                format_job_history_duration(started_at, finished_at),
                expected,
                "{finished_at:?}"
            );
        }
        assert_eq!(
            format_job_history_duration("not a timestamp", Some(started_at)),
            "-"
        );
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
        assert_eq!(parse_version("v1.2.3"), Some(semver::Version::new(1, 2, 3)));
        assert_eq!(parse_version("0.1"), Some(semver::Version::new(0, 1, 0)));
        assert_eq!(compare_versions("0.1.0", "v0.2.0"), Some(Ordering::Less));
        assert_eq!(compare_versions("1.0.0", "v1.0.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("2.0.0", "v1.9.9"), Some(Ordering::Greater));
        assert_eq!(
            compare_versions("0.2.0-rc.1", "v0.2.0"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("0.2.0", "v0.2.0-rc.1"),
            Some(Ordering::Greater)
        );
        assert_eq!(
            compare_versions("0.2.0-rc.2", "v0.2.0-rc.10"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("0.2.0+build.5", "v0.2.0"),
            Some(Ordering::Equal)
        );
        assert_eq!(parse_version("nope"), None);
    }
}

#[cfg(test)]
#[path = "log_cli_tests.rs"]
mod log_cli_tests;

#[cfg(test)]
mod pairing_qr_width_tests {
    #[test]
    fn pairing_qr_fit_counts_display_columns_and_includes_the_quiet_zone() {
        use unicode_width::UnicodeWidthStr;
        let lines = formless_tui::qr::lines("https://example.invalid/#k=private-fixture").unwrap();
        let required = super::pairing_qr_columns(&lines);
        assert_eq!(required, lines[0].width());
        assert!(
            required < lines[0].len(),
            "block glyph bytes must not count as columns"
        );
        assert!(super::pairing_qr_fits(
            &lines,
            Some(required.try_into().unwrap())
        ));
        assert!(super::pairing_qr_fits(
            &lines,
            Some((required + 1).try_into().unwrap())
        ));
        assert!(!super::pairing_qr_fits(
            &lines,
            Some((required - 1).try_into().unwrap())
        ));
        assert!(!super::pairing_qr_fits(&lines, None));
        assert!(!super::pairing_qr_fits(&[], Some(80)));
    }
}

#[cfg(test)]
#[path = "session_search_tests.rs"]
mod session_search_tests;

#[cfg(test)]
#[path = "tui_start_cli_tests.rs"]
mod tui_start_cli_tests;

#[cfg(test)]
mod config_get_json_tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn config_get_json_preserves_value_type_content_and_flag_defaults() {
        for value in [
            json!("true"),
            json!("ไทย \"quoted\"\nnext"),
            json!("{\"x\":true}"),
            json!(true),
            json!(1),
            json!([1, "x"]),
            json!({"key": true}),
            Value::Null,
        ] {
            let encoded = config_get_output(&value, true).unwrap();
            assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
            if let Value::String(text) = &value {
                assert_eq!(config_get_output(&value, false).unwrap(), *text);
            } else {
                assert_eq!(
                    config_get_output(&value, false).unwrap(),
                    serde_json::to_string_pretty(&value).unwrap()
                );
            }
        }
        for (args, expected) in [
            (vec!["formless", "config", "get", "provider.model"], false),
            (
                vec!["formless", "config", "get", "provider.model", "--json"],
                true,
            ),
        ] {
            let parsed = Cli::try_parse_from(args).unwrap();
            assert!(
                matches!(parsed.cmd, Some(Cmd::Config { action: ConfigAction::Get { json, .. } }) if json == expected)
            );
        }
    }

    #[test]
    fn config_get_json_string_survives_existing_typed_setting_validation() {
        for text in ["true", "ไทย \"quoted\"\nnext", "{\"x\":true}"] {
            let value = json!(text);
            let output = config_get_output(&value, true).unwrap();
            let parsed =
                serde_json::from_str(&output).unwrap_or_else(|_| Value::String(output.clone()));
            let mut config = Config::default();
            config.set_key("provider.model", parsed).unwrap();
            assert_eq!(config.provider.model, text);
            assert_eq!(config.setting_value("provider.model").unwrap(), Some(value));
        }
        assert!(Config::default()
            .set_key("provider.model", json!(true))
            .is_err());
    }
}
