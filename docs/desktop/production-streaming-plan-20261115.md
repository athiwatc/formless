# Minimum production streaming completion plan — 2026-11-15

Production remains canonical-only. This plan describes the implemented v1
contract, the remaining decisions and the work required to enable it. A local
merge or successful model schedule is not enablement approval or epic completion.

## Selected initial scope; production policy remains disabled

The implemented `future_attempts_only_v1` mode deliberately waits for a future
eligible attempt after attachment, reload or reconnect. It cannot reconstruct
a prefix that was produced before its barrier. Persisted history remains the
fallback. A normal new Send can stream; an already-running answer may remain
canonical-only until a fresh retry/turn or its persisted completion.

The owner has selected future-attempt-only v1 as the initial implementation.
Mid-answer attachment/reconnection shows persisted history until completion or
the next eligible attempt; this is not seamless recovery. Bounded-prefix support
is deferred as a possible versioned extension. Do not disguise that limitation
with a local offset, optimistic snapshot merge or unbounded buffer. This scope
selection does not enable production: review, protocol/compatibility, native
acceptance and release gates below still apply.

## Implemented ordering contract and checks

| Obligation | Implementation and evidence | Limit / release condition |
|---|---|---|
| Old peers remain usable | Capability negotiation and exact mode ACK in `client/reader.rs`; canonical-only fallback | `Client::start` still passes `false`; no preference, environment or CLI enables v1 |
| A live prefix starts after its storage baseline | `GetTranscriptBarrier` reads the canonical snapshot before sampling the producer clock; `Gate` admits only a later Start with the exact canonical base sequence | This is a conservative future-start cutoff, not an atomic general replay snapshot |
| Receipt timing cannot change eligibility | `Pending` stages bounded owned events and replays them through `Gate` after the barrier | New exhaustive schedules cover all 420 small causal orders, all 16 cutoff/staging combinations and 4,200 owner-retirement cuts |
| Canonical writes/publication cannot attach text to old context | Canonical events are never discarded by the producer cutoff; exact-next idle records can advance the baseline, conflicts retire it; Start binds to its base sequence | New 1,800 causal storage/publication schedules check both snapshot sequences and all six snapshot/cursor combinations; no append is allowed against the old context |
| A partial or reordered attempt cannot become a complete prefix | Session/Run/turn/attempt identity, UTF-8 byte offsets, observed scoped cursor floor, bounded live bytes, immutable attachment identity | All 24 small wire permutations expose only the ordered prefix and remain inert after failure; existing private IPC covers ignored Delta/Closed followed by delayed Start |
| Producer order and lifecycle are real | `transcript_attempts.rs` serializes clock/publication; per-attempt state permanently retires on owner return/Drop | Eleven core producer tests cover concurrent cursors, cutoff overlap, retry, late callback, loss, epoch rollover and retirement; full intervening source review remains required |
| Loss and backpressure fail closed | Scoped gaps, important-boundary disconnects, bounded handshake staging and UI queue failure retire live state; three repeatedly overlapped loads fall back canonically | Existing 23 private-daemon schedules cover retries, Queue, Tool/Step/schema/compaction, edits, EOF, pressure and reconnect; no mutation is replayed |
| Controls do not reuse observed stale status | Connection-local lifecycle revision rejects overlapping reads and retires forecasts/unsent Step work | This covers events observed by the connection, not an atomic daemon status revision; neither live text nor chunks assert Run state |
| Delivery does not rewrite admitted events | Validation-reader batching preserves individual events and order; control/RPC boundaries force flush | 16 ms is a scheduling target, not a latency guarantee; oversized events bypass batching; held-payload limits are not a total-memory bound |

The new schedules execute the actual `Pending` and `Gate` APIs. Their eligibility
oracle depends on producer Start versus barrier sample, independently of event
receipt and reply timing. Canonical schedules allow storage and publication to
separate, but preserve the real subscriber's FIFO publication order. They are
finite model checks with one canonical boundary and at most two chunks, not a
proof over all daemon interleavings. Real IPC schedules and producer tests remain
necessary evidence. No production source behavior changes in this increment.

## Ordered implementation and acceptance steps

1. The initial scope is recorded: future-attempt-only v1, with a persistent
   visible mid-answer attach/reconnect limitation. Keep production off until
   exact-head review, protocol/compatibility and acceptance conditions pass.
2. Finish independent review of the entire combined runtime/protocol range,
   including every canonical publisher, producer lock order, retry retirement,
   subscriber loss and the detached lockfile. Incremental no-blocker receipts
   do not replace this combined review. Resolve any finding with a regression.
3. On the agreed current-main head, run portable model/IPC suites on supported
   hosts. The existing `desktop.yml` includes the new model tests. Hosted CI has
   not been executed for these unpublished local commits. Windows GNU model
   compilation is not Windows execution, and no current Linux GUI acceptance
   or support-tier decision is inferred.
4. Resolve the existing Formless Sol Validation approval
   `proud-river-57508678` and establish the prior uncertain launch/control
   outcome through the supported evidence/approval path. Only then may native
   execution proceed. Do not repeat, substitute or bypass that action here.
5. Obtain current-head native evidence for typing/Thai/Japanese/dead keys,
   composition with Send/Steer/Queue/Step, reconnect during composition,
   VoiceOver/NVDA, real clipboard/selection, scaling/HiDPI and filesystem/picker
   behavior. Confirm the supported OS tier explicitly rather than silently
   dropping platforms.
6. Agree native key-to-paint/frame, CPU/RSS, large-history and accessibility
   budgets and measure them under a controlled native workload. Fake-provider
   delivery timing, concurrent build/test observations and TestPlatform checks
   cannot supply these acceptance results.
7. After scope, review and acceptance pass, make one separately reviewed
   production-policy change to negotiate v1 in the normal client constructor.
   Retain older-peer fallback, canonical recovery and mutation non-replay. Add
   production-constructor/negotiation tests and replace validation-only user
   wording before release. Do not introduce an undocumented enable switch.
8. Before publication/merge, recheck main and ownership, preserve the conflicting
   original tree until recovery is established, review the combined diff and
   rerun affected checks. Keep the PR draft while substantive gates fail.

If seamless recovery is required instead, design a versioned v2 first: a bounded
live prefix tied to Session/Run/turn/attempt and canonical base, an atomic
prefix/storage/cursor handoff, an explicit accepted-canonical boundary and a
consistent lock-order policy across every publisher. Overflow must fall back
canonically rather than retain an unbounded journal. Preserve v1/legacy
compatibility and test all cutoff/commit/receipt orders plus loss and reconnect.
A counter added only at send time cannot provide that contract. This is a new
protocol project, not a safe one-line production toggle.

## Other minimum completion gates

Essential parity remains broader than streaming: native security approvals,
Step-mode enabling and larger/multiple Step prompts, composer attachment and
Role/model/reference controls, and the required conversation/resource actions
remain in the acceptance matrix. Prioritize an agreed minimum required set;
optional presentation expansion does not clear these workflows.

Distribution still needs the mac 0.1.1 complete chosen-license bundle or
replacement, pathfinder_geometry 0.5.1 chosen terms with source notices, seahash
4.1.0 authoritative attribution or replacement, and the nineteen objc2 historical
scope/SDK questions. Dependency/assets/toolchain review, support tiers, current
packaging, signing/notarization, install/update and clean-machine acceptance are
separate gates. The reviewed main release-workflow repair does not clear the
GPUI-specific distribution scope. No package, native launch or publication is
performed by this contract-validation increment.
