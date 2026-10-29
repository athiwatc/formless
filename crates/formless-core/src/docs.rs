//! The user docs (SPEC §10.9): topics `/docs` and `formless docs` open,
//! and opening a URL in the user's browser.

/// Where the docs live (the repository's `docs/` and SPEC).
pub const BASE: &str = "https://github.com/athiwatc/formless/blob/main/";

/// (topic, path under BASE, what it covers).
pub const TOPICS: &[(&str, &str, &str)] = &[
    ("gallery", "docs/gallery.md", "what Formless can do: example tasks"),
    ("capabilities", "docs/capabilities.md", "what Tools can access, risks, trust settings"),
    ("approvals", "docs/capabilities.md#are-you-asked", "when you are asked, and how to undo"),
    ("keys", "docs/keys.md", "every key and command"),
    ("troubleshooting", "docs/troubleshooting.md", "common problems and fixes"),
    ("mcp", "SPEC.md#79-external-mcp-servers", "MCP servers"),
    ("skills", "SPEC.md#710-skills", "Skills"),
    ("jobs", "SPEC.md#97-scheduled-jobs", "scheduled jobs"),
    ("spec", "SPEC.md", "the full specification"),
];

/// The URL for a topic (or a close match), or the docs index for none.
pub fn url(topic: Option<&str>) -> Option<String> {
    let t = topic.map(|t| t.trim().to_ascii_lowercase()).filter(|t| !t.is_empty());
    let Some(t) = t else {
        return Some(format!("{BASE}docs/gallery.md"));
    };
    TOPICS
        .iter()
        .find(|(name, _, _)| *name == t)
        .or_else(|| TOPICS.iter().find(|(name, _, _)| name.starts_with(&t)))
        .map(|(_, path, _)| format!("{BASE}{path}"))
}

/// Open `url` in the default browser; false when that isn't possible.
pub fn open_url(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn topics_resolve_by_name_or_prefix() {
        assert!(super::url(Some("cap")).unwrap().ends_with("docs/capabilities.md"));
        assert!(super::url(Some("Troubleshooting")).unwrap().ends_with("troubleshooting.md"));
        assert!(super::url(None).unwrap().ends_with("gallery.md"));
        assert_eq!(super::url(Some("nope")), None);
        // Every docs page a topic names exists in the repository.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (_, path, _) in super::TOPICS {
            let file = path.split('#').next().unwrap();
            assert!(root.join(file).exists(), "{file}");
        }
    }
}
