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

/// The approvals row: the running daemon's mode, and the remedy when the
/// settings here say otherwise.
pub fn approvals_text(running: bool, configured: bool) -> String {
    let mode = if running {
        "on: sensitive calls ask"
    } else {
        "off: Tools run without asking"
    };
    if running == configured {
        mode.to_string()
    } else {
        format!(
            "{mode} (the running daemon; config says {}: /restart-daemon)",
            if configured { "on" } else { "off" }
        )
    }
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
/// An optional dollar limit: "off" when unset.
fn limit(v: Option<f64>) -> String {
    v.map_or("off".into(), |v| format!("{v}"))
}

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
            Choice(&["chat", "responses", "anthropic"]),
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
            "tui.vim_mode",
            "vim_mode",
            on(cfg.tui.vim_mode),
            Bool,
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
            "tui.theme",
            "theme",
            cfg.tui.theme.clone(),
            Choice(&["auto", "dark", "light", "high-contrast"]),
        ),
        Row::new(
            "Interface",
            "tui.inline_images",
            "inline_images",
            cfg.tui.inline_images.clone(),
            Choice(&["auto", "off"]),
        ),
        Row::new(
            "Interface",
            "tui.code_line_numbers",
            "code_line_numbers",
            on(cfg.tui.code_line_numbers),
            Bool,
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
            "tui.language",
            "language",
            cfg.tui.language.clone(),
            Choice(crate::i18n::LANGUAGES),
        ),
        Row::new(
            "Interface",
            "tui.animations",
            "animations",
            on(cfg.tui.animations),
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
            "Limits",
            "limits.daily_usd",
            "daily_usd",
            limit(cfg.limits.daily_usd),
            Number,
        ),
        Row::new(
            "Limits",
            "limits.run_usd",
            "run_usd",
            limit(cfg.limits.run_usd),
            Number,
        ),
        Row::new(
            "Limits",
            "limits.forecast_confirm_usd",
            "forecast_confirm_usd",
            limit(cfg.limits.forecast_confirm_usd),
            Number,
        ),
        Row::new(
            "Limits",
            "limits.run_steps",
            "run_steps",
            cfg.limits.run_steps.map_or("off".into(), |n| n.to_string()),
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
        Row::new(
            "Security",
            "security.secure",
            "approvals",
            if cfg.security.secure {
                "on: sensitive calls ask".to_string()
            } else {
                "off: Tools run without asking".to_string()
            },
            ReadOnly("set security.secure in config.toml, or start with `formless --secure`"),
        ),
        Row::new(
            "Security",
            "security.trusted_paths",
            "trusted folders",
            cfg.security.roots().len().to_string(),
            ReadOnly("edit config.toml; /security lists them"),
        ),
        Row::new(
            "Security",
            "security.trusted_hosts",
            "trusted hosts",
            cfg.security.trusted_hosts.len().to_string(),
            ReadOnly("edit config.toml; /security lists them"),
        ),
    ];
    let warnings = cfg.security.network_scope_warnings();
    if !warnings.is_empty() {
        out.push(Row::new(
            "Security",
            "security.network_scope_warnings",
            "⚠ network scopes",
            format!("{} warning(s); /security shows all", warnings.len()),
            ReadOnly("/security shows public-suffix and wildcard scope warnings"),
        ));
    }
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
/// A whole number; for optional limits, a decimal too, and empty or
/// "off" clears it (null).
pub fn parse_number(text: &str) -> Result<Value, String> {
    let t = text.trim();
    if t.is_empty() || t == "off" {
        return Ok(Value::Null);
    }
    if let Ok(n) = t.parse::<u64>() {
        return Ok(json!(n));
    }
    t.trim_start_matches('$')
        .parse::<f64>()
        .ok()
        .filter(|f| f.is_finite() && *f >= 0.0)
        .map(|f| json!(f))
        .ok_or_else(|| format!("'{t}' is not a number"))
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
    McpCatalog,
}

/// Add or edit a provider or Role.
pub struct Form {
    pub kind: FormKind,
    catalog: Option<&'static formless_core::mcp_catalog::Entry>,
    pub title: String,
    pub review: Vec<String>,
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

/// `Key=Value, Key2=Value2` → a map; blank → None (keep what is set).
fn pairs(text: &str) -> Result<Option<std::collections::BTreeMap<String, String>>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
                .ok_or_else(|| format!("'{p}': write key=value"))
        })
        .collect::<Result<_, _>>()
        .map(Some)
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
            catalog: None,
            review: Vec::new(),
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
                field(
                    "headers",
                    "",
                    true,
                    "advanced: extra headers, Key=Value, … (blank keeps the current ones)",
                ),
                field(
                    "query",
                    &existing
                        .and_then(|p| p["query_params"].as_object())
                        .map(|q| {
                            q.iter()
                                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default(),
                    false,
                    "advanced: query parameters, key=value, … (Azure: api-version=…)",
                ),
                field(
                    "key_header",
                    &get("api_key_header"),
                    false,
                    "advanced: send the key in this header instead of Bearer (Azure: api-key)",
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
            catalog: None,
            review: Vec::new(),
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

    pub fn mcp(
        entry: &'static formless_core::mcp_catalog::Entry,
        params: &serde_json::Map<String, Value>,
    ) -> Self {
        use formless_core::mcp_catalog::{target, ParamKind, CODE_WARNING};
        let mut fields = vec![field(
            "Server name",
            entry.id,
            false,
            "A local name for this server",
        )];
        for p in entry.params {
            let secret = p.kind == ParamKind::Secret;
            fields.push(field(
                p.name,
                if secret {
                    ""
                } else {
                    params.get(p.name).and_then(Value::as_str).unwrap_or("")
                },
                secret,
                p.description,
            ));
        }
        fields.push(field(
            "Confirm",
            "",
            false,
            "Type ADD to run; broad folders require EXPOSE followed by the canonical path",
        ));
        Form {
            kind: FormKind::McpCatalog,
            catalog: Some(entry),
            title: format!("Add MCP: {}", entry.title),
            review: vec![
                target(entry),
                if entry.command.is_empty() {
                    "Connects to this URL.".into()
                } else {
                    CODE_WARNING.into()
                },
            ],
            fields,
            idx: 1,
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
                    headers: pairs(&self.fields[4].input)?,
                    query_params: pairs(&self.value(5))?.or_else(|| Some(Default::default())),
                    api_key_header: Some(self.value(6)),
                })
            }
            FormKind::McpCatalog => {
                let entry = self.catalog.ok_or("missing catalog entry")?;
                let mut params = serde_json::Map::new();
                for (i, p) in entry.params.iter().enumerate() {
                    params.insert(p.name.into(), json!(self.value(i + 1)));
                }
                let preview = formless_core::mcp_catalog::catalog_preview(entry, &params)
                    .map_err(|e| e.to_string())?;
                let confirmation = self.value(self.fields.len() - 1);
                if !preview["warnings"].as_array().unwrap().is_empty() {
                    let path = formless_core::mcp_catalog::normalize_directory(
                        params["path"].as_str().unwrap_or(""),
                    )
                    .map_err(|e| e.to_string())?;
                    let expected = format!("EXPOSE {}", path.display());
                    if confirmation != expected {
                        return Err(format!(
                            "{}; type {expected} in Confirm",
                            preview["warnings"][0].as_str().unwrap_or_default()
                        ));
                    }
                } else if confirmation != "ADD" {
                    return Err(format!(
                        "Review {} then type ADD in Confirm",
                        preview["target"].as_str().unwrap_or_default()
                    ));
                }
                params.insert("confirm_run".into(), json!(true));
                params.insert("confirm_exposure".into(), json!(true));
                Ok(Request::AddMcpFromCatalog {
                    id: entry.id.into(),
                    name: Some(name),
                    params,
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
        assert!(parse_number("4k").unwrap_err().contains("not a number"));
        assert_eq!(parse_number("$2.50").unwrap(), json!(2.5));
        assert_eq!(parse_number("off").unwrap(), Value::Null);
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

#[cfg(test)]
mod approvals_row_tests {
    #[test]
    fn the_row_shows_the_daemons_mode_and_a_mismatch() {
        assert_eq!(super::approvals_text(true, true), "on: sensitive calls ask");
        let t = super::approvals_text(false, true);
        assert!(t.starts_with("off") && t.contains("/restart-daemon"), "{t}");
    }
}

#[cfg(test)]
mod mcp_review_tests {
    use super::*;

    #[test]
    fn credential_url_uses_a_masked_form_and_execution_needs_confirmation() {
        let entry = formless_core::mcp_catalog::find("postgres").unwrap();
        let mut form = Form::mcp(entry, &Default::default());
        assert!(form.fields[1].masked);
        form.fields[1].input = InputLine::from("postgresql://u:pw@h/db");
        assert!(form.request().is_err());
        assert!(form.review.join(" ").contains("server-postgres@0.6.2"));
        assert!(form.review.join(" ").contains("Downloads and runs code"));
        let last = form.fields.len() - 1;
        form.fields[last].input = InputLine::from("ADD");
        let Request::AddMcpFromCatalog { params, .. } = form.request().unwrap() else {
            panic!("wrong request")
        };
        assert_eq!(params["confirm_run"], true);
        assert_eq!(params["url"], "postgresql://u:pw@h/db");
    }

    #[test]
    fn home_folder_needs_the_named_exposure_confirmation() {
        let entry = formless_core::mcp_catalog::find("filesystem").unwrap();
        let mut form = Form::mcp(entry, &Default::default());
        form.fields[1].input = InputLine::from("~");
        let last = form.fields.len() - 1;
        form.fields[last].input = InputLine::from("ADD");
        let error = form.request().unwrap_err();
        let path = formless_core::mcp_catalog::normalize_directory("~").unwrap();
        assert!(error.contains(&format!("EXPOSE {}", path.display())));
        form.fields[last].input = InputLine::from(format!("EXPOSE {}", path.display()).as_str());
        let Request::AddMcpFromCatalog { params, .. } = form.request().unwrap() else {
            panic!("wrong request")
        };
        assert_eq!(params["confirm_exposure"], true);
    }
}
