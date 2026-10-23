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
    /// Diagnostics: config, dirs, keychain, endpoint probe
    Doctor,
    /// Apply artifact retention now (artifacts.max_total_mb / max_age_days)
    Gc,
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
    /// Check for a newer Formless release
    Update {
        #[command(subcommand)]
        action: UpdateAction,
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
        /// Continue an existing session by id (default: ephemeral)
        #[arg(long)]
        session: Option<String>,
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
    List,
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
    List,
    Export {
        /// Tool name or id (omit with --all)
        name: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(short, long)]
        output: Option<String>,
    },
    Import {
        file: String,
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
    List,
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
    List,
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
    List,
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
    List,
    /// Schedule a prompt: --cron "0 3 * * *" (recurring) or --at "in 30m"
    Add {
        prompt: String,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long)]
        at: Option<String>,
        /// Run in this Session (default: a new Session each time)
        #[arg(long)]
        session: Option<String>,
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
    /// Add a server: `add NAME -- COMMAND [ARGS]` (stdio) or `add NAME --url URL` (http)
    Add {
        name: String,
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
    List,
    /// Restart a server and list its tools again
    Test { name: String },
    /// Authorize Formless with a remote (http) MCP server via OAuth
    Login { server: String },
    /// Delete a server's stored OAuth tokens
    Logout { server: String },
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
enum DaemonAction {
    /// Run the daemon (used by client auto-start; blocks)
    Serve,
    /// Start the daemon if not running
    Start,
    Stop,
    Status,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    // Completions need nothing else: no profile, config or daemon.
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
        Some(Cmd::Doctor) => doctor(paths),
        Some(Cmd::Mcp { action }) => mcp_cmd(paths, action),
        Some(Cmd::Skills { action }) => skills_cmd(paths, action),
        Some(Cmd::Memory { action }) => memory_cmd(paths, action),
        Some(Cmd::Runs { limit, json }) => runs_cmd(paths, limit, json),
        Some(Cmd::Jobs { action }) => jobs_cmd(paths, action),
        Some(Cmd::Providers { action }) => providers_cmd(paths, action),
        Some(Cmd::Sessions { action }) => sessions_cmd(paths, action),
        Some(Cmd::Config { action }) => config_cmd(paths, action),
        Some(Cmd::Completions { .. }) => unreachable!("handled before the profile is set up"),
        Some(Cmd::Roles { action }) => roles_cmd(paths, action),
        Some(Cmd::Web {
            port,
            no_open,
            stop,
        }) => web_cmd(paths, port, no_open, stop),
        Some(Cmd::Gc) => {
            let cfg = Config::load(&paths)?;
            let store = formless_core::Store::open(&paths.db_file())?;
            let c = formless_core::artifacts::gc(&store, &cfg.artifacts)?;
            let (left, bytes) = formless_core::artifacts::usage(&store)?;
            println!(
                "removed {} artifact(s), {:.1} MB; {left} left ({:.1} MB)",
                c.files,
                c.bytes as f64 / 1e6,
                bytes as f64 / 1e6
            );
            Ok(())
        }
        Some(Cmd::Auth { action }) => auth_cmd(paths, action),
        Some(Cmd::Relay { action }) => relay_cmd(paths, action),
        Some(Cmd::Prompts { action }) => prompts_cmd(paths, action),
        Some(Cmd::Update { .. }) => update_check(),
        Some(Cmd::Tools { action }) => tools_cmd(paths, action),
        Some(Cmd::Naming { action }) => naming_cmd(paths, action),
        Some(Cmd::Reset { force }) => reset_cmd(paths, force),
        Some(Cmd::Run {
            task,
            session,
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
        }) => {
            let output = if json { RunOutput::JsonlEvents } else { output };
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

fn update_check() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let body = match http_get("https://api.github.com/repos/athiwatc/formless/releases/latest") {
        Ok(b) => b,
        Err(e) if e.to_string().contains("404") => {
            println!("installed: v{current}");
            println!("latest:    (no published releases yet)");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let v: Value = serde_json::from_str(&body).context("parse latest release")?;
    let tag = v["tag_name"]
        .as_str()
        .context("latest release has no tag_name")?;
    let url = v["html_url"]
        .as_str()
        .unwrap_or("https://github.com/athiwatc/formless/releases/latest");
    println!("installed: v{current}");
    println!("latest:    {tag}");
    match compare_versions(current, tag) {
        Some(std::cmp::Ordering::Less) => println!("update available: {url}"),
        Some(_) => println!("up to date"),
        None => println!("could not compare versions"),
    }
    Ok(())
}

fn prompts_cmd(paths: Paths, action: PromptsAction) -> Result<()> {
    use formless_tui::client::request_once;
    formless_core::daemon::ensure_daemon(&paths)?;
    match action {
        PromptsAction::List => {
            let v = request_once(&paths, Request::ListProposals)?;
            let arr = v.as_array().cloned().unwrap_or_default();
            if arr.is_empty() {
                println!("no proposals yet (formless prompts generate)");
            }
            for p in arr {
                println!(
                    "{} [{}] {} cand={} base={} {}",
                    p["id"].as_str().unwrap_or_default(),
                    p["kind"].as_str().unwrap_or_default(),
                    p["status"].as_str().unwrap_or_default(),
                    p["candidate_score"],
                    p["baseline_score"],
                    p["content"].as_str().unwrap_or_default()
                );
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
        RelayAction::Status { qr } => {
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
        DaemonAction::Serve => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let d = Arc::new(Daemon::new(paths)?);
                d.serve().await
            })
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
        DaemonAction::Status => {
            if daemon_reachable(&paths) {
                let v = formless_tui::client::request_once(&paths, Request::Status)?;
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("daemon not running");
            }
            Ok(())
        }
    }
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

fn doctor(paths: Paths) -> Result<()> {
    use formless_core::Store;
    println!("data dir:   {}", paths.data_dir.display());
    println!("config dir: {}", paths.config_dir.display());
    println!(
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
        println!(
            "artifacts:  {n} ({:.1} MB; caps {} MB, {} days)",
            bytes as f64 / 1e6,
            cfg.artifacts.max_total_mb,
            cfg.artifacts.max_age_days
        );
    }
    println!(
        "logs:       {:.1} MB (rotates at {:.1} MB, keeps {})",
        formless_core::logs::log_usage(&paths) as f64 / 1e6,
        cfg.daemon.log_max_bytes as f64 / 1e6,
        cfg.daemon.log_keep
    );
    println!(
        "base_url:   {}",
        if cfg.provider.base_url.is_empty() {
            "(unset)"
        } else {
            &cfg.provider.base_url
        }
    );
    println!(
        "model:      {}",
        if cfg.provider.model.is_empty() {
            "(unset)"
        } else {
            &cfg.provider.model
        }
    );
    println!(
        "api style:  {} (tools: {}, fallbacks: [{}])",
        cfg.provider.api_style,
        cfg.provider.tool_mode,
        cfg.provider.fallback_models.join(", ")
    );
    if formless_core::config::ProviderCfg::is_auto_model(&cfg.provider.model) {
        println!(
            "routing:    auto over [{}]",
            cfg.provider.routing_candidates().join(", ")
        );
        if let Ok(store) = Store::open(&paths.db_file()) {
            if let Ok(stats) = store.list_model_stats() {
                let picked = formless_core::routing::select_model(&cfg.provider, &stats, false);
                println!(
                    "            simple task now routes to: {}",
                    picked.as_deref().unwrap_or("(no candidates)")
                );
                for st in &stats {
                    println!(
                        "            {}: ok={} fail={} total_ms={}",
                        st.model, st.successes, st.failures, st.total_ms
                    );
                }
            }
        }
    }
    let auth_status = formless_core::auth::auth_status(&paths, &cfg.provider);
    println!(
        "auth:       {} ({})",
        auth_status.mode,
        if auth_status.credential_present {
            "credential present"
        } else {
            "MISSING"
        }
    );
    if cfg.provider.oauth() {
        println!(
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
        println!("auth error: {error:#}");
    }
    println!("naming:     {}", naming_model_status(&paths, &cfg));
    println!(
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
                println!("probe:      skipped (model=auto with no routing candidates)")
            }
            (_, Err(_)) => println!("probe:      skipped (credential unavailable)"),
            (Some(probe_model), Ok(credential)) => {
                match probe_blocking(&cfg.provider, credential, &probe_model) {
                    Ok(r) => {
                        println!(
                            "probe:      models={} streaming={} tool_calling={} prompt_fallback={}",
                            r.models.len(),
                            r.streaming,
                            r.tool_calling,
                            r.prompt_fallback
                        );
                        if !r.tool_calling {
                            println!("WARNING: endpoint/model does not support tool-calling; sessions will be refused (SPEC §4.3).");
                        }
                        match formless_core::provider::cache_probe_blocking(
                            &cfg.provider,
                            credential,
                            &probe_model,
                        ) {
                            Ok(Some(u)) if u.cached_tokens > 0 => println!(
                                "cache:      yes ({} of {} prompt tokens cached on a repeat)",
                                u.cached_tokens, u.prompt_tokens
                            ),
                            Ok(Some(u)) => println!(
                                "cache:      no cached tokens on a repeated {}-token prompt",
                                u.prompt_tokens
                            ),
                            Ok(None) => {
                                println!("cache:      unknown (endpoint reports no token usage)")
                            }
                            Err(e) => println!("cache:      probe failed: {e}"),
                        }
                    }
                    Err(e) => {
                        let x = formless_core::errors::explain(
                            &format!("{e:#}"),
                            &formless_core::errors::ErrorContext::from_config(&cfg, &probe_model),
                        );
                        println!("probe:      FAILED ({}): {}", x.category, x.message);
                        if let Some(action) = x.action {
                            println!("            next: {action}");
                        }
                        println!("            details: {e:#}");
                    }
                }
            }
        }
    } else {
        println!("probe:      skipped (not configured; run `formless` for the wizard)");
    }
    Ok(())
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
    let cfg = Config::load(&paths)?;
    if !cfg.provider.is_configured() {
        eprintln!("not configured; run `formless` for the wizard");
        return Ok(2);
    }
    formless_core::daemon::ensure_daemon(&paths)?;
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
                    }
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
                "status": match code { 0 => "completed", 124 => "timed_out", 130 => "interrupted", _ if run_error.is_some() => "paused", _ => "failed" },
                "answer": final_answer,
                "error": run_error,
            })
        ),
        _ => {}
    }
    Ok(code)
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
        SkillsAction::List => {
            let list = daemon(Request::ListSkills)?;
            let skills = list.as_array().cloned().unwrap_or_default();
            if skills.is_empty() {
                println!("no skills installed (formless skills add …)");
            }
            for s in skills {
                println!(
                    "{:<24} v{:<3} {:<8} {:>4} loads {:>5}  {}{}{}",
                    s["name"].as_str().unwrap_or_default(),
                    s["version"],
                    if s["retired"] == true {
                        "retired"
                    } else if s["enabled"] == true {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    s["loads"],
                    match (s["runs"].as_i64(), s["runs_completed"].as_i64()) {
                        (Some(r), Some(c)) if r > 0 => format!("{}%", 100 * c / r),
                        _ => "-".into(),
                    },
                    s["description"].as_str().unwrap_or_default(),
                    if s["has_scripts"] == true {
                        "  [scripts]"
                    } else {
                        ""
                    },
                    if s["author"] == "auto" {
                        "  [auto]"
                    } else {
                        ""
                    },
                );
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
            for s in list.iter().filter(|s| s.archived == archived) {
                println!(
                    "{}  {}  {}{}",
                    &s.id[..8.min(s.id.len())],
                    s.updated_at.get(..16).unwrap_or(&s.updated_at),
                    s.name,
                    if s.pinned { "  (pinned)" } else { "" }
                );
            }
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
        ProvidersAction::List => {
            let r = daemon(Request::ListProviders)?;
            for p in r["providers"].as_array().into_iter().flatten() {
                println!(
                    "{:<14} {:<40} model {:<24} {}",
                    p["name"].as_str().unwrap_or_default(),
                    p["base_url"].as_str().unwrap_or_default(),
                    p["model"].as_str().filter(|m| !m.is_empty()).unwrap_or("-"),
                    if p["has_key"] == true {
                        "key stored"
                    } else {
                        "no key"
                    }
                );
            }
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
        RolesAction::List => {
            let r = daemon(Request::ListProviders)?;
            let roles = r["roles"].as_object().cloned().unwrap_or_default();
            if roles.is_empty() {
                println!(
                    "no roles (formless roles add NAME --description … --provider … --model …)"
                );
            }
            for (name, role) in roles {
                println!(
                    "{:<14} {} · {}\n               {}",
                    name,
                    role["provider"].as_str().unwrap_or("default"),
                    role["model"]
                        .as_str()
                        .filter(|m| !m.is_empty())
                        .unwrap_or("provider default"),
                    role["description"].as_str().unwrap_or_default()
                );
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
        JobsAction::List => {
            let list = daemon(Request::ListJobs)?;
            let jobs = list.as_array().cloned().unwrap_or_default();
            if jobs.is_empty() {
                println!("no scheduled jobs (formless jobs add …)");
            }
            for j in jobs {
                println!(
                    "{:<10} {:<18} {:<14} next {:<25} last: {}\n           {}",
                    j["id"].as_str().unwrap_or_default(),
                    if j["approved"] == false {
                        "awaiting approval"
                    } else if j["enabled"] == true {
                        "on"
                    } else {
                        "off"
                    },
                    j["cron"].as_str().unwrap_or("once"),
                    j["next_run"].as_str().unwrap_or("-"),
                    j["last_status"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("-"),
                    j["prompt"].as_str().unwrap_or_default()
                );
            }
        }
        JobsAction::Add {
            prompt,
            cron,
            at,
            session,
        } => {
            let j = daemon(Request::AddJob {
                prompt,
                session_id: session,
                cron,
                at,
            })?;
            println!(
                "scheduled {} — next run {}",
                j["id"].as_str().unwrap_or_default(),
                j["next_run"].as_str().unwrap_or("-")
            );
        }
        JobsAction::Enable { id } => {
            daemon(Request::SetJobEnabled {
                id: id.clone(),
                enabled: true,
            })?;
            println!("enabled {id}");
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
            url,
            headers,
            env_keys,
            command,
        } => {
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
        McpAction::List => {
            let list = daemon(Request::ListMcpServers)?;
            let servers = list.as_array().cloned().unwrap_or_default();
            if servers.is_empty() {
                println!("no MCP servers configured (formless mcp add …)");
            }
            for s in servers {
                println!(
                    "{:<16} {:<5} {:<8} {:>3} tools  {}{}",
                    s["name"].as_str().unwrap_or_default(),
                    s["transport"].as_str().unwrap_or_default(),
                    if s["disabled"] == true {
                        "disabled"
                    } else if s["running"] == true {
                        "running"
                    } else {
                        "idle"
                    },
                    s["tool_count"],
                    s["target"].as_str().unwrap_or_default(),
                    s["last_error"]
                        .as_str()
                        .map(|e| format!("\n  last error: {e}"))
                        .unwrap_or_default(),
                );
            }
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

fn tools_cmd(paths: Paths, action: ToolsAction) -> Result<()> {
    use formless_tui::client::request_once;
    formless_core::daemon::ensure_daemon(&paths)?;
    match action {
        ToolsAction::List => {
            let v = request_once(&paths, Request::ListTools)?;
            let tools: Vec<formless_protocol::ToolInfo> = serde_json::from_value(v)?;
            if tools.is_empty() {
                println!("(no tools yet - the agent builds them via create_tool)");
            }
            for t in tools {
                println!(
                    "{} v{} calls={} rate={}{} [{}]",
                    t.name,
                    t.current_version,
                    t.invocations,
                    t.success_rate
                        .map(|r| format!("{:.0}%", r * 100.0))
                        .unwrap_or_else(|| "-".into()),
                    if t.retired {
                        " RETIRED"
                    } else if t.needs_attention {
                        " NEEDS-ATTENTION"
                    } else {
                        ""
                    },
                    t.capabilities.join(",")
                );
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
        ToolsAction::Import { file } => {
            let text = std::fs::read_to_string(&file)?;
            let bundle: serde_json::Value = serde_json::from_str(&text)?;
            let v = request_once(&paths, Request::ImportTool { bundle })?;
            println!("imported: {v}");
            Ok(())
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
