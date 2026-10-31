# Releasing Formless

Formless is distributed as a single binary. There is no runtime to
install and no sidecar files; configuration and data live in the
platform app-dirs (SPEC §9.4).

## Cutting a release

1. Bump `version` in the root `Cargo.toml` (`[workspace.package]`), and
   move the `CHANGELOG.md` entries under `## [X.Y.Z] - date`.
2. Run `cargo test --workspace --locked`, and
   `scripts/release-check.sh vX.Y.Z`.
3. Tag and push the tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
4. `.github/workflows/release.yml` then:
   - first checks the tag names the Cargo.toml version
     (`scripts/release-check.sh`) and stops before any build if not, so
     asset names, `formless --version` and `formless update` agree;
   - builds macOS (arm64/x86_64) and Windows (x86_64) archives with
     SHA-256 files;
   - builds the Linux archive, `.deb` and `.rpm` (`cargo-deb`,
     `cargo-generate-rpm`, metadata in `crates/formless/Cargo.toml`) on
     Debian bullseye, so they need **glibc 2.31 or newer** (Debian 11+,
     Ubuntu 20.04+, RHEL/Rocky 9+); the packages record the shared
     libraries the binary links (libc, libdbus) and ship the license;
   - installs the packages in clean Debian, Ubuntu and Rocky containers
     and runs `formless --version`;
   - builds the Docker image, runs it and checks the daemon answers, then
     pushes `ghcr.io/athiwatc/formless:<tag>` and `:latest` (`Dockerfile`,
     [docs/docker.md](../docs/docker.md)); nothing is published if that
     fails;
   - renders the Homebrew formula and AUR `PKGBUILD` for the version
     (`scripts/render-packaging.sh`, which fails on a missing checksum or
     an unfilled placeholder) and attaches them to the GitHub Release,
     whose notes (what `formless update` shows) are the version's
     `CHANGELOG.md` section (`scripts/release-notes.sh`) followed by
     GitHub's generated list of changes;
   - publishes to the tap, winget and the AUR **only when their secrets
     exist** (below), each on its own: one failing doesn't stop the
     others, the job summary lists each channel's result, and a re-run
     is safe.

## Publishing to package managers (maintainer setup, once)

| Where | Set up | Secret |
|---|---|---|
| Homebrew tap | Create the public repo `athiwatc/homebrew-tap` (with a `Formula/` folder). Users then run `brew install athiwatc/tap/formless`. | `HOMEBREW_TAP_TOKEN`: a fine-grained token with contents write on that repo |
| winget | Submit the first version by hand (`wingetcreate new`, starting from the templates in `packaging/winget/`, or a PR to `microsoft/winget-pkgs`); later versions are automatic (winget-releaser builds them from the last published one). Users run `winget install athiwatc.formless`. | `WINGET_TOKEN`: a classic token with `public_repo`, for the fork the action opens PRs from |
| AUR | Create the `formless-bin` package on aur.archlinux.org with your SSH key. Users run `yay -S formless-bin`. | `AUR_SSH_KEY`: that SSH private key |
| Docker | Nothing: the workflow's own token pushes to GHCR. Make the package public once in the repository's Packages settings. | — |

Without a secret, that step is skipped and the release still completes.

## Installing

- macOS/Linux: `scripts/install.sh` (downloads the latest release and
  verifies its checksum, or builds from source with
  `FORMLESS_FROM_SOURCE=1`).
- Homebrew: `brew install athiwatc/tap/formless` (once the tap exists).
- Winget: `winget install athiwatc.formless` (once submitted).
- Debian/Ubuntu: `sudo apt install ./formless_<version>_amd64.deb` from the
  release; Fedora/RHEL: `sudo dnf install ./formless-<version>.x86_64.rpm`.
- Arch: `yay -S formless-bin` (AUR).
- Docker: `docker run ghcr.io/athiwatc/formless` ([docs/docker.md](../docs/docker.md)).

## Uninstalling

`formless uninstall` stops the daemon, removes the login service, deletes
keychain entries and every profile's data and config, and removes the
binary (or says which package manager command removes it).
`--keep-data` keeps Sessions, Tools and config; `--keep-binary` leaves the
binary; `formless export-all` first keeps a copy of what Formless learned.

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
