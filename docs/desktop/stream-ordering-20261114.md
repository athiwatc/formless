# Stream ordering: measured boundaries and remaining contract

Later native question forms and the four-case daemon suite are documented in
[the question continuation](native-questions-20261114.md). The ordering cases
below remain characterization; streaming display is still disabled.

This continues draft #2081 / epic #1753 after independent review of
`6372d587d1975aff103fbf3ff0784c64acd2adc4`. Streaming display remains disabled.
This increment adds actual IPC/provider characterization, not a new daemon
protocol or a claim of complete transcript replay.

- New test implementation: `0b5906c64ff624632b2ed203c45bbc0f43f21d94`.
- Reconciled source: `c4bb453fb9563f578d9989874702c0fc2084ced5`, including main
  `3831336f88ff3d777733e5e3453011822402e971`.
- The original #1776 head remained
  `f8c23e0d5726fdc7d50989109f198301feb45cc9`; its branch/worktree and writers
  were not changed or controlled.

## Actual additional cases

`tests/support/stream_ordering.rs` runs through the real daemon's local IPC and
a loopback Anthropic SSE provider, using private Paths and isolated fake
credentials. It does not inject events through the presentation seam.

1. **Interrupted provider attempt:** the provider emits a Unicode prefix,
   waits, then ends without `message_stop`. A canonical snapshot while held
   has no assistant message. The real provider layer retries, publishing
   `StreamRestarted`; the next attempt emits a different prefix and waits.
   Another snapshot still has no assistant message. Completion persists
   exactly the replacement answer once, with the discarded prefix absent.
   Two provider attempts share one Run ID; one restart is observed.
2. **Tool turn followed by final answer:** the first request emits text and a
   deliberately unknown fixture Tool. The Tool is not executed; a local error
   result drives the second request. While that request's final text is held,
   the snapshot contains one assistant Tool-call message and its Tool result,
   but no final answer. The first assistant boundary is `MessageAppended`;
   the final boundary is `MessageComplete`. The two assistant records have
   distinct canonical IDs, while their deltas share one Run ID.

Both tests wait for terminal Run publication before teardown, check that the
private daemon releases its Profile lease, abort/join the fake provider and
remove only their private fixture directory. Unique event boundaries are
searched in already received events too: a fast provider can publish before
the Send acknowledgement. Gates use retained Notify permits, not timing sleeps.

The full optional daemon suite now has three cases, including the earlier
desktop transport/navigation/forecast/recovery fixture. These new cases test
the real protocol independently of the canonical-only desktop subscription.
They do not demonstrate a GUI consuming live tokens.

## What the source actually guarantees

The following source audit was made at reviewed `6372d587`; the main merge's
provider retry-hint parsing changes do not alter these publication paths.

| Fact | Source / implication |
|---|---|
| `GetSession` reads messages and Run metadata in one storage transaction and samples running while holding the writer mutex. | [daemon.rs:1973](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-core/src/daemon.rs#L1973). Internal snapshot consistency does not supply an event cursor. |
| One local output writer serializes accepted response/event frames. Non-control requests use one worker; control requests can bypass it. | [daemon.rs:14540](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-core/src/daemon.rs#L14540). Response order is not universally request order. |
| A Run's control entry is installed and `RunStarted` broadcast before its Run task is spawned. | [daemon.rs:5545](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-core/src/daemon.rs#L5545). This is a useful producer boundary to test for a constrained future-Run mode. |
| A model callback sends text/reasoning deltas and maps a provider restart to `StreamRestarted`. | [daemon.rs:6554](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-core/src/daemon.rs#L6554). A restart must discard prior partial content. |
| Persistence precedes the corresponding canonical-message broadcast; the writer mutex is released between those operations. | [final answer](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-core/src/daemon.rs#L6937), [Tool turn](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-core/src/daemon.rs#L6989). A snapshot can include a message whose event is published later. |
| Deltas carry Session, Run and text, with no message ID, attempt ID, offset or event sequence. `MessageAppended` has no Run ID. | [event DTOs](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-protocol/src/lib.rs#L2805). Run identity alone cannot reconcile a turn/attempt against a snapshot. |
| Broadcasts serialize under the subscriber lock. Droppable events can be omitted; scope/kind gaps are coalesced and flushed when queue space returns. Important-event saturation disconnects. | [send_all](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-core/src/daemon.rs#L538), [gap flush](https://github.com/athiwatc/formless/blob/6372d587/crates/formless-core/src/daemon.rs#L273). FIFO receipt is not lossless delivery. |

For example, after a delta is queued, a producer may persist its completed
message, a snapshot may read that message and enqueue its response, and only
then may the producer enqueue `MessageComplete`. Alternatively the completion
event may be queued before the snapshot response. A request's buffered deltas
can already belong to a message included in the snapshot, or can belong to
work after its storage read. Appending all buffered deltas or dropping all of
them is therefore not a general reconciliation rule. Canonical message IDs
help reconcile completed records; they do not identify an unfinished turn.

The local SnapshotGuard only rejects retired ownership and observed overlap.
It does not prove a watermark, an absent missing prefix or transport recovery.
Its acceptance must not be treated as permission to replay arbitrary deltas.

## Candidate implementation boundary, not yet proven

A narrower design may avoid needing general mid-Run replay: retain canonical
mode for a Run already active on attachment; enter live mode only for a future
`RunStarted` observed after a completed idle snapshot, on the same subscription
and selection. Keep that canonical baseline fixed while live events are
applied. A snapshot request/response overlapping a new Run would retain
canonical mode for that Run. Any gap, edit, unknown/conflicting boundary,
disconnect or selection change exits live mode and reloads canonical data.

This is an inference from the source boundaries, not an accepted contract or
implemented behavior. Before enabling it, tests must cover delayed snapshot
publication, queued/new Runs, concurrent canonical appends/edits, both provider
retry paths, schema-suppressed candidate streams, Tool/step pauses, subscriber
overflow, A→B→A, reconnect and bounded buffering. Every `MessageAppended`
producer must be audited before assuming it closes the current live turn.
The alternative of general mid-Run replay needs an explicit correlation and
cutoff contract; adding that is a separate protocol dependency, not an offset
invented in a widget. Keep the current safe canonical fallback throughout.

## Verification and platform evidence

The new reconciled-source checks use the optional manifest, `--offline --locked`,
two Cargo jobs, incremental off and dev/test debug info off:

- Complete daemon/provider suite: three passed; log
  `evidence/gpui-1753-sol/ordering-reconciled-daemon.log`.
- Strict clippy for that test target: passed, with 26 existing core dependency
  warnings; `ordering-reconciled-clippy.log`.
- Formatting and diff whitespace checks passed; `ordering-reconciled-fmt.log`.
- Independently at `6372d587`, existing core backpressure tests: eight passed;
  provider-retry ownership tests: seven passed. They used the root workspace's
  lockfile, not the optional client's lockfile. Logs:
  `6372-independent-backpressure-root.log` and
  `6372-independent-provider-retry-root.log`. An earlier attempt from the
  optional workspace correctly refused testing a non-member's dev dependencies;
  its failed log was retained.
- Full Windows daemon-fixture cross-check was attempted and stopped in
  `aws-lc-sys`: `x86_64-w64-mingw32-gcc` is missing. See
  `stream-ordering-windows-fixture-cross.log`. No compiler was installed and no
  native Windows execution is claimed. The older GUI-free cross-check is not
  evidence that this larger test target built.

The new tests are included by the existing three-host optional workflow. Its
execution result must be inspected separately; arranging coverage is not
passing Mac/Windows/Linux execution. No desktop runtime or renderer code was
changed by this increment, and previous captures are not presented as new
native interaction acceptance.

Static inspection of the existing clean `b3a9a60e` development artifact rechecked
its binary hash, found 21 system-library loads, and read its signature without
launching anything. `codesign -dv` reports a linker ad-hoc signature, no Team
Identifier, an unbound Info.plist and no sealed resources. Those are development
artifact facts, not distribution signing. Raw metadata is retained in
`6372-static-bundle.json`. Licensing, icons, signing/notarization, installers,
updates, clean installation and bundled-app launch remain open.

## Parity and native next work

Native decisions/questions remain an essential missing workflow. `UserQuestion`
is intentionally profile-wide and has an explicit Session identity plus up to
four structured questions, option descriptions and multi-selection. It must
not be reassigned to the displayed Session. `AnswerQuestion` is first-wins and
can return `ok: true` with `answered: false`; future forms must validate the
specific acknowledgement, retain text on failure/uncertainty and never replay
an answer after reconnect. Snapshot/closed-event reconciliation needs explicit
identity/tombstone handling. This is an implementation inventory, not a
reopened allegation that global attention events are incorrectly scoped.

Broader Session actions, attachments/model/Role selection, Step controls,
resource editing and contextual artifact/diff/general-purpose pages remain
open, as do preference persistence and the epic's measured performance budgets.
Synthetic model timings cannot establish foreground key-to-paint, real scroll,
screen-reader, energy or long-run RSS acceptance.

Existing Computer Use approval `proud-river-57508678` for Formless Sol Validation
and its uncertain earlier launch/control outcome remain unresolved. No repeat,
alternate launch, OS security change or bypass occurred. Foreground Mac IME,
selection, keyboard, VoiceOver, human 200% and bundle launch need that existing
approval/action resolved first. Native Windows/Linux execution also remains
unverified. Keep #2081 draft; do not merge or close #1753.
