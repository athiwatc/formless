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
     libraries the binary links (libc, libdbus) and ship the license. The RPM
     output directory is created before `cargo generate-rpm --output`: the tool
     recognizes an existing directory and appends its metadata-derived package
     name, version, release and architecture filename; a missing directory path
     would instead be treated as a filename;
   - installs the packages in clean Debian, Ubuntu and Rocky containers
     and runs `formless --version`;
   - builds the Docker image, runs it and checks the daemon answers, then
     pushes `ghcr.io/athiwatc/formless:<tag>` and `:latest` (`Dockerfile`,
     [docs/docker.md](../docs/docker.md)); nothing is published if that
     fails;
   - renders the Homebrew formula, AUR `PKGBUILD` and winget manifests
     for the version (`scripts/render-packaging.sh`, which fails on a
     missing checksum, an unfilled placeholder, a version that isn't
     Cargo.toml's, or a Windows zip without the path the manifest names) and attaches them to the GitHub Release,
     whose notes (what `formless update` shows) are the version's
     `CHANGELOG.md` section (`scripts/release-notes.sh`) followed by
     GitHub's generated list of changes;
   - publishes to the tap, winget and the AUR **only when their secrets
     exist** (below), each on its own: one failing doesn't stop the
     others, the job summary lists each channel's result, and a re-run
     is safe.

## Test-only release timing baseline

`release-baseline.yml` is a separate, manual-only workflow for measuring the
nonpublishing work before sizing job timeouts (#1164). After it is merged,
run it on a reviewed commit through **Actions → release-baseline → Run workflow**.
Choose `job_family`: `all` (default), `native` (three macOS/Windows build rows),
`linux` (bullseye build and all three install containers), `docker`, or `pipe`.
Family-specific concurrency does not cancel other in-progress runs. Rerun an
unaffected family only when new evidence is needed; a Linux-only retry need not
repeat expensive native/Docker work.
Do not use the existing `release` dispatch for this: that workflow has publishing
permissions, and selecting a version tag can reach its publishing jobs.

The baseline token has only `contents: read`; checkout does not persist its
credentials. It never creates a tag/release, logs into or pushes a container or
package registry, updates a tap, submits winget/AUR changes, or uses publishing
secrets. The bullseye SHA read uses a temporary, command-scoped Git context:
Git 2.30.2 needs its exact checkout exception in a private XDG global-scope file,
not `-c safe.directory`. The same private file serves newer Git through
`GIT_CONFIG_GLOBAL`; ambient broader trust is refused, system config is disabled
for these commands, `HOME` is unchanged, and the private context is removed on
step exit. No actual user/system Git configuration is written.
Only Linux package/checksum fixtures transfer between jobs as private,
same-run workflow artifacts (one-day retention), not GitHub Release assets or
GitHub Packages. Package installation is confined to disposable Debian/Ubuntu/
Rocky job containers. The built package must have no maintainer/trigger scripts
(including RPM file and transaction-file triggers); query failure also refuses
installation. Smoke checks run `--version` and check the license, without installing a host service.
Docker smoke uses an owned, network-isolated fixture container and always removes
it and its anonymous volumes; it requires a successful status RPC with a positive
PID, rather than inferring success from missing error text.

The build matrix uses `macos-15` (ARM), `macos-15-intel` (Intel), and
`windows-latest`. Both labels are also updated in the release workflow:
`macos-14` [retires on November 2, 2026](https://github.blog/changelog/2026-10-01-github-actions-macos-14-runner-image-retirement/),
and retired `macos-13` is replaced by its [supported Intel successor](https://github.com/actions/runner-images/issues/13045).
Each baseline starts with fresh job-local Cargo home/target directories and no
cache restore/save. Toolchain setup, hosted image and network caches are not
claimed cold. Logs record the exact commit, toolchain, runner/image and
architecture; `time -p`/Stopwatch records phase timings. The standalone pipe
family runs the same named-pipe and production backup CLI commands as the
expanded Windows job. Its fresh target measures that whole job;
a pipe test following the complete workspace suite would already be warm and
cannot serve as this cold baseline.

Build and pipe jobs in both baseline and production workflows set
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0` and `CARGO_INCREMENTAL=0`.
These Cargo overrides remove debug symbols/incremental state; they retain test
selection, debug assertions, overflow checks and release optimization settings.
See Cargo's [profile settings](https://doc.rust-lang.org/cargo/reference/profiles.html).
Only after the complete workspace test command succeeds, profile-scoped
`cargo clean --profile dev` reclaims its generated debug artifacts before the
release build. It refuses a target outside this checkout or a symlinked target.
Package-building tools use a fresh runner-temporary target, removed immediately
after installation (also on failure); installed tools and application release
artifacts remain available for packaging. No host software or ambient caches
are deleted. Disk-free and generated-directory sizes are logged around these
phases. This reduces peak overlap of debug, release and tool intermediates;
the container image, downloaded registry and release artifacts still consume
space, so actual cold Linux build plus all install jobs must pass before this
resource fix or its timeout sizing is considered verified.

Earlier native/pipe measurements, including the historical 10m43s Windows pipe
run, predate these profile flags and are superseded as sizing evidence. Keep
them as history; remeasure native, Linux and pipe with the new recipe. Docker's
release-only workload is unchanged and its prior evidence is unaffected. Do not
combine differing profile/cleanup recipes into a claimed 2x timeout baseline.

Use each green job's `completed_at - started_at` (excluding queue time) and retain
its run/job URL, SHA, selected family, runner image and cache state. A skipped
or failed job supplies no green measurement for that row. To combine rows from
workflow-only retries, verify that their Rust source, manifests/lockfile and
relevant build/test/container/package inputs have the same content; retain each
row's actual SHA and scope. Code/dependency/image/build-recipe changes require
new measurements of affected rows. Set the corresponding timeout
to `ceil(2 * seconds / 60)` minutes; for a common matrix timeout, use the slowest
green matrix row. Current 60/30-minute release caps stay explicitly provisional
until that evidence exists. Failed, timed-out, unavailable-runner or warm jobs
are not cold green evidence. Matrix timings include local archive extraction/
execution but omit distribution artifact upload; Linux build/install jobs retain
the same internal transfer, and Docker excludes publishing and cache export.
This baseline does not complete #669's provenance/SBOM/draft-release requirements.

## Publishing to package managers (maintainer setup, once)

| Where | Set up | Secret |
|---|---|---|
| Homebrew tap | Create the public repo `athiwatc/homebrew-tap` (with a `Formula/` folder). Users then run `brew install athiwatc/tap/formless`. | `HOMEBREW_TAP_TOKEN`: a fine-grained token with contents write on that repo |
| winget | Submit the first version by hand: the release carries the rendered manifests (`athiwatc.formless*.yaml`, from the `packaging/winget/*.yaml.in` templates, with the version, URL, SHA-256 and the zip's nested `formless.exe` path checked against the real archive). On Windows, check them with `winget validate --manifest <folder>` and `winget install --manifest <folder>`, then submit with `wingetcreate submit <folder>` or a PR to `microsoft/winget-pkgs`; later versions are automatic (winget-releaser builds them from the last published one). Users run `winget install athiwatc.formless`. | `WINGET_TOKEN`: a classic token with `public_repo`, for the fork the action opens PRs from |
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
