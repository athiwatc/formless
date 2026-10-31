//! Exercise the production CLI boundary: a service preview must not create
//! a profile merely because global startup resolved its directories.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tree(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    fn walk(root: &Path, dir: &Path, result: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                result.push((path.strip_prefix(root).unwrap().into(), None));
                walk(root, &path, result);
            } else {
                result.push((
                    path.strip_prefix(root).unwrap().into(),
                    Some(std::fs::read(&path).unwrap()),
                ));
            }
        }
    }
    let mut result = Vec::new();
    walk(root, root, &mut result);
    result.sort();
    result
}

#[test]
fn install_service_dry_run_leaves_absent_and_existing_profile_trees_unchanged() {
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = Fixture(std::env::temp_dir().join(format!("fl-service-dry-{nonce}")));
    std::fs::create_dir(&root.0).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let data = root.0.join("data $TOKEN %n & spaces");
    let config = root.0.join("config $TOKEN %n & spaces");
    let profile = format!("dry-{nonce}");
    let no_managers = root.0.join("empty-path");
    std::fs::create_dir(&no_managers).unwrap();
    for existing in [false, true] {
        if existing {
            std::fs::create_dir_all(data.join(&profile)).unwrap();
            std::fs::create_dir_all(config.join(&profile)).unwrap();
            // Loading profile state would fail rather than quietly appearing
            // read-only. The preview needs only paths and the real formatter.
            std::fs::write(config.join(&profile).join("config.toml"), "invalid = [").unwrap();
            std::fs::write(config.join(&profile).join("api_key"), "fixture-only").unwrap();
            std::fs::write(data.join(&profile).join("formless.db"), b"not a database").unwrap();
        }
        let before = tree(&root.0);
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        command
            .args([
                "--profile",
                &profile,
                "daemon",
                "install-service",
                "--dry-run",
            ])
            .env("FORMLESS_DATA_DIR", &data)
            .env("FORMLESS_CONFIG_DIR", &config)
            .env("PATH", &no_managers)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Child-only overrides: preserve HOME and the parent's credentials.
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy().to_ascii_uppercase();
            if name.starts_with("FORMLESS_API_KEY")
                || matches!(
                    name.as_str(),
                    "FORMLESS_BASE_URL"
                        | "FORMLESS_MODEL"
                        | "FORMLESS_OAUTH_ACCESS_TOKEN"
                        | "FORMLESS_OAUTH_CLIENT_SECRET"
                )
            {
                command.env_remove(key);
            }
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("install-service --dry-run did not finish within ten seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let service_file = stdout
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("# "))
            .and_then(|line| line.rsplit_once(" ("))
            .map(|(path, _)| PathBuf::from(path))
            .expect("preview must name its service file");
        assert!(stdout.contains(&profile), "{stdout}");
        if cfg!(target_os = "macos") {
            for argument in ["daemon", "serve", "--stay"] {
                assert!(
                    stdout.contains(&format!("<string>{argument}</string>")),
                    "{stdout}"
                );
            }
        } else {
            assert!(stdout.contains("daemon serve --stay"), "{stdout}");
        }
        assert!(!service_file.exists(), "preview installed {service_file:?}");
        assert_eq!(tree(&root.0), before, "preview changed profile state");
        assert!(!data.exists() || existing);
        assert!(!config.exists() || existing);
    }
}
