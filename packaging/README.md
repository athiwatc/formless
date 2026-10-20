# Releasing Formless

Formless is distributed as a single binary. There is no runtime to
install and no sidecar files; configuration and data live in the
platform app-dirs (SPEC §9.4).

## Cutting a release

1. Bump `version` in the root `Cargo.toml` (`[workspace.package]`).
2. Run `cargo test --workspace --locked`.
3. Tag and push the tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
4. `.github/workflows/release.yml` builds macOS (arm64/x86_64), Linux
   (x86_64), and Windows (x86_64) archives with SHA-256 files and
   publishes them to a GitHub Release.

## Installing

- macOS/Linux: `scripts/install.sh` (downloads the latest release and
  verifies its checksum, or builds from source with
  `FORMLESS_FROM_SOURCE=1`).
- Homebrew: `packaging/homebrew/formless.rb` is a source formula; update
  its `url`/`sha256` per release before publishing a tap.
- Winget: templates live in `packaging/winget/`; fill version, installer
  URL, and SHA-256 from release artifacts before submitting.

## Update checks

`formless update check` compares the installed version with the latest
GitHub release and prints the release URL when an update exists. It
never modifies the installed binary; download and checksum verification
stay with the installer/package manager.

## Signing (optional, maintainer-provided)

Release artifacts are checksummed but unsigned by default. Maintainers
with Apple/Microsoft signing identities can sign inside the workflow's
package steps using repository secrets; end users on macOS may need to
clear the quarantine attribute for unsigned binaries
(`xattr -d com.apple.quarantine formless`).
