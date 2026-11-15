# Opt-in future answer attempts: protocol and admission model

This continues independently reviewed local integration `4445d8b` (source/log
hashes reviewed; tests not rerun). The optional desktop's production connection
still requests `deltas: false`. No GUI path opts into this protocol, and no
native launch/control, PR update or remote merge is performed.

## Scope

The old wire format cannot safely join arbitrary buffered deltas to a canonical
snapshot. This increment implements a narrower contract: observe complete
**future attempts** after a producer barrier, otherwise use canonical history.
It does not replay an unfinished prefix and does not claim a global atomic
canonical snapshot/event watermark. Turn/attempt correlation and a causal
future-start admission boundary replace the need to infer such a watermark.

The daemon advertises additive capability `future_transcript_attempts_v1`.
Protocol/minimum versions remain 1. Peers must negotiate the capability and
verify the returned mode; older daemons return the existing unsupported error.

| Operation/frame | Contract |
|---|---|
| `SubscribeTranscriptAttempts {session_id}` | Changes only this connection to one bounded Session identity, opt-in answer attempts, and no competing legacy content/reasoning chunks or legacy stream-reset events. Ordinary canonical events remain subscribed. |
| `GetTranscriptBarrier {id}` | Returns `{snapshot, barrier:{epoch,cursor}, mode:"future_attempts_only_v1"}`. The canonical snapshot is the exact existing synchronous GetSession read. The producer clock is sampled **after** that read releases the store lock. |
| `TranscriptAttempt {update}` | Contains epoch, producer cursor, Session/Run/turn/attempt identity, and a tagged phase. Cursors serialize producer emission; they are not canonical commit cursors. |
| `started {base_seq}` | Published before invoking the provider. A new turn identity is scoped to the outer agent iteration, and manual/internal provider retries get fresh attempt identities. The base sequence is the context's recorded consumed sequence. |
| `delta {offset,text}` | Answer text only, with a UTF-8 byte offset within this attempt. Reasoning is excluded. |
| `closed {bytes,reason}` | Ends the provisional attempt and requires canonical reconciliation. Returned means only that the provider future ended; it is not acceptance, persistence, successful billing or Run completion. Restarted/dropped are also terminal for this attempt. |
| Existing `Subscribe` | Resets the new mode; legacy clients never receive new attempt frames. |

A subscriber joining an already active attempt receives no reconstructed
prefix. It can admit a fresh internal retry or later attempt after its barrier.
Schema-constrained candidates emit no attempt frames. Existing legacy producer
and retry/reset behavior remains available to legacy subscribers.

## Admission and the causal boundary

The GUI-free Gate installs only an owned decoded snapshot/barrier for one
attachment and Session. An attempt must have a start cursor strictly greater
than the barrier, matching epoch and canonical base sequence. Merely receiving
a later chunk never creates a provisional row. Identity and monotonically
advancing cursors must continue to match; offsets must equal bytes already
accepted. Global cursors need not be contiguous for a Session, because other
Sessions consume them. Missing or duplicate chunks, identity conflict, epoch
change, unbound canonical changes during a live attempt, and close require
canonical fallback. Nothing clears a retired Gate; a new owned load is needed.

An idle Gate may advance its canonical baseline only with a validated exact
next MessageAppended/MessageComplete sequence; it returns that record for the
caller to apply canonically. This lets the canonical user message precede a
fresh Send's started event. Duplicates, overlap, gaps and edits require a new
snapshot. Canonical events are **never** thrown away using the producer cursor.
A delayed duplicate event already included in a snapshot still invalidates it.

The proof boundary is causal, not a transactional watermark:

1. A future eligible start is registered and enqueued under the producer clock
   before invoking its provider. Its candidate cannot already be persisted in
   a snapshot whose read completed before the sampled barrier.
2. Starts registered before or during the read, or between that read and the
   later clock sample, are excluded. Conservative exclusion may skip a whole
   otherwise usable attempt; it cannot invent an absent prefix.
3. A canonical commit racing between read and clock sample is not classified
   by the barrier. Exact next canonical events may advance an idle baseline;
   any overlap/gap invalidates it, and a different base sequence rejects a start.
4. A client must serialize reply installation with staged and later frames.
   `Pending` stages at most 256 relevant frames / 1 MiB of serialized event data,
   including frames emitted after barrier sampling but received before its
   response. Overflow clears the staging buffer and requires canonical mode.
   Wrong attachment work is rejected before decode. Barrier metadata has a
   separate 2 KiB bound. Snapshot installation and draining staged events are
   one caller-owned state transition, not asynchronous widget callbacks.

The Gate retains no transcript or prefix text; it lends accepted text/records
back to the caller. Live accepted text is limited to 1 MiB per attempt. The
caller must honor Reload by discarding provisional text and arranging a current
owned canonical read. Creating new attachment tags/retiring old work on selection,
reconnect and Profile replacement remains the transport's responsibility.

## Producer lifecycle, delivery and locks

An attempt owner closes exactly once on return/drop; a callback sink cannot
publish after that owner closes. An internal provider restart closes the old
attempt and starts a new identity at byte zero while preserving turn and Run
identity. Schema visibility is fixed by the existing pipeline policy.

Lock order is attempt state → producer clock → subscriber list → event filter.
The shared snapshot read holds no producer-clock lock, and releases the store
lock before sampling that clock. Subscriber setup checks filters and releases
those locks before publishing starts. No slow I/O/await is performed with these
locks held. A cursor exhaustion rotates daemon epoch rather than wrapping in
one epoch, forcing old consumers to reload.

Only delta frames are droppable. Their scoped warning uses the existing
MessageDelta drop classification. Starts and closes are important boundaries;
a saturated connection is disconnected rather than silently omitting them.
The close's byte count includes dropped chunks; no successful count proves a
canonical result. The producer retains identities, counters and lifecycle state,
not an unfinished answer or replay journal. Legacy-only attempts do not allocate
new chunk strings or scan subscribers per chunk, although the callback checks
attempt state. Queue/frame limits remain the existing daemon limits; these are
not measured peak-RAM, latency or performance-budget claims.

## Evidence and remaining acceptance

Producer tests cover serialization, UTF-8 offsets, RAII closure, retry identity,
late subscriber admission, schema suppression, epoch exhaustion, scoped loss,
important-boundary saturation and concurrent producers. A deterministic test
pauses the canonical read under its existing hook, publishes a start before the
later clock sample, and verifies its exclusion without taking the store lock
from the publisher.

Admission tests cover ownership, future-only starts, exact prefixes, missing and
duplicate chunks, mismatched identities, byte bounds, canonical overlap, terminal
fallback and bounded pre-reply staging. Real private IPC/fake-provider tests
cover normal Send, mid-Run barrier exclusion, Unicode offsets, internal retry
identity, canonical final text, and resetting back to legacy subscription.
The existing canonical daemon suite runs alongside them. CLI/TUI compilation
also checks shared-enum compatibility; TUI explicitly ignores the unrequested
new frame and continues to use legacy Subscribe.

Validation caught a stack overflow in an initial recursive call through the
large async request handler. That recursion was removed: both GetSession and
the new barrier now share an extracted synchronous canonical snapshot read.
No stack-size override was used. Initial compile/fixture failures and successful
final command/source/log hashes remain in the sibling evidence manifest.

This is an unenabled protocol/admission foundation, not streaming acceptance.
Before the desktop opts in, independently review the proof and exact patch;
implement its serialized reader/handshake integration and explicit fallback UX;
then test full transport ownership, real queue overflow/connection loss,
A→B→A/reconnect and same-process epoch handling, queued Runs, manual retry,
Tool/Step/schema/compaction transitions, concurrent canonical edits/appends,
pre-response starts, cancellation and sustained bounded operation. Current
private tests and unit schedules are evidence for the stated constrained
contract, not all those product/transport schedules.

General mid-attempt replay would still need a retained prefix/cursor or a
stronger server snapshot/publication contract. The unresolved foreground
approval/prior launch outcome, native IME/accessibility/clipboard/scaling,
Windows/Linux GUI, remaining required parity, accepted performance budgets and
distribution gates also remain open. Preserve the original conflicting partial
tree; its ownership remains inconclusive. No publication is authorized by
these test results alone.
