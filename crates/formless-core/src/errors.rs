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

/// Readable context for this Tool only. Eval filenames, rather than line
/// numbers alone, distinguish Tool code from host/runner/module code.
pub fn tool_error_detail(message: &str, code: &str, input: &serde_json::Value) -> String {
    let src: Vec<&str> = code.lines().collect();
    let frames: Vec<(String, usize, usize)> = message
        .lines()
        .filter_map(|line| {
            let frame = line.trim().strip_prefix("at ")?;
            let (name, location) = frame.split_once(" (")?;
            if name.starts_with('<') {
                return None;
            }
            let location = location
                .strip_suffix(')')?
                .strip_prefix(formless_runtime::TOOL_SOURCE_NAME)?
                .strip_prefix(':')?;
            let mut parts = location.split(':');
            let line: usize = parts.next()?.parse().ok()?;
            let column = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
            (line > 0 && line <= src.len()).then(|| (name.to_string(), line, column))
        })
        .collect();
    let mut out = Vec::new();
    if let Some((_, line, column)) = frames.first() {
        out.push(
            message
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
        );
        let path: Vec<String> = frames
            .iter()
            .map(|(name, line, _)| format!("{name} (line {line})"))
            .collect();
        out.push(format!("  at {}", path.join(" ← ")));
        out.push(String::new());
        let width = (line + 2).min(src.len()).to_string().len();
        for number in line.saturating_sub(2).max(1)..=(line + 2).min(src.len()) {
            let mark = if number == *line { ">" } else { " " };
            let clean = crate::secrets::redact(src[number - 1]).0;
            out.push(format!(
                "{mark} {number:>width$} | {}",
                expand_tabs(&clean).0
            ));
            if number == *line && *column > 0 {
                // QuickJS-NG columns are one-based UTF-8 byte offsets. Round
                // malformed offsets down before measuring graphemes for display.
                let raw = src[number - 1];
                let mut end = (column - 1).min(raw.len());
                while !raw.is_char_boundary(end) {
                    end -= 1;
                }
                let prefix = crate::secrets::redact(&raw[..end]).0;
                let display_column = expand_tabs(&prefix).1;
                out.push(format!(
                    "  {:>width$} | {}^",
                    "",
                    " ".repeat(display_column)
                ));
            }
        }
    } else {
        out.push(message.trim_end().to_string());
    }
    // Redact the complete value before truncation: neither quoted key values
    // nor a partially truncated credential can escape the shared scanner.
    let shown = crate::tools::redact(input).to_string();
    let shown = if shown.chars().count() > 500 {
        format!("{}…", shown.chars().take(500).collect::<String>())
    } else {
        shown
    };
    out.push(String::new());
    out.push(format!("input: {shown}"));
    crate::secrets::redact(&out.join("\n")).0
}

fn expand_tabs(line: &str) -> (String, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let mut output = String::new();
    let mut column = 0;
    for grapheme in line.graphemes(true) {
        if grapheme == "\t" {
            let count = 4 - column % 4;
            output.push_str(&" ".repeat(count));
            column += count;
        } else {
            output.push_str(grapheme);
            column += UnicodeWidthStr::width(grapheme);
        }
    }
    (output, column)
}

/// Each level contributes only its own source/input. Already-formatted inner
/// detail is appended unchanged, rather than interpreted as caller stack frames.
pub fn tool_failure_detail(
    tool: &str,
    message: &str,
    code: &str,
    input: &serde_json::Value,
    nested: Option<&formless_runtime::NestedToolError>,
) -> String {
    let inner = nested.filter(|nested| !nested.detail.is_empty());
    let own_message = match inner {
        Some(nested) => message.replace(
            &nested.detail,
            &format!("nested Tool {} failed", nested.tool),
        ),
        None => message.to_string(),
    };
    let own = tool_error_detail(&own_message, code, input);
    let chain = match inner {
        Some(nested) => format!(
            "{tool} → {}",
            nested
                .detail
                .lines()
                .next()
                .and_then(|line| line.strip_prefix("Tool call chain: "))
                .unwrap_or(&nested.tool)
        ),
        None => tool.to_string(),
    };
    let mut result = format!("Tool call chain: {chain}\n\nTool {tool}:\n{own}");
    if let Some(nested) = inner {
        result.push_str("\n\n");
        result.push_str(&nested.detail);
    }
    crate::secrets::redact(&result).0
}

#[cfg(test)]
mod tool_error_tests {
    use super::{tool_error_detail, tool_failure_detail};

    #[test]
    fn js_errors_show_the_line_the_path_and_the_input() {
        let code = "function helper(x) {\n  return x.missing.deep;\n}\nfunction run(input) {\n  const a = 1;\n  return helper(input);\n}";
        let msg = "cannot read property 'deep' of undefined\n    at helper (formless-tool.js:2:10)\n    at run (formless-tool.js:6:17)\n    at <anonymous> (formless-host.js:8:34)\n    at <eval> (formless-runner.js:11:3)\n";
        let out = tool_error_detail(msg, code, &serde_json::json!({"a": 1}));
        assert_eq!(
            out,
            "cannot read property 'deep' of undefined\n  at helper (line 2) ← run (line 6)\n\n  1 | function helper(x) {\n> 2 |   return x.missing.deep;\n    |          ^\n  3 | }\n  4 | function run(input) {\n\ninput: {\"a\":1}"
        );
        // No usable frame: retain text and add the redacted input.
        assert_eq!(
            tool_error_detail("boom\n", code, &serde_json::json!({})),
            "boom\n\ninput: {}"
        );
    }

    #[test]
    fn input_redaction_handles_nested_keys_escaping_and_truncation() {
        let input = serde_json::json!({
            "api_key":"short\"escaped\\credential",
            "headers":[{"AUTHORIZATION":"pw", "nested":{"refreshToken":"private-token-value", "passphrase":"phrase", "pwd":"pw2"}}],
            "note":"ordinary \"quoted\" text",
            "body":"sk-abcdefghijklmnop1234567890"
        });
        let out = tool_error_detail("boom", "function run() {}", &input);
        let shown: serde_json::Value =
            serde_json::from_str(out.lines().last().unwrap().strip_prefix("input: ").unwrap())
                .unwrap();
        assert_eq!(shown["api_key"], "[redacted]");
        assert_eq!(shown["headers"][0]["AUTHORIZATION"], "[redacted]");
        assert_eq!(shown["headers"][0]["nested"]["refreshToken"], "[redacted]");
        assert_eq!(shown["headers"][0]["nested"]["passphrase"], "[redacted]");
        assert_eq!(shown["headers"][0]["nested"]["pwd"], "[redacted]");
        assert_eq!(shown["note"], input["note"]);
        for secret in [
            "short\"escaped\\credential",
            "pw2",
            "private-token-value",
            "sk-abcdefghijklmnop1234567890",
        ] {
            assert!(!out.contains(secret), "input credential leaked");
        }
        let out = tool_error_detail(
            "boom",
            "",
            &serde_json::json!({
                "note":format!("{} sk-abcdefghijklmnop1234567890", "a".repeat(480))
            }),
        );
        assert!(!out.contains("sk-"), "redaction happened after truncation");
    }

    #[test]
    fn utf8_byte_columns_use_grapheme_width_and_four_column_tabs() {
        let line = "\tconst x = '😀界';\tthrow new Error('x');";
        let column = line.find("throw").unwrap() + 1;
        let out = tool_error_detail(
            &format!("x\n at run (formless-tool.js:2:{column})"),
            &format!("function run() {{\n{line}\n}}"),
            &serde_json::json!({}),
        );
        assert!(
            out.contains("> 2 |     const x = '😀界';   throw new Error('x');"),
            "{out}"
        );
        assert!(out.contains(&format!("    | {}^", " ".repeat(24))), "{out}");
    }

    #[test]
    fn unnamed_module_frames_are_never_mapped_to_tool_source() {
        let code = "function run() {\n  const wrong = 'not module source';\n  return library.explode();\n}";
        let out = tool_error_detail(
            "module problem\n at explodeModule (eval_script:2:10)\n at run (formless-tool.js:3:10)",
            code,
            &serde_json::json!({}),
        );
        assert!(!out.contains("explodeModule (line 2)"), "{out}");
        assert!(!out.contains("> 2 |"), "{out}");
        assert!(out.contains("> 3 |   return library.explode();"), "{out}");
    }

    #[test]
    fn nested_detail_stays_verbatim_with_separate_callsite_and_inputs() {
        let inner = tool_failure_detail(
            "inner",
            "inner broke\n at run (formless-tool.js:2:3)",
            "function run() {\n  throw new Error('INNER_SOURCE');\n}",
            &serde_json::json!({"inner_visible":7,"token":"never-copy-this"}),
            None,
        );
        let nested = formless_runtime::NestedToolError {
            tool: "inner".into(),
            detail: inner.clone(),
        };
        let outer = tool_failure_detail(
            "outer",
            &format!("{inner}\n at run (formless-tool.js:2:10)"),
            "async function run() {\n  return await tools.call('inner', {});\n}",
            &serde_json::json!({"outer_visible":9,"password":"pw"}),
            Some(&nested),
        );
        assert!(
            outer.starts_with("Tool call chain: outer → inner\n"),
            "{outer}"
        );
        assert_eq!(
            outer.matches(&inner).count(),
            1,
            "inner detail changed or duplicated"
        );
        let own = outer.strip_suffix(&inner).unwrap();
        assert!(own.contains("outer_visible"));
        assert!(!own.contains("inner_visible"));
        assert!(!own.contains("INNER_SOURCE"));
        assert!(inner.contains("inner_visible"));
        assert!(!outer.contains("never-copy-this"));
        assert!(!outer.contains("pw"));
    }
}
