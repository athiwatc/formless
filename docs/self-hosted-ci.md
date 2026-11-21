# Self-hosted CI

Linux and macOS jobs now require `self-hosted` plus OS and architecture labels.
Using only `self-hosted` would let Linux container jobs land on a Mac, or package
an ARM binary as an Intel release. GitHub assigns these default labels when a
runner is registered normally:

| Jobs | Required labels | Prerequisites |
| --- | --- | --- |
| Documentation | `self-hosted`, `macOS`, `ARM64` | Git, Bash, curl, tar, shasum; native pinned lychee downloaded into job temp |
| Linux packages/install checks, Docker, publication | `self-hosted`, `Linux`, `X64` | Git, Bash, Docker, Python 3; normal action/toolchain dependencies |
| Native Apple Silicon release/baseline | `self-hosted`, `macOS`, `ARM64` | Git, Bash, Rust toolchain prerequisites, Xcode command-line tools; PowerShell for cold-baseline instrumentation |
| Native Intel Mac release/baseline | `self-hosted`, `macOS`, `X64` | Same Mac prerequisites |

On 2026-11-13, the GitHub runner settings UI showed one idle runner:
`Athiwats-MacBook-Pro`, labelled `self-hosted`, `macOS`, `ARM64`. Documentation
checks use this observed platform. No Linux/X64 or Intel Mac runner was listed;
those release/baseline jobs still require their correct platforms and will queue
until matching runners are available. Do not relabel a Mac as Linux to unblock them.
Jobs queue if a matching runner is absent. Verify labels, free disk space and
Docker availability before dispatching a release or baseline. A registered
self-hosted runner is not automatically an isolated or disposable machine.

The standalone Windows workflow is removed. Windows release artifacts and the
manual Windows baseline remain on GitHub-hosted Windows runners, because release
packaging still requires the Windows archive for winget. Local Windows testing
remains part of PR verification; removing that workflow does not prove portability.

## Linux relay browser checks

The Linux CI browser step installs Playwright and its Chromium build in the job's
temporary directory. The PDF fixtures use Poppler to render pages and extract
text. The runner environment needs both `pdftoppm` and `pdftotext` on the job's
`PATH`, with runtime libraries compatible with that environment. On Debian and
Ubuntu, both commands are provided by `poppler-utils`.

Run this check from the Linux runner environment and account that CI uses.

```sh
command -v pdftoppm && command -v pdftotext && pdftoppm -v && pdftotext -v
```

Supply the tools in the runner image or put a compatible runner-provided copy
on the job's `PATH`. The browser step fails before starting Playwright when a
tool is missing. It relies on the runner for system packages.

## Trust boundary

No automatic `pull_request` or `pull_request_target` event runs on self-hosted
machines. Documentation checks run on reviewed `main` pushes, weekly schedules,
and manual `main` dispatches only. The release baseline is manual on `main`;
release builds accept version tags and manual `main` dispatches. Existing release
publication remains tag-only. Review all source and workflow changes before
merging or creating a release tag. Repository write access must be trusted.

Do not dispatch unreviewed PR code on persistent runners. Use local isolated
checks for PRs; use disposable, restricted runners if automatic untrusted PR
coverage is needed later. Runner groups, network restrictions, branch protection,
and host permissions are not changed by this migration. `main` had no required
status checks at inspection time; no existing protection was removed.

## Pipeline behavior

- Documentation cancels superseded runs; release publication is not canceled
  halfway through a run.
- Build/test jobs have read-only repository tokens and checkout does not persist
  credentials. Docker client configuration is job-local; Homebrew tap access
  uses a temporary askpass helper, never a token-bearing remote URL. Only Docker
  publishing receives package-write and release
  publishing receives contents-write permissions.
- Missing native release artifacts fail instead of silently passing.
- Docker smoke checks require a successful JSON status with a positive PID and
  version, not merely absence of an error string. Smoke containers have no
  network and uniquely named images/containers are cleaned even after failure.
- Release tests use job-local Cargo and Formless configuration/data locations.
  GitHub runner temporary-directory cleanup must work; retain the existing
  storage-budget checks and provision adequate capacity. No broad cache or
  host-file deletion is performed by this migration.
- Timeout limits remain provisional until real successful cold-run timings are
  collected; YAML/static checks are not a substitute for first-run evidence.

The first live validation should be a documentation main run, followed by manual
release-baseline families on their matching platforms. Do not publish a release
to test the migration. Verify job logs, source SHA and uploaded artifacts before
claiming the self-hosted migration is operational.
