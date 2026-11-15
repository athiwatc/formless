# Native recorded Session usage

Draft #2081 continues epic #1753 after independent source review of `d63d294c`.
Runtime/tests: `382baf845cb0a80ab8c4494cf44b4a8c401756c0`.
The PR remains draft, streaming remains disabled, and the epic is incomplete.

## Behavior and actual contract

Context now offers an explicit Read usage action for the open, canonically
loaded Session. It uses the existing `SessionUsage` RPC. Opening Context,
selecting a Session, receiving events and reconnecting do not trigger a usage
read. The action does not write Session state, start/stop work or call a
Provider. It keeps composer drafts and focus in place.

The projection displays recorded Session totals and last Run counters:
steps, prompt/completion tokens, cached read tokens, optional cache-write
tokens, estimated USD cost and declared unpriced models. An omitted optional
cache-write counter is not reported, rather than invented as zero. Null cost
is unavailable, not zero. Small nonzero estimates remain nonzero; the initial
fixed-eight-decimal formatter failed a retained regression at USD 0.000000004
and was replaced with the value-preserving float display. Copy usage returns
only this validated displayed projection as JSON. Unknown fields, context-meter
fields and memories are excluded; it is not a copy of the full raw reply.

The existing handler derives costs from stored usage and its current cost
configuration/calculation. These are estimates, not billing receipts. It reads
totals, last Run and count separately. The count filters a global window of
up to 100,000 recent Profile Runs and can undercount a Session's full history.
The UI discloses those limits, labels the count's scope, and does not infer a
consistent snapshot or reject different sample times as contradictory. The
last Run payload has no Run ID; it is labelled accordingly and is not bound
to the currently displayed recovery command. SessionUsage itself does not
prove that a Session still exists at the time of the read. This UI requires a
current loaded canonical target and makes no existence/history-completeness
claim from an empty usage result.

This is not the `ContextUsage` RPC or a live context meter. The audited usage
helper leaves context fields at their defaults, and Session memory count is
not summed there, so these fields were deliberately left out of the native
usage projection. Context/model discovery can have different provider behavior;
no substitute ContextUsage/model call was added here.

Required fields and types are checked. Negative counters, unsupported Run
counts, negative/nonfinite cost, conflicting known cost with unpriced models,
and oversized model lists are unavailable, never silently zeroed/truncated.
At most 64 unpriced model IDs of 256 bytes each are accepted per counter set.
The projection and its copied JSON are consequently bounded. It does not
establish a backend scan-cost/latency budget.

The RPC has no returned Session ID. The actual transport request envelope
binds the captured target (Session, selection generation, connection) and
operation UUID. The engine validates the current target before requesting;
a reply retains its original target even after selection changes. The window
checks it again before installing the result. At most two reads remain
outstanding per connection; the current view disables repeated submission
while waiting. View UUIDs bind Read/Copy callbacks and retire previous results.
A new read, error, Session/Profile change, snapshot invalidation or disconnect
removes prior copy availability. A newer canonical snapshot in the same
selection generation does not refresh the recorded read. Nothing is replayed,
and unrelated reads cannot resolve an
unknown Send, question or metadata mutation.

Read/Copy controls have native accessible labels, keyboard focus and scroll
anchors inside the existing Context panel/overlay. Live composer preedit
blocks the read without discarding text. A reply does not move focus. The
long disclosure and counters remain scrollable at 200%; simultaneous display
of all prose/counters is not claimed at minimum size.

## Validation

All Cargo checks use the optional manifest, offline locked dependencies,
two jobs, no incremental compilation, and dev/test debug info disabled.
Raw logs, captures and command arrays remain under ignored
`evidence/gpui-1753-sol/`.

| Check at final runtime | Result / evidence |
|---|---|
| GUI-free library suite | 188 passed; `usage-verified-model.log` |
| GUI/Thai library + shell suites | 193 + 152 passed; `usage-verified-gui-tests.log` |
| Real private daemon/local fake Provider | Five passed; `usage-verified-daemon.log` |
| Strict clippy (`-- -D warnings`) | GUI-free lib/tests, GUI/Thai lib + shell/render, ax-dump/Thai binary and daemon fixture passed; `usage-verified-clippy-{model,gui,bin,daemon}.log`. Core dependency retains 26 existing warnings. |
| All-feature docs | Passed, zero doctests; `usage-verified-doc.log` |
| Windows GNU GUI-free all-targets check | Passed; `usage-verified-windows-model.log`; no GUI/link/native execution claim |
| Native Mac fonts/Metal offscreen renderer | 53 states / 105 captures passed; `usage-verified-native.log`, PNGs in `usage-native-verified/`, source/hashes in `usage-native-manifest.json` |
| Formatting/whitespace/scope | Passed; protocol, manifests, both lockfiles and relay unchanged |

Exact command arrays/exit codes: `usage-verified-checks.json` and
`usage-verified-native-checks.json`, all ten phases successful.

Pure tests cover required/optional availability, counter/price bounds,
independent sample times, omitted context/future fields, small nonzero cost,
wrong identities and retirement. Engine cases deliver replies out of order
across selection, reject duplicate/foreign target generations/connections
and enforce the outstanding-read cap. The existing actual reconnect transport
regression now processes a usage reply alongside Stop, metadata, export and
unrelated-question ACKs and verifies that neither unknown Send nor unknown
question outcome is cleared.

Three new shell cases cover explicit reads, repeated submission, exact Copy
projection, unavailable cost, stale replies/Read/Copy callbacks, errors,
composer preedit, A-to-B-to-A navigation, preserved drafts, connection epoch
changes, reconnect without replay and 480x360 / 200% Context overlay controls
and Escape. These use the recording view boundary, not a fake daemon success.

The actual daemon/local fake-Provider fixture reads usage after completing
its first real IPC Run. It verifies the stored 2,000 prompt / 100 completion
tokens and estimated USD 0.0022, compares the client's request-owned projection
with the daemon's result and confirms Provider request count stays one.
All five daemon cases and private fixture/Profile lease teardown pass. This
proves the local IPC/fake-Provider path, not paid-Provider or foreground use.

Native captures add pending, ready and error states at wide and minimum-size
200% layouts. Read/Copy and inspector bounds, disabled actions and an error
notice intersection are asserted. Selected final wide/minimum captures were
visually inspected. Full disclosure/counters require scrolling at minimum
size. Thai shaping diagnostics remain at the pinned layer. These are not
human usability, actual OS IME/selection or screen-reader acceptance results.

Failure history is retained. The initial model parse failure in
`usage-first-model.log` was an incorrectly placed new match arm, fixed before
the successful GUI compile. The first focused shell run passed all three new
cases. The initial six-phase full run in `usage-final-*` passed, then the small
cost regression failed in `usage-tiny-cost-reproduction.log`. After correcting
the formatter, all final ten phases above passed. No failure was overwritten
or counted as success.

## Fresh development artifact

Packaging ran with ResourceWarning treated as an error, `--profile dev
--offline`, from clean `3398fe7edf75ff3f50ed28c46f1b783d4027bb8c`. Only README/report
documentation differs from tested runtime `382baf8`. Output is
`evidence/gpui-1753-sol/package-3398fe7`.

- Binary SHA256: `a4fba67e2dce8a7a78156ab546bede4d32af19b78d2f298d16820b68ad6081ca`.
- ZIP SHA256: `7044b8c921216eac4fa609e864752ef798497296d9cb04bc5406de4acbb5f4db`.
- Provenance/plist, safe archive paths/types, integrity, executable mode/byte
  identity, hashes and 21 system-library loads passed. Evidence:
  `usage-final-package.log`, `usage-final-package-verification.json`.
- 655 archive files, 415 packages / 649 notice files; 46 packages lack root
  notices. Manual licensing/assets/stdlib review remains.
- Linker ad-hoc signing, no Team ID, unbound plist, no sealed resources.
  No installation or launch occurred. Distribution signing/notarization and
  native installation/launch/update acceptance remain open.

## Ownership and open gates

Same isolated checkout and `work/sol-gpui-1753-20261115` branch. Main remains
`e9bf104fb9c1bf1bff4f56e05efa92b17a1bfc14`, already included. Original #1776
remains `f8c23e0d5726fdc7d50989109f198301feb45cc9`; its worktree was checked
read-only and clean. No original branch/worktree/writer or paused Cloud session
was changed or controlled. Process inventory remains denied; old writer
terminal activity remains unverified, with no alternate inventory route used.
Separate CI fixture draft #2151 remains with its owner, untouched here.

The supported read of Computer Use approval `proud-river-57508678` for Formless
Sol Validation still has no owner response. The earlier launch/control outcome
remains uncertain. No foreground action was repeated, substituted or bypassed;
no OS permissions changed. Foreground control tools remain unavailable.
Foreground Mac input/selection, actual OS IME/VoiceOver, human 200% and bundle
launch remain blocked. Windows/Linux native GUI execution is unavailable here;
hosted jobs need their own execution evidence.

Streaming remains `deltas:false`; this increment proves no snapshot/event
cutoff, delayed-snapshot consistency, reconnect/overflow or general delta
replay safety. The [ordering report](stream-ordering-20261114.md) still applies.
Native security approvals/Step, remaining Session/composer/context/resource
actions, broader general-purpose parity, durable preferences, performance
budgets and distribution/platform gates remain open. Recovery commands remain
Session-scoped rather than conditioned on a displayed Run ID. Parent must
review the exact new head and recheck live main/#1776 ownership before any
integration. No merge, epic-completion or platform-support claim.
