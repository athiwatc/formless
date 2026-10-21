//! User memory (SPEC §7.11): short, agent-written notes about stable user
//! preferences and facts, visible to the user and injected into new
//! Sessions' system prompts.

use anyhow::{bail, Result};
use formless_protocol::MemoryInfo;

/// Longest note, in characters.
pub const MAX_CHARS: usize = 300;

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
    let text = clean(text);
    if text.is_empty() {
        bail!("memory text is empty");
    }
    if text.chars().count() > MAX_CHARS {
        bail!("memory text is longer than {MAX_CHARS} characters; keep notes short");
    }
    match scope.strip_prefix("project:") {
        _ if scope == "global" => Ok(()),
        Some(path) if path.starts_with('/') || path.contains(":\\") => Ok(()),
        _ => bail!("scope must be \"global\" or \"project:<absolute path>\""),
    }
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
