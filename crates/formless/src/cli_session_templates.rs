//! Read-only saved Session templates, distinct from local prompt commands.
use super::*;
use formless_protocol::TemplateInfo;

#[derive(Subcommand)]
pub(super) enum Action {
    /// List saved Session templates in daemon order, without long text bodies
    List {
        /// Print the complete existing array, including null and empty values
        #[arg(long)]
        json: bool,
    },
    /// Show all fields of one exact, case-sensitive stored name
    Show {
        /// Exact, case-sensitive saved Session template name
        name: String,
        /// Print the selected existing response element unchanged
        #[arg(long)]
        json: bool,
    },
}

enum Report<'a> {
    Json(&'a Value),
    List(Table),
    Show(String),
}

pub(super) fn run(paths: &Paths, action: Action) -> Result<()> {
    // sessions_cmd owns daemon startup. There is only one registry read.
    let value = formless_tui::client::request_once(paths, Request::ListSessionTemplates)?;
    match prepare(&value, &action)? {
        Report::Json(value) => println!("{}", serde_json::to_string_pretty(value)?),
        Report::List(table) => {
            if table.is_empty() {
                println!("No saved Session templates.");
            } else {
                table.print();
            }
        }
        Report::Show(text) => print!("{text}"),
    }
    Ok(())
}

fn prepare<'a>(value: &'a Value, action: &Action) -> Result<Report<'a>> {
    let list = value
        .as_array()
        .context("Session template response is not an array")?;
    match action {
        Action::List { json: true } => Ok(Report::Json(value)),
        Action::Show { name, json } => {
            let selected = list
                .iter()
                .find(|entry| entry["name"].as_str() == Some(name))
                .ok_or_else(|| {
                    usage(format!(
                        "no saved Session template '{}'; use formless sessions templates list",
                        comparison_terminal_text(name)
                    ))
                })?;
            if *json {
                Ok(Report::Json(selected))
            } else {
                Ok(Report::Show(describe(&serde_json::from_value::<
                    TemplateInfo,
                >(selected.clone())?)))
            }
        }
        Action::List { json: false } => {
            let templates: Vec<TemplateInfo> = serde_json::from_value(value.clone())?;
            let mut table = Table::new(&[
                "NAME",
                "MODEL",
                "EFFORT",
                "CWD",
                "INSTRUCTIONS",
                "FIRST MESSAGE",
            ]);
            for template in templates {
                table.row(vec![
                    c(template.name),
                    c(optional(&template.model)),
                    c(optional(&template.reasoning_effort)),
                    c(optional(&template.cwd)),
                    c(presence(&template.instructions)),
                    c(presence(&template.first_message)),
                ]);
            }
            Ok(Report::List(table))
        }
    }
}
fn optional(value: &Option<String>) -> String {
    match value {
        None => "(unset)".into(),
        Some(value) if value.is_empty() => "(empty)".into(),
        Some(value) => comparison_terminal_text(value),
    }
}
fn presence(value: &Option<String>) -> &'static str {
    match value {
        None => "unset",
        Some(value) if value.is_empty() => "empty",
        Some(_) => "present",
    }
}
fn describe(template: &TemplateInfo) -> String {
    use std::fmt::Write as _;
    let mut out = format!(
        "Name: {}\nModel: {}\nEffort: {}\nCwd: {}\n",
        comparison_terminal_text(&template.name),
        optional(&template.model),
        optional(&template.reasoning_effort),
        optional(&template.cwd)
    );
    for (label, value) in [
        ("Instructions", &template.instructions),
        ("First message", &template.first_message),
    ] {
        match value {
            None => {
                let _ = writeln!(out, "{label}: (unset)");
            }
            Some(value) if value.is_empty() => {
                let _ = writeln!(out, "{label}: (empty)");
            }
            Some(value) => {
                let _ = writeln!(out, "{label}:");
                // Keep all lines and Unicode; make terminal controls visible.
                let text = value
                    .split('\n')
                    .map(comparison_terminal_text)
                    .collect::<Vec<_>>()
                    .join("\n");
                let _ = writeln!(out, "{text}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn template_json_and_exact_name_selection_preserve_original_values_and_order() {
        let value = json!([{"name":"work","model":null,"instructions":"","future":true},{"name":"Work","model":"","instructions":null}]);
        let Report::Json(list) = prepare(&value, &Action::List { json: true }).unwrap() else {
            panic!("wrong report")
        };
        assert_eq!(list, &value);
        for (name, index) in [("work", 0), ("Work", 1)] {
            let Report::Json(selected) = prepare(
                &value,
                &Action::Show {
                    name: name.into(),
                    json: true,
                },
            )
            .unwrap() else {
                panic!("wrong report")
            };
            assert_eq!(selected, &value[index]);
        }
        for name in ["WORK", "wor", "missing"] {
            let error = prepare(
                &value,
                &Action::Show {
                    name: name.into(),
                    json: false,
                },
            )
            .err()
            .unwrap();
            assert!(error.to_string().contains("sessions templates list"));
        }
    }
    #[test]
    fn template_human_views_keep_order_presence_empty_and_complete_long_bodies() {
        let body = format!("start ไทย\n{}\nTAIL 日本語", "x".repeat(20000));
        let value = json!([{"name":"second","model":"","instructions":body,"first_message":""},{"name":"first","model":null,"instructions":"","first_message":null}]);
        let Report::List(table) = prepare(&value, &Action::List { json: false }).unwrap() else {
            panic!("wrong report")
        };
        let list = table.render(false, false, None);
        assert!(list.find("second").unwrap() < list.find("first").unwrap());
        assert!(!list.contains("TAIL"));
        assert!(list.contains("present") && list.contains("empty") && list.contains("unset"));
        let Report::Show(show) = prepare(
            &value,
            &Action::Show {
                name: "second".into(),
                json: false,
            },
        )
        .unwrap() else {
            panic!("wrong report")
        };
        assert!(show.contains(&body));
        assert!(
            show.contains("Model: (empty)")
                && show.contains("First message: (empty)")
                && show.contains("Effort: (unset)")
        );
        let empty = json!([]);
        let Report::List(table) = prepare(&empty, &Action::List { json: false }).unwrap() else {
            panic!("wrong report")
        };
        assert!(table.is_empty());
    }
}
