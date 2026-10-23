use anyhow::{Context, Result};
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

/// The role an old global `provider.model = "auto"` migrates to.
pub const PRIMARY_ROLE: &str = "primary";

/// The name of the built-in provider (`[provider]`).
pub const DEFAULT_PROVIDER: &str = "default";

impl Config {
    /// A provider's settings by name (`default` or unset = `[provider]`).
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
}

impl ModelPrice {
    pub fn cost(&self, prompt: i64, completion: i64, cached: i64) -> f64 {
        let fresh = (prompt - cached).max(0) as f64;
        let cached_rate = self.cached_input.unwrap_or(self.input);
        (fresh * self.input + cached as f64 * cached_rate + completion as f64 * self.output) / 1e6
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
    ("provider.api_style", &["chat", "responses"]),
    ("provider.tool_mode", &["native", "prompt"]),
    ("provider.cache_key", &["session", "off"]),
    ("tui.notify", &["off", "bell", "desktop"]),
    ("tui.hyperlinks", &["auto", "on", "off"]),
    ("tui.theme", &["auto", "dark", "light", "high-contrast"]),
    ("tui.inline_images", &["auto", "off"]),
    ("tui.on_start", &["last", "new", "picker"]),
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
const CREDENTIAL_KEYS: &[&str] = &["provider.extra_headers", "provider.api_key_ref"];

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
    "provider.tool_use_nudge",
    "provider.api_style",
    "provider.tool_mode",
    "provider.vision",
    "provider.prices",
    "daemon.idle_grace_secs",
    "daemon.stall_warning_secs",
    "daemon.keep_alive_for_jobs",
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
impl RelayCfg {
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
            tool_use_nudge: true,
        }
    }
}
impl ProviderCfg {
    pub fn responses(&self) -> bool {
        self.api_style.eq_ignore_ascii_case("responses")
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
    pub fn is_auto_model(model: &str) -> bool {
        model.trim().eq_ignore_ascii_case("auto")
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
            find_tools_max: default_find_tools_max(),
            subagent_max_turns: default_subagent_max_turns(),
            task_min_samples: default_task_min_samples(),
            task_quality_bar: default_task_quality_bar(),
            task_explore_rate: default_task_explore_rate(),
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
            },
            // Keys that apply only when the TUI next starts.
            "on_tui_start": ON_TUI_START,
            // What each key means and its default (SPEC §13).
            "docs": setting_docs_json(),
            "relay": { "enabled": self.relay.enabled, "url": self.relay.url },
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
        if !file.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
        cfg.apply_env();
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

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let file = paths.config_file();
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&file, text).with_context(|| format!("write {}", file.display()))?;
        Ok(())
    }
    fn apply_env(&mut self) {
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
    }
}

// --- API key storage (SPEC §4.2): OS keychain, 0600 file fallback. ---

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
    let file_result = (|| -> Result<()> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, key)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
    let keyring_ok = keyring::Entry::new(KEYRING_SERVICE, &keyring_user(paths))
        .map(|entry| entry.set_password(key).is_ok())
        .unwrap_or(false);
    if file_result.is_ok() || keyring_ok {
        Ok(())
    } else {
        file_result
    }
}

pub fn load_api_key(paths: &Paths) -> Option<String> {
    if let Ok(v) = std::env::var("FORMLESS_API_KEY") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Some(k) = std::fs::read_to_string(paths.key_fallback_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(k);
    }
    if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, &keyring_user(paths)) {
        if let Ok(k) = entry.get_password() {
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    None
}

/// The API key of a named provider (SPEC §4.5): `FORMLESS_API_KEY_<NAME>`
/// (upper-case, non-alphanumerics as `_`), else the keychain/0600 secret.
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
    if let Ok(v) = std::env::var(env) {
        if !v.is_empty() {
            return Some(v);
        }
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
pub fn reset_all(paths: &Paths) -> Result<()> {
    delete_api_key(paths);
    crate::auth::delete_oauth_tokens(paths);
    delete_relay_secret(paths);
    for dir in [&paths.data_dir, &paths.config_dir] {
        if dir.exists() {
            std::fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))?;
        }
    }
    Ok(())
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
    let file_result = (|| -> Result<()> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, secret)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
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
    if let Some(k) = std::fs::read_to_string(paths.relay_secret_file())
        .ok()
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
