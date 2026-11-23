# CI runners

Regular CI, documentation checks, optional desktop CI and the manual cold
release baseline use standard GitHub-hosted runners. Regular CI retains the
Linux x64 and Apple Silicon matrix and its same-repository PR policy. Documentation
checks remain restricted to main, and the cold baseline remains manual on main.

| Workflow | Runner labels |
| --- | --- |
| CI | `ubuntu-24.04`, `macos-15` |
| docs | `macos-15` |
| Desktop | `ubuntu-latest`, `macos-latest`, `windows-latest` |
| release-baseline | `ubuntu-24.04`, `macos-15`, `macos-15-intel`, `windows-latest` |

The separate release workflow still uses self-hosted Linux x64, Apple Silicon
and Intel Mac runners for its existing jobs. Its hosted Windows and Linux arm64
jobs, trigger conditions and publishing setup are unchanged. A release can
queue if a required self-hosted runner is offline. Do not use a personal
self-hosted runner for untrusted contributions to this public repository.

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

The CI dependency step installs `poppler-utils` when the tools are missing.
Playwright installs Chromium and its required Linux system libraries.

## Trust boundary

No current PR workflow intentionally targets self-hosted runners. Do not
approve unreviewed changes to workflows that could target a persistent runner.
Release builds accept version tags and manual main dispatches; publication
remains tag-only. Review source and workflow changes before merging or tagging.
Runner registrations, branch protection, and host permissions are not changed.

## Pipeline behavior

- Release publication is not canceled halfway through a run.
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

Validate hosted runs using job logs and the exact source SHA. A baseline run is
not required for a routine code change. Do not publish a release to test runner
routing.
