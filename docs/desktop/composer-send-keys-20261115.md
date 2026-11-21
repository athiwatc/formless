# Isolated GPUI continuation: composer send-key choice

Continues local Find commit `8a18d80a28526ff11dbde4fd7423973555952bb1`
in the separate checkout and `codex/gpui-takeover-20261115` branch.
Parent-reported independent source reviews found no confirmed blocker in
the preceding three commits. Tests/logs were inspected, not rerun by those
reviewers; real OS clipboard/keyboard/IME remain unverified. This new
increment needs its own exact-patch review.

## Behavior and implementation

Commands now offers Use Enter for newlines / Use Enter to send. The default
is unchanged: Enter invokes Send/Steer, Shift+Enter inserts a newline and
Cmd/Ctrl+Enter invokes Queue during a Run. In newline mode, Enter and
Shift+Enter insert newlines; Cmd/Ctrl+Enter invokes Send/Steer. Queue remains
a distinct button. The composer displays the active mapping with the native
platform chord. The setting is explicitly per-window and not persisted.

The kit's existing set_submit_on_enter updates the current textarea rather
than recreating it. Selection, text and undo/redo remain intact. Every
composer rebuild inherits the current mode, including Session switching.
Changing mode while composition is active is refused without committing or
discarding preedit; the notice says keys were unchanged. A stale command
cannot change the mapping behind an open dialog.

Modified Enter has a composer-scoped key binding ahead of the kit's input
handler, preventing the kit from inserting a newline before dispatching it.
The handler rechecks composer focus, Chat and modal ownership and calls the
existing command availability/submit path. Existing Send/Steer/Queue intent,
forecast consent and acknowledgement rules remain authoritative. There is
no optimistic transcript entry, daemon change or new retry path. The binding
adds no global/bare-key shortcut and does not apply to other text fields.

## Verification

All Cargo commands use the optional manifest, --offline --locked, the
isolated task target, two jobs, no incremental compilation and no dev/test
debug info. Evidence is retained in the sibling task evidence directory.

| Check | Result / evidence |
|---|---|
| GUI-free library | 189 passed / `send-keys-model-final.log` |
| GUI/Thai library phase | 194 passed / `send-keys-gui-final.log`; that command subsequently failed at the shell setup check described below |
| Four new focused shell cases | Passed / `send-keys-focused-final.log` |
| Scroll-anchor regression after setup correction | Passed / `send-keys-anchor-second.log` |
| Final full shell suite | 163 passed / `send-keys-shell-verified.log` |
| Strict clippy, GUI/Thai library + shell/render | Passed / `send-keys-clippy-verified.log` |
| cargo fmt --check and git diff --check | Passed |

The new cases exercise both modes, real scoped key dispatch, exact newline
text, primary Send/Steer versus Queue, mode retention across Session
rebuilds, unchanged textarea identity, undo/redo and selection, preedit
retention, modal isolation, palette activation and the serialized recording
client boundary. One chord prepares exactly one existing forecast Review;
no Intent/CommitPrepared or optimistic block is emitted before its reply.
This is view-boundary evidence, not a real daemon/Provider test. The prior
backend integration results are historical and were not rerun here.

Failure history is preserved. `send-keys-first.log` has a missing initial
command-state field; `send-keys-second.log` has test calls using App instead
of Context; `send-keys-fourth.log` has a Debug-formatting diagnostic for a
non-Debug command. These compile failures were fixed before passing runs.
The third/fifth logs retain intermediate focused passes.

The full run in `send-keys-gui-final.log` passed its 194 library tests and
162 shell tests but failed the old scroll-anchor setup: one wheel gesture
stopped at row 90 after the visible composer hint changed viewport height,
while setup required a row before 90. The larger-single-gesture attempt in
`send-keys-anchor.log` also failed, showing per-event wheel clamping. Setup
now uses two real wheel gestures. The original older-row requirement and
exact unchanged-top-position assertion were retained and pass. No failing
log was overwritten or counted as a passing aggregate run.

No native renderer, real OS input/IME/clipboard, foreground window/control,
installation or launch was performed. Headless tests use synthetic input
and uniform-width fonts. Clippy compiles renderer code; it does not render.
The new hint's real-font layout and native accessibility need acceptance.

## Integration and completion

The user's expanded goal is finished GPUI and a clean main integrating PRs.
The [acceptance matrix](acceptance-matrix-20261115.md) records substantive
remaining work and evidence; this increment does not close the epic.
The conflicting original partial tree is preserved. Foreground approval and
the prior uncertain launch/control outcome remain unresolved, with no retry
or workaround. No writer interruption, Claude restart, denied-path/process
inspection, permission change, push, PR update or merge occurred.

Streaming remains disabled. Before integration, review this exact new patch,
resolve ownership/safe recovery, recheck live main/#1776/#2081, and satisfy
substantive native/platform/parity/performance/distribution gates. Then
review and validate the combined integration revision before merging.
