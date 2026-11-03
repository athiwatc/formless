//! Installer onboarding uses only owned fixtures: no downloads, real cargo
//! installs, model requests, shell-profile edits or application operations.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    dir: PathBuf,
    prefix: PathBuf,
    home: PathBuf,
    path: String,
}

fn executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fl-install-guidance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let bin = dir.join("mock-bin");
        let home = dir.join("home");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        for file in [".bashrc", ".bash_profile", ".zshrc", ".profile"] {
            std::fs::write(home.join(file), "keep this profile unchanged\n").unwrap();
        }
        executable(
            &bin.join("cargo"),
            r#"#!/bin/sh
printf 'mock cargo\n' >> "$INSTALL_CALLS"
root=
while [ "$#" -gt 0 ]; do
  if [ "$1" = --root ]; then root="$2"; shift 2; else shift; fi
done
[ -n "$root" ] || exit 81
mkdir -p "$root/bin"
cp "$MOCK_FORMLESS" "$root/bin/formless"
chmod 755 "$root/bin/formless"
"#,
        );
        executable(
            &bin.join("curl"),
            "#!/bin/sh\nprintf 'unexpected download\\n' >> \"$INSTALL_CALLS\"\nexit 82\n",
        );
        executable(
            &bin.join("uname"),
            "#!/bin/sh\ncase \"$1\" in -s) printf 'Linux\\n';; -m) printf 'aarch64\\n';; *) exit 83;; esac\n",
        );
        executable(
            &dir.join("mock-formless"),
            r#"#!/bin/sh
printf '%s %s\n' "$1" "${2:-}" >> "$BINARY_CALLS"
[ "$1" = completions ] || exit 84
printf '# fixture completion\n'
"#,
        );
        Self {
            prefix: dir.join("prefix [x] quote's $cash `tick` \\slash"),
            path: format!("{}:/usr/bin:/bin", bin.display()),
            home,
            dir,
        }
    }

    fn command(&self, shell: Option<&str>) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh"))
            .env_clear()
            .env("SHELL", "sh")
            .env("PATH", &self.path)
            .env("HOME", &self.home)
            .env("PREFIX", &self.prefix)
            .env("FORMLESS_FROM_SOURCE", "1")
            .env("MOCK_FORMLESS", self.dir.join("mock-formless"))
            .env("INSTALL_CALLS", self.dir.join("install-calls"))
            .env("BINARY_CALLS", self.dir.join("binary-calls"));
        if let Some(shell) = shell {
            command.env("SHELL", shell);
        }
        command
    }

    fn success(&self, output: Output) -> String {
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("formless to start the setup wizard"),
            "{text}"
        );
        assert!(text.contains("formless doctor"), "{text}");
        assert!(text.contains("API key or a local model"), "{text}");
        for file in [".bashrc", ".bash_profile", ".zshrc", ".profile"] {
            assert_eq!(
                std::fs::read_to_string(self.home.join(file)).unwrap(),
                "keep this profile unchanged\n"
            );
        }
        let calls = std::fs::read_to_string(self.dir.join("binary-calls")).unwrap();
        assert!(calls.lines().all(|line| matches!(
            line,
            "completions bash" | "completions zsh" | "completions fish"
        )));
        assert!(!std::fs::read_to_string(self.dir.join("install-calls"))
            .unwrap()
            .contains("unexpected download"));
        text
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn posix_guidance_round_trips_literal_paths_in_native_shells() {
    let fixture = Fixture::new();
    for shell in ["/bin/sh", "/bin/bash", "/usr/bin/zsh"] {
        if !Path::new(shell).exists() {
            eprintln!("native shell unavailable: {shell}; /bin/sh remains required");
            continue;
        }
        let text = fixture.success(fixture.command(Some(shell)).output().unwrap());
        let line = text
            .lines()
            .map(str::trim_start)
            .find(|line| line.starts_with("export PATH="))
            .unwrap();
        // Execute only the emitted PATH assignment and a builtin printf in a
        // fresh child. Literal dollars/backticks must not execute substitutions.
        let out = Command::new(shell)
            .args(["-c", &format!("{line}\nprintf '%s' \"$PATH\"")])
            .env_clear()
            .env("PATH", &fixture.path)
            .env("HOME", &fixture.home)
            .output()
            .unwrap();
        assert!(out.status.success(), "{shell}: {out:?}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("{}/bin:{}", fixture.prefix.display(), fixture.path),
            "{shell}: {line}"
        );
        let startup = if shell.ends_with("bash") {
            ".bashrc"
        } else if shell.ends_with("zsh") {
            ".zshrc"
        } else {
            ".profile"
        };
        assert!(text.contains(startup), "{text}");
    }
}

#[test]
fn path_membership_requires_a_complete_directory_component() {
    let fixture = Fixture::new();
    let bin = fixture.prefix.join("bin");
    for path in [
        format!("{}:{}", bin.display(), fixture.path),
        format!("{}:{}", fixture.path, bin.display()),
        format!("{}:{}:/extra", fixture.path, bin.display()),
        format!("{}:{}/", fixture.path, bin.display()),
    ] {
        let text = fixture.success(fixture.command(None).env("PATH", path).output().unwrap());
        assert!(!text.contains("not on PATH"), "{text}");
        assert!(!text.contains("export PATH="), "{text}");
    }
    let text = fixture.success(
        fixture
            .command(None)
            .env("PATH", format!("{}:{}-other", fixture.path, bin.display()))
            .output()
            .unwrap(),
    );
    assert!(text.contains("not on PATH") && text.contains("export PATH="));
    assert!(text.contains(".profile"));
}

#[test]
fn fish_and_unknown_shells_get_explicit_guidance_without_profile_edits() {
    let fixture = Fixture::new();
    let config = fixture.dir.join("fish config");
    let text = fixture.success(
        fixture
            .command(Some("/usr/bin/fish"))
            .env("XDG_CONFIG_HOME", &config)
            .output()
            .unwrap(),
    );
    assert!(text.contains("set -gx PATH '"), "{text}");
    assert!(
        text.contains(r"quote\'s $cash `tick` \\slash/bin' $PATH"),
        "{text}"
    );
    assert!(text.contains(&format!("{}/fish/config.fish", config.display())));
    assert!(!text.contains("export PATH="));
    assert!(
        !config.exists(),
        "guidance must not create fish configuration"
    );

    let text = fixture.success(
        fixture
            .command(Some("/bin/unknown-shell"))
            .output()
            .unwrap(),
    );
    assert!(text.contains("not recognized") && text.contains("POSIX-compatible"));
    assert!(text.contains("shell startup configuration"));
}

#[test]
fn default_prefix_and_zdotdir_are_reported_without_creating_profiles() {
    let fixture = Fixture::new();
    let zdotdir = fixture.dir.join("zsh config");
    let text = fixture.success(
        fixture
            .command(Some("/bin/zsh"))
            .env_remove("PREFIX")
            .env("ZDOTDIR", &zdotdir)
            .output()
            .unwrap(),
    );
    assert!(fixture.home.join(".local/bin/formless").exists());
    assert!(text.contains(&format!("{}/.local/bin", fixture.home.display())));
    assert!(text.contains(&format!("{}/.zshrc", zdotdir.display())));
    assert!(!zdotdir.exists());
}

#[test]
fn unsupported_arm_linux_names_prerequisites_before_any_download() {
    let fixture = Fixture::new();
    let output = fixture
        .command(None)
        .env_remove("FORMLESS_FROM_SOURCE")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "unsupported platform Linux/aarch64",
        "FORMLESS_FROM_SOURCE=1",
        "Rust/cargo",
        "C compiler",
    ] {
        assert!(error.contains(expected), "{error}");
    }
    assert!(!fixture.dir.join("install-calls").exists());
    assert!(!fixture.dir.join("binary-calls").exists());
}
