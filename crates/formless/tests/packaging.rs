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
    for job in ["build:", "linux:", "docker:"] {
        let at = release.find(&format!("\n  {job}")).unwrap();
        assert!(
            release[at..at + 200].contains("needs: version-check"),
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

/// install.sh under dash with only sha256sum (no shasum) on PATH: accepts
/// a version without the v, replaces the binary atomically, leaves the old
/// one intact when verification fails, and checks HOME/PREFIX first (#840).
#[cfg(unix)]
#[test]
fn install_script_is_atomic_and_preflights() {
    use std::os::unix::fs::{symlink, PermissionsExt};
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
    let out = install(
        &[
            ("FORMLESS_VERSION", "v0.9.0"),
            ("FORMLESS_RELEASES_URL", "file:///nonexistent"),
        ],
        Some(&locked),
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not writable"),
        "{out:?}"
    );
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
