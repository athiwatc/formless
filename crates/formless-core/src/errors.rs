//! Actionable provider errors (SPEC §4.4): sort a raw provider failure
//! into a category with a short explanation and the next step, for
//! clients (`RunPaused.explained`) and `formless doctor`.

use crate::config::Config;
use formless_protocol::ErrorExplained;

/// What the explanation can name: the provider's endpoint and model.
pub struct ErrorContext<'a> {
    pub base_url: &'a str,
    pub model: &'a str,
    /// Fallback models or Roles the user could switch to.
    pub alternatives: Vec<String>,
}

impl<'a> ErrorContext<'a> {
    pub fn from_config(cfg: &'a Config, model: &'a str) -> Self {
        let mut alternatives = cfg.provider.fallback_models.clone();
        alternatives.extend(cfg.roles.keys().map(|r| format!("role:{r}")));
        ErrorContext {
            base_url: &cfg.provider.base_url,
            model,
            alternatives,
        }
    }
}

/// The HTTP status in a provider error (`provider error: 401 Unauthorized: …`).
fn status(raw: &str) -> Option<u16> {
    raw.split(|c: char| !c.is_ascii_digit())
        .filter(|w| w.len() == 3)
        .filter_map(|w| w.parse::<u16>().ok())
        .find(|s| (400..600).contains(s))
}

/// `(retry after 30s)` as added by the provider client.
fn retry_after(raw: &str) -> Option<u64> {
    let i = raw.find("retry after ")? + "retry after ".len();
    raw[i..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

pub fn explain(raw: &str, ctx: &ErrorContext) -> ErrorExplained {
    let t = raw.to_lowercase();
    let code = status(raw);
    let host = ctx
        .base_url
        .split("://")
        .nth(1)
        .unwrap_or(ctx.base_url)
        .split('/')
        .next()
        .unwrap_or_default();
    let local = host.starts_with("localhost") || host.starts_with("127.0.0.1");
    let (category, message, action) = if t.starts_with("budget:") {
        (
            "budget",
            raw.trim_start_matches("budget:").trim().to_string(),
            Some(
                "Resume to continue past it this once, or raise the limit in Settings (limits.*)"
                    .to_string(),
            ),
        )
    } else if t.contains("context")
        && (t.contains("length")
            || t.contains("too long")
            || t.contains("window")
            || t.contains("maximum"))
        || t.contains("too many tokens")
        || t.contains("prompt is too long")
    {
        (
            "context",
            format!("The conversation is too long for {}.", ctx.model),
            Some(
                "Compact or fork the Session from an earlier message (/fork), or start /new"
                    .to_string(),
            ),
        )
    } else if t.contains("tool")
        && (t.contains("not support")
            || t.contains("unsupported")
            || t.contains("does not support"))
    {
        (
            "tool_calling",
            format!("{} doesn't support native tool calling.", ctx.model),
            Some(
                "Set provider.tool_mode = prompt (Settings view), or pick another model (/model)"
                    .into(),
            ),
        )
    } else if matches!(code, Some(401 | 403))
        || t.contains("invalid api key")
        || t.contains("incorrect api key")
        || t.contains("unauthorized")
    {
        (
            "auth",
            format!("The API key was rejected by {host}."),
            Some("Re-enter the key: run `formless` setup again, or `formless providers add NAME` for a named provider".into()),
        )
    } else if code == Some(404) || crate::provider::is_model_error_text(&t) {
        (
            "model",
            format!("Model `{}` isn't available on {host}.", ctx.model),
            Some("Pick another model with /model".into()),
        )
    } else if code == Some(429) || t.contains("rate limit") || t.contains("quota") {
        let wait = retry_after(raw)
            .map(|s| format!(" Try again in {s}s."))
            .unwrap_or_default();
        let action = if ctx.alternatives.is_empty() {
            "Wait and /resume, or switch model with /model".to_string()
        } else {
            format!(
                "Wait and /resume, or switch to {} with /model",
                ctx.alternatives.join(", ")
            )
        };
        (
            "rate_limit",
            format!("{host} is rate limiting or out of quota.{wait}"),
            Some(action),
        )
    } else if t.contains("connection refused")
        || t.contains("dns error")
        || t.contains("failed to lookup")
        || t.contains("tcp connect")
        || t.contains("error sending request")
        || t.contains("timed out")
    {
        let hint = if host.contains(":11434") {
            "Is Ollama running? Start it with `ollama serve`, then /resume"
        } else if host.contains(":1234") {
            "Is LM Studio's server running? Start it, then /resume"
        } else if local {
            "Start the local server, then /resume"
        } else {
            "Check your network or the base URL (`formless doctor`), then /resume"
        };
        (
            "unreachable",
            format!("Can't reach {}.", ctx.base_url),
            Some(hint.to_string()),
        )
    } else if code.is_some_and(|c| c >= 500) {
        (
            "server",
            format!("{host} had a server error."),
            Some("Try again with /resume".into()),
        )
    } else {
        (
            "other",
            "The provider request failed.".into(),
            Some("/resume to retry".into()),
        )
    };
    ErrorExplained {
        category: category.into(),
        message,
        action,
        details: raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(base: &str) -> ErrorContext<'_> {
        ErrorContext {
            base_url: base,
            model: "gpt-x",
            alternatives: vec![],
        }
    }

    #[test]
    fn categories() {
        let c = ctx("https://api.openai.com/v1");
        let e = |raw: &str| explain(raw, &c).category;
        assert_eq!(
            e("provider error: 401 Unauthorized: {\"error\":\"bad key\"}"),
            "auth"
        );
        assert_eq!(
            e("provider error: 404 Not Found: model gpt-x does not exist"),
            "model"
        );
        assert_eq!(
            e("provider error: 400 Bad Request: The model `gpt-x` does not exist"),
            "model"
        );
        assert_eq!(e("provider request failed after retries: 429 Too Many Requests (retry after 30s): {}"), "rate_limit");
        assert_eq!(
            e("provider error: 400: This model's maximum context length is 128000 tokens"),
            "context"
        );
        assert_eq!(
            e("provider error: 400: tools are not supported for this model"),
            "tool_calling"
        );
        assert_eq!(
            e("provider request failed after retries: 503 Service Unavailable"),
            "server"
        );
        assert_eq!(
            e("provider request failed after retries: error sending request for url"),
            "unreachable"
        );
        assert_eq!(e("something odd"), "other");
    }

    #[test]
    fn messages_name_the_next_step() {
        let x = explain(
            "provider request failed after retries: 429 Too Many Requests (retry after 30s)",
            &ErrorContext {
                base_url: "https://openrouter.ai/api/v1",
                model: "m",
                alternatives: vec!["role:fast".into()],
            },
        );
        assert!(x.message.contains("Try again in 30s"));
        assert!(x.action.unwrap().contains("role:fast"));
        let local = explain(
            "error sending request: tcp connect error: Connection refused",
            &ctx("http://localhost:11434/v1"),
        );
        assert!(local.action.unwrap().contains("ollama serve"));
        let auth = explain(
            "provider error: 401 Unauthorized",
            &ctx("https://api.groq.com/openai/v1"),
        );
        assert_eq!(auth.message, "The API key was rejected by api.groq.com.");
        assert!(auth.details.contains("401"));
    }
}
