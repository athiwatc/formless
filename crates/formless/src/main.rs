use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use formless_core::daemon::{daemon_reachable, Daemon};
use formless_core::provider::probe_blocking;
use formless_core::{Config, Paths};
use formless_protocol::{ApprovalScope, Event, Request, SendMode, ServerMessage, SessionInfo};
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
        /// Task text
        task: String,
        /// Continue an existing session by id (default: ephemeral)
        #[arg(long)]
        session: Option<String>,
        /// Auto-approve sensitive tools for this Run only
        #[arg(long)]
        yes: bool,
        /// Emit one JSON event per line
        #[arg(long)]
        json: bool,
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
    Status,
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
    let paths = Paths::resolve(cli.profile.as_deref())?;
    match cli.cmd {
        None | Some(Cmd::Tui) => formless_tui::run(paths),
        Some(Cmd::Daemon { action }) => daemon_cmd(paths, action),
        Some(Cmd::Doctor) => doctor(paths),
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
            json,
        }) => {
            let code = run_headless(paths, task, session, yes, json)?;
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
        RelayAction::Status => {
            println!("enabled:    {}", cfg.relay.enabled);
            println!("url:        {}", cfg.relay.url);
            match load_relay_secret(&paths) {
                Some(secret) => println!(
                    "browser:    {}",
                    formless_core::relay::browser_url(&cfg.relay, &secret)
                ),
                None => println!("secret:     none yet (run: formless relay enable)"),
            }
            println!(
                "daemon:     {}",
                if daemon_reachable(&paths) {
                    "running"
                } else {
                    "not running"
                }
            );
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
                    }
                    Err(e) => println!("probe:      FAILED: {e}"),
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
fn run_headless(
    paths: Paths,
    task: String,
    session: Option<String>,
    yes: bool,
    json_out: bool,
) -> Result<i32> {
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
    client.send(Request::SendMessage {
        session_id: session_id.clone(),
        content: task,
        mode: SendMode::Send,
    });
    let sigints = count_sigints();
    let mut interrupting = false;
    let mut code = 1;
    loop {
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
                        if !json_out {
                            print!("{delta}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Event::MessageComplete { .. } => {
                        if !json_out {
                            println!();
                        }
                    }
                    Event::ToolCallStarted { tool_name, .. } => {
                        if !json_out {
                            eprintln!("[tool] {tool_name}");
                        }
                    }
                    Event::ApprovalRequested {
                        approval_id,
                        tool_name,
                        ..
                    } => {
                        if !json_out {
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
                    Event::RunPaused { error, .. } => {
                        eprintln!("run paused: {error}");
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

#[cfg(test)]
mod tests {
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
