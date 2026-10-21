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
    pub compaction: CompactionCfg,
    pub tui: TuiCfg,
    pub naming: NamingCfg,
    pub artifacts: ArtifactsCfg,
    pub mcp: McpCfg,
    pub skills: SkillsCfg,
    /// `[[hooks]]` (SPEC §9.8): user policy and automation around Tool
    /// calls and Runs.
    pub hooks: Vec<HookCfg>,
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
    /// Seconds without any bytes from an open provider stream before
    /// clients are told it looks stalled (0 = never).
    pub stall_notice_secs: u64,
    /// Re-send the request automatically when no first byte arrives within
    /// this many seconds (0 = off; the user can always `/resend`).
    pub first_byte_timeout_secs: u64,
    /// "session" (default): send a per-Session `prompt_cache_key` so the
    /// endpoint keeps one Session on one prompt cache; "off": never send it.
    pub cache_key: String,
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
            stall_notice_secs: 120,
            first_byte_timeout_secs: 0,
            max_backoff_secs: 30,
            cache_key: "session".into(),
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
}
impl Default for TuiCfg {
    fn default() -> Self {
        Self {
            queue_key: "ctrl+enter".into(),
            multi_pane: false,
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
            },
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
