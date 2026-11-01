use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::Paths;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonCfg,
    pub relay: RelayCfg,
    pub provider: ProviderCfg,
    pub optimiser: OptimiserCfg,
    pub metrics: MetricsCfg,
    pub tools: ToolsCfg,
    pub scripts: ScriptsCfg,
    pub compaction: CompactionCfg,
    pub tui: TuiCfg,
    pub naming: NamingCfg,
    pub artifacts: ArtifactsCfg,
    pub mcp: McpCfg,
    pub skills: SkillsCfg,
    pub limits: LimitsCfg,
    /// User memory (SPEC §7.11).
    pub memory: MemoryCfg,
    /// Trusted folders (SPEC §10.3). Set in config.toml only, never from
    /// a client or by the agent.
    pub security: SecurityCfg,
    /// `[[hooks]]` (SPEC §9.8): user policy and automation around Tool
    /// calls and Runs.
    pub hooks: Vec<HookCfg>,
    /// More providers beside `[provider]` (which is `default`), by name
    /// (SPEC §4.5). Their API keys live in the keychain, never here.
    pub providers: BTreeMap<String, ProviderCfg>,
    /// Roles: a purpose mapped to a provider + model, described for the
    /// agent so it can pick one for sub-agents (SPEC §4.5).
    pub roles: BTreeMap<String, RoleCfg>,
}

/// Folders whose file access needs no approval (SPEC §10.3). Reading
/// files never asks; writing inside a trusted folder doesn't either.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SecurityCfg {
    /// Ask for approval before sensitive Tool calls (SPEC §10.3). Off by
    /// default: calls run without prompts. `formless --secure` (or
    /// `FORMLESS_SECURE=1`) turns it on.
    pub secure: bool,
    /// Absolute paths (or `~/…`).
    pub trusted_paths: Vec<String>,
    /// Approve writes inside them automatically (logged). False keeps
    /// writes asking, as if no folder were trusted.
    pub trust_writes: bool,
    /// Hosts whose `net:` grants need no approval: `api.example.com`, or
    /// `*.github.com` (proper subdomains, excluding the apex). Bare hosts
    /// are exact only. Bare `net` (any host) is never trusted.
    pub trusted_hosts: Vec<String>,
    /// Extra secret shapes (regular expressions) blanked wherever Formless
    /// stores or quotes text, beside the built-in ones (SPEC §15).
    pub secret_patterns: Vec<String>,
}

impl Default for SecurityCfg {
    fn default() -> Self {
        Self {
            secure: false,
            trusted_paths: Vec::new(),
            trust_writes: true,
            trusted_hosts: Vec::new(),
            secret_patterns: Vec::new(),
        }
    }
}

impl SecurityCfg {
    /// Is `host` (a `net:` grant's host) one of the trusted hosts?
    pub fn host_trusted(&self, host: &str) -> bool {
        use formless_runtime::caps::Capabilities;
        let Ok(requested) = Capabilities::parse(&[format!("net:{}", host.trim())]) else {
            return false;
        };
        if requested.has_bindings() {
            return false;
        }
        self.trusted_hosts.iter().any(|trusted| {
            let trusted = trusted.trim();
            let base = trusted.strip_prefix("*.").unwrap_or(trusted);
            // Preserve rejection of bare TLDs, unscoped '*' and templates.
            if !base.contains('.') {
                return false;
            }
            Capabilities::parse(&[format!("net:{trusted}")])
                .is_ok_and(|granted| !granted.has_bindings() && granted.covers(&requested))
        })
    }

    pub fn network_scope_warnings(&self) -> Vec<String> {
        self.trusted_hosts.iter().filter_map(|trusted| {
            let trusted=trusted.trim();
            if !trusted.strip_prefix("*.").unwrap_or(trusted).contains('.') { return Some("Unsupported trusted host rule is ignored; bare suffixes/unscoped hosts cannot bypass approval".into()); }
            let parsed=formless_runtime::caps::Capabilities::parse(&[format!("net:{}",trusted.trim())]);
            match parsed {
                Ok(grant) if !grant.has_bindings()=>grant.to_specs().iter().find_map(|spec|formless_protocol::net_scope::scope_warning(spec)).map(|warning|format!("Trusted host rule: {warning}")),
                _=>Some("Invalid trusted host rule is ignored; review security.trusted_hosts in config.toml".into()),
            }
        }).collect()
    }

    pub fn roots(&self) -> Vec<String> {
        self.roots_checked().0
    }

    /// The usable trusted folders, and the entries dropped with why (not
    /// absolute, a whole drive or `/`, `..`), for a warning (SPEC §10.3).
    /// `~` is the home folder on every OS; `/` and `\` both separate.
    pub fn roots_checked(&self) -> (Vec<String>, Vec<(String, &'static str)>) {
        let home = directories::BaseDirs::new()
            .map(|d| d.home_dir().to_string_lossy().into_owned())
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_default();
        let mut ok = Vec::new();
        let mut dropped = Vec::new();
        for raw in &self.trusted_paths {
            let p = match raw.strip_prefix('~') {
                Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
                    if home.is_empty() {
                        dropped.push((raw.clone(), "no home folder to expand ~"));
                        continue;
                    }
                    format!("{home}{rest}")
                }
                _ => raw.clone(),
            };
            let parts = path_parts(&p);
            if !is_absolute_any(&p) {
                dropped.push((raw.clone(), "not an absolute path"));
            } else if parts.iter().any(|s| s == "..") {
                dropped.push((raw.clone(), "contains .."));
            } else if parts.len() < 2 && !p.starts_with('/') || parts.is_empty() {
                dropped.push((raw.clone(), "a whole drive or / is too broad"));
            } else {
                ok.push(p.trim_end_matches(['/', '\\']).to_string());
            }
        }
        (ok, dropped)
    }

    /// Is `path` inside (or equal to) a trusted folder, by whole path
    /// components on either separator, with no `..` anywhere?
    pub fn path_trusted(&self, path: &str) -> bool {
        let want = path_parts(path);
        if want.iter().any(|s| s == "..") {
            return false;
        }
        self.roots().iter().any(|r| {
            let root = path_parts(r);
            let windows = is_windows_path(r);
            want.len() >= root.len()
                && root.iter().zip(&want).all(|(a, b)| {
                    if windows {
                        a.eq_ignore_ascii_case(b)
                    } else {
                        a == b
                    }
                })
        })
    }
}

/// A path's components, split on `/` and `\` (a drive `C:` or UNC host
/// is the first one).
fn path_parts(p: &str) -> Vec<String> {
    p.split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .map(str::to_string)
        .collect()
}

/// `C:\…`, `C:/…` or a UNC `\\host\share` path.
fn is_windows_path(p: &str) -> bool {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || p.starts_with("\\\\")
}

/// Absolute on some OS: `/…`, or a Windows drive or UNC path.
fn is_absolute_any(p: &str) -> bool {
    p.starts_with('/') || is_windows_path(p)
}

/// User memory (SPEC §7.11).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MemoryCfg {
    /// Ask before keeping a note the agent writes: "never" (default),
    /// "sensitive" (personal details: health, money, contacts, places,
    /// family, beliefs) or "always".
    pub confirm_writes: String,
}

impl Default for MemoryCfg {
    fn default() -> Self {
        Self {
            confirm_writes: "never".into(),
        }
    }
}

/// Opt-in spending limits (SPEC §5.2): unset means no limit. Dollar
/// limits apply only while every model used has a price.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LimitsCfg {
    /// Provider spend across all Sessions per UTC day.
    pub daily_usd: Option<f64>,
    /// Provider spend of one Run.
    pub run_usd: Option<f64>,
    /// Model steps of one Run.
    pub run_steps: Option<u64>,
    /// Ask before sending when a priced historical p90 forecast exceeds this.
    #[serde(default, deserialize_with = "forecast_threshold")]
    pub forecast_confirm_usd: Option<f64>,
}

fn forecast_threshold<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<f64>, D::Error> {
    let value = Option::<f64>::deserialize(d)?;
    if value.is_some_and(|v| !v.is_finite() || v < 0.0) {
        return Err(serde::de::Error::custom(
            "forecast_confirm_usd must be finite and nonnegative",
        ));
    }
    Ok(value)
}

/// One Role (SPEC §4.5).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RoleCfg {
    /// When to use it, written for the agent ("fast; broad read-only search").
    pub description: String,
    /// A provider name from `[providers]`; unset or "default" = `[provider]`.
    pub provider: Option<String>,
    /// The model; empty = that provider's default model.
    pub model: String,
    pub reasoning_effort: Option<String>,
    /// Extra system prompt for sub-agents in this role.
    pub system: Option<String>,
    /// With `model = "auto"`: the models to choose from, per task, by
    /// measured quality then cost (SPEC §4.6). Empty: the global pool.
    pub candidates: Vec<String>,
    /// Most model turns a sub-agent in this role may take; unset uses
    /// `tools.subagent_max_turns`.
    pub max_turns: Option<u32>,
}

/// A configuration with every option set and one entry in every map and
/// list, so the §13 documentation test sees the keys nested inside them
/// (#851). The test fails while any value here is still null or empty: a
/// new field must be filled in here, and so gets checked.
#[doc(hidden)]
pub fn documented_shape() -> Config {
    let s = |v: &str| v.to_string();
    let mut cfg = Config::default();
    let p = &mut cfg.provider;
    p.reasoning_effort = Some(s("low"));
    p.api_key_header = Some(s("api-key"));
    p.vision = Some(true);
    p.extra_headers.insert(s("*"), s("v"));
    p.extra_body.insert(s("*"), serde_json::json!(1));
    p.aliases.insert(s("*"), s("m"));
    p.query_params.insert(s("*"), s("1"));
    p.model_context_tokens.insert(s("*"), 8192);
    p.fallback_models.push(s("m"));
    p.routing_models.push(s("m"));
    p.routing_fast_models.push(s("m"));
    p.routing_strong_models.push(s("m"));
    p.prices.insert(
        s("*"),
        ModelPrice {
            input: 1.0,
            output: 2.0,
            cached_input: Some(0.5),
            cache_write_input: Some(1.25),
            retrieved_at: Some(s("2026-01-01T00:00:00Z")),
        },
    );
    p.auth.extra_authorize_params.insert(s("*"), s("b"));
    p.auth.extra_token_params.insert(s("*"), s("b"));
    cfg.compaction.model = Some(s("m"));
    cfg.compaction.trigger_fraction = Some(0.8);
    cfg.limits.daily_usd = Some(1.0);
    cfg.limits.run_usd = Some(1.0);
    cfg.limits.forecast_confirm_usd = Some(1.0);
    cfg.limits.run_steps = Some(10);
    cfg.optimiser.model = Some(s("m"));
    cfg.optimiser.reasoning_effort = Some(s("low"));
    cfg.optimiser.token_budget = Some(1000);
    cfg.skills.model = Some(s("m"));
    cfg.security.trusted_hosts.push(s("api.example.com"));
    cfg.security.trusted_paths.push(s("/srv"));
    cfg.security.secret_patterns.push(s("ACME-[0-9]+"));
    cfg.tui.colors.insert(s("*"), s("cyan"));
    cfg.hooks.push(HookCfg {
        event: s("pre_tool"),
        tool: Some(s("t")),
        capability: Some(s("net")),
        run_tool: Some(s("t")),
        command: vec![s("true")],
        trusted: true,
        timeout_secs: Some(5),
    });
    cfg.mcp.servers.insert(
        s("*"),
        McpServerCfg {
            transport: s("stdio"),
            command: s("srv"),
            args: vec![s("--x")],
            cwd: Some(s("/srv")),
            env: [(s("*"), s("V"))].into(),
            env_keys: vec![s("HOME")],
            url: s("https://mcp.example.com"),
            headers: [(s("*"), s("v"))].into(),
            disabled: false,
            oauth_client_id: Some(s("c")),
            trusted: false,
            disabled_tools: vec![s("x")],
        },
    );
    cfg.roles.insert(
        s("*"),
        RoleCfg {
            description: s("d"),
            provider: Some(s("p")),
            model: s("m"),
            reasoning_effort: Some(s("low")),
            system: Some(s("s")),
            candidates: vec![s("m")],
            max_turns: Some(3),
        },
    );
    let provider = cfg.provider.clone();
    cfg.providers.insert(s("*"), provider);
    cfg
}

/// The role an old global `provider.model = "auto"` migrates to.
pub const PRIMARY_ROLE: &str = "primary";

/// The name of the built-in provider (`[provider]`).
pub const DEFAULT_PROVIDER: &str = "default";

impl Config {
    /// A provider's settings by name (`default` or unset = `[provider]`).
    /// USD for usage served by provider `provider` ("" or `default` for
    /// the default one), priced from that provider's own table (#810); a
    /// provider no longer configured falls back to the default's prices.
    #[allow(clippy::too_many_arguments)]
    pub fn cost_of(
        &self,
        provider: &str,
        model: &str,
        prompt: i64,
        completion: i64,
        cached: i64,
        cache_write: i64,
        reported: Option<f64>,
    ) -> Option<f64> {
        let p = match provider {
            "" | DEFAULT_PROVIDER => &self.provider,
            name => self.providers.get(name).unwrap_or(&self.provider),
        };
        p.cost_of(model, prompt, completion, cached, cache_write, reported)
    }

    pub fn provider_cfg(&self, name: Option<&str>) -> Result<ProviderCfg> {
        match name.filter(|n| !n.is_empty() && *n != DEFAULT_PROVIDER) {
            None => Ok(self.provider.clone()),
            Some(n) => self.providers.get(n).cloned().with_context(|| {
                format!(
                    "unknown provider '{n}' (configured: default{})",
                    self.providers
                        .keys()
                        .map(|k| format!(", {k}"))
                        .collect::<String>()
                )
            }),
        }
    }

    /// Migrate the deprecated global `provider.model = "auto"` (SPEC §4.6)
    /// to a `primary` role with `model = "auto"` whose candidates are the
    /// former routing pool, and point the default model at it. Returns the
    /// warning to show, or None when there was nothing to migrate.
    pub fn migrate_global_auto(&mut self) -> Option<String> {
        if !ProviderCfg::is_auto_model(&self.provider.model) {
            return None;
        }
        let candidates = self.provider.routing_candidates();
        self.roles
            .entry(PRIMARY_ROLE.to_string())
            .or_insert_with(|| RoleCfg {
                description: "The primary conversation (migrated from provider.model = \"auto\")."
                    .into(),
                model: "auto".into(),
                candidates: candidates.clone(),
                ..Default::default()
            });
        self.provider.model = format!("role:{PRIMARY_ROLE}");
        Some(format!(
            "provider.model = \"auto\" is deprecated: migrated to role '{PRIMARY_ROLE}' (model auto, candidates: {}). Routing is now per role and per task.",
            if candidates.is_empty() { "the global pool".to_string() } else { candidates.join(", ") }
        ))
    }

    /// A Role by name, with an error listing the configured ones.
    pub fn role(&self, name: &str) -> Result<&RoleCfg> {
        self.roles.get(name).with_context(|| {
            if self.roles.is_empty() {
                format!("unknown role '{name}': no roles are configured")
            } else {
                format!(
                    "unknown role '{name}' (roles: {})",
                    self.roles.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            }
        })
    }
}

/// One hook: on `event`, for matching calls, run a registry Tool or a
/// command. A pre_tool hook may deny the call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HookCfg {
    /// "pre_tool", "post_tool" or "run_end".
    pub event: String,
    /// Tool name to match: exact, or with `*` wildcards ("mcp__*").
    pub tool: Option<String>,
    /// Match only Tools declaring a Capability starting with this ("net").
    pub capability: Option<String>,
    /// Registry Tool to run with the event as input.
    pub run_tool: Option<String>,
    /// Command (no shell) run with the event as JSON on stdin.
    pub command: Vec<String>,
    /// Run `command` without asking: you wrote it. Unset, it is approved
    /// once per daemon lifetime by the user.
    pub trusted: bool,
    pub timeout_secs: Option<u64>,
}

/// Automatic skills (SPEC §7.10): distilled from successful Runs,
/// measured, and retired when unused or unhelpful.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsCfg {
    /// Distil a skill after a successful, non-trivial Run.
    pub auto_create: bool,
    /// Tool calls a Run needs before it is worth a skill (a Tool sequence
    /// already seen in another Session needs only 3).
    pub min_tool_calls: usize,
    /// Model for distilling; unset uses the Session's.
    pub model: Option<String>,
    /// Retire auto skills not loaded for this many days (0: never).
    pub auto_retire_days: u64,
    /// Retire auto skills whose Runs complete less often than this…
    pub min_success_rate: f64,
    /// …once they have been loaded in at least this many Runs.
    pub min_runs_for_rate: i64,
}
impl Default for SkillsCfg {
    fn default() -> Self {
        Self {
            auto_create: true,
            min_tool_calls: 5,
            model: None,
            auto_retire_days: 30,
            min_success_rate: 0.5,
            min_runs_for_rate: 5,
        }
    }
}

/// USD per million tokens for one model.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    /// Cached input; unset charges cached tokens at `input`.
    pub cached_input: Option<f64>,
    /// Input written to the prompt cache (Anthropic bills 1.25× input);
    /// unset charges it at `input` (#804).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_input: Option<f64>,
    /// When the price was taken from the endpoint's `/models` listing
    /// (RFC 3339), so `doctor` can say it may be stale (#809).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieved_at: Option<String>,
}

impl ModelPrice {
    /// USD for one request. `cached` and `cache_write` are parts of
    /// `prompt` (the provider layer normalises both reporting styles), so
    /// each is charged once at its own rate.
    pub fn cost(&self, prompt: i64, completion: i64, cached: i64, cache_write: i64) -> f64 {
        let fresh = (prompt - cached - cache_write).max(0) as f64;
        let cached_rate = self.cached_input.unwrap_or(self.input);
        let write_rate = self.cache_write_input.unwrap_or(self.input);
        (fresh * self.input
            + cached as f64 * cached_rate
            + cache_write as f64 * write_rate
            + completion as f64 * self.output)
            / 1e6
    }
}

/// A model id without its aggregator prefix (`anthropic/claude-x` →
/// `claude-x`).
fn bare_model(model: &str) -> &str {
    model.rsplit_once('/').map_or(model, |(_, m)| m)
}

/// A yes/no environment variable: `1/true/yes/on` or `0/false/no/off`
/// (any case); unset or empty is `None`, anything else an error.
pub fn env_flag(name: &str) -> Result<Option<bool>> {
    let v = match std::env::var(name) {
        Ok(v) => v,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        // Set but unreadable is not "unset" (#846).
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(anyhow!(
                "{name} is not text; use 1/true/yes/on or 0/false/no/off"
            ))
        }
    };
    match v.trim().to_ascii_lowercase().as_str() {
        "" => Ok(None),
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        other => Err(anyhow!(
            "{name}={other} is not a yes/no value (use 1/true/yes/on or 0/false/no/off)"
        )),
    }
}

/// Keys `harness:models` may change (SPEC §7.13); other settable keys are
/// `harness:config`.
pub const HARNESS_MODEL_KEYS: &[&str] = &[
    "provider.model",
    "provider.reasoning_effort",
    "provider.context_tokens",
    "provider.model_context_tokens",
    "provider.fallback_models",
    "provider.routing_models",
    "provider.routing_fast_models",
    "provider.routing_strong_models",
];

/// Settings clients may change through the daemon (`SetSetting`): exact
/// keys, or whole sections ending in '.'. Credentials, the endpoint, extra
/// headers, the relay, MCP servers and hooks stay file-only.
/// Settable keys whose value must be one of a few strings (null always
/// resets to the default).
pub const CHOICES: &[(&str, &[&str])] = &[
    ("provider.api_style", &["chat", "responses", "anthropic"]),
    ("provider.tool_mode", &["native", "prompt"]),
    ("provider.cache_key", &["session", "off"]),
    ("provider.cache_control", &["auto", "on", "off"]),
    ("memory.confirm_writes", &["never", "sensitive", "always"]),
    ("tui.notify", &["off", "bell", "desktop"]),
    ("tui.hyperlinks", &["auto", "on", "off"]),
    ("tui.theme", &["auto", "dark", "light", "high-contrast"]),
    ("tui.inline_images", &["auto", "off"]),
    ("tui.on_start", &["last", "new", "picker"]),
    ("tui.language", &["auto", "en", "th"]),
];

/// SPEC §13's settings table is the one description of every key; the
/// daemon serves it to clients (`GetSettings.docs`).
const SPEC: &str = include_str!("../../../SPEC.md");

/// Every key in SPEC §13 with (meaning, default as SPEC writes it). Rows
/// like `daemon.log_max_bytes` / `log_keep` name several keys of one
/// section; their defaults split on " / " the same way.
pub fn setting_docs() -> BTreeMap<String, (String, String)> {
    let section = SPEC
        .split("\n## 13.")
        .nth(1)
        .and_then(|s| s.split("\n## ").next())
        .unwrap_or_default();
    let mut out = BTreeMap::new();
    for line in section.lines().filter(|l| l.starts_with("| `")) {
        let cells: Vec<&str> = line.trim_matches('|').split(" | ").map(str::trim).collect();
        let [keys, default, meaning] = cells[..] else {
            continue;
        };
        let names: Vec<&str> = keys
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|k| !k.starts_with('['))
            .collect();
        let Some(first) = names.first() else { continue };
        let prefix = first.rsplit_once('.').map_or("", |(p, _)| p);
        let defaults: Vec<&str> = default.split(" / ").collect();
        // Plain words: no Markdown ticks, no section references.
        let mut text = meaning.replace('`', "");
        while let Some(i) = text.find(" (§") {
            let Some(j) = text[i..].find(')') else { break };
            text.replace_range(i..i + j + 1, "");
        }
        for (i, name) in names.iter().enumerate() {
            let key = if name.contains('.') {
                name.to_string()
            } else {
                format!("{prefix}.{name}")
            };
            let d = defaults
                .get(i)
                .or(defaults.first())
                .copied()
                .unwrap_or_default();
            out.insert(key, (text.trim().to_string(), d.replace('`', "")));
        }
    }
    out
}

/// Keys about credentials: never in client-facing settings data.
const CREDENTIAL_KEYS: &[&str] = &["provider.extra_headers"];

/// `{key: {description, default}}` for clients: the default is the real
/// one from `Config::default()` when it has a value, else SPEC's text.
pub fn setting_docs_json() -> serde_json::Value {
    let defaults = serde_json::to_value(Config::default()).unwrap_or_default();
    let docs: serde_json::Map<String, serde_json::Value> = setting_docs()
        .into_iter()
        // Credential-bearing keys stay out of what clients see, even by
        // name; no client can change them anyway.
        .filter(|(key, _)| !CREDENTIAL_KEYS.contains(&key.as_str()))
        .map(|(key, (description, spec_default))| {
            let pointer = format!("/{}", key.replace('.', "/"));
            let default = match defaults.pointer(&pointer) {
                Some(v) if !v.is_null() && v != "" => v.clone(),
                _ => serde_json::Value::String(spec_default),
            };
            (
                key,
                serde_json::json!({"description": description, "default": default}),
            )
        })
        .collect();
    serde_json::Value::Object(docs)
}

/// Settable keys a running client can't pick up: they apply when the TUI
/// next starts (SPEC §10.4). Every other settable key is hot: the daemon
/// re-reads the file, and running TUIs reload on `SettingsChanged`.
pub const ON_TUI_START: &[&str] = &["tui.on_start", "tui.screen_reader"];

pub const SETTABLE: &[&str] = &[
    "provider.model",
    "provider.reasoning_effort",
    "provider.context_tokens",
    "provider.model_context_tokens",
    "provider.max_retries",
    "provider.fallback_models",
    "provider.routing_models",
    "provider.routing_fast_models",
    "provider.routing_strong_models",
    "provider.routing_complex_chars",
    "provider.stall_notice_secs",
    "provider.first_byte_timeout_secs",
    "provider.max_backoff_secs",
    "provider.cache_key",
    "provider.cache_control",
    "provider.tool_use_nudge",
    "memory.confirm_writes",
    "provider.api_style",
    "provider.tool_mode",
    "provider.vision",
    "provider.prices",
    "daemon.idle_grace_secs",
    "daemon.stall_warning_secs",
    "daemon.keep_alive_for_jobs",
    "daemon.backup_restore_drill",
    "daemon.delete_undo_secs",
    "tui.",
    "tools.",
    "scripts.",
    "metrics.",
    "optimiser.",
    "compaction.",
    "naming.",
    "skills.",
    "artifacts.",
    "limits.",
];

/// External MCP servers ((SPEC §7.9)). None ship by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpCfg {
    /// Stop a server's process after this long without calls (it restarts
    /// on the next one).
    pub idle_secs: u64,
    pub servers: BTreeMap<String, McpServerCfg>,
}
impl Default for McpCfg {
    fn default() -> Self {
        Self {
            idle_secs: 300,
            servers: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpServerCfg {
    /// "stdio" (default) or "http".
    pub transport: String,
    /// stdio: the program and its arguments.
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// stdio: literal environment values for the server.
    pub env: BTreeMap<String, String>,
    /// stdio: names of daemon environment variables passed through (never
    /// `FORMLESS_*`); nothing else of the daemon's environment is.
    pub env_keys: Vec<String>,
    /// http: the endpoint, plus static headers.
    pub url: String,
    pub headers: BTreeMap<String, String>,
    /// Disabled servers keep their config but are never started.
    pub disabled: bool,
    /// http: a pre-registered OAuth client id, for servers that offer no
    /// dynamic client registration.
    pub oauth_client_id: Option<String>,
    /// Trusted servers' tools annotated `readOnlyHint` run without a
    /// per-tool approval (the server itself is still approved once).
    pub trusted: bool,
    /// The server's own names of tools to leave out of the registry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
}

/// Retention for stored Tool outputs (SPEC §9.6). 0 disables a limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtifactsCfg {
    /// Oldest artifacts are removed beyond this total size.
    pub max_total_mb: u64,
    /// Artifacts older than this are removed.
    pub max_age_days: u64,
}
impl Default for ArtifactsCfg {
    fn default() -> Self {
        Self {
            max_total_mb: 1024,
            max_age_days: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonCfg {
    pub idle_grace_secs: u64,
    pub stall_warning_secs: u64,
    pub log_max_bytes: u64,
    pub log_keep: usize,
    /// How long a client waits for an auto-started daemon to accept
    /// connections (a first start may migrate and back up a large DB).
    pub start_timeout_secs: u64,
    /// Stay up past the idle grace while scheduled jobs wait (SPEC §9.7);
    /// off: due jobs run on the next start.
    pub keep_alive_for_jobs: bool,
    /// How long a deleted Session can be restored before it is gone.
    pub delete_undo_secs: u64,
    /// Daily database backups kept in `<data>/backups`; 0 turns them off.
    pub backups_keep: usize,
    /// Offline restore rehearsal after each daily backup (opt-in).
    pub backup_restore_drill: bool,
}
impl Default for DaemonCfg {
    fn default() -> Self {
        Self {
            idle_grace_secs: 60,
            stall_warning_secs: 300,
            log_max_bytes: 10_000_000,
            log_keep: 5,
            start_timeout_secs: 60,
            keep_alive_for_jobs: false,
            delete_undo_secs: 30,
            backups_keep: 7,
            backup_restore_drill: false,
        }
    }
}

/// Hosted relay (SPEC §17 v2): the daemon dials out to `url`; browsers
/// meet it there. The pairing secret itself is never in config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RelayCfg {
    pub enabled: bool,
    pub url: String,
}
impl Default for RelayCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "wss://formless.athiwat.xyz".into(),
        }
    }
}
/// `localhost`, `*.localhost`, or a loopback IP address.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

impl RelayCfg {
    /// The relay address must be `wss://`, or `ws://` to this machine: the
    /// browser page served over plain HTTP would carry the pairing secret
    /// where anyone on the path can read it (#907).
    /// `FORMLESS_RELAY_ALLOW_INSECURE=1` allows other `ws://` relays, for
    /// development only.
    pub fn check(&self) -> Result<()> {
        let url = reqwest::Url::parse(self.url.trim()).map_err(|e| {
            anyhow::anyhow!(
                "relay.url {:?} is not a URL ({e}): use wss://host, e.g. wss://formless.athiwat.xyz",
                self.url
            )
        })?;
        let host = url.host_str().unwrap_or_default();
        if host.is_empty() {
            bail!("relay.url {:?} has no host: use wss://host", self.url);
        }
        match url.scheme() {
            "wss" => Ok(()),
            "ws" if crate::config::is_loopback_host(host) => Ok(()),
            "ws" if std::env::var("FORMLESS_RELAY_ALLOW_INSECURE").is_ok_and(|v| v == "1") => Ok(()),
            "ws" => bail!(
                "relay.url {:?} is plain ws:// to another machine: the pairing page would travel unencrypted. Use wss:// (or set FORMLESS_RELAY_ALLOW_INSECURE=1 for local development only)",
                self.url
            ),
            other => bail!(
                "relay.url {:?} has scheme {other}: use wss://host",
                self.url
            ),
        }
    }

    /// Browser-facing base URL derived from the websocket URL.
    pub fn web_url(&self) -> String {
        if let Some(rest) = self.url.strip_prefix("wss://") {
            format!("https://{rest}")
        } else if let Some(rest) = self.url.strip_prefix("ws://") {
            format!("http://{rest}")
        } else {
            self.url.clone()
        }
        .trim_end_matches('/')
        .to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderCfg {
    pub base_url: String,
    pub extra_headers: HashMap<String, String>,
    /// Vendor request fields merged into every request body (Ollama's
    /// `options`, vLLM's `chat_template_kwargs`, OpenRouter's `provider`
    /// routing). `model`, `messages`, `input`, `tools`, `stream` and
    /// `system` are never overridden (#622).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_body: BTreeMap<String, serde_json::Value>,
    /// Short names for model ids (`fast = "…"`), usable wherever a model
    /// is accepted; changing one changes everything that uses it.
    pub aliases: BTreeMap<String, String>,
    /// Query parameters added to every request (`api-version` for Azure
    /// OpenAI, gateway options).
    pub query_params: BTreeMap<String, String>,
    /// Send the API key in this header instead of `Authorization: Bearer`
    /// (`api-key` for Azure OpenAI).
    pub api_key_header: Option<String>,
    /// HTTP(S) proxy for this provider's requests; empty uses
    /// `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` from the environment.
    pub proxy: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub context_tokens: u32,
    pub model_context_tokens: HashMap<String, u32>,
    pub max_retries: usize,
    /// "chat" (Chat Completions, default) or "responses" (Responses API).
    #[serde(default = "default_api_style")]
    pub api_style: String,
    /// "native" tool-calling (default) or "prompt" fallback for
    /// endpoints/models without native tool-calling.
    #[serde(default = "default_tool_mode")]
    pub tool_mode: String,
    /// Extra models tried, in order, when the selected model fails with
    /// a retryable/model error (model routing/failover).
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// Candidate models for measured auto-selection when `model = "auto"`.
    /// Falls back to `fallback_models` when empty.
    #[serde(default)]
    pub routing_models: Vec<String>,
    /// Optional user-labelled subsets of routing candidates for simple tasks.
    #[serde(default)]
    pub routing_fast_models: Vec<String>,
    /// Optional user-labelled subsets of routing candidates for complex tasks.
    #[serde(default)]
    pub routing_strong_models: Vec<String>,
    /// Estimated context characters at/above which routing treats a task as complex.
    #[serde(default = "default_routing_complex_chars")]
    pub routing_complex_chars: usize,
    /// Credential mode: API key (default) or OAuth2 subscription login.
    #[serde(default)]
    pub auth: AuthCfg,
    /// Send attached images to the model as image parts. Unset: guessed
    /// from the model name (see `provider::is_vision_model`).
    #[serde(default)]
    pub vision: Option<bool>,
    /// Longest side, in pixels, of an image sent to a vision model; larger
    /// ones are scaled down once per Session (#616).
    pub image_max_px: u32,
    /// The `detail` asked of vision models: `auto`, `low` or `high`.
    pub image_detail: String,
    /// Prices in USD per million tokens, per model, for the cost estimate
    /// in the status line (SPEC §10.6). Unset models show no cost.
    #[serde(default)]
    pub prices: HashMap<String, ModelPrice>,
    /// Seconds without any bytes from an open provider stream before
    /// clients are told it looks stalled (0 = never).
    pub stall_notice_secs: u64,
    /// Re-send the request automatically when no first byte arrives within
    /// this many seconds (0 = off; the user can always `/resend`).
    pub first_byte_timeout_secs: u64,
    /// "session" (default): send a per-Session `prompt_cache_key` so the
    /// endpoint keeps one Session on one prompt cache; "off": never send it.
    pub cache_key: String,
    /// Prompt-cache breakpoints (`cache_control: {type: ephemeral}`) on
    /// the system prompt and the last stable message, which Anthropic
    /// models need to cache at all: "auto" (default) sends them to
    /// `claude`/`anthropic/` model ids, "on" to every model, "off" never.
    pub cache_control: String,
    /// Push models to build Tools (#237): a per-family system-prompt
    /// addendum (GPT models) and one nudge per step when an answer refuses
    /// for lack of access before using any Tool.
    pub tool_use_nudge: bool,
    /// Longest wait between retries when the server gives no hint (full
    /// jitter below it). A server's `Retry-After` is honoured up to 5 min.
    pub max_backoff_secs: u64,
}

/// OAuth2 authorization-code + PKCE settings for subscription providers.
/// Tokens are stored like the API key (keychain +0600 file), never here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthCfg {
    /// "api_key" (default) or "oauth".
    pub mode: String,
    pub authorize_url: String,
    pub token_url: String,
    pub client_id: String,
    /// Space- or comma-separated scope string, sent verbatim.
    pub scopes: String,
    /// Localhost callback port for `formless auth login` (0 = any free port).
    pub redirect_port: u16,
    /// Provider-specific authorization parameters (for example
    /// `access_type=offline`, `prompt=consent`, or an Auth0 `audience`).
    #[serde(default)]
    pub extra_authorize_params: BTreeMap<String, String>,
    /// Provider-specific token-endpoint form parameters.
    #[serde(default)]
    pub extra_token_params: BTreeMap<String, String>,
    /// RFC 7009 token revocation endpoint: `formless auth logout` revokes
    /// the refresh token there before deleting local copies (#866).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub revocation_url: String,
}
impl Default for AuthCfg {
    fn default() -> Self {
        Self {
            mode: "api_key".into(),
            authorize_url: String::new(),
            token_url: String::new(),
            client_id: String::new(),
            scopes: String::new(),
            redirect_port: 8977,
            extra_authorize_params: BTreeMap::new(),
            extra_token_params: BTreeMap::new(),
            revocation_url: String::new(),
        }
    }
}

fn default_api_style() -> String {
    "chat".into()
}
fn default_tool_mode() -> String {
    "native".into()
}
fn default_routing_complex_chars() -> usize {
    20_000
}
impl Default for ProviderCfg {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            extra_headers: HashMap::new(),
            extra_body: BTreeMap::new(),
            image_max_px: 1568,
            image_detail: "auto".into(),
            aliases: BTreeMap::new(),
            query_params: BTreeMap::new(),
            api_key_header: None,
            proxy: String::new(),
            model: String::new(),
            reasoning_effort: None,
            context_tokens: 128_000,
            model_context_tokens: HashMap::new(),
            max_retries: 5,
            api_style: default_api_style(),
            tool_mode: default_tool_mode(),
            fallback_models: Vec::new(),
            routing_models: Vec::new(),
            routing_fast_models: Vec::new(),
            routing_strong_models: Vec::new(),
            routing_complex_chars: default_routing_complex_chars(),
            auth: AuthCfg::default(),
            vision: None,
            prices: HashMap::new(),
            stall_notice_secs: 120,
            first_byte_timeout_secs: 0,
            max_backoff_secs: 30,
            cache_key: "session".into(),
            cache_control: "auto".into(),
            tool_use_nudge: true,
        }
    }
}
impl ProviderCfg {
    /// Send `cache_control` breakpoints to `model` (see `cache_control`)?
    pub fn cache_breakpoints(&self, model: &str) -> bool {
        match self.cache_control.to_ascii_lowercase().as_str() {
            "on" => true,
            "off" => false,
            _ => {
                let m = model.to_ascii_lowercase();
                m.starts_with("anthropic/") || m.contains("claude")
            }
        }
    }
    pub fn responses(&self) -> bool {
        self.api_style.eq_ignore_ascii_case("responses")
    }
    /// The native Anthropic Messages API (`api_style = "anthropic"`).
    pub fn anthropic(&self) -> bool {
        self.api_style.eq_ignore_ascii_case("anthropic")
    }
    pub fn prompt_tools(&self) -> bool {
        self.tool_mode.eq_ignore_ascii_case("prompt")
    }
    pub fn oauth(&self) -> bool {
        self.auth.mode.eq_ignore_ascii_case("oauth")
    }
    pub fn routing_candidates(&self) -> Vec<String> {
        let source = if self.routing_models.is_empty() {
            &self.fallback_models
        } else {
            &self.routing_models
        };
        let mut out: Vec<String> = Vec::new();
        for m in source {
            let m = m.trim().to_string();
            if !m.is_empty() && !m.eq_ignore_ascii_case("auto") && !out.contains(&m) {
                out.push(m);
            }
        }
        out
    }
    /// The model id an alias stands for (one level; case-insensitive), or
    /// `model` itself.
    pub fn resolve_alias(&self, model: &str) -> String {
        let m = model.trim();
        self.aliases
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(m))
            .map_or_else(|| m.to_string(), |(_, v)| v.trim().to_string())
    }

    pub fn is_auto_model(model: &str) -> bool {
        model.trim().eq_ignore_ascii_case("auto")
    }
    /// USD for some of `model`'s use: what the provider reported when it
    /// reported it, else priced from `prices`, else unknown.
    pub fn cost_of(
        &self,
        model: &str,
        prompt: i64,
        completion: i64,
        cached: i64,
        cache_write: i64,
        reported: Option<f64>,
    ) -> Option<f64> {
        reported.or_else(|| {
            self.price_for(model)
                .map(|p| p.cost(prompt, completion, cached, cache_write))
        })
    }

    /// The price for `model`: an exact entry, else one that matches once
    /// aggregator prefixes are dropped on either side (#809). Deterministic:
    /// the first such key in name order.
    pub fn price_for(&self, model: &str) -> Option<&ModelPrice> {
        if let Some(p) = self.prices.get(model) {
            return Some(p);
        }
        let want = bare_model(model);
        let mut keys: Vec<&String> = self.prices.keys().collect();
        keys.sort();
        keys.into_iter()
            .find(|k| bare_model(k) == want)
            .and_then(|k| self.prices.get(k))
    }
    pub fn context_for(&self, model: &str) -> u32 {
        self.model_context_tokens
            .get(model)
            .copied()
            .unwrap_or(self.context_tokens)
    }
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OptimiserCfg {
    pub cooldown_secs: u64,
    pub max_attempts_per_day: usize,
    pub give_up_after_failures: usize,
    pub promotion_margin_points: u32,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub token_budget: Option<u64>,
    /// Times each prompt-eval scenario runs per arm (SPEC §8.6).
    pub eval_repeats: u32,
    /// How often the optimiser checks triggers and its queue.
    pub scan_interval_secs: u64,
    /// Suggest a Tool for a pair of raw MCP calls the agent made back to
    /// back in at least this many Runs (0 = never) …
    pub mcp_chain_min_runs: usize,
    /// … within this many days (SPEC §8.3).
    pub mcp_chain_days: u32,
}
impl Default for OptimiserCfg {
    fn default() -> Self {
        Self {
            cooldown_secs: 3600,
            max_attempts_per_day: 3,
            give_up_after_failures: 3,
            promotion_margin_points: 10,
            model: None,
            reasoning_effort: None,
            token_budget: None,
            eval_repeats: 3,
            scan_interval_secs: 10,
            mcp_chain_min_runs: 3,
            mcp_chain_days: 14,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsCfg {
    pub success_window: usize,
    pub trigger_min_invocations: usize,
    pub trigger_success_below: f64,
    pub retention_per_tool: usize,
}
impl Default for MetricsCfg {
    fn default() -> Self {
        Self {
            success_window: 100,
            trigger_min_invocations: 20,
            trigger_success_below: 0.90,
            retention_per_tool: 200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsCfg {
    pub pending_threshold_secs: u64,
    pub default_memory_mb: u32,
    pub max_memory_mb: u32,
    pub artifact_preview_chars: usize,
    /// Registry size up to which every Tool is listed to the model; past
    /// it, Sessions see loaded + most-used Tools and use find_tools.
    pub inline_limit: usize,
    /// find_tools ranking: "bm25" (default: names, descriptions and input
    /// fields, with stemming and synonyms) or "keyword" (word matches).
    #[serde(default = "default_tool_search")]
    pub search: String,
    /// Most Tools a Session keeps listed; the oldest loaded ones are
    /// dropped at compaction (a Session's own creations stay).
    #[serde(default = "default_session_max_loaded")]
    pub session_max_loaded: usize,
    /// How deep `agents.run` sub-agents may nest (1 = the main agent's
    /// Tools may spawn sub-agents; theirs may not).
    pub max_agent_depth: usize,
    /// A creation smoke test (synthetic input) that runs longer than this
    /// is stopped: inconclusive for a Tool, a failure for a module.
    #[serde(default = "default_smoke_timeout_secs")]
    pub smoke_timeout_secs: u64,
    /// One optimiser replay Invocation is stopped after this long.
    #[serde(default = "default_replay_timeout_secs")]
    pub replay_timeout_secs: u64,
    /// How deep `tools.call` composition may nest (a cycle guard).
    #[serde(default = "default_max_tool_depth")]
    pub max_tool_depth: usize,
    /// Longest single `sleep(ms)` inside a Tool, in seconds.
    #[serde(default = "default_max_sleep_secs")]
    pub max_sleep_secs: u64,
    /// Largest `randomBytes(n)` a Tool may ask for.
    #[serde(default = "default_max_random_bytes")]
    pub max_random_bytes: usize,
    /// Native caps outside the JS heap (#604): one `fs.read` or `fetch`
    /// body, each stream of one `process.run`, all `log` lines of an
    /// Invocation, and everything one Invocation `emit`s.
    #[serde(default = "default_max_read_bytes")]
    pub max_read_bytes: u64,
    /// Finite network deadlines for Tool fetch/fetchChunks, in seconds.
    pub fetch_connect_timeout_secs: u64,
    pub fetch_timeout_secs: u64,
    #[serde(default = "default_max_process_output_bytes")]
    pub max_process_output_bytes: u64,
    #[serde(default = "default_max_log_bytes")]
    pub max_log_bytes: u64,
    #[serde(default = "default_max_emit_bytes")]
    pub max_emit_bytes: u64,
    /// Most results one `find_tools` call may return.
    #[serde(default = "default_find_tools_max")]
    pub find_tools_max: usize,
    /// Most model turns a sub-agent may take before it stops (a quality
    /// signal for routing, SPEC §4.6).
    #[serde(default = "default_subagent_max_turns")]
    pub subagent_max_turns: u32,
    /// Per-task routing for `auto` roles (SPEC §4.6): samples a model
    /// needs before its score counts …
    #[serde(default = "default_task_min_samples")]
    pub task_min_samples: i64,
    /// … the share of good tasks it must reach …
    #[serde(default = "default_task_quality_bar")]
    pub task_quality_bar: f64,
    /// … and how often an under-sampled candidate is tried instead.
    #[serde(default = "default_task_explore_rate")]
    pub task_explore_rate: f64,
    /// Ask the model, once per Tool version, for a plain-language summary
    /// shown on approval prompts (SPEC §10.3).
    pub approval_summaries: bool,
}
fn default_subagent_max_turns() -> u32 {
    40
}
fn default_task_min_samples() -> i64 {
    5
}
fn default_task_quality_bar() -> f64 {
    0.8
}
fn default_task_explore_rate() -> f64 {
    0.1
}
fn default_max_tool_depth() -> usize {
    8
}
fn default_max_sleep_secs() -> u64 {
    60
}
fn default_max_read_bytes() -> u64 {
    64 << 20
}
fn default_max_process_output_bytes() -> u64 {
    16 << 20
}
fn default_max_log_bytes() -> u64 {
    1 << 20
}
fn default_max_emit_bytes() -> u64 {
    1 << 30
}
fn default_max_random_bytes() -> usize {
    1 << 20
}
fn default_find_tools_max() -> usize {
    25
}
fn default_smoke_timeout_secs() -> u64 {
    10
}
fn default_replay_timeout_secs() -> u64 {
    60
}
fn default_session_max_loaded() -> usize {
    60
}

fn default_tool_search() -> String {
    "bm25".into()
}

impl Default for ToolsCfg {
    fn default() -> Self {
        Self {
            pending_threshold_secs: 300,
            default_memory_mb: 256,
            max_memory_mb: 4096,
            artifact_preview_chars: 2000,
            inline_limit: 40,
            search: default_tool_search(),
            session_max_loaded: default_session_max_loaded(),
            max_agent_depth: 1,
            smoke_timeout_secs: default_smoke_timeout_secs(),
            replay_timeout_secs: default_replay_timeout_secs(),
            max_tool_depth: default_max_tool_depth(),
            max_sleep_secs: default_max_sleep_secs(),
            max_random_bytes: default_max_random_bytes(),
            max_read_bytes: default_max_read_bytes(),
            fetch_connect_timeout_secs: 10,
            fetch_timeout_secs: 60,
            max_process_output_bytes: default_max_process_output_bytes(),
            max_log_bytes: default_max_log_bytes(),
            max_emit_bytes: default_max_emit_bytes(),
            find_tools_max: default_find_tools_max(),
            subagent_max_turns: default_subagent_max_turns(),
            task_min_samples: default_task_min_samples(),
            task_quality_bar: default_task_quality_bar(),
            task_explore_rate: default_task_explore_rate(),
            approval_summaries: true,
        }
    }
}

/// Orchestration scripts (`run_script`, SPEC §7.12).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptsCfg {
    /// Most nested Invocations one sandbox runs at once through
    /// `tools.call`; the rest queue.
    pub max_parallel: usize,
    /// Largest script source, in KB.
    pub max_source_kb: usize,
    /// Script heap cap, in MB.
    pub memory_mb: u32,
    /// A script still running after this long is stopped; its nested
    /// Invocations are abandoned, not cancelled.
    pub timeout_secs: u64,
    /// Distinct Sessions running the same script before it is flagged
    /// "save as a Tool".
    pub recurrence_threshold: usize,
}
impl Default for ScriptsCfg {
    fn default() -> Self {
        Self {
            max_parallel: 8,
            max_source_kb: 64,
            memory_mb: 256,
            timeout_secs: 600,
            recurrence_threshold: 3,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionCfg {
    /// Prefer POST {base_url}/compact when the endpoint supports it (§11).
    pub server_side: bool,
    pub model: Option<String>,
    /// Fraction of context_tokens at which compaction triggers.
    pub trigger_fraction: Option<f64>,
}
impl CompactionCfg {
    pub fn fraction(&self) -> f64 {
        self.trigger_fraction.unwrap_or(0.8)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiCfg {
    pub queue_key: String,
    /// Persistent multi-pane layout (sessions/tools beside chat) on wide
    /// terminals. Toggled with `/panes` and remembered across starts.
    #[serde(default)]
    pub multi_pane: bool,
    /// Tell the user when a Run finishes, fails or stalls, or needs an
    /// answer, while they are elsewhere: "off", "bell" or "desktop"
    /// (a terminal / browser notification plus the bell).
    pub notify: String,
    /// Runs shorter than this many seconds finish quietly.
    pub notify_min_secs: u64,
    /// Message times and day separators in the TUI chat (`/timestamps`).
    pub timestamps: bool,
    /// Clickable OSC 8 links in the TUI chat: "auto", "on" or "off".
    pub hyperlinks: String,
    /// "auto" (light or dark from the terminal), "dark", "light" or
    /// "high-contrast".
    pub theme: String,
    /// Per-role colour overrides: user, assistant, tool, note, error,
    /// accent → a colour name or `#rrggbb`.
    pub colors: HashMap<String, String>,
    /// Dim line numbers in the chat's code blocks.
    pub code_line_numbers: bool,
    /// Image thumbnails in the chat on terminals that can show them:
    /// "auto" or "off".
    pub inline_images: String,
    /// The multi-pane layout's side panes as a percentage of the width:
    /// [left, right]; 0 hides one.
    pub pane_widths: Vec<u16>,
    /// Start the line-oriented plain mode instead of the full-screen TUI
    /// (as `formless --plain` does).
    pub screen_reader: bool,
    /// What the TUI opens on launch: "last" (the most recently active
    /// Session), "new" (a fresh one) or "picker" (the Sessions view).
    pub on_start: String,
    /// UI language: "auto" (the system locale), "en" or "th".
    pub language: String,
    /// Spinners and other moving indicators; false (or the
    /// `REDUCED_MOTION` / `FORMLESS_REDUCED_MOTION` env var) shows static
    /// ones instead.
    pub animations: bool,
}
impl Default for TuiCfg {
    fn default() -> Self {
        Self {
            queue_key: "ctrl+enter".into(),
            multi_pane: false,
            notify: "desktop".into(),
            notify_min_secs: 20,
            timestamps: true,
            hyperlinks: "auto".into(),
            theme: "auto".into(),
            colors: HashMap::new(),
            code_line_numbers: false,
            inline_images: "auto".into(),
            pane_widths: vec![22, 26],
            screen_reader: false,
            on_start: "last".into(),
            animations: true,
            language: "auto".into(),
        }
    }
}

/// Automatic Session naming by a small embedded model (see `namer`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NamingCfg {
    /// Rename "Untitled" Sessions after their first Run.
    pub enabled: bool,
    /// Embedded model id ("qwen2.5-0.5b" or "lfm2.5-350m"); empty uses
    /// the heuristic title only.
    pub model: String,
    /// Fetch the model into the data dir on first use (once, in the
    /// background). Off: install with `formless naming pull`.
    pub auto_download: bool,
    /// Give up on a single inference after this long and use the
    /// heuristic title instead.
    pub timeout_secs: u64,
}
impl Default for NamingCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            model: crate::namer::QWEN2_5_0_5B.id.into(),
            auto_download: true,
            timeout_secs: 30,
        }
    }
}
impl NamingCfg {
    pub fn spec(&self) -> Option<crate::namer::ModelSpec> {
        crate::namer::spec(&self.model)
    }
}

impl Config {
    /// The config to carry to another machine (`export-all`, SPEC §15):
    /// everything, with values that can hold credentials blanked —
    /// providers' `extra_headers` and auth secrets/tokens, MCP servers'
    /// `env` values and `headers`. Returns it with the blanked paths.
    pub fn portable(&self) -> (serde_json::Value, Vec<String>) {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        let mut blanked = Vec::new();
        let mut blank_map = |obj: &mut serde_json::Value, at: String| {
            if let Some(m) = obj.as_object_mut() {
                for (k, val) in m.iter_mut() {
                    if val.as_str().is_some_and(|s| !s.is_empty()) {
                        *val = serde_json::json!("");
                        blanked.push(format!("{at}.{k}"));
                    }
                }
            }
        };
        let provider_secrets =
            |p: &mut serde_json::Value,
             at: &str,
             blank: &mut dyn FnMut(&mut serde_json::Value, String)| {
                blank(&mut p["extra_headers"], format!("{at}.extra_headers"));
                if let Some(auth) = p["auth"].as_object_mut() {
                    let mut secret = serde_json::Map::new();
                    for (k, val) in auth.iter_mut() {
                        let lk = k.to_lowercase();
                        if lk.contains("secret") || lk.contains("token") || lk.contains("password")
                        {
                            secret.insert(k.clone(), val.clone());
                        }
                    }
                    let mut secret = serde_json::Value::Object(secret);
                    blank(&mut secret, format!("{at}.auth"));
                    for (k, val) in secret.as_object().unwrap() {
                        auth.insert(k.clone(), val.clone());
                    }
                }
            };
        provider_secrets(&mut v["provider"], "provider", &mut blank_map);
        let names: Vec<String> = v["providers"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        for n in names {
            provider_secrets(
                &mut v["providers"][&n],
                &format!("providers.{n}"),
                &mut blank_map,
            );
        }
        let servers: Vec<String> = v["mcp"]["servers"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        for n in servers {
            blank_map(
                &mut v["mcp"]["servers"][&n]["env"],
                format!("mcp.servers.{n}.env"),
            );
            blank_map(
                &mut v["mcp"]["servers"][&n]["headers"],
                format!("mcp.servers.{n}.headers"),
            );
        }
        (v, blanked)
    }

    /// Read-only settings for remote clients (the Web frontend's Settings
    /// view). Mirrors what the TUI shows; deliberately omits
    /// `extra_headers` and every other field that can carry credentials.
    pub fn settings_summary(&self, paths: &Paths) -> serde_json::Value {
        serde_json::json!({
            "provider": {
                "base_url": self.provider.base_url,
                "model": self.provider.model,
                "reasoning_effort": self.provider.reasoning_effort,
                "api_style": self.provider.api_style,
                "tool_mode": self.provider.tool_mode,
                "auth_mode": self.provider.auth.mode,
                "context_tokens": self.provider.context_tokens,
                "fallback_models": self.provider.fallback_models,
            },
            "tui": {
                "multi_pane": self.tui.multi_pane,
                "queue_key": self.tui.queue_key,
                "notify": self.tui.notify,
                "notify_min_secs": self.tui.notify_min_secs,
                "timestamps": self.tui.timestamps,
                "on_start": self.tui.on_start,
                "screen_reader": self.tui.screen_reader,
                "animations": self.tui.animations,
                "language": self.tui.language,
            },
            // Keys that apply only when the TUI next starts.
            "on_tui_start": ON_TUI_START,
            // What each key means and its default (SPEC §13).
            "docs": setting_docs_json(),
            "limits": self.limits,
            "relay": { "enabled": self.relay.enabled, "url": self.relay.url },
            // The approval posture, read-only here (config.toml or --secure).
            "security": {
                "secure": self.security.secure,
                "trusted_paths": self.security.roots().len(),
                "trusted_hosts": self.security.trusted_hosts.len(),
                "scope_warnings": self.security.network_scope_warnings(),
                "trust_writes": self.security.trust_writes,
                "change": format!(
                    "security.secure in {}, or start with `formless --secure`",
                    paths.config_file().display()
                ),
            },
            "naming": {
                "enabled": self.naming.enabled,
                "model": self.naming.model,
                "installed": self.naming.spec().is_some_and(|m| m.installed(paths)),
            },
            "paths": {
                "config": paths.config_file().display().to_string(),
                "data": paths.data_dir.display().to_string(),
            },
        })
    }

    pub fn load(paths: &Paths) -> Result<Self> {
        Self::load_from(&paths.config_file())
    }
    pub fn load_from(file: &Path) -> Result<Self> {
        let mut cfg = if file.exists() {
            let text = std::fs::read_to_string(file)
                .with_context(|| format!("read {}", file.display()))?;
            toml::from_str(&text).with_context(|| format!("parse {}", file.display()))?
        } else {
            Self::default()
        };
        cfg.apply_env()?;
        Ok(cfg)
    }
    /// The config file alone, without `FORMLESS_*` environment overrides
    /// (what an edit should write back).
    pub fn load_file_only(paths: &Paths) -> Result<Self> {
        let file = paths.config_file();
        if !file.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", file.display()))
    }

    /// Set one dotted key (`tools.inline_limit`) from JSON, validated by
    /// the config types (SPEC §13). Only [`SETTABLE`] keys: never
    /// credentials, endpoints, the relay, MCP servers or hooks. `null`
    /// clears an optional key back to its default.
    pub fn set_key(&mut self, key: &str, value: serde_json::Value) -> Result<()> {
        if !SETTABLE
            .iter()
            .any(|p| key == *p || (p.ends_with('.') && key.starts_with(p)))
        {
            anyhow::bail!(
                "'{key}' cannot be changed this way; edit config.toml on the daemon's machine"
            );
        }
        if let Some((_, allowed)) = CHOICES.iter().find(|(k, _)| *k == key) {
            let ok = value.is_null() || value.as_str().is_some_and(|v| allowed.contains(&v));
            if !ok {
                anyhow::bail!("'{key}' must be one of: {}", allowed.join(", "));
            }
        }
        let mut doc = toml::Value::try_from(&*self)?;
        let parts: Vec<&str> = key.split('.').collect();
        let (leaf, path) = parts.split_last().context("empty key")?;
        let mut table = doc.as_table_mut().context("config is not a table")?;
        for p in path {
            table = table
                .entry(p.to_string())
                .or_insert_with(|| toml::Value::Table(Default::default()))
                .as_table_mut()
                .with_context(|| format!("'{p}' is not a section"))?;
        }
        if value.is_null() {
            table.remove(*leaf);
        } else {
            let v = toml::Value::try_from(&value).context("unsupported value")?;
            table.insert(leaf.to_string(), v.clone());
        }
        let next: Config = doc
            .try_into()
            .with_context(|| format!("invalid value for '{key}'"))?;
        // A typo'd key would be dropped silently: make sure it took.
        if !value.is_null() {
            let mut check = toml::Value::try_from(&next)?;
            for p in &parts {
                check = check
                    .get(p)
                    .cloned()
                    .with_context(|| format!("unknown setting '{key}'"))?;
            }
        }
        *self = next;
        Ok(())
    }

    /// Write the config (SPEC §13): merged into the existing file so its
    /// comments, order and keys this build doesn't know survive, and
    /// replaced atomically so a crash mid-save leaves the old file.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let file = paths.config_file();
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let fresh = toml::to_string_pretty(self)?;
        let text = match std::fs::read_to_string(&file) {
            Ok(old) => match merge_toml(&old, &fresh) {
                Some(merged) => merged,
                // Can't merge into it (it doesn't parse for this build):
                // keep a copy before replacing it, never lose it (#1100).
                None => {
                    let bak = file.with_extension("toml.bak");
                    write_atomic(&bak, old.as_bytes())
                        .with_context(|| format!("back up {}", file.display()))?;
                    tracing::warn!(
                        "{} could not be merged; saved a copy as {}",
                        file.display(),
                        bak.display()
                    );
                    fresh
                }
            },
            Err(_) => fresh,
        };
        write_atomic(&file, text.as_bytes())
            .with_context(|| format!("write {}", file.display()))?;
        // A saved config supersedes any unfinished TUI setup, including when
        // setup completed through the CLI or a daemon setting update.
        if let Err(e) = crate::setup_draft::discard(paths) {
            tracing::warn!("could not remove superseded setup draft: {e}");
        }
        Ok(())
    }
    fn apply_env(&mut self) -> Result<()> {
        if let Some(secure) = env_flag("FORMLESS_SECURE")? {
            self.security.secure = secure;
        }
        if let Ok(v) = std::env::var("FORMLESS_BASE_URL") {
            self.provider.base_url = v;
        }
        if let Ok(v) = std::env::var("FORMLESS_MODEL") {
            self.provider.model = v;
        }
        if let Ok(v) = std::env::var("FORMLESS_IDLE_GRACE_SECS") {
            if let Ok(n) = v.parse() {
                self.daemon.idle_grace_secs = n;
            }
        }
        Ok(())
    }
}

// --- API key storage (SPEC §4.2): OS keychain, 0600 file fallback. ---

/// Write a secret file atomically, private from the first byte: a temp
/// file created with mode 0600 (never wider, whatever the umask), synced,
/// then renamed over `file`.
pub fn write_secret_file(file: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Unique per write, so two saves never share (or unlink) a temp (#849).
    let tmp = file.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let written = (|| -> Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, file).with_context(|| format!("replace {}", file.display()))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Read a secret file; one that group or others can read is tightened
/// to 0600 first, with a warning (SPEC §4.2). A symlink is not followed:
/// a secret file is a plain file in the config folder (#849).
pub fn read_secret_file(file: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(file).ok()?;
    if meta.file_type().is_symlink() {
        tracing::warn!(
            "{} is a symlink; secret files must be plain files",
            file.display()
        );
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            match std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)) {
                Ok(()) => tracing::warn!(
                    "{} was readable by others (mode {:o}); tightened to 0600",
                    file.display(),
                    mode & 0o777
                ),
                Err(e) => tracing::warn!(
                    "{} is readable by others (mode {:o}) and could not be tightened: {e}",
                    file.display(),
                    mode & 0o777
                ),
            }
        }
    }
    std::fs::read_to_string(file).ok()
}

/// Remove secret temp files a crashed write left behind (`*.tmp-*` in the
/// config folder); run at daemon start (#849).
pub fn sweep_secret_temps(paths: &Paths) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(&paths.config_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().to_string();
        let ours = name.contains(".tmp-") && !name.starts_with('.');
        // Old enough that no write in progress owns it.
        let stale = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(60));
        if ours
            && stale
            && e.file_type().is_ok_and(|t| t.is_file())
            && std::fs::remove_file(e.path()).is_ok()
        {
            n += 1;
        }
    }
    n
}

/// Every file in the config folder that holds a secret: the API key and
/// relay fallbacks, OAuth tokens and named secrets (provider keys, MCP
/// logins), for `doctor`'s permission check.
pub fn secret_files(paths: &Paths) -> Vec<std::path::PathBuf> {
    let mut out = vec![paths.key_fallback_file(), paths.relay_secret_file()];
    for e in std::fs::read_dir(&paths.config_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("oauth_tokens")
            || name.starts_with("provider-key-")
            || name.starts_with("mcp-oauth-")
        {
            out.push(e.path());
        }
    }
    out.retain(|p| p.exists());
    out
}

const KEYRING_SERVICE: &str = "formless";

fn keyring_user(paths: &Paths) -> String {
    format!("provider-api-key-{}", paths.profile)
}

pub fn save_api_key(paths: &Paths, key: &str) -> Result<()> {
    // Always write the 0600 fallback file as well as the OS keychain:
    // keychain entries created by an unsigned dev binary can become
    // unreadable after a rebuild (ACL is tied to the binary identity),
    // which would otherwise force re-entering the key on every start.
    let file = paths.key_fallback_file();
    let file_result = (|| -> Result<()> { write_secret_file(&file, key) })();
    let keyring_ok = keyring::Entry::new(KEYRING_SERVICE, &keyring_user(paths))
        .map(|entry| entry.set_password(key).is_ok())
        .unwrap_or(false);
    if file_result.is_ok() || keyring_ok {
        Ok(())
    } else {
        file_result
    }
}

/// The key in the file a `*_FILE` variable names: None when the variable
/// is unset, an error naming the variable and file when it can't be used
/// (missing, unreadable, not UTF-8, empty). A BOM and surrounding
/// whitespace are trimmed (#1083).
pub fn key_from_file_env(var: &str) -> Option<std::result::Result<String, String>> {
    let path = std::env::var_os(var)?;
    let shown = std::path::Path::new(&path).display().to_string();
    Some(match std::fs::read(&path) {
        Err(e) => Err(format!("{var} names {shown}, which can't be read: {e}")),
        Ok(bytes) => match String::from_utf8(bytes) {
            Err(_) => Err(format!("{var} names {shown}, which isn't UTF-8 text")),
            Ok(text) => {
                let key = text.trim_start_matches('\u{feff}').trim();
                if key.is_empty() {
                    Err(format!("{var} names {shown}, which is empty"))
                } else {
                    Ok(key.to_string())
                }
            }
        },
    })
}

/// Where the API key comes from, for `doctor` (never the key itself):
/// ("env" | "file" | "fallback file" | "keychain" | "none", a problem
/// with `FORMLESS_API_KEY_FILE` if it is set but unusable).
pub fn api_key_source(paths: &Paths) -> (&'static str, Option<String>) {
    let file_problem = match key_from_file_env("FORMLESS_API_KEY_FILE") {
        Some(Err(why)) => Some(why),
        _ => None,
    };
    if std::env::var("FORMLESS_API_KEY").is_ok_and(|v| !v.is_empty()) {
        return ("env", file_problem);
    }
    if matches!(key_from_file_env("FORMLESS_API_KEY_FILE"), Some(Ok(_))) {
        return ("file", None);
    }
    if read_secret_file(&paths.key_fallback_file()).is_some_and(|s| !s.trim().is_empty()) {
        return ("fallback file", file_problem);
    }
    let keychain = keyring::Entry::new(KEYRING_SERVICE, &keyring_user(paths))
        .ok()
        .and_then(|e| e.get_password().ok())
        .is_some_and(|k| !k.is_empty());
    (if keychain { "keychain" } else { "none" }, file_problem)
}

pub fn load_api_key(paths: &Paths) -> Option<String> {
    if let Ok(v) = std::env::var("FORMLESS_API_KEY") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    // A file holding the key, e.g. a Docker / Kubernetes secret mounted
    // at /run/secrets/… (docs/docker.md).
    match key_from_file_env("FORMLESS_API_KEY_FILE") {
        Some(Ok(k)) => return Some(k),
        Some(Err(why)) => tracing::warn!("{why}; trying the other key sources"),
        None => {}
    }
    if let Some(k) = read_secret_file(&paths.key_fallback_file())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(k);
    }
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &keyring_user(paths)) {
        if let Ok(k) = entry.get_password() {
            let key = k.trim();
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    None
}

/// The API key of a named provider (SPEC §4.5): `FORMLESS_API_KEY_<NAME>`
/// (upper-case, non-alphanumerics as `_`), else the file that
/// `FORMLESS_API_KEY_<NAME>_FILE` names, else the keychain/0600 secret.
pub fn provider_key(paths: &Paths, name: &str) -> Option<String> {
    let env = format!(
        "FORMLESS_API_KEY_{}",
        name.chars()
            .map(|c| if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            })
            .collect::<String>()
    );
    if let Ok(v) = std::env::var(&env) {
        if !v.is_empty() {
            return Some(v);
        }
    }
    match key_from_file_env(&format!("{env}_FILE")) {
        Some(Ok(k)) => return Some(k),
        Some(Err(why)) => tracing::warn!("{why}; trying the stored key"),
        None => {}
    }
    crate::auth::load_named_secret(paths, &format!("provider-key-{name}"))
        .and_then(|v| v.as_str().map(str::to_string))
}

pub fn save_provider_key(paths: &Paths, name: &str, key: &str) -> Result<()> {
    crate::auth::save_named_secret(
        paths,
        &format!("provider-key-{name}"),
        &serde_json::json!(key),
    )
}

pub fn delete_provider_key(paths: &Paths, name: &str) {
    crate::auth::delete_named_secret(paths, &format!("provider-key-{name}"));
}

/// Full reset (SPEC §15): keychain entry, config, and the entire data dir.
/// Caller must stop the daemon first.
pub fn reset_all(paths: &Paths) -> Result<Vec<String>> {
    let gate = crate::profile_lock::ProfileLock::maintenance(paths, std::time::Duration::ZERO)?;
    let guard = gate.freeze()?;
    reset_all_guarded(paths, &guard)
}

/// Recursive removal requires a frozen namespace covering every affected path.
pub fn reset_all_guarded(
    paths: &Paths,
    guard: &crate::profile_lock::MaintenanceGuard,
) -> Result<Vec<String>> {
    if !guard.covers(paths)? {
        anyhow::bail!("profile is outside the frozen cleanup namespace");
    }
    if paths.profile == "default" {
        let named: Vec<_> = paths
            .list_profiles()
            .into_iter()
            .filter(|p| p.profile != "default")
            .map(|p| p.profile)
            .collect();
        if !named.is_empty() {
            anyhow::bail!("named profiles are preserved; remove them with `formless profiles rm NAME` or use `formless uninstall` first: {}", named.join(", "));
        }
    }
    let warnings = delete_secrets(paths);
    for dir in [&paths.data_dir, &paths.config_dir] {
        if dir.exists() {
            std::fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))?;
        }
    }
    // Unix sockets for long data paths live elsewhere (SPEC §9.1).
    // Windows named pipes have no filesystem entry to remove.
    #[cfg(unix)]
    {
        let sock = paths.socket_path();
        if sock.parent() != Some(paths.data_dir.as_path()) {
            let _ = std::fs::remove_file(&sock);
            if let Some(dir) = sock.parent() {
                let _ = std::fs::remove_dir(dir); // only when empty
            }
        }
    }
    Ok(warnings)
}

/// `fresh` (a whole serialized config) merged into the `old` file text:
/// changed values are replaced in place (keeping their comments), new keys
/// added, removed settings deleted, and keys the config type ignores
/// (comments' neighbours, a newer build's keys) kept. None when `old`
/// doesn't parse, so the caller writes `fresh`.
fn merge_toml(old: &str, fresh: &str) -> Option<String> {
    let mut doc: toml_edit::DocumentMut = old.parse().ok()?;
    let new: toml_edit::DocumentMut = fresh.parse().ok()?;
    // What `old` means to this build, to tell ignored keys from settings.
    // Compared as parsed values, so map order never matters (#1100).
    let meaning = |d: &toml_edit::DocumentMut| -> Option<serde_json::Value> {
        let c: Config = toml::from_str(&d.to_string()).ok()?;
        serde_json::to_value(&c).ok()
    };
    let base = meaning(&doc)?;
    let target = meaning(&new)?;
    let mut path: Vec<String> = Vec::new();
    let mut inserted: Vec<Vec<String>> = Vec::new();
    merge_table(
        doc.as_table_mut(),
        new.as_table(),
        &mut path,
        &mut inserted,
        &mut |p: &[String]| {
            // Would dropping this old-only key change the parsed config?
            let mut probe: toml_edit::DocumentMut = old.parse().ok()?;
            remove_path(probe.as_item_mut(), p)?;
            Some(meaning(&probe)? != base)
        },
    );
    // Keys added only because the serializer writes every default are
    // left out again, so a hand-written file stays as short as it was.
    for p in inserted.iter().rev() {
        let mut probe = doc.clone();
        if remove_path(probe.as_item_mut(), p).is_some()
            && meaning(&probe).as_ref() == Some(&target)
        {
            doc = probe;
        }
    }
    Some(doc.to_string())
}

/// Remove the key at `path` (table names or array-of-tables indexes,
/// then the key).
fn remove_path(item: &mut toml_edit::Item, path: &[String]) -> Option<()> {
    match path {
        [] => None,
        [last] => item.as_table_like_mut()?.remove(last).map(|_| ()),
        [first, rest @ ..] => {
            if let toml_edit::Item::ArrayOfTables(a) = item {
                let entry = a.get_mut(first.parse().ok()?)?;
                return match rest {
                    [last] => entry.remove(last).map(|_| ()),
                    [next, more @ ..] => remove_path(entry.get_mut(next)?, more),
                    [] => None,
                };
            }
            remove_path(item.get_mut(first)?, rest)
        }
    }
}

/// [`merge_table`] for an inline table (`provider = { … }`), which a
/// fresh serialization writes as a plain table.
fn merge_inline(
    old: &mut toml_edit::InlineTable,
    new: &toml_edit::InlineTable,
    path: &mut Vec<String>,
    known: &mut dyn FnMut(&[String]) -> Option<bool>,
) {
    for (k, nv) in new.iter() {
        path.push(k.to_string());
        match old.get_mut(k) {
            Some(toml_edit::Value::InlineTable(oi)) if nv.is_inline_table() => {
                merge_inline(oi, nv.as_inline_table().expect("checked"), path, known);
            }
            Some(ov) if ov.to_string().trim() != nv.to_string().trim() => {
                let decor = ov.decor().clone();
                *ov = nv.clone();
                *ov.decor_mut() = decor;
            }
            Some(_) => {}
            None => {
                old.insert(k, nv.clone());
            }
        }
        path.pop();
    }
    let gone: Vec<String> = old
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !new.contains_key(k))
        .collect();
    for k in gone {
        path.push(k.clone());
        if known(path).unwrap_or(true) {
            old.remove(&k);
        }
        path.pop();
    }
}

fn merge_table(
    old: &mut toml_edit::Table,
    new: &toml_edit::Table,
    path: &mut Vec<String>,
    inserted: &mut Vec<Vec<String>>,
    known: &mut dyn FnMut(&[String]) -> Option<bool>,
) {
    for (k, nv) in new.iter() {
        path.push(k.to_string());
        match (old.get_mut(k), nv) {
            (Some(toml_edit::Item::Table(ot)), toml_edit::Item::Table(nt)) => {
                merge_table(ot, nt, path, inserted, known);
            }
            // `[[hooks]]`: entry by entry, keeping each one's comments and
            // unknown keys (#1100).
            (Some(toml_edit::Item::ArrayOfTables(oa)), toml_edit::Item::ArrayOfTables(na)) => {
                for (i, nt) in na.iter().enumerate() {
                    path.push(i.to_string());
                    match oa.get_mut(i) {
                        Some(ot) => merge_table(ot, nt, path, inserted, known),
                        None => oa.push(nt.clone()),
                    }
                    path.pop();
                }
                while oa.len() > na.len() {
                    oa.remove(oa.len() - 1);
                }
            }
            (Some(toml_edit::Item::Value(toml_edit::Value::InlineTable(oi))), _)
                if nv.is_table_like() =>
            {
                let ni = match nv {
                    toml_edit::Item::Table(t) => t.clone().into_inline_table(),
                    toml_edit::Item::Value(toml_edit::Value::InlineTable(t)) => t.clone(),
                    _ => unreachable!("table-like"),
                };
                merge_inline(oi, &ni, path, known);
            }
            (Some(ov), _) => {
                // Same value: leave the old item (and its decor) alone.
                if ov.to_string().trim() != nv.to_string().trim() {
                    match (ov.as_value_mut(), nv.as_value()) {
                        (Some(o), Some(n)) => {
                            let decor = o.decor().clone();
                            *o = n.clone();
                            *o.decor_mut() = decor;
                        }
                        _ => *ov = nv.clone(),
                    }
                }
            }
            (None, _) => {
                old.insert(k, nv.clone());
                inserted.push(path.clone());
            }
        }
        path.pop();
    }
    let gone: Vec<String> = old
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !new.contains_key(k))
        .collect();
    for k in gone {
        path.push(k.clone());
        // Removed settings go; keys this build ignores stay.
        if known(path).unwrap_or(true) {
            old.remove(&k);
        }
        path.pop();
    }
}

/// Replace `file` with `bytes` all at once: a temporary file next to it,
/// flushed to disk, renamed over it, and the folder flushed (unix), so a
/// crash leaves the old file or the new one, never half of either.
pub fn write_atomic(file: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    // A symlinked config (dotfiles) is written where it points, never
    // replaced by a plain file (#1101).
    let target;
    let file = if std::fs::symlink_metadata(file).is_ok_and(|m| m.file_type().is_symlink()) {
        target = std::fs::canonicalize(file)
            .with_context(|| format!("{} is a symlink that doesn't resolve", file.display()))?;
        target.as_path()
    } else {
        file
    };
    let dir = file.parent().context("no parent folder")?;
    // Unique per save, so two saves never share a temporary file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = dir.join(format!(
        ".{}.tmp-{}-{}",
        file.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Ok(meta) = std::fs::metadata(file) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Every credential a profile keeps outside its folders (keychain and
/// secret files): the API key, OAuth tokens, the relay secret, named
/// providers' keys and MCP servers' logins.
///
/// Named secrets are found from the config and from their files in the
/// config folder, so ones whose provider or server was renamed or removed
/// go too (#1080). Returns warnings: when `config.toml` doesn't parse,
/// keychain-only named entries can't be listed.
pub fn delete_secrets(paths: &Paths) -> Vec<String> {
    let mut warnings = Vec::new();
    let cfg = match Config::load_file_only(paths) {
        Ok(c) => Some(c),
        Err(e) if paths.config_file().exists() => {
            warnings.push(format!(
                "config.toml could not be read ({e:#}), so keychain entries of named providers and MCP servers that have no file could not be listed"
            ));
            None
        }
        Err(_) => None,
    };
    if let Some(cfg) = &cfg {
        for name in cfg.providers.keys() {
            delete_provider_key(paths, name);
        }
        for server in cfg.mcp.servers.keys() {
            crate::mcp_auth::logout(paths, server);
        }
    }
    // Every named secret with a file, whatever the config says now.
    for entry in std::fs::read_dir(&paths.config_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let file = entry.file_name().to_string_lossy().to_string();
        if let Some(name) = file.strip_suffix(".json") {
            if name.starts_with("provider-key-") || name.starts_with("mcp-oauth-") {
                crate::auth::delete_named_secret(paths, name);
            }
        }
    }
    delete_api_key(paths);
    crate::auth::delete_all_oauth_tokens(paths, cfg.as_ref());
    delete_relay_secret(paths);
    warnings
}

// --- Relay pairing secret: 32 random bytes, base64url. It *is* the
// AES-256-GCM key (decoded) and grants full remote control, so it is
// stored like the API key (keychain + 0600 file), never in config.toml. ---

fn relay_keyring_user(paths: &Paths) -> String {
    format!("relay-secret-{}", paths.profile)
}

pub fn generate_relay_secret() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn save_relay_secret(paths: &Paths, secret: &str) -> Result<()> {
    let file = paths.relay_secret_file();
    let file_result = (|| -> Result<()> { write_secret_file(&file, secret) })();
    let keyring_ok = keyring::Entry::new(KEYRING_SERVICE, &relay_keyring_user(paths))
        .map(|entry| entry.set_password(secret).is_ok())
        .unwrap_or(false);
    if file_result.is_ok() || keyring_ok {
        Ok(())
    } else {
        file_result
    }
}

pub fn load_relay_secret(paths: &Paths) -> Option<String> {
    if let Ok(v) = std::env::var("FORMLESS_RELAY_SECRET") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Some(k) = read_secret_file(&paths.relay_secret_file())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(k);
    }
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &relay_keyring_user(paths)) {
        if let Ok(k) = entry.get_password() {
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    None
}

/// Load the pairing secret, generating and saving one on first use.
pub fn ensure_relay_secret(paths: &Paths) -> Result<String> {
    if let Some(s) = load_relay_secret(paths) {
        return Ok(s);
    }
    let s = generate_relay_secret();
    save_relay_secret(paths, &s)?;
    Ok(s)
}

pub fn delete_relay_secret(paths: &Paths) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &relay_keyring_user(paths)) {
        let _ = entry.delete_credential();
    }
    let _ = std::fs::remove_file(paths.relay_secret_file());
}

pub fn delete_api_key(paths: &Paths) {
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &keyring_user(paths)) {
        let _ = entry.delete_credential();
    }
    let _ = std::fs::remove_file(paths.key_fallback_file());
}

#[cfg(test)]
mod secret_file_tests {
    use super::*;

    /// Concurrent saves leave one whole value and no temp; a failed write
    /// leaves no temp; a symlink is not read through (#849).
    #[test]
    fn secret_writes_are_safe_under_concurrency_and_failure() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let dir = std::env::temp_dir().join(format!("fl-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("api_key");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    write_secret_file(&file, &format!("value-{i}").repeat(200))
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap().unwrap();
        }
        let v = read_secret_file(&file).unwrap();
        let first = &v[..7];
        assert_eq!(v, first.repeat(200), "one whole value");
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
        // Writing where a directory stands fails, and leaves no temp.
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(blocked.join("x")).unwrap();
        assert!(write_secret_file(&blocked, "s").is_err());
        assert_eq!(
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
                .count(),
            0
        );
        #[cfg(unix)]
        {
            let target = dir.join("elsewhere");
            std::fs::write(&target, "secret").unwrap();
            let link = dir.join("linked");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert_eq!(read_secret_file(&link), None);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod save_tests {
    use super::*;

    /// `[[hooks]]` entries and inline tables keep their comments and
    /// unknown keys through an unrelated change; a symlinked config is
    /// written through the link; an unmergeable file is backed up (#1100, #1101).
    #[test]
    fn hooks_inline_tables_and_symlinks_survive_saves() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let base = std::env::temp_dir().join(format!("fl-cfghooks-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        let original = "[provider]\n\
model = \"m\"\n\
\n\
[providers]\n\
# my local box\n\
local = { base_url = \"http://localhost:11434/v1\", model = \"llama\", someday = 1 }\n\
\n\
# audit every net call\n\
[[hooks]]\n\
event = \"pre_tool\"\n\
# only net Tools\n\
capability = \"net\"\n\
command = [\"logger\"]\n\
later_key = \"kept\"\n";
        // The real file lives elsewhere; config.toml links to it.
        let real = base.join("dotfiles.toml");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&real, original).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, paths.config_file()).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&real, paths.config_file()).unwrap();
        let mut cfg = Config::load_file_only(&paths).unwrap();
        cfg.provider.model = "other".into();
        cfg.save(&paths).unwrap();
        #[cfg(unix)]
        assert!(std::fs::symlink_metadata(paths.config_file())
            .unwrap()
            .file_type()
            .is_symlink());
        let text = std::fs::read_to_string(paths.config_file()).unwrap();
        for kept in [
            "# my local box",
            "someday = 1",
            "# audit every net call",
            "# only net Tools",
            "later_key = \"kept\"",
            "model = \"other\"",
        ] {
            assert!(text.contains(kept), "{kept} missing:\n{text}");
        }
        // A file this build can't read is backed up before it's replaced.
        std::fs::write(paths.config_file(), "[provider\nbroken").unwrap();
        cfg.save(&paths).unwrap();
        let bak = paths.config_file().with_extension("toml.bak");
        assert!(std::fs::read_to_string(bak).unwrap().contains("broken"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn saving_keeps_comments_order_and_unknown_keys() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let base = std::env::temp_dir().join(format!("fl-cfgsave-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        let original = "# My Formless setup — keep this note\n\
[provider]\n\
# the endpoint I use\n\
base_url = \"https://openrouter.ai/api/v1\"\n\
model = \"old-model\"\n\
reasoning_effort = \"high\"\n\
future_option = \"from a newer build\"\n\
\n\
[providers.local]\n\
base_url = \"http://localhost:11434/v1\"\n";
        std::fs::write(paths.config_file(), original).unwrap();
        let mut cfg = Config::load_file_only(&paths).unwrap();
        cfg.provider.model = "new-model".into();
        cfg.provider.reasoning_effort = None;
        cfg.providers.remove("local");
        cfg.save(&paths).unwrap();
        let text = std::fs::read_to_string(paths.config_file()).unwrap();
        assert!(
            text.starts_with("# My Formless setup — keep this note"),
            "{text}"
        );
        assert!(
            text.contains("# the endpoint I use\nbase_url"),
            "comment stays with its key:\n{text}"
        );
        assert!(text.contains("model = \"new-model\""));
        assert!(
            text.contains("future_option = \"from a newer build\""),
            "unknown key kept"
        );
        assert!(
            !text.contains("reasoning_effort"),
            "a cleared setting is removed"
        );
        assert!(
            !text.contains("localhost:11434"),
            "a removed provider is removed"
        );
        // It still loads, and an unchanged save changes nothing.
        let again = Config::load_file_only(&paths).unwrap();
        assert_eq!(again.provider.model, "new-model");
        again.save(&paths).unwrap();
        assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), text);
        // No temporary file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&paths.config_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
