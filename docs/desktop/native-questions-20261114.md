# Native Profile questions continuation

Draft #2081 continues epic #1753 with native question forms. Security approvals
still use Formless TUI. Streaming display remains disabled. Neither this
increment nor its offscreen checks completes the epic.

## Source and ownership

- Question implementation: `e7f98de09bb72ca7a5f7ac44b197ee4f9c42b8e3`.
- Main reconciliation: `68bf2f444b7800422371fc994bb4d82450fd398f`, including
  `e9bf104fb9c1bf1bff4f56e05efa92b17a1bfc14`.
- Fully checked implementation and clean bundle source:
  `4ed913f369195e4975e921572493f77355e75d75`. This follow-up fixes textarea
  visibility across resize/text-scale changes and lost-disconnect recovery.
- Isolated clone: `formless-gpui-1753-sol-20261115`; branch:
  `work/sol-gpui-1753-20261115`.
- The final fetch still found main at `e9bf104f` and the original #1776 at
  `f8c23e0d5726fdc7d50989109f198301feb45cc9`. The original worktree was clean
  on its read-only check. Its branch/worktree and writers were not changed or
  controlled. Process inventory remains unverified after the prior sandbox
  denial; a clean worktree is not evidence of a stopped writer.

No protocol, dependency or lockfile change was needed. Normal dependency trees
still exclude embedded core/runtime/TUI; the GUI-free tree excludes GPUI.
`daemon-tests` links the real daemon only for tests. Other workers' unrelated
changes were incorporated through published main, not reimplemented.

## Behavior and limits

The Questions command/header opens a Profile-wide directory without stealing
focus when events arrive. Every form shows its original Session identity,
even when another Session is selected. Forms support the existing one-to-four
question fields, single/multiple choices and optional own text. Empty answers
retain the daemon's existing unanswered semantics. Long visual labels are
shortened; accessible labels, prompt copying and submitted choices retain the
original values. Security permission policies are not represented as ordinary
question answers.

Explicit Submit dispatches `AnswerQuestion` once. Only `answered: true` reports
acceptance. `false` reports closure; a definite rejection retains the answers.
Missing or malformed acknowledgements and a disconnect after dispatch retain
the exact unknown operation and block that question. No mutation is replayed.
An unrelated Stop or another question's acknowledgement cannot clear unknown
Send or answer state. The existing Run-action guards remain request-specific.

The worker validates the frozen definition and connection at dispatch. The
view reads current marked text and input before submitting, including before
deferred input observers run. Old-connection inventories/acknowledgements
cannot enable a form. A shared connection counter retires waiting state even
when the bounded update queue loses the disconnect notification. A fresh
connection permits editing/copying while the retained unknown identity still
prevents resubmission.

Question events advance a connection-local revision. Snapshot hydration is
refused if a question/close event overlapped it; closed identities remain
tombstoned. Conflicting or unsupported replacement definitions retire the old
identity. This protects question ownership and observed closure, not general
transport ordering or transcript replay. Existing questions hydrate from a
Session's global `pending_events`; opening a Session after connecting restores
them. New question events arrive Profile-wide.

Bounds are explicit: 64 live questions, 512 close tombstones per connection,
256-byte identities, 128 KiB per definition, up to 64 choices per field, and
64 KiB of own text per field. Exceeding history limits disables native answers
with a TUI handoff instead of evicting tombstones. Unknown-answer history is
bounded separately at 512 and refuses additional answers rather than forgetting
unknown outcomes. Oversized input edits are rejected whole after composition
ends. Authored drafts retain at most 16 questions / 4 MiB in this window, with
an eviction notice. They are not persisted; closed answers remain explicitly
copyable until evicted or the window exits. Changed definitions cannot inherit
old answers or undo history.

The modal defaults to Close, traps navigation, supports paging and bounded
scrolling, and restores focus deliberately. Read keys are scoped outside Input
contexts so textarea cursor keys retain their meaning. Each field has its own
input entity. Escape ends composition before closing. The two-line textarea
scrolls internally and is revealed after focus, resize or text-scale changes.
The visible footer label is Submit with accessible name Submit answers; its
earlier longer label wrapped and clipped content at 200%.

## Final checks at `4ed913f3`

All Cargo commands use `--manifest-path crates/formless-desktop/Cargo.toml
--offline --locked`, with `CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`, and
`CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`. Raw evidence is retained
under ignored `evidence/gpui-1753-sol/`; these are fresh results for this source.

| Check | Result / evidence filename |
|---|---|
| `cargo test --no-default-features --lib` | 164 passed; `questions-final-model.log` |
| `cargo test --features test-support,thai-breaks --lib` | 169 passed; `questions-final-gui-model.log` |
| `cargo test --features test-support,thai-breaks --test shell` | 132 passed; `questions-final-shell.log` |
| `cargo test --no-default-features --features daemon-tests --test daemon` | 4 passed; `questions-final-daemon.log` |
| `cargo test --all-features --doc` | Passed, zero doctests; `questions-final-doc.log` |
| Strict clippy, `-- -D warnings` | Passed: GUI-free library, GUI/Thai library + shell/render targets, ax-dump/Thai binary, daemon test. Logs `questions-final-clippy-{model,gui,bin,daemon}.log`; 26 existing core dependency warnings remain. |
| Windows GNU `cargo check --target x86_64-pc-windows-gnu --no-default-features --all-targets` | Passed; `questions-final-windows-model.log`. No linking/native GUI execution. |
| Native offscreen `cargo test --features test-support,thai-breaks --test render` | 41 states / 67 captures; `questions-final-native.log`, PNGs in `questions-native/`. Real Mac fonts/Metal, no foreground window. |
| Formatting, diff whitespace, normal dependency isolation | Passed; trees and `questions-final-dependency-isolation.json` retained. |

The six added shell cases cover exact choices and Unicode/multiline text,
foreign-Session identity, F6/Tab/Space, textarea arrow keys, undo separation,
closed acknowledgements, composer preservation, retained answer copying,
stale updates, composition before deferred observers, whole-edit rejection,
repeated resize/100–200% scale, and a lost disconnect update followed by
reconnect. Raw IPC tests additionally cover unknown Send + unknown answer,
reconnect, Stop acknowledgement and a different question's acknowledgement
without releasing either unknown guard.

The fourth daemon/provider case uses actual local IPC and a private fake HTTP
provider. `ask_user` opens two fields in Session A while the desktop selects B.
Reconnect restores the global question through B's snapshot. Structured
multiple choices and Thai/emoji/CJK own text are accepted once and appear in
the provider's real Tool-result provenance envelope. Native duplicate dispatch
is refused; a raw duplicate receives `answered: false`. Exactly two provider
requests occur. Private Profile leases are released and fixtures removed.
The other three existing transport/retry/Tool-turn cases also pass.

Renderer assertions exercise long prompts/labels, keyboard reachability,
focused-control bounds, multiline input containment, waiting controls and
saved-answer copying at wide and minimum 480×360 / 200% layouts. Selected final
question captures were inspected visually. These checks do not establish
foreground OS IME, selection, VoiceOver speech or human usability acceptance.
The pinned text layer still emits its documented ICU Thai-model diagnostics.

Failed attempts remain in the same evidence directory. They include missing
test-helper methods, an initial Tab-order assumption, an offscreen fixture
clicking an unrevealed entry, a Tool-result assertion that initially omitted
the actual provenance envelope, an old status-text mismatch, a clippy clone
warning, the real 200% footer/input clipping regression, and a connection-reset
edit initially placed in the wrong method. Subsequent focused checks and the
full frozen-source checks above pass. Earlier 41-state / 65-capture results
are not substituted for this final renderer run.

## Fresh development artifact

`python3 -W error::ResourceWarning
crates/formless-desktop/scripts/package-desktop.py --profile dev --offline
--output evidence/gpui-1753-sol/package-4ed913f3` built from clean `4ed913f3`.

- Binary SHA256:
  `2790c1027f4900192e3751395356cfe54103ae03eb3cc647b800b12852be1215`.
- ZIP SHA256:
  `ad5673d4fb7da3db99294dfa8ba8086a17f1c8f171f936428323af93a6b3bc92`.
- Plist identity, archive safe paths/types, integrity, executable permissions,
  byte identity and hashes passed; 655 archived files. Logs:
  `questions-final-package.log`, `questions-final-package-verification.json`.
- 415 build packages / 649 notice files collected; 46 packages have no root
  notice file. Manual licensing, asset/stdlib review remains.
- Static inspection reports 21 system-library loads, a linker ad-hoc signature,
  no Team ID, unbound Info.plist and no sealed resources. This is a development
  bundle; no installation, launch, distribution signing or notarization occurred.

## Remaining acceptance and blocked actions

The [stream-ordering report](stream-ordering-20261114.md) remains authoritative:
the retry/Tool-turn fixtures are characterization, not evidence of delayed
snapshot, reconnect, overflow or general delta replay safety. `deltas: false`
and canonical snapshot refresh remain. Full replay needs a demonstrated
contract and adversarial coverage, not a local generation check.

Approvals, Step, broader Session/composer/context/resource parity, durable
preferences, accepted performance budgets, distribution assets/signing,
install/update flows and Windows/Linux native execution remain open. The
existing three-host workflow includes the new model/shell/daemon cases and
packaging; a scheduled workflow is not execution evidence. The prior full
Windows daemon cross-check was blocked by missing
`x86_64-w64-mingw32-gcc` in aws-lc-sys; no toolchain was installed or repeated
without a change in that condition.

Foreground Mac keyboard/selection/real IME/accessibility/human 200% and bundle
launch remain blocked by the existing Computer Use approval
`proud-river-57508678`: Allow Computer Use to use Formless Sol Validation.
The supported approval-card read still showed no owner response. The prior
launch/control outcome remains uncertain; it was not repeated, cancelled,
replaced with a different app launch or bypassed. No OS permissions changed.
The old validation wrapper/fixture must not be relaunched blindly.

Parent review must inspect the new exact head and recheck live main/original
ownership before integration. Keep this PR draft; do not merge or close #1753.
