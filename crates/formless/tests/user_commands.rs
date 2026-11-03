//! Shipped read-only command discovery. Private files, no daemon/provider,
//! inherited credentials, template expansion or execution.
use serde_json::Value;
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
struct Fixture {
    root: PathBuf,
    config: PathBuf,
    data: PathBuf,
    project: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fl-commands-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        // Commands run with an OS-resolved cwd; compare the same actual paths.
        let root = root.canonicalize().unwrap();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            config: root.join("config"),
            data: root.join("data"),
            root,
            project,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        command.env_clear();
        // Keep only OS process prerequisites; no owner/provider credentials.
        for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = command
            .args(args)
            .current_dir(&self.project)
            .env("FORMLESS_CONFIG_DIR", &self.config)
            .env("FORMLESS_DATA_DIR", &self.data)
            .env("RUST_LOG", "error")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let err = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out.join();
                let _ = err.join();
                panic!("read-only command discovery exceeded its deadline (possible daemon or attachment access)");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        }
    }
    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "JSON diagnostics belong in the structured projection, not stderr"
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn file(&self, project: bool, name: &str, content: &str) {
        let dir = if project {
            self.project.join(".formless/commands")
        } else {
            self.config.join("commands")
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), content).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn commands_list_missing_roots_creates_no_profile_or_daemon_state() {
    let f = Fixture::new();
    for args in [
        vec!["commands", "list", "--json"],
        vec!["--profile", "uncreated", "commands", "list", "--json"],
    ] {
        let value = f.json(&args);
        assert_eq!(value["commands"], serde_json::json!([]));
        assert_eq!(value["diagnostics"], serde_json::json!([]));
        assert_eq!(value["shadowed"], serde_json::json!([]));
        assert!(
            !f.data.exists(),
            "listing must not create the data directory, DB, lock, socket or daemon log"
        );
        assert!(
            !f.config.exists(),
            "listing must not create the config/profile directory"
        );
    }
}

#[test]
fn commands_list_mirrors_current_metadata_overrides_and_builtin_shadowing_without_expansion() {
    let f = Fixture::new();
    f.file(
        false,
        "workflow.md",
        "---\ndescription: Global review\n---\nGlobal $ARGUMENTS\n",
    );
    f.file(true,"workflow.md","---\ndescription: Project review\n---\nReview $1 then $ARGUMENTS @never-open-this-file @blocked-attachment\n!touch should-not-exist\n$(touch should-not-exist)\n");
    f.file(
        false,
        "release.md",
        "---\ndescription: Release notes\n---\nWrite notes for $ARGUMENTS.\n",
    );
    f.file(
        true,
        "quit.md",
        "---\ndescription: Must not replace builtin quit\n---\nDo not run this.\n",
    );
    // An invalid config proves the listing does not load provider/profile
    // configuration, and literal command text/mentions cannot execute or read.
    std::fs::write(
        f.config.join("config.toml"),
        "this is deliberately invalid TOML = [",
    )
    .unwrap();
    #[cfg(unix)]
    {
        // An accidental attachment read would block: the bounded shipped-CLI
        // runner must finish without opening this named pipe.
        let path = f.project.join("blocked-attachment");
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    let value = f.json(&["commands", "list", "--json"]);
    let expected = formless_core::user_commands::load(
        &f.config,
        Some(&f.project),
        &formless_tui::builtin_command_names(),
    )
    .projection();
    assert_eq!(
        value,
        serde_json::to_value(expected).unwrap(),
        "CLI mirrors the same shared metadata projection as the TUI"
    );
    let commands = value["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    let review = commands.iter().find(|c| c["name"] == "workflow").unwrap();
    assert_eq!(review["description"], "Project review");
    assert_eq!(review["source"], "project");
    assert!(
        !commands.iter().any(|c| c["name"] == "quit"),
        "builtin wins"
    );
    let shadowed = value["shadowed"].as_array().unwrap();
    assert!(shadowed.iter().any(|c| c["name"] == "workflow"));
    assert!(shadowed.iter().any(|c| c["name"] == "quit"));
    assert!(
        !String::from_utf8(serde_json::to_vec(&value).unwrap())
            .unwrap()
            .contains("never-open-this-file"),
        "template bodies are not exposed by metadata listing"
    );
    assert!(!f.project.join("should-not-exist").exists());
    assert!(!f.data.exists());
    let text = f.run(&["commands", "list"]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains("/workflow") && text.contains("Project review") && text.contains("/quit")
    );
    // A later file edit is visible in the very next invocation, with no daemon.
    f.file(
        true,
        "workflow.md",
        "---\ndescription: Changed review\n---\nChanged body\n",
    );
    let changed = f.json(&["commands", "list", "--json"]);
    assert_eq!(
        changed["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "workflow")
            .unwrap()["description"],
        "Changed review"
    );
}

#[test]
fn malformed_templates_are_structured_diagnostics_and_do_not_hide_valid_commands() {
    let f = Fixture::new();
    f.file(
        false,
        "valid.md",
        "---\ndescription: Still visible\n---\nLiteral prompt\n",
    );
    f.file(false, "broken.md", "---\ndescription: never closed\n");
    f.file(
        true,
        "bad name.md",
        "---\ndescription: Invalid name\n---\nPrompt\n",
    );
    let first = f.json(&["commands", "list", "--json"]);
    assert_eq!(first["commands"].as_array().unwrap().len(), 1);
    assert_eq!(first["commands"][0]["name"], "valid");
    let diagnostics = first["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 2);
    for diagnostic in diagnostics {
        assert!(diagnostic["key"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(diagnostic["message"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
        assert!(diagnostic["path"].is_string());
    }
    assert_eq!(
        first,
        f.json(&["commands", "list", "--json"]),
        "repeated discovery has stable diagnostics"
    );
    assert!(!f.data.exists());
}

#[test]
fn command_help_man_and_all_completion_dialects_include_discovery_without_profile_setup() {
    let f = Fixture::new();
    for args in [
        vec!["--help"],
        vec!["commands", "--help"],
        vec!["commands", "list", "--help"],
        vec!["man"],
    ] {
        let out = f.run(&args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.to_ascii_lowercase().contains("commands"), "{args:?}");
        if args[0] == "man" {
            assert!(text.contains("FORMLESS COMMANDS LIST"));
            assert!(text.contains("--json") || text.contains("\\-\\-json"));
        }
    }
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let out = f.run(&["completions", shell]);
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains("commands"), "{shell}");
    }
    assert!(!f.config.exists());
    assert!(!f.data.exists());
}

#[cfg(unix)]
#[test]
fn human_listing_escapes_terminal_metadata_while_json_keeps_the_original_paths() {
    let mut f = Fixture::new();
    f.config = f.root.join("config-\u{001b}[31m-\u{202e}ไทย");
    f.project = f.root.join("project-\u{007f}-\u{2066}ไทย");
    std::fs::create_dir_all(&f.project).unwrap();
    let description =
        serde_json::to_string("ไทย project description\u{001b}[2J\u{202e}end").unwrap();
    f.file(
        false,
        "workflow.md",
        "---\ndescription: Global ไทย description\n---\nBODY MUST STAY PRIVATE",
    );
    f.file(
        true,
        "workflow.md",
        &format!("---\ndescription: {description}\n---\nPRIVATE BODY"),
    );
    f.file(
        true,
        "bad-\u{001b}[2J-\u{202e}.md",
        "Invalid filename, private body",
    );
    f.file(
        true,
        "quit.md",
        "---\ndescription: Shadowed builtin\n---\nPRIVATE BODY",
    );

    let raw = f.json(&["commands", "list", "--json"]);
    assert_eq!(
        raw["commands"][0]["description"],
        "ไทย project description [2J end"
    );
    assert!(raw["commands"][0]["path"]
        .as_str()
        .unwrap()
        .contains('\u{007f}'));
    assert!(raw["commands"][0]["path"]
        .as_str()
        .unwrap()
        .contains('\u{2066}'));
    assert!(raw["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["path"].as_str().unwrap().contains('\u{001b}')));
    assert!(raw["shadowed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["path"].as_str().unwrap().contains('\u{202e}')));

    let output = f.run(&["commands", "list"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        !text.chars().any(|c| c.is_control() && c != '\n'),
        "untrusted metadata must not emit terminal controls"
    );
    assert!(!text.chars().any(|c|matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{2028}'..='\u{202e}'|'\u{2066}'..='\u{2069}')));
    for visible in [
        "\\u{1b}",
        "\\u{7f}",
        "\\u{202e}",
        "\\u{2066}",
        "ไทย",
        "description",
        "/quit",
    ] {
        assert!(text.contains(visible), "missing visible metadata {visible}");
    }
    assert!(!text.contains("PRIVATE BODY") && !text.contains("BODY MUST STAY PRIVATE"));
    assert!(!f.data.exists());
}
