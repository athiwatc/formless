//! Installer curl configuration/errors using private fake downloads only.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fl-install-timeouts-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let curl = dir.join("bin/curl");
        std::fs::write(
            &curl,
            r#"#!/bin/sh
printf 'CALL\n' >> "$CURL_CALLS"
printf '%s\n' "$@" >> "$CURL_CALLS"
connect= max= retry= delay= output= url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --connect-timeout) connect="$2"; shift 2 ;;
    --max-time) max="$2"; shift 2 ;;
    --retry) retry="$2"; shift 2 ;;
    --retry-delay) delay="$2"; shift 2 ;;
    -o) output="$2"; shift 2 ;;
    -w) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
[ "$connect/$max/$retry/$delay" = '10/120/3/2' ] || { echo missing-time-bounds >&2; exit 91; }
case "$url" in
  */latest) phase=latest ;;
  *.sha256) phase=checksum ;;
  *) phase=archive ;;
esac
if [ "$FAIL_PHASE" = "$phase" ]; then
  # A timed-out body can have HTTP 200: status alone is not success.
  [ "$phase" != latest ] || printf 200
  echo fixture-transfer-timeout >&2
  exit 28
fi
if [ "$FAIL_PHASE" = rate-limit ] && [ "$phase" = latest ]; then printf 403; exit 0; fi
if [ "$FAIL_PHASE" = http-error ]; then echo fixture-http-failure >&2; exit 22; fi
case "$phase" in
  latest) printf '{"tag_name":"v0.9.0"}\n' > "$output"; printf 200 ;;
  archive) printf 'unused fixture archive' > "$output" ;;
  *) echo unexpected-checksum-success >&2; exit 92 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(curl, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(dir.join("prefix/bin")).unwrap();
        std::fs::write(dir.join("prefix/bin/formless"), "original binary").unwrap();
        Self(dir)
    }

    fn run(&self, phase: &str, pinned: bool) -> Output {
        let mut command = Command::new("/bin/sh");
        command
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install.sh"))
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("bin").display()),
            )
            .env("HOME", self.0.join("home"))
            .env("PREFIX", self.0.join("prefix"))
            .env("TMPDIR", &self.0)
            .env("FORMLESS_TARGET", "fixture-target")
            .env("FORMLESS_RELEASES_URL", "https://fixture.invalid/download")
            .env("FORMLESS_LATEST_URL", "https://fixture.invalid/latest")
            .env("CURL_CALLS", self.0.join("calls"))
            .env("FAIL_PHASE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if pinned {
            command.env("FORMLESS_VERSION", "0.9.0");
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("installer fixture exceeded deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(
            std::fs::read_to_string(self.0.join("prefix/bin/formless")).unwrap(),
            "original binary"
        );
        assert_eq!(
            std::fs::read_dir(self.0.join("prefix/bin"))
                .unwrap()
                .count(),
            1
        );
        assert!(
            !std::fs::read_dir(&self.0).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("tmp.")),
            "download temp directory must be cleaned"
        );
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn installer_bounds_every_curl_and_reports_transfer_timeouts() {
    for (phase, pinned, calls) in [
        ("latest", false, 1),
        ("archive", false, 2),
        ("checksum", false, 3),
        ("archive", true, 1),
    ] {
        let fixture = Fixture::new();
        let output = fixture.run(phase, pinned);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("timed out"), "{phase}: {error}");
        assert!(
            error.contains(if phase == "latest" {
                "FORMLESS_VERSION"
            } else {
                "FORMLESS_RELEASES_URL"
            }),
            "{error}"
        );
        assert!(error.contains("mirror"), "{error}");
        let log = std::fs::read_to_string(fixture.0.join("calls")).unwrap();
        assert_eq!(log.lines().filter(|l| *l == "CALL").count(), calls);
        assert!(!error.contains("missing-time-bounds"), "{error}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("installed:"));
    }
}

#[test]
fn installer_preserves_rate_limit_and_other_download_errors() {
    for (phase, pinned, expected) in [
        ("rate-limit", false, "HTTP 403"),
        ("http-error", true, "could not download"),
    ] {
        let fixture = Fixture::new();
        let output = fixture.run(phase, pinned);
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("timed out"), "{error}");
    }
}
