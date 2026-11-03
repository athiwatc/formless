//! The user docs (SPEC §10.9): topics `/docs` and `formless docs` open,
//! and opening a URL in the user's browser.

/// Where the docs live (the repository's `docs/` and SPEC).
pub const BASE: &str = "https://github.com/athiwatc/formless/blob/main/";

/// (topic, path under BASE, what it covers).
pub const TOPICS: &[(&str, &str, &str)] = &[
    (
        "gallery",
        "docs/gallery.md",
        "what Formless can do: example tasks",
    ),
    (
        "capabilities",
        "docs/capabilities.md",
        "what Tools can access, risks, trust settings",
    ),
    (
        "approvals",
        "docs/capabilities.md#are-you-asked",
        "when you are asked, and how to undo",
    ),
    ("keys", "docs/keys.md", "the main keys and every command"),
    (
        "troubleshooting",
        "docs/troubleshooting.md",
        "common problems and fixes",
    ),
    ("mcp", "SPEC.md#79-external-mcp-servers", "MCP servers"),
    ("skills", "SPEC.md#710-skills", "Skills"),
    ("jobs", "SPEC.md#97-scheduled-jobs", "scheduled jobs"),
    ("spec", "SPEC.md", "the full specification"),
];

/// A topic mistake, distinct from the default gallery or a unique prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopicError {
    Unknown(String),
    Ambiguous {
        input: String,
        candidates: Vec<&'static str>,
    },
}

impl std::fmt::Display for TopicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(input) => write!(f, "unknown docs topic {input:?}"),
            Self::Ambiguous { input, candidates } => write!(
                f,
                "ambiguous docs topic {input:?}: matches {}",
                candidates.join(", ")
            ),
        }
    }
}
impl std::error::Error for TopicError {}

/// Resolve an exact name or an unambiguous prefix; omitted/blank means gallery.
pub fn resolve(topic: Option<&str>) -> Result<String, TopicError> {
    let input = topic.unwrap_or("gallery").trim().to_ascii_lowercase();
    let input = if input.is_empty() {
        "gallery".to_string()
    } else {
        input
    };
    if let Some((_, path, _)) = TOPICS.iter().find(|(name, _, _)| *name == input) {
        return Ok(format!("{BASE}{path}"));
    }
    let matching: Vec<_> = TOPICS
        .iter()
        .filter(|(name, _, _)| name.starts_with(&input))
        .collect();
    match matching.as_slice() {
        [(_, path, _)] => Ok(format!("{BASE}{path}")),
        [] => Err(TopicError::Unknown(input)),
        _ => Err(TopicError::Ambiguous {
            input,
            candidates: matching.iter().map(|(name, _, _)| *name).collect(),
        }),
    }
}

/// A compatibility lookup for callers that only need a valid URL.
pub fn url(topic: Option<&str>) -> Option<String> {
    resolve(topic).ok()
}

/// Pure launch decision: remote shells never open a browser on the remote host;
/// Linux requires a graphical session. Empty environment values count as unset.
fn should_open(
    linux: bool,
    ssh_connection: bool,
    ssh_tty: bool,
    display: bool,
    wayland: bool,
) -> bool {
    !ssh_connection && !ssh_tty && (!linux || display || wayland)
}

fn wait_for_opener(child: &mut std::process::Child, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(), // try_wait also reaps it.
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait(); // Reap even on timeout or a polling error.
                return false;
            }
        }
    }
}

fn run_opener(cmd: &mut std::process::Command, timeout: std::time::Duration) -> bool {
    match cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => wait_for_opener(&mut child, timeout),
        Err(_) => false,
    }
}

/// Ask the OS opener to open a URL. True requires a successful opener exit,
/// not merely a spawn; it cannot prove that a browser finished navigation.
/// A stalled opener is stopped/reaped after two seconds; callers retain the URL.
pub fn open_url(url: &str) -> bool {
    let set = |key| std::env::var_os(key).is_some_and(|value| !value.is_empty());
    if !should_open(
        cfg!(target_os = "linux"),
        set("SSH_CONNECTION"),
        set("SSH_TTY"),
        set("DISPLAY"),
        set("WAYLAND_DISPLAY"),
    ) {
        return false;
    }
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url);
    run_opener(&mut cmd, std::time::Duration::from_secs(2))
}

#[cfg(test)]
mod tests {
    #[test]
    fn topics_resolve_by_name_or_prefix() {
        assert!(super::url(Some("cap"))
            .unwrap()
            .ends_with("docs/capabilities.md"));
        assert!(super::url(Some("Troubleshooting"))
            .unwrap()
            .ends_with("troubleshooting.md"));
        assert!(super::url(None).unwrap().ends_with("gallery.md"));
        assert_eq!(super::url(Some("nope")), None);
        // Every docs page a topic names exists in the repository.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (_, path, _) in super::TOPICS {
            let file = path.split('#').next().unwrap();
            assert!(root.join(file).exists(), "{file}");
        }
    }

    #[test]
    fn resolution_distinguishes_exact_unique_unknown_and_ambiguous() {
        use super::{resolve, TopicError};
        assert_eq!(resolve(None), resolve(Some("  ")));
        assert!(resolve(Some(" SKILLS "))
            .unwrap()
            .ends_with("SPEC.md#710-skills"));
        assert!(resolve(Some("spe")).unwrap().ends_with("SPEC.md"));
        assert_eq!(
            resolve(Some("s")),
            Err(TopicError::Ambiguous {
                input: "s".into(),
                candidates: vec!["skills", "spec"]
            })
        );
        assert_eq!(
            resolve(Some("nope")),
            Err(TopicError::Unknown("nope".into()))
        );
        assert_eq!(super::url(Some("s")), None);
        assert!(resolve(Some("nope"))
            .unwrap_err()
            .to_string()
            .contains("unknown docs topic \"nope\""));
        assert!(resolve(Some("s"))
            .unwrap_err()
            .to_string()
            .contains("matches skills, spec"));
    }

    #[test]
    fn launch_decision_is_remote_and_graphical_session_aware() {
        for linux in [false, true] {
            for ssh in [(true, false), (false, true), (true, true)] {
                assert!(!super::should_open(linux, ssh.0, ssh.1, true, true));
            }
        }
        assert!(!super::should_open(true, false, false, false, false));
        assert!(super::should_open(true, false, false, true, false));
        assert!(super::should_open(true, false, false, false, true));
        assert!(super::should_open(false, false, false, false, false));
    }

    // An injected test executable, not xdg-open/open/cmd or a real browser.
    fn opener_fixture(mode: &str) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "docs::tests::opener_child_fixture",
                "--nocapture",
            ])
            .env("FORMLESS_DOCS_OPENER_FIXTURE", mode);
        command
    }

    #[test]
    fn opener_child_fixture() {
        match std::env::var("FORMLESS_DOCS_OPENER_FIXTURE").as_deref() {
            Ok("fail") => std::process::exit(7),
            Ok("hang") => loop {
                std::thread::park();
            },
            _ => {}
        }
    }

    #[test]
    fn opener_requires_success_and_reaps_success_failure_and_timeout() {
        use std::{
            process::Stdio,
            time::{Duration, Instant},
        };
        for (mode, success) in [("ok", true), ("fail", false), ("hang", false)] {
            let mut child = opener_fixture(mode)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let start = Instant::now();
            assert_eq!(
                super::wait_for_opener(
                    &mut child,
                    if mode == "hang" {
                        Duration::from_millis(100)
                    } else {
                        Duration::from_secs(2)
                    }
                ),
                success,
                "{mode}"
            );
            assert!(
                child.try_wait().unwrap().is_some(),
                "{mode} must be finished and reaped"
            );
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{mode} exceeded bounded wait"
            );
        }
        assert!(!super::run_opener(
            &mut std::process::Command::new("formless-nonexistent-docs-opener-fixture"),
            Duration::from_millis(100)
        ));
    }

    /// GitHub's heading anchor: lower-case, punctuation dropped, spaces
    /// as hyphens.
    fn slug(heading: &str) -> String {
        heading
            .trim_start_matches('#')
            .trim()
            .chars()
            .filter_map(|c| match c {
                ' ' | '-' => Some('-'),
                c if c.is_alphanumeric() || c == '_' => Some(c.to_ascii_lowercase()),
                _ => None,
            })
            .collect()
    }

    /// Every `#anchor` lands on a heading (renumbering SPEC breaks them),
    /// and the web UI's topic table is this one (#1082).
    #[test]
    fn anchors_resolve_and_the_web_topics_match() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for (_, path, _) in super::TOPICS {
            let Some((file, anchor)) = path.split_once('#') else {
                continue;
            };
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            let found = text
                .lines()
                .filter(|l| l.starts_with('#'))
                .any(|l| slug(l) == anchor);
            assert!(found, "{path}: no heading for #{anchor}");
        }
        let app = std::fs::read_to_string(root.join("relay/web/app.js")).unwrap();
        let line = app
            .lines()
            .find(|l| l.trim_start().starts_with("const topics = {"))
            .expect("the web /docs topic table");
        let body = &line[line.find('{').unwrap() + 1..line.rfind('}').unwrap()];
        let web: Vec<(String, String)> = body
            .split(", ")
            .map(|pair| {
                let (k, v) = pair.split_once(": ").unwrap();
                (k.trim().to_string(), v.trim().trim_matches('"').to_string())
            })
            .collect();
        let ours: Vec<(String, String)> = super::TOPICS
            .iter()
            .map(|(n, p, _)| (n.to_string(), p.to_string()))
            .collect();
        assert_eq!(
            web, ours,
            "relay/web/app.js /docs topics differ from docs::TOPICS"
        );
    }
}
