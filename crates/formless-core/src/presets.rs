//! Provider presets (SPEC §4.2): the endpoints new users would otherwise
//! have to know, for the setup wizard, Settings → Providers,
//! `formless providers add --preset` and the harness `providers.upsert`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    /// What users type (`--preset openrouter`).
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    /// "chat" or "responses" (`provider.api_style`).
    pub api_style: &'static str,
    /// Environment variable that commonly holds its key.
    pub key_env: Option<&'static str>,
    /// Where to create a key.
    pub key_url: Option<&'static str>,
    /// Pre-selected in the model picker when the endpoint lists it.
    pub default_model: &'static str,
    /// A server on this machine: no API key, and reachability is checked.
    pub local: bool,
}

const fn preset(
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    key_env: Option<&'static str>,
    key_url: Option<&'static str>,
    default_model: &'static str,
    local: bool,
) -> Preset {
    Preset {
        id,
        name,
        base_url,
        api_style: "chat",
        key_env,
        key_url,
        default_model,
        local,
    }
}

pub const PRESETS: &[Preset] = &[
    preset(
        "openai",
        "OpenAI",
        "https://api.openai.com/v1",
        Some("OPENAI_API_KEY"),
        Some("https://platform.openai.com/api-keys"),
        "gpt-5",
        false,
    ),
    preset(
        "anthropic",
        "Anthropic (OpenAI-compatible endpoint)",
        "https://api.anthropic.com/v1",
        Some("ANTHROPIC_API_KEY"),
        Some("https://console.anthropic.com/settings/keys"),
        "claude-sonnet-5-5",
        false,
    ),
    preset(
        "openrouter",
        "OpenRouter",
        "https://openrouter.ai/api/v1",
        Some("OPENROUTER_API_KEY"),
        Some("https://openrouter.ai/keys"),
        "anthropic/claude-sonnet-5.5",
        false,
    ),
    preset(
        "gemini",
        "Google Gemini (OpenAI-compatible)",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        Some("GEMINI_API_KEY"),
        Some("https://aistudio.google.com/apikey"),
        "gemini-2.5-pro",
        false,
    ),
    preset(
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        Some("GROQ_API_KEY"),
        Some("https://console.groq.com/keys"),
        "llama-3.3-70b-versatile",
        false,
    ),
    preset(
        "ollama",
        "Ollama (local)",
        "http://localhost:11434/v1",
        None,
        None,
        "",
        true,
    ),
    preset(
        "lmstudio",
        "LM Studio (local)",
        "http://localhost:1234/v1",
        None,
        None,
        "",
        true,
    ),
];

pub fn find(id: &str) -> Option<&'static Preset> {
    let id = id.trim().to_ascii_lowercase();
    PRESETS.iter().find(|p| p.id == id)
}

/// The preset whose endpoint is `base_url`, if any.
pub fn for_url(base_url: &str) -> Option<&'static Preset> {
    let url = base_url.trim().trim_end_matches('/');
    PRESETS.iter().find(|p| p.base_url == url)
}

/// A base URL from either a URL or a preset id.
pub fn resolve_base_url(value: &str) -> Option<String> {
    let v = value.trim();
    if v.starts_with("http://") || v.starts_with("https://") {
        return Some(v.trim_end_matches('/').to_string());
    }
    find(v).map(|p| p.base_url.to_string())
}

/// The latest Azure OpenAI data-plane API version this build knows.
pub const AZURE_API_VERSION: &str = "2024-10-21";

/// Azure OpenAI (SPEC §4.2): a deployment's base URL, its `api-version`
/// query parameter and the `api-key` header, from the resource name (or
/// its full `https://….openai.azure.com` endpoint) and deployment.
pub fn azure(
    resource: &str,
    deployment: &str,
    api_version: Option<&str>,
) -> anyhow::Result<(String, std::collections::BTreeMap<String, String>, &'static str)> {
    let resource = resource.trim().trim_end_matches('/');
    let deployment = deployment.trim();
    if resource.is_empty() || deployment.is_empty() {
        anyhow::bail!("Azure OpenAI needs the resource name and the deployment name");
    }
    let host = if resource.starts_with("https://") {
        resource.to_string()
    } else {
        format!("https://{resource}.openai.azure.com")
    };
    let base = format!("{host}/openai/deployments/{deployment}");
    let mut q = std::collections::BTreeMap::new();
    q.insert(
        "api-version".to_string(),
        api_version.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(AZURE_API_VERSION).to_string(),
    );
    Ok((base, q, "api-key"))
}

/// The preset's key from its environment variable, when set.
pub fn env_key(p: &Preset) -> Option<(&'static str, String)> {
    let var = p.key_env?;
    std::env::var(var)
        .ok()
        .filter(|k| !k.trim().is_empty())
        .map(|k| (var, k.trim().to_string()))
}

#[cfg(test)]
mod azure_tests {
    #[test]
    fn azure_builds_the_deployment_url_version_and_key_header() {
        let (base, q, header) = super::azure("contoso", "gpt4o", None).unwrap();
        assert_eq!(base, "https://contoso.openai.azure.com/openai/deployments/gpt4o");
        assert_eq!(q["api-version"], super::AZURE_API_VERSION);
        assert_eq!(header, "api-key");
        let (base, q, _) =
            super::azure("https://x.openai.azure.com/", "d", Some("2025-01-01")).unwrap();
        assert_eq!(base, "https://x.openai.azure.com/openai/deployments/d");
        assert_eq!(q["api-version"], "2025-01-01");
        assert!(super::azure("", "d", None).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_by_id_and_url() {
        assert_eq!(
            find("OpenRouter").unwrap().base_url,
            "https://openrouter.ai/api/v1"
        );
        assert!(find("ollama").unwrap().local);
        assert_eq!(for_url("http://localhost:11434/v1/").unwrap().id, "ollama");
        assert_eq!(
            resolve_base_url("groq").as_deref(),
            Some("https://api.groq.com/openai/v1")
        );
        assert_eq!(
            resolve_base_url("http://x:1/v1/").as_deref(),
            Some("http://x:1/v1")
        );
        assert_eq!(resolve_base_url("nope"), None);
        for p in PRESETS {
            assert!(p.local == p.key_env.is_none(), "{}", p.id);
            assert!(p.base_url.starts_with("http"));
        }
    }
}
