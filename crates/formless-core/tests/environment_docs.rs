//! Conservative inventory: every source reference, not just direct env reads.
use std::{collections::BTreeSet, path::Path};

fn documented(name: &str, docs: &str) -> bool {
    name == "FORMLESS_DAEMON_LOG"
        || docs.contains(name)
        || (name.starts_with("FORMLESS_API_KEY_")
            && docs.contains("FORMLESS_API_KEY_<NAME>")
            && (!name.ends_with("_FILE") || docs.contains("FORMLESS_API_KEY_<NAME>_FILE")))
}

fn references(dir: &Path, names: &mut BTreeSet<String>, pattern: &regex::Regex) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            references(&path, names, pattern);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            names.extend(pattern.find_iter(&text).map(|m| m.as_str().to_string()));
        }
    }
}

#[test]
fn every_formless_environment_reference_in_crate_sources_is_documented() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let docs = format!(
        "{}\n{}",
        std::fs::read_to_string(root.join("README.md")).unwrap(),
        std::fs::read_to_string(root.join("SPEC.md")).unwrap()
    );
    let pattern = regex::Regex::new(r"\bFORMLESS_[A-Z0-9_]+\b").unwrap();
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            references(&src, &mut names, &pattern);
        }
    }
    assert!(names.contains("FORMLESS_MODEL") && names.contains("FORMLESS_API_KEY_FILE"));
    let missing: Vec<_> = names
        .iter()
        .filter(|name| !documented(name, &docs))
        .collect();
    assert!(
        missing.is_empty(),
        "undocumented environment references: {missing:?}"
    );
    assert!(!documented("FORMLESS_UNDOCUMENTED_SENTINEL", &docs));
}
