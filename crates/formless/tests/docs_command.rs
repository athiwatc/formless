//! Read-only documentation commands: no daemon, browser launch or profile writes.
use std::{
    path::PathBuf,
    process::{Command, Output},
};

fn docs(args: &[&str]) -> (PathBuf, Output) {
    let root = std::env::temp_dir().join(format!("formless-docs-command-{}", uuid_token()));
    let output = Command::new(env!("CARGO_BIN_EXE_formless"))
        .args(args)
        .env("FORMLESS_DATA_DIR", root.join("data"))
        .env("FORMLESS_CONFIG_DIR", root.join("config"))
        .env("SSH_CONNECTION", "fixture remote shell")
        .env("PATH", &root) // A regression cannot find a real desktop opener.
        .output()
        .unwrap();
    (root, output)
}
fn uuid_token() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[test]
fn omitted_topic_prints_gallery_and_needs_no_profile() {
    for args in [vec!["docs", "--print"], vec!["docs"]] {
        let (root, output) = docs(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            stdout.trim(),
            "https://github.com/athiwatc/formless/blob/main/docs/gallery.md"
        );
        assert!(!root.exists(), "docs should not create a profile");
    }
}

#[test]
fn topic_mistakes_exit_two_and_explain_unknown_or_ambiguous_input() {
    for (topic, required) in [
        ("nope", "unknown docs topic \"nope\""),
        ("s", "matches skills, spec"),
    ] {
        let (root, output) = docs(&["docs", topic, "--print"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(required), "{stderr}");
        assert!(
            stderr.contains("TOPIC") && stderr.contains("troubleshooting"),
            "{stderr}"
        );
        assert!(!root.exists());
    }
    let (root, output) = docs(&["docs", "spe", "--print"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "https://github.com/athiwatc/formless/blob/main/SPEC.md"
    );
    assert!(!root.exists());
}
