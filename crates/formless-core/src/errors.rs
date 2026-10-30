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
/// Worth one retry: a rate limit, a server error, or the connection
/// failing; never an auth error, a bad request or a malformed reply.
pub fn is_transient(raw: &str) -> bool {
    match status(raw) {
        Some(code) => code == 408 || code == 429 || code >= 500,
        None => {
            let t = raw.to_lowercase();
            [
                "timed out",
                "timeout",
                "connection",
                "connect error",
                "stream",
                "overloaded",
                "reset",
            ]
            .iter()
            .any(|k| t.contains(k))
        }
    }
}

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
    } else if crate::provider::is_model_error_text(&t)
        || (code == Some(404) && !ctx.model.is_empty() && t.contains(&ctx.model.to_lowercase()))
    {
        (
            "model",
            format!("Model `{}` isn't available on {host}.", ctx.model),
            Some("Pick another model with /model".into()),
        )
    } else if code == Some(404) {
        // A 404 that names no model is almost always the wrong address
        // (a missing /v1, a typo), not the model (#833).
        (
            "endpoint",
            format!("{host} has nothing at the address Formless called (404)."),
            Some(
                "Check provider.base_url (often a missing /v1), then run `formless doctor`".into(),
            ),
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
        // A bare or HTML 404 is the address, not the model (#833).
        assert_eq!(
            e("provider error: 404 Not Found: <html><body>Not Found</body></html>"),
            "endpoint"
        );
        assert_eq!(e("provider error: 404 Not Found: "), "endpoint");
        assert!(explain("provider error: 404 Not Found: ", &c)
            .action
            .unwrap()
            .contains("base_url"));
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

#[cfg(test)]
mod transient_tests {
    use super::is_transient;

    #[test]
    fn only_transient_failures_are_retried() {
        assert!(is_transient(
            "POST /chat/completions failed: 429 Too Many Requests"
        ));
        assert!(is_transient("503 Service Unavailable"));
        assert!(is_transient("error sending request: connection refused"));
        assert!(!is_transient("401 Unauthorized: invalid api key"));
        assert!(!is_transient("400 Bad Request: unknown parameter"));
        assert!(!is_transient("parse response: expected value"));
    }
}

/// A Tool's JavaScript error made readable (SPEC §7.1): the message, the
/// call path in the Tool's own code, the failing line with two lines of
/// context, and the input that caused it. QuickJS reports Tool-code
/// frames as `at NAME (eval_script:LINE:COL)`; the runner's own frames
/// (`<anonymous>`, `<eval>`) are left out.
pub fn tool_error_detail(message: &str, code: &str, input: &serde_json::Value) -> String {
    let mut lines = message.lines();
    let head = lines.next().unwrap_or_default().trim().to_string();
    let frames: Vec<(String, usize, usize)> = lines
        .filter_map(|l| {
            let l = l.trim().strip_prefix("at ")?;
            let (name, loc) = l.split_once(" (")?;
            if name.starts_with('<') {
                return None;
            }
            let loc = loc.strip_suffix(')')?.strip_prefix("eval_script:")?;
            let mut parts = loc.split(':');
            let line = parts.next()?.parse().ok()?;
            let col = parts.next().and_then(|c| c.parse().ok()).unwrap_or(0);
            Some((name.to_string(), line, col))
        })
        .collect();
    let src: Vec<&str> = code.lines().collect();
    let mut out = vec![head];
    let Some((_, line, col)) = frames
        .first()
        .filter(|(_, l, _)| *l >= 1 && *l <= src.len())
    else {
        // No position in the Tool's code: keep what QuickJS said.
        return message.trim_end().to_string();
    };
    let path: Vec<String> = frames
        .iter()
        .map(|(name, l, _)| format!("{name} (line {l})"))
        .collect();
    out.push(format!("  at {}", path.join(" ← ")));
    out.push(String::new());
    let width = (line + 2).min(src.len()).to_string().len();
    for n in line.saturating_sub(2).max(1)..=(line + 2).min(src.len()) {
        let mark = if n == *line { ">" } else { " " };
        out.push(format!("{mark} {n:>width$} | {}", src[n - 1]));
        if n == *line && *col > 0 {
            out.push(format!("  {:>width$} | {}^", "", " ".repeat(col - 1)));
        }
    }
    let shown = input.to_string();
    let shown: String = if shown.chars().count() > 500 {
        format!("{}…", shown.chars().take(500).collect::<String>())
    } else {
        shown
    };
    out.push(String::new());
    out.push(format!("input: {shown}"));
    out.join("\n")
}

#[cfg(test)]
mod tool_error_tests {
    use super::tool_error_detail;

    #[test]
    fn js_errors_show_the_line_the_path_and_the_input() {
        let code = "function helper(x) {\n  return x.missing.deep;\n}\nfunction run(input) {\n  const a = 1;\n  return helper(input);\n}";
        let msg = "cannot read property 'deep' of undefined\n    at helper (eval_script:2:10)\n    at run (eval_script:6:17)\n    at <anonymous> (eval_script:8:34)\n    at <eval> (eval_script:11:3)\n";
        let out = tool_error_detail(msg, code, &serde_json::json!({"a": 1}));
        assert_eq!(
            out,
            "cannot read property 'deep' of undefined\n  at helper (line 2) ← run (line 6)\n\n  1 | function helper(x) {\n> 2 |   return x.missing.deep;\n    |          ^\n  3 | }\n  4 | function run(input) {\n\ninput: {\"a\":1}"
        );
        // No usable frame: unchanged.
        assert_eq!(
            tool_error_detail("boom\n", code, &serde_json::json!({})),
            "boom"
        );
    }
}
