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
        None => raw
            .to_lowercase()
            .split([':', '\n'])
            .any(transient_diagnostic),
    }
}

/// Text-only compatibility for rendered error chains. Unknown prose is not
/// enough evidence to spend another request. Match diagnostic clauses, not
/// arbitrary words inside model IDs, parameter names or permanent messages.
fn transient_diagnostic(clause: &str) -> bool {
    let clause = clause.trim();
    if matches!(
        clause,
        "timeout" | "timeout_error" | "request_timeout" | "overloaded" | "overloaded_error"
    ) {
        return true;
    }
    let event_code = clause
        .strip_prefix("error (")
        .or_else(|| clause.strip_prefix("response.failed ("))
        .and_then(|code| code.strip_suffix(')'));
    if matches!(
        event_code,
        Some("timeout" | "timeout_error" | "request_timeout" | "overloaded" | "overloaded_error")
    ) {
        return true;
    }
    [
        "timed out",
        "request timed out",
        "operation timed out",
        "connection timed out",
        "timeout waiting for",
        "deadline has elapsed",
        "connection refused",
        "connection reset",
        "connection aborted",
        "connection closed",
        "connection lost",
        "broken pipe",
        "unexpected eof",
        "unexpected end of file",
        "end of file before message length reached",
        "early eof",
        "temporary failure in name resolution",
        "stream dropped",
        "stream ended before message_stop",
        "stream ended with a content block still open",
        "server is overloaded",
        "server overloaded",
    ]
    .iter()
    .any(|prefix| {
        clause.strip_prefix(prefix).is_some_and(|rest| {
            rest.is_empty()
                || rest == "."
                || rest.starts_with(|c: char| c.is_whitespace() || matches!(c, '(' | '['))
        })
    })
}

fn status(raw: &str) -> Option<u16> {
    // A displayed HTTP status starts an error clause: a bare status, an
    // anyhow/provider context after ':', or reqwest's parenthesized status.
    // Do not scan arbitrary digit runs: parser coordinates (and URL ports,
    // sizes, request IDs, etc.) are diagnostic data, not HTTP evidence.
    raw.split([':', '(', '\n']).find_map(|clause| {
        let word = clause.split_ascii_whitespace().next()?;
        let code = word.trim_end_matches(')');
        if code.len() != 3 {
            return None;
        }
        code.parse::<u16>().ok().filter(|s| (400..600).contains(s))
    })
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
    let parsed_url = reqwest::Url::parse(ctx.base_url)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https") && url.has_host());
    let host = parsed_url
        .as_ref()
        .and_then(|url| {
            let host = url.host_str()?;
            Some(match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_string(),
            })
        })
        .unwrap_or_else(|| "the configured endpoint".to_string());
    let display_endpoint = || match parsed_url.as_ref() {
        Some(_) => crate::rate_limits::display_endpoint(ctx.base_url),
        None => "(invalid endpoint)".to_string(),
    };
    // Keep the existing diagnostic text while masking echoes of the
    // configured URL and its normalized spelling when they carry credentials.
    let private_endpoint = match parsed_url.as_ref() {
        Some(url) => {
            !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
        }
        None => ctx
            .base_url
            .trim()
            .split_once("://")
            .is_some_and(|(scheme, _)| {
                scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
            }),
    };
    let details = if private_endpoint && !ctx.base_url.is_empty() {
        let endpoint = display_endpoint();
        let details = raw.replace(ctx.base_url, &endpoint);
        if let Some(url) = parsed_url
            .as_ref()
            .filter(|url| url.as_str() != ctx.base_url)
        {
            details.replace(url.as_str(), &endpoint)
        } else {
            details
        }
    } else {
        raw.to_string()
    };
    let local = parsed_url
        .as_ref()
        .and_then(|url| url.host_str())
        .is_some_and(crate::config::is_loopback_host);
    let (category, message, action) = if t.starts_with("stuck: repeated daemon correction:") {
        (
            "stuck",
            "The model repeated the same invalid Tool call or all-correction batch three times without fixing it. The Run is paused to avoid repeating paid requests.".into(),
            Some("Revise your request or switch model, then Resume to try again".into()),
        )
    } else if t.starts_with("budget:") {
        (
            "budget",
            details.trim_start_matches("budget:").trim().to_string(),
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
        let port = parsed_url
            .as_ref()
            .and_then(|url| url.port_or_known_default());
        let hint = if port == Some(11434) {
            "Is Ollama running? Start it with `ollama serve`, then /resume"
        } else if port == Some(1234) {
            "Is LM Studio's server running? Start it, then /resume"
        } else if local {
            "Start the local server, then /resume"
        } else {
            "Check your network or the base URL (`formless doctor`), then /resume"
        };
        (
            "unreachable",
            format!("Can't reach {}.", display_endpoint()),
            Some(hint.to_string()),
        )
    } else if code.is_some_and(|c| c >= 500) {
        (
            "server",
            format!("{host} had a server error."),
            Some("Try again with /resume".into()),
        )
    } else if code.is_some_and(|c| (400..500).contains(&c)) && !is_transient(raw) {
        (
            "request",
            format!("The provider rejected the request for {}.", ctx.model),
            Some("Inspect /details, try another model with /model, and run `formless doctor` to check endpoint/API settings".into()),
        )
    } else if is_transient(raw) {
        (
            "other",
            "The provider request failed temporarily.".into(),
            Some("/resume to retry".into()),
        )
    } else {
        (
            "other",
            "The provider request failed.".into(),
            Some("Inspect /details, try another model with /model, and run `formless doctor` to check the provider configuration".into()),
        )
    };
    ErrorExplained {
        category: category.into(),
        message,
        action,
        details,
    }
}

/// A resumed Run gets stronger guidance when its predecessor failed in the
/// same category. Budget/correction choices retain their existing semantics.
pub fn explain_with_previous(
    raw: &str,
    previous: Option<&str>,
    ctx: &ErrorContext,
) -> ErrorExplained {
    let mut current = explain(raw, ctx);
    let Some(previous) = previous else {
        return current;
    };
    if explain(previous, ctx).category != current.category {
        return current;
    }
    let keys = match current.category.as_str() {
        "auth" => "provider.base_url and provider.auth.mode",
        "model" => "provider.model and provider.aliases",
        "endpoint" => "provider.base_url and provider.api_style",
        "tool_calling" => "provider.tool_mode, provider.api_style and provider.model",
        "context" => "provider.context_tokens, provider.model_context_tokens and compaction.trigger_fraction",
        "rate_limit" => "provider.model and provider.max_retries",
        "unreachable" => "provider.base_url and provider.proxy",
        "request" | "server" | "other" => "provider.api_style, provider.reasoning_effort, provider.base_url and provider.extra_body",
        _ => return current,
    };
    current.message = format!(
        "Same {} error again after Resume: check {keys}. {}",
        current.category, current.message
    );
    current.action = Some(format!("Check {keys} with `formless config edit`; inspect /details, try /model, and run `formless doctor` before retrying"));
    current
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
    fn error_explanations_name_endpoints_without_url_credentials() {
        let base = "https://fixture-user:fixture-pass@example.test:9443/v1?api_key=fixture-query#fixture-fragment";
        for (raw, category, message) in [
            (
                "401 Unauthorized",
                "auth",
                "The API key was rejected by example.test:9443.",
            ),
            (
                "429 Too Many Requests",
                "rate_limit",
                "example.test:9443 is rate limiting or out of quota.",
            ),
            (
                "503 Service Unavailable",
                "server",
                "example.test:9443 had a server error.",
            ),
            (
                "connection refused",
                "unreachable",
                "Can't reach https://example.test:9443/v1.",
            ),
        ] {
            let detail = format!("{raw}\nEndpoint {base}");
            let explained = explain(&detail, &ctx(base));
            assert_eq!(explained.category, category);
            assert_eq!(explained.message, message);
            assert_eq!(
                explained.details,
                format!("{raw}\nEndpoint https://example.test:9443/v1")
            );
        }
        let local = explain(
            "connection refused",
            &ctx("http://fixture-user:fixture-pass@localhost:11434/v1?key=fixture-query"),
        );
        assert_eq!(local.message, "Can't reach http://localhost:11434/v1.");
        assert!(local.action.unwrap().contains("ollama serve"));
        let invalid = explain(
            "connection refused",
            &ctx("not an endpoint?key=fixture-query"),
        );
        assert_eq!(invalid.message, "Can't reach (invalid endpoint).");
        let rate_limit = "429 Too Many Requests";
        assert_eq!(explain(rate_limit, &ctx("429")).details, rate_limit);
        let malformed = "https://[bad-host?token=fixture-query";
        let diagnostic = format!("connection refused: {malformed}");
        assert_eq!(
            explain(&diagnostic, &ctx(malformed)).details,
            "connection refused: (invalid endpoint)"
        );
    }

    #[test]
    fn loopback_provider_hints_use_parsed_endpoint_identity() {
        for endpoint in [
            "http://[::1]:8080/v1",
            "http://127.0.0.2:8000/v1",
            "http://app.localhost:8000/v1",
            "http://demo:placeholder@localhost:8000/v1",
            "HTTP://LOCALHOST:8000/v1",
        ] {
            let raw = "error sending request: tcp connect error: connection refused";
            let explained = explain(raw, &ctx(endpoint));
            assert_eq!(explained.category, "unreachable", "{endpoint}");
            assert_eq!(
                explained.action.as_deref(),
                Some("Start the local server, then /resume"),
                "{endpoint}"
            );
            assert_eq!(explained.details, raw);
        }
    }

    #[test]
    fn remote_lookalikes_and_invalid_urls_do_not_get_local_server_hints() {
        for endpoint in [
            "http://localhost.example.com:8080/v1",
            "http://127.0.0.1.nip.io:8080/v1",
            "http://localhost@remote.invalid:8080/v1",
            "http://remote.invalid:8080/localhost",
            "not a URL",
        ] {
            let explained = explain("connection refused", &ctx(endpoint));
            assert_eq!(explained.category, "unreachable");
            assert!(
                explained.action.unwrap().starts_with("Check your network"),
                "{endpoint}"
            );
        }
        assert!(explain("connection refused", &ctx("http://[::1]:11434/v1"))
            .action
            .unwrap()
            .contains("Ollama"));
        assert!(
            explain("connection refused", &ctx("http://localhost:1234/v1"))
                .action
                .unwrap()
                .contains("LM Studio")
        );
    }

    #[test]
    fn parser_coordinates_do_not_change_error_advice_or_transience() {
        use formless_runtime::structured_output::JsonOutputSchema;

        let schema = JsonOutputSchema::compile(serde_json::json!({})).unwrap();
        let context = ctx("https://private.invalid/v1");
        let baseline = explain("structured output is not one complete JSON value", &context);
        // Exercise every HTTP-shaped position, on both axes, through the real
        // parser and Display contract rather than fabricated diagnostics.
        for position in 400..600 {
            for input in [
                format!("{}?", " ".repeat(position - 1)),
                format!("{}?", "\n".repeat(position - 1)),
            ] {
                let diagnostic = schema.parse_and_validate(&input).unwrap_err().to_string();
                for raw in [
                    diagnostic.clone(),
                    format!("schema: final answer still mismatches after one repair: {diagnostic}"),
                    format!("llm.complete failed: {diagnostic}"),
                ] {
                    let explained = explain(&raw, &context);
                    assert_eq!(explained.category, "other", "{raw}");
                    assert_eq!(explained.message, baseline.message, "{raw}");
                    assert_eq!(explained.action, baseline.action, "{raw}");
                    assert_eq!(explained.details, raw);
                    assert!(!is_transient(&raw), "{raw}");
                    assert_eq!(
                        explain_with_previous(&raw, Some(&raw), &context).category,
                        "other",
                        "{raw}"
                    );
                }
            }
        }
    }

    #[test]
    fn diagnostic_numbers_are_not_http_statuses() {
        for raw in [
            "parse response: expected value at line 503 column 401",
            "provider error: request id 429 failed validation",
            "response size 500 bytes was invalid",
            "error sending request for url (https://private.invalid:503/v1)",
        ] {
            assert_eq!(status(raw), None, "{raw}");
        }
        assert_eq!(status("HTTP status error (503)"), Some(503));
    }

    #[test]
    fn genuine_http_statuses_keep_their_categories_and_retry_policy() {
        let context = ctx("https://private.invalid/v1");
        for (status_text, category, transient) in [
            ("400 Bad Request", "request", false),
            ("401 Unauthorized", "auth", false),
            ("403 Forbidden", "auth", false),
            ("404 Not Found", "endpoint", false),
            ("408 Request Timeout", "other", true),
            ("429 Too Many Requests", "rate_limit", true),
            ("500 Internal Server Error", "server", true),
            ("503 Service Unavailable", "server", true),
            ("599 <unknown status code>", "server", true),
        ] {
            for raw in [
                status_text.to_string(),
                format!("provider error: {status_text}: {{}}"),
                format!("provider request failed after retries: {status_text}"),
                format!("POST /chat/completions failed: {status_text}"),
                format!("outer context: HTTP status error ({status_text}) for url (https://private.invalid/v1)"),
                format!("provider error: {status_text}: structured output is not one complete JSON value (parser: syntax, line 429, column 503)"),
            ] {
                assert_eq!(explain(&raw, &context).category, category, "{raw}");
                assert_eq!(is_transient(&raw), transient, "{raw}");
            }
        }
    }

    #[test]
    fn persistent_bad_requests_point_to_diagnostics_instead_of_resume() {
        for raw in [
            "provider error: 400 Bad Request: unknown parameter reasoning_effort",
            "provider error: 400 Bad Request: API style mismatch; expected Messages API",
        ] {
            let e = explain(raw, &ctx("https://private.invalid/v1"));
            assert_eq!(e.category, "request");
            let action = e.action.unwrap();
            for command in ["/details", "/model", "formless doctor"] {
                assert!(action.contains(command), "{action}");
            }
            assert!(!action.contains("/resume"), "{action}");
        }
    }

    #[test]
    fn repeated_categories_escalate_without_rewriting_details_or_local_budget_choices() {
        let c = ctx("https://private.invalid/v1");
        let raw = "provider error: 400 Bad Request: unknown parameter reasoning_effort";
        let repeated =
            explain_with_previous(raw, Some("provider error: 422 unsupported argument"), &c);
        assert!(repeated.message.contains("again"));
        assert!(
            repeated
                .message
                .chars()
                .take(200)
                .collect::<String>()
                .contains("provider.extra_body"),
            "configuration keys must fit in the TUI banner: {}",
            repeated.message
        );
        assert!(repeated.action.unwrap().contains("provider.api_style"));
        assert_eq!(repeated.details, raw);
        assert!(
            !explain_with_previous(raw, Some("provider error: 401 Unauthorized"), &c)
                .message
                .contains("again")
        );
        let budget = "budget: limit reached";
        assert_eq!(
            explain_with_previous(budget, Some(budget), &c).action,
            explain(budget, &c).action
        );
        for transient in [
            "provider error: 408 timeout",
            "provider error: 503 Service Unavailable",
            "connection reset",
        ] {
            assert!(explain(transient, &c).action.unwrap().contains("/resume"));
        }
        assert!(!explain("malformed reply", &c)
            .action
            .unwrap()
            .contains("/resume"));
    }

    #[test]
    fn local_server_hints_match_exact_ports_instead_of_prefixes() {
        for (url, expected) in [
            ("http://localhost:1234/v1", "LM Studio"),
            ("http://localhost:12345/v1", "Start the local server"),
            ("http://localhost:11434/v1", "ollama serve"),
            ("http://[::1]:1234/v1", "LM Studio"),
            ("http://localhost:12349/v1", "Start the local server"),
        ] {
            let explained = explain("connection refused", &ctx(url));
            assert_eq!(explained.category, "unreachable");
            assert!(explained.action.unwrap().contains(expected), "{url}");
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
    fn permanent_words_do_not_trigger_an_extra_retry_or_temporary_advice() {
        let context = super::ErrorContext {
            base_url: "https://fixture.invalid/v1",
            model: "fixture",
            alternatives: vec![],
        };
        for message in [
            "model does not support streaming",
            "downstream rejected the schema",
            "password reset required",
            "connection field is missing",
            "timeout parameter must be positive",
            "overloaded model family is unsupported",
            "resettable is not a known parameter",
            "stream droppedness is not supported",
        ] {
            for raw in [
                message.to_string(),
                format!("provider error: {message}"),
                format!("prompt evaluation: {message}"),
            ] {
                assert!(!is_transient(&raw), "{raw}");
                let explained = super::explain(&raw, &context);
                assert!(!explained.message.contains("temporarily"), "{explained:?}");
                assert!(!explained
                    .action
                    .as_deref()
                    .unwrap_or_default()
                    .contains("/resume"));
                assert_eq!(explained.details, raw);
            }
        }
    }

    #[test]
    fn specific_transport_diagnostics_and_status_precedence_are_retained() {
        for raw in [
            "request timed out",
            "outer: operation timed out",
            "connection reset by peer (os error 104)",
            "error sending request: connection refused (os error 111)",
            "stream read: unexpected EOF",
            "stream read: error reading a body from connection: end of file before message length reached",
            "timeout_error: request expired",
            "provider error: stream dropped: stream ended before message_stop",
            "provider error: overloaded_error: Overloaded",
            "provider error: error (overloaded_error): server busy",
            "timeout",
            "deadline has elapsed",
        ] {
            assert!(is_transient(raw), "{raw}");
        }
        assert!(!is_transient("error sending request: error trying to connect: invalid peer certificate: UnknownIssuer"));
        assert!(!is_transient(
            "provider error: error (invalid_timeout_parameter): timeout must be positive"
        ));
        for raw in [
            "400 Bad Request: connection reset",
            "401 Unauthorized: stream dropped",
            "404 Not Found: request timed out",
        ] {
            assert!(!is_transient(raw), "{raw}");
        }
        for raw in [
            "408 Request Timeout",
            "429 Too Many Requests",
            "503 Service Unavailable: unsupported streaming",
        ] {
            assert!(is_transient(raw), "{raw}");
        }
    }

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
    fn anonymous_tool_callbacks_keep_their_own_source_location() {
        let code = "async function run() {\n  return Promise.all([1].map(async () => {\n    await tools.call('inner', {});\n  }));\n}";
        let message = "inner failed\n    at <anonymous> (formless-tool.js:3:11)\n    at run (formless-tool.js:2:30)\n    at <anonymous> (formless-host.js:9:4)";
        let detail = tool_error_detail(message, code, &serde_json::json!({}));
        assert!(detail.contains("at <anonymous> (line 3)"), "{detail}");
        assert!(
            detail.contains("> 3 |     await tools.call('inner', {});"),
            "{detail}"
        );
        assert!(!detail.contains("line 9"), "{detail}");
    }

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
