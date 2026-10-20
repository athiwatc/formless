use anyhow::Result;
use clap::{Parser, Subcommand};
use formless_core::daemon::{daemon_reachable, Daemon};
use formless_core::provider::probe_blocking;
use formless_core::{Config, Paths};
use formless_protocol::{Event, Request, SendMode, ServerMessage, SessionInfo};
use std::io::Write;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "formless", about = "General-purpose agent harness (SPEC.md)")]
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
    /// Tool registry inspection/transfer (SPEC §7.8, §12)
    Tools {
        #[command(subcommand)]
        action: ToolsAction,
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
        Some(Cmd::Tools { action }) => tools_cmd(paths, action),
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

fn doctor(paths: Paths) -> Result<()> {
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
    let key = formless_core::config::load_api_key(&paths);
    println!(
        "api key:    {}",
        if key.is_some() { "present" } else { "MISSING" }
    );
    println!(
        "daemon:     {}",
        if daemon_reachable(&paths) {
            "running"
        } else {
            "not running"
        }
    );
    if cfg.provider.is_configured() {
        match probe_blocking(&cfg.provider, &key.unwrap_or_default(), &cfg.provider.model) {
            Ok(r) => {
                println!(
                    "probe:      models={} streaming={} tool_calling={}",
                    r.models.len(),
                    r.streaming,
                    r.tool_calling
                );
                if !r.tool_calling {
                    println!("WARNING: endpoint/model does not support tool-calling; sessions will be refused (SPEC §4.3).");
                }
            }
            Err(e) => println!("probe:      FAILED: {e}"),
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
    let mut code = 1;
    loop {
        match rx.recv() {
            Ok(ServerMessage::Event(ev)) => {
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
                                    "approved (--yes)"
                                } else {
                                    "DENIED (fail closed; pass --yes or approve in the TUI)"
                                }
                            );
                        }
                        client.send(Request::ApprovalDecision {
                            approval_id,
                            approved: yes,
                        });
                    }
                    Event::RunPaused { error, .. } => {
                        eprintln!("run paused: {error}");
                        code = 1;
                        break;
                    }
                    Event::RunFinished {
                        status,
                        session_id: sid,
                        ..
                    } if sid == session_id => {
                        code = if status == "completed" { 0 } else { 1 };
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
                    if t.needs_attention {
                        " NEEDS-ATTENTION"
                    } else if t.retired {
                        " retired"
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
