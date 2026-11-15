# Session metadata and Step recovery continuation

Draft #2081 continues epic #1753 after review of `f8c042f3`. This increment
adds native Session name/tags/pin editing and measures an existing Step
reconnect gap. Streaming display remains disabled; the epic is incomplete.

The later [archive/restore continuation](session-archive-20261115.md) adds
archived browsing and changes metadata reads from GetSession to ListSessions.
The source and checks below describe this earlier checkpoint.

## Exact source and ownership

- Implementation: `2ecd8a74bbae74dc12c1390cab5dfcbc1f98f3da`.
- Palette correction: `b19e296ce5b3f9db9f3b9d61db53a0ab2bbeab2a`.
- Final runtime: `52a33eed64de11973c9165987d9d3e072fd45209`. It additionally
  observes metadata input notifications, which cover marked-text transitions
  that do not emit ordinary Change events. Explicit unmarking notifies the
  observer; composed text remains owned by the input method until then.
- Stronger worker regression, test code only:
  `a2aeb6616cba74860acda56f6af2242f14766443`. Its only changed file is
  `src/client.rs`, entirely inside `#[cfg(test)] mod tests`; the production
  prefix is byte-identical to `b19e296`. Both complete library suites and
  strict unit-test clippy passed again after this change. The final runtime
  above includes this stronger regression and the later composition fix.
- Same isolated clone: `formless-gpui-1753-sol-20261115`; branch:
  `work/sol-gpui-1753-20261115`.
- Main remained `e9bf104fb9c1bf1bff4f56e05efa92b17a1bfc14`, already included
  by the prior continuation. Original #1776 remained
  `f8c23e0d5726fdc7d50989109f198301feb45cc9`; its worktree was clean on the
  read-only check. Neither its branch/worktree nor any writer was changed or
  controlled. Writer terminal activity remains unverified after the earlier
  sandbox-denied process inventory; no alternative inventory route was used.

Protocol, manifests, dependencies and both lockfiles are unchanged relative
to the reviewed `f8c042f3`. Normal dependency trees still exclude embedded
core/runtime/TUI; the GUI-free tree excludes GPUI. The real daemon remains an
optional test-only dependency.

## Native Session editing

The shared action registry exposes Session details in the palette and the
Sessions page's Details button. In Sessions it targets the chosen list entry;
elsewhere it targets the open Session. Previewing a different list entry does
not activate it or send its composer's draft.

Opening the editor requests a fresh `GetSession` for the frozen target before
enabling Save. Responses are matched by independent operation UUID, connection
and Session identity. A metadata response cannot install a transcript or follow
the current navigation generation to another Session. Each explicit Save sends
only one existing setter: `RenameSession`, `SetSessionTags` or
`SetSessionPinned`. It neither starts a Run nor calls a provider. These setters
replace a current field and have no compare-and-swap condition; the dialog
explicitly discloses that another client's intervening edit can be overwritten.

Successful replies must decode the original Session and confirm the requested
field, including existing daemon tag normalization. Missing, malformed,
foreign-Session or mismatched success replies become unknown outcomes. Errors
retain the edit. Reconnect or an unknown outcome freezes Save until an explicit
fresh Reload. No write is automatically replayed. Metadata operations have a
separate pending variant and cannot resolve an unknown Send or question answer.
Live input typed during a pending save is not replaced by its acknowledgement.

Names are trimmed, nonempty and one line on submission. Tags are one per line;
spaces/commas within a tag are preserved before the daemon normalizes case,
leading `#`, empties and duplicates. The native editor limits input to 16 KiB,
128 tags and 256 bytes per tag, with a TUI handoff for unsupported metadata.
Blank tags clear the set. Oversized committed input edits are rejected whole
and rebuild the input to clear the rejected edit's undo history. Save, field
changes and Reload recheck current marked text, including before deferred
observers. Escape finishes composition before cancelling.

The modal defaults to Cancel, traps focus, keeps input editing keys distinct
from body scroll keys and reveals focused controls after resizing/text scaling.
Name and Tags have separate input entities/undo histories. Copy edit preserves
the current text for manual recovery. Cancel and Reload explicitly discard
unsaved metadata edits; neither undoes a dispatched save. Drafts in the main
composer remain untouched. The editor's metadata read uses the existing full
Session RPC; it does not establish a smaller metadata-only transport budget.

## Step contract measured through actual IPC

The fifth daemon/provider test starts a private Run with Step mode enabled and
a deliberately missing fixture Tool, then receives a real `StepPending`.
The subscribed desktop receives an explicit Formless TUI handoff notice.
The test disconnects its clients and reconnects a subscriber. Before and after
reconnect, `GetSession` reports `step_mode: true` and `running: true` but has no
pending Step ID in `pending_events`; Subscribe does not replay the prompt.
Using the previously observed ID, an explicit raw `StepDecision { skip }`
allows the Run to finish. Exactly two fake provider requests occur, no external
Tool executes, and private fixture/lease teardown passes.

This confirms a recovery limitation, not native Step parity. Source audit
agrees: `GetSession` samples the Profile-wide pending-attention list, whose
retained variants omit Step; `step_gate` holds the opaque ID in a waiter.
See [snapshot](https://github.com/athiwatc/formless/blob/b19e296/crates/formless-core/src/daemon.rs#L2011),
[attention retention](https://github.com/athiwatc/formless/blob/b19e296/crates/formless-core/src/daemon.rs#L1671),
[Step gate](https://github.com/athiwatc/formless/blob/b19e296/crates/formless-core/src/daemon.rs#L7352).
The notice is historical and only for a subscribed Session; it does not assert
a currently recoverable prompt, expose controls for an invented identity or
claim Profile-wide Step coverage. Native Step decisions remain unavailable.
Restoring them needs an established inventory/correlation contract.

## Checks and evidence

All Cargo commands use the optional manifest, `--offline --locked`, two Cargo
jobs, incremental disabled and dev/test debug info disabled. Raw evidence stays
under ignored `evidence/gpui-1753-sol/`.

| Check at final runtime source `52a33ee` | Result / evidence |
|---|---|
| `cargo test --no-default-features --lib` | 167 passed; `metadata-ime-final-model.log` |
| `cargo test --features test-support,thai-breaks --lib` | 172 passed; `metadata-ime-final-gui-model.log` |
| `cargo test --features test-support,thai-breaks --test shell` | 135 passed; `metadata-ime-final-shell.log` |
| `cargo test --no-default-features --features daemon-tests --test daemon` | 5 passed; `metadata-ime-final-daemon.log` |
| `cargo test --all-features --doc` | Passed, zero doctests; `metadata-ime-final-doc.log` |
| Strict clippy (`-- -D warnings`) | Passed: GUI-free lib/unit tests, GUI/Thai lib + shell/render targets, ax-dump/Thai binary, daemon fixture. `metadata-ime-final-clippy-{model,gui,bin,daemon}.log`; 26 existing core dependency warnings remain. |
| Windows GNU GUI-free all-targets cross-check | Passed; `metadata-ime-final-windows-model.log`. No linking/native GUI execution. |
| Native fonts/Metal offscreen renderer | 43 states / 75 captures; `metadata-ime-final-native.log`, PNGs in `metadata-ime-final-native/`. |
| Formatting, whitespace, normal dependency isolation | Passed; `metadata-ime-final-{model,gui}-normal-tree.txt`. |

At test-only `a2aeb66`, complete library suites passed again: 167 without GUI,
172 with GUI/Thai support, plus strict GUI-free library/unit-test clippy.
Logs are `metadata-regression-final-{model,gui-model,unit-clippy}.log`.
The stronger actual connection-worker/duplex regression retains unknown Send
and question operations through reconnect, Stop ACK, metadata ACK and another
question's ACK. It verifies the metadata ACK was processed and the exact
retained unknown set is unchanged. The malformed-reply unit test is explicitly
scoped to decoding rather than claiming worker acknowledgement safety.

The real daemon suite additionally verifies metadata read/rename/tag/pin ACKs
and stored normalized values, independent of the open transcript. Existing
transport/navigation/forecast/recovery, provider retry, Tool-turn and native
question integration cases still pass. The three new shell cases cover a
chosen Session different from the open one, original target/operation binding,
unrelated replies, rejected save/manual retry, exact Unicode and tags, pinning,
composer preservation, lost-disconnect recovery, stale ACK, explicit Reload,
IME guards before deferred observers, repeated 100/150/200% resizing, whole-paste
rejection and undo reset. An additional input-handler regression ends oversized
preedit through an unmark/notification without a Change event and verifies the
earlier text is restored; ordinary Save guards still inspect live marked text.

New native captures exercise Name, multiline Tags, Pin and waiting states at
wide and minimum 480×360 / 200% sizes. Keyboard reachability and focused input/
footer bounds are asserted. Selected final editor captures were visually
inspected. Native font shaping still emits the documented pinned-layer Thai
diagnostics. These checks do not establish foreground OS IME, text selection,
VoiceOver speech or human usability acceptance.

Failures were retained. Early attempts included unsupported component helper
calls/imports; test assumptions about enabled-node representation and
programmatic input-change events; and an oversized-paste test using the helper's
character-by-character typing path. That test run was interrupted through its
own terminal session, then corrected to deliver one input-handler commit.
The first Step test had no Session subscription and correctly received no Step
event; its setup was corrected and the subscribed-only scope documented.
The full `2ecd8a7` shell run found a real regression: metadata's “Go to” group
made it match “go sessions” before navigation. `b19e296` gives it a Session
group; the existing regression test and all 135 shell tests pass unchanged.
Earlier captures/artifacts and failed logs are not substitutes for final checks.
An initial oversized-composition case combined Save/Escape and assumed preedit
would remain active; its assertion failed. The corrected case isolates the
unmark/notification boundary, while Save/marked-text guards have separate
coverage. The notification-observer fix and complete final checks include both.

## Fresh development artifact

The packager ran with ResourceWarning treated as an error, `--profile dev
--offline`, output `evidence/gpui-1753-sol/package-3b739d2`, from clean
`3b739d214e59fbc0eec965657b67013baca725e3`. Its only changes from tested
`52a33ee` are README/report documentation; the implementation and tests match.

- Binary SHA256:
  `7e739ff6dafc76094639fce676facd6a3969e0e673e700c319f8e55519a6f045`.
- ZIP SHA256:
  `ac767b781301dadf5e924f337cb0eafb8fb19637892d7e533612a0d62a78092d`.
- Plist identity, safe archive paths/types, integrity, executable mode/byte
  identity and hashes passed; 655 archive files. Evidence:
  `metadata-ime-final-package.log`, `metadata-ime-final-package-verification.json`.
- 415 build packages / 649 notice files; 46 packages have no root notice file.
  Manual licensing/assets/stdlib review remains.
- Static inspection reports 21 system-library loads and linker ad-hoc signing,
  no Team ID, unbound plist and no sealed resources. Nothing was installed or
  launched; no distribution signing/notarization occurred.

Earlier `2ecd8a7`/`b19e296` artifacts and verification remain historical and
are not substitutes for this final composition-fix bundle.

## Remaining gates and blocked actions

The [ordering report](stream-ordering-20261114.md) still governs live-token
work: `deltas: false`, canonical snapshot refresh, no invented snapshot cursor
or replay safety. Delayed snapshots, gaps/overflow, reconnect and general delta
reconciliation remain unproven. The new Step case establishes its stated gap
only. Native security approvals/Step, remaining Session/composer/context/resource
actions, general-purpose surfaces, durable preferences, measured performance,
distribution and Windows/Linux native execution remain open. Prior Windows
daemon cross-check failure due to missing MinGW GCC is unchanged; no compiler
was installed or that blocked check repeated without a toolchain change.

Foreground Mac keyboard/selection/real IME/accessibility/human 200% and bundle
launch remain blocked by existing Computer Use approval `proud-river-57508678`
for Formless Sol Validation. Its supported card read still showed no owner
response. The prior launch/control outcome remains uncertain and was not
repeated, replaced by another app launch or bypassed. No OS permissions changed.

Keep #2081 draft. Parent review must inspect the exact published head and
recheck live main/#1776 ownership before integration. Do not merge or close #1753.
