# Future-attempt reader integration, disabled for production

This continues reviewed retirement fix `7b9dcc8`; parent reports the specific
late-restart fix was independently re-reviewed, with hashes checked and no test
rerun. This increment adds the serialized reader/worker and headless view
adapter for the opt-in future-attempt protocol. Production `Client::start` and
Shell::connect_daemon still use canonical-only subscriptions (`deltas: false`).
There is no preference, environment variable or CLI switch enabling streaming.
Only explicit test/daemon-test validation constructors select the new path.

## Ownership through the reader and view

Ping must advertise `future_transcript_attempts_v1`; subscription must acknowledge
the exact mode. Missing or malformed capability/acknowledgement falls back to the
existing canonical subscription. The reader binds every subscription, barrier,
status read and emitted batch to a client-instance UUID, physical connection,
synchronous view epoch, selection generation, Session and fresh basis identity.
Baseline loads additionally carry a monotonic read identity. The view refuses
old/different contexts and duplicate/older loads before replacing any content.
A Profile replacement cannot revive queued frames even if numeric counters match.

The view epoch is separate from forecast/draft invalidation. Select/Reconnect
retire it synchronously before bounded command enqueue; physical disconnect and
Drop also retire it. Typing an unsent draft does not interrupt a valid stream.
The reader clears pending live state on retirement, refuses stale replies before
JSON decode, and renegotiates on reconnect. Mutations are never replayed.

Barrier staging uses the existing 256-frame / 1 MiB bounds. Snapshot installation
and validated staged replay form one tagged UI batch, avoiding a partial
256-frame replay through the 16-slot UI queue. Accepted live frames pass through
the existing transcript reducer only after attempt/offset admission. Canonical
records may advance an idle baseline only at their exact next sequence.

Periodic status reads use canonical GetSession data, but return only sampled
running/recovery metadata to the view. They never replace a live transcript
baseline or infer Run state from chunks. A changed sequence forces a fresh
barrier; same-sequence edits still invalidate through the important canonical
edit event. The view always keeps its admitted canonical baseline fixed while
live text is present. Whole-output/copy/search invalidation and provisional-row
retirement reuse the existing transcript/view logic; drafts and input entities
are not rebuilt by stream/status batches.

Close, gap, edit, identity conflict and explicit Refresh retire provisional text
and request a fresh owned barrier. Read slots are bounded across retired
selections. Three consecutively overlapped loads, invalid data, or staging
saturation latch canonical mode for that connection rather than spinning live
loads. UI delivery saturation fails the connection, retires its shared epoch,
and recovers canonically; no live chunk or mutation acknowledgement is silently
replayed. Existing uncertain mutation/Question/fork identity handling remains
separate and unchanged.

## Validation and limits

Reader tests cover capability fallback, false mode acknowledgement, staged
pre-response prefix batching, status-only refresh, changed-sequence fallback,
pre-decode view retirement, forecast/view-epoch separation, staging saturation,
bounded pending reads, explicit Refresh during subscription, repeated overlap
and UI-queue failure.

Headless GPUI tests cover production-mode rejection, live/status/draft coexistence,
stale legacy snapshots, wrong basis/connection/epoch, monotonic baseline loads,
physical retirement and Profile replacement with matching counters. They execute
in TestPlatform, not an OS application or screen-reader session.

Three real Client worker/private-daemon/fake-provider schedules cover normal
Send and concurrent status reads; A→B→A and same-process reconnect while an
answer is held; and deliberately not draining the UI channel until it overflows.
Recovery omits the old incomplete prefix, obtains canonical final text and keeps
the provider request count at one. The full prior private daemon suite also runs.
These tests use private Profiles and fake credentials, with no paid calls.

Initial validation hit a full disk in this task's generated Cargo target. Only
owned disposable package outputs were cleared; source, immutable evidence
archives and the conflicting partial checkout were preserved. The failed log is
retained. No writer/process was interrupted and no foreground action occurred.

These are correctness schedules, not throughput/native acceptance. The simple
bounded delivery policy may reconnect at high token rates; accepted sustained
performance and coalescing policy remain open. New-mode actual daemon schedules
for manual retry, queued Runs, Tool/Step/schema/compaction, arbitrary EOF/server
subscriber overflow, concurrent edit publication and delayed status/question
replies still need broader coverage and independent review. General mid-attempt
replay is not implemented; the deliberate fallback is persisted history until
an eligible future attempt. Do not enable production streaming on these results
alone.

The supplied newer main and fetched main are accounted for in the final merge
provenance/evidence manifest. Core, protocol and desktop source identity across
that merge is checked explicitly. No remote branch/PR update or merge is made.
The original partial-tree ownership remains unresolved. Existing Formless Sol
Validation approval `proud-river-57508678` and prior uncertain foreground action
remain unresolved and unattempted here. Native IME/accessibility/input/clipboard,
scaling/picker/filesystem, Windows/Linux GUI, remaining required parity,
performance budgets and distribution gates remain open.
