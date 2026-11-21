# Recovery, stream characterization and development packaging

Later native forecast confirmation and current evidence are recorded in
[the forecast continuation](forecast-confirmation-20261114.md). The TUI-only
forecast handoff described below is historical.

This continues #1753 in draft PR #2081. It does not complete the epic or
establish a supported native release. The original #1776 branch/worktree and
writers remain untouched; its last verified head is
`f8c23e0d5726fdc7d50989109f198301feb45cc9`.

## Exact implementation

- Prior reviewed retry-guard fix: `317a66370d4da2157a523b82e54682d87a371671`.
- Main reconciliation `432d33cf3f9ec0b050424670e563dc32866800c7` brought in
  `4ceb6ac2`; subsequent reconciliation
  `7e081e86fdd898777129ee8f43da2e5b4c123a54` brought in
  `16b7f436e15b1e82cfa8846e1e2610cc379afa40`.
- New code, tests, workflow and packaging helper:
  `59d05633dd705d9d09443a95f5ae7f9153575579`.
- Same isolated clone and branch: `formless-gpui-1753-sol-20261115`,
  `work/sol-gpui-1753-20261115`.

The optional workspace, independent lockfile and exact GPUI family pins remain.
Only a test-feature dependency edge to the already locked `futures-util` package
was added; the normal GUI still does not embed the daemon/runtime/TUI. The root
lockfile and launch/release behavior were not changed by this implementation.

## Real stream characterization

The private real-daemon/fake-HTTP-provider fixture now has a second real IPC
subscriber with `deltas: true`. The provider emits one prefix, then waits at a
gate. The observer receives that delta and requests `GetSession` on the same
connection while the answer is held. Its canonical reply says running and has
no assistant message. After releasing the gate, the remaining delta and
`MessageComplete` arrive; concatenated live text matches the Unicode answer,
and a subsequent canonical reply has exactly one persisted answer.

The desktop is simultaneously navigated to another Session before the first
provider response, then back to the original Session. It still uses canonical
snapshots with `deltas: false`; no trusted presentation seam is being passed off
as the real event transport.

These tests establish the observed mid-answer and completed-answer boundaries.
They do not establish a watermark, offsets, a replay cursor, or an atomic
snapshot/publication boundary. Provider retry/restart, slow-subscriber gaps,
multiple assistant turns in one Run and commit/publication interleavings still
need characterization before enabling token display. A separate partial live
overlay with explicit missing-prefix handling can be evaluated against those
cases; complete replay must not be assumed. No protocol contract was invented
or daemon behavior redesigned here.

## Run recovery

`GetSession.unfinished_run` is decoded and validated. The UI offers Resume Run
and Abandon Run only for a current connected snapshot with a paused/interrupted
Run. Both actions share the typed registry, palette and buttons. Navigation,
disconnect and failed refresh disable or remove the controls; queued retired
snapshots cannot restore them for the wrong Session. Draft text is retained.

The existing `ResumeRun`/`AbandonRun` requests and daemon admission path are used
unchanged. Resume can require a forecast confirmation; that is still an explicit
TUI handoff. No confirmation is synthesized or bypassed. These are Session-scoped
existing requests, not a newly introduced conditional backend Run-id operation.

Unknown earlier outcomes block both Send and Resume. The adversarial duplex
test now covers attempting either action after reconnect, successful/rejected
Stop replies and an unrelated Session's Send acknowledgement. Every mutation
retains its unique operation identity; no automatic mutation retry was added.

The real fixture also interrupts a gated provider request, resumes it through
the desktop transport and sees exactly one new provider request. A second
interruption is abandoned through the same transport and starts no provider
request. Reconnect preserves the resulting canonical history without replay.

At 200%, native rendering exposed a focused recovery button outside the
viewport. Descendant scroll anchors now reveal the focused recovery control or
input when the complete composer is taller than the viewport. The real-font
test traverses both buttons with Tab, advances the requested native frames and
asserts viewport containment plus space for the focus ring. Error previews read
at most 501 characters in Render, and the UI takes ownership of recovery data
instead of cloning a potentially large error string.

## Development package evidence

`scripts/package-desktop.py` builds the optional binary with Cargo's locked
dependency graph. It uses Python 3.9+ standard-library modules, rejects existing
output directories and refuses uncommitted source unless `--allow-dirty` is
explicit. It records commit/profile/target/build command and binary/lock hashes,
collects notices for Cargo build packages and verifies the ZIP's executable.
No install, launch, signing or OS permission action is performed.

The clean Mac development artifact is retained locally under ignored
`evidence/gpui-1753-sol/package-59d05633/`:

- Source: `59d05633dd705d9d09443a95f5ae7f9153575579`, `dirty: false`.
- Profile/target: dev, `aarch64-apple-darwin`.
- Binary SHA256: `3b034711a30aee4d489cc4501c2a0d58e9701a22935c8c3b7b25046087cdaac5`.
- ZIP SHA256: `6b6f317a5dff735107fd6e4f62d213445874d78a22c112d69e7148d8dc22f770`.
- `Formless Development.app` metadata passed `plutil -lint`. Executable bytes,
  ZIP integrity, safe entry paths and executable permissions were checked.
- The earlier smoke bundle's load commands named system frameworks/libraries.
  No third-party dylib bundling was inferred from the Cargo package count.
- 415 Cargo build packages, 649 copied notice files including Lucide. 46
  packages have metadata without root notice files; manual distribution,
  asset and standard-library licensing review remains open.

The bundle has a development identity, no distribution signing and no accepted
launch/install result. The binary's load command supplies the plist's minimum
OS value; that is not a claim that Formless was tested on that OS release.
Windows/Linux portable packaging is arranged in the optional desktop workflow
but has not executed here. Signed installers, icons, notarization, updates and
clean-install/upgrade acceptance remain open. A separate release build was not
attempted with approximately 10 GiB free; this artifact explicitly records dev.

## Checks and limits

Commands use `--offline --locked`, two Cargo jobs, incremental off and debug info
off, as in the earlier report. Fresh results at the implementation source:

| Check | Result |
|---|---|
| GUI-free model suite | 150 passed |
| Model suite with Thai study | 155 passed |
| Headless shell suite | 123 passed |
| Native renderer with real fonts | 35 states, 55 captures passed |
| Real daemon/provider/stream/recovery/reconnect fixture | 1 passed |
| Strict clippy in four build configurations and daemon fixture | Passed; existing 26 core warnings remain |
| GUI-free Windows GNU all-targets cross-check | Passed; no linking or execution |
| Formatting and doc tests | Passed |
| Python packaging with ResourceWarning as error | Clean development build/package passed; existing-output and dirty-source refusals checked |

Local logs are retained in ignored `evidence/gpui-1753-sol/`: `recovery-frozen-*`,
`recovery-model-final.log`, `recovery-native-frozen.log`, `59d05633-model.log`,
`59d05633-windows-cross.log`, `59d05633-daemon*.log`, `package-59d05633*`, and
`recovery-native/shell-*.png`. The final wide and 200% recovery captures were
visually inspected. They are serialized view fixtures, not a foreground GUI
connected to the private daemon.

Failed attempts are retained separately: collapsed-sidebar fixture setup, the
actual offscreen focused control, an unsupported anchor call on a stateless
element, not advancing requested native frames, and a viewport-size typo in
the focus-margin test. The latter also stopped one clippy orchestration; the
corrected strict configurations and frozen checks passed independently. An
initial dirty-source development package is clearly marked dirty and is not
the clean artifact cited above. Capture filenames are reused on reruns; logs
are per attempt. Existing Thai segmentation warnings remain.

The existing Computer Use approval `proud-river-57508678` for Formless Sol
Validation still had no affirmative owner reaction on the last supported read.
Its previous launch/control outcome remains unresolved; no repeat or bypass
occurred. Native keyboard, real IME, selection, VoiceOver, human 200% acceptance,
bundled launch and Windows/Linux execution are not established by these checks.

Full streaming, native decision/forecast forms, Step controls, Session metadata
actions, attachments/model/Role selection, artifact/diff and broader general
purpose surface parity, UI preference persistence and performance budgets remain
open. Keep #2081 draft; do not merge or close #1753 on this evidence.
