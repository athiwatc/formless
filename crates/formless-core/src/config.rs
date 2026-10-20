use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonCfg {
    pub idle_grace_secs: u64,
    pub stall_warning_secs: u64,
    pub log_max_bytes: u64,
    pub log_keep: usize,
}
impl Default for DaemonCfg {
    fn default() -> Self {
        Self {
            idle_grace_secs: 60,
            stall_warning_secs: 300,
            log_max_bytes: 10_000_000,
            log_keep: 5,
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
        }
    }
}
impl ProviderCfg {
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
}
impl Default for ToolsCfg {
    fn default() -> Self {
        Self {
            pending_threshold_secs: 300,
            default_memory_mb: 256,
            max_memory_mb: 4096,
            artifact_preview_chars: 2000,
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
}
impl Default for TuiCfg {
    fn default() -> Self {
        Self {
            queue_key: "ctrl+enter".into(),
        }
    }
}

impl Config {
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
