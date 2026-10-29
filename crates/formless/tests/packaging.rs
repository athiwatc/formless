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
            .output()
            .unwrap()
    };
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
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let pkgbuild = std::fs::read_to_string(dir.join("o2/aur/PKGBUILD")).unwrap();
    assert!(pkgbuild.contains("pkgver=9.8.7") && pkgbuild.contains(&"a".repeat(64)));
    assert!(
        !pkgbuild.contains("@VERSION@") && !pkgbuild.contains("@SHA256"),
        "{pkgbuild}"
    );
    let formula = std::fs::read_to_string(dir.join("o2/homebrew/formless.rb")).unwrap();
    assert!(formula.contains("v9.8.7.tar.gz") && formula.contains(&"b".repeat(64)));
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
