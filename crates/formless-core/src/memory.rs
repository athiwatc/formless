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

/// Does a note hold a personal detail the user may not want kept
/// (`memory.confirm_writes = "sensitive"`): health, money, contacts,
/// where they live, family, beliefs? A word-list heuristic.
pub fn looks_sensitive(text: &str) -> bool {
    let t = text.to_lowercase();
    const WORDS: &[&str] = &[
        "health",
        "medical",
        "diagnos",
        "medication",
        "doctor",
        "therapy",
        "pregnan",
        "salary",
        "income",
        "debt",
        "bank",
        "account number",
        "credit card",
        "tax",
        "address",
        "lives in",
        "live in",
        "home is",
        "phone",
        "email",
        "birthday",
        "date of birth",
        "wife",
        "husband",
        "partner",
        "child",
        "kids",
        "daughter",
        "son ",
        "religio",
        "church",
        "politic",
        "vote",
        "sexual",
        "ethnic",
        "passport",
        "ssn",
    ];
    WORDS.iter().any(|w| t.contains(w))
        || t.contains('@')
        || t.chars().filter(char::is_ascii_digit).count() >= 7
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
    // The same scanner every stored or sent text goes through (#709).
    crate::secrets::scan(text)
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
