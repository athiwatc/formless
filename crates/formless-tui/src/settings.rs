//! The interactive Settings view (SPEC §10.4): selectable rows that save
//! through `SetSetting`, plus forms for providers and Roles.

use crate::input::InputLine;
use formless_core::Config;
use formless_protocol::Request;
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// Shown only (endpoint, relay, paths: file-only or CLI).
    ReadOnly(&'static str),
    Bool,
    /// Cycled with Enter; "" stands for unset (the default).
    Choice(&'static [&'static str]),
    Number,
    /// Opens the model picker for the global default.
    Model,
    /// The TUI's own thinking display (not a config key).
    Thinking,
    Provider(String),
    Role(String),
    AddProvider,
    AddRole,
    OpenEditor,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub section: &'static str,
    /// The `SetSetting` key (or a label for non-setting rows).
    pub key: String,
    pub label: String,
    pub value: String,
    pub kind: Kind,
}

impl Row {
    fn new(section: &'static str, key: &str, label: &str, value: String, kind: Kind) -> Self {
        Row {
            section,
            key: key.into(),
            label: label.into(),
            value,
            kind,
        }
    }

    pub fn hint(&self) -> &'static str {
        match &self.kind {
            Kind::ReadOnly(why) => why,
            Kind::Bool | Kind::Thinking => "Enter toggles",
            Kind::Choice(_) => "Enter cycles",
            Kind::Number => "Enter edits",
            Kind::Model => "Enter picks",
            Kind::Provider(_) | Kind::Role(_) => "Enter edits · d removes",
            Kind::AddProvider | Kind::AddRole | Kind::OpenEditor => "Enter",
        }
    }
}

const EFFORTS: &[&str] = &["", "low", "medium", "high"];

fn on(b: bool) -> String {
    if b { "on" } else { "off" }.into()
}

/// The rows for `cfg`, with providers and Roles from a `ListProviders`
/// result.
pub fn rows(
    cfg: &Config,
    providers: &Value,
    show_thinking: bool,
    paths: &formless_core::Paths,
) -> Vec<Row> {
    use Kind::*;
    let p = &cfg.provider;
    let mut out = vec![
        Row::new(
            "Provider",
            "provider.base_url",
            "base_url",
            p.base_url.clone(),
            ReadOnly("the endpoint is file-only: re-run setup or edit config.toml"),
        ),
        Row::new(
            "Provider",
            "provider.model",
            "model",
            p.model.clone(),
            Model,
        ),
        Row::new(
            "Provider",
            "provider.reasoning_effort",
            "reasoning_effort",
            p.reasoning_effort.clone().unwrap_or_default(),
            Choice(EFFORTS),
        ),
        Row::new(
            "Provider",
            "provider.api_style",
            "api_style",
            p.api_style.clone(),
            Choice(&["chat", "responses"]),
        ),
        Row::new(
            "Provider",
            "provider.tool_mode",
            "tool_mode",
            p.tool_mode.clone(),
            Choice(&["native", "prompt"]),
        ),
        Row::new(
            "Provider",
            "provider.context_tokens",
            "context_tokens",
            p.context_tokens.to_string(),
            Number,
        ),
        Row::new(
            "Provider",
            "provider.max_retries",
            "max_retries",
            p.max_retries.to_string(),
            Number,
        ),
        Row::new(
            "Provider",
            "provider.stall_notice_secs",
            "stall_notice_secs",
            p.stall_notice_secs.to_string(),
            Number,
        ),
        Row::new(
            "Provider",
            "provider.tool_use_nudge",
            "tool_use_nudge",
            on(p.tool_use_nudge),
            Bool,
        ),
        Row::new(
            "Provider",
            "provider.auth",
            "auth",
            p.auth.mode.clone(),
            ReadOnly("change with `formless auth`"),
        ),
        Row::new(
            "Interface",
            "tui.multi_pane",
            "multi_pane",
            on(cfg.tui.multi_pane),
            Bool,
        ),
        Row::new(
            "Interface",
            "thinking",
            "thinking",
            on(show_thinking),
            Thinking,
        ),
        Row::new(
            "Interface",
            "tui.timestamps",
            "timestamps",
            on(cfg.tui.timestamps),
            Bool,
        ),
        Row::new(
            "Interface",
            "tui.notify",
            "notify",
            cfg.tui.notify.clone(),
            Choice(&["off", "bell", "desktop"]),
        ),
        Row::new(
            "Interface",
            "tui.notify_min_secs",
            "notify_min_secs",
            cfg.tui.notify_min_secs.to_string(),
            Number,
        ),
        Row::new(
            "Naming",
            "naming.enabled",
            "auto-naming",
            on(cfg.naming.enabled),
            Bool,
        ),
        Row::new(
            "Daemon",
            "daemon.idle_grace_secs",
            "idle_grace_secs",
            cfg.daemon.idle_grace_secs.to_string(),
            Number,
        ),
        Row::new(
            "Relay",
            "relay.enabled",
            "enabled",
            on(cfg.relay.enabled),
            ReadOnly("change with `formless relay enable|disable`"),
        ),
        Row::new(
            "Relay",
            "relay.url",
            "url",
            cfg.relay.url.clone(),
            ReadOnly("edit config.toml"),
        ),
    ];
    for prov in providers["providers"].as_array().into_iter().flatten() {
        let name = prov["name"].as_str().unwrap_or_default();
        if name == formless_core::config::DEFAULT_PROVIDER {
            continue;
        }
        let model = prov["model"]
            .as_str()
            .filter(|m| !m.is_empty())
            .unwrap_or("default model");
        let key = if prov["has_key"] == true {
            ""
        } else {
            " · no key"
        };
        out.push(Row::new(
            "Providers",
            name,
            name,
            format!(
                "{} · {model}{key}",
                prov["base_url"].as_str().unwrap_or_default()
            ),
            Provider(name.into()),
        ));
    }
    out.push(Row::new(
        "Providers",
        "+provider",
        "+ add provider",
        String::new(),
        AddProvider,
    ));
    for (name, r) in providers["roles"].as_object().into_iter().flatten() {
        let model = r["model"]
            .as_str()
            .filter(|m| !m.is_empty())
            .unwrap_or("default model");
        out.push(Row::new(
            "Roles",
            name,
            name,
            format!(
                "{} · {model} · {}",
                r["provider"].as_str().unwrap_or("default"),
                r["description"].as_str().unwrap_or_default()
            ),
            Role(name.into()),
        ));
    }
    out.push(Row::new(
        "Roles",
        "+role",
        "+ add role",
        String::new(),
        AddRole,
    ));
    out.push(Row::new(
        "Files",
        "config",
        "config.toml",
        paths.config_file().display().to_string(),
        OpenEditor,
    ));
    out.push(Row::new(
        "Files",
        "data",
        "data",
        paths.data_dir.display().to_string(),
        ReadOnly(""),
    ));
    out
}

/// The value after `current` in `choices`, as JSON (null for "").
pub fn next_choice(choices: &[&str], current: &str) -> Value {
    let i = choices
        .iter()
        .position(|c| *c == current)
        .map_or(0, |i| i + 1);
    match choices[i % choices.len()] {
        "" => Value::Null,
        v => json!(v),
    }
}

/// Validate a typed number for `key`.
pub fn parse_number(text: &str) -> Result<Value, String> {
    text.trim()
        .parse::<u64>()
        .map(|n| json!(n))
        .map_err(|_| format!("'{}' is not a whole number", text.trim()))
}

pub struct Field {
    pub label: &'static str,
    pub input: InputLine,
    pub masked: bool,
    pub hint: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormKind {
    Provider,
    Role,
}

/// Add or edit a provider or Role.
pub struct Form {
    pub kind: FormKind,
    pub title: String,
    pub fields: Vec<Field>,
    pub idx: usize,
    pub error: Option<String>,
}

fn field(label: &'static str, value: &str, masked: bool, hint: &'static str) -> Field {
    Field {
        label,
        input: InputLine::from(value),
        masked,
        hint,
    }
}

impl Form {
    pub fn provider(existing: Option<&Value>) -> Self {
        let get = |k: &str| {
            existing
                .and_then(|p| p[k].as_str())
                .unwrap_or_default()
                .to_string()
        };
        Form {
            kind: FormKind::Provider,
            title: if existing.is_some() {
                format!("Edit provider {}", get("name"))
            } else {
                "Add provider".into()
            },
            fields: vec![
                field("name", &get("name"), false, "letters, digits, - and _"),
                field(
                    "base_url",
                    &get("base_url"),
                    false,
                    "a URL, or a preset: openai, anthropic, openrouter, gemini, groq, ollama, lmstudio",
                ),
                field("model", &get("model"), false, "optional: its default model"),
                field(
                    "api_key",
                    "",
                    true,
                    if existing.is_some() {
                        "blank keeps the stored key"
                    } else {
                        "stored in the keychain"
                    },
                ),
            ],
            idx: if existing.is_some() { 1 } else { 0 },
            error: None,
        }
    }

    pub fn role(name: Option<&str>, existing: Option<&Value>) -> Self {
        let get = |k: &str| {
            existing
                .and_then(|r| r[k].as_str())
                .unwrap_or_default()
                .to_string()
        };
        Form {
            kind: FormKind::Role,
            title: match name {
                Some(n) => format!("Edit role {n}"),
                None => "Add role".into(),
            },
            fields: vec![
                field("name", name.unwrap_or_default(), false, "e.g. explorer"),
                field("provider", &get("provider"), false, "blank = default"),
                field(
                    "model",
                    &get("model"),
                    false,
                    "blank = the provider's default",
                ),
                field(
                    "description",
                    &get("description"),
                    false,
                    "when to use it (the agent reads this)",
                ),
            ],
            idx: if name.is_some() { 1 } else { 0 },
            error: None,
        }
    }

    fn value(&self, i: usize) -> String {
        self.fields[i].input.trim().to_string()
    }

    /// The request this form saves with, or why it can't yet.
    pub fn request(&self) -> Result<Request, String> {
        let name = self.value(0);
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("name: use letters, digits, '-' and '_'".into());
        }
        match self.kind {
            FormKind::Provider => {
                let Some(base_url) = formless_core::presets::resolve_base_url(&self.value(1))
                else {
                    return Err(
                        "base_url must start with http:// or https://, or name a preset".into(),
                    );
                };
                Ok(Request::UpsertProvider {
                    name,
                    base_url,
                    model: Some(self.value(2)).filter(|m| !m.is_empty()),
                    api_key: Some(self.fields[3].input.to_string()).filter(|k| !k.is_empty()),
                })
            }
            FormKind::Role => {
                let description = self.value(3);
                if description.is_empty() {
                    return Err("description: say when the agent should use this role".into());
                }
                let provider = self.value(1);
                Ok(Request::UpsertRole {
                    name,
                    role: json!({
                        "provider": if provider.is_empty() { "default".into() } else { provider },
                        "model": self.value(2),
                        "description": description,
                    }),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_cycle_through_unset() {
        assert_eq!(next_choice(EFFORTS, ""), json!("low"));
        assert_eq!(next_choice(EFFORTS, "high"), Value::Null);
        assert_eq!(
            next_choice(&["chat", "responses"], "responses"),
            json!("chat")
        );
        assert_eq!(next_choice(&["chat", "responses"], "weird"), json!("chat"));
        assert!(parse_number(" 42 ").is_ok());
        assert!(parse_number("4k").unwrap_err().contains("whole number"));
    }

    #[test]
    fn rows_list_settings_providers_and_roles() {
        let paths = formless_core::Paths {
            data_dir: "/d".into(),
            config_dir: "/c".into(),
            profile: "p".into(),
        };
        let providers = json!({
            "providers": [{"name": "default", "base_url": "x"},
                          {"name": "local", "base_url": "http://l/v1", "model": "", "has_key": false}],
            "roles": {"explorer": {"provider": "local", "model": "small", "description": "cheap"}},
        });
        let rows = rows(&Config::default(), &providers, true, &paths);
        let find = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
        assert_eq!(find("provider.model").kind, Kind::Model);
        assert_eq!(find("local").value, "http://l/v1 · default model · no key");
        assert_eq!(find("explorer").kind, Kind::Role("explorer".into()));
        assert!(rows.iter().all(|r| r.key != "default"));
        assert_eq!(find("thinking").value, "on");
    }

    #[test]
    fn forms_validate_and_build_requests() {
        let mut f = Form::provider(None);
        assert!(f.request().unwrap_err().starts_with("name"));
        f.fields[0].input.set("local");
        f.fields[1].input.set("localhost:11434");
        assert!(f.request().unwrap_err().contains("http://"));
        f.fields[1].input.set("ollama");
        f.fields[3].input.set("sk-1");
        match f.request().unwrap() {
            Request::UpsertProvider {
                name,
                model,
                api_key,
                ..
            } => {
                assert_eq!(
                    (name.as_str(), model, api_key.as_deref()),
                    ("local", None, Some("sk-1"))
                );
            }
            other => panic!("{other:?}"),
        }
        let existing = json!({"provider": "local", "model": "", "description": "cheap"});
        let r = Form::role(Some("explorer"), Some(&existing));
        assert_eq!(r.idx, 1);
        match r.request().unwrap() {
            Request::UpsertRole { name, role } => {
                assert_eq!(name, "explorer");
                assert_eq!(role["provider"], "local");
            }
            other => panic!("{other:?}"),
        }
    }
}
