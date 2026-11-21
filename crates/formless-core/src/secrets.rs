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
    ("rk_test_", "API key"),
    ("rk_live_", "API key"),
    ("whsec_", "API key"),
    ("ghp_", "GitHub token"),
    ("gho_", "GitHub token"),
    ("ghu_", "GitHub token"),
    ("ghs_", "GitHub token"),
    ("ghr_", "GitHub token"),
    ("github_pat_", "GitHub token"),
    ("npm_", "npm token"),
    ("glpat-", "GitLab token"),
    ("gldt-", "GitLab token"),
    ("glrt-", "GitLab token"),
    ("xoxb-", "Slack token"),
    ("xoxe.xapp-", "Slack token"),
    ("xoxe.xoxb-", "Slack token"),
    ("xoxe.xoxp-", "Slack token"),
    ("xoxe-", "Slack token"),
    ("xapp-", "Slack token"),
    ("xoxp-", "Slack token"),
    ("shpat_", "API key"),
    ("shpss_", "API key"),
    ("AIza", "Google API key"),
    ("AKIA", "AWS access key"),
    ("ASIA", "AWS access key"),
    ("dop_v1_", "DigitalOcean token"),
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

fn urls() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s\"'`<>{}()]*"#).expect("valid"))
}

/// `password = hunter2`, `api_key: …`, `token=…` in a URL, `"secret": "…"`,
/// `my password is hunter2`.
fn assignment() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"(?i)\b((?:password|passwd|pwd|passphrase|secret|client[_-]?secret|token|access[_-]?token|refresh[_-]?token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|cookie|credentials?)"?(\s*[:=]\s*|\s+is\s+)(["']?))([^\s"',;&<>]{4,})"#,
        )
        .expect("valid")
    })
}

/// Unquoted `is` also introduces ordinary English prose. Require a secret
/// shape there; explicit delimiters and quotes retain conservative masking.
/// A leading capital alone is ordinary sentence case.
fn prose_secret(value: &str) -> bool {
    value.chars().any(char::is_numeric)
        || (value.chars().any(char::is_lowercase) && value.chars().skip(1).any(char::is_uppercase))
        || word_kind(value).is_some()
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
        && !(w.len() == 36 && uuid::Uuid::parse_str(w).is_ok())
}

fn known_word_kind(w: &str) -> Option<&'static str> {
    for (prefix, what) in PREFIXES {
        if w.starts_with(prefix) && w.len() >= prefix.len() + 16 {
            return Some(what);
        }
    }
    if w.starts_with("eyJ") && w.matches('.').count() == 2 && w.len() > 40 {
        return Some("JWT");
    }
    None
}

/// What kind of secret a single word is, if it is one.
fn word_kind(w: &str) -> Option<&'static str> {
    known_word_kind(w)
        .or_else(|| looks_random(w.trim_end_matches(['.', ':'])).then_some("random-looking token"))
}

fn redact_url(url: &str, found: &mut impl FnMut(&'static str)) -> String {
    let scheme_end = url.find("://").expect("matched URL scheme") + 3;
    let rest = &url[scheme_end..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(authority_end);
    let mut clean = String::with_capacity(url.len());
    clean.push_str(&url[..scheme_end]);
    if let Some((userinfo, host)) = authority.rsplit_once('@') {
        if !userinfo.is_empty() && userinfo != MARK {
            found(
                userinfo
                    .split(':')
                    .find_map(known_word_kind)
                    .unwrap_or("URL credentials"),
            );
            clean.push_str(MARK);
            clean.push('@');
            clean.push_str(host);
        } else {
            clean.push_str(authority);
        }
    } else {
        clean.push_str(authority);
    }
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let (path, suffix) = rest.split_at(path_end);
    for part in path.split_inclusive('/') {
        let segment = part.strip_suffix('/').unwrap_or(part);
        if let Some(kind) = known_word_kind(segment) {
            found(kind);
            clean.push_str(MARK);
            if part.ends_with('/') {
                clean.push('/');
            }
        } else {
            clean.push_str(part);
        }
    }
    clean.push_str(suffix);
    clean
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
    if let std::borrow::Cow::Owned(clean) = urls()
        .replace_all(&out, |captures: &regex::Captures| {
            redact_url(&captures[0], &mut found)
        })
    {
        out = clean;
    }
    if assignment().is_match(&out) {
        let mut any = false;
        out = assignment()
            .replace_all(&out, |c: &regex::Captures| {
                let value = &c[4];
                let explicit = c[2].contains([':', '=']) || !c[3].is_empty();
                if value.starts_with('[') || (!explicit && !prose_secret(value)) {
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
    fn slack_app_and_rotating_tokens_are_redacted_as_whole_values() {
        let suffix = "0123456789-abcdefghijklmnop";
        for prefix in ["xapp-", "xoxe.xapp-", "xoxe.xoxb-", "xoxe.xoxp-", "xoxe-"] {
            assert_eq!(
                redact(&format!("({prefix}{suffix})")),
                ("([REDACTED])".into(), Some("Slack token"))
            );
        }
        let note = "documentation mentions tools-xapp and xapp-short";
        assert_eq!(redact(note), (note.into(), None));
    }

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
    fn urls_redact_userinfo_and_known_path_tokens_without_rewriting_other_parts() {
        let example_key = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        for (text, expected, kind) in [
            (
                format!("https://{example_key}@git.example/org/repo.git"),
                "https://[REDACTED]@git.example/org/repo.git",
                "GitHub token",
            ),
            (
                format!("Open <https://example.test/hook/{example_key}/callback?q=ok>."),
                "Open <https://example.test/hook/[REDACTED]/callback?q=ok>.",
                "GitHub token",
            ),
            (
                "https://user:fixture@example.test/v1".into(),
                "https://[REDACTED]@example.test/v1",
                "URL credentials",
            ),
            (
                format!("https://example.test/?token={example_key}&page=2"),
                "https://example.test/?token=[REDACTED]&page=2",
                "credential",
            ),
        ] {
            assert_eq!(redact(&text), (expected.to_string(), Some(kind)));
            assert_eq!(redact(expected), (expected.to_string(), None));
        }
        for ordinary in [
            "https://git.example",
            "https://example.test/u/alice@example.test",
            "https://example.test/sessions/550E8400-E29b-41D4-A716-446655440000",
        ] {
            assert_eq!(redact(ordinary), (ordinary.to_string(), None));
        }
    }

    #[test]
    fn digitalocean_personal_tokens_are_redacted_without_matching_short_mentions() {
        let suffix = "0123456789abcdef".repeat(4);
        let input = format!("credential rejected: (dop_v1_{suffix})");
        let (redacted, kind) = redact(&input);
        assert_eq!(redacted, "credential rejected: ([REDACTED])");
        assert_eq!(kind, Some("DigitalOcean token"));

        let notes = format!("examples: dop_v1_short dop_{suffix}");
        assert_eq!(redact(&notes), (notes, None));
    }

    #[test]
    fn finds_and_blanks_secret_shapes_not_key_names() {
        for prefix in ["gldt-", "glrt-"] {
            let token = format!("{prefix}0123456789abcdefghij");
            assert_eq!(redact(&token), (MARK.into(), Some("GitLab token")));
        }
        let note = "GitLab documentation includes gldt-short and glpat-";
        assert_eq!(redact(note), (note.into(), None));

        let cases = [
            ("key sk-proj-abcdefghijklmnopqrstuvwxyz0123 here", "API key"),
            ("Authorization: Bearer abcdefghijklmnop1234", "bearer token"),
            ("my passwd: hunter22", "credential"),
            ("https://x.dev/cb?token=abc123def456&x=1", "credential"),
            ("cookie=sessionid1234", "credential"),
            ("token=550E8400-E29b-41D4-A716-446655440000", "credential"),
            ("{\"client_secret\": \"s3cr3tvalue\"}", "credential"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "GitHub token"),
            ("aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3h", "random-looking token"),
            (
                "550G8400-E29b-41D4-A716-446655440000",
                "random-looking token",
            ),
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
            "550E8400-E29b-41D4-A716-446655440000",
            "550e8400-e29b-41d4-a716-446655440000",
            "550E8400-E29B-41D4-A716-446655440000",
            "{550E8400-E29b-41D4-A716-446655440000}",
        ] {
            assert_eq!(redact(text), (text.to_string(), None), "{text}");
        }
        // Already redacted stays as it is.
        assert_eq!(redact("token=[REDACTED]").1, None);
    }

    #[test]
    fn github_app_refresh_tokens_are_scanned_and_redacted() {
        let token = format!("ghr_{}", "0123456789abcdefghijklmnop");
        assert_eq!(scan(&token), Some("GitHub token"));
        assert_eq!(redact(&token), (MARK.into(), Some("GitHub token")));
        assert_eq!(redact("ghr_short"), ("ghr_short".into(), None));
    }

    #[test]
    fn ordinary_is_prose_is_not_a_credential_assignment() {
        for text in [
            "The token is refreshed nightly",
            "password is required before login",
            "The secret is stored in the keychain",
            "cookie is httponly",
            "the passphrase is correct",
            "The token IS Refreshed nightly",
            "PASSWORD IS REQUIRED before login",
            "tokenizer is faster",
            "credential is optional",
            "The secret\nis\nstored in the keychain",
        ] {
            assert_eq!(redact(text), (text.to_string(), None), "{text}");
            assert_eq!(scan(text), None, "{text}");
        }
    }

    #[test]
    fn shopify_admin_tokens_and_shared_secrets_are_redacted_by_exact_prefix() {
        let suffix = "0123456789abcdef0123456789abcdef";
        for prefix in ["shpat_", "shpss_"] {
            assert_eq!(
                redact(&format!("({prefix}{suffix})")),
                ("([REDACTED])".into(), Some("API key"))
            );
        }
        let note = format!("examples: shpat_abc shpss_short shp_{suffix}");
        assert_eq!(redact(&note), (note, None));
    }

    #[test]
    fn explicit_and_secret_shaped_is_assignments_are_still_redacted() {
        for (text, value) in [
            ("my password is hunter2", "hunter2"),
            ("password IS 123456", "123456"),
            ("password is １２３４", "１２３４"),
            ("the passphrase is hUnTeR", "hUnTeR"),
            (
                "token is sk-abcdefghijklmnopqrstuvwxyz",
                "sk-abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "token is aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3h",
                "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dF3h",
            ),
            ("password is 'sunshine'", "sunshine"),
            ("password is \"sunshine\"", "sunshine"),
            ("token=refreshed", "refreshed"),
            ("password: required", "required"),
            (r#"{"client_secret": "stored"}"#, "stored"),
            ("https://example.test/?token=correct&x=1", "correct"),
        ] {
            let (out, kind) = redact(text);
            assert_eq!(kind, Some("credential"), "{text}: {out}");
            assert!(!out.contains(value), "{text}: {out}");
            assert!(out.contains(MARK), "{text}: {out}");
            assert_eq!(scan(text), Some("credential"), "{text}");
            assert_eq!(
                redact(&out),
                (out.clone(), None),
                "redaction stays idempotent"
            );
        }
    }

    #[test]
    fn stripe_webhook_signing_secrets_use_the_shared_token_floor() {
        for suffix in [
            "0123456789abcdef0123456789abcdef",
            "abcdefghijklmnop0123456789",
        ] {
            assert_eq!(
                redact(&format!("signature setting (whsec_{suffix})")),
                ("signature setting ([REDACTED])".into(), Some("API key"))
            );
        }
        let note = "docs mention whsec_abc in a webhook example";
        assert_eq!(redact(note), (note.into(), None));
    }

    #[test]
    fn known_values_still_mask_plain_words_in_is_prose() {
        assert_eq!(
            redact_known("password is sunshine", &["sunshine"]),
            "password is [REDACTED]"
        );
        assert_eq!(
            redact("password is [REDACTED]"),
            ("password is [REDACTED]".into(), None)
        );
    }

    #[test]
    fn npm_access_tokens_are_redacted_without_matching_short_prefix_mentions() {
        let token = format!("npm_{}", "abcdefghijklmnopqrstuvwxyz012345");
        assert_eq!(scan(&token), Some("npm token"));
        assert_eq!(redact(&token), (MARK.into(), Some("npm token")));
        let note = "npm documentation mentions npm_short";
        assert_eq!(redact(note), (note.into(), None));
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

    #[test]
    fn stripe_restricted_test_keys_are_redacted() {
        let key = format!("rk_test_{}", "512345678901234567890123");
        assert_eq!(redact(&key), (MARK.into(), Some("API key")));
        assert_eq!(redact("rk_test_short"), ("rk_test_short".into(), None));
    }
}
