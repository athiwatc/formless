# Bounded delivery of admitted live text

This follows local native-Step integration `0fe411f`. Production streaming
remains disabled. The existing future-attempt validation reader previously
pushed every admitted delta immediately into the 16-slot UI channel. A short
provider burst could therefore force reconnect even when the UI consumed at a
normal frame cadence. The existing failure policy was safe canonical recovery,
but lacked a bounded batch stage for these bursts.

## Delivery contract

Only the validation-reader path uses the new stage. Production canonical
updates still use the original immediate delivery function. A held batch can
contain only already-admitted MessageDelta events for the same exact owner and
Run: Client, physical connection, view epoch, selection generation, Session,
basis and load identity must all match. Events retain their individual payloads
and original order; no source text, offset or chunk boundary is rewritten.
Attempt admission and canonical reconciliation logic are unchanged.

Held payload is limited to 64 KiB and 256 events. Crossing a limit flushes the
previous batch before accepting another; a single admitted larger update passes
through immediately. These are held-payload limits, not allocator/RSS or total
channel-memory claims. The first held chunk establishes a 16 ms target deadline;
later chunks do not extend it. Delivery uses the same monotonic Tokio clock as
its reusable timer. The worker checks the deadline at the top of each iteration
and gives an armed flush priority over reading another frame. A quiet partial
answer does not wait for the existing 250 ms refresh tick.

A pending RPC write flushes held text first, before any mutation is marked as
possibly written, so that prefix is not parked behind a blocking socket write.
Canonical records, baselines, reloads, status metadata, action receipts and all
other non-live updates force delivery in their existing order. They are not
merged or delayed by the stage. Retired held text is discarded before delivery;
important reload/baseline updates still pass to the existing view-owner checks.
The added stage does not make stale content valid or establish any new authority.

UI channel saturation still fails the connection and retires queued frames;
it never silently drops an owned chunk and continues the attempt. Unsent held
text is not replayed across reconnect. A fresh owned canonical baseline remains
the recovery path. The 16 ms value is a worker scheduling target, not a measured
paint/latency SLA: CPU scheduling and other I/O can still delay processing.

## Evidence

Deterministic checks cover a first-chunk deadline, exact UTF-8 events, byte/event
bounds, every owner dimension and Run identity, canonical/status/reload barriers,
retirement, queue failure and a flush before outgoing I/O. A regression puts the
same 100-event microburst through the old per-event delivery and the new stage:
the former saturates 16 slots, while the latter returns all 100 original events
in one ordered UI update. No separate checkout or build cache is needed for
that policy comparison.

The private IPC/fake-provider burst schedule emits 2,048 indexed UTF-8 deltas
in groups of 32, with a 4 ms minimum timer between groups. A consumer drains at
a 16 ms minimum timer. Every original delta and the full canonical result must
match exactly, with no connection replacement and one fake provider request.
`reader_burst_batch_metrics` records observed UI batch count, largest event and
payload batch, elapsed time and maximum updates drained together. The older
5,000-frame paced schedule continues to check every original chunk individually
and overlap real status reads; its timings now include the batch target delay.
These are controlled transport samples, not renderer or production budgets.

The intentional undrained-UI schedule now spaces 100 deltas by 20 ms. This
forces repeated UI flushes even with batching, while keeping server traffic far
below its separate 4,096-frame subscriber capacity. It must still retire the
connection and recover the final canonical answer without replaying work. The
independent server-saturation, EOF, retry/Queue, delayed metadata, edit,
Tool/Step/schema/compaction and native Step receipt schedules remain in the suite.
All providers, Profiles and credentials are private fixtures; no real Tool,
paid API, desktop window or renderer is executed.

This increment does not enable production streaming, general mid-attempt replay
or lossless delivery during arbitrary UI stalls. Independent review of the new
stage and complete intervening runtime/protocol range, product acceptance of
future-only/canonical fallback, native/performance/platform and distribution
gates remain open. Native Step-mode enabling, larger/multiple prompt controls,
security decisions and other required parity also remain open. The conflicting
original checkout and existing branches are preserved. The unresolved foreground
approval and prior uncertain launch/control are neither repeated nor bypassed;
no other writer or Claude session is controlled, and nothing is published.
