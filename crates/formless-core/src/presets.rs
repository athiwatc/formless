//! Provider presets (SPEC §4.2): the endpoints new users would otherwise
//! have to know, for the setup wizard, Settings → Providers,
//! `formless providers add --preset` and the harness `providers.upsert`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    /// What users type (`--preset openrouter`).
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    /// "chat", "responses" or "anthropic" (`provider.api_style`).
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
    Preset {
        api_style: "anthropic",
        ..preset(
            "anthropic",
            "Anthropic (native Messages API)",
            "https://api.anthropic.com/v1",
            Some("ANTHROPIC_API_KEY"),
            Some("https://console.anthropic.com/settings/keys"),
            "claude-sonnet-5-5",
            false,
        )
    },
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

/// A URL compared loosely: case, trailing `/` and a trailing `/v1`
/// don't matter (`https://api.anthropic.com` is the Anthropic preset).
fn url_key(url: &str) -> String {
    let u = url.trim().trim_end_matches('/').to_ascii_lowercase();
    let u = if let Some(rest) = u
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| u.strip_prefix("http://[::1]:"))
        .or_else(|| u.strip_prefix("http://0.0.0.0:"))
        .or_else(|| u.strip_prefix("http://[::]:"))
    {
        format!("http://localhost:{rest}")
    } else {
        u
    };
    u.strip_suffix("/v1").unwrap_or(&u).to_string()
}

/// The preset whose endpoint is `base_url`, if any.
pub fn for_url(base_url: &str) -> Option<&'static Preset> {
    let url = url_key(base_url);
    PRESETS.iter().find(|p| url_key(p.base_url) == url)
}

pub fn for_provider(provider: &crate::config::ProviderCfg) -> Option<&'static Preset> {
    provider
        .preset
        .as_deref()
        .and_then(find)
        .or_else(|| for_url(&provider.base_url))
}

/// The API style a provider should have after its base URL changes from
/// `old_url` (None: a new provider) to `new_url`, when it should change.
/// A new provider, or one moved to another preset's endpoint, takes that
/// preset's style; one moved off a preset back to an unknown endpoint
/// returns to `chat` only if its style was the old preset's (an explicit
/// choice is kept). None = keep `current`.
pub fn restyle(old_url: Option<&str>, new_url: &str, current: &str) -> Option<&'static str> {
    let new = for_url(new_url);
    let Some(old_url) = old_url else {
        return new.map(|p| p.api_style);
    };
    if url_key(old_url) == url_key(new_url) {
        return None;
    }
    let old = for_url(old_url);
    let style = match (old, new) {
        (_, Some(n)) => n.api_style,
        (Some(o), None) if o.api_style.eq_ignore_ascii_case(current) => "chat",
        _ => return None,
    };
    (!style.eq_ignore_ascii_case(current)).then_some(style)
}

/// A base URL from either a URL or a preset id.
pub fn resolve_base_url(value: &str) -> Option<String> {
    let v = value.trim();
    if let Some((scheme, remainder)) = v.split_once("://") {
        if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
            return Some(
                format!("{}://{}", scheme.to_ascii_lowercase(), remainder)
                    .trim_end_matches('/')
                    .to_string(),
            );
        }
    }
    find(v).map(|p| p.base_url.to_string())
}

/// Azure OpenAI (SPEC §4.2) uses the v1 API unless a dated API version
/// is explicitly supplied for the deployment-scoped route.
pub fn azure(
    resource: &str,
    deployment: &str,
    api_version: Option<&str>,
) -> anyhow::Result<(
    String,
    std::collections::BTreeMap<String, String>,
    &'static str,
)> {
    let deployment = deployment.trim();
    if deployment.is_empty()
        || matches!(deployment, "." | "..")
        || !deployment
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("Azure deployment names must use letters, digits, '-', '_' or '.'");
    }
    let endpoint = azure_endpoint(resource)?;
    let (path, query) = match api_version.map(str::trim) {
        Some("") => anyhow::bail!("--api-version must specify an Azure API version"),
        Some(version) => (
            format!("/openai/deployments/{deployment}"),
            [("api-version".to_owned(), version.to_owned())].into(),
        ),
        None => ("/openai/v1".to_owned(), Default::default()),
    };
    Ok((format!("{endpoint}{path}"), query, "api-key"))
}

fn azure_endpoint(resource: &str) -> anyhow::Result<String> {
    let resource = resource.trim();
    let resource = resource.strip_suffix('/').unwrap_or(resource);
    let invalid = || {
        anyhow::anyhow!(
        "Azure resource must be a resource name, Azure host, or HTTPS endpoint without a path, credentials, query or fragment"
    )
    };
    let label = |part: &str| {
        !part.is_empty()
            && part.len() <= 63
            && !part.starts_with('-')
            && !part.ends_with('-')
            && part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    };
    let origin = if let Some((scheme, host)) = resource.split_once("://") {
        if !scheme.eq_ignore_ascii_case("https")
            || host.is_empty()
            || host.contains(['/', '\\', '@', '?', '#'])
            || host.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(invalid());
        }
        format!("https://{host}")
    } else if label(resource) {
        format!("https://{resource}.openai.azure.com")
    } else {
        let host = resource.to_ascii_lowercase();
        if !host.split('.').all(label)
            || ![
                ".openai.azure.com",
                ".cognitiveservices.azure.com",
                ".services.ai.azure.com",
            ]
            .iter()
            .any(|suffix| host.ends_with(suffix))
        {
            return Err(invalid());
        }
        format!("https://{host}")
    };
    let url = reqwest::Url::parse(&origin).map_err(|_| invalid())?;
    Ok(url.origin().ascii_serialization())
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
    use super::azure;

    #[test]
    fn azure_uses_v1_for_resource_names_hosts_and_endpoints() {
        for (resource, host) in [
            ("contoso", "contoso.openai.azure.com"),
            ("Contoso.OpenAI.Azure.COM", "contoso.openai.azure.com"),
            (
                "contoso.cognitiveservices.azure.com",
                "contoso.cognitiveservices.azure.com",
            ),
            (
                "https://x.services.ai.azure.com/",
                "x.services.ai.azure.com",
            ),
            (
                "https://azure-gateway.example:8443/",
                "azure-gateway.example:8443",
            ),
        ] {
            let (base, query, header) = azure(resource, "deploy_2026.v1", None).unwrap();
            assert_eq!(base, format!("https://{host}/openai/v1"));
            assert!(query.is_empty());
            assert_eq!(header, "api-key");
        }
    }

    #[test]
    fn azure_pins_only_an_explicit_api_version() {
        let (base, query, header) =
            azure("https://x.openai.azure.com/", "gpt-4o", Some("2025-01-01")).unwrap();
        assert_eq!(base, "https://x.openai.azure.com/openai/deployments/gpt-4o");
        assert_eq!(query, [("api-version".into(), "2025-01-01".into())].into());
        assert_eq!(header, "api-key");
        assert!(azure("contoso", "d", Some(" ")).is_err());
    }

    #[test]
    fn azure_rejects_invalid_resources_and_deployments() {
        for resource in [
            "",
            "http://contoso.openai.azure.com",
            "contoso.example",
            "https://",
            "https://x.services.ai.azure.com/api/projects/p",
            "https://x.openai.azure.com/.",
            "https://x.openai.azure.com?api-version=preview",
            "https://x.openai.azure.com/#part",
            "https://@x.openai.azure.com",
        ] {
            assert!(azure(resource, "d", None).is_err(), "{resource}");
        }
        for deployment in ["", ".", "..", "with space", "with/slash", "with%2fescape"] {
            assert!(azure("contoso", deployment, None).is_err(), "{deployment}");
            assert!(azure("contoso", deployment, Some("2025-01-01")).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_schemes_are_case_insensitive_and_only_the_scheme_is_normalized() {
        for (input, expected) in [
            (
                "HTTPS://Case.Example/V1/Keep%2FCase?Token=AbC#Frag",
                "https://Case.Example/V1/Keep%2FCase?Token=AbC#Frag",
            ),
            (
                "  Http://localhost:11434/v1/  ",
                "http://localhost:11434/v1",
            ),
            ("hTtPs://Example.test/Mixed/", "https://Example.test/Mixed"),
        ] {
            assert_eq!(resolve_base_url(input).as_deref(), Some(expected));
        }
        for input in [
            "FTP://example.test",
            "file:///tmp/fixture",
            "data:text/plain,fixture",
            "ws://example.test",
            "https:example.test",
            "ht tp://example.test",
            "unknown-preset",
        ] {
            assert_eq!(resolve_base_url(input), None, "{input}");
        }
        assert_eq!(
            resolve_base_url(" OPENROUTER "),
            resolve_base_url("openrouter")
        );
        assert_eq!(
            for_url(&resolve_base_url("HTTPS://API.ANTHROPIC.COM/v1/").unwrap())
                .unwrap()
                .id,
            "anthropic"
        );
    }

    #[test]
    fn presets_resolve_by_id_and_url() {
        assert_eq!(
            find("OpenRouter").unwrap().base_url,
            "https://openrouter.ai/api/v1"
        );
        assert!(find("ollama").unwrap().local);
        assert_eq!(for_url("http://localhost:11434/v1/").unwrap().id, "ollama");
        assert_eq!(for_url("http://127.0.0.1:11434/v1").unwrap().id, "ollama");
        assert_eq!(for_url("http://[::1]:11434/v1").unwrap().id, "ollama");
        assert_eq!(for_url("http://[::1]:1234/v1").unwrap().id, "lmstudio");
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

    #[test]
    fn restyle_follows_the_endpoint_family() {
        let (oa, an) = ("https://api.openai.com/v1", "https://api.anthropic.com/v1");
        assert_eq!(
            restyle(None, "https://api.anthropic.com", "chat"),
            Some("anthropic")
        );
        assert_eq!(restyle(Some(oa), an, "chat"), Some("anthropic"));
        assert_eq!(restyle(Some(an), oa, "anthropic"), Some("chat"));
        assert_eq!(
            restyle(Some(an), "http://my-proxy:8080/v1", "anthropic"),
            Some("chat")
        );
        // Unrelated edits and explicit choices are kept.
        assert_eq!(
            restyle(Some(an), "https://api.anthropic.com/v1/", "anthropic"),
            None
        );
        assert_eq!(
            restyle(Some("http://a/v1"), "http://b/v1", "responses"),
            None
        );
        assert_eq!(restyle(Some(an), "http://my-proxy/v1", "responses"), None);
    }
}
