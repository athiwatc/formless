//! GUI-free controls for daemon-managed resources.
use crate::model::Destination;
use formless_protocol::Request;
use serde_json::Value;

pub const MAX_INPUT_CHARS: usize = 16_384;

#[derive(Debug, Clone)]
pub struct RowAction {
    pub label: &'static str,
    pub confirm: bool,
    request: Request,
}

impl RowAction {
    pub fn request(&self) -> Request {
        self.request.clone()
    }
}

fn action(label: &'static str, confirm: bool, request: Request) -> RowAction {
    RowAction {
        label,
        confirm,
        request,
    }
}

pub fn list_request(destination: Destination) -> Option<Request> {
    Some(match destination {
        Destination::Tools => Request::ListTools,
        Destination::Mcp => Request::ListMcpServers,
        Destination::Skills => Request::ListSkills,
        Destination::Memory => Request::ListMemories,
        Destination::Jobs => Request::ListJobs,
        _ => return None,
    })
}

pub fn page_actions(destination: Destination) -> Vec<RowAction> {
    if destination == Destination::Tools {
        vec![action(
            "Undo last change",
            false,
            Request::UndoRegistryChange,
        )]
    } else {
        vec![]
    }
}

fn input(value: &str, name: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if value.chars().count() > MAX_INPUT_CHARS {
        return Err(format!("{name} is too long"));
    }
    Ok(value.to_owned())
}

pub fn add_memory(text: &str) -> Result<Request, String> {
    Ok(Request::UpsertMemory {
        id: None,
        text: Some(input(text, "Memory")?),
        scope: None,
        enabled: None,
    })
}

pub fn add_skill(source: &str) -> Result<Request, String> {
    Ok(Request::AddSkill {
        source: input(source, "Source")?,
        subdir: None,
        git_ref: None,
    })
}

pub fn add_job(prompt: &str, schedule: &str) -> Result<Request, String> {
    Ok(Request::AddJob {
        prompt: input(prompt, "Prompt")?,
        cron: Some(input(schedule, "Schedule")?),
        session_id: None,
        at: None,
        overlap: None,
        deliver: None,
    })
}

pub(crate) fn projection(
    destination: Destination,
    value: &Value,
) -> Result<(String, Option<String>, Vec<RowAction>), String> {
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
    let key = if matches!(destination, Destination::Mcp | Destination::Skills) {
        "name"
    } else {
        "id"
    };
    let id = text(key);
    if !value.is_object() || id.trim().is_empty() {
        return Err(format!("Missing {key} in {} reply", destination.label()));
    }
    let mut actions = Vec::new();
    let (label, status) = match destination {
        Destination::Tools => {
            let disabled = flag("disabled");
            let pinned = flag("pinned");
            let retired = flag("retired");
            actions.push(action(
                if disabled { "Enable" } else { "Disable" },
                false,
                Request::SetToolDisabled {
                    id: id.clone(),
                    disabled: !disabled,
                },
            ));
            actions.push(action(
                if pinned { "Unpin" } else { "Pin" },
                false,
                Request::SetToolPinned {
                    id: id.clone(),
                    pinned: !pinned,
                },
            ));
            actions.push(if retired {
                action("Restore", false, Request::RestoreTool { id: id.clone() })
            } else {
                action("Retire", true, Request::RetireTool { id: id.clone() })
            });
            let label = [text("title"), text("name"), id]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap();
            (
                label,
                if retired {
                    "retired"
                } else if disabled {
                    "disabled"
                } else if pinned {
                    "pinned"
                } else {
                    ""
                }
                .to_owned(),
            )
        }
        Destination::Mcp => {
            actions.push(action(
                "Restart",
                false,
                Request::RestartMcpServer { name: id.clone() },
            ));
            actions.push(action(
                "Remove",
                true,
                Request::RemoveMcpServer { name: id.clone() },
            ));
            let mut state = text("state");
            if state.is_empty() {
                state = if flag("disabled") {
                    "disabled"
                } else if flag("running") {
                    "ready"
                } else if !text("last_error").is_empty() {
                    "failed"
                } else {
                    "idle"
                }
                .into();
            }
            (
                format!(
                    "{id} · {state} · {} tools",
                    value.get("tool_count").and_then(Value::as_u64).unwrap_or(0)
                ),
                state,
            )
        }
        Destination::Skills => {
            let enabled = flag("enabled");
            actions.push(action(
                if enabled { "Disable" } else { "Enable" },
                false,
                Request::SetSkillEnabled {
                    name: id.clone(),
                    enabled: !enabled,
                },
            ));
            actions.push(action(
                "Remove",
                true,
                Request::RemoveSkill { name: id.clone() },
            ));
            (
                format!(
                    "{id} v{}",
                    value.get("version").and_then(Value::as_i64).unwrap_or(0)
                ),
                if flag("retired") {
                    "retired"
                } else if !enabled {
                    "disabled"
                } else {
                    ""
                }
                .into(),
            )
        }
        Destination::Memory => {
            let enabled = flag("enabled");
            actions.push(action(
                if enabled { "Disable" } else { "Enable" },
                false,
                Request::UpsertMemory {
                    id: Some(id.clone()),
                    text: None,
                    scope: None,
                    enabled: Some(!enabled),
                },
            ));
            actions.push(action("Delete", true, Request::DeleteMemory { id }));
            (text("text"), if enabled { "" } else { "off" }.into())
        }
        Destination::Jobs => {
            let approved = flag("approved");
            let enabled = flag("enabled");
            actions.push(action(
                if !approved {
                    "Approve"
                } else if enabled {
                    "Disable"
                } else {
                    "Enable"
                },
                false,
                Request::SetJobEnabled {
                    id: id.clone(),
                    enabled: !approved || !enabled,
                },
            ));
            actions.push(action(
                "Run now",
                false,
                Request::RunJobNow { id: id.clone() },
            ));
            actions.push(action("Delete", true, Request::DeleteJob { id }));
            (
                text("prompt"),
                if !approved {
                    "needs approval".into()
                } else if !enabled {
                    "off".into()
                } else {
                    text("last_status")
                },
            )
        }
        _ => return Err("Not a manageable destination".into()),
    };
    Ok((
        label.chars().take(crate::pages::PREVIEW_CHARS).collect(),
        (!status.is_empty()).then_some(status),
        actions,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn state_controls_requests_and_destructive_confirmation() {
        let cases = [
            (
                Destination::Tools,
                json!({"id":"x","disabled":true,"pinned":true}),
                vec!["Enable", "Unpin", "Retire"],
            ),
            (
                Destination::Tools,
                json!({"id":"x","retired":true}),
                vec!["Disable", "Pin", "Restore"],
            ),
            (
                Destination::Mcp,
                json!({"name":"x"}),
                vec!["Restart", "Remove"],
            ),
            (
                Destination::Skills,
                json!({"name":"x","enabled":true}),
                vec!["Disable", "Remove"],
            ),
            (
                Destination::Memory,
                json!({"id":"x","enabled":true}),
                vec!["Disable", "Delete"],
            ),
            (
                Destination::Jobs,
                json!({"id":"x","approved":true,"enabled":true}),
                vec!["Disable", "Run now", "Delete"],
            ),
            (
                Destination::Jobs,
                json!({"id":"x","approved":true,"enabled":false}),
                vec!["Enable", "Run now", "Delete"],
            ),
            (
                Destination::Jobs,
                json!({"id":"x","approved":false,"enabled":true}),
                vec!["Approve", "Run now", "Delete"],
            ),
        ];
        for (destination, value, labels) in cases {
            let (_, _, actions) = projection(destination, &value).unwrap();
            assert_eq!(actions.iter().map(|a| a.label).collect::<Vec<_>>(), labels);
            for action in actions {
                assert_eq!(
                    action.confirm,
                    matches!(action.label, "Retire" | "Remove" | "Delete")
                );
                let request = serde_json::to_value(action.request()).unwrap();
                assert_eq!(
                    request.get("id").or_else(|| request.get("name")),
                    Some(&json!("x"))
                );
                match action.request() {
                    Request::SetToolDisabled { disabled, .. } => {
                        assert_eq!(disabled, action.label == "Disable")
                    }
                    Request::SetToolPinned { pinned, .. } => {
                        assert_eq!(pinned, action.label == "Pin")
                    }
                    Request::SetJobEnabled { enabled, .. }
                    | Request::SetSkillEnabled { enabled, .. } => {
                        assert_eq!(enabled, action.label != "Disable")
                    }
                    Request::UpsertMemory {
                        enabled,
                        text,
                        scope,
                        ..
                    } => {
                        assert_eq!(enabled, Some(false));
                        assert!(text.is_none() && scope.is_none());
                    }
                    _ => {}
                }
            }
        }
        assert!(matches!(
            page_actions(Destination::Tools)[0].request(),
            Request::UndoRegistryChange
        ));
        assert_eq!(
            page_actions(Destination::Tools)[0].label,
            "Undo last change"
        );
        assert!(page_actions(Destination::Memory).is_empty());
        assert!(list_request(Destination::Chat).is_none());
    }

    #[test]
    fn add_helpers_trim_validate_and_preserve_plain_english_schedule() {
        assert!(
            matches!(add_memory(" note ").unwrap(), Request::UpsertMemory { id: None, text: Some(t), scope: None, enabled: None } if t == "note")
        );
        assert!(
            matches!(add_skill(" https://example.org/skill ").unwrap(), Request::AddSkill { source, subdir: None, git_ref: None } if source == "https://example.org/skill")
        );
        assert!(
            matches!(add_job(" task ", " every morning ").unwrap(), Request::AddJob { prompt, cron: Some(c), at: None, .. } if prompt == "task" && c == "every morning")
        );
        for invalid in [
            "".to_owned(),
            " \n ".to_owned(),
            "😀".repeat(MAX_INPUT_CHARS + 1),
        ] {
            assert!(add_memory(&invalid).is_err());
            assert!(add_skill(&invalid).is_err());
            assert!(add_job(&invalid, "daily").is_err());
            assert!(add_job("task", &invalid).is_err());
        }
        assert!(add_memory(&"a".repeat(MAX_INPUT_CHARS)).is_ok());
    }
}
