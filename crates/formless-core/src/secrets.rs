//! One secret scanner for everything Formless stores or sends (SPEC §15,
//! #709): memory notes, recorded Tool inputs, outputs and errors,
//! artifacts, and the code approval summaries quote. It looks for the
//! shape of a secret (provider key prefixes, JWTs, private keys, bearer
//! tokens, `password = …`-style assignments, long random-looking tokens)
//! rather than trusting key names, plus the user's own
//! `security.secret_patterns`.

use regex::Regex;
use std::sync::{OnceLock, RwLock};

const PREFIXES: &[(&str, &str)] = &[
    ("sk-", "API key"),
    ("sk_live_", "API key"),
    ("sk_test_", "API key"),
    ("rk_live_", "API key"),
    ("ghp_", "GitHub token"),
    ("gho_", "GitHub token"),
    ("ghu_", "GitHub token"),
    ("ghs_", "GitHub token"),
    ("github_pat_", "GitHub token"),
    ("glpat-", "GitLab token"),
    ("xoxb-", "Slack token"),
    ("xoxp-", "Slack token"),
    ("AIza", "Google API key"),
    ("AKIA", "AWS access key"),
    ("ASIA", "AWS access key"),
];

const MARK: &str = "[REDACTED]";

fn private_key() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----(?s:.*?)(-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)")
            .expect("valid")
    })
}

fn bearer() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9\-._~+/=]{16,}").expect("valid"))
}

/// `password = hunter2`, `api_key: …`, `token=…` in a URL, `"secret": "…"`,
/// `my password is hunter2`.
fn assignment() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"(?i)\b((?:password|passwd|pwd|passphrase|secret|client[_-]?secret|token|access[_-]?token|refresh[_-]?token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|cookie|credentials?)"?(?:\s*[:=]\s*|\s+is\s+)["']?)([^\s"',;&<>]{4,})"#,
        )
        .expect("valid")
    })
}

fn configured() -> &'static RwLock<Vec<Regex>> {
    static R: OnceLock<RwLock<Vec<Regex>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Use `patterns` (regular expressions from `security.secret_patterns`)
/// as well as the built-in shapes. Patterns that don't compile are
/// returned so the caller can say so.
pub fn set_patterns(patterns: &[String]) -> Vec<String> {
    let mut good = Vec::new();
    let mut bad = Vec::new();
    for p in patterns {
        match Regex::new(p) {
            Ok(r) => good.push(r),
            Err(_) => bad.push(p.clone()),
        }
    }
    *configured().write().unwrap_or_else(|e| e.into_inner()) = good;
    bad
}

/// Error endpoints can echo opaque credentials that have no recognizable
/// pattern. Mask exact submitted values before scanning or truncating output.
pub(crate) fn redact_known(text: &str, values: &[&str]) -> String {
    let mut variants = Vec::new();
    for value in values.iter().copied().filter(|v| !v.is_empty()) {
        variants.push(value.to_string());
        // Error bodies may quote a value as JSON or as submitted form data.
        if let Ok(quoted) = serde_json::to_string(value) {
            variants.push(quoted[1..quoted.len() - 1].to_string());
        }
        let mut encoded = reqwest::Url::parse("https://redaction.invalid/").expect("static URL");
        encoded.query_pairs_mut().append_pair("v", value);
        variants.push(
            encoded
                .query()
                .unwrap_or_default()
                .trim_start_matches("v=")
                .to_string(),
        );
    }
    variants.sort_by_key(|value| std::cmp::Reverse(value.len()));
    variants.dedup();
    fn replace(text: &str, variants: &[String]) -> String {
        let mut clean = text.to_string();
        for value in variants {
            clean = clean.replace(value, MARK);
        }
        clean
    }
    fn json_strings(value: &mut serde_json::Value, variants: &[String]) {
        match value {
            serde_json::Value::String(text) => *text = replace(text, variants),
            serde_json::Value::Array(values) => {
                for value in values {
                    json_strings(value, variants);
                }
            }
            serde_json::Value::Object(values) => {
                *values = std::mem::take(values)
                    .into_iter()
                    .map(|(key, mut value)| {
                        json_strings(&mut value, variants);
                        (replace(&key, variants), value)
                    })
                    .collect();
            }
            _ => {}
        }
    }
    // Decode JSON string escapes too: an endpoint can echo the same value as
    // Unicode escapes rather than the spelling serde_json normally emits.
    let clean = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(mut value) => {
            json_strings(&mut value, &variants);
            value.to_string()
        }
        Err(_) => replace(text, &variants),
    };
    redact(&clean).0
}

/// A long run of mixed-case letters and digits with no structure: an
/// API key or token rather than a word, path or hash-like id in prose.
pub fn looks_random(w: &str) -> bool {
    w.len() >= 32
        && !w.contains('/')
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_+=".contains(c))
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_lowercase())
        && w.chars().any(|c| c.is_ascii_uppercase())
}

/// What kind of secret a single word is, if it is one.
fn word_kind(w: &str) -> Option<&'static str> {
    for (prefix, what) in PREFIXES {
        if w.starts_with(prefix) && w.len() >= prefix.len() + 16 {
            return Some(what);
        }
    }
    if w.starts_with("eyJ") && w.matches('.').count() == 2 && w.len() > 40 {
        return Some("JWT");
    }
    looks_random(w.trim_end_matches(['.', ':'])).then_some("random-looking token")
}

/// `text` with every secret replaced by `[REDACTED]`, and the kind of the
/// first one found.
pub fn redact(text: &str) -> (String, Option<&'static str>) {
    let mut kind: Option<&'static str> = None;
    let mut found = |k: &'static str| {
        kind.get_or_insert(k);
    };
    let mut out = text.to_string();
    if private_key().is_match(&out) {
        found("private key");
        out = private_key()
            .replace_all(&out, "[REDACTED private key]")
            .into_owned();
    }
    for r in configured()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
    {
        if r.is_match(&out) {
            found("configured pattern");
            out = r.replace_all(&out, MARK).into_owned();
        }
    }
    if bearer().is_match(&out) {
        found("bearer token");
        out = bearer()
            .replace_all(&out, format!("${{1}}{MARK}"))
            .into_owned();
    }
    if assignment().is_match(&out) {
        let mut any = false;
        out = assignment()
            .replace_all(&out, |c: &regex::Captures| {
                if c[2].starts_with('[') {
                    return c[0].to_string();
                }
                any = true;
                format!("{}{MARK}", &c[1])
            })
            .into_owned();
        if any {
            found("credential");
        }
    }
    // Word by word: provider key prefixes, JWTs, random-looking tokens.
    let is_delim = |c: char| c.is_whitespace() || "\"'`,;()[]{}<>=&?".contains(c);
    let mut result = String::with_capacity(out.len());
    let mut word = String::new();
    let flush = |word: &mut String, result: &mut String, found: &mut dyn FnMut(&'static str)| {
        if let Some(k) = word_kind(word) {
            found(k);
            result.push_str(MARK);
        } else {
            result.push_str(word);
        }
        word.clear();
    };
    for c in out.chars() {
        if is_delim(c) {
            flush(&mut word, &mut result, &mut found);
            result.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut result, &mut found);
    (result, kind)
}

/// The kind of the first secret in `text`, if any.
pub fn scan(text: &str) -> Option<&'static str> {
    redact(text).1
}

#[cfg(test)]
mod tests {
    #[test]
    fn exact_credential_masking_decodes_json_escapes_without_changing_usage() {
        let body = r#"{"error":{"message":"echo \u006fpaque-access"},"usage":{"prompt_tokens":12,"completion_tokens":3}}"#;
        let clean = super::redact_known(body, &["opaque-access"]);
        let value: serde_json::Value = serde_json::from_str(&clean).unwrap();
        assert_eq!(value["error"]["message"], "echo [REDACTED]");
        assert_eq!(value["usage"]["prompt_tokens"], 12);
        assert_eq!(value["usage"]["completion_tokens"], 3);
    }

    use super::*;

    #[test]
    fn finds_and_blanks_secret_shapes_not_key_names() {
        let cases = [
            ("key sk-proj-abcdefghijklmnopqrstuvwxyz0123 here", "API key"),
            ("Authorization: Bearer abcdefghijklmnop1234", "bearer token"),
            ("my passwd: hunter22", "credential"),
            ("https://x.dev/cb?token=abc123def456&x=1", "credential"),
            ("cookie=sessionid1234", "credential"),
            ("{\"client_secret\": \"s3cr3tvalue\"}", "credential"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "GitHub token"),
            ("aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3h", "random-looking token"),
        ];
        for (text, kind) in cases {
            let (out, k) = redact(text);
            assert_eq!(k, Some(kind), "{text} → {out}");
            assert!(out.contains("[REDACTED]"), "{out}");
        }
        let pem = "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIabc\n-----END RSA PRIVATE KEY-----\nb";
        assert_eq!(redact(pem).0, "a\n[REDACTED private key]\nb");
        // Ordinary text is left alone.
        for text in [
            "the token count is high",
            "src/main.rs:42",
            "password reset flow",
            "sha 3f2a9c1",
        ] {
            assert_eq!(redact(text), (text.to_string(), None), "{text}");
        }
        // Already redacted stays as it is.
        assert_eq!(redact("token=[REDACTED]").1, None);
    }

    #[test]
    fn configured_patterns_apply() {
        let bad = set_patterns(&["ACME-[0-9]{6}".into(), "(".into()]);
        assert_eq!(bad, vec!["(".to_string()]);
        let (out, k) = redact("ticket ACME-123456 done");
        assert_eq!(
            (out.as_str(), k),
            ("ticket [REDACTED] done", Some("configured pattern"))
        );
        set_patterns(&[]);
    }
}
