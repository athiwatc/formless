//! Release packaging stays consistent (#1076–#1078): the renderer fills
//! every placeholder or fails, the image carries its runtime libraries and
//! pinned bases, and every package ships the license.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[cfg(unix)]
#[test]
fn the_renderer_fills_everything_or_fails() {
    let dir = std::env::temp_dir().join(format!("fl-render-{}", std::process::id()));
    let assets = dir.join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    let run = |version: &str, out: &str| {
        std::process::Command::new("bash")
            .arg(root().join("scripts/render-packaging.sh"))
            .args([
                version,
                assets.to_str().unwrap(),
                dir.join(out).to_str().unwrap(),
            ])
            .env("SRC_SHA", "b".repeat(64))
            .env("MANIFEST", dir.join("Cargo.toml"))
            .output()
            .unwrap()
    };
    // The packages must name the Cargo.toml version (#841).
    std::fs::write(
        dir.join("Cargo.toml"),
        "[workspace.package]\nversion = \"9.8.7\"\n",
    )
    .unwrap();
    // No checksum file: a named failure, not a half-filled formula.
    let r = run("9.8.7", "o1");
    assert!(!r.status.success());
    assert!(String::from_utf8_lossy(&r.stderr).contains("missing"));
    std::fs::write(
        assets.join("formless-v9.8.7-x86_64-unknown-linux-gnu.tar.gz.sha256"),
        format!("{}  x\n", "a".repeat(64)),
    )
    .unwrap();
    let r = run("9.8.7", "o2");
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("windows-msvc.zip.sha256"),
        "the Windows sum is required"
    );
    std::fs::write(
        assets.join("formless-v9.8.7-x86_64-pc-windows-msvc.zip.sha256"),
        format!("{}  x\n", "c".repeat(64)),
    )
    .unwrap();
    // A version that is not Cargo.toml's is refused.
    std::fs::write(
        assets.join("formless-v9.9.9-x86_64-unknown-linux-gnu.tar.gz.sha256"),
        format!("{}  x\n", "a".repeat(64)),
    )
    .unwrap();
    let r = run("9.9.9", "o4");
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("does not match"),
        "{r:?}"
    );
    let r = run("9.8.7", "o2");
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let pkgbuild = std::fs::read_to_string(dir.join("o2/aur/PKGBUILD")).unwrap();
    assert!(pkgbuild.contains("pkgver=9.8.7") && pkgbuild.contains(&"a".repeat(64)));
    assert!(
        !pkgbuild.contains("@VERSION@") && !pkgbuild.contains("@SHA256"),
        "{pkgbuild}"
    );
    let formula = std::fs::read_to_string(dir.join("o2/homebrew/formless.rb")).unwrap();
    assert!(formula.contains("v9.8.7.tar.gz") && formula.contains(&"b".repeat(64)));
    // brew audit: a description under 80 characters, no leading article.
    let desc = formula
        .lines()
        .find_map(|l| l.trim().strip_prefix("desc \""))
        .unwrap()
        .trim_end_matches('"');
    assert!(
        desc.len() < 80 && !desc.starts_with("A ") && !desc.starts_with("An "),
        "{desc}"
    );
    let installer =
        std::fs::read_to_string(dir.join("o2/winget/athiwatc.formless.installer.yaml")).unwrap();
    assert!(installer.contains("PackageVersion: 9.8.7") && installer.contains(&"c".repeat(64)));
    assert!(
        installer.contains("RelativeFilePath: formless-v9.8.7-x86_64-pc-windows-msvc/formless.exe")
    );
    for f in [
        "athiwatc.formless.yaml",
        "athiwatc.formless.locale.en-US.yaml",
    ] {
        let m = std::fs::read_to_string(dir.join("o2/winget").join(f)).unwrap();
        assert!(
            m.contains("PackageVersion: 9.8.7") && !m.contains('@'),
            "{m}"
        );
    }
    // With the zip present, its layout must match the manifest.
    if std::process::Command::new("zip").arg("-v").output().is_ok() {
        let zip = assets.join("formless-v9.8.7-x86_64-pc-windows-msvc.zip");
        let stage = dir.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("formless.exe"), "exe").unwrap();
        let zip_it = |what: &str| {
            let _ = std::fs::remove_file(&zip);
            assert!(std::process::Command::new("zip")
                .args(["-qr", zip.to_str().unwrap(), what])
                .current_dir(&stage)
                .status()
                .unwrap()
                .success());
        };
        zip_it("formless.exe");
        let r = run("9.8.7", "o5");
        assert!(
            String::from_utf8_lossy(&r.stderr).contains("does not hold"),
            "{r:?}"
        );
        let nested = stage.join("formless-v9.8.7-x86_64-pc-windows-msvc");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::rename(stage.join("formless.exe"), nested.join("formless.exe")).unwrap();
        zip_it("formless-v9.8.7-x86_64-pc-windows-msvc");
        assert!(run("9.8.7", "o6").status.success());
    }
    assert!(!run("not-a-version", "o3").status.success());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_image_and_packages_carry_what_they_need() {
    let docker = std::fs::read_to_string(root().join("Dockerfile")).unwrap();
    for from in docker.lines().filter(|l| l.starts_with("FROM ")) {
        assert!(from.contains("@sha256:"), "unpinned base: {from}");
    }
    assert!(docker.contains("libdbus-1-dev") && docker.contains("libdbus-1-3"));
    assert!(docker.contains("HEALTHCHECK"));
    let license = std::fs::read_to_string(root().join("LICENSE")).unwrap();
    assert!(license.contains("Apache License") && license.contains("Version 2.0"));
    let cargo = std::fs::read_to_string(root().join("crates/formless/Cargo.toml")).unwrap();
    assert!(cargo.contains("$auto") && cargo.contains("license-file"));
    assert!(cargo.contains("/usr/share/licenses/formless/LICENSE"));
    let aur = std::fs::read_to_string(root().join("packaging/aur/PKGBUILD.in")).unwrap();
    assert!(aur.contains("'dbus'") && aur.contains("/usr/share/licenses/"));
    let release = std::fs::read_to_string(root().join(".github/workflows/release.yml")).unwrap();
    assert!(release.contains("needs: [build, linux, linux-install-test, docker]"));
}

fn sh() -> &'static str {
    if Path::new("/bin/dash").exists() {
        "/bin/dash"
    } else {
        "/bin/sh"
    }
}

// Only a direct property of this job counts, regardless of how many settings
// precede it. Steps and neighboring jobs cannot satisfy the dependency.
fn job_needs_version_check(workflow: &str, job: &str) -> bool {
    let header = format!("  {job}:");
    let mut lines = workflow.lines();
    if !lines.any(|line| line == header) {
        return false;
    }
    lines
        .take_while(|line| {
            line.trim().is_empty() || line.trim_start().starts_with('#') || line.starts_with("    ")
        })
        .any(|line| {
            let Some(value) = line.strip_prefix("    needs: ") else {
                return false;
            };
            let value = value.split('#').next().unwrap_or_default().trim();
            if value == "version-check" {
                return true;
            }
            value
                .strip_prefix('[')
                .and_then(|items| items.strip_suffix(']'))
                .is_some_and(|items| items.split(',').any(|item| item.trim() == "version-check"))
        })
}

#[test]
fn version_check_dependency_is_scoped_to_the_job() {
    let settings = format!("    env:\n      LONG_SETTING: {}\n", "x".repeat(300));
    let workflow = format!(
        "jobs:\n  build:\n    needs: version-check\n  docker:\n{settings}    needs: version-check\n  publish:\n    needs: docker\n"
    );
    assert!(job_needs_version_check(&workflow, "docker"));
    let missing = workflow.replace(&format!("{settings}    needs: version-check\n"), &settings);
    assert!(!job_needs_version_check(&missing, "docker"));
    assert!(!job_needs_version_check(
        "jobs:\n  docker:\n    runs-on: ubuntu-latest\n  publish:\n    needs: version-check\n",
        "docker"
    ));
    assert!(!job_needs_version_check(
        "jobs:\n  docker:\n    # needs: version-check\n    steps:\n      - run: |\n          needs: version-check\n",
        "docker"
    ));
    assert!(job_needs_version_check(
        "jobs:\n  docker:\n    needs: [build, version-check, linux, linux-install-test] # gate\n",
        "docker"
    ));
    for value in [
        "[build, linux]",
        "[build, version-check-extra]",
        "[not-version-check]",
    ] {
        assert!(!job_needs_version_check(
            &format!("jobs:\n  docker:\n    needs: {value}\n"),
            "docker"
        ));
    }
    assert!(!job_needs_version_check(&workflow, "missing"));
}

/// A release whose tag is not the Cargo.toml version is refused before
/// any build; the real tag for this tree passes (#842).
#[cfg(unix)]
#[test]
fn release_tags_must_match_the_cargo_version() {
    let check = root().join("scripts/release-check.sh");
    let run = |tag: &str, manifest: &Path| {
        std::process::Command::new(sh())
            .arg(&check)
            .arg(tag)
            .arg(manifest)
            .output()
            .unwrap()
    };
    let real = root().join("Cargo.toml");
    let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    assert!(run(&tag, &real).status.success(), "{tag} matches");
    let wrong = run("v99.0.0", &real);
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("does not match"));
    assert!(
        !run(env!("CARGO_PKG_VERSION"), &real).status.success(),
        "the v is required"
    );

    let release = std::fs::read_to_string(root().join(".github/workflows/release.yml")).unwrap();
    assert!(release.contains("scripts/release-check.sh \"$GITHUB_REF_NAME\""));
    for job in ["build", "linux", "docker"] {
        assert!(
            job_needs_version_check(&release, job),
            "{job} waits for the check"
        );
    }
    assert!(
        release.contains("generate_release_notes: true")
            && release.contains("body_path: release-notes.md")
    );

    let dir = std::env::temp_dir().join(format!("fl-notes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cl = dir.join("CHANGELOG.md");
    std::fs::write(&cl, "## [Unreleased]\n- next\n\n## [0.2.0] - 2026-01-01\n\n### Fixed\n- a bug\n\n## [0.1.0]\n- first\n").unwrap();
    let notes = std::process::Command::new(sh())
        .arg(root().join("scripts/release-notes.sh"))
        .args(["v0.2.0", cl.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&notes.stdout),
        "### Fixed\n- a bug\n\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn source_installer_writes_a_complete_optional_manual() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("fl source install {}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let prefix = dir.join("install prefix");
    let cargo = dir.join("cargo");
    std::fs::write(
        &cargo,
        r#"#!/bin/sh
set -eu
test "$1" = install
while [ "$1" != --root ]; do shift; done
destination="$2/bin/formless"
cat > "$destination" <<'BIN'
#!/bin/sh
case "$1" in
  man)
    case "$FIXTURE_MAN" in
      success) printf '.TH FORMLESS 1\n' ;;
      empty) exit 0 ;;
      failure) printf 'partial text\n'; exit 2 ;;
    esac
    ;;
  completions) printf '# fixture completion for %s\n' "$2" ;;
  *) exit 1 ;;
esac
BIN
chmod 755 "$destination"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let install = |mode| {
        std::process::Command::new(sh())
            .arg(root().join("scripts/install.sh"))
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .env("PREFIX", &prefix)
            .env("FORMLESS_FROM_SOURCE", "1")
            .env("FIXTURE_MAN", mode)
            .output()
            .unwrap()
    };
    let page = prefix.join("share/man/man1/formless.1");
    let output = install("success");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read_to_string(&page).unwrap(), ".TH FORMLESS 1\n");
    assert_eq!(
        std::fs::metadata(&page).unwrap().permissions().mode() & 0o777,
        0o644
    );
    for mode in ["empty", "failure"] {
        let output = install(mode);
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("could not install the optional man page"));
        assert_eq!(std::fs::read_to_string(&page).unwrap(), ".TH FORMLESS 1\n");
        assert_eq!(
            std::fs::read_dir(page.parent().unwrap()).unwrap().count(),
            1
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// install.sh under dash with only sha256sum (no shasum) on PATH: accepts
/// a version without the v, replaces the binary atomically, leaves the old
/// one intact when verification fails, and checks HOME/PREFIX first (#840).
#[cfg(unix)]
#[test]
fn install_script_is_atomic_and_preflights() {
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::os::unix::process::CommandExt;
    let which = |tool: &str| {
        [
            "/bin",
            "/usr/bin",
            "/sbin",
            "/usr/sbin",
            "/opt/homebrew/bin",
            "/usr/local/bin",
        ]
        .iter()
        .map(|d| Path::new(d).join(tool))
        .find(|p| p.exists())
    };
    let Some(sha256sum) = which("sha256sum") else {
        eprintln!("skipped: no sha256sum on this machine");
        return;
    };
    let dir = std::env::temp_dir().join(format!("fl-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bin = dir.join("path");
    std::fs::create_dir_all(&bin).unwrap();
    // Everything the script uses, except shasum.
    for tool in [
        "sh", "curl", "tar", "gzip", "mkdir", "cp", "chmod", "mv", "rm", "sed", "head", "cat",
        "dirname", "basename", "mktemp", "uname",
    ] {
        if let Some(p) = which(tool) {
            symlink(p, bin.join(tool)).unwrap();
        }
    }
    symlink(&sha256sum, bin.join("sha256sum")).unwrap();

    // A fake release v0.9.0 for a made-up target.
    let release = dir.join("releases/v0.9.0");
    let pkg = dir.join("build/formless-v0.9.0-test-target");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(pkg.join("formless"), "#!/bin/sh\necho new\n").unwrap();
    std::fs::write(pkg.join("formless.1"), ".TH FORMLESS 1\n").unwrap();
    let asset = "formless-v0.9.0-test-target.tar.gz";
    assert!(std::process::Command::new("tar")
        .args([
            "-czf",
            release.join(asset).to_str().unwrap(),
            "formless-v0.9.0-test-target"
        ])
        .current_dir(dir.join("build"))
        .status()
        .unwrap()
        .success());
    let sum = std::process::Command::new(&sha256sum)
        .arg(asset)
        .current_dir(&release)
        .output()
        .unwrap();
    std::fs::write(release.join(format!("{asset}.sha256")), &sum.stdout).unwrap();

    let prefix = dir.join("prefix");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let target = prefix.join("bin/formless");
    std::fs::write(&target, "#!/bin/sh\necho old\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    let install = |envs: &[(&str, &str)], prefix: Option<&Path>| {
        let mut c = std::process::Command::new(sh());
        c.arg(root().join("scripts/install.sh"))
            .env_clear()
            .env("PATH", &bin)
            .env("HOME", dir.join("home"))
            .env("FORMLESS_TARGET", "test-target")
            .env(
                "FORMLESS_RELEASES_URL",
                format!("file://{}", dir.join("releases").display()),
            );
        if let Some(p) = prefix {
            c.env("PREFIX", p);
        }
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    };

    // A broken checksum: refused, the old binary untouched, nothing left.
    let good_sum = std::fs::read(release.join(format!("{asset}.sha256"))).unwrap();
    std::fs::write(
        release.join(format!("{asset}.sha256")),
        format!("{}  {asset}\n", "0".repeat(64)),
    )
    .unwrap();
    let out = install(&[("FORMLESS_VERSION", "0.9.0")], Some(&prefix));
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("checksum mismatch"),
        "{out:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "#!/bin/sh\necho old\n"
    );
    assert_eq!(
        std::fs::read_dir(prefix.join("bin")).unwrap().count(),
        1,
        "no temp files left"
    );

    // The real checksum, a version without its v: installed.
    std::fs::write(release.join(format!("{asset}.sha256")), good_sum).unwrap();
    let out = install(&[("FORMLESS_VERSION", "0.9.0")], Some(&prefix));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "#!/bin/sh\necho new\n"
    );
    assert!(prefix.join("share/man/man1/formless.1").exists());
    let guidance = String::from_utf8_lossy(&out.stdout);
    assert!(guidance.contains("export PATH="), "{guidance}");
    assert!(guidance.contains(".profile"), "{guidance}");
    assert!(guidance.contains("formless to start the setup wizard"));
    assert!(guidance.contains("formless doctor"));
    assert!(guidance.contains("API key or a local model"));
    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "executable");

    // No HOME and no PREFIX: a clear error, not an unbound variable.
    let out = std::process::Command::new(sh())
        .arg(root().join("scripts/install.sh"))
        .env_clear()
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("HOME is not set"),
        "{out:?}"
    );

    // An unwritable PREFIX is found before downloading anything.
    let locked = dir.join("locked");
    std::fs::create_dir_all(locked.join("bin")).unwrap();
    std::fs::set_permissions(locked.join("bin"), std::fs::Permissions::from_mode(0o555)).unwrap();
    // Container-root can write through 0555. Drop privileges only for these
    // permission checks, without looking up/changing any host account. uid()
    // also clears supplementary groups. A failed privilege drop fails the test.
    let root_owned_fixture = std::fs::metadata(&dir).unwrap().uid() == 0;
    let permission_child = || {
        let mut child = std::process::Command::new(sh());
        if root_owned_fixture {
            child.gid(65534).uid(65534);
        }
        child.env_clear().current_dir(&dir);
        child
    };
    // The checkout may itself be private. Copy only the installer and expose
    // traversal/read access to this synthetic fixture, never to the checkout.
    let preflight_script = dir.join("preflight-install.sh");
    std::fs::copy(root().join("scripts/install.sh"), &preflight_script).unwrap();
    for path in [&dir, &bin, &locked] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::set_permissions(&preflight_script, std::fs::Permissions::from_mode(0o644)).unwrap();
    // Prove this exact child really cannot write, rather than assuming modes
    // imply denial for its credentials or silently skipping a privileged run.
    let probe = locked.join("bin/direct-write-probe");
    let out = permission_child()
        .args(["-c", ": > \"$1\"", "write-probe"])
        .arg(&probe)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "permission fixture is writable: {out:?}"
    );
    assert!(!probe.exists());

    // A private curl spy forbids network access and observes download ordering.
    let preflight_bin = dir.join("preflight-path");
    std::fs::create_dir(&preflight_bin).unwrap();
    std::fs::set_permissions(&preflight_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    for entry in std::fs::read_dir(&bin).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() != "curl" {
            symlink(entry.path(), preflight_bin.join(entry.file_name())).unwrap();
        }
    }
    let curl_spy = preflight_bin.join("curl");
    std::fs::write(
        &curl_spy,
        "#!/bin/sh\necho installer-download-reached >&2\nexit 73\n",
    )
    .unwrap();
    std::fs::set_permissions(&curl_spy, std::fs::Permissions::from_mode(0o755)).unwrap();
    let preflight = |prefix: &Path| {
        permission_child()
            .arg(&preflight_script)
            .env("PATH", &preflight_bin)
            .env("HOME", dir.join("home"))
            .env("PREFIX", prefix)
            .env("FORMLESS_TARGET", "test-target")
            .env("FORMLESS_VERSION", "v0.9.0")
            .env("FORMLESS_RELEASES_URL", "file:///nonexistent")
            .output()
            .unwrap()
    };
    let out = preflight(&locked);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("is not writable"), "{out:?}");
    assert!(!stderr.contains("installer-download-reached"), "{out:?}");
    assert_eq!(std::fs::read_dir(locked.join("bin")).unwrap().count(), 0);

    // Positive control: the same child, script and PATH reach the curl spy
    // after a successful write probe. Only these empty dummy directories need
    // child-write access; the real release/atomic/checksum fixtures stay owned.
    let writable = dir.join("preflight-writable");
    std::fs::create_dir_all(writable.join("bin")).unwrap();
    for path in [&writable, &writable.join("bin")] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();
    }
    let out = preflight(&writable);
    assert!(
        !out.status.success(),
        "the curl spy refuses downloads: {out:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("installer-download-reached"), "{out:?}");
    assert!(!stderr.contains("is not writable"), "{out:?}");
    std::fs::set_permissions(locked.join("bin"), std::fs::Permissions::from_mode(0o755)).unwrap();

    // A rate-limited release lookup says so.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let latest = format!("http://{addr}/latest");
    let out = install(&[("FORMLESS_LATEST_URL", latest.as_str())], Some(&prefix));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("HTTP 403"),
        "{out:?}"
    );
    let _ = std::fs::remove_dir_all(dir);
}
