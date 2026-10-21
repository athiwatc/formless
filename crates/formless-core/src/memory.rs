//! User memory (SPEC §7.11): short, agent-written notes about stable user
//! preferences and facts, visible to the user and injected into new
//! Sessions' system prompts.

use anyhow::{bail, Result};
use formless_protocol::MemoryInfo;

/// Longest note, in characters.
pub const MAX_CHARS: usize = 300;
/// Longest optimiser note (a migrated proposal, up to 800 characters).
pub const MAX_OPTIMISER_CHARS: usize = 800;

/// Trimmed text with whitespace collapsed (what is stored).
pub fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercased [`clean`] text: two notes with the same key are duplicates.
pub fn dedup_key(text: &str) -> String {
    clean(text).to_lowercase()
}

/// Validate a note and its scope: `global` or `project:<absolute path>`.
pub fn validate(text: &str, scope: &str) -> Result<()> {
    validate_len(text, MAX_CHARS)?;
    validate_scope(scope)
}

pub(crate) fn validate_len(text: &str, max: usize) -> Result<()> {
    let text = clean(text);
    if text.is_empty() {
        bail!("memory text is empty");
    }
    if let Some(what) = find_secret(&text) {
        bail!("refused: memory looks like it contains a secret ({what}); memory is sent to the model every Session, so never store credentials");
    }
    if text.chars().count() > max {
        bail!("memory text is longer than {max} characters; keep notes short");
    }
    Ok(())
}

pub(crate) fn validate_scope(scope: &str) -> Result<()> {
    match scope.strip_prefix("project:") {
        _ if scope == "global" => Ok(()),
        Some(path) if path.starts_with('/') || path.contains(":\\") => Ok(()),
        _ => bail!("scope must be \"global\" or \"project:<absolute path>\""),
    }
}

/// What kind of secret `text` seems to contain, if any: known key and
/// token formats, private keys, bearer tokens, JWTs, `password: …`-style
/// assignments and long random-looking strings.
pub fn find_secret(text: &str) -> Option<&'static str> {
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
    if text.contains("PRIVATE KEY-----") {
        return Some("private key");
    }
    let lower = text.to_lowercase();
    if lower.contains("bearer ") {
        let after = &lower[lower.find("bearer ").unwrap() + 7..];
        if after
            .split_whitespace()
            .next()
            .is_some_and(|t| t.len() >= 16)
        {
            return Some("bearer token");
        }
    }
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || "\"'`,;()[]{}<>".contains(c))
        .filter(|w| !w.is_empty())
        .collect();
    for w in &words {
        for (prefix, what) in PREFIXES {
            if w.starts_with(prefix) && w.len() >= prefix.len() + 16 {
                return Some(what);
            }
        }
        if w.starts_with("eyJ") && w.matches('.').count() == 2 && w.len() > 40 {
            return Some("JWT");
        }
        if looks_random(w.trim_end_matches(['.', ':'])) {
            return Some("random-looking token");
        }
    }
    // "password is hunter2", "api_key=…", "token: …"
    for (i, w) in words.iter().enumerate() {
        let lw = w.to_lowercase();
        let (key, inline) = match lw.split_once(['=', ':']) {
            Some((k, v)) => (k.to_string(), (!v.is_empty()).then(|| v.to_string())),
            None => (lw.clone(), None),
        };
        let key = key.trim_matches(|c: char| !c.is_alphanumeric());
        let secretish = matches!(
            key,
            "password"
                | "passwd"
                | "pwd"
                | "passphrase"
                | "secret"
                | "token"
                | "apikey"
                | "api_key"
                | "api-key"
                | "pin"
        );
        if !secretish {
            continue;
        }
        let value = inline.or_else(|| {
            let next = words.get(i + 1)?;
            let next = if matches!(*next, "is" | "=" | ":") {
                words.get(i + 2)?
            } else if lw.ends_with([':', '=']) {
                next
            } else {
                return None;
            };
            Some(next.to_string())
        });
        if value.is_some_and(|v| v.len() >= 4) {
            return Some("credential");
        }
    }
    None
}

/// A long run of mixed-case letters and digits with no structure: an
/// API key or token rather than a word, path or hash-like id in prose.
fn looks_random(w: &str) -> bool {
    w.len() >= 32
        && !w.contains('/')
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_+=".contains(c))
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_lowercase())
        && w.chars().any(|c| c.is_ascii_uppercase())
}

/// The "User memory" block for the system prompt (empty without notes):
/// global notes first, then project notes grouped by path.
pub fn prompt_block(memories: &[MemoryInfo]) -> String {
    let enabled: Vec<&MemoryInfo> = memories.iter().filter(|m| m.enabled).collect();
    if enabled.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\n\nUser memory (notes the user can see and edit; follow them, and remember/forget as the user asks):",
    );
    for m in enabled.iter().filter(|m| m.scope == "global") {
        out.push_str(&format!("\n- {} [{}]", m.text, m.id));
    }
    let mut projects: Vec<&str> = enabled
        .iter()
        .filter_map(|m| m.scope.strip_prefix("project:"))
        .collect();
    projects.sort_unstable();
    projects.dedup();
    for p in projects {
        out.push_str(&format!("\nWhen working in {p}:"));
        for m in enabled
            .iter()
            .filter(|m| m.scope.strip_prefix("project:") == Some(p))
        {
            out.push_str(&format!("\n- {} [{}]", m.text, m.id));
        }
    }
    out
}
