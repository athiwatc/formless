# Local daemon client continuation — 2026-11-14

This is an incremental implementation for #1753, not completion of the epic.
The published prototype in #1776 is preserved. No original branch, worktree,
writer, paused Cloud session, daemon policy, credentials or OS security setting
was changed. The desktop remains a separate optional workspace with its own
lockfile and pinned GPUI Kit 0.9.1 / GPUI-pre 0.3.12.

## Provenance

- Isolated clone: `formless-gpui-1753-sol-20261115` in the task workspace.
- New branch: `work/sol-gpui-1753-20261115`.
- Published checkpoint: `f8c23e0d5726fdc7d50989109f198301feb45cc9`.
- First main reconciliation: `7ab3e1cf96a44dd4fc52911a88682e653ad555d3`,
  incorporating main `93f68358104166a5585be388af4191f894ea88e5`.
- Implementation: `b1696d8493ca2e26d51b27e850f81ad9798c0b88`.
- Second main reconciliation: `1e68d032e49be62fd1c73722a1d2198981d0d523`,
  incorporating main `546c26d23641e3982300d60d9e5c4350291fb45a`.
- Metrics scope and native states: `1dc16a1adddc482efed2f594cd89f4aa5c704553`.
- Native 200% header correction: `524e53afc902c733c04db08dc60b7119d5f9a224`.
- Deterministic provider/navigation and teardown fixture:
  `ddcf55eb7a3f97d517959f93341dfeb7e2c9efc1`.
- Third main reconciliation: `26f9eddfa4e657985d8479d13e45752110fb1a77`,
  incorporating main `47ebac8c30af92a7cd937501f9761df101b83187`.
- Post-publication main reconciliation: `cc68b2cb911400a19c479b3d70dedb119da58a65`,
  incorporating main `da34c1172bf049edad6a0759ca268ad3af915f4e`.
- Published draft: [PR #2081](https://github.com/athiwatc/formless/pull/2081).

The desktop tree is `3ad560af5cc4a63c4ffcbc8e250ec53ffc793086` and the
protocol tree is `a9912076a69b36043508622f3d9bf25742405790` at both the initial
implementation (`b1696d84`) and second reconciliation (`1e68d032`) commits.
Later UI/test follow-ups are listed above. Daemon integration
was rerun against the updated core after each main merge. The final merge changes
neither desktop source nor render tests relative to the header correction, and
changes no protocol or workspace dependency manifest.

## Implemented behavior

`--connect LOCAL_SOCKET_OR_PIPE` connects to an existing local daemon. The
normal GUI dependency graph contains no core, runtime or TUI crate. The
optional `daemon-tests` feature brings the daemon into the private integration
fixture only. The client does not open daemon storage or configuration.

The worker owns Tokio IPC, bounded command/update queues, capped and
cancellation-safe NDJSON framing, correlated requests, protocol negotiation,
deadlines and reconnect backoff. Window selection generations reject stale
replies, including A→B→A and queued replies already waiting at the view boundary.

Live mode supports real Session listing/activation, New Session,
Send/Steer/Queue/Stop, canonical transcript refreshes and loaded-text Search.
Sending stays disabled until the selected Session has a current snapshot.
Disconnect or a failed refresh makes Run state unknown and retains the last
confirmed transcript and draft. Unchanged snapshots preserve the viewer and
reading position; changed snapshots splice the changed suffix. Removed Sessions
clear their transcript and retain a bounded draft with eviction notices.

Tools, recorded Run Metrics, Proposals and Settings have bounded read-only
list/detail views, keyboard selection, explicit load errors, refresh and exact
JSON copying. They are not edit forms or full parity with the TUI. Pending
decisions from events and snapshots remain visible through a TUI handoff.

## Actual ordering and retry contract

The daemon has one output queue/writer per local connection. Its independent
event broadcasts and request responses enter that queue at different points.
`GetSession` reads canonical state without an event watermark or stream offset.
Queue order does not establish which deltas are already represented by a
snapshot. The prototype's local snapshot guard does not establish that either.

This client requests `deltas: false`. Events invalidate canonical reads rather
than mutating transcript text. An invalidation during a pending read survives
that reply and schedules another read; a delayed event may cause a redundant
read, never a delta replay. A two-second canonical poll also revalidates the
selected Session. This deliberately gives up token-by-token display. It is not
a new daemon sequence contract, a linearizable snapshot protocol, or a claim
that the existing trusted event/view seams are network callbacks.

Daemon send keys are scoped to connection identity. The client never retries
mutations automatically, and remembers Sessions with uncertain action outcomes
across reconnects. Another Send in such a Session is blocked in that window;
the user must inspect and continue through the established TUI. Draft text is
kept after acknowledgement, rejection and transport failure. Restarting the
application does not make an uncertain retry safe. In-flight Windows pipe-write
cancellation/resource retention has not been established by native testing.

## Recorded checks

Host: macOS 27.2 (26B123), ARM64; rustc/cargo 1.100.0. Cargo used two jobs,
incremental off, debug info off, `--offline --locked`, and this clone's private
desktop target directory. Commands below run from `crates/formless-desktop`.

| Check | Result |
|---|---|
| `cargo fmt -- --check` | Passed |
| `cargo test --no-default-features --lib` | 147 passed |
| `cargo test --features test-support,thai-breaks --lib` | 152 passed |
| `cargo test --features test-support,thai-breaks --test shell` | 122 passed |
| `cargo clippy --all-targets -- -D warnings` in GUI-free, plain GUI, `test-support,thai-breaks`, and `ax-dump` configurations | Passed |
| `cargo clippy --no-default-features --features daemon-tests --test daemon -- -D warnings` after latest main merge | Passed; existing core dependency warnings remain |
| `cargo test --doc` | Passed |
| Python platform-checker failure-path harness with ResourceWarning as error | 8 passed |
| Normal dependency-graph assertions | 52-package GUI-free graph has no GPUI; 379-package GUI graph has no core/runtime/TUI |
| `cargo test --no-default-features --features daemon-tests --test daemon` after latest main merge | 1 passed |
| `cargo test --features test-support --test render` at the committed frontend | 33 states, 53 captures, real font checks passed |

The later UI follow-ups were checked again with GUI-free model tests, the
feature-enabled model/shell suite and strict clippy. The final header checks
include actual-font viewport bounds for Refresh and Back at 200%.

The integration test uses a real private-profile daemon and local fake HTTP
provider. It checks one send causing one provider request, canonical Thai/emoji
output, interrupted Session navigation (the provider reply is held until the
other Session is confirmed and the original Run is still running), all four
resource reply shapes, Session
creation and reconnect restoration without mutation replay. This is actual IPC
and provider-path integration, not a fixture directory pretending to be a daemon.
It does not validate a paid provider, real credentials or foreground native GUI
input. The in-process daemon fixture explicitly retires the old socket and waits
for the old daemon's Profile lease to be released before restart; a real daemon
process exit would also tear down its runtime.

Local evidence is retained under ignored `evidence/gpui-1753-sol/`:
`final-model.log`, `final-daemon.log`, `final-checks.json`,
`b1696d84-*.log`, `1e68d032-daemon.log`, `1e68d032-clippy-daemon.log`,
`graph-assertions.json`, `final-scope-checks.json`, `final-metrics-scope.log`,
`resource-header-render.log`, `final-header-shell.log`, `final-header-clippy.log`,
`final-synchronized-daemon.log`, `final-synchronized-clippy.log`,
`cc68b2cb-daemon.log`, `cc68b2cb-clippy-daemon.log` and `shell-*.png`. The native captures include the new
Tools and Metrics resource views at wide/100% and minimum/200%. Those capture serialized view
fixtures, not a foreground GUI connected to the private daemon. Existing Thai
segmentation warnings remain; the pinned toolkit was not upgraded.

Failed attempts are preserved in separate logs: the sandbox blocked loopback
binding and native pasteboard startup; approved normal-access runs proceeded
without changing OS permissions. Fixture corrections fixed a TOML u64 overflow,
HTTP model-discovery handling, and in-process shutdown/lease assumptions. The
native renderer then exposed a genuine empty-sidebar panic; resetting list sizes
and bounding stale virtual-list ranges fixed it, with a regression test over a
populated/scrolled list, empty attachment, repopulation and removal. Visual review
then found that Metrics' Refresh control overflowed at 200% despite the broad
pixel check passing. Grouping actions below the title in reflowed layouts fixed
it; the final native tests assert both controls are inside the viewport, and the
fresh captures were inspected. Metrics also names its 100-recent-Run request
scope instead of implying complete history. A dependency
diagnostic initially used `cargo tree -i` for an inactive optional package and
returned 101; explicit normal-graph absence assertions replaced that incorrect
diagnostic. Prior failures are not counted as passes.

The earlier bounded native fixture was interrupted by the Mac disconnection and
has no recorded final result. It is not counted as a pass. After reconnection,
read-only process inspection found no Formless test process. The fresh integration
runs above passed independently. Native screenshot filenames are reused on
subsequent render runs; per-attempt logs are retained.

The first hosted desktop run, [43632030492](https://github.com/athiwatc/formless/actions/runs/43632030492),
at `871447053b45240f85f6831b9467bff14cbf5127` failed on all three platforms
before any job steps ran. Each job reported runner ID zero and no steps; the
Windows job log returned BlobNotFound. The cause is not established by these
results. They provide no Windows/Linux build or test evidence. The Code Review
diagnostics tool also required local Desktop access unavailable to this task.

## Remaining acceptance

- Foreground native keyboard input, text selection, real Thai/Japanese/dead-key
  IME behavior, VoiceOver and human 200% visual acceptance are unverified. The
  Computer Use approval for “Formless Sol Validation” was still pending at this
  record; the previous launch/control outcome was uncertain. No repeated launch
  or UI control was attempted while approval was unresolved.
- Windows/Linux native builds, rendering, input and accessibility have not run
  here. The added desktop workflow arranges native builds plus portable model,
  daemon/provider and headless keyboard/layout coverage on all three platforms;
  it does not claim that native IME or screen-reader gates passed.
- Token streaming needs a genuinely established snapshot/delta reconciliation
  contract. The current safe canonical-only mode does not close that requirement.
- Native decision forms; Run resume/abandon/step controls; Session rename/fork/pin/tag/export/comparison actions;
  attachments, model/Role selection; full Tools/Metrics/Proposals/Settings edit
  flows; and dedicated Memory/Skills/MCP/Jobs/Providers/Invocations/sub-agent/plan/
  compaction surfaces remain open.
- Profile discovery/setup beyond the explicit existing-CLI handoff, UI preference
  persistence, packaging/signing/update integration, platform support tiers and
  foreground performance budgets remain open. Transcript paging is not added.

Do not merge this continuation, close #1753, or treat the prior prototype's
screenshots as acceptance for these changes. Parent review must recheck the
original PR head and ownership, inspect exact new commits, and coordinate any
further integration.
