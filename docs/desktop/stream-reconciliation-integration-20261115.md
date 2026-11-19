# Streaming reconciliation and local main integration

This work follows local takeover `cf29e66384f545e2d275bab6a635b630e78f89ec`.
It concentrates on reconciliation and integration rather than optional UI parity.
It is not completion of epic #1753, native acceptance or permission to publish.

## Frozen integration inputs

Remote reads found main `1fc1df2aef430fdded0ed83052f02ecdd8199a65`, draft
#2081 `cc1d0c9805230d7807a4a871396f91c0ebeec91a`, and open #1776
`f8c23e0d5726fdc7d50989109f198301feb45cc9`. These are time-of-read facts;
recheck before any publication. GitHub PR base OIDs differed from the current
main ref, so integration uses the fetched immutable main commit explicitly.

The takeover clone was shallow at the published reviewed head. After fetching
its published ancestry, the real common ancestor is
`e9bf104fb9c1bf1bff4f56e05efa92b17a1bfc14`. An unrelated-histories merge
was not used. A new local integration checkout and branch were created:
`formless-gpui-integration`, `codex/gpui-integration-20261115-1fc1df2`.
Local merge `d3afa0ba7d37cdc921693156de00120d00a28f22` combines the takeover
and current main without source conflicts. The original takeover branch remains
at `cf29e66`. No preserved conflicting tree or original PR branch was changed.

The first locked validation failed because main updates core's zip requirement
to 9.5.1 while the detached desktop lockfile still resolved 8.6.0. Offline Cargo
resolution updated zip and typed-path and added constant_time_eq and nix 0.31;
subsequent checks use `--locked`. Root manifest/lock changes are inherited from
main; no separate root dependency change is introduced here.

## Reconciliation failures now fail closed

The transcript reducer latched a reload reason but continued applying later
deltas and canonical events. A suffix after a dropped prefix could therefore
create a new provisional row despite known failure. A new regression reproduced
that behavior. Once any reconciliation failure is latched, subsequent events
now leave the transcript, sequence and first failure unchanged until a trusted
same-Session snapshot replaces the state. This covers edits, dropped tokens,
restarts, wrong Run, invalid records and conflicting sequence identities.
Foreign snapshots cannot clear the latch. Preexisting tests were adjusted to
assert the stronger behavior; a shell test also checks the freeze directly.

A separate transport regression puts a delayed delta and duplicate completion
after a snapshot that already contains the canonical assistant record. The
canonical-only Engine never installs a phantom provisional turn and explicitly
reloads. This tests the safe fallback, not safe delta replay.

## Producer boundaries and remaining wire contract

At the frozen main input, `MessageDelta` still has only Session, Run and text;
there is no turn/message, attempt, byte offset or event cursor. `GetSession`
contains no corresponding cutoff. Main's protocol change is an audit cursor,
which is unrelated to transcript streaming. The previous source audit remains
applicable: persistence and publication are separate boundaries, and subscriber
backpressure may omit droppable events or disconnect saturated clients.

All production MessageAppended publishers were inventoried in daemon.rs plus
branch_summary.rs, pipeline_daemon.rs and review_daemon.rs. They include ordinary
Send/Steer/queued input, recovery, Tool-call assistant turns, Tool results,
paused assistant turns, context/system notes, loaded Skill notes, asynchronous
Invocation delivery, branch summaries, review comments and pipeline starts.
A generic MessageAppended cannot be assumed to identify the current streaming
assistant turn or even belong to its Run. A Run may contain multiple turns and
provider attempts. Schema-constrained pipeline candidates suppress live text.

The source audit also identified three discarded-candidate retry boundaries:
refusal nudges, pending-Tool reminders, and successful context-window compaction
retries. Actual private IPC/provider tests reproduced missing reset events.
These paths now publish the existing StreamRestarted event before the next
answer attempt. No wire DTO, persisted content, retry limit or provider-call
policy changes. A compaction failure retains its existing partial-answer path
and does not publish this reset. These fixes affect existing live subscribers,
including TUI/web; the desktop remains canonical-only.

One gated test covers both refusal and pending-Tool cases (the Tool is
capability-free and completes naturally). Another seeds two completed turns,
forces context-window exhaustion, completes a fake summarization and holds the
replacement answer. Each checks exactly one reset between discarded and new
prefixes, no discarded canonical assistant record, one Run for the retry and
expected provider request counts. Both tests reproduced the missing reset
before their fixes. The initial compaction fixture had too little history to
reach a whole-turn cut; its diagnostic failures remain in evidence. These
producer repairs do not establish a general snapshot/event cutoff.

General replay remains blocked. A concrete server/client contract must provide:

1. Explicit capability negotiation and canonical-only fallback for older peers.
2. Correlated Session/Run/turn/attempt identity for live chunks and reset/close
   boundaries, with an offset or equivalent duplicate/missing-prefix detection.
3. A snapshot cutoff tied to the same producer ordering domain as canonical
   commits and live publication. Assigning a counter only at subscriber send
   time does not make a storage snapshot atomic with that counter.
4. Defined attachment behavior for an already active turn: either an included,
   bounded live prefix with a cursor, or canonical mode until an explicit new
   eligible turn boundary. A client-created offset cannot supply missing text.
5. Sequenced/bounded delivery with gap detection, explicit retry/reset semantics,
   and canonical fallback on edits, loss, overflow, disconnect or stale ownership.
6. A lock-order and memory policy covering every publisher above; no global
   subscriber lock held over slow I/O or unbounded replay journal.

Acceptance needs both snapshot-before-completion and completion-before-snapshot
schedules; actual subscriber delay/overflow; queued/new Runs; mid-Run attach;
retry and discarded candidates; Tool/Step/schema/compaction transitions;
concurrent appends/edits; A→B→A and reconnect; duplicate/missing offsets and
bounded retention. Passing the fallback tests does not discharge these items.
The earlier future-Run-only proposal is still a proposal, not an enabled mode.
Streaming remains `deltas: false` throughout this work.

## Acceptance and ownership gates

Ownership of the original conflicting partial edits remains unestablished by
the parent's read-only audit. A clean separate integration tree does not prove
that those edits are safe to adopt or discard. They remain preserved; no writer
or Claude process was inspected, interrupted or restarted.

The existing foreground approval `proud-river-57508678` for Formless Sol
Validation and prior uncertain launch/control outcome remain unresolved. No
retry, alternate launch, renderer execution or bypass was attempted. Current
native IME, VoiceOver/NVDA, input/clipboard, scaling and picker/filesystem tests
remain unavailable through that blocked action. Current Windows/Linux GUI,
performance budgets, remaining required product parity and distribution licence,
signing/notarization/install/update acceptance also remain open.

Independent review must cover the combined main diff and this reconciliation
increment, including the detached lockfile. Then recheck main/PR heads and
ownership and run aggregate/native checks on the agreed integration head before
publishing or merging. No remote branch/PR changes are made by this work.
