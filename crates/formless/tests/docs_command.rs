//! Read-only documentation commands: no daemon, browser launch or profile writes.
use std::{
    path::PathBuf,
    process::{Command, Output},
};

fn docs(args: &[&str]) -> (PathBuf, Output) {
    let root = std::env::temp_dir().join(format!("formless-docs-command-{}", uuid_token()));
    let cwd = std::env::temp_dir().join(format!("formless-docs-cwd-{}", uuid_token()));
    std::fs::create_dir(&cwd).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_formless"))
        .current_dir(&cwd)
        .args(args)
        .env("FORMLESS_DATA_DIR", root.join("data"))
        .env("FORMLESS_CONFIG_DIR", root.join("config"))
        .env("SSH_CONNECTION", "fixture remote shell")
        .env("PATH", &root) // A regression cannot find a real desktop opener.
        .output()
        .unwrap();
    std::fs::remove_dir(&cwd).unwrap();
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
        let url = "https://github.com/athiwatc/formless/blob/main/docs/gallery.md";
        if args.contains(&"--print") {
            assert_eq!(stdout.trim(), url);
        } else {
            assert!(stdout.starts_with("Bundled guide (Formless "));
            assert!(stdout.contains(include_str!("../../../docs/gallery.md")));
            assert!(stdout.contains(&format!(
                "Current online copy (may differ from this build): {url}"
            )));
        }
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

#[test]
fn task_topics_print_the_user_guides_without_creating_a_profile() {
    for topic in ["mcp", "skills", "jobs"] {
        let (root, output) = docs(&["docs", topic, "--print"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            format!("https://github.com/athiwatc/formless/blob/main/docs/{topic}.md")
        );
        assert!(!root.exists(), "docs should not create a profile");
    }
}

#[test]
fn text_is_the_compiled_markdown_and_needs_no_profile_or_working_tree() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (topic, path, _) in formless_core::docs::TOPICS {
        let (root, output) = docs(&["docs", topic, "--text"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = if path.contains('#') {
            formless_core::docs::text(Some(topic))
                .unwrap()
                .as_bytes()
                .to_vec()
        } else {
            std::fs::read(source.join(path)).unwrap()
        };
        assert_eq!(output.stdout, expected, "{topic}");
        assert!(!root.exists());
    }
    for topic in ["nope", "s"] {
        let (root, output) = docs(&["docs", topic, "--text"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!root.exists());
    }
}

#[cfg(unix)]
#[test]
fn bundled_spec_can_be_piped_to_a_reader_that_closes_early() {
    use std::io::Read;
    use std::process::Stdio;
    let root = std::env::temp_dir().join(format!("formless-docs-pipe-{}", uuid_token()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_formless"))
        .args(["docs", "spec", "--text"])
        .env("FORMLESS_DATA_DIR", root.join("data"))
        .env("FORMLESS_CONFIG_DIR", root.join("config"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    stdout.read_exact(&mut [0u8; 1]).unwrap();
    drop(stdout);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(!root.exists());
}
