use super::*;
use formless_protocol::{HarnessKind, MainChatBackend, MainChatBackends};

#[derive(Subcommand)]
pub(super) enum Action {
    /// Show or set the Main Chat provider or local harness
    Backend {
        #[command(subcommand)]
        action: Option<BackendAction>,
    },
}

#[derive(Subcommand)]
pub(super) enum BackendAction {
    /// Show the selected backend and the daemon's providers and harnesses
    Show {
        /// Print the backend and inventory as JSON
        #[arg(long)]
        json: bool,
    },
    /// Save the backend for future Main Chat messages
    Set {
        #[command(subcommand)]
        backend: Backend,
    },
}

#[derive(Subcommand)]
pub(super) enum Backend {
    /// Use the existing session model and profile defaults
    Inherit,
    /// Use a configured provider (default or a named provider)
    Provider {
        /// Configured provider name, or default
        name: String,
        /// Override the configured provider's model
        #[arg(long)]
        model: Option<String>,
        /// Override the provider's reasoning effort
        #[arg(long)]
        effort: Option<String>,
    },
    /// Use a supported local harness on the daemon's machine
    Harness {
        /// Harness kind from the daemon's inventory
        #[arg(value_parser = ["claude", "codex", "pi", "opencode", "antigravity", "acp", "muse"])]
        kind: String,
        /// Agent name from the daemon's ACP inventory
        #[arg(long, required_if_eq("kind", "acp"))]
        acp_agent: Option<String>,
    },
}

pub(super) fn run(paths: &Paths, Action::Backend { action }: Action) -> Result<()> {
    formless_core::daemon::ensure_daemon(paths)?;
    let request = |request| formless_tui::client::request_once(paths, request);
    match action.unwrap_or(BackendAction::Show { json: false }) {
        BackendAction::Show { json } => {
            let catalog: MainChatBackends =
                serde_json::from_value(request(Request::ListMainChatBackends)?)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&catalog)?);
            } else {
                println!(
                    "Main Chat: {}",
                    comparison_terminal_text(&catalog.selected.label())
                );
                for (backend, detail) in catalog.choices() {
                    println!(
                        "  {} — {}",
                        comparison_terminal_text(&backend.label()),
                        comparison_terminal_text(&detail)
                    );
                }
            }
        }
        BackendAction::Set { backend } => {
            let backend = match backend {
                Backend::Inherit => MainChatBackend::Inherit,
                Backend::Provider {
                    name,
                    model,
                    effort,
                } => MainChatBackend::Provider {
                    name,
                    model,
                    effort,
                },
                Backend::Harness { kind, acp_agent } => {
                    let harness: HarnessKind = serde_json::from_value(Value::String(kind))?;
                    MainChatBackend::Harness { harness, acp_agent }
                }
            };
            let selected: MainChatBackend =
                serde_json::from_value(request(Request::SetMainChatBackend { backend })?)?;
            println!("Main Chat: {}", comparison_terminal_text(&selected.label()));
        }
    }
    Ok(())
}
