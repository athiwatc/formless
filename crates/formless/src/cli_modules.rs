//! Read-only wrappers over the existing all-version module reports.
use super::*;
use formless_protocol::{ModuleDetail, ModuleInfo};

#[derive(Subcommand)]
pub(super) enum Action {
    /// List all modules, including retired modules
    List {
        /// Print the complete module array as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect a module by exact name or full ID
    Show {
        /// Module name or full ID, resolved by the daemon
        name: String,
        /// Print the complete metadata and source history as JSON
        #[arg(long, conflicts_with = "version")]
        json: bool,
        /// Write exactly this stored version's source, with no heading
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        version: Option<i64>,
    },
}

pub(super) fn run(paths: &Paths, action: Action) -> Result<()> {
    formless_core::daemon::ensure_daemon(paths)?;
    let (request, json, version) = match action {
        Action::List { json } => (Request::ListModules, json, None),
        Action::Show {
            name,
            json,
            version,
        } => (Request::GetModule { id: name }, json, version),
    };
    let list = matches!(&request, Request::ListModules);
    let value = formless_tui::client::request_once(paths, request)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if list {
        let modules: Vec<ModuleInfo> = serde_json::from_value(value)?;
        let mut table = Table::new(&["NAME", "VER", "STATE", "UPDATED", "DESCRIPTION"]);
        for module in modules {
            table.row(vec![
                c(module.name),
                c(module.current_version.to_string()),
                t(
                    if module.retired { "retired" } else { "active" },
                    if module.retired {
                        Tone::Dim
                    } else {
                        Tone::Plain
                    },
                ),
                c(module.updated_at),
                t(module.description, Tone::Dim),
            ]);
        }
        if table.is_empty() {
            println!("No modules.");
        } else {
            table.print();
        }
    } else {
        let detail: ModuleDetail = serde_json::from_value(value)?;
        if let Some(version) = version {
            let code = version_source(&detail, version)?;
            std::io::stdout().write_all(code.as_bytes())?;
        } else {
            print!("{}", describe(&detail));
        }
    }
    Ok(())
}

fn version_source(detail: &ModuleDetail, version: i64) -> Result<&str> {
    detail
        .versions
        .iter()
        .find(|entry| entry.version == version)
        .map(|entry| entry.code.as_str())
        .ok_or_else(|| usage(format!("module has no stored version {version}")))
}

fn describe(detail: &ModuleDetail) -> String {
    use std::fmt::Write;
    let module = &detail.module;
    let mut out=format!("Module: {}\nID: {}\nCurrent version: {}\nState: {}\nDescription: {}\nCreated: {}\nUpdated: {}\nVersions (oldest first):\n",comparison_terminal_text(&module.name),comparison_terminal_text(&module.id),module.current_version,if module.retired {"retired"} else {"active"},comparison_terminal_text(&module.description),comparison_terminal_text(&module.created_at),comparison_terminal_text(&module.updated_at));
    if detail.versions.is_empty() {
        out.push_str("  (none)\n");
    }
    for version in &detail.versions {
        writeln!(
            out,
            "  v{} · {} · {}",
            version.version,
            comparison_terminal_text(&version.author),
            comparison_terminal_text(&version.created_at)
        )
        .unwrap();
    }
    out.push_str("Used by active Tools:\n");
    if detail.used_by.is_empty() {
        out.push_str("  (none)\n");
    }
    for tool in &detail.used_by {
        writeln!(out, "  {}", comparison_terminal_text(tool)).unwrap();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn module_flags_reject_conflicts_and_invalid_versions_during_parsing() {
        for args in [
            vec!["formless", "modules", "list", "--json"],
            vec!["formless", "modules", "show", "日本語", "--json"],
            vec!["formless", "modules", "show", "module-id", "--version", "2"],
        ] {
            Cli::try_parse_from(args).unwrap();
        }
        for args in [
            vec![
                "formless",
                "modules",
                "show",
                "name",
                "--json",
                "--version",
                "1",
            ],
            vec!["formless", "modules", "show", "name", "--version", "0"],
            vec!["formless", "modules", "show", "name", "--version", "-1"],
            vec![
                "formless",
                "modules",
                "show",
                "name",
                "--version",
                "overflow",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}
