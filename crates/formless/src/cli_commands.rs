//! Metadata-only discovery shares the exact Core projection used by the TUI.
//! Deliberately handled before profile creation, locks, config or daemon setup.
use super::*;

#[derive(Subcommand)]
pub(super) enum Action {
    /// List config/commands and this project's .formless/commands templates
    List {
        /// Metadata, diagnostics and shadowed files as one JSON object
        #[arg(long)]
        json: bool,
    },
}

pub(super) fn run(paths: &Paths, action: &Action) -> Result<()> {
    let cwd = std::env::current_dir().context("resolve project directory for user commands")?;
    let snapshot = formless_core::user_commands::load(
        &paths.config_dir,
        Some(&cwd),
        &formless_tui::builtin_command_names(),
    );
    let projection = snapshot.projection();
    match action {
        Action::List { json: true } => {
            println!("{}", serde_json::to_string_pretty(&projection)?);
        }
        Action::List { json: false } => {
            let mut table = Table::new(&["COMMAND", "DESCRIPTION", "SOURCE", "FILE"]);
            for command in &projection.commands {
                table.row(vec![
                    t(
                        comparison_terminal_text(&format!("/{}", command.name)),
                        Tone::Strong,
                    ),
                    c(comparison_terminal_text(&command.description)),
                    c(serde_json::to_value(&command.source)?
                        .as_str()
                        .unwrap_or_default()),
                    c(comparison_terminal_text(
                        &command.path.display().to_string(),
                    )),
                ]);
            }
            table.print();
            if projection.commands.is_empty() {
                println!("No user commands found in config/commands or .formless/commands.");
            }
            for diagnostic in &projection.diagnostics {
                println!(
                    "{}: {}",
                    comparison_terminal_text(&diagnostic.path.display().to_string()),
                    comparison_terminal_text(&diagnostic.message)
                );
            }
            for shadowed in &projection.shadowed {
                println!(
                    "/{}: {} ({})",
                    comparison_terminal_text(&shadowed.name),
                    comparison_terminal_text(&shadowed.reason),
                    comparison_terminal_text(&shadowed.path.display().to_string())
                );
            }
        }
    }
    Ok(())
}
